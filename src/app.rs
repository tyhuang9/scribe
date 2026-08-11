use std::collections::{HashMap, HashSet};
use std::env;
use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use arboard::Clipboard;
use crossbeam_channel::{Receiver, Sender, bounded, unbounded};
use eframe::egui::{
    self, Align, Button, Color32, ComboBox, FontId, Frame, Layout, Margin, RichText, Rounding,
    ScrollArea, Stroke, TextEdit, Ui, Vec2, ViewportCommand,
};
#[cfg(target_os = "windows")]
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use serde::Deserialize;

use crate::audio::{
    self, CaptureCancellation, CaptureCompletion, CaptureError, CaptureIntent, CaptureMetrics,
    CaptureOptions, CaptureStopReason, LevelSnapshot, RecordingSession, Sensitivity, VadOptions,
};
use crate::benchmark::{
    self, BenchmarkMetric, BenchmarkModelInput, BenchmarkModelResult, RankingMode,
};
use crate::compatibility_bridge::{self, ProviderHandle};
use crate::config::{
    self, AppConfig, HistoryMode, HotkeyMode, OverlayMode, OverlayPosition, SettingsStore,
    StreamingMode, ThemeMode,
};
use crate::core::{DictationPhase, SessionCoordinator, SessionPurpose, StopReason};
use crate::diagnostics::{
    self, DiagnosticsStore, FailureStage as DiagnosticFailureStage, SessionDiagnostic,
    SessionMetrics as DiagnosticSessionMetrics, SessionOutcome as DiagnosticSessionOutcome,
};
use crate::history::{
    CompletedHistoryEntry, HistoryCursor, HistoryError, HistoryMetrics, HistoryPage, HistoryQuery,
    HistoryRecord, HistoryRetentionPolicy, HistoryStatus, HistoryStore, NewHistoryEntry,
};
use crate::history_playback::{PlaybackEvent, PlaybackService};
use crate::hotkey::{HotkeyEvent, HotkeyService};
use crate::huggingface_catalog::{
    CatalogSource, HuggingFaceCatalogService, ModelInventorySnapshot, RemoteModel, TrustedArtifact,
};
use crate::installations::{
    ActivationJournal, ActivationPhase, DirectoryReplacement, FileReplacement, InstallCancellation,
    InstallError, InstallProgress, InstallStage, ManagedRemoval, discover_managed_removal_targets,
    fingerprint_file_cancellable, reconcile_activation_journal, reconcile_managed_removal,
};
use crate::installed_manifest;
use crate::managed_downloads;
use crate::models::{
    ModelInstallStatus, ModelRuntimeStatus, SttModelInfo, TranscriptionStatus, format_bytes,
};
use crate::overlay::{
    self, CapturedTarget, OverlayController, OverlayMode as NativeOverlayMode, OverlayPhase,
    OverlayPosition as NativeOverlayPosition, OverlayRecovery,
};
use crate::prepared_audio::PreparedAudio;
use crate::streaming::PreviewEvent;
use crate::text_output;
use crate::transcription::{
    AccelerationPreference, CompatibilityStatus, InstallSmoke, InstallationCandidate,
    ModelDescriptor, ModelId, RequestId, RollingPreviewHandle, SessionId, TranscriptionOptions,
    TranscriptionOutcome, TranscriptionRequest, TranscriptionService,
    VerifiedInstallationCapability, verified_installation_capability,
};
use crate::tray::{TrayCommand, TrayService};
use crate::ui::{
    AppPage, ComparisonPhase, ComparisonResult, ComparisonResultPhase, HistoryPageAction,
    HistoryPageState, MicrophonePermission, ModelCapabilities, ModelComparisonState,
    ModelCompatibility, ModelDialog, ModelDownloadState, ModelLanguageFilter, ModelManagementState,
    ModelReadiness, ModelSizeTier, ModelSpeedTier, ModelViewModel, RecordingMode,
    RecordingSettingsView, RemoteCatalogActionKind, RemoteCatalogActionView,
    RemoteCatalogEntryView, RemoteCatalogFilters, RemoteCatalogSort, RemoteCatalogStatusKind,
    RemoteCatalogStatusView, RemoteCatalogVariantView, RemoteCatalogView, ScreenAction, ScreenView,
    SettingsTab, ThemePalette, UiRoute, about_page, configure_accessible_style, history_page,
    minimum_primary_target_height, recording_mode, render_screen, scroll_focused_control_into_view,
    settings_save_state, show_navigation, show_route_scroll, theme_palette, transcription_state,
    ui_palette,
};

#[cfg(test)]
use crate::ui::RemoteCatalogSizeTier;

const ACTIVE_REPAINT_DELAY: Duration = Duration::from_millis(100);
const METER_REPAINT_DELAY: Duration = Duration::from_millis(40);
const IDLE_REPAINT_DELAY: Duration = Duration::from_millis(500);
const INPUT_LEVEL_ATTACK: Duration = Duration::from_millis(30);
const INPUT_LEVEL_RELEASE: Duration = Duration::from_millis(240);
const INPUT_LEVEL_STALE_AFTER: Duration = Duration::from_millis(160);
const INPUT_LEVEL_REPAINT_DELAY: Duration = Duration::from_millis(50);
const INPUT_LEVEL_MIN_DBFS: f32 = -72.0;
const INPUT_LEVEL_MAX_DBFS: f32 = 0.0;
const SETTINGS_SAVE_DEBOUNCE: Duration = Duration::from_millis(300);
const PREVIEW_FINISH_GRACE: Duration = Duration::from_secs(2);
const PREVIEW_CANCEL_ACK_WARNING: Duration = Duration::from_secs(2);
const LOCAL_GGUF_IMPORT_SHUTDOWN_TIMEOUT: Duration = Duration::from_millis(250);
const REMOTE_CATALOG_VISIBLE_LIMIT: usize = 100;
const RETRY_RELEASE_ATTEMPTS: usize = 4;
static INSTALL_JOB_SEQUENCE: AtomicU64 = AtomicU64::new(1);

#[cfg(target_os = "windows")]
fn native_main_window_handle(cc: &eframe::CreationContext<'_>) -> Option<isize> {
    let window = cc.window_handle().ok()?;
    match window.as_raw() {
        RawWindowHandle::Win32(handle) => Some(handle.hwnd.get()),
        _ => None,
    }
}

#[cfg(not(target_os = "windows"))]
fn native_main_window_handle(_cc: &eframe::CreationContext<'_>) -> Option<isize> {
    None
}

fn capture_options_from_config(config: &AppConfig) -> CaptureOptions {
    CaptureOptions {
        // Input sensitivity always gates accepted speech. The legacy VAD setting controls
        // only whether Toggle mode stops automatically after the configured silence.
        vad_enabled: true,
        endpointing_enabled: config.recording.vad_enabled
            && config.recording.hotkey_mode == HotkeyMode::Toggle,
        vad: VadOptions::new(
            Duration::from_millis(config.recording.speech_confirmation_ms.into()),
            Duration::from_millis(config.recording.internal_pause_ms.into()),
            Duration::from_millis(config.recording.endpoint_silence_ms.into()),
            Duration::from_millis(config.recording.pre_roll_ms.into()),
            Duration::from_millis(config.recording.post_roll_ms.into()),
        ),
        sensitivity: Sensitivity::Manual {
            activation_rms: config.recording.manual_activation_rms,
        },
        intent: CaptureIntent::Dictation,
    }
}

fn rms_to_dbfs(rms: f32) -> f32 {
    if !rms.is_finite() || rms <= 0.0 {
        return INPUT_LEVEL_MIN_DBFS;
    }
    (20.0 * rms.log10()).clamp(INPUT_LEVEL_MIN_DBFS, INPUT_LEVEL_MAX_DBFS)
}

fn dbfs_to_rms(dbfs: f32) -> f32 {
    let dbfs = if dbfs.is_finite() {
        dbfs.clamp(INPUT_LEVEL_MIN_DBFS, INPUT_LEVEL_MAX_DBFS)
    } else {
        INPUT_LEVEL_MIN_DBFS
    };
    10.0_f32.powf(dbfs / 20.0).clamp(
        config::settings::MIN_MANUAL_ACTIVATION_RMS,
        config::settings::MAX_MANUAL_ACTIVATION_RMS,
    )
}

fn dbfs_to_slider_position(dbfs: f32) -> f32 {
    ((dbfs.clamp(INPUT_LEVEL_MIN_DBFS, INPUT_LEVEL_MAX_DBFS) - INPUT_LEVEL_MIN_DBFS)
        / (INPUT_LEVEL_MAX_DBFS - INPUT_LEVEL_MIN_DBFS))
        .clamp(0.0, 1.0)
}

fn slider_position_to_dbfs(position: f32) -> f32 {
    INPUT_LEVEL_MIN_DBFS + position.clamp(0.0, 1.0) * (INPUT_LEVEL_MAX_DBFS - INPUT_LEVEL_MIN_DBFS)
}

fn rms_to_slider_position(rms: f32) -> f32 {
    dbfs_to_slider_position(rms_to_dbfs(rms))
}

#[derive(Clone, Debug)]
struct MicrophoneLevelEnvelope {
    position: f32,
    last_step_at: Option<Instant>,
    last_fresh_sample_at: Option<Instant>,
    last_revision: Option<u64>,
}

impl Default for MicrophoneLevelEnvelope {
    fn default() -> Self {
        Self {
            position: 0.0,
            last_step_at: None,
            last_fresh_sample_at: None,
            last_revision: None,
        }
    }
}

impl MicrophoneLevelEnvelope {
    fn reset_source(&mut self) {
        self.last_fresh_sample_at = None;
        self.last_revision = None;
    }

    fn clear(&mut self) {
        *self = Self::default();
    }

    fn update(
        &mut self,
        rms: f32,
        revision: Option<u64>,
        source_active: bool,
        now: Instant,
    ) -> f32 {
        if source_active && revision != self.last_revision {
            self.last_revision = revision;
            self.last_fresh_sample_at = Some(now);
        }
        let fresh = source_active
            && self.last_fresh_sample_at.is_some_and(|observed| {
                now.saturating_duration_since(observed) <= INPUT_LEVEL_STALE_AFTER
            });
        let target = if fresh {
            rms_to_slider_position(rms)
        } else {
            0.0
        };
        let elapsed = self.last_step_at.map_or(METER_REPAINT_DELAY, |previous| {
            now.saturating_duration_since(previous)
        });
        self.last_step_at = Some(now);
        let time_constant = if target > self.position {
            INPUT_LEVEL_ATTACK
        } else {
            INPUT_LEVEL_RELEASE
        };
        let alpha = 1.0 - (-elapsed.as_secs_f32() / time_constant.as_secs_f32()).exp();
        self.position += (target - self.position) * alpha.clamp(0.0, 1.0);
        if target == 0.0 && self.position < 0.001 {
            self.position = 0.0;
        }
        self.position.clamp(0.0, 1.0)
    }

    fn is_animating(&self) -> bool {
        self.position > 0.0
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct NoSpeechFeedback {
    status_message: String,
    overlay_message: &'static str,
}

fn no_speech_feedback(
    maximum_input_rms: Option<f32>,
    input_device_name: Option<&str>,
    activation_floor: f32,
) -> NoSpeechFeedback {
    let input_was_too_low = maximum_input_rms
        .filter(|rms| rms.is_finite())
        .is_some_and(|rms| rms < activation_floor);
    if !input_was_too_low {
        return NoSpeechFeedback {
            status_message: "No speech detected; nothing was pasted.".to_owned(),
            overlay_message: "No speech detected",
        };
    }

    let is_fifine = input_device_name.is_some_and(|name| {
        name.to_ascii_lowercase()
            .split_whitespace()
            .any(|part| part.contains("fifine"))
    });
    let status_message = if is_fifine {
        "FIFINE microphone signal was silent or too low. Tap its top mute control, turn up the physical gain knob, move closer, and try again."
    } else {
        "Microphone signal was silent or too low. Check its hardware mute and gain, move closer, verify the selected input, and try again."
    };
    NoSpeechFeedback {
        status_message: status_message.to_owned(),
        overlay_message: "Microphone signal too low — check mute and gain",
    }
}

fn no_speech_feedback_for_capture(
    maximum_input_rms: Option<f32>,
    diagnostics: &CaptureDiagnosticContext,
) -> NoSpeechFeedback {
    no_speech_feedback(
        maximum_input_rms,
        diagnostics.input_device_name.as_deref(),
        diagnostics.activation_floor,
    )
}

fn diagnostic_activation_floor(config: &AppConfig) -> f32 {
    config.recording.manual_activation_rms
}

fn discard_recording_async(session: RecordingSession) {
    let _ = thread::Builder::new()
        .name("scribe-stale-audio-cleanup".to_owned())
        .spawn(move || {
            let _ = session.stop_and_discard(Duration::from_secs(2));
        });
}

fn history_retention_policy(config: &AppConfig) -> HistoryRetentionPolicy {
    HistoryRetentionPolicy {
        max_unpinned_entries: config.history.max_unpinned_entries,
        transcript_retention_days: config.history.transcript_retention_days,
        audio_retention_days: config.history.audio_retention_days,
    }
}

enum RetryReleaseAttempt {
    Acknowledged { retention_error: Option<String> },
    Retryable(String),
    WorkerDisconnected(String),
}

fn observe_accepted_retry_release(
    reply: Receiver<crate::history::RetryReleaseAcknowledgement>,
) -> RetryReleaseAttempt {
    match reply.recv() {
        Ok(acknowledgement) => RetryReleaseAttempt::Acknowledged {
            retention_error: acknowledgement.retention_error,
        },
        Err(_) => {
            RetryReleaseAttempt::WorkerDisconnected("release reply channel disconnected".into())
        }
    }
}

fn retry_release_until_acknowledged(
    mut attempt: impl FnMut() -> RetryReleaseAttempt,
    mut pause: impl FnMut(Duration),
) -> (bool, Result<(), String>) {
    let mut last_error = "retry lease release was not attempted".to_owned();
    for attempt_index in 0..RETRY_RELEASE_ATTEMPTS {
        match attempt() {
            RetryReleaseAttempt::Acknowledged { retention_error } => {
                return match retention_error {
                    Some(error) => (
                        true,
                        Err(format!(
                            "retry lease was released but retention failed: {error}"
                        )),
                    ),
                    None => (true, Ok(())),
                };
            }
            RetryReleaseAttempt::WorkerDisconnected(error) => {
                return (
                    true,
                    Err(format!(
                        "history worker disconnected while releasing retry ownership: {error}"
                    )),
                );
            }
            RetryReleaseAttempt::Retryable(error) => last_error = error,
        }
        if attempt_index + 1 < RETRY_RELEASE_ATTEMPTS {
            pause(Duration::from_millis(25_u64 << attempt_index));
        }
    }
    (
        false,
        Err(format!(
            "retry lease release was not acknowledged after {RETRY_RELEASE_ATTEMPTS} attempts: {last_error}"
        )),
    )
}

fn release_history_retry_with_bounded_retry(
    store: &HistoryStore,
    history_id: i64,
) -> (bool, Result<(), String>) {
    retry_release_until_acknowledged(
        || match store.enqueue_release_retry(history_id) {
            // Once admission succeeds, keep the receiver and wait on this
            // background thread. Dropping it on a timeout would lose a late
            // acknowledgement after the worker had already removed the lease.
            Ok(reply) => observe_accepted_retry_release(reply),
            Err(HistoryError::WorkerUnavailable | HistoryError::WorkerStopped) => {
                RetryReleaseAttempt::WorkerDisconnected("command channel disconnected".into())
            }
            Err(error) => RetryReleaseAttempt::Retryable(error.to_string()),
        },
        thread::sleep,
    )
}

/// Persists a failed retry and, if that terminal request is not cleanly
/// observed, queues an explicit idempotent lease release behind it. The caller
/// keeps its UI-side lease until `retry_lease_released` is true.
fn settle_failed_history_retry(
    store: &HistoryStore,
    history_id: i64,
    failure: impl Into<String>,
) -> (bool, Result<(), String>) {
    match store.fail_retry(history_id, failure) {
        Ok(_) => (true, Ok(())),
        Err(terminal_error) => {
            let (released, release_result) =
                release_history_retry_with_bounded_retry(store, history_id);
            let error = match release_result {
                Ok(()) => terminal_error.to_string(),
                Err(release_error) => format!("{terminal_error}; {release_error}"),
            };
            (released, Err(error))
        }
    }
}

type Tab = AppPage;

fn initial_tab() -> Tab {
    std::env::var("SCRIBE_START_TAB")
        .ok()
        .and_then(|value| tab_from_env_value(&value))
        .unwrap_or(Tab::Transcribe)
}

fn tab_from_env_value(value: &str) -> Option<Tab> {
    match value.trim().to_ascii_lowercase().as_str() {
        "transcribe" => Some(Tab::Transcribe),
        "models" | "model" => Some(Tab::Models),
        "general" | "settings" => Some(Tab::General),
        "history" => Some(Tab::History),
        "advanced" => Some(Tab::Advanced),
        "about" => Some(Tab::About),
        "debug" | "playground" | "model-playground" | "model playground" => Some(Tab::Debug),
        _ => None,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RecordingSource {
    Transcribe,
    Playground,
}

impl RecordingSource {
    fn purpose(self) -> SessionPurpose {
        match self {
            Self::Transcribe => SessionPurpose::Dictation,
            Self::Playground => SessionPurpose::Comparison,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TriggerObservation {
    AppAction,
    HotkeyPoll,
}

#[derive(Clone, Copy, Debug)]
struct DeferredRecordingStart {
    source: RecordingSource,
    activation_at: Instant,
    trigger_observation: TriggerObservation,
}

struct ActiveRecording {
    session_id: SessionId,
    session: RecordingSession,
    source: RecordingSource,
    stop_requested: bool,
    started_at: Instant,
    max_duration_seconds: u32,
    latency: LatencyTrace,
    capture_diagnostics: CaptureDiagnosticContext,
}

struct PendingRecording {
    session_id: SessionId,
    source: RecordingSource,
    stop_requested: bool,
    max_duration_seconds: u32,
    latency: LatencyTrace,
    capture_diagnostics: CaptureDiagnosticContext,
    abandon: Arc<AtomicBool>,
}

#[derive(Default)]
enum MicrophoneTest {
    #[default]
    Idle,
    Starting {
        request_id: u64,
        stop_requested: bool,
        cancellation: CaptureCancellation,
    },
    Active {
        session: RecordingSession,
    },
    Stopping {
        session: RecordingSession,
    },
}

impl MicrophoneTest {
    fn session(&self) -> Option<&RecordingSession> {
        match self {
            Self::Active { session } | Self::Stopping { session } => Some(session),
            Self::Idle | Self::Starting { .. } => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
struct CaptureDiagnosticContext {
    activation_floor: f32,
    input_device_name: Option<String>,
}

impl CaptureDiagnosticContext {
    fn from_config(config: &AppConfig) -> Self {
        Self {
            activation_floor: diagnostic_activation_floor(config),
            input_device_name: config.recording.audio_input_device_name.clone(),
        }
    }
}

impl Default for CaptureDiagnosticContext {
    fn default() -> Self {
        Self {
            activation_floor: audio::MIN_SPEECH_ACTIVATION_RMS,
            input_device_name: None,
        }
    }
}

struct PendingOutput {
    session_id: SessionId,
    history_id: Option<i64>,
    transcript: String,
    completion_message: String,
    config: AppConfig,
    latency: Option<LatencyTrace>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum HistoryRequestKind {
    Dictation,
    Retry,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct HistoryRequestContext {
    id: i64,
    kind: HistoryRequestKind,
}

struct PendingHistoryCompletion {
    session_id: SessionId,
    history_id: i64,
    kind: HistoryRequestKind,
    transcript: String,
    output_config: AppConfig,
    completion_message: String,
    latency: Option<LatencyTrace>,
}

struct HistoryCapturePlan {
    store: HistoryStore,
    entry: NewHistoryEntry,
    retain_audio: bool,
}

struct ArmedHistoryRepaste {
    id: i64,
    text: String,
    expires_at: Instant,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct FinalizedText {
    raw: String,
    final_text: String,
}

impl FinalizedText {
    fn without_cleanup(raw: String) -> Option<Self> {
        if raw.trim().is_empty() {
            return None;
        }
        Some(Self {
            final_text: raw.clone(),
            raw,
        })
    }
}

struct FinishedCapture {
    session_id: SessionId,
    source: RecordingSource,
    result: Result<CaptureCompletion, CaptureError>,
    stop_requested: bool,
    started_at: Instant,
    latency: LatencyTrace,
    capture_diagnostics: CaptureDiagnosticContext,
}

enum PreviewDrainAction {
    Continue,
    FinishCapture(Box<FinishedCapture>),
    ReapAfterFailure,
    Fail {
        session_id: SessionId,
        message: String,
    },
    Cancel {
        session_id: SessionId,
    },
}

struct PendingPreviewDrain {
    preview: RollingPreviewHandle,
    action: PreviewDrainAction,
    closed_at: Instant,
    cancel_requested_at: Option<Instant>,
    timeout_reported: bool,
}

struct PlaygroundRunState {
    pending_requests: HashMap<RequestId, String>,
    _audio: Arc<PreparedAudio>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct TrayUiState {
    is_recording: bool,
    has_transcript: bool,
}

#[derive(Clone, Debug)]
struct LatencyTrace {
    activation_at: Instant,
    trigger_observation: TriggerObservation,
    overlay_visible_at: Option<Instant>,
    recorder_started_at: Option<Instant>,
    first_meter_update_at: Option<Instant>,
    model_load_started_at: Option<Instant>,
    model_loaded_at: Option<Instant>,
    first_partial_at: Option<Instant>,
    stop_requested_at: Option<Instant>,
    capture_finalized_at: Option<Instant>,
    transcription_dispatched_at: Option<Instant>,
    transcription_job_completed_at: Option<Instant>,
    final_text_ready_at: Option<Instant>,
    ui_result_at: Option<Instant>,
    output_started_at: Option<Instant>,
    target_activated_at: Option<Instant>,
    paste_completed_at: Option<Instant>,
    output_completed_at: Option<Instant>,
    model_id: Option<String>,
    resolved_backend: Option<String>,
    compute_backend: Option<String>,
    streaming_mode: Option<String>,
    cold_or_warm: Option<String>,
    reported_model_load_ms: Option<u64>,
    audio_duration_ms: Option<u64>,
    processing_duration_ms: Option<u64>,
    maximum_input_rms: Option<f32>,
    maximum_input_peak: Option<f32>,
    capture_diagnostics: Option<CaptureDiagnosticContext>,
}

impl LatencyTrace {
    fn started_at(activation_at: Instant, trigger_observation: TriggerObservation) -> Self {
        Self {
            activation_at,
            trigger_observation,
            overlay_visible_at: None,
            recorder_started_at: None,
            first_meter_update_at: None,
            model_load_started_at: None,
            model_loaded_at: None,
            first_partial_at: None,
            stop_requested_at: None,
            capture_finalized_at: None,
            transcription_dispatched_at: None,
            transcription_job_completed_at: None,
            final_text_ready_at: None,
            ui_result_at: None,
            output_started_at: None,
            target_activated_at: None,
            paste_completed_at: None,
            output_completed_at: None,
            model_id: None,
            resolved_backend: None,
            compute_backend: None,
            streaming_mode: None,
            cold_or_warm: None,
            reported_model_load_ms: None,
            audio_duration_ms: None,
            processing_duration_ms: None,
            maximum_input_rms: None,
            maximum_input_peak: None,
            capture_diagnostics: None,
        }
    }

    fn observe_session_context(&mut self, model_id: Option<String>, streaming_mode: StreamingMode) {
        self.model_id = model_id;
        self.streaming_mode = Some(
            match streaming_mode {
                StreamingMode::Auto => "auto",
                StreamingMode::Rolling => "rolling",
                StreamingMode::FinalOnly => "final_only",
            }
            .to_owned(),
        );
    }

    fn observe_transcription_outcome(&mut self, outcome: &TranscriptionOutcome) {
        self.model_id = Some(outcome.model_id.as_str().to_owned());
        self.resolved_backend = Some(outcome.resolved_backend_label().to_owned());
        self.compute_backend = outcome
            .resolved_acceleration
            .as_ref()
            .map(|acceleration| acceleration.resolved.label().to_owned());
        self.cold_or_warm = Some(
            if outcome.warm_model_reused {
                "warm"
            } else {
                "cold"
            }
            .to_owned(),
        );
        self.reported_model_load_ms = outcome
            .model_load_duration_ms
            .and_then(|value| u64::try_from(value).ok());
        self.audio_duration_ms = outcome
            .transcript
            .duration_ms
            .and_then(|value| u64::try_from(value).ok());
        self.processing_duration_ms = outcome
            .processing_duration_ms
            .and_then(|value| u64::try_from(value).ok());
    }

    fn observe_capture_metrics(&mut self, metrics: &CaptureMetrics) {
        self.maximum_input_rms = Some(metrics.maximum_input_rms);
        self.maximum_input_peak = Some(metrics.maximum_input_peak);
    }

    fn diagnostic_snapshot(
        &self,
        session_id: SessionId,
        outcome: DiagnosticSessionOutcome,
        failure_stage: Option<DiagnosticFailureStage>,
    ) -> SessionDiagnostic {
        let millis = |start: Option<Instant>, end: Option<Instant>| {
            end?.checked_duration_since(start?)?
                .as_millis()
                .try_into()
                .ok()
        };
        let realtime_factor = self
            .processing_duration_ms
            .zip(self.audio_duration_ms)
            .filter(|(_, audio)| *audio > 0)
            .map(|(processing, audio)| processing as f64 / audio as f64);
        SessionDiagnostic {
            session_id: session_id.0,
            outcome,
            failure_stage,
            trigger: match self.trigger_observation {
                TriggerObservation::AppAction => "app_action",
                TriggerObservation::HotkeyPoll => "hotkey_poll",
            },
            model_id: self.model_id.clone(),
            // Architecture and package version are intentionally absent until
            // the application-facing service exposes verified neutral values.
            model_architecture: None,
            resolved_backend: self.resolved_backend.clone(),
            runtime_package_version: None,
            compute_backend: self.compute_backend.clone(),
            streaming_mode: self.streaming_mode.clone(),
            cold_or_warm: self.cold_or_warm.clone(),
            metrics: DiagnosticSessionMetrics {
                hotkey_to_overlay_visible_ms: millis(
                    Some(self.activation_at),
                    self.overlay_visible_at,
                ),
                hotkey_to_capture_started_ms: millis(
                    Some(self.activation_at),
                    self.recorder_started_at,
                ),
                hotkey_to_first_meter_update_ms: millis(
                    Some(self.activation_at),
                    self.first_meter_update_at,
                ),
                maximum_input_rms: self.maximum_input_rms,
                maximum_input_peak: self.maximum_input_peak,
                // The existing recorder reports meter cadence, not a verified
                // speech-onset timestamp. Preserve that distinction as null.
                speech_start_detected_ms: None,
                model_load_ms: millis(self.model_load_started_at, self.model_loaded_at)
                    .or(self.reported_model_load_ms),
                first_partial_ms: millis(Some(self.activation_at), self.first_partial_at),
                recording_duration_ms: millis(self.recorder_started_at, self.stop_requested_at),
                stop_to_capture_finalized_ms: millis(
                    self.stop_requested_at,
                    self.capture_finalized_at,
                ),
                recording_end_to_final_text_ms: millis(
                    self.stop_requested_at,
                    self.final_text_ready_at,
                ),
                // No separately measured post-processing phase exists yet.
                post_processing_ms: None,
                final_text_to_paste_ms: millis(self.final_text_ready_at, self.paste_completed_at),
                final_text_to_output_completed_ms: millis(
                    self.final_text_ready_at,
                    self.output_completed_at,
                ),
                total_end_to_end_ms: millis(
                    Some(self.activation_at),
                    self.final_observed_instant(),
                ),
                realtime_factor,
            },
        }
    }

    fn summary_lines(&self) -> Vec<String> {
        let mut lines = Vec::new();
        let trigger_label = match self.trigger_observation {
            TriggerObservation::AppAction => "App action",
            TriggerObservation::HotkeyPoll => "Hotkey dequeued",
        };
        if let Some(duration) = duration_between(Some(self.activation_at), self.overlay_visible_at)
        {
            lines.push(format!("{trigger_label} to overlay visible: {duration}"));
        }
        if let Some(duration) = duration_between(Some(self.activation_at), self.recorder_started_at)
        {
            lines.push(format!("{trigger_label} to recorder ready: {duration}"));
        }
        if let Some(duration) =
            duration_between(Some(self.activation_at), self.first_meter_update_at)
        {
            lines.push(format!("{trigger_label} to first meter update: {duration}"));
        }
        if let Some(duration) = duration_between(self.model_load_started_at, self.model_loaded_at) {
            lines.push(format!("Model load: {duration}"));
        }
        if let Some(duration) = duration_between(Some(self.activation_at), self.first_partial_at) {
            lines.push(format!("{trigger_label} to first partial: {duration}"));
        }
        if let Some(duration) = duration_between(self.stop_requested_at, self.capture_finalized_at)
        {
            lines.push(format!("Stop to audio finalized: {duration}"));
        }
        if let Some(duration) = duration_between(
            self.transcription_dispatched_at,
            self.transcription_job_completed_at,
        ) {
            lines.push(format!("Transcription job: {duration}"));
        }
        if let Some(duration) = duration_between(self.stop_requested_at, self.final_text_ready_at) {
            lines.push(format!("Stop to final text: {duration}"));
        }
        if let Some(duration) =
            duration_between(self.transcription_job_completed_at, self.ui_result_at)
        {
            lines.push(format!("STT done to UI update: {duration}"));
        }
        if let Some(duration) = duration_between(self.output_started_at, self.target_activated_at) {
            lines.push(format!("Output start to target activation: {duration}"));
        }
        if let Some(duration) = duration_between(self.target_activated_at, self.paste_completed_at)
        {
            lines.push(format!("Target activation to paste: {duration}"));
        }
        if let Some(duration) = duration_between(self.final_text_ready_at, self.paste_completed_at)
        {
            lines.push(format!("Final text ready to paste complete: {duration}"));
        }
        if let Some(duration) = duration_between(self.output_started_at, self.output_completed_at) {
            lines.push(format!("Focused-app output: {duration}"));
        }
        if let Some(duration) =
            duration_between(Some(self.activation_at), self.final_observed_instant())
        {
            lines.push(format!("Total observed: {duration}"));
        }
        lines
    }

    fn final_observed_instant(&self) -> Option<Instant> {
        self.output_completed_at
            .or(self.paste_completed_at)
            .or(self.ui_result_at)
            .or(self.transcription_job_completed_at)
            .or(self.capture_finalized_at)
            .or(self.stop_requested_at)
            .or(self.recorder_started_at)
    }
}

fn duration_between(start: Option<Instant>, end: Option<Instant>) -> Option<String> {
    Some(format_duration_ms(end?.saturating_duration_since(start?)))
}

fn format_duration_ms(duration: Duration) -> String {
    format!("{} ms", duration.as_millis())
}

fn effective_native_overlay_mode(mode: OverlayMode) -> NativeOverlayMode {
    if !overlay::overlay_focus_safety_available() {
        return NativeOverlayMode::Off;
    }
    match mode {
        OverlayMode::Live => NativeOverlayMode::Live,
        OverlayMode::Minimal => NativeOverlayMode::Minimal,
        OverlayMode::Off => NativeOverlayMode::Off,
    }
}

fn rolling_preview_enabled(source: RecordingSource, mode: StreamingMode) -> bool {
    source == RecordingSource::Transcribe && mode != StreamingMode::FinalOnly
}

fn native_overlay_position(position: OverlayPosition) -> NativeOverlayPosition {
    match position {
        OverlayPosition::Top => NativeOverlayPosition::TopCenter,
        OverlayPosition::Bottom => NativeOverlayPosition::BottomCenter,
    }
}

#[derive(Clone, Debug)]
struct PlaygroundCardState {
    descriptor: ModelDescriptor,
    install_status: ModelInstallStatus,
    status: ModelRuntimeStatus,
    transcript: String,
    latency_ms: Option<u128>,
    audio_duration_ms: Option<u128>,
    peak_ram_mb: Option<f64>,
    peak_vram_mb: Option<f64>,
}

#[derive(Clone, Copy, Debug)]
struct ComparisonProjectionCacheEntry {
    reference_revision: u64,
    output_revision: u64,
    word_error_rate: Option<f32>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct RemoteCatalogProjectionKey {
    revision: u64,
    inventory_revision: u64,
    search: String,
    language_filter: ModelLanguageFilter,
    filters: RemoteCatalogFilters,
    sort: RemoteCatalogSort,
    mutation_block_reason: Option<String>,
}

#[derive(Clone, Debug)]
struct RemoteCatalogProjection {
    key: RemoteCatalogProjectionKey,
    matching_count: usize,
    total_count: usize,
    entries: Vec<RemoteCatalogEntryView>,
}

struct RemoteCatalogState {
    snapshot: Option<ModelInventorySnapshot>,
    local_models: Arc<[ModelViewModel]>,
    local_models_dirty: bool,
    loading: bool,
    force_refresh_requested: bool,
    refresh_generation: u64,
    active_refresh_generation: Option<u64>,
    error: Option<String>,
    projection_revision: u64,
    projection: Option<RemoteCatalogProjection>,
    #[cfg(test)]
    projection_build_count: usize,
    #[cfg(test)]
    disk_probe_count: usize,
    #[cfg(test)]
    local_models_build_count: usize,
    #[cfg(test)]
    catalog_io_request_count: usize,
}

impl Default for RemoteCatalogState {
    fn default() -> Self {
        Self {
            snapshot: Some(ModelInventorySnapshot::bundled()),
            local_models: Arc::default(),
            local_models_dirty: true,
            loading: false,
            force_refresh_requested: false,
            refresh_generation: 1,
            active_refresh_generation: None,
            error: None,
            projection_revision: 0,
            projection: None,
            #[cfg(test)]
            projection_build_count: 0,
            #[cfg(test)]
            disk_probe_count: 0,
            #[cfg(test)]
            local_models_build_count: 0,
            #[cfg(test)]
            catalog_io_request_count: 0,
        }
    }
}

impl RemoteCatalogState {
    fn invalidate_projection(&mut self) {
        self.projection_revision = self.projection_revision.wrapping_add(1);
        self.projection = None;
    }

    fn invalidate_local_models(&mut self) {
        self.local_models_dirty = true;
        self.invalidate_projection();
    }
}

struct LocalGgufImportJob {
    job_id: u64,
    cancellation: InstallCancellation,
    completion: Receiver<()>,
    worker: Option<thread::JoinHandle<()>>,
}

impl LocalGgufImportJob {
    fn cancel_and_wait(&mut self, timeout: Duration) -> bool {
        self.cancel_and_wait_with(timeout, |completion, timeout| {
            matches!(
                completion.recv_timeout(timeout),
                Ok(()) | Err(crossbeam_channel::RecvTimeoutError::Disconnected)
            )
        })
    }

    fn cancel_and_wait_with(
        &mut self,
        timeout: Duration,
        wait: impl FnOnce(&Receiver<()>, Duration) -> bool,
    ) -> bool {
        self.cancellation.cancel();
        if !wait(&self.completion, timeout) {
            return false;
        }
        self.worker
            .take()
            .is_none_or(|worker| worker.join().is_ok())
    }

    fn reap_completed(&mut self) {
        if matches!(
            self.completion.try_recv(),
            Ok(()) | Err(crossbeam_channel::TryRecvError::Disconnected)
        ) {
            let _ = self.worker.take().map(thread::JoinHandle::join);
        }
    }
}

enum PlaygroundAction {
    Clear(String),
    MoveBy {
        model_id: String,
        offset: isize,
    },
    MoveBefore {
        dragged_id: String,
        target_id: String,
    },
}

enum AppEvent {
    CaptureReady {
        session_id: SessionId,
        result: Result<RecordingSession, audio::CaptureError>,
    },
    MicrophoneTestReady {
        request_id: u64,
        result: Result<RecordingSession, audio::CaptureError>,
    },
    ModelPreloadFinished {
        session_id: SessionId,
        model_id: ModelId,
        load_duration_ms: u128,
    },
    ModelPreloadFailed {
        session_id: SessionId,
        model_id: ModelId,
        message: String,
    },
    TranscriptionDone {
        source: RecordingSource,
        session_id: SessionId,
        request_id: RequestId,
        result: Box<TranscriptionOutcome>,
        latency: Option<LatencyTrace>,
    },
    PlaygroundModelStarted {
        session_id: SessionId,
        request_id: RequestId,
        model_id: String,
    },
    TranscriptionFailed {
        source: RecordingSource,
        session_id: SessionId,
        request_id: RequestId,
        model_id: String,
        message: String,
        latency: Option<LatencyTrace>,
    },
    HistoryCompletionObserved {
        history_id: i64,
        retry_lease_released: bool,
        result: Result<(), String>,
    },
    HistoryRetryStartFailed {
        session_id: SessionId,
        request_id: RequestId,
        history_id: i64,
        retry_lease_released: bool,
        message: String,
    },
    HistoryPageLoaded {
        query_id: u64,
        append: bool,
        search: String,
        result: Result<HistoryPage, String>,
    },
    HistoryMutationFinished {
        operation_id: u64,
        message: String,
        result: Result<(), String>,
    },
    HistoryPlaybackPathReady {
        history_id: i64,
        result: Result<Option<PathBuf>, String>,
    },
    HistoryOutputRecorded {
        result: Result<(), String>,
    },
    HistoryRetryTerminalPersisted {
        history_id: i64,
        retry_lease_released: bool,
        result: Result<(), String>,
    },
    RemoteCatalogLoaded {
        generation: u64,
        result: Result<ModelInventorySnapshot, String>,
    },
    ModelDownloadProgress {
        job_id: u64,
        model_id: String,
        progress: InstallProgress,
    },
    VerifiedInstallDone {
        job_id: u64,
        model_id: String,
        result: Box<VerifiedInstallResult>,
    },
    VerifiedInstallFailed {
        job_id: u64,
        model_id: String,
        message: String,
        recovery_required: bool,
    },
    LocalGgufImportFinished {
        job_id: u64,
        result: Result<Box<ValidatedLocalGgufImport>, String>,
    },
    RuntimeInstallDone {
        runtime_id: String,
        runtime_label: String,
        replacement: RuntimeReplacement,
        source_label: &'static str,
    },
    RuntimeInstallFailed {
        runtime_id: String,
        message: String,
    },
}

#[derive(Debug)]
struct VerifiedInstallResult {
    model: FileReplacement,
    manifest: FileReplacement,
    runtime: Option<DirectoryReplacement>,
    runtime_id: String,
    runtime_entrypoint: Option<PathBuf>,
    runtime_version: Option<String>,
    runtime_package_id: Option<String>,
    runtime_archive_sha256: Option<String>,
    retain_runtime_as_previous: bool,
    model_sha256: String,
    smoke: InstallSmoke,
    journal: ActivationJournal,
    remote_install: Option<config::ManagedRemoteModelInstall>,
}

/// A local source has been fully re-hashed and exercised by the isolated
/// embedded runtime. The UI thread owns the final receipt/config transaction.
#[derive(Debug)]
struct ValidatedLocalGgufImport {
    model_id: ModelId,
    install: config::ImportedGgufModelInstall,
    manifest: installed_manifest::InstalledModelManifest,
    smoke: InstallSmoke,
}

#[derive(Debug)]
struct InstallJobFailure {
    message: String,
    recovery_required: bool,
}

impl InstallJobFailure {
    fn normal(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            recovery_required: false,
        }
    }

    fn recovery_required(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            recovery_required: true,
        }
    }
}

impl From<String> for InstallJobFailure {
    fn from(message: String) -> Self {
        Self::normal(message)
    }
}

impl From<InstallError> for InstallJobFailure {
    fn from(error: InstallError) -> Self {
        if error.requires_recovery() {
            Self::recovery_required(error.to_string())
        } else {
            Self::normal(error.to_string())
        }
    }
}

fn send_install_progress(
    tx: &Sender<AppEvent>,
    job_id: u64,
    model_id: &str,
    progress: InstallProgress,
) {
    let _ = tx.send(AppEvent::ModelDownloadProgress {
        job_id,
        model_id: model_id.to_owned(),
        progress,
    });
}

fn send_verified_install_result(
    tx: &Sender<AppEvent>,
    job_id: u64,
    model_id: String,
    result: Result<VerifiedInstallResult, InstallJobFailure>,
) {
    match result {
        Ok(result) => {
            let _ = tx.send(AppEvent::VerifiedInstallDone {
                job_id,
                model_id,
                result: Box::new(result),
            });
        }
        Err(failure) => {
            let _ = tx.send(AppEvent::VerifiedInstallFailed {
                job_id,
                model_id,
                message: failure.message,
                recovery_required: failure.recovery_required,
            });
        }
    }
}

fn activation_journal_path() -> PathBuf {
    config::runtime_storage_dir().join("activation-journal.json")
}

fn failure_after_safe_rollback(
    journal: ActivationJournal,
    message: impl Into<String>,
) -> InstallJobFailure {
    let message = message.into();
    match journal.clear() {
        Ok(()) => InstallJobFailure::normal(message),
        Err(clear_error) => InstallJobFailure::recovery_required(format!(
            "{message}. Rollback completed, but the activation journal could not be cleared: {clear_error}"
        )),
    }
}

fn failure_after_activated_artifact_rollback(
    journal: ActivationJournal,
    model: FileReplacement,
    manifest: Option<FileReplacement>,
    runtime: Option<DirectoryReplacement>,
    message: impl Into<String>,
) -> InstallJobFailure {
    let manifest_rollback = manifest.and_then(|replacement| replacement.rollback().err());
    let model_rollback = model.rollback().err();
    let runtime_rollback = runtime.and_then(|replacement| replacement.rollback().err());
    if manifest_rollback.is_none() && model_rollback.is_none() && runtime_rollback.is_none() {
        return failure_after_safe_rollback(journal, message);
    }
    let message = message.into();
    InstallJobFailure::recovery_required(format!(
        "{message}{}{}{}",
        manifest_rollback
            .as_ref()
            .map(|error| format!(". Installed-model manifest rollback also failed: {error}"))
            .unwrap_or_default(),
        model_rollback
            .as_ref()
            .map(|error| format!(". Model rollback also failed: {error}"))
            .unwrap_or_default(),
        runtime_rollback
            .as_ref()
            .map(|error| format!(". Runtime rollback also failed: {error}"))
            .unwrap_or_default(),
    ))
}

struct VerifiedInstallRequest {
    config: AppConfig,
    service: TranscriptionService,
    runtime_id: String,
    model_id: ModelId,
    existing_runtime_root: Option<PathBuf>,
    force_runtime_package: bool,
    cancellation: InstallCancellation,
    source: VerifiedInstallSource,
}

/// The UI can only construct this request from a typed backend catalog card.
/// It carries source facts, never an arbitrary URL or filesystem path.
#[derive(Clone, Debug)]
struct TrustedRemoteInstallRequest {
    artifact: TrustedArtifact,
    display_name: String,
    description: String,
    languages: Vec<String>,
    recommended: bool,
}

#[derive(Clone, Debug)]
enum VerifiedInstallSource {
    NormalizedCatalog,
    TrustedRemote(TrustedRemoteInstallRequest),
}

#[derive(Clone, Debug)]
enum RemoteModelCardAction {
    InstallNormalized(ModelId),
    InstallTrusted(TrustedRemoteInstallRequest),
    CancelInstall(ModelId),
    SelectInstalled(ModelId),
    RemoveInstalled(ModelId),
}

fn run_verified_install(
    request: VerifiedInstallRequest,
    progress: &dyn Fn(InstallProgress),
) -> Result<VerifiedInstallResult, InstallJobFailure> {
    let VerifiedInstallRequest {
        config,
        service,
        runtime_id,
        model_id,
        existing_runtime_root,
        force_runtime_package,
        cancellation,
        source,
    } = request;
    let (model, model_uses_embedded_runtime, manifest_source, remote_install_request) = match source
    {
        VerifiedInstallSource::NormalizedCatalog => {
            let model =
                managed_downloads::prepare_model(&config, &model_id, &cancellation, progress)
                    .map_err(InstallJobFailure::from)?;
            let source = installed_manifest::ArtifactSource::normalized(&model_id)
                .map_err(|error| error.to_string())?;
            (
                model,
                crate::model_catalog::model_uses_embedded_runtime(&model_id),
                source,
                None,
            )
        }
        VerifiedInstallSource::TrustedRemote(request) => {
            let model = managed_downloads::prepare_trusted_gguf_model(
                &config,
                &request.artifact,
                &cancellation,
                progress,
            )
            .map_err(InstallJobFailure::from)?;
            let source = installed_manifest::ArtifactSource::trusted_gguf(
                request.artifact.model_id.clone(),
                request.artifact.revision.clone(),
                request.artifact.filename.clone(),
                request.artifact.size_bytes,
                request.artifact.expected_sha256.clone(),
            );
            (model, true, source, Some(request))
        }
    };
    if cancellation.is_cancelled() {
        return Err(InstallJobFailure::normal(
            "Installation cancelled. The verified partial was retained for Resume.",
        ));
    }

    let target_root = config::runtime_storage_dir().join(&runtime_id);
    let mut staged_runtime = None;
    let mut runtime_entrypoint = None;
    let mut runtime_version = None;
    let mut runtime_package_id = None;
    let mut runtime_archive_sha256 = None;
    let mut candidate_root = (!model_uses_embedded_runtime)
        .then_some(existing_runtime_root)
        .flatten();

    let smoke_current = (!model_uses_embedded_runtime)
        .then_some(candidate_root.as_ref())
        .flatten()
        .and_then(|root| {
            progress(InstallProgress {
                stage: InstallStage::HealthChecking,
                completed_bytes: model.size_bytes,
                total_bytes: model.size_bytes,
                bytes_per_second: None,
            });
            service
                .verify_installation_candidate(
                    InstallationCandidate::normalized(
                        model_id.clone(),
                        model.path.clone(),
                        Some(root.clone()),
                    )
                    .ok()?,
                    &cancellation,
                )
                .ok()
        });
    let current_runtime_known_good = smoke_current.is_some()
        && candidate_root
            .as_ref()
            .is_some_and(|root| fs::canonicalize(root).ok() == fs::canonicalize(&target_root).ok());

    let smoke = if model_uses_embedded_runtime {
        progress(InstallProgress {
            stage: InstallStage::HealthChecking,
            completed_bytes: model.size_bytes,
            total_bytes: model.size_bytes,
            bytes_per_second: None,
        });
        service
            .verify_installation_candidate(
                InstallationCandidate::pinned(
                    model_id.clone(),
                    model.path.clone(),
                    None,
                    manifest_source.expected_size_bytes,
                    manifest_source.expected_sha256.clone(),
                ),
                &cancellation,
            )
            .map_err(|error| error.to_string())?
    } else if !force_runtime_package && let Some(smoke) = smoke_current {
        smoke
    } else {
        let prepared =
            managed_downloads::prepare_primary_runtime(&target_root, &cancellation, progress)
                .map_err(InstallJobFailure::from)?;
        candidate_root = Some(prepared.staged.root.clone());
        runtime_entrypoint = Some(prepared.installed_entrypoint.clone());
        runtime_version = Some(prepared.version.clone());
        runtime_package_id = Some(prepared.package_id.clone());
        runtime_archive_sha256 = Some(prepared.archive_sha256.clone());
        progress(InstallProgress {
            stage: InstallStage::HealthChecking,
            completed_bytes: model.size_bytes,
            total_bytes: model.size_bytes,
            bytes_per_second: None,
        });
        let smoke = service
            .verify_installation_candidate(
                InstallationCandidate::normalized(
                    model_id.clone(),
                    model.path.clone(),
                    Some(
                        candidate_root
                            .clone()
                            .ok_or_else(|| "staged runtime root was lost".to_owned())?,
                    ),
                )
                .map_err(|error| error.to_string())?,
                &cancellation,
            )
            .map_err(|error| error.to_string())?;
        staged_runtime = Some(prepared.staged);
        smoke
    };

    if cancellation.is_cancelled() {
        return Err(InstallJobFailure::normal(
            "Installation cancelled after smoke testing; no artifacts were activated.",
        ));
    }
    progress(InstallProgress {
        stage: InstallStage::Activating,
        completed_bytes: model.size_bytes,
        total_bytes: model.size_bytes,
        bytes_per_second: None,
    });

    let runtime_target = staged_runtime.as_ref().map(|_| target_root.clone());
    let prior_config_fingerprint = config::settings::artifact_config_fingerprint(&config)
        .map_err(|error| format!("could not fingerprint pre-install artifact settings: {error}"))?;
    let mut journal = ActivationJournal::begin(
        activation_journal_path(),
        model.destination.clone(),
        runtime_target,
        current_runtime_known_good,
        prior_config_fingerprint,
    )
    .map_err(|error| {
        if error.requires_recovery() {
            InstallJobFailure::recovery_required(error.to_string())
        } else {
            InstallJobFailure::normal(error.to_string())
        }
    })?;

    let runtime = if let Some(staged) = staged_runtime {
        let replacement = match staged.activate() {
            Ok(replacement) => replacement,
            Err(error) => {
                if !error.requires_recovery() {
                    return Err(failure_after_safe_rollback(journal, error.to_string()));
                }
                return Err(InstallJobFailure::recovery_required(error.to_string()));
            }
        };
        if let Err(error) = journal.mark(ActivationPhase::RuntimeActivated) {
            let rollback = replacement.rollback().err();
            if rollback.is_none() && !error.requires_recovery() {
                return Err(failure_after_safe_rollback(journal, error.to_string()));
            }
            return Err(rollback.map_or_else(
                || InstallJobFailure::normal(error.to_string()),
                |rollback| {
                    InstallJobFailure::recovery_required(format!(
                        "{error}. Runtime rollback also failed: {rollback}"
                    ))
                },
            ));
        }
        Some(replacement)
    } else {
        None
    };

    let model_sha256 = model.sha256.clone();
    let model = match model.activate() {
        Ok(replacement) => replacement,
        Err(error) => {
            let rollback = runtime.and_then(|replacement| replacement.rollback().err());
            if rollback.is_none() && !error.requires_recovery() {
                return Err(failure_after_safe_rollback(journal, error.to_string()));
            }
            return Err(rollback.map_or_else(
                || {
                    if error.requires_recovery() {
                        InstallJobFailure::recovery_required(error.to_string())
                    } else {
                        InstallJobFailure::normal(error.to_string())
                    }
                },
                |rollback| {
                    InstallJobFailure::recovery_required(format!(
                        "{error}. Runtime rollback also failed: {rollback}"
                    ))
                },
            ));
        }
    };
    if let Err(error) = journal.mark(ActivationPhase::ModelActivated) {
        return Err(failure_after_activated_artifact_rollback(
            journal,
            model,
            None,
            runtime,
            error.to_string(),
        ));
    }

    let manifest_document = match installed_manifest::build_manifest(
        &model_id,
        manifest_source,
        model_uses_embedded_runtime,
        model.destination(),
        &model_sha256,
        &smoke,
    ) {
        Ok(manifest) => manifest,
        Err(error) => {
            return Err(failure_after_activated_artifact_rollback(
                journal,
                model,
                None,
                runtime,
                error.to_string(),
            ));
        }
    };
    if let Err(error) =
        journal.record_manifest_target(installed_manifest::manifest_path_for(model.destination()))
    {
        return Err(failure_after_activated_artifact_rollback(
            journal,
            model,
            None,
            runtime,
            error.to_string(),
        ));
    }
    let manifest = match installed_manifest::stage_manifest(&manifest_document) {
        Ok(replacement) => replacement,
        Err(error) if error.requires_recovery() => {
            return Err(InstallJobFailure::recovery_required(error.to_string()));
        }
        Err(error) => {
            return Err(failure_after_activated_artifact_rollback(
                journal,
                model,
                None,
                runtime,
                error.to_string(),
            ));
        }
    };

    let remote_install = remote_install_request.map(|request| {
        config::ManagedRemoteModelInstall::trusted(
            config::RemoteGgufArtifact {
                repository: request.artifact.model_id,
                revision: request.artifact.revision,
                filename: request.artifact.filename,
                expected_size_bytes: request.artifact.size_bytes,
                expected_sha256: request.artifact.expected_sha256,
            },
            model.destination().to_path_buf(),
            request.display_name,
            request.description,
            request.languages,
            request.recommended,
        )
    });

    Ok(VerifiedInstallResult {
        model,
        manifest,
        runtime,
        runtime_id,
        runtime_entrypoint,
        runtime_version,
        runtime_package_id,
        runtime_archive_sha256,
        retain_runtime_as_previous: current_runtime_known_good,
        model_sha256,
        smoke,
        journal,
        remote_install,
    })
}

fn validate_local_gguf_import(
    source_path: PathBuf,
    model_storage_dir: PathBuf,
    service: TranscriptionService,
    cancellation: InstallCancellation,
) -> Result<ValidatedLocalGgufImport, String> {
    if !source_path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("gguf"))
    {
        return Err("Choose a local .gguf file to import.".to_owned());
    }
    let fingerprint = fingerprint_file_cancellable(&source_path, &cancellation)
        .map_err(local_gguf_import_install_error)?;
    reject_import_source_in_model_storage(&fingerprint.canonical_path, &model_storage_dir)?;
    let model_id = config::imported_gguf_model_id(&fingerprint.sha256)
        .map(ModelId::new)
        .ok_or_else(|| "The local GGUF hash was invalid.".to_owned())?;
    let filename = fingerprint
        .canonical_path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.trim().is_empty())
        .ok_or_else(|| "The local GGUF file has no usable filename.".to_owned())?;
    let display_name = fingerprint
        .canonical_path
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| "Imported local GGUF".to_owned());
    let smoke = service
        .verify_installation_candidate(
            InstallationCandidate::pinned(
                model_id.clone(),
                fingerprint.canonical_path.clone(),
                None,
                fingerprint.size_bytes,
                fingerprint.sha256.clone(),
            ),
            &cancellation,
        )
        .map_err(local_gguf_import_error)?;
    let after_smoke = fingerprint_file_cancellable(&fingerprint.canonical_path, &cancellation)
        .map_err(local_gguf_import_install_error)?;
    ensure_local_gguf_fingerprint_unchanged(&fingerprint, &after_smoke)?;
    let install = config::ImportedGgufModelInstall::validated(
        fingerprint.canonical_path.clone(),
        fingerprint.size_bytes,
        fingerprint.sha256.clone(),
        display_name,
    );
    let manifest = installed_manifest::build_manifest(
        &model_id,
        installed_manifest::ArtifactSource::local_import(
            filename,
            fingerprint.size_bytes,
            fingerprint.sha256,
        ),
        true,
        &fingerprint.canonical_path,
        &after_smoke.sha256,
        &smoke,
    )
    .map_err(|error| format!("Could not prepare local import receipt: {error}"))?;
    Ok(ValidatedLocalGgufImport {
        model_id,
        install,
        manifest,
        smoke,
    })
}

fn reject_import_source_in_model_storage(
    canonical_source_path: &Path,
    model_storage_dir: &Path,
) -> Result<(), String> {
    let canonical_storage_dir = match fs::canonicalize(model_storage_dir) {
        Ok(path) => path,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(format!(
                "Could not inspect Scribe model storage {}: {error}",
                model_storage_dir.display()
            ));
        }
    };
    if canonical_source_path.starts_with(&canonical_storage_dir) {
        return Err(
            "Choose a GGUF outside Scribe's managed model storage. Local imports must remain external to Scribe."
                .to_owned(),
        );
    }
    Ok(())
}

fn ensure_local_gguf_fingerprint_unchanged(
    before: &crate::installations::FileFingerprint,
    after_smoke: &crate::installations::FileFingerprint,
) -> Result<(), String> {
    if before == after_smoke {
        Ok(())
    } else {
        Err(
            "The local GGUF changed during validation. It was not imported; choose it again to revalidate its current bytes."
                .to_owned(),
        )
    }
}

fn local_gguf_import_install_error(error: InstallError) -> String {
    match error {
        InstallError::Cancelled { .. } => {
            "Local GGUF import was cancelled. The source file was left unchanged.".to_owned()
        }
        error => format!("Local GGUF could not be loaded by the embedded runtime: {error}"),
    }
}

fn local_gguf_import_error(error: anyhow::Error) -> String {
    if matches!(
        error.downcast_ref::<InstallError>(),
        Some(InstallError::Cancelled { .. })
    ) {
        "Local GGUF import was cancelled. The source file was left unchanged.".to_owned()
    } else {
        format!("Local GGUF could not be loaded by the embedded runtime: {error}")
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RuntimeActionKind {
    Install,
    Update,
    Uninstall,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct RuntimeActionState {
    kind: RuntimeActionKind,
    enabled: bool,
    disabled_tooltip: Option<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct RuntimeInstallJob {
    download_model_ids: Vec<String>,
    repair_model_ids: Vec<String>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct RuntimeConsumerActivity {
    recording: bool,
    transcribing: bool,
    playground_jobs: bool,
    model_download: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum RuntimePersistenceTransition {
    Persisted(RuntimeInstallJob),
    Failed {
        job: RuntimeInstallJob,
        message: String,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum RuntimeJobIntent {
    DownloadModel(String),
    RepairModel(String),
    Maintenance,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct RuntimeReplacement {
    installed_path: PathBuf,
    target_root: PathBuf,
    backup_root: Option<PathBuf>,
}

fn queue_runtime_model(model_ids: &mut Vec<String>, model_id: String) -> bool {
    if model_ids.iter().any(|queued| queued == &model_id) {
        false
    } else {
        model_ids.push(model_id);
        true
    }
}

fn apply_runtime_record(
    config: &mut AppConfig,
    runtime_id: &str,
    install: config::ManagedRuntimeInstall,
) -> Option<config::ManagedRuntimeInstall> {
    config
        .general
        .managed_runtimes
        .insert(runtime_id.to_owned(), install)
}

fn rollback_runtime_record(
    config: &mut AppConfig,
    runtime_id: &str,
    previous: Option<config::ManagedRuntimeInstall>,
) {
    match previous {
        Some(install) => {
            config
                .general
                .managed_runtimes
                .insert(runtime_id.to_owned(), install);
        }
        None => {
            config.general.managed_runtimes.remove(runtime_id);
        }
    }
}

fn persist_runtime_install(
    config: &mut AppConfig,
    runtime_id: &str,
    install: config::ManagedRuntimeInstall,
    job: RuntimeInstallJob,
    persist: impl FnOnce(&AppConfig) -> Result<(), String>,
) -> RuntimePersistenceTransition {
    let previous_runtime = apply_runtime_record(config, runtime_id, install);
    config::normalize_config(config);
    match persist(config) {
        Ok(()) => RuntimePersistenceTransition::Persisted(job),
        Err(err) => {
            rollback_runtime_record(config, runtime_id, previous_runtime);
            RuntimePersistenceTransition::Failed {
                job,
                message: format!("Failed to persist the installed runtime: {err}"),
            }
        }
    }
}

fn runtime_metadata_matches(
    app_config: &AppConfig,
    runtime_id: &str,
    install: &config::ManagedRuntimeInstall,
) -> bool {
    app_config.general.managed_runtimes.get(runtime_id) == Some(install)
}

fn missing_runtime_source_message() -> String {
    "This build does not include the required local speech runtime. Install a packaged or staged build that includes it."
        .to_owned()
}

fn should_activate_installed_model(active_model_is_runnable: bool) -> bool {
    !active_model_is_runnable
}

fn runtime_needs_preparation(status: &ModelRuntimeStatus) -> bool {
    status != &ModelRuntimeStatus::Ready
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum RuntimeVersionState {
    NotTracked,
    Current(String),
    UpdateAvailable {
        installed: Option<String>,
        available: String,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum RuntimeInstallSource {
    Packaged(PathBuf),
    DevelopmentScript(DevelopmentRuntimePackage),
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct DevelopmentRuntimePackage {
    script: PathBuf,
    destination_env: &'static str,
    destination_root: PathBuf,
    executable_path: PathBuf,
}

#[derive(Debug, Default, Deserialize)]
struct RuntimeManifestMetadata {
    version: Option<String>,
    sha256: Option<String>,
    checksum: Option<String>,
}

#[cfg(test)]
fn runtime_action_state(config: &AppConfig, model: &SttModelInfo) -> RuntimeActionState {
    runtime_action_state_with_busy(config, model, false)
}

#[cfg(test)]
fn runtime_action_state_with_busy(
    config: &AppConfig,
    model: &SttModelInfo,
    busy: bool,
) -> RuntimeActionState {
    runtime_action_state_with_activity(config, model, busy, RuntimeConsumerActivity::default())
}

fn runtime_action_state_with_activity(
    config: &AppConfig,
    model: &SttModelInfo,
    busy: bool,
    activity: RuntimeConsumerActivity,
) -> RuntimeActionState {
    let state = runtime_action_state_inner(config, model);
    restrict_runtime_action(state, busy, activity)
}

fn restrict_runtime_action(
    mut state: RuntimeActionState,
    busy: bool,
    activity: RuntimeConsumerActivity,
) -> RuntimeActionState {
    if busy {
        state.enabled = false;
        state.disabled_tooltip =
            Some("The shared local runtime is already being prepared.".to_owned());
    } else if matches!(
        state.kind,
        RuntimeActionKind::Update | RuntimeActionKind::Uninstall
    ) && let Some(reason) = runtime_consumer_block_reason(activity)
    {
        state.enabled = false;
        state.disabled_tooltip = Some(reason);
    }
    state
}

fn runtime_consumer_block_reason(activity: RuntimeConsumerActivity) -> Option<String> {
    if activity.recording {
        Some("Stop the active recording before changing the shared local runtime.".to_owned())
    } else if activity.transcribing {
        Some(
            "Wait for transcription to finish before changing the shared local runtime.".to_owned(),
        )
    } else if activity.playground_jobs {
        Some(
            "Wait for Playground jobs to finish before changing the shared local runtime."
                .to_owned(),
        )
    } else if activity.model_download {
        Some("Wait for the model download to finish before changing its runtime.".to_owned())
    } else {
        None
    }
}

fn model_download_uses_runtime(
    config: &AppConfig,
    model_downloads: &HashMap<String, ModelInstallStatus>,
    runtime_id: &str,
) -> bool {
    config::configured_models(config).into_iter().any(|model| {
        matches!(
            model_downloads.get(&model.id),
            Some(ModelInstallStatus::Downloading { .. })
        ) && compatibility_bridge::provider_for_model(&model)
            .is_some_and(|provider| provider.id() == runtime_id)
    })
}

#[cfg(test)]
fn apply_runtime_uninstall_result(
    config: &mut AppConfig,
    runtime_id: &str,
    removal: Result<bool, String>,
) -> Result<bool, String> {
    let removed_files = removal?;
    config.general.managed_runtimes.remove(runtime_id);
    Ok(removed_files)
}

fn runtime_action_state_inner(config: &AppConfig, model: &SttModelInfo) -> RuntimeActionState {
    let Some(provider) = compatibility_bridge::provider_for_model(model) else {
        return RuntimeActionState {
            kind: RuntimeActionKind::Install,
            enabled: false,
            disabled_tooltip: Some("This model has no compatible local provider.".to_owned()),
        };
    };

    if !provider.runtime_install_supported() {
        return RuntimeActionState {
            kind: RuntimeActionKind::Install,
            enabled: false,
            disabled_tooltip: Some(
                "The managed local runtime installer is not bundled in this build.".to_owned(),
            ),
        };
    }

    if let Some(capability) = verified_installation_capability(&ModelId::new(&model.id)) {
        let package_version = match capability {
            VerifiedInstallationCapability::Available { package_version } => package_version,
            VerifiedInstallationCapability::Unavailable { reason } => {
                return RuntimeActionState {
                    kind: RuntimeActionKind::Install,
                    enabled: false,
                    disabled_tooltip: Some(reason),
                };
            }
        };
        let source_available = model.local_path.as_ref().is_some_and(|path| path.is_file());
        if has_managed_runtime_install(config, provider) {
            let installed = config
                .general
                .managed_runtimes
                .get(provider.id())
                .and_then(|install| install.version.as_deref());
            if installed != Some(package_version.as_str()) && source_available {
                return RuntimeActionState {
                    kind: RuntimeActionKind::Update,
                    enabled: true,
                    disabled_tooltip: None,
                };
            }
            return RuntimeActionState {
                kind: RuntimeActionKind::Uninstall,
                enabled: true,
                disabled_tooltip: None,
            };
        }
        return RuntimeActionState {
            kind: RuntimeActionKind::Install,
            enabled: source_available,
            disabled_tooltip: (!source_available).then(|| {
                "Install a pinned model before installing or updating its native runtime."
                    .to_owned()
            }),
        };
    }

    runtime_action_state_for_source(
        config,
        model,
        provider,
        runtime_install_source(config, model).is_some(),
    )
}

fn runtime_action_state_for_source(
    config: &AppConfig,
    _model: &SttModelInfo,
    provider: ProviderHandle,
    source_available: bool,
) -> RuntimeActionState {
    if has_managed_runtime_install(config, provider) {
        if runtime_needs_update(config, provider) && source_available {
            return RuntimeActionState {
                kind: RuntimeActionKind::Update,
                enabled: true,
                disabled_tooltip: None,
            };
        }

        return RuntimeActionState {
            kind: RuntimeActionKind::Uninstall,
            enabled: true,
            disabled_tooltip: None,
        };
    }

    if source_available {
        RuntimeActionState {
            kind: RuntimeActionKind::Install,
            enabled: true,
            disabled_tooltip: None,
        }
    } else {
        RuntimeActionState {
            kind: RuntimeActionKind::Install,
            enabled: false,
            disabled_tooltip: Some(missing_runtime_source_message()),
        }
    }
}

fn supports_managed_install(model: &SttModelInfo) -> bool {
    if model.download_model.is_none() {
        return false;
    }
    if let Some(capability) = verified_installation_capability(&ModelId::new(&model.id)) {
        return matches!(capability, VerifiedInstallationCapability::Available { .. });
    }
    compatibility_bridge::provider_for_model(model)
        .is_some_and(|provider| provider.can_install_model(model))
}

fn supports_managed_uninstall(model: &SttModelInfo, install_status: &ModelInstallStatus) -> bool {
    compatibility_bridge::provider_for_model(model).is_some_and(|provider| {
        let mut model = model.clone();
        model.install_status = install_status.clone();
        provider.can_uninstall_model(&model)
    })
}

fn has_managed_runtime_install(config: &AppConfig, provider: ProviderHandle) -> bool {
    resolve_managed_runtime_executable(config, provider).is_some()
}

fn runtime_needs_update(config: &AppConfig, provider: ProviderHandle) -> bool {
    matches!(
        runtime_version_state(config, provider),
        RuntimeVersionState::UpdateAvailable { .. }
    )
}

fn runtime_version_state(config: &AppConfig, provider: ProviderHandle) -> RuntimeVersionState {
    let Some(available) = provider.available_version() else {
        return RuntimeVersionState::NotTracked;
    };
    let Some(install) = config.general.managed_runtimes.get(provider.id()) else {
        return RuntimeVersionState::NotTracked;
    };
    let installed = install
        .version
        .as_deref()
        .map(str::trim)
        .filter(|version| !version.is_empty());

    match installed {
        Some(version) if version == available => RuntimeVersionState::Current(version.to_owned()),
        Some(version) => RuntimeVersionState::UpdateAvailable {
            installed: Some(version.to_owned()),
            available: available.to_owned(),
        },
        None => RuntimeVersionState::UpdateAvailable {
            installed: None,
            available: available.to_owned(),
        },
    }
}

fn resolve_managed_runtime_executable(
    config: &AppConfig,
    provider: ProviderHandle,
) -> Option<PathBuf> {
    let root = provider.managed_root(config)?;
    provider.resolve_entrypoint([root])
}

fn packaged_runtime_path(config: &AppConfig, model: &SttModelInfo) -> Option<PathBuf> {
    let provider = compatibility_bridge::provider_for_model(model)?;
    let bundled_root = env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(Path::to_path_buf));
    let managed_root = provider.managed_root(config);
    provider.resolve_entrypoint(bundled_root.into_iter().chain(managed_root))
}

fn runtime_install_source(
    config: &AppConfig,
    model: &SttModelInfo,
) -> Option<RuntimeInstallSource> {
    runtime_install_source_from_candidates(
        config,
        model,
        packaged_runtime_path(config, model),
        development_runtime_package(config, model),
    )
}

fn runtime_install_source_from_candidates(
    config: &AppConfig,
    model: &SttModelInfo,
    packaged: Option<PathBuf>,
    development: Option<DevelopmentRuntimePackage>,
) -> Option<RuntimeInstallSource> {
    packaged
        .filter(|path| runtime_source_is_staged(config, model, path))
        .map(RuntimeInstallSource::Packaged)
        .or_else(|| development.map(RuntimeInstallSource::DevelopmentScript))
}

fn runtime_source_is_staged(config: &AppConfig, model: &SttModelInfo, path: &Path) -> bool {
    if path_is_within(path, &config::runtime_storage_dir()) {
        return false;
    }
    let Some(package_root) = runtime_package_root(path) else {
        return false;
    };

    let Some(provider) = compatibility_bridge::provider_for_model(model) else {
        return false;
    };
    let Some(current) = config.general.managed_runtimes.get(provider.id()) else {
        return true;
    };

    Some(package_root) != runtime_package_root(&current.path)
}

fn path_is_within(path: &Path, root: &Path) -> bool {
    match (path.canonicalize(), root.canonicalize()) {
        (Ok(path), Ok(root)) => path.starts_with(root),
        _ => path.starts_with(root),
    }
}

fn development_runtime_package(
    _config: &AppConfig,
    model: &SttModelInfo,
) -> Option<DevelopmentRuntimePackage> {
    let provider = compatibility_bridge::provider_for_model(model)?;
    let spec = provider.development_package()?;
    let script = find_development_bundle_script(spec.script_name)?;
    let destination_root = config::runtime_storage_dir().join(provider.id());
    Some(DevelopmentRuntimePackage {
        script,
        destination_env: spec.destination_env,
        executable_path: destination_root.join(spec.executable_relative_path),
        destination_root,
    })
}

fn find_development_bundle_script(script_name: &str) -> Option<PathBuf> {
    if !cfg!(unix) {
        return None;
    }
    if !development_runtime_installs_enabled() {
        return None;
    }

    let mut roots = Vec::new();
    if let Ok(executable) = env::current_exe()
        && let Some(parent) = executable.parent()
    {
        roots.extend(parent.ancestors().map(Path::to_path_buf));
    }
    if let Ok(cwd) = env::current_dir() {
        roots.extend(cwd.ancestors().map(Path::to_path_buf));
    }

    let mut seen = Vec::<PathBuf>::new();
    for root in roots {
        if seen.iter().any(|seen_root| seen_root == &root) {
            continue;
        }
        seen.push(root.clone());
        let script = root.join("scripts").join(script_name);
        if root.join("Cargo.toml").is_file() && script.is_file() {
            return Some(script);
        }
    }
    None
}

fn development_runtime_installs_enabled() -> bool {
    let opt_in = env::var("SCRIBE_ALLOW_DEV_RUNTIME_INSTALL").ok();
    development_runtime_installs_enabled_for(cfg!(debug_assertions), opt_in.as_deref())
}

fn development_runtime_installs_enabled_for(
    debug_assertions: bool,
    opt_in_value: Option<&str>,
) -> bool {
    debug_assertions || opt_in_value.is_some_and(env_flag_value_enabled)
}

fn env_flag_value_enabled(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

pub struct LocalTranscriberApp {
    config: AppConfig,
    config_path: Option<PathBuf>,
    settings_store: Option<SettingsStore>,
    current_tab: Tab,
    settings_tab: SettingsTab,
    models_show_comparison: bool,
    model_comparison: ModelComparisonState,
    comparison_run_model_ids: Option<Vec<String>>,
    comparison_started_at: Option<Instant>,
    comparison_reference_revision: u64,
    comparison_output_revisions: HashMap<String, u64>,
    comparison_projection_cache: HashMap<String, ComparisonProjectionCacheEntry>,
    #[cfg(test)]
    comparison_wer_compute_count: usize,
    #[cfg(test)]
    comparison_output_replacement_count: usize,
    model_management: ModelManagementState,
    status: TranscriptionStatus,
    transcript: String,
    raw_transcript: String,
    status_message: String,
    hotkey_input: String,
    model_search: String,
    model_language_filter: ModelLanguageFilter,
    remote_catalog_filters: RemoteCatalogFilters,
    remote_catalog_sort: RemoteCatalogSort,
    model_import_path: String,
    remote_catalog: RemoteCatalogState,
    audio_devices: Vec<String>,
    microphone_test: MicrophoneTest,
    microphone_test_sequence: u64,
    microphone_test_error: Option<String>,
    microphone_monitor_retry_required: bool,
    microphone_level_envelope: MicrophoneLevelEnvelope,
    deferred_recording_start: Option<DeferredRecordingStart>,
    deferred_history_playback: Option<i64>,
    capturing_hotkey: bool,
    model_downloads: HashMap<String, ModelInstallStatus>,
    runtime_jobs: HashMap<String, RuntimeInstallJob>,
    artifact_installations: HashMap<String, (u64, InstallCancellation)>,
    local_gguf_import: Option<LocalGgufImportJob>,
    artifact_recovery_error: Option<String>,
    active_recording: Option<ActiveRecording>,
    pending_recording: Option<PendingRecording>,
    pending_output: Option<PendingOutput>,
    history_requests: HashMap<(SessionId, RequestId), HistoryRequestContext>,
    leased_history_retry_ids: HashSet<i64>,
    history_store: Option<HistoryStore>,
    history_records: Vec<HistoryRecord>,
    history_next: Option<HistoryCursor>,
    history_search: String,
    history_applied_search: String,
    history_loading: bool,
    history_query_sequence: u64,
    active_history_query: Option<u64>,
    history_refresh_pending: bool,
    history_error: Option<String>,
    history_delete_confirmation: Option<i64>,
    history_confirmation_focus_pending: bool,
    history_search_focus_pending: bool,
    history_mutation_sequence: u64,
    history_mutation_in_flight: Option<u64>,
    pending_history_retention_policy: Option<HistoryRetentionPolicy>,
    armed_history_repaste: Option<ArmedHistoryRepaste>,
    history_playback: Option<PlaybackService>,
    playing_history_id: Option<i64>,
    history_playback_stopping: bool,
    rolling_preview: Option<RollingPreviewHandle>,
    pending_preview_drain: Option<PendingPreviewDrain>,
    transcription_service: TranscriptionService,
    tx: Sender<AppEvent>,
    rx: Receiver<AppEvent>,
    playground_cards: Vec<PlaygroundCardState>,
    playground_selector_draft: Option<Vec<String>>,
    playground_selector_return_focus: Option<egui::Id>,
    playground_selector_header_focus: Option<egui::Id>,
    playground_selector_needs_initial_focus: bool,
    playground_reference_transcript: String,
    playground_reference_user_edited: bool,
    playground_ranking_mode: RankingMode,
    playground_pending: usize,
    session_coordinator: SessionCoordinator,
    playground_runs: HashMap<SessionId, PlaygroundRunState>,
    latest_latency: Option<LatencyTrace>,
    diagnostics: DiagnosticsStore,
    #[cfg(test)]
    test_gguf_fixture: Option<PathBuf>,
    captured_targets: HashMap<SessionId, CapturedTarget>,
    overlay_controller: OverlayController,
    overlay_hide_at: Option<Instant>,
    hotkey_service: HotkeyService,
    tray_service: Option<TrayService>,
    last_tray_state: Option<TrayUiState>,
    window_hidden_to_tray: bool,
    quit_requested: bool,
}

impl LocalTranscriberApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        configure_stitch_style(&cc.egui_ctx);

        let (mut config, config_path, mut status_message) = match config::load_config() {
            Ok((config, path)) => (config, Some(path), "Ready".to_owned()),
            Err(err) => (
                AppConfig::default(),
                None,
                format!("Config error: {err}. Using defaults."),
            ),
        };
        config::normalize_config(&mut config);
        cc.egui_ctx.set_visuals(stitch_visuals(resolve_theme_mode(
            config.general.theme_mode,
            cc.integration_info.system_theme,
        )));

        let (tx, rx) = unbounded();
        let transcription_service = TranscriptionService::new(config.clone());
        let playground_cards = cards_from_config(&config, &transcription_service);
        let history_root = config::history_storage_dir().map_err(|error| error.to_string());
        let (history_store, history_error) = match history_root.and_then(|root| {
            HistoryStore::open(root, history_retention_policy(&config))
                .map_err(|error| error.to_string())
        }) {
            Ok(store) => {
                let report = store.startup_reconciliation();
                if report != Default::default() {
                    status_message = format!(
                        "History recovered: {} interrupted, {} deletion(s), {} missing audio reference(s), {} orphan/temp audio file(s).",
                        report.interrupted_pending_failed,
                        report.deletions_completed,
                        report.missing_audio_cleared,
                        report.orphan_audio_removed + report.temporary_audio_removed,
                    );
                }
                (Some(store), None)
            }
            Err(error) => {
                let message = format!("Local history is unavailable: {error}");
                status_message = message.clone();
                (None, Some(message))
            }
        };
        let history_playback = PlaybackService::new().ok();
        let settings_store = config_path
            .clone()
            .map(|path| SettingsStore::new(path, SETTINGS_SAVE_DEBOUNCE));
        let mut app = Self {
            hotkey_input: config.recording.hotkey.clone(),
            model_search: String::new(),
            model_language_filter: ModelLanguageFilter::default(),
            remote_catalog_filters: RemoteCatalogFilters::default(),
            remote_catalog_sort: RemoteCatalogSort::default(),
            model_import_path: String::new(),
            remote_catalog: RemoteCatalogState::default(),
            audio_devices: Vec::new(),
            microphone_test: MicrophoneTest::Idle,
            microphone_test_sequence: 0,
            microphone_test_error: None,
            microphone_monitor_retry_required: false,
            microphone_level_envelope: MicrophoneLevelEnvelope::default(),
            deferred_recording_start: None,
            deferred_history_playback: None,
            capturing_hotkey: false,
            model_downloads: HashMap::new(),
            runtime_jobs: HashMap::new(),
            artifact_installations: HashMap::new(),
            local_gguf_import: None,
            artifact_recovery_error: None,
            playground_cards,
            playground_selector_draft: None,
            playground_selector_return_focus: None,
            playground_selector_header_focus: None,
            playground_selector_needs_initial_focus: false,
            playground_reference_transcript: String::new(),
            playground_reference_user_edited: false,
            playground_ranking_mode: RankingMode::Balanced,
            hotkey_service: HotkeyService::new(&config.recording.hotkey),
            config,
            config_path,
            settings_store,
            current_tab: initial_tab(),
            settings_tab: SettingsTab::General,
            models_show_comparison: false,
            model_comparison: ModelComparisonState::default(),
            comparison_run_model_ids: None,
            comparison_started_at: None,
            comparison_reference_revision: 0,
            comparison_output_revisions: HashMap::new(),
            comparison_projection_cache: HashMap::new(),
            #[cfg(test)]
            comparison_wer_compute_count: 0,
            #[cfg(test)]
            comparison_output_replacement_count: 0,
            model_management: ModelManagementState::default(),
            status: TranscriptionStatus::Idle,
            transcript: String::new(),
            raw_transcript: String::new(),
            status_message,
            active_recording: None,
            pending_recording: None,
            pending_output: None,
            history_requests: HashMap::new(),
            leased_history_retry_ids: HashSet::new(),
            history_store,
            history_records: Vec::new(),
            history_next: None,
            history_search: String::new(),
            history_applied_search: String::new(),
            history_loading: false,
            history_query_sequence: 0,
            active_history_query: None,
            history_refresh_pending: false,
            history_error,
            history_delete_confirmation: None,
            history_confirmation_focus_pending: false,
            history_search_focus_pending: false,
            history_mutation_sequence: 0,
            history_mutation_in_flight: None,
            pending_history_retention_policy: None,
            armed_history_repaste: None,
            history_playback,
            playing_history_id: None,
            history_playback_stopping: false,
            rolling_preview: None,
            pending_preview_drain: None,
            transcription_service,
            tx,
            rx,
            playground_pending: 0,
            session_coordinator: SessionCoordinator::default(),
            playground_runs: HashMap::new(),
            latest_latency: None,
            diagnostics: DiagnosticsStore::default(),
            #[cfg(test)]
            test_gguf_fixture: None,
            captured_targets: HashMap::new(),
            overlay_controller: OverlayController::new(overlay::reduced_motion_preferred()),
            overlay_hide_at: None,
            tray_service: None,
            last_tray_state: None,
            window_hidden_to_tray: false,
            quit_requested: false,
        };

        let allowed_model_targets = config::configured_models(&app.config)
            .into_iter()
            .filter(|model| {
                config::remote_gguf_artifact(&app.config, &model.id).is_some()
                    || app
                        .transcription_service
                        .model_descriptor(&ModelId::new(&model.id))
                        .is_ok()
            })
            .filter_map(|model| {
                config::managed_remote_model_path(&app.config, &model.id)
                    .or_else(|| config::downloaded_model_path(&app.config, &model))
            })
            .collect::<Vec<_>>();
        let allowed_removal_targets = config::configured_models(&app.config)
            .into_iter()
            .filter_map(|model| {
                config::managed_remote_model_path(&app.config, &model.id)
                    .and_then(|path| path.parent().map(Path::to_path_buf))
                    .or_else(|| config::downloaded_model_path(&app.config, &model))
            })
            .chain(app.config.general.imported_gguf_models.keys().map(|id| {
                installed_manifest::imported_manifest_path_for(
                    &config::model_storage_dir(&app.config),
                    &ModelId::new(id),
                )
            }))
            .collect::<Vec<_>>();
        let allowed_manifest_targets = allowed_model_targets
            .iter()
            .map(|model_path| installed_manifest::manifest_path_for(model_path))
            .chain(app.config.general.imported_gguf_models.keys().map(|id| {
                installed_manifest::imported_manifest_path_for(
                    &config::model_storage_dir(&app.config),
                    &ModelId::new(id),
                )
            }))
            .collect::<Vec<_>>();
        let allowed_runtime_bindings = app
            .transcription_service
            .model_descriptors()
            .into_iter()
            .filter_map(|descriptor| {
                app.transcription_service
                    .recovery_installation_binding(&descriptor.id)
                    .ok()
                    .map(|binding| {
                        let target =
                            config::runtime_storage_dir().join(&binding.managed_runtime_id);
                        (binding.managed_runtime_id, target)
                    })
            })
            .collect::<Vec<_>>();
        let allowed_runtime_targets = allowed_runtime_bindings
            .iter()
            .map(|(_, target)| target.clone())
            .collect::<Vec<_>>();
        let durable_artifact_fingerprint =
            config::settings::artifact_config_fingerprint(&app.config).map_err(|error| {
                crate::installations::InstallError::RecoveryRequired(format!(
                    "could not fingerprint durable settings before artifact recovery: {error}"
                ))
            });
        let removal_recovery = durable_artifact_fingerprint
            .as_ref()
            .map_err(|error| {
                crate::installations::InstallError::RecoveryRequired(error.to_string())
            })
            .and_then(|fingerprint| {
                let removal_roots = vec![
                    config::model_storage_dir(&app.config),
                    config::runtime_storage_dir(),
                ];
                discover_managed_removal_targets(&removal_roots)
                    .and_then(|discovered| {
                        let mut allowed_targets = allowed_removal_targets.clone();
                        allowed_targets.extend(allowed_runtime_targets.iter().cloned());
                        allowed_targets.extend(discovered);
                        allowed_targets.sort();
                        allowed_targets.dedup();
                        allowed_targets.iter().try_for_each(|target| {
                            reconcile_managed_removal(target, &allowed_targets, fingerprint)
                                .map(|_| ())
                        })
                    })
                    .and_then(|_| {
                        allowed_runtime_bindings
                            .iter()
                            .try_for_each(|(runtime_id, target)| {
                                crate::installations::reconcile_orphaned_previous_runtime(
                                    target,
                                    app.config.general.managed_runtimes.contains_key(runtime_id),
                                )
                                .map(|_| ())
                            })
                    })
            });
        if let Err(error) = removal_recovery {
            let message = format!("Could not reconcile an interrupted artifact removal: {error}");
            app.status = TranscriptionStatus::Error;
            app.status_message = message.clone();
            app.artifact_recovery_error = Some(message);
        }
        let activation_recovery = durable_artifact_fingerprint.and_then(|fingerprint| {
            reconcile_activation_journal(
                &activation_journal_path(),
                &allowed_model_targets,
                &allowed_manifest_targets,
                &allowed_runtime_targets,
                Some(&fingerprint),
            )
        });
        match activation_recovery {
            Ok(true) => {
                app.status_message =
                    "Recovered an interrupted model/runtime activation transaction.".to_owned();
            }
            Ok(false) => {}
            Err(error) => {
                let message =
                    format!("Could not reconcile an interrupted artifact activation: {error}");
                app.status = TranscriptionStatus::Error;
                app.status_message = message.clone();
                app.artifact_recovery_error = Some(message);
            }
        }
        if app.artifact_recovery_error.is_none() {
            app.validate_startup_runtime_or_recover();
        }

        if let Err(err) = audio::cleanup_abandoned_recordings()
            && app.artifact_recovery_error.is_none()
        {
            app.status_message = format!("Recording cleanup warning: {err}");
        }

        let initial_tray_state = TrayUiState {
            is_recording: false,
            has_transcript: false,
        };
        let tray_context = cc.egui_ctx.clone();
        match TrayService::new(
            initial_tray_state.is_recording,
            initial_tray_state.has_transcript,
            native_main_window_handle(cc),
            move || tray_context.request_repaint(),
        ) {
            Ok(tray_service) => {
                app.tray_service = Some(tray_service);
                app.last_tray_state = Some(initial_tray_state);
            }
            Err(err) => {
                if app.artifact_recovery_error.is_none() {
                    app.status_message = format!("Tray unavailable: {err}");
                }
            }
        }

        if let Some(err) = &app.hotkey_service.last_error
            && app.artifact_recovery_error.is_none()
        {
            app.status_message = format!("Hotkey unavailable: {err}");
        }

        app.rebuild_model_inventory_projection();
        app.request_history_page(false);

        app
    }

    fn selected_model(&self) -> Option<SttModelInfo> {
        config::selected_model(&self.config)
    }

    fn validate_startup_runtime_or_recover(&mut self) {
        let Some(model) = self.selected_model() else {
            return;
        };
        let model_id = ModelId::new(&model.id);
        let embedded_gguf = config::remote_gguf_artifact(&self.config, &model.id).is_some()
            || config::imported_gguf_artifact(&self.config, &model.id).is_some();
        if !embedded_gguf
            && self
                .transcription_service
                .model_descriptor(&model_id)
                .is_err()
        {
            return;
        }
        let binding = match self.transcription_service.installation_binding(&model_id) {
            Ok(binding) => binding,
            Err(error) => {
                let message = format!(
                    "The managed runtime settings record is unsafe or unavailable; repair or remove it before transcription: {error}"
                );
                self.artifact_recovery_error = Some(message.clone());
                self.status = TranscriptionStatus::Error;
                self.status_message = message;
                return;
            }
        };
        if !embedded_gguf
            && !self
                .config
                .general
                .managed_runtimes
                .contains_key(&binding.managed_runtime_id)
        {
            return;
        }
        if model.local_path.as_ref().is_none_or(|path| !path.is_file()) {
            return;
        }
        if let Err(error) = self
            .transcription_service
            .verify_model_artifact_for_installation(&model_id, model.local_path.clone())
        {
            let message = format!(
                "The selected model failed integrity verification; repair the model without replacing the runtime: {error}"
            );
            self.model_downloads
                .insert(model.id.clone(), ModelInstallStatus::Error(message.clone()));
            self.status = TranscriptionStatus::Error;
            self.status_message = message;
            return;
        }
        let current = self
            .transcription_service
            .startup_runtime_health_and_load(&model_id, model.local_path.clone());
        if current.is_ok() {
            return;
        }
        let current_error = current.unwrap_err();
        let _ = self.transcription_service.unload_runtime();
        if embedded_gguf {
            let message = format!(
                "The selected GGUF could not be loaded by the embedded runtime: {current_error}"
            );
            self.model_downloads
                .insert(model.id.clone(), ModelInstallStatus::Error(message.clone()));
            self.status = TranscriptionStatus::Error;
            self.status_message = message;
            return;
        }
        let recovery = match self
            .transcription_service
            .rollback_to_previous_runtime(&model_id)
        {
            Ok(Some(recovery)) => recovery,
            Ok(None) => {
                match self.restore_bundled_runtime_fallback(
                    &model,
                    &binding.managed_runtime_id,
                    "Managed runtime failed; restored and verified the immutable bundled runtime.",
                ) {
                    Ok(()) => return,
                    Err(fallback_error) => {
                        let message = format!(
                            "Installed speech runtime failed startup health/load checks and no previous or bundled known-good package is available: {current_error}. {fallback_error}"
                        );
                        self.artifact_recovery_error = Some(message.clone());
                        self.status = TranscriptionStatus::Error;
                        self.status_message = message;
                        return;
                    }
                }
            }
            Err(error) => {
                match self.restore_bundled_runtime_fallback(
                    &model,
                    &binding.managed_runtime_id,
                    "Managed runtime and its previous package failed verification; restored and verified the immutable bundled runtime.",
                ) {
                    Ok(()) => return,
                    Err(fallback_error) => {
                        let message = format!(
                            "Installed speech runtime failed and neither previous nor bundled recovery succeeded: {current_error}. Previous runtime: {error}. Bundled fallback: {fallback_error}"
                        );
                        self.artifact_recovery_error = Some(message.clone());
                        self.status = TranscriptionStatus::Error;
                        self.status_message = message;
                        return;
                    }
                }
            }
        };
        let mut install = config::ManagedRuntimeInstall::app_managed(
            recovery.entrypoint,
            "startup-previous-known-good-rollback",
        );
        install.version = Some(recovery.version);
        install.sha256 = Some(recovery.archive_sha256);
        self.config
            .general
            .managed_runtimes
            .insert(recovery.managed_runtime_id, install);
        if let Err(error) = config::save_config(&self.config) {
            let message = format!(
                "Previous runtime was restored, but its settings record could not be persisted: {error}"
            );
            self.artifact_recovery_error = Some(message.clone());
            self.status = TranscriptionStatus::Error;
            self.status_message = message;
            return;
        }
        self.transcription_service = self.transcription_service.with_config(self.config.clone());
        let recovered = self
            .transcription_service
            .startup_runtime_health_and_load(&model_id, model.local_path.clone());
        match recovered {
            Ok(()) => {
                self.status = TranscriptionStatus::Idle;
                self.status_message =
                    "Restored and verified the previous known-good speech runtime.".to_owned();
            }
            Err(error) => {
                match self.restore_bundled_runtime_fallback(
                    &model,
                    &binding.managed_runtime_id,
                    "The previous runtime failed verification; restored and verified the immutable bundled runtime.",
                ) {
                    Ok(()) => {}
                    Err(fallback_error) => {
                        let message = format!(
                            "The previous runtime was restored but failed native smoke verification, and bundled fallback also failed: {error}. Bundled fallback: {fallback_error}"
                        );
                        self.artifact_recovery_error = Some(message.clone());
                        self.status = TranscriptionStatus::Error;
                        self.status_message = message;
                    }
                }
            }
        }
    }

    fn restore_bundled_runtime_fallback(
        &mut self,
        model: &SttModelInfo,
        managed_runtime_id: &str,
        success_message: &str,
    ) -> Result<(), String> {
        let mut fallback_config = self.config.clone();
        if fallback_config
            .general
            .managed_runtimes
            .remove(managed_runtime_id)
            .is_none()
        {
            return Err("managed runtime settings record was already absent".to_owned());
        }
        let fallback_service = self
            .transcription_service
            .with_config(fallback_config.clone());
        fallback_service
            .startup_bundled_runtime_health_and_load(
                &ModelId::new(&model.id),
                model.local_path.clone(),
            )
            .map_err(|error| format!("bundled runtime health/load failed: {error}"))?;
        config::save_config(&fallback_config)
            .map_err(|error| format!("bundled fallback settings could not be saved: {error}"))?;
        self.config = fallback_config;
        self.transcription_service = fallback_service;
        if let Some(store) = self.settings_store.as_mut() {
            store.mark_current_persisted();
        }
        self.status = TranscriptionStatus::Idle;
        self.status_message = success_message.to_owned();
        Ok(())
    }

    fn playground_selected_models(&self) -> Vec<SttModelInfo> {
        let Some(selected_ids) = self.comparison_run_model_ids.as_ref() else {
            return config::playground_selected_installed_models(&self.config);
        };
        let configured = config::configured_models(&self.config);
        selected_ids
            .iter()
            .filter_map(|id| {
                configured
                    .iter()
                    .find(|model| &model.id == id && model.install_status.is_runnable())
                    .cloned()
            })
            .collect()
    }

    fn save_config(&mut self) {
        config::normalize_config(&mut self.config);
        #[cfg(test)]
        if self.config_path.is_none() {
            self.status_message = "Settings saved".to_owned();
            self.refresh_playground_cards_from_config();
            return;
        }
        if self.settings_store.is_none()
            && let Ok(path) = config::config_file_path()
        {
            self.config_path = Some(path.clone());
            self.settings_store = Some(SettingsStore::new(path, SETTINGS_SAVE_DEBOUNCE));
        }
        if let Some(store) = self.settings_store.as_mut() {
            store.schedule(&self.config);
            self.status_message = "Settings updated".to_owned();
        } else {
            self.status = TranscriptionStatus::Error;
            self.status_message = "Failed to resolve the settings file path".to_owned();
        }
        self.refresh_playground_cards_from_config();
    }

    fn save_history_config(&mut self) {
        if self.config.history.mode == HistoryMode::Off {
            self.armed_history_repaste = None;
        }
        self.save_config();
        let policy = history_retention_policy(&self.config);
        if self.history_mutation_in_flight.is_some() || self.history_retry_is_active() {
            self.pending_history_retention_policy = Some(policy);
        } else {
            self.start_history_retention_mutation(policy);
        }
    }

    fn start_history_retention_mutation(&mut self, policy: HistoryRetentionPolicy) {
        if self.history_retry_is_active() {
            self.pending_history_retention_policy = Some(policy);
            self.status_message =
                "History retention will apply after the active retry finishes".to_owned();
            return;
        }
        self.start_history_mutation("History privacy settings updated", move |store| {
            store
                .set_retention_policy(policy)
                .map_err(|error| error.to_string())
        });
    }

    fn history_retry_is_active(&self) -> bool {
        !self.leased_history_retry_ids.is_empty()
    }

    fn apply_deferred_history_retention_if_idle(&mut self) {
        if self.history_mutation_in_flight.is_none()
            && !self.history_retry_is_active()
            && let Some(policy) = self.pending_history_retention_policy.take()
        {
            self.start_history_retention_mutation(policy);
        }
    }

    fn start_history_mutation(
        &mut self,
        message: &'static str,
        operation: impl FnOnce(HistoryStore) -> Result<(), String> + Send + 'static,
    ) {
        // A mutation may enforce retention and remove the armed row. Clear the
        // in-memory transcript before queuing any such operation.
        self.armed_history_repaste = None;
        if self.history_mutation_in_flight.is_some() {
            self.status_message = "Wait for the current history operation to finish".to_owned();
            return;
        }
        let Some(store) = self.history_store.clone() else {
            self.history_error = Some("Local history is unavailable".to_owned());
            return;
        };
        self.history_mutation_sequence = self.history_mutation_sequence.wrapping_add(1);
        let operation_id = self.history_mutation_sequence;
        self.history_mutation_in_flight = Some(operation_id);
        let tx = self.tx.clone();
        thread::spawn(move || {
            let result = operation(store);
            let _ = tx.send(AppEvent::HistoryMutationFinished {
                operation_id,
                message: message.to_owned(),
                result,
            });
        });
    }

    fn poll_settings_save(&mut self) {
        let Some(store) = self.settings_store.as_mut() else {
            return;
        };
        if let Err(err) = store.flush_if_due() {
            self.status = TranscriptionStatus::Error;
            self.status_message = format!("Failed to save settings: {err}");
        }
    }

    fn flush_settings(&mut self) {
        let Some(store) = self.settings_store.as_mut() else {
            return;
        };
        if !store.has_pending() {
            return;
        }
        if let Err(err) = store.flush() {
            self.status = TranscriptionStatus::Error;
            self.status_message = format!("Failed to save settings: {err}");
        }
    }

    fn stop_and_discard_active_recording(&mut self) {
        if let Some(pending) = self.pending_output.take()
            && let Some(latency) = pending.latency.as_ref()
        {
            self.record_session_diagnostic(
                pending.session_id,
                latency,
                DiagnosticSessionOutcome::Cancelled,
                None,
            );
        }
        if let Some(pending) = self.pending_recording.take() {
            pending.abandon.store(true, Ordering::Release);
            self.record_session_diagnostic(
                pending.session_id,
                &pending.latency,
                DiagnosticSessionOutcome::Cancelled,
                None,
            );
            let _ = self.session_coordinator.cancel_active();
            self.retire_captured_target(pending.session_id);
            let _ = self.overlay_controller.hide(pending.session_id);
        }
        let Some(active) = self.active_recording.take() else {
            return;
        };
        self.record_session_diagnostic(
            active.session_id,
            &active.latency,
            DiagnosticSessionOutcome::Cancelled,
            None,
        );
        self.retire_captured_target(active.session_id);
        let _ = self.overlay_controller.hide(active.session_id);
        if let Err(err) = active.session.stop_and_discard(Duration::from_secs(2)) {
            eprintln!("failed to stop and discard active recording: {err:#}");
        }
    }

    fn refresh_playground_cards_from_config(&mut self) {
        let existing_cards = std::mem::take(&mut self.playground_cards);
        let mut existing_by_id = existing_cards
            .into_iter()
            .map(|card| (card.descriptor.id.as_str().to_owned(), card))
            .collect::<HashMap<_, _>>();

        self.playground_cards = cards_from_config(&self.config, &self.transcription_service)
            .into_iter()
            .map(|mut card| {
                if let Some(mut existing) = existing_by_id.remove(card.descriptor.id.as_str()) {
                    existing.descriptor = card.descriptor;
                    existing.install_status = card.install_status;
                    existing.status =
                        runtime_status_for_id(&self.config, existing.descriptor.id.as_str());
                    existing
                } else {
                    card.status = runtime_status_for_id(&self.config, card.descriptor.id.as_str());
                    card
                }
            })
            .collect();
        let removed_outputs = existing_by_id
            .into_iter()
            .filter_map(|(model_id, card)| (!card.transcript.is_empty()).then_some(model_id))
            .collect::<Vec<_>>();
        for model_id in removed_outputs {
            self.mark_comparison_output_changed(&model_id);
        }
    }

    fn next_repaint_delay(&self) -> Duration {
        if self.capture_is_active() {
            METER_REPAINT_DELAY
        } else if self.microphone_test_is_active() || self.microphone_level_envelope.is_animating()
        {
            INPUT_LEVEL_REPAINT_DELAY
        } else if self.has_active_work() {
            ACTIVE_REPAINT_DELAY
        } else {
            // Hotkey events are integrated from update(), so idle still polls slowly. Tray
            // handlers wake the event loop directly and do not depend on this polling clock.
            IDLE_REPAINT_DELAY
        }
    }

    fn has_active_work(&self) -> bool {
        self.capture_is_active()
            || self.microphone_test_is_active()
            || self.deferred_recording_start.is_some()
            || self.deferred_history_playback.is_some()
            || self.session_coordinator.phase() != DictationPhase::Idle
            || self.playground_pending > 0
            || self.history_loading
            || self.history_mutation_in_flight.is_some()
            || self.history_retry_is_active()
            || self.playing_history_id.is_some()
            || matches!(
                self.status,
                TranscriptionStatus::Listening | TranscriptionStatus::Transcribing
            )
            || self.model_downloads.values().any(|status| {
                matches!(
                    status,
                    ModelInstallStatus::Downloading { .. } | ModelInstallStatus::InstallingRuntime
                )
            })
            || !self.runtime_jobs.is_empty()
    }

    fn capture_is_active(&self) -> bool {
        self.pending_recording.is_some()
            || self.active_recording.is_some()
            || self.pending_preview_drain.is_some()
    }

    fn recording_source(&self) -> Option<RecordingSource> {
        self.deferred_recording_start
            .as_ref()
            .map(|pending| pending.source)
            .or_else(|| self.active_recording.as_ref().map(|active| active.source))
            .or_else(|| {
                self.pending_recording
                    .as_ref()
                    .map(|pending| pending.source)
            })
            .or_else(|| {
                self.pending_preview_drain
                    .as_ref()
                    .and_then(|pending| match &pending.action {
                        PreviewDrainAction::FinishCapture(capture) => Some(capture.source),
                        _ => None,
                    })
            })
    }

    fn current_audio_levels(&self) -> LevelSnapshot {
        self.active_recording
            .as_ref()
            .map_or_else(LevelSnapshot::default, |active| {
                active.session.latest_levels()
            })
    }

    fn apply_input_sensitivity_threshold(&mut self) {
        let threshold = self.config.recording.manual_activation_rms;
        if let Some(active) = self.active_recording.as_mut() {
            active.session.set_manual_activation_threshold(threshold);
            active.capture_diagnostics.activation_floor = threshold;
        }
        if let Some(pending) = self.pending_recording.as_mut() {
            pending.capture_diagnostics.activation_floor = threshold;
        }
        if let Some(session) = self.microphone_test.session() {
            session.set_manual_activation_threshold(threshold);
        }
    }

    fn microphone_test_is_active(&self) -> bool {
        !matches!(self.microphone_test, MicrophoneTest::Idle)
    }

    fn current_sensitivity_level_sample(&self) -> (LevelSnapshot, Option<u64>, bool) {
        if let Some(active) = self.active_recording.as_ref() {
            return (
                active.session.latest_levels(),
                Some(active.session.latest_level_revision()),
                true,
            );
        }
        if let MicrophoneTest::Active { session } = &self.microphone_test {
            return (
                session.latest_levels(),
                Some(session.latest_level_revision()),
                true,
            );
        }
        (LevelSnapshot::default(), None, false)
    }

    fn ensure_microphone_monitor(&mut self) {
        if self.quit_requested
            || self.capture_is_active()
            || self.deferred_recording_start.is_some()
            || self.deferred_history_playback.is_some()
            || self.playing_history_id.is_some()
            || self.microphone_test_is_active()
            || self.microphone_monitor_retry_required
        {
            return;
        }
        self.start_microphone_test();
    }

    fn start_microphone_test(&mut self) {
        if self.quit_requested
            || self.microphone_test_is_active()
            || self.capture_is_active()
            || self.playing_history_id.is_some()
        {
            return;
        }

        let mut options = capture_options_from_config(&self.config);
        options.intent = CaptureIntent::MeterOnly;
        options.vad_enabled = true;
        options.endpointing_enabled = false;
        let input_device_name = self.config.recording.audio_input_device_name.clone();
        let max_duration_seconds = config::MAX_RECORDING_SECONDS;
        self.microphone_test_sequence = self.microphone_test_sequence.wrapping_add(1);
        let request_id = self.microphone_test_sequence;
        let cancellation = CaptureCancellation::new();
        self.microphone_test = MicrophoneTest::Starting {
            request_id,
            stop_requested: false,
            cancellation: cancellation.clone(),
        };
        self.microphone_test_error = None;
        self.microphone_monitor_retry_required = false;
        let tx = self.tx.clone();
        thread::spawn(move || {
            let result = audio::start_recording(
                max_duration_seconds,
                input_device_name,
                options,
                None,
                cancellation,
            );
            let _ = tx.send(AppEvent::MicrophoneTestReady { request_id, result });
        });
    }

    fn stop_microphone_test(&mut self) {
        self.microphone_test = match std::mem::take(&mut self.microphone_test) {
            MicrophoneTest::Starting {
                request_id,
                cancellation,
                ..
            } => {
                cancellation.cancel();
                MicrophoneTest::Starting {
                    request_id,
                    stop_requested: true,
                    cancellation,
                }
            }
            MicrophoneTest::Active { session } => {
                session.stop();
                MicrophoneTest::Stopping { session }
            }
            state @ (MicrophoneTest::Idle | MicrophoneTest::Stopping { .. }) => state,
        };
    }

    fn suspend_microphone_monitor(&mut self) {
        self.stop_microphone_test();
        self.microphone_level_envelope.clear();
    }

    fn poll_microphone_test(&mut self) {
        let completion = self
            .microphone_test
            .session()
            .and_then(RecordingSession::try_finish);
        if let Some(result) = completion {
            self.microphone_test = MicrophoneTest::Idle;
            self.microphone_level_envelope.reset_source();
            self.microphone_test_error = result.err().map(|error| error.to_string());
            if let Some(error) = self.microphone_test_error.as_ref() {
                self.microphone_monitor_retry_required = true;
                self.status_message = format!("Microphone monitoring unavailable: {error}");
            }
        }
        if self.quit_requested {
            self.deferred_recording_start = None;
            self.deferred_history_playback = None;
            return;
        }
        if matches!(self.microphone_test, MicrophoneTest::Idle)
            && let Some(pending) = self.deferred_recording_start.take()
        {
            self.deferred_history_playback = None;
            self.start_recording_at(
                pending.source,
                pending.activation_at,
                pending.trigger_observation,
            );
        } else if matches!(self.microphone_test, MicrophoneTest::Idle)
            && let Some(history_id) = self.deferred_history_playback.take()
        {
            self.apply_history_action(HistoryPageAction::Play(history_id));
        }
    }

    fn sync_overlay_state(&mut self) {
        let Some(session_id) = self.session_coordinator.active_session_id() else {
            if self
                .overlay_hide_at
                .is_some_and(|deadline| Instant::now() >= deadline)
                && let Some(session_id) = self.overlay_controller.state().session_id
            {
                let _ = self.overlay_controller.hide(session_id);
                self.retire_captured_target(session_id);
                self.overlay_hide_at = None;
            }
            return;
        };

        let mode = if self.session_coordinator.active_purpose() == Some(SessionPurpose::Dictation) {
            effective_native_overlay_mode(self.config.overlay.mode)
        } else {
            NativeOverlayMode::Off
        };
        if self.overlay_controller.state().session_id != Some(session_id) {
            self.overlay_controller.begin_session(session_id, mode);
        } else {
            self.overlay_controller.set_mode(mode);
        }

        if self.status == TranscriptionStatus::Error {
            let recovery = if self
                .pending_preview_drain
                .as_ref()
                .is_some_and(|pending| pending.timeout_reported)
            {
                OverlayRecovery::WaitForPreview
            } else {
                OverlayRecovery::None
            };
            let _ = self.overlay_controller.show_error(
                session_id,
                self.status_message.clone(),
                recovery,
            );
            return;
        }

        let phase = match self.session_coordinator.phase() {
            DictationPhase::Idle => OverlayPhase::Hidden,
            DictationPhase::StartingCapture => OverlayPhase::Preparing,
            DictationPhase::Capturing => OverlayPhase::Listening,
            DictationPhase::FinalizingCapture => OverlayPhase::Finalizing,
            DictationPhase::Transcribing => OverlayPhase::Processing,
            DictationPhase::Output => OverlayPhase::Pasting,
        };
        let _ = self.overlay_controller.set_phase(session_id, phase);
        let levels = self.current_audio_levels();
        if let Some(active) = self.active_recording.as_mut()
            && active.session_id == session_id
            && active.latency.first_meter_update_at.is_none()
            && active.session.has_level_update()
        {
            active.latency.first_meter_update_at = Some(Instant::now());
        }
        let _ = self
            .overlay_controller
            .update_audio_level(session_id, levels.rms, levels.peak);
        let elapsed = self
            .active_recording
            .as_ref()
            .filter(|active| active.session_id == session_id)
            .map(|active| active.started_at.elapsed())
            .or_else(|| {
                self.pending_recording
                    .as_ref()
                    .filter(|pending| pending.session_id == session_id)
                    .map(|pending| pending.latency.activation_at.elapsed())
            });
        if let Some(elapsed) = elapsed {
            let _ = self.overlay_controller.update_elapsed(session_id, elapsed);
        }
    }

    fn finish_overlay_success(&mut self, session_id: SessionId) {
        if self
            .overlay_controller
            .set_phase(session_id, OverlayPhase::Success)
        {
            self.overlay_hide_at = Some(Instant::now() + Duration::from_millis(900));
        }
    }

    fn finish_overlay_error(&mut self, session_id: SessionId, message: &str) {
        if self.overlay_controller.show_error(
            session_id,
            message.to_owned(),
            OverlayRecovery::Retry,
        ) {
            self.overlay_hide_at = Some(Instant::now() + Duration::from_secs(3));
        }
    }

    fn retire_captured_target(&mut self, session_id: SessionId) {
        if let Some(target) = self.captured_targets.remove(&session_id) {
            crate::overlay::platform::release_captured_target(&target);
        }
    }

    fn begin_overlay_session(
        &mut self,
        session_id: SessionId,
        mode: NativeOverlayMode,
        target: Option<CapturedTarget>,
    ) {
        if let Some(previous_session_id) = self.overlay_controller.state().session_id
            && previous_session_id != session_id
        {
            self.retire_captured_target(previous_session_id);
        }
        if let Some(target) = target {
            self.captured_targets.insert(session_id, target);
        }
        self.overlay_controller.begin_session(session_id, mode);
        self.overlay_hide_at = None;
    }

    fn fail_dictation_session(&mut self, session_id: SessionId, message: impl Into<String>) {
        if self.session_coordinator.active_session_id() != Some(session_id) {
            return;
        }
        let message = message.into();
        if self.begin_preview_drain(
            session_id,
            PreviewDrainAction::Fail {
                session_id,
                message: message.clone(),
            },
        ) {
            self.status_message = "Stopping live preview after an error".to_owned();
            return;
        }
        self.fail_dictation_session_now(session_id, message);
    }

    fn fail_dictation_session_now(&mut self, session_id: SessionId, message: impl Into<String>) {
        if self.session_coordinator.active_session_id() != Some(session_id) {
            return;
        }
        let message = message.into();
        let _ = self.session_coordinator.fail(session_id);
        if let Some(pending) = self.pending_output.take()
            && let Some(latency) = pending.latency.as_ref()
        {
            self.record_session_diagnostic(
                pending.session_id,
                latency,
                DiagnosticSessionOutcome::Cancelled,
                None,
            );
        }
        self.status = TranscriptionStatus::Error;
        self.status_message = message.clone();
        self.finish_overlay_error(session_id, &message);
    }

    fn record_session_diagnostic(
        &mut self,
        session_id: SessionId,
        latency: &LatencyTrace,
        outcome: DiagnosticSessionOutcome,
        failure_stage: Option<DiagnosticFailureStage>,
    ) {
        self.diagnostics
            .record(latency.diagnostic_snapshot(session_id, outcome, failure_stage));
    }

    fn poll_pending_output(&mut self) {
        self.poll_pending_output_with(text_output::write_to_captured_target);
    }

    fn poll_pending_output_with<O>(
        &mut self,
        write_output: impl FnOnce(&str, &AppConfig, Option<&CapturedTarget>) -> O,
    ) where
        O: Into<text_output::TextOutputOutcome>,
    {
        let Some(mut pending) = self.pending_output.take() else {
            return;
        };
        if self.session_coordinator.active_session_id() != Some(pending.session_id)
            || self.session_coordinator.phase() != DictationPhase::Output
        {
            return;
        }

        if let Some(latency) = pending.latency.as_mut() {
            latency.output_started_at = Some(Instant::now());
        }
        let outcome = write_output(
            &pending.transcript,
            &pending.config,
            self.captured_targets.get(&pending.session_id),
        )
        .into();
        let result = outcome.result;
        if let Some(latency) = pending.latency.as_mut() {
            let completed_at = Instant::now();
            latency.target_activated_at = outcome.timing.target_activated_at;
            latency.paste_completed_at = outcome.timing.paste_completed_at;
            latency.output_completed_at = Some(completed_at);
        }

        let output_message = result.status_message();
        if let Some(history_id) = pending.history_id {
            self.record_history_output_outcome(history_id, &result);
        }
        if let Some(latency) = pending.latency.as_ref() {
            let (outcome, failure_stage) =
                if matches!(result, text_output::TextOutputResult::Failed(_)) {
                    (
                        DiagnosticSessionOutcome::Failed,
                        Some(DiagnosticFailureStage::Output),
                    )
                } else {
                    (DiagnosticSessionOutcome::Completed, None)
                };
            self.record_session_diagnostic(pending.session_id, latency, outcome, failure_stage);
        }
        self.status_message = format!("{}. {}", pending.completion_message, output_message);
        self.latest_latency = pending.latency;
        let _ = self.session_coordinator.complete(pending.session_id);
        if let text_output::TextOutputResult::Failed(message) = &result {
            self.status = TranscriptionStatus::Error;
            self.finish_overlay_error(pending.session_id, message);
        } else if !result.did_insert() {
            self.status = TranscriptionStatus::Idle;
            self.finish_overlay_error(pending.session_id, &output_message);
        } else if matches!(
            result,
            text_output::TextOutputResult::InsertedClipboardRestoreFailed(_)
        ) {
            self.status = TranscriptionStatus::Idle;
            if self.overlay_controller.show_error(
                pending.session_id,
                output_message,
                OverlayRecovery::None,
            ) {
                self.overlay_hide_at = Some(Instant::now() + Duration::from_secs(3));
            }
        } else {
            self.status = TranscriptionStatus::Idle;
            self.finish_overlay_success(pending.session_id);
        }
    }

    fn finish_transcription_after_history(&mut self, pending: PendingHistoryCompletion) {
        match pending.kind {
            HistoryRequestKind::Dictation => {
                if pending.output_config.output.auto_insert_transcript {
                    if let Err(error) = self.session_coordinator.begin_output(pending.session_id) {
                        self.status = TranscriptionStatus::Error;
                        self.status_message = format!("Could not begin final output: {error}");
                        let _ = self.session_coordinator.fail(pending.session_id);
                        return;
                    }
                    self.status = TranscriptionStatus::Transcribing;
                    self.status_message = format!(
                        "{}. Verifying the original target before paste.",
                        pending.completion_message
                    );
                    self.pending_output = Some(PendingOutput {
                        session_id: pending.session_id,
                        history_id: Some(pending.history_id),
                        transcript: pending.transcript,
                        completion_message: pending.completion_message,
                        config: pending.output_config,
                        latency: pending.latency,
                    });
                } else {
                    self.status = TranscriptionStatus::Idle;
                    self.status_message = pending.completion_message;
                    if let Some(latency) = pending.latency.as_ref() {
                        self.record_session_diagnostic(
                            pending.session_id,
                            latency,
                            DiagnosticSessionOutcome::Completed,
                            None,
                        );
                    }
                    self.latest_latency = pending.latency;
                    let _ = self.session_coordinator.complete(pending.session_id);
                    self.finish_overlay_success(pending.session_id);
                    self.record_history_output_label(pending.history_id, "not_requested");
                }
            }
            HistoryRequestKind::Retry => {
                self.status = TranscriptionStatus::Idle;
                self.status_message = format!(
                    "{}. History retry completed; nothing was pasted.",
                    pending.completion_message
                );
                if let Some(latency) = pending.latency.as_ref() {
                    self.record_session_diagnostic(
                        pending.session_id,
                        latency,
                        DiagnosticSessionOutcome::Completed,
                        None,
                    );
                }
                self.latest_latency = pending.latency;
                let _ = self.session_coordinator.complete(pending.session_id);
            }
        }
        self.request_history_page(false);
    }

    fn fail_history_entry(&self, history_id: i64, failure: impl Into<String>) {
        let Some(store) = self.history_store.clone() else {
            return;
        };
        let failure = failure.into();
        let tx = self.tx.clone();
        thread::spawn(move || {
            let result = store
                .fail(history_id, failure)
                .map(|_| ())
                .map_err(|error| error.to_string());
            let _ = tx.send(AppEvent::HistoryOutputRecorded { result });
        });
    }

    fn fail_history_retry(&self, history_id: i64, failure: impl Into<String>) {
        let Some(store) = self.history_store.clone() else {
            return;
        };
        let failure = failure.into();
        let tx = self.tx.clone();
        thread::spawn(move || {
            let (retry_lease_released, result) =
                settle_failed_history_retry(&store, history_id, failure);
            let _ = tx.send(AppEvent::HistoryRetryTerminalPersisted {
                history_id,
                retry_lease_released,
                result,
            });
        });
    }

    fn discard_history_entry(&self, history_id: i64) {
        let Some(store) = self.history_store.clone() else {
            return;
        };
        let tx = self.tx.clone();
        thread::spawn(move || {
            let result = store.delete(history_id).map_err(|error| error.to_string());
            let _ = tx.send(AppEvent::HistoryOutputRecorded { result });
        });
    }

    fn finish_no_speech_history_context(&self, context: HistoryRequestContext, message: &str) {
        match context.kind {
            HistoryRequestKind::Dictation => self.discard_history_entry(context.id),
            HistoryRequestKind::Retry => self.fail_history_retry(context.id, message),
        }
    }

    fn fail_history_context(&self, context: HistoryRequestContext, failure: impl Into<String>) {
        let failure = failure.into();
        match context.kind {
            HistoryRequestKind::Dictation => self.fail_history_entry(context.id, failure),
            HistoryRequestKind::Retry => self.fail_history_retry(context.id, failure),
        }
    }

    fn fail_correlated_history_request(
        &mut self,
        session_id: SessionId,
        request_id: RequestId,
        failure: impl Into<String>,
    ) {
        if let Some(context) = self.history_requests.remove(&(session_id, request_id)) {
            self.fail_history_context(context, failure);
        }
    }

    fn effective_status(&self) -> TranscriptionStatus {
        if self.status == TranscriptionStatus::Error {
            return TranscriptionStatus::Error;
        }
        match self.session_coordinator.phase() {
            DictationPhase::StartingCapture | DictationPhase::Capturing => {
                TranscriptionStatus::Listening
            }
            DictationPhase::FinalizingCapture
            | DictationPhase::Transcribing
            | DictationPhase::Output => TranscriptionStatus::Transcribing,
            DictationPhase::Idle => self.status,
        }
    }

    fn apply_playground_action(&mut self, action: PlaygroundAction) {
        match action {
            PlaygroundAction::Clear(model_id) => {
                let clearing_active_model = model_id == self.config.general.selected_default_model;
                let mut output_changed = false;
                if let Some(card) = self
                    .playground_cards
                    .iter_mut()
                    .find(|card| card.descriptor.id.as_str() == model_id)
                {
                    output_changed = !card.transcript.is_empty();
                    card.transcript.clear();
                    card.latency_ms = None;
                    card.audio_duration_ms = None;
                    card.peak_ram_mb = None;
                    card.peak_vram_mb = None;
                }
                if output_changed {
                    self.mark_comparison_output_changed(&model_id);
                }
                if clearing_active_model && !self.playground_reference_user_edited {
                    self.playground_reference_transcript.clear();
                }
            }
            PlaygroundAction::MoveBy { model_id, offset } => {
                let selected_ids = self
                    .playground_cards
                    .iter()
                    .map(|card| card.descriptor.id.as_str().to_owned())
                    .collect::<Vec<_>>();
                if let Some(position) = move_selected_model_by(
                    &mut self.config.general.playground_model_order,
                    &selected_ids,
                    &model_id,
                    offset,
                ) {
                    let model_name = self
                        .playground_cards
                        .iter()
                        .find(|card| card.descriptor.id.as_str() == model_id)
                        .map(|card| card.descriptor.display_name.to_owned())
                        .unwrap_or(model_id);
                    self.save_config();
                    self.status_message = format!(
                        "Moved {model_name} to position {} of {}.",
                        position + 1,
                        selected_ids.len()
                    );
                }
            }
            PlaygroundAction::MoveBefore {
                dragged_id,
                target_id,
            } => {
                if dragged_id != target_id {
                    move_model_before(
                        &mut self.config.general.playground_model_order,
                        &dragged_id,
                        &target_id,
                    );
                    self.save_config();
                    if let Some(position) = self
                        .playground_cards
                        .iter()
                        .position(|card| card.descriptor.id.as_str() == dragged_id)
                    {
                        self.status_message = format!(
                            "Moved model to position {} of {}.",
                            position + 1,
                            self.playground_cards.len()
                        );
                    }
                }
            }
        }
    }

    fn start_recording(&mut self, source: RecordingSource) {
        self.start_recording_at(source, Instant::now(), TriggerObservation::AppAction);
    }

    fn start_recording_at(
        &mut self,
        source: RecordingSource,
        activation_at: Instant,
        trigger_observation: TriggerObservation,
    ) {
        // A recording request has priority over retained-audio playback that was waiting
        // for the same monitor teardown. Never allow the two deferred audio owners to coexist.
        self.deferred_history_playback = None;
        if self.microphone_test_is_active() {
            if self.deferred_recording_start.is_none() {
                self.deferred_recording_start = Some(DeferredRecordingStart {
                    source,
                    activation_at,
                    trigger_observation,
                });
                self.stop_microphone_test();
                self.status_message = "Preparing microphone".to_owned();
            }
            return;
        }
        if self.playing_history_id.is_some() {
            self.status_message =
                "Stop retained-audio playback before starting dictation".to_owned();
            return;
        }
        if let Some(message) = self.artifact_recovery_error.as_ref() {
            self.status = TranscriptionStatus::Error;
            self.status_message = message.clone();
            return;
        }
        if !self.artifact_installations.is_empty() || !self.runtime_jobs.is_empty() {
            self.status = TranscriptionStatus::Error;
            self.status_message =
                "Wait for the active model/runtime installation to finish or cancel it before transcribing."
                    .to_owned();
            return;
        }
        if self.capture_is_active() {
            return;
        }
        if source == RecordingSource::Playground
            && let Some(message) = self.playground_run_block_reason()
        {
            self.status = TranscriptionStatus::Error;
            self.status_message = message;
            return;
        }
        let mut latency = LatencyTrace::started_at(activation_at, trigger_observation);
        latency.capture_diagnostics = Some(CaptureDiagnosticContext::from_config(&self.config));

        let preload_model = if source == RecordingSource::Transcribe {
            let Some(model) = self.selected_model() else {
                self.status = TranscriptionStatus::Error;
                self.status_message =
                    "Choose or install a local model before transcribing.".to_owned();
                return;
            };
            let runtime_status = runtime_status_for_model(&self.config, &model);
            if runtime_status != ModelRuntimeStatus::Ready {
                self.status = TranscriptionStatus::Error;
                self.status_message = setup_message_for_status(&runtime_status);
                return;
            }
            Some(model)
        } else {
            None
        };

        if source == RecordingSource::Playground {
            self.reset_playground_for_run();
        }

        self.armed_history_repaste = None;
        self.supersede_active_session();
        // Capture the external destination before any overlay/window state can
        // change. The platform adapter rejects every window owned by Scribe.
        let captured_target = (source == RecordingSource::Transcribe)
            .then(overlay::capture_foreground_target)
            .flatten();

        let session_id = match self.session_coordinator.begin(source.purpose()) {
            Ok(session_id) => session_id,
            Err(err) => {
                if let Some(target) = captured_target.as_ref() {
                    crate::overlay::platform::release_captured_target(target);
                }
                self.status = TranscriptionStatus::Error;
                self.status_message = format!("Could not start dictation: {err}");
                return;
            }
        };
        latency.observe_session_context(
            preload_model.as_ref().map(|model| model.id.clone()),
            self.config.streaming.mode,
        );
        let overlay_mode = if source == RecordingSource::Transcribe {
            effective_native_overlay_mode(self.config.overlay.mode)
        } else {
            NativeOverlayMode::Off
        };
        self.begin_overlay_session(session_id, overlay_mode, captured_target);
        if overlay_mode != NativeOverlayMode::Off {
            latency.overlay_visible_at = Some(Instant::now());
        }

        let max_duration_seconds = self.config.recording.max_recording_seconds;
        let input_device_name = self.config.recording.audio_input_device_name.clone();
        let capture_options = capture_options_from_config(&self.config);
        let mut preview_publisher = None;
        let mut preview_status = None;
        if rolling_preview_enabled(source, self.config.streaming.mode)
            && let Some(model) = preload_model.as_ref()
        {
            let model_id = ModelId::new(model.id.clone());
            match self
                .session_coordinator
                .start_preview(session_id, model_id.clone())
            {
                Ok(request_id) => match self.current_transcription_service().start_rolling_preview(
                    session_id,
                    request_id,
                    model_id.clone(),
                    model.local_path.clone(),
                ) {
                    Ok((publisher, handle)) => {
                        preview_publisher = Some(publisher);
                        self.rolling_preview = Some(handle);
                    }
                    Err(error) => {
                        let _ = self
                            .session_coordinator
                            .finish_preview(session_id, request_id, &model_id);
                        preview_status = Some(format!(
                            "Preparing microphone. Live preview is unavailable; the final pass remains enabled: {error}"
                        ));
                    }
                },
                Err(error) => {
                    preview_status = Some(format!(
                        "Preparing microphone. Live preview could not start; the final pass remains enabled: {error}"
                    ));
                }
            }
        }
        let abandon = Arc::new(AtomicBool::new(false));
        let capture_diagnostics = latency
            .capture_diagnostics
            .as_ref()
            .cloned()
            .unwrap_or_default();
        self.pending_recording = Some(PendingRecording {
            session_id,
            source,
            stop_requested: false,
            max_duration_seconds,
            latency,
            capture_diagnostics,
            abandon: abandon.clone(),
        });
        self.status = TranscriptionStatus::Listening;
        self.status_message =
            preview_status.unwrap_or_else(|| "Preparing microphone and local model".to_owned());

        if let Some(model) = preload_model {
            self.start_model_preload(session_id, model);
        }

        let tx = self.tx.clone();
        thread::spawn(move || {
            let result = audio::start_recording(
                max_duration_seconds,
                input_device_name,
                capture_options,
                preview_publisher,
                CaptureCancellation::new(),
            );
            if abandon.load(Ordering::Acquire) {
                if let Ok(session) = result {
                    let _ = session.stop_and_discard(Duration::from_secs(2));
                }
                return;
            }
            let _ = tx.send(AppEvent::CaptureReady { session_id, result });
        });
    }

    fn start_model_preload(&mut self, session_id: SessionId, model: SttModelInfo) {
        let model_id = ModelId::new(model.id.clone());
        if self
            .session_coordinator
            .model_load_started(session_id, model_id.clone())
            .is_err()
        {
            return;
        }
        if let Some(active) = self.active_recording.as_mut()
            && active.session_id == session_id
        {
            active.latency.model_load_started_at = Some(Instant::now());
        }
        if let Some(pending) = self.pending_recording.as_mut()
            && pending.session_id == session_id
        {
            pending.latency.model_load_started_at = Some(Instant::now());
        }
        let service = self.current_transcription_service();
        let tx = self.tx.clone();
        thread::spawn(
            move || match service.preload_model(&model_id, model.local_path) {
                Ok(outcome) => {
                    let _ = tx.send(AppEvent::ModelPreloadFinished {
                        session_id,
                        model_id,
                        load_duration_ms: outcome.model_load_duration_ms,
                    });
                }
                Err(err) => {
                    let _ = tx.send(AppEvent::ModelPreloadFailed {
                        session_id,
                        model_id,
                        message: err.to_string(),
                    });
                }
            },
        );
    }

    fn current_transcription_service(&self) -> TranscriptionService {
        self.transcription_service.with_config(self.config.clone())
    }

    fn supersede_active_session(&mut self) {
        self.armed_history_repaste = None;
        let Some(previous_session) = self.session_coordinator.active_session_id() else {
            return;
        };
        let superseded_history = self
            .history_requests
            .iter()
            .filter_map(|(&(session_id, request_id), context)| {
                (session_id == previous_session).then_some((request_id, *context))
            })
            .collect::<Vec<_>>();
        for (request_id, context) in superseded_history {
            self.history_requests
                .remove(&(previous_session, request_id));
            self.fail_history_context(context, "Dictation was superseded");
        }
        if let Some(pending) = self.pending_recording.take() {
            pending.abandon.store(true, Ordering::Release);
            self.record_session_diagnostic(
                pending.session_id,
                &pending.latency,
                DiagnosticSessionOutcome::Cancelled,
                None,
            );
        }
        if let Some(pending) = self.pending_output.take()
            && let Some(latency) = pending.latency.as_ref()
        {
            self.record_session_diagnostic(
                pending.session_id,
                latency,
                DiagnosticSessionOutcome::Cancelled,
                None,
            );
        }
        if self.begin_preview_drain(
            previous_session,
            PreviewDrainAction::Cancel {
                session_id: previous_session,
            },
        ) {
            self.transcription_service.cancel_active();
            if let Some(pending) = self.pending_preview_drain.as_mut() {
                pending.cancel_requested_at = Some(Instant::now());
            }
            self.status_message = "Cancelling the previous live preview".to_owned();
            return;
        }
        self.transcription_service.cancel_active();
        let _ = self.session_coordinator.cancel_active();
        self.retire_captured_target(previous_session);
        let _ = self.overlay_controller.hide(previous_session);
        self.playground_runs.remove(&previous_session);
        self.playground_pending = 0;
        self.refresh_playground_runtime_statuses();
    }

    /// Closes preview input and transfers the join handle into app-owned drain
    /// state. The UI thread only polls this state; it never waits for decoder
    /// completion. While the handle is retained, new capture is fail-closed.
    fn begin_preview_drain(&mut self, session_id: SessionId, action: PreviewDrainAction) -> bool {
        if let Some(pending) = self.pending_preview_drain.as_mut() {
            if pending.preview.identity().session_id == session_id {
                pending.action = action;
                return true;
            }
            return false;
        }
        let Some(preview) = self.rolling_preview.take() else {
            return false;
        };
        if preview.identity().session_id != session_id {
            self.rolling_preview = Some(preview);
            return false;
        }
        preview.close();
        self.pending_preview_drain = Some(PendingPreviewDrain {
            preview,
            action,
            closed_at: Instant::now(),
            cancel_requested_at: None,
            timeout_reported: false,
        });
        true
    }

    fn has_preview_for_session(&self, session_id: SessionId) -> bool {
        self.rolling_preview
            .as_ref()
            .is_some_and(|preview| preview.identity().session_id == session_id)
            || self
                .pending_preview_drain
                .as_ref()
                .is_some_and(|pending| pending.preview.identity().session_id == session_id)
    }

    fn poll_preview_drain(&mut self) {
        let service = self.transcription_service.clone();
        self.poll_preview_drain_at(Instant::now(), move || service.cancel_active());
    }

    fn poll_preview_drain_at(&mut self, now: Instant, request_cancel: impl FnOnce()) {
        let Some(pending) = self.pending_preview_drain.as_mut() else {
            return;
        };
        let mut terminal_timeout = None;
        if !pending.preview.is_finished() {
            if pending.cancel_requested_at.is_none()
                && now.saturating_duration_since(pending.closed_at) >= PREVIEW_FINISH_GRACE
            {
                request_cancel();
                pending.cancel_requested_at = Some(now);
                self.status_message = "Cancelling a slow live preview before finalizing".to_owned();
            } else if pending.cancel_requested_at.is_some_and(|cancelled_at| {
                now.saturating_duration_since(cancelled_at) >= PREVIEW_CANCEL_ACK_WARNING
            }) && !pending.timeout_reported
            {
                pending.timeout_reported = true;
                let session_id = pending.preview.identity().session_id;
                pending.action = PreviewDrainAction::ReapAfterFailure;
                terminal_timeout = Some(session_id);
            }
            if let Some(session_id) = terminal_timeout {
                let message = "Live preview did not acknowledge cancellation; final transcription and paste were cancelled. New dictation is blocked until the native worker exits";
                let _ = self.session_coordinator.fail(session_id);
                self.pending_output = None;
                self.retire_captured_target(session_id);
                self.status = TranscriptionStatus::Error;
                self.status_message = message.to_owned();
                self.finish_overlay_error(session_id, message);
            }
            return;
        }

        let mut pending = self
            .pending_preview_drain
            .take()
            .expect("preview drain checked above");
        let identity = pending.preview.identity().clone();
        if !pending.preview.stop_and_join(Duration::ZERO) {
            self.pending_preview_drain = Some(pending);
            return;
        }
        let _ = self.session_coordinator.finish_preview(
            identity.session_id,
            identity.request_id,
            &identity.model_id,
        );
        match pending.action {
            PreviewDrainAction::Continue => {
                if self
                    .active_recording
                    .as_ref()
                    .is_some_and(|active| active.session_id == identity.session_id)
                {
                    self.status = TranscriptionStatus::Listening;
                    self.status_message =
                        "Listening. Live preview stopped; the final pass remains enabled."
                            .to_owned();
                }
            }
            PreviewDrainAction::FinishCapture(capture) => self.finish_capture(*capture),
            PreviewDrainAction::ReapAfterFailure => {}
            PreviewDrainAction::Fail {
                session_id,
                message,
            } => self.fail_dictation_session_now(session_id, message),
            PreviewDrainAction::Cancel { session_id } => {
                let _ = self.session_coordinator.cancel_active();
                self.retire_captured_target(session_id);
                let _ = self.overlay_controller.hide(session_id);
                self.playground_runs.remove(&session_id);
                self.playground_pending = 0;
                self.refresh_playground_runtime_statuses();
            }
        }
    }

    fn shutdown_rolling_preview(&mut self, deadline: Instant) {
        let preview = self
            .pending_preview_drain
            .take()
            .map(|pending| pending.preview)
            .or_else(|| self.rolling_preview.take());
        let Some(mut preview) = preview else {
            return;
        };
        preview.close();
        self.transcription_service.cancel_active();
        if !preview.stop_and_join(deadline.saturating_duration_since(Instant::now())) {
            eprintln!(
                "rolling preview did not stop before the process-exit deadline for session {}",
                preview.identity().session_id.0
            );
            // The handle still owns a live native decoder. Returning would
            // eventually unload its DLL; dropping would have to wait forever.
            // Abort skips teardown and prevents either unsafe outcome.
            std::mem::forget(preview);
            std::process::abort();
        }
    }

    fn shutdown_transcription_for_exit(&mut self) {
        self.stop_and_discard_active_recording();
        let deadline = Instant::now() + Duration::from_secs(2);
        self.shutdown_rolling_preview(deadline);
        self.transcription_service.cancel_active();
        let compatibility_stopped = self
            .transcription_service
            .cancel_active_and_wait(deadline.saturating_duration_since(Instant::now()));
        let runtime_stopped = compatibility_stopped
            && self
                .transcription_service
                .shutdown_runtime_and_wait(deadline.saturating_duration_since(Instant::now()));
        if !runtime_stopped {
            eprintln!("native transcription shutdown exceeded the process-exit deadline");
            std::process::abort();
        }
    }

    fn poll_rolling_preview(&mut self) {
        let Some(event) = self
            .rolling_preview
            .as_ref()
            .and_then(RollingPreviewHandle::try_next)
        else {
            return;
        };
        self.apply_rolling_preview_event(event);
    }

    fn apply_rolling_preview_event(&mut self, event: PreviewEvent<anyhow::Error>) {
        match event {
            PreviewEvent::Update { identity, update } => {
                if self
                    .session_coordinator
                    .accept_preview_update(
                        identity.session_id,
                        identity.request_id,
                        &identity.model_id,
                        identity.sequence,
                    )
                    .is_err()
                {
                    return;
                }
                if !update.committed.is_empty() || !update.tentative.is_empty() {
                    if let Some(active) = self.active_recording.as_mut()
                        && active.session_id == identity.session_id
                        && active.latency.first_partial_at.is_none()
                    {
                        active.latency.first_partial_at = Some(Instant::now());
                    }
                    if let Some(pending) = self.pending_recording.as_mut()
                        && pending.session_id == identity.session_id
                        && pending.latency.first_partial_at.is_none()
                    {
                        pending.latency.first_partial_at = Some(Instant::now());
                    }
                }
                let _ = self.overlay_controller.update_transcript(
                    identity.session_id,
                    update.committed,
                    update.tentative,
                    identity.sequence,
                );
            }
            PreviewEvent::Error { identity, error } => {
                if self.session_coordinator.is_current_preview(
                    identity.session_id,
                    identity.request_id,
                    &identity.model_id,
                ) {
                    self.status_message = format!(
                        "Listening. Live preview stopped; the final pass remains enabled: {error}"
                    );
                    let _ = self.overlay_controller.show_preview_unavailable(
                        identity.session_id,
                        "Live preview stopped; final transcription continues.",
                    );
                    let _ =
                        self.begin_preview_drain(identity.session_id, PreviewDrainAction::Continue);
                }
            }
        }
    }

    fn stop_recording(&mut self) {
        if self.deferred_recording_start.take().is_some() {
            self.status = TranscriptionStatus::Idle;
            self.status_message = "Recording cancelled".to_owned();
            return;
        }
        if let Some(pending) = self.pending_recording.as_mut()
            && !pending.stop_requested
        {
            let _ = self
                .session_coordinator
                .request_stop(pending.session_id, StopReason::Explicit);
            pending.stop_requested = true;
            pending.latency.stop_requested_at = Some(Instant::now());
            self.status_message = "Cancelling microphone startup".to_owned();
            return;
        }
        if let Some(active) = self.active_recording.as_mut()
            && !active.stop_requested
        {
            let _ = self
                .session_coordinator
                .request_stop(active.session_id, StopReason::Explicit);
            active.session.stop();
            active.stop_requested = true;
            active.latency.stop_requested_at = Some(Instant::now());
            self.status_message = "Stopping recording".to_owned();
        }
    }

    fn toggle_recording(&mut self) {
        self.toggle_recording_at(Instant::now(), TriggerObservation::AppAction);
    }

    fn toggle_recording_at(
        &mut self,
        activation_at: Instant,
        trigger_observation: TriggerObservation,
    ) {
        if self.capture_is_active() || self.deferred_recording_start.is_some() {
            self.stop_recording();
        } else {
            self.start_recording_at(
                RecordingSource::Transcribe,
                activation_at,
                trigger_observation,
            );
        }
    }

    fn poll_recording(&mut self) {
        let finished = self.active_recording.as_ref().and_then(|active| {
            active
                .session
                .try_finish()
                .map(|result| (active.source, active.session_id, result))
        });

        if let Some((source, session_id, result)) = finished {
            let active = self
                .active_recording
                .take()
                .expect("finished recording should still be active");
            let capture = FinishedCapture {
                session_id,
                source,
                result,
                stop_requested: active.stop_requested,
                started_at: active.started_at,
                latency: active.latency,
                capture_diagnostics: active.capture_diagnostics,
            };
            if self.has_preview_for_session(session_id) {
                let scheduled = self.begin_preview_drain(
                    session_id,
                    PreviewDrainAction::FinishCapture(Box::new(capture)),
                );
                debug_assert!(scheduled);
                self.status_message = "Finalizing live preview before the full pass".to_owned();
            } else {
                self.finish_capture(capture);
            }
        }
    }

    fn finish_capture(&mut self, mut capture: FinishedCapture) {
        let session_id = capture.session_id;
        match capture.result {
            Ok(completion) => {
                capture.latency.observe_capture_metrics(&completion.metrics);
                if !capture.stop_requested {
                    let reason = match completion.stop_reason {
                        CaptureStopReason::Explicit => StopReason::Explicit,
                        CaptureStopReason::Endpoint => StopReason::Endpoint,
                        CaptureStopReason::MaximumDuration => StopReason::MaximumDuration,
                    };
                    if let Err(err) = self.session_coordinator.request_stop(session_id, reason) {
                        self.fail_dictation_session_now(
                            session_id,
                            format!("Rejected capture stop reason: {err}"),
                        );
                        return;
                    }
                    let observed_at = capture
                        .started_at
                        .checked_add(completion.metrics.stop_trigger_elapsed)
                        .unwrap_or_else(Instant::now)
                        .min(Instant::now());
                    capture.latency.stop_requested_at = Some(observed_at);
                }
                debug_assert!(self.session_coordinator.stop_reason().is_some());
                capture.latency.capture_finalized_at = Some(Instant::now());
                if let Err(err) = self.session_coordinator.capture_finalized(session_id) {
                    self.fail_dictation_session_now(
                        session_id,
                        format!("Rejected stale capture result: {err}"),
                    );
                    return;
                }
                let Some(audio) = completion.audio else {
                    let feedback = no_speech_feedback_for_capture(
                        capture.latency.maximum_input_rms,
                        &capture.capture_diagnostics,
                    );
                    let _ = self.session_coordinator.cancel_active();
                    self.status = TranscriptionStatus::Idle;
                    self.status_message = feedback.status_message;
                    self.record_session_diagnostic(
                        session_id,
                        &capture.latency,
                        DiagnosticSessionOutcome::Cancelled,
                        Some(DiagnosticFailureStage::NoSpeech),
                    );
                    self.latest_latency = Some(capture.latency);
                    self.finish_overlay_error(session_id, feedback.overlay_message);
                    return;
                };
                self.status = TranscriptionStatus::Transcribing;
                self.status_message = format!(
                    "Transcribing {} ms of locally prepared audio",
                    audio.duration_ms()
                );
                match capture.source {
                    RecordingSource::Transcribe => {
                        self.dispatch_default_transcription(session_id, audio, capture.latency)
                    }
                    RecordingSource::Playground => {
                        self.dispatch_playground_transcriptions(session_id, audio)
                    }
                }
            }
            Err(error) => {
                capture.latency.capture_finalized_at = Some(Instant::now());
                self.record_session_diagnostic(
                    session_id,
                    &capture.latency,
                    DiagnosticSessionOutcome::Failed,
                    Some(DiagnosticFailureStage::Capture),
                );
                self.latest_latency = Some(capture.latency);
                self.fail_dictation_session_now(session_id, format!("Recording failed: {error}"));
            }
        }
    }

    fn poll_hotkey(&mut self) {
        for observed in self.hotkey_service.poll_events() {
            if observed.event == HotkeyEvent::Pressed
                && self.consume_armed_history_repaste(observed.observed_at)
            {
                continue;
            }
            match hotkey_recording_action(
                self.config.recording.hotkey_mode,
                observed.event,
                self.recording_source(),
            ) {
                Some(HotkeyRecordingAction::StartTranscribe) => self.start_recording_at(
                    RecordingSource::Transcribe,
                    observed.observed_at,
                    TriggerObservation::HotkeyPoll,
                ),
                Some(HotkeyRecordingAction::Stop) => self.stop_recording(),
                Some(HotkeyRecordingAction::Toggle) => {
                    self.toggle_recording_at(observed.observed_at, TriggerObservation::HotkeyPoll)
                }
                None => {}
            }
        }
    }

    fn poll_tray(&mut self, ctx: &egui::Context) {
        while let Some(command) = self
            .tray_service
            .as_ref()
            .and_then(TrayService::poll_command)
        {
            self.apply_tray_command(command, ctx);
            if command == TrayCommand::Quit {
                break;
            }
        }
    }

    fn sync_tray_state(&mut self) {
        let Some(tray_service) = &self.tray_service else {
            self.last_tray_state = None;
            return;
        };

        let current = tray_ui_state(self.capture_is_active(), &self.transcript);
        if !tray_state_needs_sync(self.last_tray_state, current) {
            return;
        }

        tray_service.set_recording(current.is_recording);
        tray_service.set_has_transcript(current.has_transcript);
        self.last_tray_state = Some(current);
    }

    fn apply_tray_command(&mut self, command: TrayCommand, ctx: &egui::Context) {
        match command {
            TrayCommand::Show => self.show_window(ctx),
            TrayCommand::Hide => self.hide_window(ctx),
            TrayCommand::ToggleRecording => self.toggle_recording(),
            TrayCommand::CopyLastTranscript => self.copy_transcript_to_clipboard(),
            TrayCommand::Quit => {
                self.quit_requested = true;
                self.window_hidden_to_tray = false;
                self.deferred_recording_start = None;
                self.deferred_history_playback = None;
                if let Some(tray_service) = &self.tray_service {
                    tray_service.cancel_hidden_repaint();
                }
                self.stop_microphone_test();
                self.shutdown_transcription_for_exit();
                let _ = self.session_coordinator.cancel_active();
                self.flush_settings();
                ctx.send_viewport_cmd(ViewportCommand::Close);
            }
        }
    }

    fn handle_close_request(&mut self, ctx: &egui::Context) {
        let close_requested = ctx.input(|input| input.viewport().close_requested());
        if close_requested
            && self.config.general.close_to_tray
            && self.tray_service.is_some()
            && !self.quit_requested
        {
            ctx.send_viewport_cmd(ViewportCommand::CancelClose);
            self.hide_window(ctx);
        }
    }

    fn hide_window(&mut self, ctx: &egui::Context) {
        if let Some(tray_service) = &self.tray_service
            && let Err(error) = tray_service.schedule_hidden_repaint(self.next_repaint_delay())
        {
            self.status_message = format!(
                "Could not keep Scribe responsive in the tray; the window remains open: {error}"
            );
            return;
        }
        self.suspend_microphone_monitor();
        self.window_hidden_to_tray = true;
        ctx.send_viewport_cmd(ViewportCommand::Visible(false));
        self.status_message = "Scribe is running in the tray".to_owned();
    }

    fn show_window(&mut self, ctx: &egui::Context) {
        self.window_hidden_to_tray = false;
        if let Some(tray_service) = &self.tray_service {
            tray_service.cancel_hidden_repaint();
        }
        ctx.send_viewport_cmd(ViewportCommand::Visible(true));
        ctx.send_viewport_cmd(ViewportCommand::Focus);
        self.status_message = "Scribe window restored".to_owned();
    }

    fn copy_transcript_to_clipboard(&mut self) {
        let transcript = self.transcript.clone();
        self.copy_text_to_clipboard(&transcript, "Transcript");
    }

    fn clear_transcript_history(&mut self) {
        self.transcript.clear();
        self.raw_transcript.clear();
        if let Some(pending) = self.pending_output.take() {
            if let Some(history_id) = pending.history_id {
                self.record_history_output_label(history_id, "cancelled_by_user");
            }
            if let Some(latency) = pending.latency.as_ref() {
                self.record_session_diagnostic(
                    pending.session_id,
                    latency,
                    DiagnosticSessionOutcome::Cancelled,
                    None,
                );
            }
            let _ = self.session_coordinator.cancel_active();
            self.retire_captured_target(pending.session_id);
            let _ = self.overlay_controller.hide(pending.session_id);
            self.overlay_hide_at = None;
            self.status = TranscriptionStatus::Idle;
            self.status_message = "Transcript cleared; pending output cancelled".to_owned();
        } else {
            self.status_message = "Transcript cleared".to_owned();
        }
    }

    fn copy_text_to_clipboard(&mut self, text: &str, label: &str) {
        if text.trim().is_empty() {
            self.status_message = format!("No {} to copy", label.to_ascii_lowercase());
            return;
        }

        match Clipboard::new().and_then(|mut clipboard| clipboard.set_text(text.to_owned())) {
            Ok(()) => self.status_message = format!("{label} copied"),
            Err(err) => {
                self.status_message = format!("Clipboard failed: {err}");
            }
        }
    }

    fn request_history_page(&mut self, append: bool) {
        if self.history_loading {
            if !append {
                self.history_refresh_pending = true;
            }
            return;
        }
        let Some(store) = self.history_store.clone() else {
            return;
        };
        let before = if append { self.history_next } else { None };
        if append && before.is_none() {
            return;
        }
        let search = self.history_applied_search.clone();
        let tx = self.tx.clone();
        self.history_query_sequence = self.history_query_sequence.wrapping_add(1);
        let query_id = self.history_query_sequence;
        self.active_history_query = Some(query_id);
        self.history_loading = true;
        thread::spawn(move || {
            let result = store
                .search(HistoryQuery {
                    text: (!search.is_empty()).then_some(search.clone()),
                    before,
                    limit: 20,
                    ..HistoryQuery::default()
                })
                .map_err(|error| error.to_string());
            let _ = tx.send(AppEvent::HistoryPageLoaded {
                query_id,
                append,
                search,
                result,
            });
        });
    }

    fn apply_history_action(&mut self, action: HistoryPageAction) {
        match action {
            HistoryPageAction::ApplySearch => {
                self.history_applied_search = self.history_search.trim().to_owned();
                self.history_records.clear();
                self.history_next = None;
                self.request_history_page(false);
            }
            HistoryPageAction::Refresh => self.request_history_page(false),
            HistoryPageAction::LoadMore => self.request_history_page(true),
            HistoryPageAction::Copy { text, label } => self.copy_text_to_clipboard(&text, label),
            HistoryPageAction::ArmRepaste { id, text } => {
                self.armed_history_repaste = Some(ArmedHistoryRepaste {
                    id,
                    text,
                    expires_at: Instant::now() + Duration::from_secs(30),
                });
                self.status_message = format!(
                    "Paste armed for history entry {id}. Focus the destination and press {} within 30 seconds.",
                    self.config.recording.hotkey
                );
            }
            HistoryPageAction::TogglePinned { id, pinned } => {
                self.start_history_mutation(
                    if pinned {
                        "History entry pinned"
                    } else {
                        "History entry unpinned"
                    },
                    move |store| {
                        store
                            .set_pinned(id, pinned)
                            .map(|_| ())
                            .map_err(|error| error.to_string())
                    },
                );
            }
            HistoryPageAction::Play(history_id) => {
                if self.capture_is_active() || self.deferred_recording_start.is_some() {
                    self.status_message = "Stop recording before playing retained audio".to_owned();
                    return;
                }
                if self.microphone_test_is_active() {
                    self.deferred_history_playback = Some(history_id);
                    self.stop_microphone_test();
                    self.status_message = "Preparing audio playback".to_owned();
                    return;
                }
                let Some(store) = self.history_store.clone() else {
                    return;
                };
                self.playing_history_id = Some(history_id);
                self.history_playback_stopping = false;
                let tx = self.tx.clone();
                thread::spawn(move || {
                    let result = store
                        .validated_audio_path(history_id)
                        .map_err(|error| error.to_string());
                    let _ = tx.send(AppEvent::HistoryPlaybackPathReady { history_id, result });
                });
            }
            HistoryPageAction::StopPlayback => {
                self.history_playback_stopping = true;
                self.status_message = "Stopping history playback".to_owned();
                if let Some(playback) = self.history_playback.as_ref()
                    && let Err(error) = playback.stop()
                {
                    if error == crate::history_playback::PlaybackCommandError::Disconnected {
                        self.playing_history_id = None;
                        self.history_playback_stopping = false;
                    }
                    self.status_message = format!("Could not stop history playback: {error}");
                }
                if self.history_playback.is_none() {
                    self.playing_history_id = None;
                    self.history_playback_stopping = false;
                    self.status_message = "Native history playback is unavailable".to_owned();
                }
            }
            HistoryPageAction::Retry(history_id) => self.start_history_retry(history_id),
            HistoryPageAction::DeleteAudio(id) => {
                self.start_history_mutation("Retained audio deleted", move |store| {
                    store
                        .delete_audio(id)
                        .map(|_| ())
                        .map_err(|error| error.to_string())
                });
            }
            HistoryPageAction::RequestDelete(id) => {
                self.history_delete_confirmation = Some(id);
                self.history_confirmation_focus_pending = true;
            }
            HistoryPageAction::ConfirmDelete(id) => {
                self.history_delete_confirmation = None;
                self.history_search_focus_pending = true;
                self.delete_history_entry(id);
            }
            HistoryPageAction::CancelDelete => {
                self.history_delete_confirmation = None;
                self.history_search_focus_pending = true;
            }
        }
    }

    fn delete_history_entry(&mut self, id: i64) {
        if self.playing_history_id == Some(id)
            && let Some(playback) = self.history_playback.as_ref()
        {
            let _ = playback.stop();
        }
        if self.armed_history_repaste.as_ref().map(|armed| armed.id) == Some(id) {
            self.armed_history_repaste = None;
        }
        self.start_history_mutation("History entry deleted", move |store| {
            store.delete(id).map_err(|error| error.to_string())
        });
    }

    fn start_history_retry(&mut self, history_id: i64) {
        if self.has_active_work() {
            self.status_message =
                "Wait for the active dictation or playback before retrying history".to_owned();
            return;
        }
        if self.playing_history_id.is_some() {
            self.status_message = "Stop history playback before retrying".to_owned();
            return;
        }
        let Some(record) = self
            .history_records
            .iter()
            .find(|record| record.id == history_id)
            .cloned()
        else {
            self.status_message = "History entry is no longer available".to_owned();
            return;
        };
        if record.status != HistoryStatus::Failed || record.audio_path.is_none() {
            self.status_message = "Retry requires a failed entry with retained audio".to_owned();
            return;
        }
        let Some(model) = config::configured_models(&self.config)
            .into_iter()
            .find(|model| model.id == record.model_id)
        else {
            self.status_message =
                "The model used by this history entry is no longer configured".to_owned();
            return;
        };
        let runtime_status = runtime_status_for_model(&self.config, &model);
        if runtime_status != ModelRuntimeStatus::Ready {
            self.status_message = format!(
                "The history model is not ready: {}",
                setup_message_for_status(&runtime_status)
            );
            return;
        }
        let Some(store) = self.history_store.clone() else {
            self.status_message = "Local history is unavailable".to_owned();
            return;
        };

        self.supersede_active_session();
        let session_id = match self.session_coordinator.begin(SessionPurpose::Dictation) {
            Ok(id) => id,
            Err(error) => {
                self.status_message = format!("Could not start history retry: {error}");
                return;
            }
        };
        if self
            .session_coordinator
            .capture_started(session_id)
            .and_then(|_| {
                self.session_coordinator
                    .request_stop(session_id, StopReason::Explicit)
                    .map(|_| ())
            })
            .and_then(|_| self.session_coordinator.capture_finalized(session_id))
            .is_err()
        {
            let _ = self.session_coordinator.fail(session_id);
            self.status_message = "Could not initialize history retry state".to_owned();
            return;
        }
        let request_id = match self
            .session_coordinator
            .start_request(session_id, ModelId::new(model.id.clone()))
        {
            Ok(id) => id,
            Err(error) => {
                let _ = self.session_coordinator.fail(session_id);
                self.status_message = format!("Could not start history retry: {error}");
                return;
            }
        };
        let service = self.transcription_service.with_config(self.config.clone());
        let task = match service.begin_transcription_task() {
            Ok(task) => task,
            Err(error) => {
                let _ = self.session_coordinator.fail(session_id);
                self.status_message = format!("Could not dispatch history retry: {error}");
                return;
            }
        };
        self.history_requests.insert(
            (session_id, request_id),
            HistoryRequestContext {
                id: history_id,
                kind: HistoryRequestKind::Retry,
            },
        );
        self.leased_history_retry_ids.insert(history_id);
        self.begin_overlay_session(session_id, NativeOverlayMode::Off, None);
        self.status = TranscriptionStatus::Transcribing;
        self.status_message = "Loading retained audio for history retry".to_owned();
        let tx = self.tx.clone();
        thread::spawn(move || {
            let retry = match store.retry(history_id) {
                Ok(retry) => retry,
                Err(error) => {
                    // Retry may have been dequeued before a caller-side reply
                    // timeout. Queue terminal cleanup behind it so the row
                    // cannot remain Pending in that ambiguous case.
                    let (retry_lease_released, terminal_result) = settle_failed_history_retry(
                        &store,
                        history_id,
                        "History retry could not be started",
                    );
                    let message = match terminal_result {
                        Ok(()) => error.to_string(),
                        Err(terminal_error) => format!(
                            "{error}; terminal retry cleanup was not clean: {terminal_error}"
                        ),
                    };
                    let _ = tx.send(AppEvent::HistoryRetryStartFailed {
                        session_id,
                        request_id,
                        history_id,
                        retry_lease_released,
                        message,
                    });
                    return;
                }
            };
            if retry.record.id != history_id || retry.record.model_id != model.id {
                let message = "history retry returned mismatched record identity".to_owned();
                let (retry_lease_released, terminal_result) = settle_failed_history_retry(
                    &store,
                    history_id,
                    "History retry identity was rejected",
                );
                let message = match terminal_result {
                    Ok(()) => message,
                    Err(terminal_error) => {
                        format!("{message}; terminal retry cleanup was not clean: {terminal_error}")
                    }
                };
                let _ = tx.send(AppEvent::HistoryRetryStartFailed {
                    session_id,
                    request_id,
                    history_id,
                    retry_lease_released,
                    message,
                });
                return;
            }
            let mut latency =
                LatencyTrace::started_at(Instant::now(), TriggerObservation::AppAction);
            latency.transcription_dispatched_at = Some(Instant::now());
            let mut request = TranscriptionRequest::new(
                session_id,
                request_id,
                Arc::new(retry.audio),
                model.id.clone(),
            );
            request.model_path = model.local_path.clone();
            request.options = TranscriptionOptions::default();
            let result = service.transcribe_task(request, task);
            latency.transcription_job_completed_at = Some(Instant::now());
            match result {
                Ok(result) => {
                    let _ = tx.send(AppEvent::TranscriptionDone {
                        source: RecordingSource::Transcribe,
                        session_id,
                        request_id,
                        result: Box::new(result),
                        latency: Some(latency),
                    });
                }
                Err(error) => {
                    let _ = tx.send(AppEvent::TranscriptionFailed {
                        source: RecordingSource::Transcribe,
                        session_id,
                        request_id,
                        model_id: model.id,
                        message: error.to_string(),
                        latency: Some(latency),
                    });
                }
            }
        });
    }

    fn poll_history_playback(&mut self) {
        while let Some(event) = self
            .history_playback
            .as_ref()
            .and_then(PlaybackService::try_next_event)
        {
            match event {
                PlaybackEvent::Started { history_id } => {
                    self.playing_history_id = Some(history_id);
                    if !self.history_playback_stopping {
                        self.status_message =
                            format!("Playing retained audio for entry {history_id}");
                    }
                }
                PlaybackEvent::Completed { history_id } => {
                    if self.playing_history_id == Some(history_id) {
                        self.playing_history_id = None;
                    }
                    self.history_playback_stopping = false;
                    self.status_message = "History playback finished".to_owned();
                }
                PlaybackEvent::Stopped { history_id } => {
                    if self.playing_history_id == Some(history_id) {
                        self.playing_history_id = None;
                    }
                    self.history_playback_stopping = false;
                    self.status_message = "History playback stopped".to_owned();
                }
                PlaybackEvent::Failed { history_id, error } => {
                    if self.playing_history_id == Some(history_id) {
                        self.playing_history_id = None;
                    }
                    self.history_playback_stopping = false;
                    self.status = TranscriptionStatus::Error;
                    self.status_message = format!("History playback failed: {error}");
                }
            }
        }
    }

    fn consume_armed_history_repaste(&mut self, observed_at: Instant) -> bool {
        self.consume_armed_history_repaste_with(
            observed_at,
            overlay::capture_foreground_target,
            text_output::write_to_captured_target,
        )
    }

    fn consume_armed_history_repaste_with<O>(
        &mut self,
        observed_at: Instant,
        capture_target: impl FnOnce() -> Option<CapturedTarget>,
        write_output: impl FnOnce(&str, &AppConfig, Option<&CapturedTarget>) -> O,
    ) -> bool
    where
        O: Into<text_output::TextOutputOutcome>,
    {
        if self.has_active_work() {
            self.armed_history_repaste = None;
            return false;
        }
        let Some(armed) = self.armed_history_repaste.take() else {
            return false;
        };
        if observed_at >= armed.expires_at {
            self.status_message = "Paste-again request expired".to_owned();
            return false;
        }

        let target = capture_target();
        let mut output_config = self.config.clone();
        output_config.output.auto_insert_transcript = true;
        let outcome = write_output(&armed.text, &output_config, target.as_ref()).into();
        if let Some(target) = target.as_ref() {
            crate::overlay::platform::release_captured_target(target);
        }
        self.status = if matches!(outcome.result, text_output::TextOutputResult::Failed(_)) {
            TranscriptionStatus::Error
        } else {
            TranscriptionStatus::Idle
        };
        self.status_message = format!(
            "History entry {}: {}",
            armed.id,
            outcome.result.status_message()
        );
        self.record_history_output_outcome(armed.id, &outcome.result);
        true
    }

    fn record_history_output_outcome(
        &self,
        history_id: i64,
        result: &text_output::TextOutputResult,
    ) {
        let outcome = match result {
            text_output::TextOutputResult::Inserted => "inserted",
            text_output::TextOutputResult::InsertedClipboardRestoreFailed(_) => {
                "inserted_clipboard_restore_failed"
            }
            text_output::TextOutputResult::CopiedOnly(
                text_output::CopyOnlyReason::TargetUnavailable,
            ) => "copied_only_target_unavailable",
            text_output::TextOutputResult::CopiedOnly(
                text_output::CopyOnlyReason::AutomationUnavailable,
            ) => "copied_only_automation_unavailable",
            text_output::TextOutputResult::CopiedOnly(text_output::CopyOnlyReason::PasteFailed) => {
                "copied_only_paste_failed"
            }
            text_output::TextOutputResult::CopiedOnly(
                text_output::CopyOnlyReason::ClipboardSnapshotUnavailable,
            ) => "copied_only_clipboard_snapshot_unavailable",
            text_output::TextOutputResult::CopiedOnly(
                text_output::CopyOnlyReason::ClipboardSnapshotUnsupported,
            ) => "copied_only_clipboard_snapshot_unsupported",
            text_output::TextOutputResult::CopiedOnly(
                text_output::CopyOnlyReason::ClipboardSnapshotError,
            ) => "copied_only_clipboard_snapshot_error",
            text_output::TextOutputResult::NotInserted(_) => "not_inserted_clipboard_changed",
            text_output::TextOutputResult::Failed(_) => "failed",
        }
        .to_owned();
        self.record_history_output_label(history_id, outcome);
    }

    fn record_history_output_label(&self, history_id: i64, outcome: impl Into<String>) {
        let Some(store) = self.history_store.clone() else {
            return;
        };
        let outcome = outcome.into();
        let tx = self.tx.clone();
        thread::spawn(move || {
            let result = store
                .record_output_outcome(history_id, outcome)
                .map(|_| ())
                .map_err(|error| error.to_string());
            let _ = tx.send(AppEvent::HistoryOutputRecorded { result });
        });
    }

    fn transcription_event_is_current(
        &self,
        source: RecordingSource,
        session_id: SessionId,
        request_id: RequestId,
    ) -> bool {
        self.session_coordinator
            .is_current_request(source.purpose(), session_id, request_id)
    }

    fn expected_playground_model_id(
        &self,
        session_id: SessionId,
        request_id: RequestId,
    ) -> Option<&str> {
        self.session_coordinator
            .request_model(session_id, request_id)
            .map(ModelId::as_str)
    }

    fn reject_transcription_correlation(
        &mut self,
        source: RecordingSource,
        session_id: SessionId,
        request_id: RequestId,
        message: &str,
    ) {
        if source == RecordingSource::Transcribe {
            self.fail_dictation_session(session_id, message);
            return;
        }
        self.status = TranscriptionStatus::Error;
        self.status_message = message.to_owned();
        if source == RecordingSource::Playground {
            let expected_model_id = self
                .expected_playground_model_id(session_id, request_id)
                .map(str::to_owned);
            let mut output_changed = false;
            if let Some(expected_model_id) = expected_model_id.as_deref()
                && let Some(card) = self
                    .playground_cards
                    .iter_mut()
                    .find(|card| card.descriptor.id.as_str() == expected_model_id)
            {
                card.status = ModelRuntimeStatus::Error(message.to_owned());
                output_changed = !card.transcript.is_empty();
                card.transcript.clear();
                card.latency_ms = None;
            }
            if output_changed && let Some(expected_model_id) = expected_model_id {
                self.mark_comparison_output_changed(&expected_model_id);
            }
        }
    }

    fn poll_events(&mut self) {
        while let Ok(event) = self.rx.try_recv() {
            match event {
                AppEvent::MicrophoneTestReady { request_id, result } => {
                    let state = std::mem::take(&mut self.microphone_test);
                    match state {
                        MicrophoneTest::Starting {
                            request_id: expected,
                            stop_requested,
                            cancellation: _,
                        } if expected == request_id => match result {
                            Ok(session)
                                if stop_requested
                                    || self.capture_is_active()
                                    || self.playing_history_id.is_some() =>
                            {
                                session.stop();
                                self.microphone_test = MicrophoneTest::Stopping { session };
                            }
                            Ok(session) => {
                                session.set_manual_activation_threshold(
                                    self.config.recording.manual_activation_rms,
                                );
                                self.microphone_level_envelope.reset_source();
                                self.microphone_test = MicrophoneTest::Active { session };
                            }
                            Err(error) => {
                                self.microphone_level_envelope.reset_source();
                                if !stop_requested {
                                    self.microphone_test_error = Some(error.to_string());
                                    self.microphone_monitor_retry_required = true;
                                    self.status_message =
                                        format!("Microphone monitoring unavailable: {error}");
                                }
                            }
                        },
                        current => {
                            self.microphone_test = current;
                            if let Ok(session) = result {
                                discard_recording_async(session);
                            }
                        }
                    }
                }
                AppEvent::CaptureReady { session_id, result } => {
                    let Some(mut pending) = self.pending_recording.take() else {
                        if let Ok(session) = result {
                            let _ = session.stop_and_discard(Duration::from_secs(2));
                        }
                        continue;
                    };
                    if pending.session_id != session_id
                        || self.session_coordinator.active_session_id() != Some(session_id)
                    {
                        pending.abandon.store(true, Ordering::Release);
                        if let Ok(session) = result {
                            let _ = session.stop_and_discard(Duration::from_secs(2));
                        }
                        continue;
                    }
                    match result {
                        Ok(session) => {
                            let threshold = self.config.recording.manual_activation_rms;
                            session.set_manual_activation_threshold(threshold);
                            pending.capture_diagnostics.activation_floor = threshold;
                            if let Err(err) = self.session_coordinator.capture_started(session_id) {
                                let _ = session.stop_and_discard(Duration::from_secs(2));
                                self.fail_dictation_session(
                                    session_id,
                                    format!("Could not enter capture state: {err}"),
                                );
                                continue;
                            }
                            if pending.source == RecordingSource::Transcribe {
                                self.playground_pending = 0;
                                self.refresh_playground_runtime_statuses();
                            }
                            pending.latency.recorder_started_at = Some(Instant::now());
                            let stop_requested = pending.stop_requested;
                            self.active_recording = Some(ActiveRecording {
                                session_id,
                                session,
                                source: pending.source,
                                stop_requested,
                                started_at: Instant::now(),
                                max_duration_seconds: pending.max_duration_seconds,
                                latency: pending.latency,
                                capture_diagnostics: pending.capture_diagnostics,
                            });
                            if stop_requested {
                                if let Some(active) = self.active_recording.as_ref() {
                                    active.session.stop();
                                }
                                self.status_message = "Stopping recording".to_owned();
                            } else {
                                self.status = TranscriptionStatus::Listening;
                                self.status_message = "Listening".to_owned();
                            }
                        }
                        Err(message) => {
                            self.fail_dictation_session(
                                session_id,
                                format!("Microphone failed: {message}"),
                            );
                        }
                    }
                }
                AppEvent::ModelPreloadFinished {
                    session_id,
                    model_id,
                    load_duration_ms,
                } => {
                    if self
                        .session_coordinator
                        .model_load_finished(session_id, &model_id, true)
                        .is_err()
                    {
                        continue;
                    }
                    if let Some(active) = self.active_recording.as_mut()
                        && active.session_id == session_id
                    {
                        active.latency.model_loaded_at = Some(Instant::now());
                        self.status_message = if load_duration_ms == 0 {
                            "Listening. Model is warm.".to_owned()
                        } else {
                            format!("Listening. Model loaded in {load_duration_ms} ms.")
                        };
                    }
                    if let Some(pending) = self.pending_recording.as_mut()
                        && pending.session_id == session_id
                    {
                        pending.latency.model_loaded_at = Some(Instant::now());
                        self.status_message = if load_duration_ms == 0 {
                            "Preparing microphone. Model is warm.".to_owned()
                        } else {
                            format!("Preparing microphone. Model loaded in {load_duration_ms} ms.")
                        };
                    }
                }
                AppEvent::ModelPreloadFailed {
                    session_id,
                    model_id,
                    message,
                } => {
                    if self
                        .session_coordinator
                        .model_load_finished(session_id, &model_id, false)
                        .is_err()
                    {
                        continue;
                    }
                    if self
                        .active_recording
                        .as_ref()
                        .is_some_and(|active| active.session_id == session_id)
                    {
                        self.status_message = format!(
                            "Listening. Model warm-up was unavailable; final transcription will retry safely: {message}"
                        );
                    }
                    if self
                        .pending_recording
                        .as_ref()
                        .is_some_and(|pending| pending.session_id == session_id)
                    {
                        self.status_message = format!(
                            "Preparing microphone. Model warm-up was unavailable; final transcription will retry safely: {message}"
                        );
                    }
                }
                AppEvent::HistoryCompletionObserved {
                    history_id,
                    retry_lease_released,
                    result,
                } => {
                    if retry_lease_released {
                        self.leased_history_retry_ids.remove(&history_id);
                    }
                    if let Err(error) = result {
                        self.history_error = Some(format!(
                            "History entry {history_id} completion was not confirmed: {error}"
                        ));
                    }
                    if self.current_tab == Tab::History {
                        self.request_history_page(false);
                    }
                }
                AppEvent::HistoryRetryStartFailed {
                    session_id,
                    request_id,
                    history_id,
                    retry_lease_released,
                    message,
                } => {
                    if retry_lease_released {
                        self.leased_history_retry_ids.remove(&history_id);
                    }
                    if self
                        .history_requests
                        .get(&(session_id, request_id))
                        .copied()
                        == Some(HistoryRequestContext {
                            id: history_id,
                            kind: HistoryRequestKind::Retry,
                        })
                    {
                        self.history_requests.remove(&(session_id, request_id));
                        let model_id = self
                            .session_coordinator
                            .request_model(session_id, request_id)
                            .cloned();
                        if let Some(model_id) = model_id {
                            let _ = self
                                .session_coordinator
                                .fail_request(session_id, request_id, &model_id);
                        }
                        let _ = self.session_coordinator.fail(session_id);
                        self.status = TranscriptionStatus::Error;
                        self.status_message = format!("History retry failed: {message}");
                    }
                }
                AppEvent::HistoryPageLoaded {
                    query_id,
                    append,
                    search,
                    result,
                } => {
                    if self.active_history_query != Some(query_id) {
                        continue;
                    }
                    self.active_history_query = None;
                    self.history_loading = false;
                    if self.history_refresh_pending {
                        self.history_refresh_pending = false;
                        self.request_history_page(false);
                        continue;
                    }
                    if search != self.history_applied_search {
                        self.request_history_page(false);
                        continue;
                    }
                    match result {
                        Ok(page) => {
                            if append {
                                self.history_records.extend(page.records);
                            } else {
                                self.history_records = page.records;
                            }
                            self.history_next = page.next;
                            self.history_error = None;
                        }
                        Err(error) => self.history_error = Some(format!("History failed: {error}")),
                    }
                }
                AppEvent::HistoryMutationFinished {
                    operation_id,
                    message,
                    result,
                } => {
                    if self.history_mutation_in_flight != Some(operation_id) {
                        continue;
                    }
                    self.history_mutation_in_flight = None;
                    match result {
                        Ok(()) => {
                            self.status_message = message;
                            self.history_error = None;
                        }
                        Err(error) => {
                            self.history_error = Some(format!(
                                "History operation outcome was not confirmed: {error}"
                            ));
                        }
                    }
                    self.request_history_page(false);
                    if let Some(policy) = self.pending_history_retention_policy.take() {
                        self.start_history_retention_mutation(policy);
                    }
                }
                AppEvent::HistoryPlaybackPathReady { history_id, result } => {
                    if self.playing_history_id != Some(history_id) {
                        continue;
                    }
                    if self.history_playback_stopping {
                        self.playing_history_id = None;
                        self.history_playback_stopping = false;
                        self.status_message = "History playback stopped".to_owned();
                        continue;
                    }
                    match result {
                        Ok(Some(path)) => match self.history_playback.as_ref() {
                            Some(playback) => {
                                if let Err(error) = playback.play(history_id, path) {
                                    self.playing_history_id = None;
                                    self.history_playback_stopping = false;
                                    self.status_message =
                                        format!("Could not start history playback: {error}");
                                }
                            }
                            None => {
                                self.playing_history_id = None;
                                self.history_playback_stopping = false;
                                self.status_message =
                                    "Native history playback is unavailable".to_owned();
                            }
                        },
                        Ok(None) => {
                            self.playing_history_id = None;
                            self.history_playback_stopping = false;
                            self.status_message =
                                "This history entry has no retained audio".to_owned();
                            self.request_history_page(false);
                        }
                        Err(error) => {
                            self.playing_history_id = None;
                            self.history_playback_stopping = false;
                            self.history_error =
                                Some(format!("Could not load history audio: {error}"));
                            self.request_history_page(false);
                        }
                    }
                }
                AppEvent::HistoryOutputRecorded { result } => match result {
                    Ok(()) => {
                        if self.current_tab == Tab::History {
                            self.request_history_page(false);
                        }
                    }
                    Err(error) => {
                        self.history_error =
                            Some(format!("Could not update history metadata: {error}"));
                    }
                },
                AppEvent::HistoryRetryTerminalPersisted {
                    history_id,
                    retry_lease_released,
                    result,
                } => {
                    if retry_lease_released {
                        self.leased_history_retry_ids.remove(&history_id);
                    }
                    match result {
                        Ok(()) => {
                            if self.current_tab == Tab::History {
                                self.request_history_page(false);
                            }
                        }
                        Err(error) => {
                            self.history_error = Some(format!(
                                "Could not persist the terminal history retry state: {error}"
                            ));
                        }
                    }
                }
                AppEvent::RemoteCatalogLoaded { generation, result } => {
                    if self.remote_catalog.active_refresh_generation != Some(generation) {
                        continue;
                    }
                    self.remote_catalog.loading = false;
                    self.remote_catalog.active_refresh_generation = None;
                    match result {
                        Ok(snapshot) => {
                            self.remote_catalog.snapshot = Some(snapshot);
                            self.remote_catalog.error = None;
                            self.remote_catalog.invalidate_projection();
                        }
                        Err(error) => {
                            self.remote_catalog.error = Some(error);
                        }
                    }
                }
                AppEvent::LocalGgufImportFinished { job_id, result } => {
                    self.finish_local_gguf_import(job_id, result);
                }
                AppEvent::TranscriptionDone {
                    source,
                    session_id,
                    request_id,
                    result,
                    latency,
                } => {
                    let current =
                        self.transcription_event_is_current(source, session_id, request_id);
                    if result.session_id != session_id || result.request_id != request_id {
                        if current {
                            self.fail_correlated_history_request(
                                session_id,
                                request_id,
                                "Transcription service returned mismatched correlation IDs",
                            );
                            self.reject_transcription_correlation(
                                source,
                                session_id,
                                request_id,
                                "Transcription service returned mismatched correlation IDs",
                            );
                            let _ = self.session_coordinator.fail(session_id);
                        }
                        self.cleanup_after_job(source, session_id, request_id);
                        continue;
                    }
                    if !current {
                        self.cleanup_after_job(source, session_id, request_id);
                        continue;
                    }
                    if source == RecordingSource::Playground
                        && self.expected_playground_model_id(session_id, request_id)
                            != Some(result.model_id.as_str())
                    {
                        self.reject_transcription_correlation(
                            source,
                            session_id,
                            request_id,
                            "Transcription service returned the wrong model for a Playground request",
                        );
                        let _ = self.session_coordinator.fail(session_id);
                        self.cleanup_after_job(source, session_id, request_id);
                        continue;
                    }

                    let result_model_id = ModelId::new(result.model_id.as_str());
                    let all_requests_completed = match self.session_coordinator.complete_request(
                        session_id,
                        request_id,
                        &result_model_id,
                    ) {
                        Ok(all_completed) => all_completed,
                        Err(err) => {
                            self.fail_correlated_history_request(
                                session_id,
                                request_id,
                                format!("Rejected transcription completion: {err}"),
                            );
                            self.reject_transcription_correlation(
                                source,
                                session_id,
                                request_id,
                                &format!("Rejected transcription completion: {err}"),
                            );
                            let _ = self.session_coordinator.fail(session_id);
                            self.cleanup_after_job(source, session_id, request_id);
                            continue;
                        }
                    };

                    let mut latency = latency.map(|mut latency| {
                        latency.ui_result_at = Some(Instant::now());
                        latency.observe_transcription_outcome(&result);
                        latency
                    });
                    match source {
                        RecordingSource::Transcribe => {
                            let history_context =
                                self.history_requests.remove(&(session_id, request_id));
                            let Some(finalized_text) =
                                FinalizedText::without_cleanup(result.transcript.text.clone())
                            else {
                                let diagnostics = latency
                                    .as_ref()
                                    .and_then(|trace| trace.capture_diagnostics.as_ref())
                                    .cloned()
                                    .unwrap_or_default();
                                let feedback = no_speech_feedback_for_capture(
                                    latency.as_ref().and_then(|trace| trace.maximum_input_rms),
                                    &diagnostics,
                                );
                                if let Some(context) = history_context {
                                    self.finish_no_speech_history_context(
                                        context,
                                        feedback.overlay_message,
                                    );
                                }
                                if let Some(latency) = latency.as_mut() {
                                    latency.final_text_ready_at = None;
                                    latency.output_started_at = None;
                                    latency.target_activated_at = None;
                                    latency.paste_completed_at = None;
                                    latency.output_completed_at = None;
                                }
                                let _ = self.session_coordinator.cancel_active();
                                self.status = TranscriptionStatus::Idle;
                                self.status_message = feedback.status_message;
                                if let Some(latency) = latency.as_ref() {
                                    self.record_session_diagnostic(
                                        session_id,
                                        latency,
                                        DiagnosticSessionOutcome::Cancelled,
                                        Some(DiagnosticFailureStage::NoSpeech),
                                    );
                                }
                                self.latest_latency = latency;
                                self.finish_overlay_error(session_id, feedback.overlay_message);
                                self.cleanup_after_job(source, session_id, request_id);
                                continue;
                            };
                            if let Some(latency) = latency.as_mut() {
                                latency.final_text_ready_at = latency
                                    .transcription_job_completed_at
                                    .or_else(|| Some(Instant::now()));
                            }
                            let segment_count = result.transcript.segments.len();
                            let timed_segments = result
                                .transcript
                                .segments
                                .iter()
                                .filter(|segment| {
                                    segment.start_ms.is_some() || segment.end_ms.is_some()
                                })
                                .count();
                            let segment_text_bytes = result
                                .transcript
                                .segments
                                .iter()
                                .map(|segment| segment.text.len())
                                .sum::<usize>();
                            let stdout_bytes = result.stdout.len();
                            let stderr_bytes = result.stderr.len();
                            self.raw_transcript = finalized_text.raw;
                            self.transcript = finalized_text.final_text;
                            let _ = self
                                .overlay_controller
                                .replace_with_final(session_id, self.transcript.clone());
                            self.status = TranscriptionStatus::Idle;
                            let completion_message = format!(
                                "{} finished in {} ms ({} segment(s), {} timed, {} text bytes, {} stdout bytes, {} stderr bytes)",
                                result.model_name,
                                result.processing_duration_ms.unwrap_or_default(),
                                segment_count,
                                timed_segments,
                                segment_text_bytes,
                                stdout_bytes,
                                stderr_bytes
                            );
                            if let Some(context) = history_context {
                                let history_entry = CompletedHistoryEntry {
                                    raw_text: self.raw_transcript.clone(),
                                    final_text: self.transcript.clone(),
                                    metrics: HistoryMetrics {
                                        audio_duration_ms: result
                                            .transcript
                                            .duration_ms
                                            .and_then(|value| u64::try_from(value).ok()),
                                        processing_duration_ms: result
                                            .processing_duration_ms
                                            .and_then(|value| u64::try_from(value).ok()),
                                        realtime_factor: result
                                            .processing_duration_ms
                                            .zip(result.transcript.duration_ms)
                                            .filter(|(_, audio)| *audio > 0)
                                            .map(|(processing, audio)| {
                                                processing as f64 / audio as f64
                                            }),
                                    },
                                };
                                let pending = PendingHistoryCompletion {
                                    session_id,
                                    history_id: context.id,
                                    kind: context.kind,
                                    transcript: self.transcript.clone(),
                                    output_config: self.config.clone(),
                                    completion_message,
                                    latency,
                                };
                                let queued = self
                                    .history_store
                                    .as_ref()
                                    .ok_or_else(|| "history worker is unavailable".to_owned())
                                    .and_then(|store| {
                                        match context.kind {
                                            HistoryRequestKind::Dictation => {
                                                store.enqueue_complete(context.id, history_entry)
                                            }
                                            HistoryRequestKind::Retry => store
                                                .enqueue_complete_retry(context.id, history_entry),
                                        }
                                        .map_err(|error| error.to_string())
                                    });
                                match queued {
                                    Ok(completion) => {
                                        let tx = self.tx.clone();
                                        let is_retry = context.kind == HistoryRequestKind::Retry;
                                        thread::spawn(move || {
                                            let observed = if is_retry {
                                                completion.recv().map_err(|error| error.to_string())
                                            } else {
                                                completion
                                                    .recv_timeout(Duration::from_secs(2))
                                                    .map_err(|error| error.to_string())
                                            };
                                            let result = observed.and_then(|result| {
                                                result
                                                    .map(|_| ())
                                                    .map_err(|error| error.to_string())
                                            });
                                            let _ = tx.send(AppEvent::HistoryCompletionObserved {
                                                history_id: context.id,
                                                // Receiving a terminal reply, including an
                                                // operation error, means the worker's
                                                // failure-safe guard consumed the lease. A
                                                // disconnected worker cannot retain one.
                                                retry_lease_released: is_retry,
                                                result,
                                            });
                                        });
                                    }
                                    Err(error) => {
                                        self.history_error = Some(format!(
                                            "History completion was not queued: {error}"
                                        ));
                                        if context.kind == HistoryRequestKind::Retry
                                            && let Some(store) = self.history_store.clone()
                                        {
                                            let tx = self.tx.clone();
                                            thread::spawn(move || {
                                                let (retry_lease_released, result) =
                                                    release_history_retry_with_bounded_retry(
                                                        &store, context.id,
                                                    );
                                                let _ = tx.send(
                                                    AppEvent::HistoryRetryTerminalPersisted {
                                                        history_id: context.id,
                                                        retry_lease_released,
                                                        result,
                                                    },
                                                );
                                            });
                                        }
                                    }
                                }
                                self.finish_transcription_after_history(pending);
                            } else if self.config.output.auto_insert_transcript {
                                if let Err(err) = self.session_coordinator.begin_output(session_id)
                                {
                                    self.fail_dictation_session(
                                        session_id,
                                        format!("Could not begin final output: {err}"),
                                    );
                                    self.cleanup_after_job(source, session_id, request_id);
                                    continue;
                                }
                                self.status = TranscriptionStatus::Transcribing;
                                self.status_message = format!(
                                    "{completion_message}. Verifying the original target before paste."
                                );
                                self.pending_output = Some(PendingOutput {
                                    session_id,
                                    history_id: None,
                                    transcript: self.transcript.clone(),
                                    completion_message,
                                    config: self.config.clone(),
                                    latency,
                                });
                            } else {
                                self.status = TranscriptionStatus::Idle;
                                self.status_message = completion_message;
                                if let Some(latency) = latency.as_ref() {
                                    self.record_session_diagnostic(
                                        session_id,
                                        latency,
                                        DiagnosticSessionOutcome::Completed,
                                        None,
                                    );
                                }
                                self.latest_latency = latency;
                                let _ = self.session_coordinator.complete(session_id);
                                self.finish_overlay_success(session_id);
                            }
                        }
                        RecordingSource::Playground => {
                            self.apply_playground_result(*result);
                            if all_requests_completed {
                                if self
                                    .session_coordinator
                                    .has_failed_requests(session_id)
                                    .unwrap_or(false)
                                {
                                    let _ = self.session_coordinator.fail(session_id);
                                } else {
                                    let _ = self.session_coordinator.complete(session_id);
                                }
                            }
                        }
                    }
                    self.cleanup_after_job(source, session_id, request_id);
                }
                AppEvent::PlaygroundModelStarted {
                    session_id,
                    request_id,
                    model_id,
                } => {
                    if !self.transcription_event_is_current(
                        RecordingSource::Playground,
                        session_id,
                        request_id,
                    ) || self.expected_playground_model_id(session_id, request_id)
                        != Some(model_id.as_str())
                    {
                        continue;
                    }
                    if let Some(card) = self
                        .playground_cards
                        .iter_mut()
                        .find(|card| card.descriptor.id.as_str() == model_id)
                    {
                        card.status = ModelRuntimeStatus::Running;
                    }
                }
                AppEvent::TranscriptionFailed {
                    source,
                    session_id,
                    request_id,
                    model_id,
                    message,
                    latency,
                } => {
                    if !self.transcription_event_is_current(source, session_id, request_id) {
                        self.cleanup_after_job(source, session_id, request_id);
                        continue;
                    }
                    if source == RecordingSource::Playground
                        && self.expected_playground_model_id(session_id, request_id)
                            != Some(model_id.as_str())
                    {
                        self.reject_transcription_correlation(
                            source,
                            session_id,
                            request_id,
                            "Transcription service returned the wrong model for a Playground request",
                        );
                        let _ = self.session_coordinator.fail(session_id);
                        self.cleanup_after_job(source, session_id, request_id);
                        continue;
                    }
                    let failed_model_id = ModelId::new(model_id.as_str());
                    let all_requests_completed = match self.session_coordinator.fail_request(
                        session_id,
                        request_id,
                        &failed_model_id,
                    ) {
                        Ok(all_completed) => all_completed,
                        Err(err) => {
                            self.fail_correlated_history_request(
                                session_id,
                                request_id,
                                format!("Rejected transcription failure: {err}"),
                            );
                            self.reject_transcription_correlation(
                                source,
                                session_id,
                                request_id,
                                &format!("Rejected transcription failure: {err}"),
                            );
                            let _ = self.session_coordinator.fail(session_id);
                            self.cleanup_after_job(source, session_id, request_id);
                            continue;
                        }
                    };
                    if let Some(mut latency) = latency {
                        latency.ui_result_at = Some(Instant::now());
                        self.record_session_diagnostic(
                            session_id,
                            &latency,
                            DiagnosticSessionOutcome::Failed,
                            Some(DiagnosticFailureStage::Transcription),
                        );
                        self.latest_latency = Some(latency);
                    }
                    match source {
                        RecordingSource::Transcribe => {
                            if let Some(context) =
                                self.history_requests.remove(&(session_id, request_id))
                            {
                                self.fail_history_context(context, "Transcription failed");
                            }
                            self.fail_dictation_session(session_id, &message);
                        }
                        RecordingSource::Playground => {
                            self.status = TranscriptionStatus::Error;
                            let mut output_changed = false;
                            if let Some(card) = self
                                .playground_cards
                                .iter_mut()
                                .find(|card| card.descriptor.id.as_str() == model_id)
                            {
                                card.status = ModelRuntimeStatus::Error(message.clone());
                                output_changed = !card.transcript.is_empty();
                                card.transcript.clear();
                                card.latency_ms = None;
                            }
                            if output_changed {
                                self.mark_comparison_output_changed(&model_id);
                            }
                            self.status_message = message;
                            if all_requests_completed {
                                let _ = self.session_coordinator.fail(session_id);
                            }
                        }
                    }
                    self.cleanup_after_job(source, session_id, request_id);
                }
                AppEvent::ModelDownloadProgress {
                    job_id,
                    model_id,
                    progress,
                } => {
                    if self
                        .artifact_installations
                        .get(&model_id)
                        .is_none_or(|(active_job, _)| *active_job != job_id)
                    {
                        continue;
                    }
                    self.remote_catalog.invalidate_local_models();
                    let stage_label = match progress.stage {
                        InstallStage::Downloading => "Downloading",
                        InstallStage::Verifying => "Verifying checksum",
                        InstallStage::Extracting => "Extracting verified runtime files",
                        InstallStage::HealthChecking => {
                            "Running native health/load/transcribe smoke test"
                        }
                        InstallStage::Activating => "Activating verified artifacts",
                    };
                    self.model_downloads.insert(
                        model_id.clone(),
                        if progress.stage == InstallStage::Downloading {
                            ModelInstallStatus::Downloading {
                                downloaded_bytes: progress.completed_bytes,
                                total_bytes: Some(progress.total_bytes),
                                bytes_per_second: progress.bytes_per_second,
                            }
                        } else {
                            ModelInstallStatus::InstallingRuntime
                        },
                    );
                    self.status_message = format!("{stage_label} for {model_id}...");
                }
                AppEvent::VerifiedInstallDone {
                    job_id,
                    model_id,
                    result,
                } => {
                    let mut result = *result;
                    if self
                        .artifact_installations
                        .get(&model_id)
                        .is_none_or(|(active_job, _)| *active_job != job_id)
                    {
                        let manifest_rollback = result.manifest.rollback().err();
                        let model_rollback = result.model.rollback().err();
                        let runtime_rollback =
                            result.runtime.and_then(|runtime| runtime.rollback().err());
                        let journal_clear = if manifest_rollback.is_none()
                            && model_rollback.is_none()
                            && runtime_rollback.is_none()
                        {
                            result.journal.clear().err()
                        } else {
                            None
                        };
                        if manifest_rollback.is_some()
                            || model_rollback.is_some()
                            || runtime_rollback.is_some()
                            || journal_clear.is_some()
                        {
                            let message = "A stale installation result could not be rolled back; startup recovery is required."
                                .to_owned();
                            self.artifact_recovery_error = Some(message.clone());
                            self.status = TranscriptionStatus::Error;
                            self.status_message = message;
                        }
                        continue;
                    }
                    self.remote_catalog.invalidate_local_models();
                    self.artifact_installations.remove(&model_id);
                    let previous_config = self.config.clone();
                    self.model_downloads
                        .insert(model_id.clone(), ModelInstallStatus::Installed);
                    let remote_install = result.remote_install.take();
                    if let Some(remote_install) = remote_install.as_ref() {
                        self.config
                            .general
                            .managed_remote_models
                            .insert(model_id.clone(), remote_install.clone());
                    }
                    if let Some(model) = config::configured_models(&self.config)
                        .into_iter()
                        .find(|model| model.id == model_id)
                    {
                        let active_model_is_runnable =
                            self.selected_model().is_some_and(|active| {
                                runtime_status_for_model(&self.config, &active)
                                    == ModelRuntimeStatus::Ready
                            });
                        if remote_install.is_none() {
                            self.config
                                .general
                                .managed_models
                                .insert(model_id.clone(), {
                                    let mut install = config::ManagedModelInstall::app_managed(
                                        result.model.destination().to_path_buf(),
                                        "verified-manifest-download",
                                    );
                                    install.sha256 = Some(result.model_sha256.clone());
                                    install
                                });
                        }
                        set_model_selected(&mut self.config, &model_id, true);
                        if should_activate_installed_model(active_model_is_runnable) {
                            self.config.general.selected_default_model = model_id.clone();
                            compatibility_bridge::record_selected_provider(
                                &mut self.config,
                                &model,
                            );
                        }
                    }
                    if let Some(entrypoint) = result.runtime_entrypoint.as_ref() {
                        let mut install = config::ManagedRuntimeInstall::app_managed(
                            entrypoint.clone(),
                            "verified-pinned-runtime-package",
                        );
                        install.version = result.runtime_version.clone();
                        install.sha256 = result.runtime_archive_sha256.clone();
                        if let Some(package_id) = result.runtime_package_id.as_ref() {
                            install.unknown.insert(
                                "package_id".to_owned(),
                                serde_json::Value::String(package_id.clone()),
                            );
                        }
                        self.config
                            .general
                            .managed_runtimes
                            .insert(result.runtime_id.clone(), install);
                    }
                    config::normalize_config(&mut self.config);
                    let journal_preparation =
                        config::settings::artifact_config_fingerprint(&self.config)
                            .map_err(|error| error.to_string())
                            .and_then(|fingerprint| {
                                result
                                    .journal
                                    .prepare_config_commit(fingerprint)
                                    .map_err(|error| error.to_string())
                            });
                    if let Err(error) = journal_preparation {
                        self.config = previous_config;
                        let manifest_rollback = result.manifest.rollback().err();
                        let model_rollback = result.model.rollback().err();
                        let runtime_rollback =
                            result.runtime.and_then(|runtime| runtime.rollback().err());
                        let recovery_required = manifest_rollback.is_some()
                            || model_rollback.is_some()
                            || runtime_rollback.is_some();
                        let journal_clear = if recovery_required {
                            None
                        } else {
                            result.journal.clear().err()
                        };
                        let recovery_required = recovery_required || journal_clear.is_some();
                        let message = format!(
                            "Could not prepare the durable settings commit: {error}{}{}{}",
                            manifest_rollback
                                .as_ref()
                                .map(|error| format!(
                                    ". Installed-model manifest rollback failed: {error}"
                                ))
                                .unwrap_or_default(),
                            model_rollback
                                .as_ref()
                                .map(|error| format!(". Model rollback failed: {error}"))
                                .unwrap_or_default(),
                            runtime_rollback
                                .as_ref()
                                .map(|error| format!(". Runtime rollback failed: {error}"))
                                .unwrap_or_default(),
                        ) + &journal_clear
                            .as_ref()
                            .map(|error| format!(". Activation journal cleanup failed: {error}"))
                            .unwrap_or_default();
                        if recovery_required {
                            self.artifact_recovery_error = Some(message.clone());
                        }
                        self.model_downloads
                            .insert(model_id, ModelInstallStatus::Error(message.clone()));
                        self.status = TranscriptionStatus::Error;
                        self.status_message = message;
                        continue;
                    }
                    let persistence =
                        config::save_config(&self.config).map_err(|error| error.to_string());
                    if let Err(message) = persistence {
                        let message = format!(
                            "Could not confirm the verified installation settings commit: {message}. Artifacts and the activation journal were retained unchanged; restart Scribe to reconcile against the durable settings fingerprint."
                        );
                        self.artifact_recovery_error = Some(message.clone());
                        self.model_downloads
                            .insert(model_id, ModelInstallStatus::Error(message.clone()));
                        self.status = TranscriptionStatus::Error;
                        self.status_message = message;
                        continue;
                    }
                    if let Err(error) = result.journal.mark(ActivationPhase::ConfigPersisted) {
                        let message = format!(
                            "Could not advance the installation journal after settings persistence: {error}. Artifacts and the journal were retained unchanged; restart Scribe to reconcile against the durable settings fingerprint."
                        );
                        self.artifact_recovery_error = Some(message.clone());
                        self.model_downloads
                            .insert(model_id, ModelInstallStatus::Error(message.clone()));
                        self.status = TranscriptionStatus::Error;
                        self.status_message = message;
                        continue;
                    }
                    let model_cleanup = result.model.commit().err();
                    let manifest_cleanup = result.manifest.commit().err();
                    let runtime_cleanup = result.runtime.and_then(|runtime| {
                        runtime
                            .commit_with_previous_policy(result.retain_runtime_as_previous)
                            .err()
                    });
                    let journal_cleanup = if model_cleanup.is_none()
                        && manifest_cleanup.is_none()
                        && runtime_cleanup.is_none()
                    {
                        result.journal.clear().err()
                    } else {
                        None
                    };
                    let cleanup_requires_recovery = model_cleanup.is_some()
                        || manifest_cleanup.is_some()
                        || runtime_cleanup.is_some()
                        || journal_cleanup.is_some();
                    if let Some(store) = self.settings_store.as_mut() {
                        store.mark_current_persisted();
                    }
                    self.transcription_service =
                        self.transcription_service.with_config(self.config.clone());
                    self.refresh_playground_cards_from_config();
                    let message = format!(
                        "Model installed and smoke-tested (health {} ms, load {} ms, decode {} ms, reload {} ms, {}).{}{}{}{}",
                        result.smoke.health_duration_ms,
                        result.smoke.load_duration_ms,
                        result.smoke.decode_duration_ms,
                        result.smoke.reload_duration_ms,
                        result.smoke.resolved_acceleration.resolved.label(),
                        model_cleanup
                            .map(|error| format!(" Model cleanup warning: {error}."))
                            .unwrap_or_default(),
                        manifest_cleanup
                            .map(|error| format!(
                                " Installed-model manifest cleanup warning: {error}."
                            ))
                            .unwrap_or_default(),
                        runtime_cleanup
                            .map(|error| format!(" Runtime backup warning: {error}."))
                            .unwrap_or_default(),
                        journal_cleanup
                            .map(|error| format!(" Journal cleanup warning: {error}."))
                            .unwrap_or_default(),
                    );
                    if cleanup_requires_recovery {
                        let message = format!(
                            "{message} Artifact cleanup is incomplete; restart Scribe to reconcile the retained transaction before another install, update, repair, removal, or runtime switch."
                        );
                        self.artifact_recovery_error = Some(message.clone());
                        self.status = TranscriptionStatus::Error;
                        self.status_message = message;
                    } else {
                        self.status = TranscriptionStatus::Idle;
                        self.status_message = message;
                    }
                }
                AppEvent::VerifiedInstallFailed {
                    job_id,
                    model_id,
                    message,
                    recovery_required,
                } => {
                    if self
                        .artifact_installations
                        .get(&model_id)
                        .is_none_or(|(active_job, _)| *active_job != job_id)
                    {
                        continue;
                    }
                    self.remote_catalog.invalidate_local_models();
                    self.artifact_installations.remove(&model_id);
                    if recovery_required {
                        self.artifact_recovery_error = Some(message.clone());
                    }
                    self.model_downloads
                        .insert(model_id, ModelInstallStatus::Error(message.clone()));
                    self.status = TranscriptionStatus::Error;
                    self.status_message = format!("Installation failed: {message}");
                }
                AppEvent::RuntimeInstallDone {
                    runtime_id,
                    runtime_label,
                    replacement,
                    source_label,
                } => {
                    self.remote_catalog.invalidate_local_models();
                    let installed_path = replacement.installed_path.clone();
                    let new_runtime = managed_runtime_install_record(installed_path, source_label);
                    let job = self.runtime_jobs.remove(&runtime_id).unwrap_or_default();
                    let job = match persist_runtime_install(
                        &mut self.config,
                        &runtime_id,
                        new_runtime.clone(),
                        job,
                        |config| config::save_config(config).map_err(|err| err.to_string()),
                    ) {
                        RuntimePersistenceTransition::Persisted(job) => job,
                        RuntimePersistenceTransition::Failed { job, message } => {
                            let rollback_message = match replacement.rollback() {
                                Ok(()) => message,
                                Err(rollback_err) => format!("{message}. {rollback_err}"),
                            };
                            for model_id in job.download_model_ids {
                                self.model_downloads.insert(
                                    model_id,
                                    ModelInstallStatus::Error(rollback_message.clone()),
                                );
                            }
                            for model_id in job.repair_model_ids {
                                self.model_downloads.insert(
                                    model_id,
                                    ModelInstallStatus::RuntimeError(rollback_message.clone()),
                                );
                            }
                            self.status = TranscriptionStatus::Error;
                            self.status_message = rollback_message;
                            continue;
                        }
                    };
                    if let Some(store) = self.settings_store.as_mut() {
                        store.mark_current_persisted();
                    }
                    let cleanup_warning = replacement.commit().err();
                    if self.config_path.is_none() {
                        self.config_path = config::config_file_path().ok();
                    }
                    debug_assert!(runtime_metadata_matches(
                        &self.config,
                        &runtime_id,
                        &new_runtime,
                    ));
                    self.refresh_playground_runtime_statuses();
                    for model_id in job.repair_model_ids {
                        self.model_downloads.remove(&model_id);
                    }
                    for model_id in job.download_model_ids {
                        if let Some(model) = config::configured_models(&self.config)
                            .into_iter()
                            .find(|model| model.id == model_id)
                        {
                            self.start_model_download_only(&model);
                        }
                    }
                    self.status = TranscriptionStatus::Idle;
                    self.status_message = cleanup_warning.map_or_else(
                        || format!("{runtime_label} runtime is ready."),
                        |warning| {
                            format!(
                                "{runtime_label} runtime is ready. Old runtime backup cleanup warning: {warning}"
                            )
                        },
                    );
                }
                AppEvent::RuntimeInstallFailed {
                    runtime_id,
                    message,
                } => self.fail_runtime_job(&runtime_id, message),
            }
        }
        self.rebuild_model_inventory_projection();
    }

    fn cleanup_after_job(
        &mut self,
        source: RecordingSource,
        session_id: SessionId,
        request_id: RequestId,
    ) {
        match source {
            RecordingSource::Transcribe => {
                let owns_terminal = self
                    .session_coordinator
                    .last_terminal()
                    .is_some_and(|terminal| terminal.session_id == session_id);
                if self.session_coordinator.active_session_id().is_some() || !owns_terminal {
                    return;
                }
                self.status = if self.status == TranscriptionStatus::Error {
                    TranscriptionStatus::Error
                } else {
                    TranscriptionStatus::Idle
                };
            }
            RecordingSource::Playground => {
                let mut completed = false;
                let mut remaining = None;
                if let Some(run) = self.playground_runs.get_mut(&session_id)
                    && run.pending_requests.remove(&request_id).is_some()
                {
                    remaining = Some(run.pending_requests.len());
                    completed = run.pending_requests.is_empty();
                }

                if completed {
                    self.playground_runs.remove(&session_id);
                }

                let owns_active = self.session_coordinator.active_session_id() == Some(session_id);
                let owns_terminal = self
                    .session_coordinator
                    .last_terminal()
                    .is_some_and(|terminal| terminal.session_id == session_id);

                if (owns_active || owns_terminal)
                    && let Some(remaining) = remaining
                {
                    self.playground_pending = remaining;
                }

                if (owns_active || owns_terminal)
                    && completed
                    && self.status != TranscriptionStatus::Error
                {
                    self.status = TranscriptionStatus::Idle;
                    self.status_message = "Model playground finished".to_owned();
                }
            }
        }
    }

    fn dispatch_default_transcription(
        &mut self,
        session_id: SessionId,
        audio: Arc<PreparedAudio>,
        mut latency: LatencyTrace,
    ) {
        let Some(model) = self.selected_model() else {
            self.fail_dictation_session(session_id, "No default model selected");
            return;
        };

        let request_id = match self
            .session_coordinator
            .start_request(session_id, ModelId::new(model.id.as_str()))
        {
            Ok(request_id) => request_id,
            Err(err) => {
                self.fail_dictation_session(
                    session_id,
                    format!("Could not begin transcription request: {err}"),
                );
                return;
            }
        };
        let history_plan = if self.config.history.mode.stores_transcripts() {
            self.history_store.clone().map(|store| {
                let source_app = self
                    .config
                    .history
                    .store_application_identity
                    .then(|| {
                        self.captured_targets
                            .get(&session_id)
                            .and_then(overlay::captured_target_application_identity)
                    })
                    .flatten();
                HistoryCapturePlan {
                    store,
                    entry: NewHistoryEntry {
                        raw_text: String::new(),
                        model_id: model.id.clone(),
                        source_app,
                        metrics: HistoryMetrics {
                            audio_duration_ms: u64::try_from(audio.duration_ms()).ok(),
                            ..HistoryMetrics::default()
                        },
                    },
                    retain_audio: self.config.history.mode.stores_audio(),
                }
            })
        } else {
            None
        };
        if self.config.history.mode.stores_transcripts() && history_plan.is_none() {
            self.history_error
                .get_or_insert_with(|| "Local history is unavailable".to_owned());
        }
        let service = self.transcription_service.with_config(self.config.clone());
        let task = match service.begin_transcription_task() {
            Ok(task) => task,
            Err(err) => {
                self.fail_dictation_session(
                    session_id,
                    format!("Could not dispatch transcription: {err}"),
                );
                return;
            }
        };
        let tx = self.tx.clone();

        if let Some(plan) = history_plan {
            let history_id = HistoryStore::reserve_id();
            let retained_audio = plan.retain_audio.then(|| Arc::clone(&audio));
            match plan
                .store
                .enqueue_pending(history_id, plan.entry, retained_audio)
            {
                Ok(_completion) => {
                    self.history_requests.insert(
                        (session_id, request_id),
                        HistoryRequestContext {
                            id: history_id,
                            kind: HistoryRequestKind::Dictation,
                        },
                    );
                }
                Err(error) => {
                    self.history_error = Some(format!("History was not queued: {error}"));
                }
            }
        }

        thread::spawn(move || {
            latency.transcription_dispatched_at = Some(Instant::now());
            let mut request =
                TranscriptionRequest::new(session_id, request_id, audio, model.id.clone());
            request.model_path = model.local_path.clone();
            request.options = TranscriptionOptions::default();
            let result = service.transcribe_task(request, task);
            let completed_at = Instant::now();
            latency.transcription_job_completed_at = Some(completed_at);

            match result {
                Ok(result) => {
                    let _ = tx.send(AppEvent::TranscriptionDone {
                        source: RecordingSource::Transcribe,
                        session_id,
                        request_id,
                        result: Box::new(result),
                        latency: Some(latency),
                    });
                }
                Err(err) => {
                    let _ = tx.send(AppEvent::TranscriptionFailed {
                        source: RecordingSource::Transcribe,
                        session_id,
                        request_id,
                        model_id: model.id,
                        message: err.to_string(),
                        latency: Some(latency),
                    });
                }
            }
        });
    }

    fn dispatch_playground_transcriptions(
        &mut self,
        session_id: SessionId,
        audio: Arc<PreparedAudio>,
    ) {
        let models = self.playground_selected_models();
        if let Some(message) = self.playground_run_block_reason() {
            let _ = self.session_coordinator.fail(session_id);
            self.playground_pending = 0;
            self.status = TranscriptionStatus::Error;
            self.status_message = message;
            return;
        }

        if self.session_coordinator.active_session_id() != Some(session_id)
            || self.session_coordinator.active_purpose() != Some(SessionPurpose::Comparison)
        {
            return;
        }

        self.playground_pending = models.len();
        let audio_duration_ms = Some(audio.duration_ms());
        let service = self.transcription_service.with_config(self.config.clone());

        let mut requests = Vec::with_capacity(models.len());
        for model in models {
            let request_id = match self
                .session_coordinator
                .start_request(session_id, ModelId::new(model.id.as_str()))
            {
                Ok(request_id) => request_id,
                Err(err) => {
                    let _ = self.session_coordinator.fail(session_id);
                    self.playground_pending = 0;
                    self.status = TranscriptionStatus::Error;
                    self.status_message = format!("Could not start model comparison: {err}");
                    return;
                }
            };
            requests.push((request_id, model));
        }
        self.playground_runs.insert(
            session_id,
            PlaygroundRunState {
                pending_requests: requests
                    .iter()
                    .map(|(request_id, model)| (*request_id, model.id.clone()))
                    .collect(),
                _audio: Arc::clone(&audio),
            },
        );

        let mut changed_outputs = Vec::new();
        for (_, model) in &requests {
            if let Some(card) = self
                .playground_cards
                .iter_mut()
                .find(|card| card.descriptor.id.as_str() == model.id)
            {
                card.status = ModelRuntimeStatus::Ready;
                if !card.transcript.is_empty() {
                    changed_outputs.push(model.id.clone());
                }
                card.transcript.clear();
                card.latency_ms = None;
                card.audio_duration_ms = audio_duration_ms;
                card.peak_ram_mb = None;
                card.peak_vram_mb = None;
            }
        }
        for model_id in changed_outputs {
            self.mark_comparison_output_changed(&model_id);
        }

        let tx = self.tx.clone();
        thread::spawn(move || {
            for (request_id, model) in requests {
                let _ = tx.send(AppEvent::PlaygroundModelStarted {
                    session_id,
                    request_id,
                    model_id: model.id.clone(),
                });
                let task = match service.begin_transcription_task() {
                    Ok(task) => task,
                    Err(err) => {
                        let _ = tx.send(AppEvent::TranscriptionFailed {
                            source: RecordingSource::Playground,
                            session_id,
                            request_id,
                            model_id: model.id,
                            message: err.to_string(),
                            latency: None,
                        });
                        continue;
                    }
                };
                let mut request = TranscriptionRequest::new(
                    session_id,
                    request_id,
                    Arc::clone(&audio),
                    model.id.clone(),
                );
                request.model_path = model.local_path.clone();
                request.options = TranscriptionOptions::default();
                match service.transcribe_task(request, task) {
                    Ok(result) => {
                        let _ = tx.send(AppEvent::TranscriptionDone {
                            source: RecordingSource::Playground,
                            session_id,
                            request_id,
                            result: Box::new(result),
                            latency: None,
                        });
                    }
                    Err(err) => {
                        let _ = tx.send(AppEvent::TranscriptionFailed {
                            source: RecordingSource::Playground,
                            session_id,
                            request_id,
                            model_id: model.id,
                            message: err.to_string(),
                            latency: None,
                        });
                    }
                }
            }
        });
    }

    fn reset_playground_for_run(&mut self) {
        self.reset_comparison_output_projection();
        self.playground_cards = cards_for_models(
            &self.config,
            &self.transcription_service,
            self.playground_selected_models(),
        );
        for card in &mut self.playground_cards {
            card.status = runtime_status_for_id(&self.config, card.descriptor.id.as_str());
            card.transcript.clear();
            card.latency_ms = None;
            card.audio_duration_ms = None;
            card.peak_ram_mb = None;
            card.peak_vram_mb = None;
        }
        if !self.playground_reference_user_edited {
            self.playground_reference_transcript.clear();
        }
        self.playground_pending = 0;
    }

    fn apply_playground_result(&mut self, result: TranscriptionOutcome) {
        let is_active_model =
            result.model_id.as_str() == self.config.general.selected_default_model;
        let duration_ms = result.processing_duration_ms;
        let transcript = result.transcript.text;
        let mut output_changed = false;
        if let Some(card) = self
            .playground_cards
            .iter_mut()
            .find(|card| card.descriptor.id == result.model_id)
        {
            card.status = ModelRuntimeStatus::Ready;
            output_changed = card.transcript != transcript;
            card.transcript = transcript.clone();
            card.latency_ms = duration_ms;
        }
        if output_changed {
            self.mark_comparison_output_changed(result.model_id.as_str());
        }
        if is_active_model && !self.playground_reference_user_edited {
            self.playground_reference_transcript = transcript;
        }
    }

    fn clear_playground_results(&mut self, clear_reference: bool) {
        self.reset_comparison_output_projection();
        for card in &mut self.playground_cards {
            card.transcript.clear();
            card.latency_ms = None;
            card.audio_duration_ms = None;
            card.peak_ram_mb = None;
            card.peak_vram_mb = None;
            card.status = runtime_status_for_id(&self.config, card.descriptor.id.as_str());
        }
        if clear_reference {
            self.playground_reference_transcript.clear();
            self.playground_reference_user_edited = false;
        }
    }

    fn playground_selector_busy(&self) -> bool {
        self.capture_is_active() || self.playground_pending > 0
    }

    fn open_playground_selector(&mut self, opener_id: Option<egui::Id>) {
        if !self.playground_selector_busy() {
            self.playground_selector_draft =
                Some(self.config.general.playground_selected_models.clone());
            self.playground_selector_return_focus = opener_id;
            self.playground_selector_needs_initial_focus = true;
        }
    }

    fn close_playground_selector(&mut self, ctx: &egui::Context) {
        self.playground_selector_draft = None;
        self.playground_selector_needs_initial_focus = false;
        if let Some(opener_id) = self.playground_selector_return_focus.take() {
            ctx.memory_mut(|memory| memory.request_focus(opener_id));
        }
    }

    fn apply_playground_selector(&mut self, ctx: &egui::Context) {
        if self.playground_selector_busy() {
            return;
        }
        if let Some(draft) = self.playground_selector_draft.take() {
            apply_playground_selector_draft(&mut self.config, draft);
            self.save_config();
            self.status_message = "Playground models updated".to_owned();
            self.playground_selector_needs_initial_focus = false;
            let return_focus = self
                .playground_selector_header_focus
                .or_else(|| self.playground_selector_return_focus.take());
            self.playground_selector_return_focus = None;
            if let Some(return_focus) = return_focus {
                ctx.memory_mut(|memory| memory.request_focus(return_focus));
            }
        }
    }

    fn playground_run_block_reason(&self) -> Option<String> {
        let models = self.playground_selected_models();
        if models.is_empty() {
            return Some(
                if self
                    .comparison_run_model_ids
                    .as_ref()
                    .is_some_and(Vec::is_empty)
                    || (self.comparison_run_model_ids.is_none()
                        && self.config.general.playground_selected_models.is_empty())
                {
                    "Choose models to test before starting a test recording.".to_owned()
                } else {
                    "Install the selected Playground models before starting a test recording."
                        .to_owned()
                },
            );
        }
        models
            .iter()
            .find(|model| runtime_status_for_model(&self.config, model) != ModelRuntimeStatus::Ready)
            .map(|model| {
                format!(
                    "{} is not ready. Repair or install its runtime from Models before running the Playground.",
                    model.name
                )
            })
    }

    fn active_playground_output(&self) -> Option<(String, String)> {
        self.playground_cards
            .iter()
            .find(|card| {
                card.descriptor.id.as_str() == self.config.general.selected_default_model
                    && !card.transcript.trim().is_empty()
            })
            .map(|card| {
                (
                    card.descriptor.display_name.to_owned(),
                    card.transcript.clone(),
                )
            })
    }

    fn apply_active_playground_output_as_reference(&mut self) -> bool {
        let Some((_model_name, transcript)) = self.active_playground_output() else {
            return false;
        };
        self.playground_reference_transcript = transcript;
        self.playground_reference_user_edited = false;
        true
    }

    fn playground_benchmark_results(&self) -> Vec<BenchmarkModelResult> {
        let reference = self.playground_reference_transcript.trim();
        if reference.is_empty() {
            return Vec::new();
        }

        let inputs = self
            .playground_cards
            .iter()
            .filter(|card| !card.transcript.trim().is_empty())
            .map(|card| BenchmarkModelInput {
                model_id: card.descriptor.id.as_str().to_owned(),
                model_name: card.descriptor.display_name.to_owned(),
                predicted_transcript: card.transcript.clone(),
                reference_transcript: reference.to_owned(),
                elapsed_ms: card.latency_ms,
                audio_duration_ms: card.audio_duration_ms,
                peak_ram_mb: card.peak_ram_mb,
                peak_vram_mb: card.peak_vram_mb,
            })
            .collect::<Vec<_>>();

        benchmark::score_benchmark_models(inputs)
    }

    fn apply_hotkey(&mut self) {
        match self.hotkey_service.register(&self.hotkey_input) {
            Ok(()) => {
                self.capturing_hotkey = false;
                self.config.recording.hotkey = self.hotkey_input.clone();
                self.save_config();
                self.status_message = format!("Registered hotkey {}", self.config.recording.hotkey);
            }
            Err(err) => {
                self.status = TranscriptionStatus::Error;
                self.status_message = format!("Failed to register hotkey: {err}");
            }
        }
    }

    fn apply_theme(&self, ctx: &egui::Context, frame: &eframe::Frame) {
        ctx.set_visuals(stitch_visuals(resolve_theme_mode(
            self.config.general.theme_mode,
            frame.info().system_theme,
        )));
    }

    fn poll_hotkey_capture(&mut self, ctx: &egui::Context) {
        if !self.capturing_hotkey {
            return;
        }

        if let Some(spec) = ctx.input(captured_hotkey_spec) {
            self.hotkey_input = spec;
            self.apply_hotkey();
        }
    }

    fn refresh_audio_devices(&mut self) {
        match audio::input_device_names() {
            Ok(devices) => {
                self.audio_devices = devices;
                self.status_message = "Microphone list refreshed".to_owned();
            }
            Err(err) => {
                self.status = TranscriptionStatus::Error;
                self.status_message = format!("Could not list microphones: {err}");
            }
        }
    }

    fn select_model_as_default(&mut self, model: &SttModelInfo) -> bool {
        if let Some(reason) = self.artifact_mutation_block_reason() {
            self.status_message = reason;
            return false;
        }
        if let Err(error) = self.transcription_service.unload_runtime() {
            self.status = TranscriptionStatus::Error;
            self.status_message =
                format!("Could not unload the warm model before switching: {error}");
            return false;
        }
        self.config.general.selected_default_model = model.id.clone();
        compatibility_bridge::record_selected_provider(&mut self.config, model);
        self.save_config();
        self.remote_catalog.invalidate_local_models();
        true
    }

    fn effective_install_status(&self, model: &SttModelInfo) -> ModelInstallStatus {
        self.model_downloads
            .get(&model.id)
            .cloned()
            .unwrap_or_else(|| model.install_status.clone())
    }

    fn runtime_consumer_activity(&self, runtime_id: &str) -> RuntimeConsumerActivity {
        RuntimeConsumerActivity {
            recording: self.capture_is_active(),
            transcribing: self.effective_status() == TranscriptionStatus::Transcribing,
            playground_jobs: self.playground_pending > 0,
            model_download: model_download_uses_runtime(
                &self.config,
                &self.model_downloads,
                runtime_id,
            ),
        }
    }

    fn start_model_download(&mut self, model: &SttModelInfo) {
        if self.artifact_installations.contains_key(&model.id)
            || self.runtime_jobs.values().any(|job| {
                job.download_model_ids
                    .iter()
                    .any(|queued_model_id| queued_model_id == &model.id)
            })
        {
            self.status_message = format!("{} is already being installed.", model.name);
            return;
        }
        if self
            .transcription_service
            .installation_binding(&ModelId::new(&model.id))
            .is_ok()
        {
            self.start_model_download_only(model);
            return;
        }
        let Some(provider) = compatibility_bridge::provider_for_model(model) else {
            self.fail_model_install(&model.id, "Model provider is not available.".to_owned());
            return;
        };

        if !runtime_needs_preparation(&provider.runtime_status(&self.config)) {
            self.start_model_download_only(model);
            return;
        }

        self.request_runtime_install(model, RuntimeJobIntent::DownloadModel(model.id.clone()));
    }

    fn start_model_download_only(&mut self, model: &SttModelInfo) {
        self.start_model_download_with_runtime_policy(model, false);
    }

    fn start_model_download_with_runtime_policy(
        &mut self,
        model: &SttModelInfo,
        force_runtime_package: bool,
    ) {
        if let Some(reason) = self.artifact_mutation_block_reason() {
            self.fail_model_install(&model.id, reason);
            return;
        }
        if model.download_model.is_none() {
            self.status = TranscriptionStatus::Error;
            self.status_message = format!("{} does not have a supported download.", model.name);
            return;
        }

        if !supports_managed_install(model) {
            self.status = TranscriptionStatus::Error;
            self.status_message = format!(
                "Managed installer for {} is not available in this build.",
                model.name
            );
            return;
        }

        if config::downloaded_model_path(&self.config, model).is_none() {
            self.status = TranscriptionStatus::Error;
            self.status_message = "No model storage directory is configured.".to_owned();
            return;
        }

        let model_id = ModelId::new(&model.id);
        if let Some(reason) = normalized_model_install_space_error(&self.config, &model_id) {
            self.fail_model_install(&model.id, reason);
            return;
        }
        let binding = match self.transcription_service.installation_binding(&model_id) {
            Ok(binding) => binding,
            Err(error) => {
                self.fail_model_install(
                    &model.id,
                    format!("This model is not eligible for verified installation: {error}"),
                );
                return;
            }
        };
        if let Err(error) = self.transcription_service.unload_runtime() {
            self.fail_model_install(
                &model.id,
                format!("Could not release the active speech artifact before install: {error}"),
            );
            return;
        }
        let expected_total_bytes = model_download_total_bytes(model);
        self.model_downloads.insert(
            model.id.clone(),
            ModelInstallStatus::Downloading {
                downloaded_bytes: 0,
                total_bytes: expected_total_bytes,
                bytes_per_second: None,
            },
        );
        self.remote_catalog.invalidate_local_models();
        self.status = TranscriptionStatus::Idle;
        self.status_message = format!("Downloading {}...", model.name);

        let tx = self.tx.clone();
        let model_name = model.id.clone();
        let config = self.config.clone();
        let service = self.transcription_service.with_config(config.clone());
        let cancellation = InstallCancellation::default();
        let thread_cancellation = cancellation.clone();
        let job_id = INSTALL_JOB_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        self.artifact_installations
            .insert(model.id.clone(), (job_id, cancellation));
        thread::spawn(move || {
            let progress = |progress| send_install_progress(&tx, job_id, &model_name, progress);
            let result = run_verified_install(
                VerifiedInstallRequest {
                    config,
                    service,
                    runtime_id: binding.managed_runtime_id,
                    model_id,
                    existing_runtime_root: binding.installed_package_root,
                    force_runtime_package,
                    cancellation: thread_cancellation,
                    source: VerifiedInstallSource::NormalizedCatalog,
                },
                &progress,
            );
            send_verified_install_result(&tx, job_id, model_name, result);
        });
    }

    /// Starts a catalog-owned GGUF installation. Unlike the static catalog
    /// path above, its identity is derived from the full pinned Hub source;
    /// the source is only persisted after smoke validation and activation.
    fn start_trusted_remote_model_download(&mut self, request: TrustedRemoteInstallRequest) {
        if let Some(reason) = self.artifact_mutation_block_reason() {
            self.status = TranscriptionStatus::Error;
            self.status_message = reason;
            return;
        }
        let Some(model_id) = config::managed_remote_model_id(
            &request.artifact.model_id,
            &request.artifact.revision,
            &request.artifact.filename,
        ) else {
            self.status = TranscriptionStatus::Error;
            self.status_message = "The selected Hugging Face artifact failed Scribe's source validation. Refresh the catalog and try again.".to_owned();
            return;
        };
        if self.artifact_installations.contains_key(&model_id) {
            self.status_message =
                "That verified model variant is already being installed.".to_owned();
            return;
        }
        if let Some(reason) = trusted_model_install_space_error(&self.config, &request.artifact) {
            self.status = TranscriptionStatus::Error;
            self.status_message = reason;
            return;
        }
        if let Err(error) = self.transcription_service.unload_runtime() {
            self.status = TranscriptionStatus::Error;
            self.status_message =
                format!("Could not release the active speech artifact before install: {error}");
            return;
        }

        self.model_downloads.insert(
            model_id.clone(),
            ModelInstallStatus::Downloading {
                downloaded_bytes: 0,
                total_bytes: Some(request.artifact.size_bytes),
                bytes_per_second: None,
            },
        );
        self.remote_catalog.invalidate_local_models();
        self.status = TranscriptionStatus::Idle;
        self.status_message = format!("Downloading {}...", request.display_name);

        let tx = self.tx.clone();
        let config = self.config.clone();
        let service = self.transcription_service.with_config(config.clone());
        let cancellation = InstallCancellation::default();
        let thread_cancellation = cancellation.clone();
        let job_id = INSTALL_JOB_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        self.artifact_installations
            .insert(model_id.clone(), (job_id, cancellation));
        thread::spawn(move || {
            let progress = |progress| send_install_progress(&tx, job_id, &model_id, progress);
            let result = run_verified_install(
                VerifiedInstallRequest {
                    config,
                    service,
                    runtime_id: "embedded-transcribe-cpp".to_owned(),
                    model_id: ModelId::new(model_id.clone()),
                    existing_runtime_root: None,
                    force_runtime_package: false,
                    cancellation: thread_cancellation,
                    source: VerifiedInstallSource::TrustedRemote(request),
                },
                &progress,
            );
            send_verified_install_result(&tx, job_id, model_id, result);
        });
    }

    /// Validates a user-chosen GGUF in place. This is deliberately separate
    /// from the downloader: no remote source, app-managed path, or model
    /// replacement is created for an imported file.
    fn start_local_gguf_import(&mut self) {
        if self.local_gguf_import.is_some() {
            self.status_message = "A local GGUF import is already being validated.".to_owned();
            return;
        }
        if let Some(reason) = self.artifact_mutation_block_reason() {
            self.status = TranscriptionStatus::Error;
            self.status_message = reason;
            return;
        }
        let source = self.model_import_path.trim();
        if source.is_empty() {
            self.status = TranscriptionStatus::Error;
            self.status_message = "Enter the path to a local .gguf file to import.".to_owned();
            return;
        }
        let source_path = PathBuf::from(source);
        let cancellation = InstallCancellation::default();
        let thread_cancellation = cancellation.clone();
        self.status = TranscriptionStatus::Idle;
        self.status_message = "Hashing and validating the local GGUF...".to_owned();
        let tx = self.tx.clone();
        let service = self.transcription_service.with_config(self.config.clone());
        let model_storage_dir = config::model_storage_dir(&self.config);
        let job_id = INSTALL_JOB_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let (completion_tx, completion) = bounded(1);
        let worker = thread::Builder::new()
            .name("scribe-local-gguf-import".to_owned())
            .spawn(move || {
                let result = validate_local_gguf_import(
                    source_path,
                    model_storage_dir,
                    service,
                    thread_cancellation,
                )
                .map(Box::new);
                // Signal only after the final post-smoke fingerprint witness is
                // complete. The following unbounded event send cannot block.
                let _ = completion_tx.send(());
                let _ = tx.send(AppEvent::LocalGgufImportFinished { job_id, result });
            });
        match worker {
            Ok(worker) => {
                self.local_gguf_import = Some(LocalGgufImportJob {
                    job_id,
                    cancellation,
                    completion,
                    worker: Some(worker),
                });
            }
            Err(error) => {
                self.status = TranscriptionStatus::Error;
                self.status_message = format!("Could not start local GGUF validation: {error}");
            }
        }
    }

    fn finish_local_gguf_import(
        &mut self,
        job_id: u64,
        result: Result<Box<ValidatedLocalGgufImport>, String>,
    ) {
        if self
            .local_gguf_import
            .as_ref()
            .is_none_or(|job| job.job_id != job_id)
        {
            return;
        }
        let mut job = self
            .local_gguf_import
            .take()
            .expect("the matching local GGUF job must still be active");
        job.reap_completed();
        if job.cancellation.is_cancelled() {
            self.status = TranscriptionStatus::Idle;
            self.status_message =
                "Local GGUF import was cancelled. The source file was left unchanged.".to_owned();
            return;
        }
        let imported = match result {
            Ok(imported) => *imported,
            Err(error) => {
                self.status = TranscriptionStatus::Error;
                self.status_message = format!("Local GGUF import failed: {error}");
                return;
            }
        };
        // The worker supplied the final post-smoke fingerprint witness.
        // Completion deliberately performs no source file metadata/read/hash
        // work on the UI thread.
        let previous_config = self.config.clone();
        self.config.general.imported_gguf_models.insert(
            imported.model_id.as_str().to_owned(),
            imported.install.clone(),
        );
        config::normalize_config(&mut self.config);
        if !self
            .config
            .general
            .imported_gguf_models
            .contains_key(imported.model_id.as_str())
        {
            self.config = previous_config;
            self.status = TranscriptionStatus::Error;
            self.status_message = "The local GGUF changed before Scribe could persist its import receipt. Revalidate it before importing."
                .to_owned();
            return;
        }
        let receipt_path = installed_manifest::imported_manifest_path_for(
            &config::model_storage_dir(&self.config),
            &imported.model_id,
        );
        if let Err(error) = config::save_config(&self.config) {
            self.config = previous_config;
            self.status = TranscriptionStatus::Error;
            self.status_message = format!("Could not save the local import record: {error}.");
            return;
        }
        if let Err(error) =
            installed_manifest::persist_manifest_at(&imported.manifest, &receipt_path)
        {
            self.config = previous_config;
            let config_rollback = config::save_config(&self.config).err();
            let message = format!(
                "Could not persist the local import receipt after saving settings: {error}.{}",
                config_rollback
                    .as_ref()
                    .map(|error| format!(" Restoring the previous settings also failed: {error}"))
                    .unwrap_or_default()
            );
            if config_rollback.is_some() {
                self.artifact_recovery_error = Some(message.clone());
            }
            self.status = TranscriptionStatus::Error;
            self.status_message = message;
            return;
        }
        if let Some(store) = self.settings_store.as_mut() {
            store.mark_current_persisted();
        }
        self.transcription_service = self.transcription_service.with_config(self.config.clone());
        self.refresh_playground_cards_from_config();
        self.rebuild_local_models_after_committed_change();
        let model_name = imported.install.display_name;
        self.status = TranscriptionStatus::Idle;
        self.status_message = format!(
            "Imported and smoke-tested {model_name} in place (health {} ms, load {} ms, decode {} ms).",
            imported.smoke.health_duration_ms,
            imported.smoke.load_duration_ms,
            imported.smoke.decode_duration_ms,
        );
    }

    fn uninstall_model(&mut self, model: &SttModelInfo) {
        if let Some(reason) = self.artifact_mutation_block_reason() {
            self.status_message = reason;
            return;
        }
        if let Err(error) = self.transcription_service.unload_runtime() {
            self.status_message = format!("Could not unload the selected model: {error}");
            return;
        }
        let static_managed_target = self
            .config
            .general
            .managed_models
            .get(&model.id)
            .map(|install| install.path.clone())
            .filter(|path| {
                config::downloaded_model_path(&self.config, model).as_ref() == Some(path)
            })
            .filter(|path| is_app_managed_model_path(&self.config, path));
        // Each trusted remote artifact owns an opaque leaf directory. Staging
        // that directory removes the generated provenance manifest together
        // with the model, without affecting another variant in the same Hub
        // revision.
        let remote_managed_target = config::managed_remote_model_path(&self.config, &model.id)
            .and_then(|path| path.parent().map(Path::to_path_buf))
            .filter(|path| is_app_managed_model_path(&self.config, path));
        let imported_receipt_target = self
            .config
            .general
            .imported_gguf_models
            .contains_key(&model.id)
            .then(|| {
                installed_manifest::imported_manifest_path_for(
                    &config::model_storage_dir(&self.config),
                    &ModelId::new(&model.id),
                )
            })
            .filter(|path| is_app_managed_model_path(&self.config, path));
        let imported_local = imported_receipt_target.is_some();
        let managed_target = static_managed_target
            .or(remote_managed_target)
            .or(imported_receipt_target);
        let prior_fingerprint = match config::settings::artifact_config_fingerprint(&self.config) {
            Ok(fingerprint) => fingerprint,
            Err(error) => {
                self.status = TranscriptionStatus::Error;
                self.status_message =
                    format!("Could not prepare model removal settings witness: {error}");
                return;
            }
        };
        let mut staged_removal = match managed_target.as_ref() {
            Some(target) => {
                match ManagedRemoval::stage(target, std::slice::from_ref(target), prior_fingerprint)
                {
                    Ok(removal) => Some(removal),
                    Err(error) => {
                        if error.requires_recovery() {
                            self.artifact_recovery_error = Some(error.to_string());
                        }
                        self.status = TranscriptionStatus::Error;
                        self.status_message = format!("Could not stage model removal: {error}");
                        return;
                    }
                }
            }
            None => None,
        };
        let removed_files = staged_removal
            .as_ref()
            .is_some_and(ManagedRemoval::removed_files);
        let previous_config = self.config.clone();
        self.remote_catalog.invalidate_local_models();
        self.model_downloads.remove(&model.id);
        self.config.general.managed_models.remove(&model.id);
        self.config.general.managed_remote_models.remove(&model.id);
        self.config.general.imported_gguf_models.remove(&model.id);
        self.config.general.model_paths.remove(&model.id);
        set_model_selected(&mut self.config, &model.id, false);

        if self.config.general.selected_default_model == model.id {
            select_first_installed_model(&mut self.config);
        }
        config::normalize_config(&mut self.config);
        let removal_preparation = config::settings::artifact_config_fingerprint(&self.config)
            .map_err(|error| error.to_string())
            .and_then(|fingerprint| {
                staged_removal.as_mut().map_or(Ok(()), |removal| {
                    removal
                        .prepare_config_commit(fingerprint)
                        .map_err(|error| error.to_string())
                })
            });
        if let Err(error) = removal_preparation {
            self.config = previous_config;
            let rollback = staged_removal.and_then(|removal| removal.rollback().err());
            let message = format!(
                "Could not prepare model removal transaction: {error}{}",
                rollback
                    .as_ref()
                    .map(|error| format!(". Restoring the model also failed: {error}"))
                    .unwrap_or_default()
            );
            if rollback.is_some() {
                self.artifact_recovery_error = Some(message.clone());
            }
            self.status = TranscriptionStatus::Error;
            self.status_message = message;
            return;
        }
        if let Err(error) = config::save_config(&self.config) {
            self.config = previous_config;
            let message = format!(
                "Could not confirm model removal settings persistence: {error}. The artifact tombstone and removal journal were retained; restart Scribe to reconcile the durable settings witness."
            );
            if staged_removal.is_some() {
                self.artifact_recovery_error = Some(message.clone());
            }
            self.status = TranscriptionStatus::Error;
            self.status_message = message;
            return;
        }
        let cleanup = staged_removal.and_then(|removal| removal.commit().err());
        if let Some(store) = self.settings_store.as_mut() {
            store.mark_current_persisted();
        }
        self.transcription_service = self.transcription_service.with_config(self.config.clone());
        self.refresh_playground_cards_from_config();
        if let Some(error) = cleanup {
            let message = format!(
                "{} was removed, but cleanup is incomplete; restart Scribe before changing artifacts again: {error}",
                model.name
            );
            self.artifact_recovery_error = Some(message.clone());
            self.status = TranscriptionStatus::Error;
            self.status_message = message;
            return;
        }
        self.status = TranscriptionStatus::Idle;
        self.status_message = if imported_local {
            format!(
                "Removed {} from Scribe. The local GGUF file was kept.",
                model.name
            )
        } else {
            match removed_files {
                true => format!("Uninstalled {}.", model.name),
                false => format!("Removed {} from Scribe.", model.name),
            }
        };
    }

    fn request_runtime_install(&mut self, model: &SttModelInfo, intent: RuntimeJobIntent) {
        self.remote_catalog.invalidate_local_models();
        if let Some(reason) = self.artifact_mutation_block_reason() {
            self.status_message = reason;
            return;
        }
        if self
            .transcription_service
            .installation_binding(&ModelId::new(&model.id))
            .is_ok()
        {
            match intent {
                RuntimeJobIntent::DownloadModel(_) => self.start_model_download_only(model),
                RuntimeJobIntent::RepairModel(_) | RuntimeJobIntent::Maintenance
                    if model.local_path.as_ref().is_some_and(|path| path.is_file()) =>
                {
                    self.start_model_download_with_runtime_policy(model, true);
                }
                RuntimeJobIntent::RepairModel(_) | RuntimeJobIntent::Maintenance => {
                    self.fail_model_install(
                        &model.id,
                        "Install this model to verify a runtime update against an exact pinned artifact before activation."
                            .to_owned(),
                    );
                }
            }
            return;
        }
        let Some(provider) = compatibility_bridge::provider_for_model(model) else {
            self.status = TranscriptionStatus::Error;
            self.status_message = "Model provider is not available.".to_owned();
            return;
        };

        if !provider.runtime_install_supported() {
            self.status = TranscriptionStatus::Error;
            self.status_message =
                "Managed local runtime installation is not available in this build.".to_owned();
            return;
        }

        let Some(source) = runtime_install_source(&self.config, model) else {
            let message = missing_runtime_source_message();
            match intent {
                RuntimeJobIntent::DownloadModel(model_id) => {
                    self.model_downloads
                        .insert(model_id, ModelInstallStatus::Error(message.clone()));
                }
                RuntimeJobIntent::RepairModel(model_id) => {
                    self.model_downloads
                        .insert(model_id, ModelInstallStatus::RuntimeError(message.clone()));
                }
                RuntimeJobIntent::Maintenance => {}
            }
            self.status = TranscriptionStatus::Error;
            self.status_message = message;
            return;
        };

        if let Some(job) = self.runtime_jobs.get_mut(provider.id()) {
            let queued_model_id = match intent {
                RuntimeJobIntent::DownloadModel(model_id) => {
                    queue_runtime_model(&mut job.download_model_ids, model_id.clone())
                        .then_some(model_id)
                }
                RuntimeJobIntent::RepairModel(model_id) => {
                    queue_runtime_model(&mut job.repair_model_ids, model_id.clone())
                        .then_some(model_id)
                }
                RuntimeJobIntent::Maintenance => None,
            };
            if let Some(model_id) = queued_model_id {
                self.model_downloads
                    .insert(model_id, ModelInstallStatus::InstallingRuntime);
            }
            return;
        }

        let mut job = RuntimeInstallJob::default();
        let queued_model_id = match intent {
            RuntimeJobIntent::DownloadModel(model_id) => {
                queue_runtime_model(&mut job.download_model_ids, model_id.clone());
                Some(model_id)
            }
            RuntimeJobIntent::RepairModel(model_id) => {
                queue_runtime_model(&mut job.repair_model_ids, model_id.clone());
                Some(model_id)
            }
            RuntimeJobIntent::Maintenance => None,
        };
        if let Some(model_id) = queued_model_id {
            self.model_downloads
                .insert(model_id.clone(), ModelInstallStatus::InstallingRuntime);
        }
        self.runtime_jobs.insert(provider.id().to_owned(), job);
        self.status = TranscriptionStatus::Idle;
        self.status_message = "Preparing local speech runtime...".to_owned();

        let tx = self.tx.clone();
        let runtime_id = provider.id().to_owned();
        let runtime_label = "Local speech".to_owned();
        thread::spawn(move || {
            let (result, source_label) = match source {
                RuntimeInstallSource::Packaged(packaged_path) => (
                    install_runtime_files(&runtime_id, &packaged_path),
                    "packaged-runtime",
                ),
                RuntimeInstallSource::DevelopmentScript(package) => (
                    build_development_runtime_package(&runtime_id, &runtime_label, package),
                    "development-script",
                ),
            };
            match result {
                Ok(replacement) => {
                    let _ = tx.send(AppEvent::RuntimeInstallDone {
                        runtime_id,
                        runtime_label,
                        replacement,
                        source_label,
                    });
                }
                Err(message) => {
                    let _ = tx.send(AppEvent::RuntimeInstallFailed {
                        runtime_id,
                        message,
                    });
                }
            }
        });
    }

    fn fail_model_install(&mut self, model_id: &str, message: String) {
        self.remote_catalog.invalidate_local_models();
        self.model_downloads.insert(
            model_id.to_owned(),
            ModelInstallStatus::Error(message.clone()),
        );
        self.status = TranscriptionStatus::Error;
        self.status_message = message;
    }

    fn fail_runtime_job(&mut self, runtime_id: &str, message: String) {
        self.remote_catalog.invalidate_local_models();
        if let Some(job) = self.runtime_jobs.remove(runtime_id) {
            for model_id in job.download_model_ids {
                self.model_downloads
                    .insert(model_id, ModelInstallStatus::Error(message.clone()));
            }
            for model_id in job.repair_model_ids {
                self.model_downloads
                    .insert(model_id, ModelInstallStatus::RuntimeError(message.clone()));
            }
        }
        self.status = TranscriptionStatus::Error;
        self.status_message = format!("Runtime installation failed: {message}");
    }

    fn uninstall_runtime(&mut self, model: &SttModelInfo) {
        if let Some(reason) = self.artifact_mutation_block_reason() {
            self.status_message = reason;
            return;
        }
        if let Err(error) = self.transcription_service.unload_runtime() {
            self.status_message = format!("Could not unload the local runtime: {error}");
            return;
        }
        if let Ok(binding) = self
            .transcription_service
            .installation_binding(&ModelId::new(&model.id))
        {
            let target = config::runtime_storage_dir().join(&binding.managed_runtime_id);
            let owns_target = self
                .config
                .general
                .managed_runtimes
                .get(&binding.managed_runtime_id)
                .and_then(|install| {
                    runtime_uninstall_target(
                        &config::runtime_storage_dir(),
                        &binding.managed_runtime_id,
                        &install.path,
                    )
                })
                .is_some_and(|candidate| candidate == target);
            let prior_fingerprint =
                match config::settings::artifact_config_fingerprint(&self.config) {
                    Ok(fingerprint) => fingerprint,
                    Err(error) => {
                        self.status = TranscriptionStatus::Error;
                        self.status_message =
                            format!("Could not prepare runtime removal settings witness: {error}");
                        return;
                    }
                };
            let mut staged_removal = if owns_target {
                match ManagedRemoval::stage(
                    &target,
                    std::slice::from_ref(&target),
                    prior_fingerprint,
                ) {
                    Ok(removal) => Some(removal),
                    Err(error) => {
                        if error.requires_recovery() {
                            self.artifact_recovery_error = Some(error.to_string());
                        }
                        self.status = TranscriptionStatus::Error;
                        self.status_message = format!("Could not stage runtime removal: {error}");
                        return;
                    }
                }
            } else {
                None
            };
            let removed_files = staged_removal
                .as_ref()
                .is_some_and(ManagedRemoval::removed_files);
            let previous_config = self.config.clone();
            self.config
                .general
                .managed_runtimes
                .remove(&binding.managed_runtime_id);
            config::normalize_config(&mut self.config);
            let removal_preparation = config::settings::artifact_config_fingerprint(&self.config)
                .map_err(|error| error.to_string())
                .and_then(|fingerprint| {
                    staged_removal.as_mut().map_or(Ok(()), |removal| {
                        removal
                            .prepare_config_commit(fingerprint)
                            .map_err(|error| error.to_string())
                    })
                });
            if let Err(error) = removal_preparation {
                self.config = previous_config;
                let rollback = staged_removal.and_then(|removal| removal.rollback().err());
                let message = format!(
                    "Could not prepare runtime removal transaction: {error}{}",
                    rollback
                        .as_ref()
                        .map(|error| format!(". Restoring the runtime also failed: {error}"))
                        .unwrap_or_default()
                );
                if rollback.is_some() {
                    self.artifact_recovery_error = Some(message.clone());
                }
                self.status = TranscriptionStatus::Error;
                self.status_message = message;
                return;
            }
            if let Err(error) = config::save_config(&self.config) {
                self.config = previous_config;
                let message = format!(
                    "Could not confirm runtime removal settings persistence: {error}. The runtime tombstone and removal journal were retained; restart Scribe to reconcile the durable settings witness."
                );
                if staged_removal.is_some() {
                    self.artifact_recovery_error = Some(message.clone());
                }
                self.status = TranscriptionStatus::Error;
                self.status_message = message;
                return;
            }
            let cleanup = staged_removal
                .and_then(|removal| removal.commit().err())
                .or_else(|| crate::installations::remove_previous_runtime_if_exists(&target).err());
            if let Some(store) = self.settings_store.as_mut() {
                store.mark_current_persisted();
            }
            self.transcription_service =
                self.transcription_service.with_config(self.config.clone());
            self.refresh_playground_runtime_statuses();
            self.rebuild_local_models_after_committed_change();
            if let Some(error) = cleanup {
                let message = format!(
                    "Runtime was removed, but cleanup is incomplete; restart Scribe before changing artifacts again: {error}"
                );
                self.artifact_recovery_error = Some(message.clone());
                self.status = TranscriptionStatus::Error;
                self.status_message = message;
                return;
            }
            self.status = TranscriptionStatus::Idle;
            self.status_message = if removed_files {
                "Uninstalled verified local speech runtime.".to_owned()
            } else {
                "Removed verified local speech runtime from Scribe.".to_owned()
            };
            return;
        }
        let Some(provider) = compatibility_bridge::provider_for_model(model) else {
            self.status = TranscriptionStatus::Error;
            self.status_message = "Model provider is not available.".to_owned();
            return;
        };

        let previous_config = self.config.clone();
        self.config.general.managed_runtimes.remove(provider.id());
        config::normalize_config(&mut self.config);
        if let Err(error) = config::save_config(&self.config) {
            self.config = previous_config;
            self.status = TranscriptionStatus::Error;
            self.status_message = format!(
                "Could not persist legacy runtime settings removal: {error}. Unmanaged files were left untouched."
            );
            return;
        }
        if let Some(store) = self.settings_store.as_mut() {
            store.mark_current_persisted();
        }
        self.transcription_service = self.transcription_service.with_config(self.config.clone());
        self.refresh_playground_runtime_statuses();
        self.rebuild_local_models_after_committed_change();
        self.status = TranscriptionStatus::Idle;
        self.status_message = "Removed the legacy runtime from Scribe settings. Its files were preserved because they are not governed by the normalized manifest transaction.".to_owned();
    }

    fn refresh_playground_runtime_statuses(&mut self) {
        for card in &mut self.playground_cards {
            card.status = runtime_status_for_id(&self.config, card.descriptor.id.as_str());
        }
    }

    fn artifact_mutation_block_reason(&self) -> Option<String> {
        if let Some(error) = self.artifact_recovery_error.as_ref() {
            Some(error.clone())
        } else if self.capture_is_active() || self.pending_recording.is_some() {
            Some("Stop the active recording before changing speech artifacts.".to_owned())
        } else if self.effective_status() == TranscriptionStatus::Transcribing
            || self.pending_output.is_some()
        {
            Some("Wait for final transcription and output to finish before changing speech artifacts.".to_owned())
        } else if self.playground_pending > 0 {
            Some("Wait for Playground jobs to finish before changing speech artifacts.".to_owned())
        } else if !self.artifact_installations.is_empty()
            || !self.runtime_jobs.is_empty()
            || self.local_gguf_import.is_some()
        {
            Some("Wait for the active installation to finish or cancel it first.".to_owned())
        } else {
            None
        }
    }

    fn cancel_installations_for_shutdown(&mut self) {
        for (_, cancellation) in self.artifact_installations.values() {
            cancellation.cancel();
        }
        if let Some(mut job) = self.local_gguf_import.take()
            && !job.cancel_and_wait(LOCAL_GGUF_IMPORT_SHUTDOWN_TIMEOUT)
        {
            eprintln!(
                "local GGUF import exceeded the shutdown deadline; detaching cancelled worker"
            );
        }
    }
}

impl Drop for LocalTranscriberApp {
    fn drop(&mut self) {
        #[cfg(test)]
        if let Some(fixture) = self.test_gguf_fixture.take() {
            let _ = fs::remove_file(fixture);
        }
        for (_, target) in self.captured_targets.drain() {
            crate::overlay::platform::release_captured_target(&target);
        }
    }
}

impl eframe::App for LocalTranscriberApp {
    fn clear_color(&self, visuals: &egui::Visuals) -> [f32; 4] {
        ThemePalette::from_visuals(visuals)
            .content_bg
            .to_normalized_gamma_f32()
    }

    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        self.apply_theme(ctx, frame);
        paint_viewport_background(ctx);
        self.handle_close_request(ctx);
        self.poll_tray(ctx);
        self.poll_hotkey_capture(ctx);
        if !self.capturing_hotkey {
            self.poll_hotkey();
        }
        self.poll_rolling_preview();
        self.poll_preview_drain();
        self.poll_recording();
        self.poll_microphone_test();
        self.poll_preview_drain();
        // Pending output was created in the prior frame, allowing the overlay
        // to present the correlated Output/Pasting phase before any blocking
        // clipboard or synthetic-input work begins.
        self.poll_pending_output();
        self.poll_history_playback();
        self.poll_events();
        self.apply_deferred_history_retention_if_idle();
        self.poll_settings_save();
        self.sync_tray_state();

        show_navigation(ctx, &mut self.current_tab, self.config.developer.debug_mode);
        if self.current_tab == Tab::Advanced {
            self.settings_tab = SettingsTab::Advanced;
            self.current_tab = Tab::General;
        }
        if self.current_tab == Tab::General
            && self.settings_tab == SettingsTab::Recording
            && !self.window_hidden_to_tray
        {
            self.ensure_microphone_monitor();
        } else {
            self.suspend_microphone_monitor();
        }
        egui::CentralPanel::default()
            .frame(content_panel_frame(ctx))
            .show(ctx, |ui| match self.current_tab {
                Tab::Transcribe => {
                    show_route_scroll(ui, UiRoute::Transcribe, |ui| self.ui_transcribe(ui))
                }
                Tab::General => show_route_scroll(ui, UiRoute::Settings(self.settings_tab), |ui| {
                    self.ui_general_settings(ui)
                }),
                Tab::Models => show_route_scroll(ui, UiRoute::Models, |ui| self.ui_models(ui)),
                Tab::History => show_route_scroll(ui, UiRoute::History, |ui| self.ui_history(ui)),
                Tab::Advanced => unreachable!("advanced navigation is routed to Settings"),
                Tab::About => show_route_scroll(ui, UiRoute::About, |ui| self.ui_about(ui)),
                Tab::Debug => show_route_scroll(ui, UiRoute::Debug, |ui| self.ui_playground(ui)),
            });

        if self.current_tab != Tab::General
            || self.settings_tab != SettingsTab::Recording
            || self.window_hidden_to_tray
        {
            self.suspend_microphone_monitor();
        }

        self.sync_overlay_state();
        let overlay_session_id = self.overlay_controller.state().session_id;
        let target = overlay_session_id.and_then(|id| self.captured_targets.get(&id));
        overlay::show_overlay_viewport(
            ctx,
            self.overlay_controller.state(),
            target,
            native_overlay_position(self.config.overlay.position),
        );

        let repaint_delay = self.next_repaint_delay();
        if self.window_hidden_to_tray
            && let Some(error) = self
                .tray_service
                .as_ref()
                .and_then(|tray_service| tray_service.schedule_hidden_repaint(repaint_delay).err())
        {
            self.window_hidden_to_tray = false;
            ctx.send_viewport_cmd(ViewportCommand::Visible(true));
            self.status_message = format!(
                "Scribe restored its window because hidden tray processing failed: {error}"
            );
        }
        ctx.request_repaint_after(repaint_delay);
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.quit_requested = true;
        self.deferred_recording_start = None;
        self.deferred_history_playback = None;
        self.stop_microphone_test();
        self.cancel_installations_for_shutdown();
        self.shutdown_transcription_for_exit();
        let _ = self.session_coordinator.cancel_active();
        self.flush_settings();
    }
}

impl LocalTranscriberApp {
    fn ui_transcribe(&mut self, ui: &mut Ui) {
        let models = self.transcribe_screen_models();
        let (selected_model_id, model_readiness) = self.selected_model_screen_state();
        let microphone_permission = self.microphone_permission();
        let no_speech = self.status_message == "No speech detected; nothing was pasted.";
        let provisional_transcript = self.capture_is_active().then(|| {
            let preview = &self.overlay_controller.state().transcript;
            [preview.committed.as_str(), preview.tentative.as_str()]
                .into_iter()
                .filter(|text| !text.trim().is_empty())
                .collect::<Vec<_>>()
                .join(" ")
        });
        let state = transcription_state(
            self.effective_status(),
            selected_model_id,
            model_readiness,
            self.pending_recording.is_some(),
            no_speech,
            self.active_recording
                .as_ref()
                .map(|recording| recording.started_at.elapsed().as_millis() as u64)
                .unwrap_or_default(),
            self.transcript.clone(),
            provisional_transcript.unwrap_or_default(),
            if no_speech {
                Some("No speech detected — nothing was added.".to_owned())
            } else {
                (!self.status_message.is_empty()).then(|| self.status_message.clone())
            },
            self.config.recording.hotkey.clone(),
            recording_mode(self.config.recording.hotkey_mode == HotkeyMode::HoldToTalk),
            microphone_permission,
        );
        let settings = RecordingSettingsView {
            duration_label: format!("{} seconds", self.config.recording.max_recording_seconds),
            provisional_feedback: self.config.streaming.mode != StreamingMode::FinalOnly,
            device_label: self
                .config
                .recording
                .audio_input_device_name
                .clone()
                .unwrap_or_else(|| "OS default".to_owned()),
            save_state: settings_save_state(
                self.settings_store
                    .as_ref()
                    .is_some_and(SettingsStore::has_pending),
                self.effective_status() == TranscriptionStatus::Error
                    && self.status_message.starts_with("Failed to save settings:"),
            ),
            ..Default::default()
        };
        let action = render_screen(
            ui,
            &ScreenView {
                route: UiRoute::Transcribe,
                transcription: &state,
                models: &models,
                model_catalog: &[],
                comparison: &Default::default(),
                model_management: &Default::default(),
                model_language_filter: ModelLanguageFilter::default(),
                remote_catalog: &Default::default(),
                recording_settings: &settings,
            },
        );
        self.apply_transcribe_screen_action(action);
    }

    fn selected_model_screen_state(&self) -> (Option<String>, ModelReadiness) {
        let Some(model) = self.selected_model() else {
            return (None, ModelReadiness::Error);
        };
        let readiness = match runtime_status_for_model(&self.config, &model) {
            ModelRuntimeStatus::Ready => ModelReadiness::Ready,
            ModelRuntimeStatus::Downloading | ModelRuntimeStatus::Running => {
                ModelReadiness::Loading
            }
            ModelRuntimeStatus::MissingConfiguration
            | ModelRuntimeStatus::NotInstalled
            | ModelRuntimeStatus::NotImplemented
            | ModelRuntimeStatus::Error(_) => ModelReadiness::Error,
        };
        (Some(model.id), readiness)
    }

    fn microphone_permission(&self) -> MicrophonePermission {
        if self.effective_status() == TranscriptionStatus::Listening {
            MicrophonePermission::Granted
        } else if self.effective_status() == TranscriptionStatus::Error
            && self.status_message.starts_with("Microphone failed:")
        {
            MicrophonePermission::Denied
        } else {
            MicrophonePermission::Unknown
        }
    }

    fn transcribe_screen_models(&self) -> Vec<ModelViewModel> {
        let Some(model) = self.selected_model() else {
            return Vec::new();
        };
        let descriptor = self
            .transcription_service
            .model_descriptor(&ModelId::new(&model.id))
            .ok();
        vec![self.model_management_view_model(&model, descriptor.as_ref())]
    }

    fn apply_transcribe_screen_action(&mut self, action: ScreenAction) {
        match action {
            ScreenAction::AddModel | ScreenAction::ChangeModel => self.current_tab = Tab::Models,
            ScreenAction::StartRecording | ScreenAction::RetryMicrophone => {
                self.start_recording(RecordingSource::Transcribe)
            }
            ScreenAction::StopRecording => self.stop_recording(),
            ScreenAction::OpenAudioSettings => self.open_system_audio_settings(),
            ScreenAction::ClearTranscript => self.clear_transcript_history(),
            ScreenAction::CopyTranscript => self.copy_transcript_to_clipboard(),
            ScreenAction::None
            | ScreenAction::SelectModel(_)
            | ScreenAction::InstallModel(_)
            | ScreenAction::CancelModelInstall(_)
            | ScreenAction::ShowModelDetails(_)
            | ScreenAction::RequestModelRemoval(_)
            | ScreenAction::ConfirmModelRemoval(_)
            | ScreenAction::CloseModelDialog
            | ScreenAction::ToggleComparison
            | ScreenAction::ToggleComparisonModel(_)
            | ScreenAction::StartComparison
            | ScreenAction::StopComparison
            | ScreenAction::ShowComparisonReferenceEditor
            | ScreenAction::HideComparisonReferenceEditor
            | ScreenAction::EditComparisonReference(_)
            | ScreenAction::ApplyComparisonReference
            | ScreenAction::ClearComparisonReference
            | ScreenAction::SetSettingsTab(_)
            | ScreenAction::SetCloseToTray(_)
            | ScreenAction::SetRecordingMode(_)
            | ScreenAction::SetDurationSeconds(_)
            | ScreenAction::ToggleProvisionalFeedback
            | ScreenAction::SetAudioDevice(_)
            | ScreenAction::SetInputSensitivity(_)
            | ScreenAction::RepairModelRuntime(_)
            | ScreenAction::MaintainModelRuntime(_)
            | ScreenAction::SetRemoteCatalogQuery(_)
            | ScreenAction::SetModelLanguageFilter(_)
            | ScreenAction::ToggleInstalledModels
            | ScreenAction::ToggleAvailableModels
            | ScreenAction::FocusModelCard(_)
            | ScreenAction::AcknowledgeModelCardFocus(_)
            | ScreenAction::AcknowledgeModelControlFocus { .. }
            | ScreenAction::RetryRemoteCatalog
            | ScreenAction::InstallRemoteCatalogVariant { .. }
            | ScreenAction::CancelRemoteCatalogInstall(_)
            | ScreenAction::UseRemoteCatalogModel(_)
            | ScreenAction::RemoveRemoteCatalogModel(_)
            | ScreenAction::SetLocalGgufImportPath(_)
            | ScreenAction::ValidateAndImportLocalGguf
            | ScreenAction::CancelLocalGgufImport
            | ScreenAction::RefreshDevices
            | ScreenAction::ChangeShortcut
            | ScreenAction::SetAutoInsertTranscript(_)
            | ScreenAction::SetRestoreClipboardAfterInsert(_)
            | ScreenAction::SetPasteDelayMs(_)
            | ScreenAction::OpenModelSettings
            | ScreenAction::SetHotkeyInput(_)
            | ScreenAction::ApplyHotkey
            | ScreenAction::SetTheme(_)
            | ScreenAction::SetOverlayMode(_)
            | ScreenAction::SetVadEnabled(_)
            | ScreenAction::SetSpeechConfirmationMs(_)
            | ScreenAction::SetInternalPauseMs(_)
            | ScreenAction::SetEndpointSilenceMs(_)
            | ScreenAction::SetPreRollMs(_)
            | ScreenAction::SetPostRollMs(_)
            | ScreenAction::SetStreamingMode(_)
            | ScreenAction::SetAcceleration(_)
            | ScreenAction::SetOverlayPosition(_)
            | ScreenAction::SetDebugMode(_)
            | ScreenAction::SetHistoryMode(_)
            | ScreenAction::SetMaxHistoryEntries(_)
            | ScreenAction::SetTranscriptRetentionDays(_)
            | ScreenAction::SetAudioRetentionDays(_)
            | ScreenAction::SetStoreApplicationIdentity(_) => {}
        }
    }

    fn request_remote_catalog(&mut self) {
        #[cfg(test)]
        {
            self.remote_catalog.catalog_io_request_count += 1;
        }
        let Some(next_generation) = self.remote_catalog.refresh_generation.checked_add(1) else {
            self.remote_catalog.loading = false;
            self.remote_catalog.active_refresh_generation = None;
            self.remote_catalog.error = Some(
                "The trusted catalog refresh counter was exhausted; restart Scribe to refresh."
                    .to_owned(),
            );
            return;
        };
        self.remote_catalog.refresh_generation = next_generation;
        let generation = self.remote_catalog.refresh_generation;
        self.remote_catalog.loading = true;
        self.remote_catalog.active_refresh_generation = Some(generation);
        self.remote_catalog.error = None;
        let tx = self.tx.clone();
        let spawn = thread::Builder::new()
            .name("scribe-huggingface-catalog".to_owned())
            .spawn(move || {
                let service = HuggingFaceCatalogService::online();
                let result = service
                    .refresh(generation)
                    .map_err(|error| error.to_string());
                let _ = tx.send(AppEvent::RemoteCatalogLoaded { generation, result });
            });
        if let Err(error) = spawn {
            self.remote_catalog.loading = false;
            self.remote_catalog.active_refresh_generation = None;
            self.remote_catalog.error = Some(format!(
                "Could not start trusted catalog discovery: {error}"
            ));
        }
    }

    fn ui_models(&mut self, ui: &mut Ui) {
        if self.remote_catalog.force_refresh_requested {
            self.remote_catalog.force_refresh_requested = false;
            self.request_remote_catalog();
        }
        self.model_management.mutation_block_reason = self.artifact_mutation_block_reason();
        self.model_comparison.start_disabled_reason =
            self.comparison_start_block_reason().map(str::to_owned);
        let catalog = Arc::clone(&self.remote_catalog.local_models);
        self.model_comparison.selected_model_ids.retain(|id| {
            catalog.iter().any(|model| {
                &model.id == id
                    && model.installed
                    && model.ready
                    && model.compatibility != ModelCompatibility::Incompatible
            })
        });
        self.sync_model_comparison_state();
        let remote_catalog = self.remote_catalog_view();
        let clear_initial_dialog_focus = self.model_management.focus_dialog_initial;
        let clear_add_focus = self.model_management.restore_add_focus;
        let clear_after_removal_focus = self.model_management.restore_after_removal_focus;
        let clear_removal_notice = self.model_management.removal_notice.is_some();
        let clear_reference_editor_focus = self.model_comparison.focus_reference_editor;
        let clear_comparison_focus = self.model_comparison.focus_panel;
        let clear_reference_action_focus = self.model_comparison.restore_reference_action_focus;
        let clear_reference_notice = self.model_comparison.reference_notice.is_some();
        let view = ScreenView {
            route: UiRoute::Models,
            transcription: &Default::default(),
            models: &catalog,
            model_catalog: &catalog,
            comparison: &self.model_comparison,
            model_management: &self.model_management,
            model_language_filter: self.model_language_filter,
            remote_catalog: &remote_catalog,
            recording_settings: &Default::default(),
        };
        let action = render_screen(ui, &view);
        if clear_initial_dialog_focus {
            self.model_management.focus_dialog_initial = false;
        }
        if clear_add_focus {
            self.model_management.restore_add_focus = false;
        }
        if clear_after_removal_focus {
            self.model_management.restore_after_removal_focus = false;
        }
        if clear_removal_notice {
            self.model_management.removal_notice = None;
        }
        if clear_reference_editor_focus {
            self.model_comparison.focus_reference_editor = false;
        }
        if clear_comparison_focus {
            self.model_comparison.focus_panel = false;
        }
        if clear_reference_action_focus {
            self.model_comparison.restore_reference_action_focus = false;
        }
        if clear_reference_notice {
            self.model_comparison.reference_notice = None;
        }
        self.apply_model_management_action(action);
    }

    fn remote_catalog_view(&mut self) -> RemoteCatalogView {
        let search = self.model_search.trim().to_ascii_lowercase();
        let projection_key = RemoteCatalogProjectionKey {
            revision: self.remote_catalog.projection_revision,
            inventory_revision: self
                .remote_catalog
                .snapshot
                .as_ref()
                .map_or(0, ModelInventorySnapshot::revision),
            search: search.clone(),
            language_filter: self.model_language_filter,
            filters: self.remote_catalog_filters,
            sort: self.remote_catalog_sort,
            mutation_block_reason: self.artifact_mutation_block_reason(),
        };
        if self
            .remote_catalog
            .projection
            .as_ref()
            .is_none_or(|projection| projection.key != projection_key)
        {
            let projection = self.build_remote_catalog_projection(projection_key);
            self.remote_catalog.projection = Some(projection);
            #[cfg(test)]
            {
                self.remote_catalog.projection_build_count += 1;
            }
        }
        let projection = self
            .remote_catalog
            .projection
            .as_ref()
            .expect("catalog projection was populated above");
        let entries = projection.entries.clone();
        let shown_count = entries.len();
        let matching_count = projection.matching_count;
        let total_count = projection.total_count;
        let count_summary = if matching_count > shown_count {
            format!(
                "Showing {shown_count} of {matching_count} matching models ({total_count} total). Refine search or filters to see omitted matches."
            )
        } else {
            format!("Showing {shown_count} of {total_count} models.")
        };
        let snapshot = self.remote_catalog.snapshot.as_ref();
        let (kind, message) = if self.remote_catalog.loading {
            let message = snapshot.map_or_else(
                || "Loading the trusted catalog.".to_owned(),
                |snapshot| {
                    format!(
                        "Refreshing the trusted catalog from {}. {count_summary}",
                        snapshot.source().label(),
                    )
                },
            );
            (RemoteCatalogStatusKind::Loading, message)
        } else if let Some(error) = self.remote_catalog.error.as_deref() {
            let message = snapshot.map_or_else(
                || format!("Catalog unavailable: {error}"),
                |snapshot| {
                    format!(
                        "Catalog refresh failed; showing data from {}. {count_summary} {error}",
                        snapshot.source().label(),
                    )
                },
            );
            (RemoteCatalogStatusKind::Error, message)
        } else if let Some(snapshot) = snapshot {
            let kind = match snapshot.source() {
                CatalogSource::BundledFallback => RemoteCatalogStatusKind::Offline,
                CatalogSource::Network => RemoteCatalogStatusKind::Available,
            };
            (
                kind,
                format!("{} · {count_summary}", snapshot.source().label(),),
            )
        } else {
            (
                RemoteCatalogStatusKind::Idle,
                "Catalog discovery has not completed yet. Refresh to retry.".to_owned(),
            )
        };

        RemoteCatalogView {
            local_import: crate::ui::LocalGgufImportView {
                path: self.model_import_path.clone(),
                in_progress: self.local_gguf_import.is_some(),
                import_enabled: self.local_gguf_import.is_none()
                    && self.artifact_mutation_block_reason().is_none(),
                disabled_reason: self.artifact_mutation_block_reason(),
            },
            query: self.model_search.clone(),
            filters: self.remote_catalog_filters,
            sort: self.remote_catalog_sort,
            status: RemoteCatalogStatusView { kind, message },
            refresh_enabled: !self.remote_catalog.loading,
            has_snapshot: snapshot.is_some(),
            entries,
        }
    }

    fn build_remote_catalog_projection(
        &self,
        key: RemoteCatalogProjectionKey,
    ) -> RemoteCatalogProjection {
        let Some(snapshot) = self.remote_catalog.snapshot.as_ref() else {
            return RemoteCatalogProjection {
                key,
                matching_count: 0,
                total_count: 0,
                entries: Vec::new(),
            };
        };
        let matching = filtered_remote_models(
            snapshot.models(),
            &self.config,
            &key.search,
            key.filters,
            key.sort,
            key.language_filter,
        );
        let matching_count = matching.len();
        let entries = matching
            .into_iter()
            .take(REMOTE_CATALOG_VISIBLE_LIMIT)
            .map(|model| self.remote_catalog_entry_view(model))
            .collect();
        RemoteCatalogProjection {
            key,
            matching_count,
            total_count: snapshot.models().len(),
            entries,
        }
    }

    fn remote_catalog_entry_view(&self, model: &RemoteModel) -> RemoteCatalogEntryView {
        let mutation_block_reason = self.artifact_mutation_block_reason();
        let variants = model
            .variants
            .iter()
            .map(|variant| {
                let artifact = model.artifact_for(&variant.id);
                let normalized_model_id = artifact.as_ref().and_then(|artifact| {
                    crate::model_catalog::normalized_model_id_for_pinned_artifact(
                        &artifact.model_id,
                        &artifact.revision,
                        &artifact.filename,
                    )
                });
                let remote_id = artifact.as_ref().and_then(|artifact| {
                    config::managed_remote_model_id(
                        &artifact.model_id,
                        &artifact.revision,
                        &artifact.filename,
                    )
                });
                let installed_id = remote_id.as_ref().and_then(|id| {
                    self.config
                        .general
                        .managed_remote_models
                        .contains_key(id)
                        .then(|| id.clone())
                });
                let previous_revision = artifact.as_ref().is_some_and(|artifact| {
                    self.config
                        .general
                        .managed_remote_models
                        .values()
                        .any(|install| {
                            install.repository == artifact.model_id
                                && install.filename == artifact.filename
                                && install.revision != artifact.revision
                        })
                });
                let install_disabled_reason = mutation_block_reason.clone();
                let install_action = |label: &str| RemoteCatalogActionView {
                    label: label.to_owned(),
                    kind: RemoteCatalogActionKind::Install {
                        remote_model_id: model.id.clone(),
                        variant_id: variant.id.clone(),
                    },
                    enabled: artifact.is_some() && install_disabled_reason.is_none(),
                    disabled_reason: install_disabled_reason.clone().or_else(|| {
                        artifact.is_none().then(|| {
                            "This catalog entry no longer resolves to a validated pinned artifact. Refresh the catalog."
                                .to_owned()
                        })
                    }),
                };

                let status_label;
                let mut actions = Vec::new();
                if let Some(remote_id) = remote_id.as_deref()
                    && let Some(status) = self.model_downloads.get(remote_id)
                {
                    status_label = Some(status.label());
                    if matches!(
                        status,
                        ModelInstallStatus::Downloading { .. }
                            | ModelInstallStatus::InstallingRuntime
                    ) {
                        actions.push(RemoteCatalogActionView {
                            label: "Cancel".to_owned(),
                            kind: RemoteCatalogActionKind::Cancel {
                                model_id: remote_id.to_owned(),
                            },
                            enabled: true,
                            disabled_reason: None,
                        });
                    } else if matches!(status, ModelInstallStatus::Error(_)) {
                        actions.push(install_action("Resume"));
                    }
                } else if let Some(installed_id) = installed_id {
                    status_label = Some("Installed and verified".to_owned());
                    actions.push(RemoteCatalogActionView {
                        label: "Use".to_owned(),
                        kind: RemoteCatalogActionKind::Use {
                            model_id: installed_id.clone(),
                        },
                        enabled: mutation_block_reason.is_none(),
                        disabled_reason: mutation_block_reason.clone(),
                    });
                    actions.push(RemoteCatalogActionView {
                        label: "Remove".to_owned(),
                        kind: RemoteCatalogActionKind::Remove {
                            model_id: installed_id,
                        },
                        enabled: mutation_block_reason.is_none(),
                        disabled_reason: mutation_block_reason.clone(),
                    });
                } else if previous_revision {
                    status_label = Some("Update available".to_owned());
                    actions.push(install_action("Install update"));
                } else if normalized_model_id.is_some() {
                    status_label = Some("Available in local catalog".to_owned());
                    actions.push(install_action("Install verified variant"));
                } else {
                    status_label = Some("Pinned GGUF".to_owned());
                    actions.push(install_action("Install"));
                }

                RemoteCatalogVariantView {
                    id: variant.id.clone(),
                    filename: variant.filename.clone(),
                    size_label: format_bytes(variant.size_bytes),
                    status_label,
                    expected_sha256: variant.expected_sha256.clone(),
                    normalized_model_id: normalized_model_id.map(|id| id.to_string()),
                    managed_model_id: remote_id,
                    size_bytes: variant.size_bytes,
                    size_tier: size_tier_for_bytes(variant.size_bytes),
                    speed_tier: speed_tier_for_bytes(variant.size_bytes),
                    actions,
                }
            })
            .collect();

        RemoteCatalogEntryView {
            id: model.id.clone(),
            display_name: model.display_name.clone(),
            description: model.description.clone(),
            languages: model.languages.clone(),
            language_summary: model.languages.join(", "),
            recommended: model.recommended,
            trust_label: model.trust.label().to_owned(),
            compatibility_detail: model.compatibility.detail().to_owned(),
            repository: model.id.clone(),
            pinned_revision: model.revision.clone(),
            variants,
        }
    }

    fn mark_comparison_output_changed(&mut self, model_id: &str) {
        let revision = self
            .comparison_output_revisions
            .entry(model_id.to_owned())
            .or_default();
        *revision = revision.wrapping_add(1);
        self.comparison_projection_cache.remove(model_id);
    }

    fn reset_comparison_output_projection(&mut self) {
        self.comparison_output_revisions.clear();
        self.comparison_projection_cache.clear();
    }

    fn set_comparison_reference(&mut self, reference: Option<String>) {
        if self.model_comparison.reference_transcript != reference {
            self.model_comparison.reference_transcript = reference;
            self.comparison_reference_revision = self.comparison_reference_revision.wrapping_add(1);
        }
    }

    fn sync_model_comparison_state(&mut self) {
        let Some(model_ids) = self.comparison_run_model_ids.clone() else {
            return;
        };
        let pending_model_ids = self
            .playground_runs
            .values()
            .flat_map(|run| run.pending_requests.values().cloned())
            .collect::<HashSet<_>>();
        self.model_comparison.recording_elapsed_ms = self
            .comparison_started_at
            .map(|started| u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX))
            .unwrap_or_default();
        self.model_comparison.audio_duration_ms = self
            .playground_cards
            .iter()
            .find_map(|card| card.audio_duration_ms)
            .and_then(|duration| u64::try_from(duration).ok());
        let results_rebuilt = self.model_comparison.results.len() != model_ids.len()
            || self
                .model_comparison
                .results
                .iter()
                .zip(&model_ids)
                .any(|((result_model_id, _), model_id)| result_model_id != model_id);
        if results_rebuilt {
            self.model_comparison.results = model_ids
                .iter()
                .cloned()
                .map(|model_id| (model_id, ComparisonResult::default()))
                .collect();
        }

        for (index, model_id) in model_ids.iter().enumerate() {
            let card = self
                .playground_cards
                .iter()
                .find(|card| card.descriptor.id.as_str() == model_id);
            let phase = match card.map(|card| &card.status) {
                Some(ModelRuntimeStatus::Running) => ComparisonResultPhase::Processing,
                Some(ModelRuntimeStatus::Error(_)) => ComparisonResultPhase::Error,
                Some(_) if card.and_then(|card| card.latency_ms).is_some() => {
                    ComparisonResultPhase::Complete
                }
                _ if pending_model_ids.contains(model_id) => ComparisonResultPhase::Pending,
                _ => ComparisonResultPhase::Pending,
            };
            let processing_ms = card
                .and_then(|card| card.latency_ms)
                .and_then(|duration| u64::try_from(duration).ok());
            let realtime_factor = processing_ms
                .zip(self.model_comparison.audio_duration_ms)
                .filter(|(_, audio)| *audio > 0)
                .map(|(processing, audio)| processing as f32 / audio as f32);
            let error = card.and_then(|card| match &card.status {
                ModelRuntimeStatus::Error(error) => Some(error.clone()),
                _ => None,
            });
            let output_revision = self
                .comparison_output_revisions
                .get(model_id)
                .copied()
                .unwrap_or_default();
            let cached = self.comparison_projection_cache.get(model_id).copied();
            let output_needs_replacement = results_rebuilt
                || cached.is_none_or(|cached| cached.output_revision != output_revision);
            let wer_needs_recompute = cached.is_none_or(|cached| {
                cached.output_revision != output_revision
                    || cached.reference_revision != self.comparison_reference_revision
            });
            let output_text = (output_needs_replacement || wer_needs_recompute)
                .then(|| {
                    card.map(|card| card.transcript.as_str())
                        .filter(|output| !output.trim().is_empty())
                })
                .flatten();
            let replacement = output_needs_replacement.then(|| output_text.map(str::to_owned));
            let word_error_rate = if wer_needs_recompute {
                let reference = self
                    .model_comparison
                    .reference_transcript
                    .as_deref()
                    .filter(|reference| !reference.trim().is_empty());
                let word_error_rate = reference
                    .zip(output_text)
                    .map(|(reference, output)| benchmark::calculate_wer(reference, output) as f32);
                #[cfg(test)]
                if reference.is_some() && output_text.is_some() {
                    self.comparison_wer_compute_count += 1;
                }
                self.comparison_projection_cache.insert(
                    model_id.clone(),
                    ComparisonProjectionCacheEntry {
                        reference_revision: self.comparison_reference_revision,
                        output_revision,
                        word_error_rate,
                    },
                );
                word_error_rate
            } else {
                cached.and_then(|cached| cached.word_error_rate)
            };

            let result = &mut self.model_comparison.results[index].1;
            if let Some(replacement) = replacement {
                result.output = replacement;
                #[cfg(test)]
                {
                    self.comparison_output_replacement_count += 1;
                }
            }
            result.phase = phase;
            result.processing_ms = processing_ms;
            result.realtime_factor = realtime_factor;
            result.word_error_rate = word_error_rate;
            result.error = error;
        }
        self.comparison_projection_cache
            .retain(|model_id, _| model_ids.contains(model_id));

        self.model_comparison.phase =
            if self.recording_source() == Some(RecordingSource::Playground) {
                ComparisonPhase::Recording
            } else if self.playground_pending > 0 {
                ComparisonPhase::Processing
            } else if self
                .model_comparison
                .results
                .iter()
                .any(|(_, result)| result.phase == ComparisonResultPhase::Complete)
            {
                ComparisonPhase::Complete
            } else if self.comparison_started_at.is_some() {
                ComparisonPhase::Error
            } else {
                ComparisonPhase::Idle
            };
    }

    fn comparison_start_block_reason(&self) -> Option<&'static str> {
        (self.pending_output.is_some()
            || matches!(
                self.session_coordinator.active_purpose(),
                Some(SessionPurpose::Dictation)
            ))
        .then_some("Finish the current dictation before starting a comparison.")
    }

    fn rebuild_model_inventory_projection(&mut self) {
        if !self.remote_catalog.local_models_dirty {
            return;
        }
        self.remote_catalog.local_models = self.build_model_management_catalog().into();
        self.remote_catalog.local_models_dirty = false;
        #[cfg(test)]
        {
            self.remote_catalog.local_models_build_count += 1;
        }
    }

    fn rebuild_local_models_after_committed_change(&mut self) {
        self.remote_catalog.invalidate_local_models();
        self.rebuild_model_inventory_projection();
    }

    #[cfg(test)]
    fn model_management_catalog(&self) -> Vec<ModelViewModel> {
        self.remote_catalog.local_models.to_vec()
    }

    fn build_model_management_catalog(&self) -> Vec<ModelViewModel> {
        let descriptors = self
            .transcription_service
            .model_descriptors()
            .into_iter()
            .map(|descriptor| (descriptor.id.as_str().to_owned(), descriptor))
            .collect::<HashMap<_, _>>();
        config::configured_models(&self.config)
            .into_iter()
            .filter_map(|model| {
                let effective_status = self.effective_install_status(&model);
                let artifact_present = model_artifact_remains_manageable(&model, &effective_status);
                let descriptor = descriptors.get(&model.id).cloned().or_else(|| {
                    // Retained compatibility models stay out of discovery, but an existing
                    // installed model must remain visible so it can be selected or removed.
                    artifact_present.then(|| {
                        self.transcription_service
                            .model_descriptor(&ModelId::new(&model.id))
                            .ok()
                    })?
                });
                (descriptor.is_some() || artifact_present)
                    .then(|| self.model_management_view_model(&model, descriptor.as_ref()))
            })
            .collect()
    }

    fn model_management_view_model(
        &self,
        model: &SttModelInfo,
        descriptor: Option<&ModelDescriptor>,
    ) -> ModelViewModel {
        let (display_name, variant_label) = model_ui_labels(model, descriptor);
        let install_status = self.effective_install_status(model);
        let runtime_ready =
            runtime_status_for_model(&self.config, model) == ModelRuntimeStatus::Ready;
        let installed = model_artifact_remains_manageable(model, &install_status);
        let custom = model.local_path.is_some()
            && !self.config.general.managed_models.contains_key(&model.id);
        let download_state = match &install_status {
            ModelInstallStatus::Installed => ModelDownloadState::Installed,
            ModelInstallStatus::Downloading { .. } => ModelDownloadState::Downloading,
            ModelInstallStatus::InstallingRuntime => ModelDownloadState::Verifying,
            ModelInstallStatus::Error(message) | ModelInstallStatus::RuntimeError(message)
                if message.contains("cancelled") || message.contains("Cancelled") =>
            {
                ModelDownloadState::Cancelled
            }
            ModelInstallStatus::Error(_)
            | ModelInstallStatus::RuntimeError(_)
            | ModelInstallStatus::Missing => ModelDownloadState::Failed,
            ModelInstallStatus::NotInstalled => ModelDownloadState::NotInstalled,
        };
        let (downloaded_bytes, total_bytes) = match &install_status {
            ModelInstallStatus::Downloading {
                downloaded_bytes,
                total_bytes,
                ..
            } => (*downloaded_bytes, *total_bytes),
            _ => (
                0,
                descriptor.map(|descriptor| descriptor.artifact_size_bytes),
            ),
        };
        let mutation_block_reason = self.artifact_mutation_block_reason();
        let mutation_blocked = mutation_block_reason.is_some();
        let active =
            installed && runtime_ready && self.config.general.selected_default_model == model.id;
        let (
            primary_action_label,
            primary_action_enabled,
            primary_action_repairs_runtime,
            primary_action_disabled_reason,
        ) = if active {
            (
                "Active".to_owned(),
                false,
                false,
                Some("This model is already active.".to_owned()),
            )
        } else if installed && runtime_ready {
            (
                "Use this model".to_owned(),
                !mutation_blocked,
                false,
                mutation_block_reason.clone(),
            )
        } else if installed {
            let runtime_busy = compatibility_bridge::provider_for_model(model)
                .is_some_and(|provider| self.runtime_jobs.contains_key(provider.id()));
            let runtime_action = runtime_action_state_with_activity(
                &self.config,
                model,
                runtime_busy,
                compatibility_bridge::provider_for_model(model)
                    .map_or_else(RuntimeConsumerActivity::default, |provider| {
                        self.runtime_consumer_activity(provider.id())
                    }),
            );
            let repairable = matches!(
                runtime_action.kind,
                RuntimeActionKind::Install | RuntimeActionKind::Update
            );
            (
                if repairable {
                    "Repair runtime"
                } else {
                    "Runtime unavailable"
                }
                .to_owned(),
                repairable && runtime_action.enabled && !mutation_blocked,
                repairable,
                mutation_block_reason
                    .clone()
                    .or(runtime_action.disabled_tooltip)
                    .or_else(|| {
                        (!repairable)
                            .then(|| "This model does not have a repairable runtime.".to_owned())
                    }),
            )
        } else {
            (
                "Not installed".to_owned(),
                false,
                false,
                Some("Install this model before using it.".to_owned()),
            )
        };
        ModelViewModel {
            id: model.id.clone(),
            display_name,
            variant_label,
            description: Some(descriptor.map_or_else(
                || model.description.clone(),
                |value| value.description.to_owned(),
            )),
            runtime_group: "Local speech runtime".to_owned(),
            installed,
            active,
            ready: installed && runtime_ready,
            recommended: descriptor.is_some_and(|descriptor| descriptor.recommended),
            custom,
            install_supported: supports_managed_install(model),
            install_action_enabled: !mutation_blocked
                && !installed
                && !matches!(
                    install_status,
                    ModelInstallStatus::Downloading { .. } | ModelInstallStatus::InstallingRuntime
                )
                && supports_managed_install(model),
            primary_action_label,
            primary_action_enabled,
            primary_action_repairs_runtime,
            primary_action_disabled_reason,
            cancel_supported: self.artifact_installations.contains_key(&model.id),
            removal_supported: !custom && supports_managed_uninstall(model, &install_status),
            download_state,
            downloaded_bytes,
            total_bytes,
            disk_bytes: installed
                .then(|| {
                    model
                        .local_path
                        .as_ref()
                        .and_then(|path| std::fs::metadata(path).ok())
                        .map(|metadata| metadata.len())
                })
                .flatten(),
            estimated_ram_bytes: descriptor
                .map_or(model.expected_ram.as_str(), |descriptor| {
                    descriptor.expected_ram
                })
                .trim_end_matches(" GB")
                .parse::<u64>()
                .ok()
                .map(|gigabytes| gigabytes * 1_000_000_000),
            languages: descriptor.map_or_else(Vec::new, |descriptor| {
                descriptor
                    .languages
                    .iter()
                    .map(|language| (*language).to_owned())
                    .collect()
            }),
            language_summary: descriptor.map_or_else(
                || "Not specified".to_owned(),
                |descriptor| descriptor.languages.join(", "),
            ),
            speed_tier: match descriptor.map_or(model.speed_tier.as_str(), |descriptor| {
                descriptor.speed_guidance
            }) {
                "Fastest" => ModelSpeedTier::VeryFast,
                "Fast" => ModelSpeedTier::Fast,
                "Medium" => ModelSpeedTier::Balanced,
                "Slower" => ModelSpeedTier::AccurateSlow,
                _ => ModelSpeedTier::Unknown,
            },
            size_tier: descriptor.map_or(ModelSizeTier::Unknown, |descriptor| {
                match descriptor.artifact_size_bytes {
                    0..=100_000_000 => ModelSizeTier::Tiny,
                    100_000_001..=200_000_000 => ModelSizeTier::Base,
                    200_000_001..=600_000_000 => ModelSizeTier::Small,
                    600_000_001..=1_000_000_000 => ModelSizeTier::Medium,
                    _ => ModelSizeTier::Large,
                }
            }),
            capabilities: descriptor.map_or_else(ModelCapabilities::default, |descriptor| {
                ModelCapabilities {
                    streaming_preview: descriptor.capabilities.native_streaming,
                    translation: descriptor.capabilities.translation,
                    timestamps: descriptor.capabilities.timestamps,
                    language_detection: descriptor.capabilities.language_detection,
                }
            }),
            compatibility: descriptor.map_or(ModelCompatibility::Incompatible, |descriptor| {
                match descriptor.compatibility {
                    CompatibilityStatus::Supported { .. } => ModelCompatibility::Supported,
                    CompatibilityStatus::Experimental { .. } => ModelCompatibility::Experimental,
                    CompatibilityStatus::Incompatible { .. } => ModelCompatibility::Incompatible,
                }
            }),
            error_message: match install_status {
                ModelInstallStatus::Error(message) | ModelInstallStatus::RuntimeError(message) => {
                    Some(message)
                }
                _ => None,
            },
            ..Default::default()
        }
        .normalize()
    }

    fn apply_model_management_action(&mut self, action: ScreenAction) {
        match action {
            ScreenAction::AddModel => {
                self.model_management.dialog = Some(ModelDialog::Add);
                self.model_management.focus_dialog_initial = true;
            }
            ScreenAction::ShowModelDetails(id) => {
                self.model_management.dialog = Some(ModelDialog::Details(id));
                self.model_management.focus_dialog_initial = true;
            }
            ScreenAction::RequestModelRemoval(id) => {
                self.model_management.dialog = Some(ModelDialog::Remove(id));
                self.model_management.focus_dialog_initial = true;
            }
            ScreenAction::CloseModelDialog => match self.model_management.dialog.take() {
                Some(ModelDialog::Add) => self.model_management.restore_add_focus = true,
                Some(ModelDialog::Details(id)) => {
                    self.model_management.restore_details_focus = Some(id)
                }
                Some(ModelDialog::Remove(id)) => {
                    self.model_management.restore_remove_focus = Some(id)
                }
                None => {}
            },
            ScreenAction::ToggleComparison => {
                self.model_comparison.expanded = !self.model_comparison.expanded
            }
            ScreenAction::ToggleComparisonModel(id) => {
                if matches!(
                    self.model_comparison.phase,
                    ComparisonPhase::Recording | ComparisonPhase::Processing
                ) {
                    self.model_comparison.selection_feedback =
                        Some("Model selection is locked during a comparison.".to_owned());
                } else if self.model_comparison.selected_model_ids.contains(&id) {
                    self.model_comparison.selected_model_ids.remove(&id);
                    self.model_comparison.selection_feedback = None;
                } else if self.model_comparison.selected_model_ids.len() >= 4 {
                    self.model_comparison.selection_feedback =
                        Some("A comparison can include at most four models.".to_owned());
                } else {
                    self.model_comparison.selected_model_ids.insert(id);
                    self.model_comparison.selection_feedback = None;
                }
            }
            ScreenAction::StartComparison => {
                if let Some(reason) = self.comparison_start_block_reason() {
                    self.model_comparison.start_disabled_reason = Some(reason.to_owned());
                    self.model_comparison.selection_feedback = Some(reason.to_owned());
                } else if !self.model_comparison.begin() {
                    self.model_comparison.selection_feedback = Some(
                        "Select two to four ready models before starting a comparison.".to_owned(),
                    );
                } else {
                    self.model_comparison.start_disabled_reason = None;
                    self.comparison_run_model_ids = Some(
                        self.model_comparison
                            .selected_model_ids
                            .iter()
                            .cloned()
                            .collect(),
                    );
                    self.comparison_started_at = Some(Instant::now());
                    self.start_recording(RecordingSource::Playground);
                    if self.recording_source() != Some(RecordingSource::Playground) {
                        self.model_comparison.phase = ComparisonPhase::Error;
                    }
                }
            }
            ScreenAction::StopComparison => {
                if self.recording_source() == Some(RecordingSource::Playground) {
                    self.stop_recording();
                }
            }
            ScreenAction::ShowComparisonReferenceEditor => {
                if let Some(reference) = self.model_comparison.reference_transcript.as_deref() {
                    self.model_comparison.reference_draft = reference.to_owned();
                }
                self.model_comparison.reference_editor_visible = true;
                self.model_comparison.focus_reference_editor = true;
                self.model_comparison.restore_reference_action_focus = false;
            }
            ScreenAction::HideComparisonReferenceEditor => {
                self.model_comparison.reference_draft = self
                    .model_comparison
                    .reference_transcript
                    .clone()
                    .unwrap_or_default();
                self.model_comparison.reference_editor_visible = false;
                self.model_comparison.focus_reference_editor = false;
                self.model_comparison.restore_reference_action_focus = true;
            }
            ScreenAction::EditComparisonReference(reference) => {
                self.model_comparison.reference_draft = reference;
            }
            ScreenAction::ApplyComparisonReference => {
                let reference = self.model_comparison.reference_draft.trim().to_owned();
                self.model_comparison.reference_draft = reference.clone();
                self.set_comparison_reference((!reference.is_empty()).then_some(reference));
                self.model_comparison.reference_editor_visible = false;
                self.model_comparison.focus_reference_editor = false;
                self.model_comparison.restore_reference_action_focus = true;
                self.model_comparison.reference_notice =
                    Some("Reference transcript applied.".to_owned());
                self.sync_model_comparison_state();
            }
            ScreenAction::ClearComparisonReference => {
                self.model_comparison.reference_draft.clear();
                self.set_comparison_reference(None);
                self.model_comparison.reference_editor_visible = false;
                self.model_comparison.focus_reference_editor = false;
                self.model_comparison.restore_reference_action_focus = true;
                self.model_comparison.reference_notice =
                    Some("Reference transcript cleared.".to_owned());
                self.sync_model_comparison_state();
            }
            ScreenAction::InstallModel(id) => {
                if let Some(model) = config::configured_models(&self.config)
                    .into_iter()
                    .find(|model| model.id == id)
                {
                    self.start_model_download(&model);
                }
            }
            ScreenAction::CancelModelInstall(id) => {
                if let Some((_, cancellation)) = self.artifact_installations.get(&id) {
                    cancellation.cancel();
                    self.status_message =
                        format!("Cancelling {id}. Downloaded partials will be kept for Resume.");
                }
            }
            ScreenAction::SelectModel(id) => {
                if let Some(model) = config::configured_models(&self.config)
                    .into_iter()
                    .find(|model| model.id == id)
                    && self.select_model_as_default(&model)
                {
                    self.model_management.dialog = None;
                    self.model_management.restore_details_focus = Some(id);
                }
            }
            ScreenAction::ConfirmModelRemoval(id) => {
                self.model_management.dialog = None;
                self.model_management.restore_after_removal_focus = true;
                if let Some(model) = config::configured_models(&self.config)
                    .into_iter()
                    .find(|model| model.id == id)
                {
                    self.uninstall_model(&model);
                }
            }
            ScreenAction::RepairModelRuntime(id) => {
                if let Some(model) = config::configured_models(&self.config)
                    .into_iter()
                    .find(|model| model.id == id)
                {
                    self.request_runtime_install(&model, RuntimeJobIntent::RepairModel(id));
                }
            }
            ScreenAction::MaintainModelRuntime(id) => {
                if let Some(model) = config::configured_models(&self.config)
                    .into_iter()
                    .find(|model| model.id == id)
                    && let Some(provider) = compatibility_bridge::provider_for_model(&model)
                {
                    let state = runtime_action_state_with_activity(
                        &self.config,
                        &model,
                        self.runtime_jobs.contains_key(provider.id()),
                        self.runtime_consumer_activity(provider.id()),
                    );
                    if state.enabled {
                        match state.kind {
                            RuntimeActionKind::Install | RuntimeActionKind::Update => {
                                self.request_runtime_install(&model, RuntimeJobIntent::Maintenance)
                            }
                            RuntimeActionKind::Uninstall => self.uninstall_runtime(&model),
                        }
                    }
                }
            }
            ScreenAction::SetLocalGgufImportPath(path) => self.model_import_path = path,
            ScreenAction::ValidateAndImportLocalGguf => self.start_local_gguf_import(),
            ScreenAction::CancelLocalGgufImport => {
                if let Some(job) = self.local_gguf_import.as_ref() {
                    job.cancellation.cancel();
                    self.status_message =
                        "Cancelling local GGUF validation; source bytes are unchanged.".to_owned();
                }
            }
            ScreenAction::SetRemoteCatalogQuery(query) => self.model_search = query,
            ScreenAction::SetModelLanguageFilter(filter) => self.model_language_filter = filter,
            ScreenAction::ToggleInstalledModels => {
                self.model_management.installed_expanded = !self.model_management.installed_expanded
            }
            ScreenAction::ToggleAvailableModels => {
                self.model_management.available_expanded = !self.model_management.available_expanded
            }
            ScreenAction::FocusModelCard(key) => {
                self.model_management.focus_model_card = Some(key);
            }
            ScreenAction::AcknowledgeModelCardFocus(key) => {
                if self.model_management.focus_model_card.as_ref() == Some(&key) {
                    self.model_management.focus_model_card = None;
                }
            }
            ScreenAction::AcknowledgeModelControlFocus { model_id, control } => {
                self.model_management
                    .acknowledge_control_focus(&model_id, control);
            }
            ScreenAction::RetryRemoteCatalog => {
                self.remote_catalog.force_refresh_requested = true;
            }
            ScreenAction::InstallRemoteCatalogVariant {
                remote_model_id,
                variant_id,
            } => {
                let action = self
                    .remote_catalog
                    .snapshot
                    .as_ref()
                    .and_then(|snapshot| {
                        snapshot
                            .models()
                            .iter()
                            .find(|model| model.id == remote_model_id)
                    })
                    .and_then(|model| {
                        let artifact = model.artifact_for(&variant_id)?;
                        let action = crate::model_catalog::normalized_model_id_for_pinned_artifact(
                            &artifact.model_id,
                            &artifact.revision,
                            &artifact.filename,
                        )
                        .map_or_else(
                            || {
                                RemoteModelCardAction::InstallTrusted(
                                    trusted_remote_install_request(model, &artifact),
                                )
                            },
                            RemoteModelCardAction::InstallNormalized,
                        );
                        Some(action)
                    });
                if let Some(action) = action {
                    self.apply_remote_model_card_action(action);
                } else {
                    self.status = TranscriptionStatus::Error;
                    self.status_message =
                        "The selected catalog variant is no longer in the validated snapshot. Refresh the catalog and try again."
                            .to_owned();
                }
            }
            ScreenAction::CancelRemoteCatalogInstall(model_id) => self
                .apply_remote_model_card_action(RemoteModelCardAction::CancelInstall(
                    ModelId::new(model_id),
                )),
            ScreenAction::UseRemoteCatalogModel(model_id) => self.apply_remote_model_card_action(
                RemoteModelCardAction::SelectInstalled(ModelId::new(model_id)),
            ),
            ScreenAction::RemoveRemoteCatalogModel(model_id) => self
                .apply_remote_model_card_action(RemoteModelCardAction::RemoveInstalled(
                    ModelId::new(model_id),
                )),
            ScreenAction::None => {}
            other => self.apply_transcribe_screen_action(other),
        }
        self.model_management.mutation_block_reason = self.artifact_mutation_block_reason();
        self.rebuild_model_inventory_projection();
    }

    fn apply_remote_model_card_action(&mut self, action: RemoteModelCardAction) {
        match action {
            RemoteModelCardAction::InstallNormalized(model_id) => {
                if self.artifact_installations.contains_key(model_id.as_str()) {
                    self.status_message =
                        "That verified model variant is already being installed.".to_owned();
                } else if let Some(model) = config::configured_models(&self.config)
                    .into_iter()
                    .find(|model| model.id == model_id.as_str())
                {
                    self.start_model_download(&model);
                } else {
                    self.status = TranscriptionStatus::Error;
                    self.status_message =
                        "The selected catalog variant is no longer available in Scribe's verified local catalog. Refresh and try again."
                            .to_owned();
                }
            }
            RemoteModelCardAction::InstallTrusted(request) => {
                self.start_trusted_remote_model_download(request);
            }
            RemoteModelCardAction::CancelInstall(model_id) => {
                if let Some((_, cancellation)) = self.artifact_installations.get(model_id.as_str())
                {
                    cancellation.cancel();
                    self.status_message =
                        "Cancelling verified model installation. Downloaded partial bytes will be retained for Resume."
                            .to_owned();
                }
            }
            RemoteModelCardAction::SelectInstalled(model_id) => {
                if let Some(model) = config::configured_models(&self.config)
                    .into_iter()
                    .find(|model| model.id == model_id.as_str())
                {
                    self.select_model_as_default(&model);
                }
            }
            RemoteModelCardAction::RemoveInstalled(model_id) => {
                if let Some(model) = config::configured_models(&self.config)
                    .into_iter()
                    .find(|model| model.id == model_id.as_str())
                {
                    self.uninstall_model(&model);
                }
            }
        }
    }

    fn ui_playground(&mut self, ui: &mut Ui) {
        let status = self.effective_status();
        let status_message = self.status_message.clone();
        page(ui, "Model Playground", status, &status_message, |ui| {
            if self.current_tab == Tab::Models {
                if ui.add(small_button(ui, "Back to models")).clicked() {
                    self.models_show_comparison = false;
                }
                ui.add_space(12.0);
            }
            panel(ui, |ui| {
                let run_blocked = self.playground_run_block_reason();
                let selector_busy = self.playground_selector_busy();
                ui.horizontal_wrapped(|ui| {
                    let text = if self.capture_is_active() {
                        "Stop Recording"
                    } else {
                        "Start Test Recording"
                    };
                    let recording_button = add_enabled_button(
                        ui,
                        self.capture_is_active() || run_blocked.is_none(),
                        primary_small_button(ui, text),
                        run_blocked.as_deref(),
                    )
                    .on_hover_text("Record one audio sample and run every selected ready model.");
                    if recording_button.clicked() {
                        if self.capture_is_active() {
                            self.stop_recording();
                        } else {
                            self.comparison_run_model_ids = None;
                            self.comparison_started_at = None;
                            self.start_recording(RecordingSource::Playground);
                        }
                    }
                    if ui.add(small_button(ui, "Clear Results")).clicked() {
                        self.clear_playground_results(true);
                    }
                    let selector_button = ui
                        .push_id("playground-selector-header-control", |ui| {
                            add_enabled_button(
                                ui,
                                !selector_busy,
                                small_button(ui, "Choose models to test"),
                                Some("Finish the current Playground recording or transcription before changing its model selection."),
                            )
                        })
                        .inner
                        .on_hover_text(
                            "Choose which installed models participate in Playground tests.",
                        );
                    self.playground_selector_header_focus = Some(selector_button.id);
                    if selector_button.clicked() {
                        self.open_playground_selector(Some(selector_button.id));
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        badge(
                            ui,
                            &format!("{} selected", self.config.general.playground_selected_models.len()),
                            ChipTone::Neutral,
                        );
                    });
                });
                if let Some(active) = &self.active_recording {
                    ui.add_space(8.0);
                    ui.add(
                        egui::ProgressBar::new(
                            (active.started_at.elapsed().as_secs_f32()
                                / active.max_duration_seconds.max(1) as f32)
                                .clamp(0.0, 1.0),
                        )
                        .desired_width(260.0)
                        .text(recording_timer_text(active)),
                    );
                }
                if self.active_recording.is_none()
                    && let Some(reason) = run_blocked
                {
                    ui.add_space(6.0);
                    ui.label(mut_text(&reason));
                }
                if selector_busy {
                    ui.add_space(4.0);
                    ui.label(mut_text(
                        "Finish the current Playground recording or transcription before changing models.",
                    ));
                }
            });

            ui.add_space(12.0);
            panel(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.vertical(|ui| {
                        ui.label(section_heading("Benchmark Reference"));
                        if let Some((model_name, _)) = self.active_playground_output() {
                            ui.horizontal_wrapped(|ui| {
                                badge(ui, "Active model truth", ChipTone::Active);
                                badge(ui, &model_name, ChipTone::Neutral);
                            });
                        } else {
                            ui.horizontal_wrapped(|ui| {
                                badge(ui, "Waiting for active model", ChipTone::Warning);
                                if let Some(model) = self.selected_model() {
                                    badge(ui, &model.name, ChipTone::Neutral);
                                }
                            });
                        }
                    });
                    ui.with_layout(Layout::right_to_left(Align::TOP), |ui| {
                        if ui.add(small_button(ui, "Use Active Output")).clicked()
                            && !self.apply_active_playground_output_as_reference()
                        {
                            self.status_message =
                                "Run the active model before using its output.".to_owned();
                        }
                    });
                });
                ui.add_space(8.0);
                if ui
                    .add(
                        TextEdit::multiline(&mut self.playground_reference_transcript)
                            .desired_rows(4)
                            .desired_width(usable_width(ui))
                            .hint_text("Reference transcript"),
                    )
                    .changed()
                {
                    self.playground_reference_user_edited = true;
                }
                ui.add_space(8.0);
                ui.horizontal_wrapped(|ui| {
                    ui.label(label_caps("Ranking"));
                    ComboBox::from_id_source("playground-ranking-mode")
                        .selected_text(self.playground_ranking_mode.label())
                        .width(130.0)
                        .show_ui(ui, |ui| {
                            for mode in RankingMode::ALL {
                                ui.selectable_value(
                                    &mut self.playground_ranking_mode,
                                    mode,
                                    mode.label(),
                                );
                            }
                        });
                    let completed = self
                        .playground_cards
                        .iter()
                        .filter(|card| !card.transcript.trim().is_empty())
                        .count();
                    badge(ui, &format!("{completed} completed"), ChipTone::Neutral);
                });
            });

            let benchmark_results = self.playground_benchmark_results();
            if !benchmark_results.is_empty() {
                ui.add_space(12.0);
                benchmark_grid_ui(
                    ui,
                    &benchmark_results,
                    self.playground_ranking_mode,
                    &self.config.general.selected_default_model,
                );
            }

            ui.add_space(12.0);
            ui.horizontal_wrapped(|ui| {
                ui.vertical(|ui| {
                    ui.label(section_heading("Selected Models"));
                    ui.label(mut_text(
                        "Performance comparison based on current system hardware.",
                    ));
                });
            });
            ui.add_space(8.0);
            let mut pending_actions = Vec::new();
            if self.playground_cards.is_empty() {
                panel(ui, |ui| {
                    ui.label(mut_text(
                        "No installed models are selected for Playground tests.",
                    ));
                    let selector_busy = self.playground_selector_busy();
                    let choose_models = ui
                        .add_enabled(!selector_busy, primary_small_button(ui, "Choose Models"))
                        .on_hover_text("Select installed models to compare in the Playground.");
                    if choose_models.clicked() {
                        self.open_playground_selector(Some(choose_models.id));
                    }
                    if selector_busy {
                        ui.label(mut_text(
                            "Finish active Playground work before changing models.",
                        ));
                    }
                });
            }
            let card_count = self.playground_cards.len();
            for (card_index, card_state) in self.playground_cards.iter_mut().enumerate() {
                let model_id = card_state.descriptor.id.as_str().to_owned();
                let is_active_model = model_id == self.config.general.selected_default_model;
                let drag_id = ui.id().with(("playground-card", &model_id));
                let outer_width = usable_width(ui);
                let (inner, dropped_payload) = ui
                    .scope(|ui| {
                        configure_drop_zone_feedback(ui);
                        ui.allocate_ui_with_layout(
                            Vec2::new(outer_width, 0.0),
                            Layout::top_down(Align::LEFT),
                            |ui| {
                                set_exact_width(ui, outer_width);
                                ui.dnd_drop_zone::<String, _>(drop_zone_frame(), |ui| {
                                    full_width_frame(
                                        ui,
                                        model_card_frame(ui, is_active_model),
                                        |ui| {
                                            playground_card_ui(
                                                ui,
                                                card_state,
                                                is_active_model,
                                                drag_id,
                                                card_index > 0,
                                                card_index + 1 < card_count,
                                            )
                                        },
                                    )
                                    .inner
                                })
                            },
                        )
                        .inner
                    })
                    .inner;
                pending_actions.extend(inner.inner);

                if let Some(dragged_id) = dropped_payload {
                    pending_actions.push(PlaygroundAction::MoveBefore {
                        dragged_id: dragged_id.to_string(),
                        target_id: model_id,
                    });
                }
                ui.add_space(4.0);
            }

            if let Some(action) = pending_actions.into_iter().next() {
                self.apply_playground_action(action);
            }

            ui.add_space(8.0);
            info_panel(ui, |ui| {
                ui.label(section_heading("Testing Environment"));
                ui.label(mut_text("Performance numbers are calculated locally. Duration includes model loading, inference, and post-processing for the current recording."));
                ui.add_space(6.0);
                ui.horizontal_wrapped(|ui| {
                    badge(ui, "Local execution", ChipTone::Success);
                    badge(ui, "No cloud STT", ChipTone::Neutral);
                    badge(ui, "Model load included", ChipTone::Neutral);
                });
            });
        });
        self.ui_playground_model_selector(ui.ctx());
    }

    fn ui_playground_model_selector(&mut self, ctx: &egui::Context) {
        let Some(mut draft) = self.playground_selector_draft.take() else {
            return;
        };
        let busy = self.playground_selector_busy();
        let descriptors = self
            .transcription_service
            .model_descriptors()
            .into_iter()
            .map(|descriptor| (descriptor.id.as_str().to_owned(), descriptor))
            .collect::<HashMap<_, _>>();
        let installed_models = config::configured_models(&self.config)
            .into_iter()
            .filter(|model| model.install_status.is_runnable())
            .filter_map(|model| {
                let descriptor = descriptors.get(&model.id)?.clone();
                Some((model, descriptor))
            })
            .collect::<Vec<_>>();
        let request_initial_focus =
            std::mem::take(&mut self.playground_selector_needs_initial_focus);
        let mut open = true;
        let mut apply = false;
        let mut cancel = false;
        let screen_rect = ctx.screen_rect();
        egui::Area::new(egui::Id::new("playground-selector-shield"))
            .order(egui::Order::Background)
            .fixed_pos(screen_rect.min)
            .movable(false)
            .show(ctx, |ui| {
                let shield_rect = egui::Rect::from_min_size(egui::Pos2::ZERO, screen_rect.size());
                ui.allocate_rect(shield_rect, egui::Sense::click_and_drag());
                ui.painter().rect_filled(
                    shield_rect,
                    Rounding::ZERO,
                    Color32::from_black_alpha(72),
                );
            });
        egui::Window::new("Choose models to test")
            .collapsible(false)
            .resizable(true)
            .default_width(480.0)
            .min_width(432.0)
            .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
            .open(&mut open)
            .show(ctx, |ui| {
                ui.label(mut_text(
                    "Only installed models can be selected for Playground tests.",
                ));
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    let select_all =
                        ui.add_enabled(
                            !busy && !installed_models.is_empty(),
                            small_button(ui, "Select all installed models"),
                        );
                    if request_initial_focus {
                        select_all.request_focus();
                    }
                    if select_all.clicked() {
                        draft = installed_models
                            .iter()
                            .map(|(model, _)| model.id.clone())
                            .collect();
                    }
                    if ui
                        .add_enabled(!busy, small_button(ui, "Clear selected models"))
                        .clicked()
                    {
                        draft.clear();
                    }
                });
                ui.add_space(8.0);
                if installed_models.is_empty() {
                    ui.label(mut_text(
                        "No installed models yet. Install a model from Models, then return here to select it.",
                    ));
                } else {
                    egui::ScrollArea::vertical()
                        .max_height(320.0)
                        .show(ui, |ui| {
                            for (model, descriptor) in &installed_models {
                                ui.horizontal_wrapped(|ui| {
                                    let mut selected = draft.iter().any(|id| id == &model.id);
                                    let readiness = runtime_status_for_model(&self.config, model);
                                    let checkbox = ui.add_enabled(
                                        !busy,
                                        egui::Checkbox::new(
                                            &mut selected,
                                            descriptor.display_name,
                                        ),
                                    );
                                    checkbox.widget_info(|| {
                                        let mut info = egui::WidgetInfo::selected(
                                            egui::WidgetType::Checkbox,
                                            selected,
                                            format!(
                                                "{}; compatibility {}; readiness {}",
                                                descriptor.display_name,
                                                descriptor.compatibility.label(),
                                                readiness
                                            ),
                                        );
                                        info.enabled = !busy;
                                        info
                                    });
                                    checkbox.on_hover_text(
                                        "Include this model in the next Playground test.",
                                    );
                                    if selected {
                                        if !draft.iter().any(|id| id == &model.id) {
                                            draft.push(model.id.clone());
                                        }
                                    } else {
                                        draft.retain(|id| id != &model.id);
                                    }
                                    badge(
                                        ui,
                                        descriptor.compatibility.label(),
                                        ChipTone::Warning,
                                    );
                                    badge(
                                        ui,
                                        &readiness.to_string(),
                                        runtime_chip_tone(&readiness),
                                    );
                                });
                            }
                        });
                }
                ui.add_space(10.0);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui
                        .add_enabled(!busy, primary_small_button(ui, "Apply model selection"))
                        .clicked()
                    {
                        apply = true;
                    }
                    if ui.add(small_button(ui, "Cancel model selection")).clicked() {
                        cancel = true;
                    }
                });
            });
        if ctx.input(|input| input.key_pressed(egui::Key::Escape)) {
            cancel = true;
        }
        if apply {
            self.playground_selector_draft = Some(draft);
            self.apply_playground_selector(ctx);
        } else if open && !cancel {
            self.playground_selector_draft = Some(draft);
        } else {
            self.close_playground_selector(ctx);
        }
    }

    fn settings_diagnostics(&self) -> Vec<String> {
        let mut diagnostics = self
            .latest_latency
            .as_ref()
            .map(LatencyTrace::summary_lines)
            .unwrap_or_else(|| vec!["No completed session latency is available yet.".to_owned()]);
        if self.tray_service.is_none() {
            diagnostics.push("Tray integration is unavailable in this desktop session.".to_owned());
        }
        if let Some(notice) = text_output::paste_automation_notice() {
            diagnostics.push(notice.to_owned());
        }
        diagnostics
    }

    fn selected_model_ui_label(&self) -> String {
        let Some(model) = self.selected_model() else {
            return "No model selected".to_owned();
        };
        let descriptor = self
            .transcription_service
            .model_descriptor(&ModelId::new(&model.id))
            .ok();
        model_ui_labels(&model, descriptor.as_ref()).0
    }

    fn ui_general_settings(&mut self, ui: &mut Ui) {
        let (selected_model_id, model_readiness) = self.selected_model_screen_state();
        let (levels, level_revision, level_source_active) = self.current_sensitivity_level_sample();
        let input_level_percent = (self.microphone_level_envelope.update(
            levels.rms,
            level_revision,
            level_source_active,
            Instant::now(),
        ) * 100.0)
            .round() as u8;
        let no_speech = self.status_message == "No speech detected; nothing was pasted.";
        let state = transcription_state(
            self.effective_status(),
            selected_model_id,
            model_readiness,
            self.pending_recording.is_some(),
            no_speech,
            0,
            self.transcript.clone(),
            String::new(),
            if no_speech {
                Some("No speech detected — nothing was added.".to_owned())
            } else {
                (!self.status_message.is_empty()).then(|| self.status_message.clone())
            },
            self.config.recording.hotkey.clone(),
            recording_mode(self.config.recording.hotkey_mode == HotkeyMode::HoldToTalk),
            self.microphone_permission(),
        );
        let settings = RecordingSettingsView {
            close_to_tray: self.config.general.close_to_tray,
            duration_seconds: self.config.recording.max_recording_seconds,
            duration_label: format!("{} seconds", self.config.recording.max_recording_seconds),
            provisional_feedback: self.config.streaming.mode != StreamingMode::FinalOnly,
            selected_audio_device: self.config.recording.audio_input_device_name.clone(),
            audio_devices: self.audio_devices.clone(),
            device_label: self
                .config
                .recording
                .audio_input_device_name
                .clone()
                .unwrap_or_else(|| "OS default".to_owned()),
            input_sensitivity_percent: (rms_to_slider_position(
                self.config.recording.manual_activation_rms,
            ) * 100.0)
                .round() as u8,
            input_level_percent,
            microphone_error: self.microphone_test_error.clone(),
            auto_insert_transcript: self.config.output.auto_insert_transcript,
            output_label: if cfg!(target_os = "windows") {
                "Insert final transcript into captured app".to_owned()
            } else {
                "Copy final transcript to clipboard automatically".to_owned()
            },
            show_restore_clipboard: cfg!(target_os = "windows"),
            output_notice: self
                .config
                .output
                .auto_insert_transcript
                .then(text_output::paste_automation_notice)
                .flatten()
                .map(str::to_owned),
            restore_clipboard_after_insert: self.config.output.restore_clipboard_after_insert,
            paste_delay_ms: self.config.output.paste_delay_ms,
            active_model_label: self.selected_model_ui_label(),
            hotkey_input: self.hotkey_input.clone(),
            hotkey_capture_active: self.capturing_hotkey,
            hotkey_capture_status: self.capturing_hotkey.then(|| self.status_message.clone()),
            theme_label: self.config.general.theme_mode.label().to_owned(),
            overlay_label: self.config.overlay.mode.label().to_owned(),
            overlay_available: overlay::overlay_focus_safety_available(),
            vad_enabled: self.config.recording.vad_enabled,
            speech_confirmation_ms: self.config.recording.speech_confirmation_ms,
            internal_pause_ms: self.config.recording.internal_pause_ms,
            endpoint_silence_ms: self.config.recording.endpoint_silence_ms,
            pre_roll_ms: self.config.recording.pre_roll_ms,
            post_roll_ms: self.config.recording.post_roll_ms,
            streaming_label: self.config.streaming.mode.label().to_owned(),
            acceleration_label: self
                .config
                .performance
                .acceleration_preference
                .label()
                .to_owned(),
            gpu_available: self
                .transcription_service
                .model_descriptor(&ModelId::new(&self.config.general.selected_default_model))
                .is_ok_and(|descriptor| descriptor.capabilities.gpu),
            overlay_position_label: self.config.overlay.position.label().to_owned(),
            debug_mode: self.config.developer.debug_mode,
            history_mode_label: self.config.history.mode.label().to_owned(),
            history_locked: self.history_retry_is_active(),
            max_history_entries: self.config.history.max_unpinned_entries,
            transcript_retention_days: self.config.history.transcript_retention_days,
            audio_retention_days: self.config.history.audio_retention_days,
            store_application_identity: self.config.history.store_application_identity,
            diagnostics: self.settings_diagnostics(),
            save_state: settings_save_state(
                self.settings_store
                    .as_ref()
                    .is_some_and(SettingsStore::has_pending),
                self.effective_status() == TranscriptionStatus::Error
                    && self.status_message.starts_with("Failed to save settings:"),
            ),
        };
        let comparison = Default::default();
        let action = render_screen(
            ui,
            &ScreenView {
                route: UiRoute::Settings(self.settings_tab),
                transcription: &state,
                models: &[],
                model_catalog: &[],
                comparison: &comparison,
                model_management: &Default::default(),
                model_language_filter: ModelLanguageFilter::default(),
                remote_catalog: &Default::default(),
                recording_settings: &settings,
            },
        );
        self.apply_settings_screen_action(action);
    }

    fn open_system_audio_settings(&mut self) {
        match launch_system_audio_settings() {
            Ok(()) => {
                self.status_message = "Opened system microphone settings".to_owned();
            }
            Err(error) => {
                self.settings_tab = SettingsTab::Recording;
                self.current_tab = Tab::General;
                self.status_message = format!("Could not open system microphone settings: {error}");
            }
        }
    }

    fn apply_settings_screen_action(&mut self, action: ScreenAction) {
        match action {
            ScreenAction::SetSettingsTab(tab) => self.settings_tab = tab,
            ScreenAction::SetCloseToTray(value) => {
                self.config.general.close_to_tray = value;
                self.save_config();
            }
            ScreenAction::OpenModelSettings => self.current_tab = Tab::Models,
            ScreenAction::SetHotkeyInput(value) => self.hotkey_input = value,
            ScreenAction::ApplyHotkey => self.apply_hotkey(),
            ScreenAction::SetTheme(value) => {
                self.config.general.theme_mode = match value.as_str() {
                    "Light" => ThemeMode::Light,
                    "Dark" => ThemeMode::Dark,
                    "System" => ThemeMode::System,
                    _ => return,
                };
                self.save_config();
            }
            ScreenAction::SetOverlayMode(value) => {
                self.config.overlay.mode = match value.as_str() {
                    "Live" => OverlayMode::Live,
                    "Minimal" => OverlayMode::Minimal,
                    "Off" => OverlayMode::Off,
                    _ => return,
                };
                self.save_config();
            }
            ScreenAction::SetRecordingMode(mode) => {
                self.config.recording.hotkey_mode = match mode {
                    RecordingMode::PressOnce => HotkeyMode::Toggle,
                    RecordingMode::Hold => HotkeyMode::HoldToTalk,
                };
                self.save_config();
            }
            ScreenAction::SetDurationSeconds(seconds) => {
                self.config.recording.max_recording_seconds =
                    seconds.clamp(1, config::MAX_RECORDING_SECONDS);
                self.save_config();
            }
            ScreenAction::ToggleProvisionalFeedback => {
                self.config.streaming.mode =
                    if self.config.streaming.mode == StreamingMode::FinalOnly {
                        StreamingMode::Auto
                    } else {
                        StreamingMode::FinalOnly
                    };
                self.save_config();
            }
            ScreenAction::SetAudioDevice(device) => {
                self.config.recording.audio_input_device_name = device;
                self.stop_microphone_test();
                self.microphone_monitor_retry_required = false;
                self.microphone_level_envelope.reset_source();
                self.save_config();
            }
            ScreenAction::SetInputSensitivity(percent) => {
                self.config.recording.manual_activation_rms =
                    dbfs_to_rms(slider_position_to_dbfs(f32::from(percent) / 100.0));
                self.apply_input_sensitivity_threshold();
                self.save_config();
            }
            ScreenAction::RefreshDevices => self.refresh_audio_devices(),
            ScreenAction::RetryMicrophone => {
                self.microphone_test_error = None;
                self.microphone_monitor_retry_required = false;
                self.ensure_microphone_monitor();
            }
            ScreenAction::OpenAudioSettings => self.open_system_audio_settings(),
            ScreenAction::ChangeShortcut => {
                self.capturing_hotkey = !self.capturing_hotkey;
                self.status_message = if self.capturing_hotkey {
                    "Press the new hotkey combination. Press Capture again to cancel.".to_owned()
                } else {
                    "Hotkey capture cancelled.".to_owned()
                };
            }
            ScreenAction::SetAutoInsertTranscript(value) => {
                self.config.output.auto_insert_transcript = value;
                self.save_config();
            }
            ScreenAction::SetRestoreClipboardAfterInsert(value) => {
                self.config.output.restore_clipboard_after_insert = value;
                self.save_config();
            }
            ScreenAction::SetPasteDelayMs(value) => {
                self.config.output.paste_delay_ms = value.clamp(1, 1_000);
                self.save_config();
            }
            ScreenAction::SetVadEnabled(value) => {
                self.config.recording.vad_enabled = value;
                self.save_config();
            }
            ScreenAction::SetSpeechConfirmationMs(value) => {
                self.config.recording.speech_confirmation_ms = value.clamp(50, 1_000);
                self.config.recording.internal_pause_ms = self
                    .config
                    .recording
                    .internal_pause_ms
                    .max(self.config.recording.speech_confirmation_ms);
                self.save_config();
            }
            ScreenAction::SetInternalPauseMs(value) => {
                self.config.recording.internal_pause_ms =
                    value.clamp(self.config.recording.speech_confirmation_ms, 3_000);
                self.config.recording.endpoint_silence_ms = self
                    .config
                    .recording
                    .endpoint_silence_ms
                    .max(self.config.recording.internal_pause_ms);
                self.save_config();
            }
            ScreenAction::SetEndpointSilenceMs(value) => {
                self.config.recording.endpoint_silence_ms =
                    value.clamp(self.config.recording.internal_pause_ms, 5_000);
                self.save_config();
            }
            ScreenAction::SetPreRollMs(value) => {
                self.config.recording.pre_roll_ms = value.min(2_000);
                self.save_config();
            }
            ScreenAction::SetPostRollMs(value) => {
                self.config.recording.post_roll_ms = value.min(2_000);
                self.save_config();
            }
            ScreenAction::SetStreamingMode(value) => {
                self.config.streaming.mode = match value.as_str() {
                    "Auto" => StreamingMode::Auto,
                    "Rolling preview" => StreamingMode::Rolling,
                    "Final text only" => StreamingMode::FinalOnly,
                    _ => return,
                };
                self.save_config();
            }
            ScreenAction::SetAcceleration(value) => {
                self.config.performance.acceleration_preference = match value.as_str() {
                    "Auto" => AccelerationPreference::Auto,
                    "GPU" => AccelerationPreference::Gpu,
                    "CPU only" => AccelerationPreference::Cpu,
                    _ => return,
                };
                self.save_config();
            }
            ScreenAction::SetOverlayPosition(value) => {
                self.config.overlay.position = match value.as_str() {
                    "Top" => OverlayPosition::Top,
                    "Bottom" => OverlayPosition::Bottom,
                    _ => return,
                };
                self.save_config();
            }
            ScreenAction::SetDebugMode(value) => {
                self.config.developer.debug_mode = value;
                if !value && self.current_tab == Tab::Debug {
                    self.settings_tab = SettingsTab::Advanced;
                    self.current_tab = Tab::General;
                }
                self.save_config();
            }
            ScreenAction::SetHistoryMode(value) => {
                if !self.history_retry_is_active() {
                    self.config.history.mode = match value.as_str() {
                        "Off" => HistoryMode::Off,
                        "Transcript only" => HistoryMode::TranscriptOnly,
                        "Transcript and audio" => HistoryMode::TranscriptAndAudio,
                        _ => return,
                    };
                    self.save_history_config();
                }
            }
            ScreenAction::SetMaxHistoryEntries(value) => {
                if !self.history_retry_is_active() {
                    self.config.history.max_unpinned_entries =
                        value.clamp(1, config::MAX_HISTORY_ENTRIES);
                    self.save_history_config();
                }
            }
            ScreenAction::SetTranscriptRetentionDays(value) => {
                if !self.history_retry_is_active() {
                    self.config.history.transcript_retention_days =
                        value.map(|days| days.clamp(1, config::MAX_HISTORY_RETENTION_DAYS));
                    self.save_history_config();
                }
            }
            ScreenAction::SetAudioRetentionDays(value) => {
                if !self.history_retry_is_active() {
                    self.config.history.audio_retention_days =
                        value.map(|days| days.clamp(1, config::MAX_HISTORY_RETENTION_DAYS));
                    self.save_history_config();
                }
            }
            ScreenAction::SetStoreApplicationIdentity(value) => {
                if !self.history_retry_is_active() {
                    self.config.history.store_application_identity = value;
                    self.save_history_config();
                }
            }
            ScreenAction::None
            | ScreenAction::SelectModel(_)
            | ScreenAction::InstallModel(_)
            | ScreenAction::CancelModelInstall(_)
            | ScreenAction::ShowModelDetails(_)
            | ScreenAction::RequestModelRemoval(_)
            | ScreenAction::ConfirmModelRemoval(_)
            | ScreenAction::CloseModelDialog
            | ScreenAction::AddModel
            | ScreenAction::ChangeModel
            | ScreenAction::StartRecording
            | ScreenAction::StopRecording
            | ScreenAction::ClearTranscript
            | ScreenAction::CopyTranscript
            | ScreenAction::ToggleComparison
            | ScreenAction::ToggleComparisonModel(_)
            | ScreenAction::StartComparison
            | ScreenAction::StopComparison
            | ScreenAction::ShowComparisonReferenceEditor
            | ScreenAction::HideComparisonReferenceEditor
            | ScreenAction::EditComparisonReference(_)
            | ScreenAction::ApplyComparisonReference
            | ScreenAction::ClearComparisonReference
            | ScreenAction::SetRemoteCatalogQuery(_)
            | ScreenAction::SetModelLanguageFilter(_)
            | ScreenAction::ToggleInstalledModels
            | ScreenAction::ToggleAvailableModels
            | ScreenAction::FocusModelCard(_)
            | ScreenAction::AcknowledgeModelCardFocus(_)
            | ScreenAction::AcknowledgeModelControlFocus { .. }
            | ScreenAction::RetryRemoteCatalog
            | ScreenAction::InstallRemoteCatalogVariant { .. }
            | ScreenAction::CancelRemoteCatalogInstall(_)
            | ScreenAction::UseRemoteCatalogModel(_)
            | ScreenAction::RemoveRemoteCatalogModel(_) => {}
            ScreenAction::RepairModelRuntime(_)
            | ScreenAction::MaintainModelRuntime(_)
            | ScreenAction::SetLocalGgufImportPath(_)
            | ScreenAction::ValidateAndImportLocalGguf
            | ScreenAction::CancelLocalGgufImport => {}
        }
    }

    fn ui_history(&mut self, ui: &mut Ui) {
        let status = self.effective_status();
        let status_message = self.status_message.clone();
        let has_more = self.history_next.is_some();
        let work_active = self.has_active_work();
        let playing = self.playing_history_id;
        let armed = self.armed_history_repaste.as_ref().map(|armed| armed.id);
        let focus_search = self.history_search_focus_pending;
        let focus_delete_confirmation = self.history_confirmation_focus_pending;
        let mut action = None;
        page(ui, "History", status, &status_message, |ui| {
            action = history_page(
                ui,
                HistoryPageState {
                    search: &mut self.history_search,
                    records: &self.history_records,
                    has_more,
                    loading: self.history_loading,
                    error: self.history_error.as_deref(),
                    confirm_delete: self.history_delete_confirmation,
                    work_active,
                    playing,
                    playback_stopping: self.history_playback_stopping,
                    armed_repaste: armed,
                    focus_search,
                    focus_delete_confirmation,
                },
            );
        });
        self.history_search_focus_pending = false;
        self.history_confirmation_focus_pending = false;
        if let Some(action) = action {
            self.apply_history_action(action);
        }
    }

    fn ui_about(&mut self, ui: &mut Ui) {
        let status = self.effective_status();
        let status_message = self.status_message.clone();
        let model_dir = config::model_storage_dir(&self.config);
        let config_path = self.config_path.clone();
        let export_dir = config_path
            .as_deref()
            .and_then(Path::parent)
            .map(|parent| parent.join("diagnostics"));
        let session_count = self.diagnostics.len();
        let mut export_requested = false;
        page(ui, "About", status, &status_message, |ui| {
            about_page(ui, &model_dir, config_path.as_deref());
            ui.add_space(12.0);
            ui.group(|ui| {
                semantic_heading(ui, section_heading("Redacted diagnostics"));
                ui.label(format!(
                    "{session_count} recent session snapshot(s) are held in memory. Exports exclude transcript and audio content, secrets, filesystem paths, and raw errors."
                ));
                let unavailable_reason = export_dir.is_none().then_some(
                    "The platform settings directory is unavailable, so Scribe cannot choose a private export location.",
                );
                let button = ui.add_enabled(
                    export_dir.is_some(),
                    Button::new("Export redacted diagnostics").min_size(Vec2::new(220.0, 44.0)),
                );
                if let Some(reason) = unavailable_reason {
                    ui.ctx().accesskit_node_builder(button.id, |builder| {
                        builder.set_description(reason);
                    });
                    ui.label(mut_text(reason));
                }
                export_requested = button.clicked();
            });
        });
        if export_requested && let Some(directory) = export_dir {
            match diagnostics::export_redacted(&directory, &self.diagnostics) {
                Ok(path) => {
                    self.status_message =
                        format!("Redacted diagnostics exported to {}", path.display());
                }
                Err(error) => {
                    self.status = TranscriptionStatus::Error;
                    self.status_message = format!("Could not export redacted diagnostics: {error}");
                }
            }
        }
    }
}

const PLAYGROUND_RESULT_HEIGHT: f32 = 92.0;

#[derive(Clone, Copy)]
enum ChipTone {
    Neutral,
    Success,
    Warning,
    Error,
    Active,
}

fn configure_stitch_style(ctx: &egui::Context) {
    configure_accessible_style(ctx);
}

fn paint_viewport_background(ctx: &egui::Context) {
    let colors = theme_palette(ctx);
    ctx.layer_painter(egui::LayerId::background()).rect_filled(
        ctx.screen_rect(),
        0.0,
        colors.content_bg,
    );
}

fn page(
    ui: &mut Ui,
    title: &str,
    status: TranscriptionStatus,
    status_message: &str,
    add_contents: impl FnOnce(&mut Ui),
) {
    let page_width = usable_width(ui);
    ui.allocate_ui_with_layout(
        Vec2::new(page_width, 0.0),
        Layout::top_down(Align::LEFT),
        |ui| {
            set_exact_width(ui, page_width);
            ui.horizontal_top(|ui| {
                let heading = ui.label(
                    RichText::new(title)
                        .font(FontId::proportional(24.0))
                        .color(ui_palette(ui).primary)
                        .strong(),
                );
                ui.ctx().accesskit_node_builder(heading.id, |builder| {
                    builder.set_role(egui::accesskit::Role::Heading);
                    builder.set_bounds(egui::accesskit::Rect {
                        x0: heading.rect.min.x.into(),
                        y0: heading.rect.min.y.into(),
                        x1: heading.rect.max.x.into(),
                        y1: heading.rect.max.y.into(),
                    });
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    status_badge(ui, status);
                });
            });
            if !status_message.trim().is_empty() {
                ui.add_space(2.0);
                let response = ui.add(
                    egui::Label::new(mut_text(status_message))
                        .wrap(true)
                        .sense(egui::Sense::hover()),
                );
                ui.ctx().accesskit_node_builder(response.id, |builder| {
                    builder.set_live(egui::accesskit::Live::Polite);
                });
            }
            ui.add_space(14.0);
            with_usable_width_cap(ui, page_width, |ui| {
                set_exact_width(ui, page_width);
                add_contents(ui);
            });
        },
    );
}

fn content_panel_frame(ctx: &egui::Context) -> Frame {
    let colors = theme_palette(ctx);
    Frame::none().fill(colors.content_bg)
}

fn card(ui: &mut Ui, add_contents: impl FnOnce(&mut Ui)) {
    full_width_frame(ui, card_frame(ui), add_contents);
}

fn card_frame(ui: &Ui) -> Frame {
    let colors = ui_palette(ui);
    Frame::none()
        .fill(colors.card_bg)
        .stroke(Stroke::new(1.0, colors.border))
        .rounding(Rounding::same(6.0))
        .inner_margin(Margin::same(14.0))
}

fn model_card_frame(ui: &Ui, selected: bool) -> Frame {
    let colors = ui_palette(ui);
    let fill = if selected {
        colors.active_card_bg
    } else {
        colors.card_bg
    };
    let stroke = if selected {
        Stroke::new(1.5, colors.accent)
    } else {
        Stroke::new(1.0, colors.border)
    };
    Frame::none()
        .fill(fill)
        .stroke(stroke)
        .rounding(Rounding::same(6.0))
        .inner_margin(Margin::same(14.0))
}

fn configure_drop_zone_feedback(ui: &mut Ui) {
    let colors = ui_palette(ui);
    let transparent = Color32::from_rgba_unmultiplied(0, 0, 0, 0);
    let transparent_stroke = Stroke::new(0.0, transparent);
    let widgets = &mut ui.visuals_mut().widgets;
    widgets.inactive.bg_fill = transparent;
    widgets.inactive.bg_stroke = transparent_stroke;
    widgets.active.bg_fill = colors.active_card_bg;
    widgets.active.bg_stroke = Stroke::new(1.5, colors.accent);
}

fn drop_zone_frame() -> Frame {
    Frame::none()
        .rounding(Rounding::same(8.0))
        .inner_margin(Margin::same(3.0))
}

fn panel(ui: &mut Ui, add_contents: impl FnOnce(&mut Ui)) {
    card(ui, add_contents);
}

fn info_panel(ui: &mut Ui, add_contents: impl FnOnce(&mut Ui)) {
    let colors = ui_palette(ui);
    full_width_frame(
        ui,
        Frame::none()
            .fill(colors.panel_bg)
            .stroke(Stroke::new(1.0, colors.border))
            .rounding(Rounding::same(6.0))
            .inner_margin(Margin::same(14.0)),
        add_contents,
    );
}

fn remote_model_matches_search(model: &RemoteModel, search: &str) -> bool {
    search.is_empty()
        || model.display_name.to_ascii_lowercase().contains(search)
        || model.description.to_ascii_lowercase().contains(search)
        || model.id.to_ascii_lowercase().contains(search)
        || model
            .languages
            .iter()
            .any(|language| language.to_ascii_lowercase().contains(search))
        || model
            .variants
            .iter()
            .any(|variant| variant.filename.to_ascii_lowercase().contains(search))
}

fn remote_model_smallest_variant_size(model: &RemoteModel) -> Option<u64> {
    model
        .variants
        .iter()
        .map(|variant| variant.size_bytes)
        .min()
}

fn remote_model_is_installed(model: &RemoteModel, app_config: &AppConfig) -> bool {
    model.variants.iter().any(|variant| {
        config::managed_remote_model_id(&model.id, &model.revision, &variant.filename).is_some_and(
            |model_id| {
                app_config
                    .general
                    .managed_remote_models
                    .contains_key(&model_id)
            },
        )
    })
}

fn remote_model_is_multilingual(model: &RemoteModel) -> bool {
    let unique_languages = model
        .languages
        .iter()
        .map(|language| language.trim().to_ascii_lowercase())
        .filter(|language| !language.is_empty())
        .collect::<HashSet<_>>();
    unique_languages.len() > 1
        || unique_languages
            .iter()
            .any(|language| language == "multilingual")
}

fn remote_model_matches_catalog_filters(
    model: &RemoteModel,
    app_config: &AppConfig,
    filters: RemoteCatalogFilters,
) -> bool {
    (!filters.installed_only || remote_model_is_installed(model, app_config))
        && (!filters.recommended_only || model.recommended)
        && (!filters.multilingual_only || remote_model_is_multilingual(model))
        && filters
            .size_tier
            .matches(remote_model_smallest_variant_size(model))
}

fn filtered_remote_models<'model>(
    models: &'model [RemoteModel],
    app_config: &AppConfig,
    search: &str,
    filters: RemoteCatalogFilters,
    sort: RemoteCatalogSort,
    language_filter: ModelLanguageFilter,
) -> Vec<&'model RemoteModel> {
    let mut matching = models
        .iter()
        .filter(|model| remote_model_matches_search(model, search))
        .filter(|model| remote_model_matches_catalog_filters(model, app_config, filters))
        .filter(|model| language_filter.matches(&model.languages))
        .collect::<Vec<_>>();

    matching.sort_by(|left, right| match sort {
        RemoteCatalogSort::Recommended => right
            .recommended
            .cmp(&left.recommended)
            .then_with(|| left.display_name.cmp(&right.display_name))
            .then_with(|| left.id.cmp(&right.id)),
        RemoteCatalogSort::Smallest => remote_model_smallest_variant_size(left)
            .unwrap_or(u64::MAX)
            .cmp(&remote_model_smallest_variant_size(right).unwrap_or(u64::MAX))
            .then_with(|| left.display_name.cmp(&right.display_name))
            .then_with(|| left.id.cmp(&right.id)),
        RemoteCatalogSort::Largest => remote_model_smallest_variant_size(right)
            .unwrap_or(0)
            .cmp(&remote_model_smallest_variant_size(left).unwrap_or(0))
            .then_with(|| left.display_name.cmp(&right.display_name))
            .then_with(|| left.id.cmp(&right.id)),
        RemoteCatalogSort::Name => left
            .display_name
            .cmp(&right.display_name)
            .then_with(|| left.id.cmp(&right.id)),
    });
    matching
}

fn size_tier_for_bytes(size_bytes: u64) -> ModelSizeTier {
    const MIB: u64 = 1024 * 1024;
    match size_bytes / MIB {
        0..=256 => ModelSizeTier::Tiny,
        257..=512 => ModelSizeTier::Small,
        513..=1024 => ModelSizeTier::Base,
        1025..=2048 => ModelSizeTier::Medium,
        _ => ModelSizeTier::Large,
    }
}

fn speed_tier_for_bytes(size_bytes: u64) -> ModelSpeedTier {
    match size_tier_for_bytes(size_bytes) {
        ModelSizeTier::Tiny => ModelSpeedTier::VeryFast,
        ModelSizeTier::Small => ModelSpeedTier::Fast,
        ModelSizeTier::Base => ModelSpeedTier::Balanced,
        ModelSizeTier::Medium | ModelSizeTier::Large => ModelSpeedTier::AccurateSlow,
        ModelSizeTier::Unknown => ModelSpeedTier::Unknown,
    }
}

fn disk_space_preflight_error(
    preflight: Result<crate::disk_space::DiskSpacePreflight, InstallError>,
) -> Option<String> {
    match preflight {
        Ok(preflight) if preflight.has_sufficient_space() => None,
        Ok(preflight) => Some(format!(
            "Install disabled: {} free is required on {} (including Scribe's {} safety headroom); only {} is available.",
            format_bytes(preflight.required_bytes),
            preflight.volume,
            format_bytes(crate::disk_space::SAFETY_HEADROOM_BYTES),
            format_bytes(preflight.available_bytes),
        )),
        Err(error) => Some(format!(
            "Install disabled because Scribe could not safely verify available disk space: {error}"
        )),
    }
}

fn normalized_model_install_space_error(config: &AppConfig, model_id: &ModelId) -> Option<String> {
    disk_space_preflight_error(
        managed_downloads::normalized_model_download_space_preflight(config, model_id),
    )
}

fn trusted_model_install_space_error(
    config: &AppConfig,
    artifact: &TrustedArtifact,
) -> Option<String> {
    disk_space_preflight_error(managed_downloads::trusted_gguf_download_space_preflight(
        config, artifact,
    ))
}

fn trusted_remote_install_request(
    model: &RemoteModel,
    artifact: &TrustedArtifact,
) -> TrustedRemoteInstallRequest {
    TrustedRemoteInstallRequest {
        artifact: artifact.clone(),
        display_name: model.display_name.clone(),
        description: model.description.clone(),
        languages: model.languages.clone(),
        recommended: model.recommended,
    }
}

fn playground_drag_handle(
    ui: &mut Ui,
    drag_id: egui::Id,
    payload: String,
    model_name: &str,
    active_model: bool,
) {
    let colors = ui_palette(ui);
    let dragging = ui.ctx().is_being_dragged(drag_id);
    let size = Vec2::new(ui.available_width().max(24.0), 34.0);
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    let response = ui
        .interact(rect, drag_id, egui::Sense::drag())
        .on_hover_cursor(egui::CursorIcon::Grab)
        .on_hover_text("Drag to reorder this model");
    response.dnd_set_drag_payload(payload);

    let fill = if dragging || response.hovered() {
        colors.active_card_bg
    } else if active_model {
        colors.panel_bg
    } else {
        colors.card_bg
    };
    let stroke = if dragging {
        Stroke::new(1.5, colors.accent)
    } else {
        Stroke::new(1.0, colors.border)
    };
    ui.painter().rect_filled(rect, Rounding::same(5.0), fill);
    ui.painter().rect_stroke(rect, Rounding::same(5.0), stroke);
    for row in 0..3 {
        for col in 0..2 {
            let offset = Vec2::new((col as f32 - 0.5) * 7.0, (row as f32 - 1.0) * 7.0);
            ui.painter()
                .circle_filled(rect.center() + offset, 1.6, colors.muted_text);
        }
    }

    if dragging {
        paint_playground_drag_preview(ui.ctx(), model_name);
    }
}

fn paint_playground_drag_preview(ctx: &egui::Context, model_name: &str) {
    let Some(pointer_pos) = ctx.pointer_interact_pos() else {
        return;
    };
    let colors = theme_palette(ctx);
    let rect =
        egui::Rect::from_min_size(pointer_pos + Vec2::new(14.0, 14.0), Vec2::new(230.0, 34.0));
    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Tooltip,
        egui::Id::new("playground-drag-preview"),
    ));

    painter.rect_filled(rect, Rounding::same(6.0), colors.card_bg);
    painter.rect_stroke(rect, Rounding::same(6.0), Stroke::new(1.0, colors.accent));
    painter.text(
        rect.left_center() + Vec2::new(12.0, 0.0),
        egui::Align2::LEFT_CENTER,
        drag_preview_label(model_name),
        FontId::proportional(13.0),
        colors.text,
    );
}

fn drag_preview_label(model_name: &str) -> String {
    const MAX_CHARS: usize = 30;
    let mut chars = model_name.chars();
    let preview = chars.by_ref().take(MAX_CHARS).collect::<String>();
    if chars.next().is_some() {
        format!("{preview}...")
    } else {
        preview
    }
}

fn playground_card_ui(
    ui: &mut Ui,
    card_state: &mut PlaygroundCardState,
    is_active_model: bool,
    drag_id: egui::Id,
    can_move_up: bool,
    can_move_down: bool,
) -> Vec<PlaygroundAction> {
    let mut actions = Vec::new();

    ui.scope(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        let move_width = 28.0;
        let actions_width = 92.0;
        let gap = 12.0;
        let detail_width = (ui.available_width() - move_width - actions_width - gap * 2.0).max(0.0);
        ui.horizontal_top(|ui| {
            ui.allocate_ui_with_layout(
                Vec2::new(move_width, 0.0),
                Layout::top_down(Align::LEFT),
                |ui| {
                    set_exact_width(ui, move_width);
                    playground_drag_handle(
                        ui,
                        drag_id,
                        card_state.descriptor.id.as_str().to_owned(),
                        card_state.descriptor.display_name,
                        is_active_model,
                    );
                },
            );
            ui.add_space(gap);
            ui.allocate_ui_with_layout(
                Vec2::new(detail_width, 0.0),
                Layout::top_down(Align::LEFT),
                |ui| {
                    set_exact_width(ui, detail_width);
                    wrapped_label(
                        ui,
                        card_title(ui, card_state.descriptor.display_name, is_active_model),
                    );
                    wrapped_label(ui, mut_text(card_state.descriptor.description));
                    ui.add_space(8.0);
                    tag_row(ui, |ui| {
                        badge(
                            ui,
                            if card_state.descriptor.capabilities.gpu {
                                "CPU / GPU eligible"
                            } else {
                                "CPU verified path"
                            },
                            ChipTone::Neutral,
                        );
                        badge(
                            ui,
                            &card_state.install_status.label(),
                            install_chip_tone(&card_state.install_status),
                        );
                        badge(
                            ui,
                            &format!("{} speed", card_state.descriptor.speed_guidance),
                            ChipTone::Neutral,
                        );
                        badge(
                            ui,
                            card_state.descriptor.compatibility.label(),
                            ChipTone::Warning,
                        );
                    });
                },
            );
            ui.add_space(gap);
            ui.allocate_ui_with_layout(
                Vec2::new(actions_width, 0.0),
                Layout::top_down(Align::RIGHT),
                |ui| {
                    set_exact_width(ui, actions_width);
                    let move_up = ui.add_enabled(can_move_up, small_button(ui, "Move up"));
                    move_up.widget_info(|| {
                        let mut info = egui::WidgetInfo::labeled(
                            egui::WidgetType::Button,
                            format!("Move {} up", card_state.descriptor.display_name),
                        );
                        info.enabled = can_move_up;
                        info
                    });
                    if move_up.clicked() {
                        actions.push(PlaygroundAction::MoveBy {
                            model_id: card_state.descriptor.id.as_str().to_owned(),
                            offset: -1,
                        });
                    }
                    let move_down = ui.add_enabled(can_move_down, small_button(ui, "Move down"));
                    move_down.widget_info(|| {
                        let mut info = egui::WidgetInfo::labeled(
                            egui::WidgetType::Button,
                            format!("Move {} down", card_state.descriptor.display_name),
                        );
                        info.enabled = can_move_down;
                        info
                    });
                    if move_down.clicked() {
                        actions.push(PlaygroundAction::MoveBy {
                            model_id: card_state.descriptor.id.as_str().to_owned(),
                            offset: 1,
                        });
                    }
                    let clear = ui.add(small_button(ui, "Clear"));
                    scroll_focused_control_into_view(ui, &clear);
                    if clear.clicked() {
                        actions.push(PlaygroundAction::Clear(
                            card_state.descriptor.id.as_str().to_owned(),
                        ));
                    }
                    badge(
                        ui,
                        &card_state.status.to_string(),
                        runtime_chip_tone(&card_state.status),
                    );
                },
            );
        });
    });

    if let Some(latency) = card_state.latency_ms {
        ui.label(RichText::new(format!("Latency: {latency} ms")).weak());
    }

    ui.add_space(6.0);
    playground_result_editor(
        ui,
        card_state.descriptor.id.as_str(),
        &card_state.transcript,
    );

    actions
}

fn playground_result_editor(ui: &mut Ui, result_id: &str, transcript: &str) -> egui::Response {
    let colors = ui_palette(ui);
    let width = usable_width(ui).max(0.0);
    let (rect, response) = ui.allocate_exact_size(
        Vec2::new(width, PLAYGROUND_RESULT_HEIGHT),
        egui::Sense::hover(),
    );

    ui.painter()
        .rect_filled(rect, Rounding::same(4.0), colors.card_bg);
    ui.painter()
        .rect_stroke(rect, Rounding::same(4.0), Stroke::new(1.0, colors.border));

    let content_rect = rect.shrink2(Vec2::new(10.0, 8.0));
    let mut content_ui = ui.child_ui(content_rect, Layout::top_down(Align::LEFT));
    content_ui.set_clip_rect(content_rect);
    ScrollArea::vertical()
        .id_source(("playground-result", result_id))
        .max_height(content_rect.height())
        .auto_shrink([false, false])
        .show(&mut content_ui, |ui| {
            set_exact_width(ui, content_rect.width());
            if transcript.trim().is_empty() {
                ui.label(mut_text("Result"));
            } else {
                ui.label(RichText::new(transcript));
            }
        });

    if transcript.trim().is_empty() {
        response
    } else {
        response.on_hover_text(transcript)
    }
}

fn benchmark_grid_ui(
    ui: &mut Ui,
    results: &[BenchmarkModelResult],
    ranking_mode: RankingMode,
    active_model_id: &str,
) {
    let mut rows = results.iter().collect::<Vec<_>>();
    rows.sort_by(|left, right| {
        let left_score = left
            .overall_scores
            .get(&ranking_mode)
            .copied()
            .unwrap_or(-1.0);
        let right_score = right
            .overall_scores
            .get(&ranking_mode)
            .copied()
            .unwrap_or(-1.0);
        right_score
            .partial_cmp(&left_score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let best_score = rows
        .iter()
        .filter_map(|result| result.overall_scores.get(&ranking_mode).copied())
        .reduce(f64::max);
    let metrics = visible_benchmark_metrics(results);

    panel(ui, |ui| {
        ui.horizontal_wrapped(|ui| {
            ui.vertical(|ui| {
                semantic_heading(ui, section_heading("Benchmark Scores"));
                ui.label(mut_text(
                    "Raw metric values are colored by relative score in this run.",
                ));
            });
            ui.with_layout(Layout::right_to_left(Align::TOP), |ui| {
                badge(ui, ranking_mode.label(), ChipTone::Active);
            });
        });
        ui.add_space(10.0);
        ScrollArea::horizontal()
            .id_source("benchmark-grid-scroll")
            .auto_shrink([false, true])
            .show(ui, |ui| {
                egui::Grid::new("benchmark-grid")
                    .striped(true)
                    .num_columns(metrics.len() + 2)
                    .spacing(Vec2::new(8.0, 6.0))
                    .show(ui, |ui| {
                        ui.label(label_caps("Model"));
                        let overall_header = ui.label(label_caps("Overall")).on_hover_text(
                            "Weighted normalized score for the selected ranking mode.",
                        );
                        ui.ctx()
                            .accesskit_node_builder(overall_header.id, |builder| {
                                builder.set_description(
                                    "Weighted normalized score for the selected ranking mode.",
                                );
                            });
                        for metric in &metrics {
                            let header = ui
                                .label(label_caps(metric.header()))
                                .on_hover_text(metric.tooltip());
                            ui.ctx().accesskit_node_builder(header.id, |builder| {
                                builder.set_description(metric.tooltip());
                            });
                        }
                        ui.end_row();

                        for result in rows {
                            let overall = result.overall_scores.get(&ranking_mode).copied();
                            let is_best = best_score
                                .zip(overall)
                                .is_some_and(|(best, score)| (best - score).abs() <= 0.0001);
                            benchmark_model_cell(ui, result, active_model_id, is_best);
                            benchmark_score_cell(
                                ui,
                                benchmark::format_overall_score(overall),
                                overall,
                                &format!(
                                    "{}, overall {} score",
                                    result.model_name,
                                    ranking_mode.label()
                                ),
                            );
                            for metric in &metrics {
                                let value = result.raw_metrics.value(*metric);
                                let score = result.normalized_scores.get(metric).copied();
                                benchmark_score_cell(
                                    ui,
                                    benchmark::format_metric_value(*metric, value),
                                    score,
                                    &format!(
                                        "{}, {}. {}",
                                        result.model_name,
                                        metric.header(),
                                        metric.tooltip()
                                    ),
                                );
                            }
                            ui.end_row();
                        }
                    });
            });
    });
}

fn visible_benchmark_metrics(results: &[BenchmarkModelResult]) -> Vec<BenchmarkMetric> {
    [
        BenchmarkMetric::Wer,
        BenchmarkMetric::Cer,
        BenchmarkMetric::Wip,
        BenchmarkMetric::Wil,
        BenchmarkMetric::Latency,
        BenchmarkMetric::Rtf,
        BenchmarkMetric::Ram,
        BenchmarkMetric::Vram,
    ]
    .into_iter()
    .filter(|metric| {
        matches!(
            metric,
            BenchmarkMetric::Wer
                | BenchmarkMetric::Cer
                | BenchmarkMetric::Wip
                | BenchmarkMetric::Wil
        ) || results
            .iter()
            .any(|result| result.raw_metrics.value(*metric).is_some())
    })
    .collect()
}

fn benchmark_model_cell(
    ui: &mut Ui,
    result: &BenchmarkModelResult,
    active_model_id: &str,
    is_best: bool,
) {
    ui.vertical(|ui| {
        ui.set_min_width(190.0);
        wrapped_label(ui, body_strong(&result.model_name));
        ui.horizontal_wrapped(|ui| {
            if result.model_id == active_model_id {
                badge(ui, "Reference", ChipTone::Active);
            }
            if is_best {
                badge(ui, "Best", ChipTone::Success);
            }
        });
    });
}

fn benchmark_score_cell(
    ui: &mut Ui,
    label: String,
    score: Option<f64>,
    accessible_description: &str,
) {
    let colors = ui_palette(ui);
    Frame::none()
        .fill(benchmark_heatmap_fill(ui, score))
        .stroke(Stroke::new(1.0, colors.border))
        .rounding(Rounding::same(4.0))
        .inner_margin(Margin::symmetric(8.0, 5.0))
        .show(ui, |ui| {
            ui.set_min_width(68.0);
            let value = ui.label(RichText::new(label).strong().color(colors.text));
            ui.ctx().accesskit_node_builder(value.id, |builder| {
                builder.set_description(accessible_description);
            });
        });
}

fn benchmark_heatmap_fill(ui: &Ui, score: Option<f64>) -> Color32 {
    let Some(score) = score else {
        return ui_palette(ui).panel_bg;
    };
    benchmark_heatmap_fill_for_mode(ui.visuals().dark_mode, score)
}

fn benchmark_heatmap_fill_for_mode(dark_mode: bool, score: f64) -> Color32 {
    let score = score.clamp(0.0, 1.0);
    let (low, middle, high) = if dark_mode {
        (
            Color32::from_rgb(78, 32, 40),
            Color32::from_rgb(80, 65, 22),
            Color32::from_rgb(25, 72, 48),
        )
    } else {
        (
            Color32::from_rgb(254, 226, 226),
            Color32::from_rgb(254, 249, 195),
            Color32::from_rgb(220, 252, 231),
        )
    };
    if score < 0.5 {
        let t = score / 0.5;
        lerp_color(low, middle, t)
    } else {
        let t = (score - 0.5) / 0.5;
        lerp_color(middle, high, t)
    }
}

fn lerp_color(start: Color32, end: Color32, t: f64) -> Color32 {
    let t = t.clamp(0.0, 1.0) as f32;
    let mix = |a: u8, b: u8| -> u8 { (a as f32 + (b as f32 - a as f32) * t).round() as u8 };
    Color32::from_rgb(
        mix(start.r(), end.r()),
        mix(start.g(), end.g()),
        mix(start.b(), end.b()),
    )
}

fn usable_width(ui: &Ui) -> f32 {
    let mut width = None;
    if let Some(cap) = ui.data(|data| data.get_temp::<f32>(usable_width_cap_id()))
        && cap.is_finite()
        && cap > 0.0
    {
        width = Some(cap);
    }
    let cursor_x = ui.next_widget_position().x;
    let available_rect = ui.available_rect_before_wrap();
    let min_rect = ui.min_rect();
    let screen_rect = ui.ctx().screen_rect();
    for candidate in [
        ui.available_width(),
        ui.max_rect().width(),
        min_rect.width(),
        ui.clip_rect().width(),
        screen_rect.width(),
        ui.available_rect_before_wrap().max.x - cursor_x,
        min_rect.max.x - cursor_x,
        ui.max_rect().max.x - cursor_x,
        ui.clip_rect().max.x - cursor_x,
        screen_rect.max.x - cursor_x,
        ui.clip_rect().max.x - available_rect.min.x,
        screen_rect.max.x - available_rect.min.x,
    ] {
        if candidate.is_finite() && candidate > 0.0 {
            width = Some(width.map_or(candidate, |current: f32| current.min(candidate)));
        }
    }
    width.unwrap_or(0.0)
}

fn with_usable_width_cap<R>(ui: &mut Ui, width: f32, add_contents: impl FnOnce(&mut Ui) -> R) -> R {
    let id = usable_width_cap_id();
    let previous = ui.data(|data| data.get_temp::<f32>(id));
    ui.data_mut(|data| data.insert_temp(id, width));
    let result = add_contents(ui);
    ui.data_mut(|data| {
        if let Some(previous) = previous {
            data.insert_temp(id, previous);
        } else {
            data.remove_temp::<f32>(id);
        }
    });
    result
}

fn usable_width_cap_id() -> egui::Id {
    egui::Id::new("scribe-usable-width-cap")
}

fn set_exact_width(ui: &mut Ui, width: f32) {
    if width.is_finite() && width > 0.0 {
        ui.set_width(width);
    }
}

fn full_width_frame<R>(
    ui: &mut Ui,
    frame: Frame,
    add_contents: impl FnOnce(&mut Ui) -> R,
) -> egui::InnerResponse<R> {
    let outer_width = usable_width(ui);
    let margins = frame.inner_margin + frame.outer_margin;
    let inner_width = (outer_width - margins.sum().x).max(0.0);
    let outer_min = ui.next_widget_position();
    let outer_seed = egui::Rect::from_min_size(outer_min, Vec2::new(outer_width, 0.0));
    let mut inner_seed = margins.shrink_rect(outer_seed);
    inner_seed.max.x = inner_seed.max.x.max(inner_seed.min.x);
    inner_seed.max.y = inner_seed.max.y.max(inner_seed.min.y);

    let background_idx = ui.painter().add(egui::Shape::Noop);
    let mut content_ui = ui.child_ui(inner_seed, Layout::top_down(Align::LEFT));
    let horizontal_clip = egui::Rect::from_min_max(
        egui::pos2(outer_min.x, ui.clip_rect().min.y),
        egui::pos2(outer_min.x + outer_width, ui.clip_rect().max.y),
    );
    content_ui.set_clip_rect(ui.clip_rect().intersect(horizontal_clip));
    set_exact_width(&mut content_ui, inner_width);
    let inner = add_contents(&mut content_ui);

    let content_height = content_ui.min_rect().height().max(0.0);
    let fixed_content_rect =
        egui::Rect::from_min_size(inner_seed.min, Vec2::new(inner_width, content_height));
    let paint_rect = frame.inner_margin.expand_rect(fixed_content_rect);
    let allocated_rect = margins.expand_rect(fixed_content_rect);
    if ui.is_rect_visible(paint_rect) {
        ui.painter().set(background_idx, frame.paint(paint_rect));
    }
    let response = ui.allocate_rect(allocated_rect, egui::Sense::hover());

    egui::InnerResponse::new(inner, response)
}

fn primary_small_button<'a>(ui: &Ui, label: &'a str) -> Button<'a> {
    let colors = ui_palette(ui);
    Button::new(
        RichText::new(label)
            .color(colors.primary_button_text)
            .strong(),
    )
    .fill(colors.primary_button_bg)
    .stroke(Stroke::new(1.0, colors.primary_button_bg))
    .rounding(Rounding::same(5.0))
    .min_size(Vec2::new(72.0, minimum_primary_target_height()))
}

fn small_button<'a>(ui: &Ui, label: &'a str) -> Button<'a> {
    let colors = ui_palette(ui);
    Button::new(RichText::new(label).color(colors.text))
        .fill(colors.card_bg)
        .stroke(Stroke::new(1.0, colors.border_strong))
        .rounding(Rounding::same(5.0))
        .min_size(Vec2::new(68.0, minimum_primary_target_height()))
}

fn add_enabled_button<'a>(
    ui: &mut Ui,
    enabled: bool,
    button: Button<'a>,
    disabled_tooltip: Option<&str>,
) -> egui::Response {
    let response = ui.add_enabled(enabled, button);
    if enabled {
        response
    } else if let Some(disabled_tooltip) = disabled_tooltip {
        response.on_disabled_hover_text(disabled_tooltip)
    } else {
        response
    }
}

fn tag_row(ui: &mut Ui, add_contents: impl FnOnce(&mut Ui)) {
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = Vec2::new(7.0, 6.0);
        add_contents(ui);
    });
}

fn status_badge(ui: &mut Ui, status: TranscriptionStatus) {
    let tone = match status {
        TranscriptionStatus::Idle => ChipTone::Success,
        TranscriptionStatus::Listening => ChipTone::Active,
        TranscriptionStatus::Transcribing => ChipTone::Warning,
        TranscriptionStatus::Error => ChipTone::Error,
    };
    badge(ui, &status.to_string(), tone);
}

fn badge(ui: &mut Ui, label: &str, tone: ChipTone) {
    let (text, fill, stroke) = chip_colors(ui, tone);
    Frame::none()
        .fill(fill)
        .stroke(stroke)
        .rounding(Rounding::same(4.0))
        .inner_margin(Margin::symmetric(8.0, 4.0))
        .show(ui, |ui| {
            ui.label(RichText::new(label).size(12.0).color(text).strong());
        })
        .response
        .on_hover_text(label);
}

fn label_caps(label: impl Into<String>) -> RichText {
    RichText::new(label.into().to_ascii_uppercase())
        .size(11.0)
        .weak()
        .strong()
}

fn section_heading(label: &str) -> RichText {
    RichText::new(label)
        .font(FontId::proportional(16.0))
        .strong()
}

fn semantic_heading(ui: &mut Ui, text: RichText) -> egui::Response {
    let response = ui.label(text);
    ui.ctx().accesskit_node_builder(response.id, |builder| {
        builder.set_role(egui::accesskit::Role::Heading);
    });
    response
}

fn body_strong(label: &str) -> RichText {
    RichText::new(label)
        .font(FontId::proportional(15.0))
        .strong()
}

fn card_title(ui: &Ui, label: &str, active: bool) -> RichText {
    let text = RichText::new(label)
        .font(FontId::proportional(15.0))
        .strong();
    if active {
        text.color(ui_palette(ui).accent)
    } else {
        text
    }
}

fn mut_text(label: impl Into<String>) -> RichText {
    RichText::new(label.into()).weak()
}

fn wrapped_label(ui: &mut Ui, text: RichText) {
    ui.add(egui::Label::new(text).wrap(true));
}

fn chip_colors(ui: &Ui, tone: ChipTone) -> (Color32, Color32, Stroke) {
    let colors = ui_palette(ui);
    match tone {
        ChipTone::Neutral => (
            colors.muted_text,
            colors.panel_bg,
            Stroke::new(1.0, colors.border),
        ),
        ChipTone::Success if ui.visuals().dark_mode => (
            colors.success,
            Color32::from_rgb(20, 61, 42),
            Stroke::new(1.0, Color32::from_rgb(34, 105, 70)),
        ),
        ChipTone::Success => (
            Color32::from_rgb(22, 101, 52),
            Color32::from_rgb(240, 253, 244),
            Stroke::new(1.0, Color32::from_rgb(187, 247, 208)),
        ),
        ChipTone::Warning if ui.visuals().dark_mode => (
            colors.warning,
            Color32::from_rgb(69, 50, 18),
            Stroke::new(1.0, Color32::from_rgb(117, 83, 25)),
        ),
        ChipTone::Warning => (
            Color32::from_rgb(146, 64, 14),
            Color32::from_rgb(254, 252, 232),
            Stroke::new(1.0, Color32::from_rgb(254, 240, 138)),
        ),
        ChipTone::Error if ui.visuals().dark_mode => (
            colors.error,
            Color32::from_rgb(78, 29, 36),
            Stroke::new(1.0, Color32::from_rgb(127, 45, 55)),
        ),
        ChipTone::Error => (
            Color32::from_rgb(185, 28, 28),
            Color32::from_rgb(254, 242, 242),
            Stroke::new(1.0, Color32::from_rgb(254, 202, 202)),
        ),
        ChipTone::Active if ui.visuals().dark_mode => (
            colors.accent,
            Color32::from_rgb(24, 48, 84),
            Stroke::new(1.0, Color32::from_rgb(42, 86, 143)),
        ),
        ChipTone::Active => (
            Color32::from_rgb(29, 78, 216),
            Color32::from_rgb(219, 234, 254),
            Stroke::new(1.0, Color32::from_rgb(191, 219, 254)),
        ),
    }
}

fn install_chip_tone(status: &ModelInstallStatus) -> ChipTone {
    match status {
        ModelInstallStatus::Installed => ChipTone::Success,
        ModelInstallStatus::Downloading { .. } | ModelInstallStatus::InstallingRuntime => {
            ChipTone::Active
        }
        ModelInstallStatus::Missing
        | ModelInstallStatus::Error(_)
        | ModelInstallStatus::RuntimeError(_) => ChipTone::Error,
        ModelInstallStatus::NotInstalled => ChipTone::Warning,
    }
}

fn runtime_chip_tone(status: &ModelRuntimeStatus) -> ChipTone {
    match status {
        ModelRuntimeStatus::Ready => ChipTone::Success,
        ModelRuntimeStatus::Running => ChipTone::Active,
        ModelRuntimeStatus::NotImplemented
        | ModelRuntimeStatus::NotInstalled
        | ModelRuntimeStatus::MissingConfiguration
        | ModelRuntimeStatus::Downloading => ChipTone::Warning,
        ModelRuntimeStatus::Error(_) => ChipTone::Error,
    }
}

fn recording_timer_text(active: &ActiveRecording) -> String {
    let elapsed = active.started_at.elapsed().as_secs();
    let total = active.max_duration_seconds.max(1) as u64;
    let remaining = total.saturating_sub(elapsed);
    format!("{elapsed}s elapsed - {remaining}s left")
}

fn launch_system_audio_settings() -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        Command::new("explorer.exe")
            .arg("ms-settings:privacy-microphone")
            .spawn()
            .map(|_| ())
            .map_err(|error| error.to_string())
    }
    #[cfg(target_os = "macos")]
    {
        Command::new("open")
            .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone")
            .spawn()
            .map(|_| ())
            .map_err(|error| error.to_string())
    }
    #[cfg(target_os = "linux")]
    {
        Command::new("pavucontrol")
            .spawn()
            .or_else(|_| Command::new("gnome-control-center").arg("sound").spawn())
            .map(|_| ())
            .map_err(|error| error.to_string())
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    {
        Err("this platform does not expose a supported audio-settings launcher".to_owned())
    }
}

fn tray_ui_state(is_recording: bool, transcript: &str) -> TrayUiState {
    TrayUiState {
        is_recording,
        has_transcript: !transcript.trim().is_empty(),
    }
}

fn tray_state_needs_sync(previous: Option<TrayUiState>, current: TrayUiState) -> bool {
    previous != Some(current)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum HotkeyRecordingAction {
    StartTranscribe,
    Stop,
    Toggle,
}

fn hotkey_recording_action(
    mode: HotkeyMode,
    event: HotkeyEvent,
    active_source: Option<RecordingSource>,
) -> Option<HotkeyRecordingAction> {
    match (mode, event, active_source) {
        (HotkeyMode::Toggle, HotkeyEvent::Pressed, _) => Some(HotkeyRecordingAction::Toggle),
        (HotkeyMode::Toggle, HotkeyEvent::Released, _) => None,
        (HotkeyMode::HoldToTalk, HotkeyEvent::Pressed, None) => {
            Some(HotkeyRecordingAction::StartTranscribe)
        }
        (HotkeyMode::HoldToTalk, HotkeyEvent::Pressed, Some(_)) => None,
        (HotkeyMode::HoldToTalk, HotkeyEvent::Released, Some(RecordingSource::Transcribe)) => {
            Some(HotkeyRecordingAction::Stop)
        }
        (HotkeyMode::HoldToTalk, HotkeyEvent::Released, Some(RecordingSource::Playground)) => None,
        (HotkeyMode::HoldToTalk, HotkeyEvent::Released, None) => None,
    }
}

fn setup_message_for_status(status: &ModelRuntimeStatus) -> String {
    match status {
        ModelRuntimeStatus::Ready => "Ready to transcribe.".to_owned(),
        ModelRuntimeStatus::MissingConfiguration => {
            "Install the selected model and managed runtime from Models before transcribing."
                .to_owned()
        }
        ModelRuntimeStatus::NotInstalled => {
            "Download an evidence-listed local model before transcribing.".to_owned()
        }
        ModelRuntimeStatus::Downloading => "The selected model is still downloading.".to_owned(),
        ModelRuntimeStatus::Running => "A transcription is already running.".to_owned(),
        ModelRuntimeStatus::NotImplemented => {
            "No verified local runtime is bundled for this model.".to_owned()
        }
        ModelRuntimeStatus::Error(message) => message.clone(),
    }
}

fn resolve_theme_mode(theme_mode: ThemeMode, system_theme: Option<eframe::Theme>) -> ThemeMode {
    match theme_mode {
        ThemeMode::System => match system_theme {
            Some(eframe::Theme::Dark) => ThemeMode::Dark,
            Some(eframe::Theme::Light) | None => ThemeMode::Light,
        },
        explicit => explicit,
    }
}

fn stitch_visuals(theme_mode: ThemeMode) -> egui::Visuals {
    let mut visuals = match theme_mode {
        ThemeMode::Dark => egui::Visuals::dark(),
        ThemeMode::Light | ThemeMode::System => egui::Visuals::light(),
    };
    let colors = ThemePalette::from_visuals(&visuals);
    visuals.override_text_color = Some(colors.text);
    visuals.selection.bg_fill = colors.accent;
    visuals.hyperlink_color = colors.accent;
    visuals.panel_fill = colors.content_bg;
    visuals.window_fill = colors.card_bg;
    visuals.extreme_bg_color = colors.panel_bg;
    visuals.widgets.noninteractive.bg_fill = colors.card_bg;
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, colors.border);
    visuals.widgets.inactive.bg_fill = colors.card_bg;
    visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, colors.border);
    visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, colors.border_strong);
    visuals.widgets.active.bg_stroke = Stroke::new(1.0, colors.accent);
    visuals
}

#[cfg(test)]
fn model_storage_estimate(model: &SttModelInfo) -> &'static str {
    compatibility_bridge::model_storage_estimate(model)
}

fn model_ui_labels(model: &SttModelInfo, descriptor: Option<&ModelDescriptor>) -> (String, String) {
    let variant = model_variant_label(model, descriptor);
    let name = descriptor.map_or(model.name.as_str(), |value| value.display_name);
    (name.to_owned(), variant)
}

fn model_artifact_remains_manageable(
    model: &SttModelInfo,
    effective_status: &ModelInstallStatus,
) -> bool {
    model.install_status.is_runnable() || effective_status.is_runnable()
}

fn model_variant_label(model: &SttModelInfo, descriptor: Option<&ModelDescriptor>) -> String {
    if let Some(descriptor) = descriptor {
        return descriptor.variant_label.to_owned();
    }

    let words = model.name.split_whitespace().collect::<Vec<_>>();
    let language = words
        .last()
        .is_some_and(|word| word.eq_ignore_ascii_case("English"))
        .then_some("en");
    let candidate = words
        .iter()
        .rev()
        .find(|word| !word.eq_ignore_ascii_case("English"))
        .copied()
        .unwrap_or(model.name.as_str())
        .to_ascii_lowercase();
    match language {
        Some(language) if !candidate.ends_with(format!(".{language}").as_str()) => {
            format!("{candidate}.{language}")
        }
        _ => candidate,
    }
}

fn model_download_total_bytes(model: &SttModelInfo) -> Option<u64> {
    compatibility_bridge::model_download_total_bytes(model)
}

fn build_development_runtime_package(
    runtime_id: &str,
    runtime_label: &str,
    package: DevelopmentRuntimePackage,
) -> Result<RuntimeReplacement, String> {
    let relative_executable = package
        .executable_path
        .strip_prefix(&package.destination_root)
        .map_err(|_| {
            format!(
                "Runtime executable {} is outside destination {}.",
                package.executable_path.display(),
                package.destination_root.display()
            )
        })?
        .to_path_buf();
    let stage_root = runtime_transaction_path(&package.destination_root, "installing");
    let staged_package = DevelopmentRuntimePackage {
        script: package.script,
        destination_env: package.destination_env,
        executable_path: stage_root.join(&relative_executable),
        destination_root: stage_root.clone(),
    };
    remove_path_if_exists(&stage_root)?;
    if let Err(message) = build_development_runtime_into(runtime_id, runtime_label, &staged_package)
    {
        let _ = remove_path_if_exists(&stage_root);
        return Err(message);
    }
    activate_staged_runtime(&package.destination_root, &stage_root, &relative_executable)
}

fn build_development_runtime_into(
    runtime_id: &str,
    runtime_label: &str,
    package: &DevelopmentRuntimePackage,
) -> Result<(), String> {
    if let Some(parent) = package.destination_root.parent() {
        fs::create_dir_all(parent)
            .map_err(|err| format!("Could not create {}: {err}", parent.display()))?;
    }

    let output = Command::new(&package.script)
        .env(package.destination_env, &package.destination_root)
        .output()
        .map_err(|err| format!("Could not run {}: {err}", package.script.display()))?;

    if !output.status.success() {
        return Err(format!(
            "Could not build {} runtime with {}: {}",
            runtime_label,
            package.script.display(),
            command_output_message(&output.stdout, &output.stderr)
        ));
    }

    if !compatibility_bridge::entrypoint_is_usable(runtime_id, &package.executable_path) {
        return Err(format!(
            "{} runtime build finished but did not create a usable runtime at {}.",
            runtime_label,
            package.executable_path.display()
        ));
    }

    Ok(())
}

fn install_runtime_files(
    runtime_id: &str,
    packaged_executable: &Path,
) -> Result<RuntimeReplacement, String> {
    install_runtime_files_to(
        runtime_id,
        packaged_executable,
        &config::runtime_storage_dir().join(runtime_id),
    )
}

fn install_runtime_files_to(
    runtime_id: &str,
    packaged_executable: &Path,
    target_root: &Path,
) -> Result<RuntimeReplacement, String> {
    let Some(source_root) = runtime_package_root(packaged_executable) else {
        return Err(format!(
            "Could not determine runtime package root for {}.",
            packaged_executable.display()
        ));
    };
    let relative_executable = packaged_executable
        .strip_prefix(&source_root)
        .map_err(|_| {
            format!(
                "Runtime executable {} is outside package root {}.",
                packaged_executable.display(),
                source_root.display()
            )
        })?
        .to_path_buf();
    if source_root == target_root {
        return Err("The managed runtime cannot be used as its own update source.".to_owned());
    }

    let stage_root = runtime_transaction_path(target_root, "installing");
    validate_runtime_copy_paths(&source_root, target_root, &stage_root)?;
    remove_path_if_exists(&stage_root)?;
    if let Err(message) = copy_dir_all(&source_root, &stage_root) {
        let _ = remove_path_if_exists(&stage_root);
        return Err(message);
    }
    let staged_executable = stage_root.join(&relative_executable);
    if !compatibility_bridge::entrypoint_is_usable(runtime_id, &staged_executable) {
        let _ = remove_path_if_exists(&stage_root);
        return Err(format!(
            "Runtime install did not create a usable runtime at {}.",
            staged_executable.display()
        ));
    }
    activate_staged_runtime(target_root, &stage_root, &relative_executable)
}

fn activate_staged_runtime(
    target_root: &Path,
    stage_root: &Path,
    relative_executable: &Path,
) -> Result<RuntimeReplacement, String> {
    let parent = target_root
        .parent()
        .ok_or_else(|| format!("Runtime target {} has no parent.", target_root.display()))?;
    fs::create_dir_all(parent)
        .map_err(|err| format!("Could not create {}: {err}", parent.display()))?;

    let backup_root = runtime_transaction_path(target_root, "backup");
    remove_path_if_exists(&backup_root)?;
    let previous = if target_root.exists() {
        fs::rename(target_root, &backup_root).map_err(|err| {
            format!(
                "Could not preserve existing runtime {}: {err}",
                target_root.display()
            )
        })?;
        Some(backup_root)
    } else {
        None
    };

    if let Err(err) = fs::rename(stage_root, target_root) {
        let restore_error = previous.as_ref().and_then(|backup_root| {
            fs::rename(backup_root, target_root)
                .err()
                .map(|restore_err| format!(" Previous runtime restore also failed: {restore_err}"))
        });
        let _ = remove_path_if_exists(stage_root);
        return Err(format!(
            "Could not activate staged runtime {}: {err}",
            stage_root.display()
        ) + restore_error.as_deref().unwrap_or_default());
    }

    Ok(RuntimeReplacement {
        installed_path: target_root.join(relative_executable),
        target_root: target_root.to_path_buf(),
        backup_root: previous,
    })
}

impl RuntimeReplacement {
    fn commit(self) -> Result<(), String> {
        if let Some(backup_root) = self.backup_root {
            remove_path_if_exists(&backup_root)?;
        }
        Ok(())
    }

    fn rollback(self) -> Result<(), String> {
        remove_path_if_exists(&self.target_root)?;
        if let Some(backup_root) = self.backup_root {
            fs::rename(&backup_root, &self.target_root).map_err(|err| {
                format!(
                    "Could not restore previous runtime {}: {err}",
                    self.target_root.display()
                )
            })?;
        }
        Ok(())
    }
}

fn runtime_transaction_path(target_root: &Path, phase: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let name = target_root
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("runtime");
    target_root.with_file_name(format!(".{name}.{phase}-{}-{nonce}", std::process::id()))
}

fn remove_path_if_exists(path: &Path) -> Result<(), String> {
    if !path.exists() {
        return Ok(());
    }
    let result = if path.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    };
    result.map_err(|err| format!("Could not remove {}: {err}", path.display()))
}

fn managed_runtime_install_record(path: PathBuf, source: &str) -> config::ManagedRuntimeInstall {
    let mut install = config::ManagedRuntimeInstall::app_managed(path.clone(), source);
    if let Some(metadata) = runtime_manifest_metadata(&path) {
        install.version = metadata
            .version
            .map(|version| version.trim().to_owned())
            .filter(|version| !version.is_empty());
        install.sha256 = metadata
            .sha256
            .or(metadata.checksum)
            .map(|sha256| sha256.trim().to_owned())
            .filter(|sha256| !sha256.is_empty());
    }
    install
}

fn runtime_manifest_metadata(executable: &Path) -> Option<RuntimeManifestMetadata> {
    let manifest = runtime_package_root(executable)?.join("runtime-manifest.json");
    let contents = fs::read_to_string(manifest).ok()?;
    serde_json::from_str(&contents).ok()
}

fn command_output_message(stdout: &[u8], stderr: &[u8]) -> String {
    let stderr = String::from_utf8_lossy(stderr).trim().to_owned();
    if !stderr.is_empty() {
        return stderr;
    }
    let stdout = String::from_utf8_lossy(stdout).trim().to_owned();
    if stdout.is_empty() {
        "process exited without an error message".to_owned()
    } else {
        stdout
    }
}

fn runtime_package_root(executable: &Path) -> Option<PathBuf> {
    let metadata = fs::symlink_metadata(executable).ok()?;
    if runtime_entry_is_link(&metadata) || !metadata.is_file() {
        return None;
    }
    let parent = executable.parent()?;
    if parent.file_name().is_some_and(|name| name == "bin") {
        parent.parent().map(Path::to_path_buf)
    } else {
        None
    }
}

fn validate_runtime_copy_paths(
    source_root: &Path,
    target_root: &Path,
    stage_root: &Path,
) -> Result<(), String> {
    let metadata = fs::symlink_metadata(source_root)
        .map_err(|err| format!("Could not inspect {}: {err}", source_root.display()))?;
    if runtime_entry_is_link(&metadata) {
        return Err(format!(
            "Runtime package root {} cannot be a symbolic link or reparse point.",
            source_root.display()
        ));
    }
    if !metadata.is_dir() {
        return Err(format!(
            "Runtime package root {} is not a directory.",
            source_root.display()
        ));
    }

    let canonical_source = canonicalize_runtime_path(source_root)?;
    for (label, path) in [("target", target_root), ("staging target", stage_root)] {
        let canonical_path = canonicalize_runtime_path(path)?;
        if canonical_path == canonical_source
            || canonical_path.starts_with(&canonical_source)
            || canonical_source.starts_with(&canonical_path)
        {
            return Err(format!(
                "Runtime package {} cannot overlap the managed runtime {label} {}.",
                source_root.display(),
                path.display()
            ));
        }
    }
    Ok(())
}

fn canonicalize_runtime_path(path: &Path) -> Result<PathBuf, String> {
    let mut unresolved = Vec::new();
    let mut current = path;
    loop {
        match current.canonicalize() {
            Ok(mut canonical) => {
                for component in unresolved.iter().rev() {
                    canonical.push(component);
                }
                return Ok(canonical);
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                let name = current.file_name().ok_or_else(|| {
                    format!("Could not resolve runtime path {}: {err}", path.display())
                })?;
                unresolved.push(name.to_os_string());
                current = current.parent().ok_or_else(|| {
                    format!("Could not resolve runtime path {}: {err}", path.display())
                })?;
            }
            Err(err) => {
                return Err(format!(
                    "Could not resolve runtime path {}: {err}",
                    path.display()
                ));
            }
        }
    }
}

fn runtime_entry_is_link(metadata: &fs::Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;

        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
    #[cfg(not(windows))]
    {
        false
    }
}

fn copy_dir_all(source: &Path, target: &Path) -> Result<(), String> {
    fs::create_dir_all(target)
        .map_err(|err| format!("Could not create {}: {err}", target.display()))?;
    for entry in
        fs::read_dir(source).map_err(|err| format!("Could not read {}: {err}", source.display()))?
    {
        let entry = entry.map_err(|err| format!("Could not read {}: {err}", source.display()))?;
        let source_path = entry.path();
        let target_path = target.join(entry.file_name());
        let metadata = fs::symlink_metadata(&source_path)
            .map_err(|err| format!("Could not inspect {}: {err}", source_path.display()))?;
        if runtime_entry_is_link(&metadata) {
            return Err(format!(
                "Runtime package entry {} cannot be a symbolic link or reparse point.",
                source_path.display()
            ));
        }
        if metadata.is_dir() {
            copy_dir_all(&source_path, &target_path)?;
        } else if metadata.is_file() {
            fs::copy(&source_path, &target_path).map_err(|err| {
                format!(
                    "Could not copy {} to {}: {err}",
                    source_path.display(),
                    target_path.display()
                )
            })?;
        } else {
            return Err(format!(
                "Runtime package entry {} is not a regular file or directory.",
                source_path.display()
            ));
        }
    }
    Ok(())
}

fn runtime_uninstall_target(
    storage_dir: &Path,
    runtime_id: &str,
    installed_path: &Path,
) -> Option<PathBuf> {
    let mut runtime_components = Path::new(runtime_id).components();
    if installed_path.as_os_str().is_empty()
        || installed_path
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
        || !matches!(
            runtime_components.next(),
            Some(std::path::Component::Normal(_))
        )
        || runtime_components.next().is_some()
    {
        return None;
    }

    let runtime_dir = storage_dir.join(runtime_id);
    let storage_canonical = storage_dir.canonicalize().ok()?;
    let installed_canonical = installed_path.canonicalize().ok()?;
    if !installed_canonical.starts_with(&storage_canonical)
        || path_has_link_below(storage_dir, installed_path)
    {
        return None;
    }
    if let Ok(runtime_canonical) = runtime_dir.canonicalize()
        && installed_canonical.starts_with(&runtime_canonical)
        && runtime_canonical.starts_with(&storage_canonical)
        && !path_has_link_below(storage_dir, &runtime_dir)
    {
        Some(runtime_dir)
    } else if installed_path.starts_with(storage_dir) {
        Some(installed_path.to_path_buf())
    } else {
        None
    }
}

fn path_has_link_below(root: &Path, path: &Path) -> bool {
    let Ok(relative) = path.strip_prefix(root) else {
        return true;
    };
    let mut current = root.to_path_buf();
    for component in relative.components() {
        if !matches!(component, std::path::Component::Normal(_)) {
            return true;
        }
        current.push(component.as_os_str());
        let Ok(metadata) = fs::symlink_metadata(&current) else {
            return true;
        };
        if runtime_entry_is_link(&metadata) {
            return true;
        }
    }
    false
}

fn is_app_managed_model_path(config: &AppConfig, path: &Path) -> bool {
    let storage = config::model_storage_dir(config);
    if path
        .components()
        .any(|component| matches!(component, std::path::Component::ParentDir))
        || !path.starts_with(&storage)
    {
        return false;
    }
    path.canonicalize()
        .ok()
        .zip(storage.canonicalize().ok())
        .is_some_and(|(path, storage)| path.starts_with(storage))
}

fn select_first_installed_model(config: &mut AppConfig) {
    config.general.selected_default_model = config::configured_models(config)
        .into_iter()
        .find(|model| model.install_status.is_runnable())
        .map(|model| model.id)
        .unwrap_or_default();
}

fn set_model_selected(config: &mut AppConfig, model_id: &str, selected: bool) {
    if selected {
        if !config
            .general
            .playground_selected_models
            .iter()
            .any(|id| id == model_id)
        {
            config
                .general
                .playground_selected_models
                .push(model_id.to_owned());
        }
    } else {
        config
            .general
            .playground_selected_models
            .retain(|id| id != model_id);
    }
}

fn apply_playground_selector_draft(config: &mut AppConfig, draft: Vec<String>) {
    let installed_ids = config::configured_models(config)
        .into_iter()
        .filter(|model| model.install_status.is_runnable())
        .map(|model| model.id)
        .collect::<Vec<_>>();
    config.general.playground_selected_models = draft
        .into_iter()
        .filter(|id| installed_ids.iter().any(|installed| installed == id))
        .collect();
}

fn move_model_before(order: &mut Vec<String>, dragged_id: &str, target_id: &str) {
    let Some(from_index) = order.iter().position(|id| id == dragged_id) else {
        return;
    };
    let dragged = order.remove(from_index);
    let to_index = order
        .iter()
        .position(|id| id == target_id)
        .unwrap_or(order.len());
    order.insert(to_index, dragged);
}

fn move_selected_model_by(
    order: &mut [String],
    selected_ids: &[String],
    model_id: &str,
    offset: isize,
) -> Option<usize> {
    let from_selected = selected_ids.iter().position(|id| id == model_id)?;
    let to_selected = from_selected.checked_add_signed(offset)?;
    let target_id = selected_ids.get(to_selected)?;
    let from_order = order.iter().position(|id| id == model_id)?;
    let to_order = order.iter().position(|id| id == target_id)?;
    order.swap(from_order, to_order);
    Some(to_selected)
}

fn cards_from_config(
    config: &AppConfig,
    service: &TranscriptionService,
) -> Vec<PlaygroundCardState> {
    cards_for_models(
        config,
        service,
        config::playground_selected_installed_models(config),
    )
}

fn cards_for_models(
    config: &AppConfig,
    service: &TranscriptionService,
    models: Vec<SttModelInfo>,
) -> Vec<PlaygroundCardState> {
    let descriptors = service
        .model_descriptors()
        .into_iter()
        .map(|descriptor| (descriptor.id.as_str().to_owned(), descriptor))
        .collect::<HashMap<_, _>>();
    models
        .into_iter()
        .filter_map(|model| {
            let descriptor = descriptors.get(&model.id)?.clone();
            let status = runtime_status_for_model(config, &model);
            Some(PlaygroundCardState {
                install_status: model.install_status,
                descriptor,
                status,
                transcript: String::new(),
                latency_ms: None,
                audio_duration_ms: None,
                peak_ram_mb: None,
                peak_vram_mb: None,
            })
        })
        .collect()
}

fn runtime_status_for_model(config: &AppConfig, model: &SttModelInfo) -> ModelRuntimeStatus {
    if config::remote_gguf_artifact(config, &model.id).is_some()
        || config::imported_gguf_artifact(config, &model.id).is_some()
        || crate::model_catalog::model_uses_embedded_runtime(&ModelId::new(&model.id))
    {
        return match model.install_status {
            ModelInstallStatus::Installed => ModelRuntimeStatus::Ready,
            ModelInstallStatus::Downloading { .. } | ModelInstallStatus::InstallingRuntime => {
                ModelRuntimeStatus::Downloading
            }
            ModelInstallStatus::NotInstalled => ModelRuntimeStatus::NotInstalled,
            ModelInstallStatus::Missing => ModelRuntimeStatus::MissingConfiguration,
            ModelInstallStatus::Error(ref message)
            | ModelInstallStatus::RuntimeError(ref message) => {
                ModelRuntimeStatus::Error(message.clone())
            }
        };
    }
    let Some(provider) = compatibility_bridge::provider_for_model(model) else {
        return ModelRuntimeStatus::Error("Model provider is not available.".to_owned());
    };

    match provider.model_install_status(model) {
        ModelInstallStatus::Installed => provider.runtime_status(config),
        ModelInstallStatus::Downloading { .. } => ModelRuntimeStatus::Downloading,
        ModelInstallStatus::InstallingRuntime => ModelRuntimeStatus::Downloading,
        ModelInstallStatus::NotInstalled => ModelRuntimeStatus::NotInstalled,
        ModelInstallStatus::Missing => ModelRuntimeStatus::MissingConfiguration,
        ModelInstallStatus::Error(message) => ModelRuntimeStatus::Error(message),
        ModelInstallStatus::RuntimeError(message) => ModelRuntimeStatus::Error(message),
    }
}

fn runtime_status_for_id(config: &AppConfig, model_id: &str) -> ModelRuntimeStatus {
    config::configured_models(config)
        .into_iter()
        .find(|model| model.id == model_id)
        .map_or_else(
            || ModelRuntimeStatus::Error("Model is no longer configured.".to_owned()),
            |model| runtime_status_for_model(config, &model),
        )
}

fn captured_hotkey_spec(input: &egui::InputState) -> Option<String> {
    input.events.iter().find_map(|event| {
        if let egui::Event::Key {
            key,
            pressed: true,
            repeat: false,
            modifiers,
            ..
        } = event
        {
            hotkey_spec_from_key(*key, *modifiers)
        } else {
            None
        }
    })
}

fn hotkey_spec_from_key(key: egui::Key, modifiers: egui::Modifiers) -> Option<String> {
    let key = key_to_hotkey_token(key)?;
    let mut parts = Vec::new();
    if modifiers.ctrl {
        parts.push("Ctrl");
    }
    if modifiers.shift {
        parts.push("Shift");
    }
    if modifiers.alt {
        parts.push("Alt");
    }
    if modifiers.command && !modifiers.ctrl {
        parts.push("Super");
    }
    parts.push(key);
    Some(parts.join("+"))
}

fn key_to_hotkey_token(key: egui::Key) -> Option<&'static str> {
    Some(match key {
        egui::Key::Space => "Space",
        egui::Key::Enter => "Enter",
        egui::Key::Tab => "Tab",
        egui::Key::Escape => "Esc",
        egui::Key::Backspace => "Backspace",
        egui::Key::Delete => "Delete",
        egui::Key::ArrowUp => "Up",
        egui::Key::ArrowDown => "Down",
        egui::Key::ArrowLeft => "Left",
        egui::Key::ArrowRight => "Right",
        egui::Key::F1 => "F1",
        egui::Key::F2 => "F2",
        egui::Key::F3 => "F3",
        egui::Key::F4 => "F4",
        egui::Key::F5 => "F5",
        egui::Key::F6 => "F6",
        egui::Key::F7 => "F7",
        egui::Key::F8 => "F8",
        egui::Key::F9 => "F9",
        egui::Key::F10 => "F10",
        egui::Key::F11 => "F11",
        egui::Key::F12 => "F12",
        egui::Key::A => "A",
        egui::Key::B => "B",
        egui::Key::C => "C",
        egui::Key::D => "D",
        egui::Key::E => "E",
        egui::Key::F => "F",
        egui::Key::G => "G",
        egui::Key::H => "H",
        egui::Key::I => "I",
        egui::Key::J => "J",
        egui::Key::K => "K",
        egui::Key::L => "L",
        egui::Key::M => "M",
        egui::Key::N => "N",
        egui::Key::O => "O",
        egui::Key::P => "P",
        egui::Key::Q => "Q",
        egui::Key::R => "R",
        egui::Key::S => "S",
        egui::Key::T => "T",
        egui::Key::U => "U",
        egui::Key::V => "V",
        egui::Key::W => "W",
        egui::Key::X => "X",
        egui::Key::Y => "Y",
        egui::Key::Z => "Z",
        egui::Key::Num0 => "0",
        egui::Key::Num1 => "1",
        egui::Key::Num2 => "2",
        egui::Key::Num3 => "3",
        egui::Key::Num4 => "4",
        egui::Key::Num5 => "5",
        egui::Key::Num6 => "6",
        egui::Key::Num7 => "7",
        egui::Key::Num8 => "8",
        egui::Key::Num9 => "9",
        _ => return None,
    })
}

#[cfg(test)]
mod layout_tests {
    use super::*;
    use crate::audio::CaptureMetrics;
    use crate::streaming::StreamIdentity;
    use crate::transcription::StreamUpdate;

    static NEXT_TEST_SESSION: AtomicU64 = AtomicU64::new(1);

    #[test]
    fn start_tab_env_parser_accepts_known_tabs() {
        assert_eq!(tab_from_env_value("models"), Some(Tab::Models));
        assert_eq!(tab_from_env_value("model playground"), Some(Tab::Debug));
        assert_eq!(tab_from_env_value("settings"), Some(Tab::General));
        assert_eq!(tab_from_env_value("unknown"), None);
    }

    #[test]
    fn models_page_paints_within_viewport_at_minimum_and_wide_widths() {
        for width in [840.0, 1440.0, 4096.0] {
            let output = render_models_page(width);
            let max_painted_x = max_visible_painted_x(&output);

            assert!(
                max_painted_x <= width + 1.0,
                "models page painted beyond viewport: max_x={max_painted_x}, width={width}"
            );
        }
    }

    #[test]
    fn normal_models_page_does_not_expose_runtime_package_maintenance() {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        configure_stitch_style(&ctx);
        let mut app = test_app();
        app.remote_catalog.snapshot = Some(
            ModelInventorySnapshot::from_trusted_records(
                1,
                crate::huggingface_catalog::CatalogSource::BundledFallback,
                vec![remote_catalog_model(
                    "handy-computer/catalog-fixture",
                    "Catalog fixture",
                    &["en", "es"],
                    true,
                    320 * 1024 * 1024,
                )],
            )
            .unwrap(),
        );
        let output = ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1_024.0, 1_600.0),
                )),
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default()
                    .frame(content_panel_frame(ctx))
                    .show(ctx, |ui| app.ui_models(ui));
            },
        );
        let update = output.platform_output.accesskit_update.unwrap();
        assert!(
            !update
                .nodes
                .iter()
                .any(|(_, node)| node.name() == Some("Runtime maintenance"))
        );
        for (label_name, role) in [
            ("Search", egui::accesskit::Role::TextInput),
            ("Language", egui::accesskit::Role::ComboBox),
        ] {
            let label_id = update
                .nodes
                .iter()
                .find(|(_, node)| {
                    node.role() == egui::accesskit::Role::StaticText
                        && node
                            .name()
                            .is_some_and(|name| name.eq_ignore_ascii_case(label_name))
                })
                .map(|(id, _)| *id)
                .unwrap_or_else(|| panic!("missing Models-page label {label_name:?}"));
            assert!(
                update.nodes.iter().any(|(_, node)| {
                    node.role() == role && node.labelled_by().contains(&label_id)
                }),
                "no {role:?} is programmatically labelled by {label_name:?}"
            );
        }
        assert!(!update.nodes.iter().any(|(_, node)| {
            node.role() == egui::accesskit::Role::CheckBox
                && node
                    .name()
                    .is_some_and(|name| name.contains("trusted catalog models only"))
        }));
        assert!(update.nodes.iter().any(|(_, node)| {
            node.role() == egui::accesskit::Role::Status
                && node.name() == Some("Bundled trusted catalog · Showing 1 of 1 models.")
                && node.live() == Some(egui::accesskit::Live::Polite)
                && node.is_live_atomic()
        }));
    }

    #[test]
    fn app_shell_pages_paint_within_viewport_at_minimum_and_wide_widths() {
        for tab in [
            Tab::Transcribe,
            Tab::General,
            Tab::Models,
            Tab::History,
            Tab::Advanced,
            Tab::About,
            Tab::Debug,
        ] {
            for width in [840.0, 1440.0, 4096.0] {
                let output = render_app_tab(tab, width);
                let max_painted_x = max_visible_painted_x(&output);

                assert!(
                    max_painted_x <= width + 1.0,
                    "{tab:?} painted beyond viewport: max_x={max_painted_x}, width={width}"
                );
            }
        }
    }

    #[test]
    fn production_route_headings_share_the_28_point_top_inset() {
        for (tab, title) in [
            (Tab::Transcribe, "Transcribe"),
            (Tab::General, "Settings"),
            (Tab::Models, "Models"),
            (Tab::History, "History"),
            (Tab::Advanced, "Settings"),
            (Tab::About, "About"),
            (Tab::Debug, "Model Playground"),
        ] {
            let output = render_app_tab(tab, 840.0);
            let bounds = output
                .platform_output
                .accesskit_update
                .as_ref()
                .unwrap()
                .nodes
                .iter()
                .find(|(_, node)| {
                    node.role() == egui::accesskit::Role::Heading && node.name() == Some(title)
                })
                .and_then(|(_, node)| node.bounds())
                .unwrap_or_else(|| panic!("missing production route heading {title}"));
            assert!(
                (bounds.y0 - 28.0).abs() <= 6.0,
                "{title} heading should share the route top inset, got {bounds:?}"
            );
        }
    }

    #[test]
    fn focused_final_playground_control_scrolls_the_outer_production_route() {
        let width = 840.0;
        let height = 420.0;
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        configure_stitch_style(&ctx);
        ctx.set_visuals(stitch_visuals(ThemeMode::Light));
        let mut app = test_app();
        app.current_tab = Tab::Debug;
        assert_eq!(app.playground_cards.len(), 1);

        let initial =
            render_debug_route_with_input(&ctx, &mut app, width, height, Vec::new(), Some(0.0));
        let target = initial
            .platform_output
            .accesskit_update
            .as_ref()
            .unwrap()
            .nodes
            .iter()
            .filter(|(_, node)| {
                node.role() == egui::accesskit::Role::Button && node.name() == Some("Clear")
            })
            .max_by(|(_, left), (_, right)| {
                left.bounds()
                    .unwrap()
                    .y1
                    .total_cmp(&right.bounds().unwrap().y1)
            })
            .map(|(id, _)| *id)
            .expect("final Playground card should expose a Clear action");
        let _ = render_debug_route_with_input(
            &ctx,
            &mut app,
            width,
            height,
            vec![egui::Event::AccessKitActionRequest(
                egui::accesskit::ActionRequest {
                    action: egui::accesskit::Action::Focus,
                    target,
                    data: None,
                },
            )],
            Some(0.1),
        );
        let _ = render_debug_route_with_input(&ctx, &mut app, width, height, Vec::new(), Some(0.2));
        let settled =
            render_debug_route_with_input(&ctx, &mut app, width, height, Vec::new(), Some(1.0));
        let target_bounds = settled
            .platform_output
            .accesskit_update
            .as_ref()
            .unwrap()
            .nodes
            .iter()
            .find(|(id, _)| *id == target)
            .and_then(|(_, node)| node.bounds())
            .expect("focused Playground action should remain accessible");
        let (_, offset, content_size, viewport) = ctx
            .data(|data| {
                data.get_temp::<(egui::Id, egui::Vec2, egui::Vec2, egui::Rect)>(egui::Id::new(
                    "route-scroll-diagnostics",
                ))
            })
            .expect("production Debug route should expose scroll diagnostics in tests");
        assert!(
            content_size.y > viewport.height() && offset.y > 0.0,
            "final Playground focus must advance the overflowing production route"
        );
        let visible_y0 = target_bounds.y0 - f64::from(offset.y);
        let visible_y1 = target_bounds.y1 - f64::from(offset.y);
        assert!(
            visible_y0 >= f64::from(viewport.min.y) - 1.0
                && visible_y1 <= f64::from(viewport.max.y) + 1.0,
            "focused Playground action must be visible; bounds={target_bounds:?}, offset={offset:?}, viewport={viewport:?}"
        );
    }

    #[test]
    fn model_pages_do_not_expand_across_repaints() {
        for tab in [Tab::Models, Tab::Debug] {
            let max_painted_x = render_app_tab_repeatedly(tab, 840.0, 8)
                .into_iter()
                .fold(0.0_f32, f32::max);

            assert!(
                max_painted_x <= 841.0,
                "{tab:?} expanded across repaints: max_x={max_painted_x}"
            );
        }
    }

    #[test]
    fn app_clear_color_is_opaque_content_background() {
        let app = test_app();
        let light_clear_color = eframe::App::clear_color(&app, &egui::Visuals::light());
        let dark_clear_color = eframe::App::clear_color(&app, &egui::Visuals::dark());

        assert_eq!(
            light_clear_color,
            ThemePalette::light().content_bg.to_normalized_gamma_f32()
        );
        assert_eq!(
            dark_clear_color,
            ThemePalette::dark().content_bg.to_normalized_gamma_f32()
        );
        assert_eq!(light_clear_color[3], 1.0);
        assert_eq!(dark_clear_color[3], 1.0);
    }

    #[test]
    fn dark_visuals_use_dark_shell_palette() {
        let visuals = stitch_visuals(ThemeMode::Dark);
        let dark = ThemePalette::dark();
        let light = stitch_visuals(ThemeMode::Light);

        assert!(visuals.dark_mode);
        assert_eq!(visuals.panel_fill, dark.content_bg);
        assert_eq!(visuals.window_fill, dark.card_bg);
        assert_eq!(visuals.extreme_bg_color, dark.panel_bg);
        assert_ne!(visuals.panel_fill, light.panel_fill);
    }

    #[test]
    fn light_theme_status_badges_meet_aa_text_contrast() {
        let ctx = egui::Context::default();
        ctx.set_visuals(stitch_visuals(ThemeMode::Light));
        let mut combinations = Vec::new();
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                for tone in [
                    ChipTone::Success,
                    ChipTone::Warning,
                    ChipTone::Active,
                    ChipTone::Error,
                ] {
                    let (text, fill, _) = chip_colors(ui, tone);
                    combinations.push((text, fill));
                }
            });
        });

        for (text, fill) in combinations {
            let ratio = contrast_ratio(text, fill);
            assert!(ratio >= 4.5, "contrast ratio was {ratio:.2}");
        }
    }

    #[test]
    fn benchmark_heatmap_text_meets_aa_in_light_and_dark_themes() {
        for (dark_mode, palette) in [(false, ThemePalette::light()), (true, ThemePalette::dark())] {
            for score in [0.0, 0.25, 0.5, 0.75, 1.0] {
                let fill = benchmark_heatmap_fill_for_mode(dark_mode, score);
                let ratio = contrast_ratio(palette.text, fill);
                assert!(
                    ratio >= 4.5,
                    "benchmark score {score} contrast was {ratio:.2} in dark_mode={dark_mode}"
                );
            }
            let ratio = contrast_ratio(palette.text, palette.panel_bg);
            assert!(
                ratio >= 4.5,
                "unavailable benchmark contrast was {ratio:.2} in dark_mode={dark_mode}"
            );
        }
    }

    fn contrast_ratio(a: Color32, b: Color32) -> f64 {
        let a = relative_luminance(a);
        let b = relative_luminance(b);
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }

    fn relative_luminance(color: Color32) -> f64 {
        let channel = |value: u8| {
            let value = f64::from(value) / 255.0;
            if value <= 0.04045 {
                value / 12.92
            } else {
                ((value + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * channel(color.r()) + 0.7152 * channel(color.g()) + 0.0722 * channel(color.b())
    }

    #[test]
    fn drag_preview_label_truncates_long_model_names() {
        assert_eq!(drag_preview_label("tiny"), "tiny");
        assert_eq!(
            drag_preview_label("faster-whisper distil-large-v3 experimental"),
            "faster-whisper distil-large-v3..."
        );
    }

    #[test]
    fn system_theme_uses_reported_native_theme() {
        assert_eq!(
            resolve_theme_mode(ThemeMode::System, Some(eframe::Theme::Dark)),
            ThemeMode::Dark
        );
        assert_eq!(
            resolve_theme_mode(ThemeMode::System, Some(eframe::Theme::Light)),
            ThemeMode::Light
        );
        assert_eq!(
            resolve_theme_mode(ThemeMode::System, None),
            ThemeMode::Light
        );
        assert_eq!(
            resolve_theme_mode(ThemeMode::Dark, Some(eframe::Theme::Light)),
            ThemeMode::Dark
        );
    }

    #[test]
    fn active_model_can_stay_pinned_when_removed_from_playground_selection() {
        let mut config = AppConfig::default();
        let active_model = config.general.selected_default_model.clone();

        set_model_selected(&mut config, &active_model, false);
        config::normalize_config(&mut config);

        assert_eq!(config.general.selected_default_model, active_model);
        assert!(
            !config
                .general
                .playground_selected_models
                .iter()
                .any(|id| id == &active_model)
        );
    }

    #[test]
    fn toggle_hotkey_toggles_only_on_press() {
        assert_eq!(
            hotkey_recording_action(HotkeyMode::Toggle, HotkeyEvent::Pressed, None),
            Some(HotkeyRecordingAction::Toggle)
        );
        assert_eq!(
            hotkey_recording_action(
                HotkeyMode::Toggle,
                HotkeyEvent::Pressed,
                Some(RecordingSource::Transcribe)
            ),
            Some(HotkeyRecordingAction::Toggle)
        );
        assert_eq!(
            hotkey_recording_action(
                HotkeyMode::Toggle,
                HotkeyEvent::Released,
                Some(RecordingSource::Transcribe)
            ),
            None
        );
    }

    #[test]
    fn hold_to_talk_starts_on_press_and_stops_on_release() {
        assert_eq!(
            hotkey_recording_action(HotkeyMode::HoldToTalk, HotkeyEvent::Pressed, None),
            Some(HotkeyRecordingAction::StartTranscribe)
        );
        assert_eq!(
            hotkey_recording_action(
                HotkeyMode::HoldToTalk,
                HotkeyEvent::Pressed,
                Some(RecordingSource::Transcribe)
            ),
            None
        );
        assert_eq!(
            hotkey_recording_action(
                HotkeyMode::HoldToTalk,
                HotkeyEvent::Released,
                Some(RecordingSource::Transcribe)
            ),
            Some(HotkeyRecordingAction::Stop)
        );
        assert_eq!(
            hotkey_recording_action(HotkeyMode::HoldToTalk, HotkeyEvent::Released, None),
            None
        );
    }

    #[test]
    fn hold_to_talk_release_does_not_stop_playground_recording() {
        assert_eq!(
            hotkey_recording_action(
                HotkeyMode::HoldToTalk,
                HotkeyEvent::Released,
                Some(RecordingSource::Playground)
            ),
            None
        );
    }

    #[test]
    fn explicit_stop_during_pending_capture_is_preserved() {
        let mut app = test_app();
        let session_id = app
            .session_coordinator
            .begin(SessionPurpose::Dictation)
            .unwrap();
        app.pending_recording = Some(PendingRecording {
            session_id,
            source: RecordingSource::Transcribe,
            stop_requested: false,
            max_duration_seconds: 30,
            latency: LatencyTrace::started_at(Instant::now(), TriggerObservation::HotkeyPoll),
            capture_diagnostics: CaptureDiagnosticContext::default(),
            abandon: Arc::new(AtomicBool::new(false)),
        });

        app.stop_recording();

        let pending = app.pending_recording.as_ref().unwrap();
        assert!(pending.stop_requested);
        assert!(pending.latency.stop_requested_at.is_some());
        assert_eq!(
            app.session_coordinator.stop_reason(),
            Some(StopReason::Explicit)
        );
        assert_eq!(app.status_message, "Cancelling microphone startup");
    }

    #[test]
    fn explicit_stop_before_capture_ready_finalizes_and_dispatches_once() {
        let mut app = test_app();
        let session_id = app
            .session_coordinator
            .begin(SessionPurpose::Dictation)
            .unwrap();
        app.pending_recording = Some(PendingRecording {
            session_id,
            source: RecordingSource::Transcribe,
            stop_requested: false,
            max_duration_seconds: 30,
            latency: LatencyTrace::started_at(Instant::now(), TriggerObservation::HotkeyPoll),
            capture_diagnostics: CaptureDiagnosticContext::default(),
            abandon: Arc::new(AtomicBool::new(false)),
        });

        app.stop_recording();
        app.tx
            .send(AppEvent::CaptureReady {
                session_id,
                result: Ok(RecordingSession::simulated(
                    Some(test_prepared_audio()),
                    CaptureStopReason::Explicit,
                )),
            })
            .unwrap();
        app.poll_events();

        assert!(app.active_recording.as_ref().unwrap().stop_requested);
        assert_eq!(
            app.session_coordinator.stop_reason(),
            Some(StopReason::Explicit)
        );

        for _ in 0..100 {
            app.poll_recording();
            if app.active_recording.is_none() {
                break;
            }
            thread::sleep(Duration::from_millis(5));
        }

        assert!(app.active_recording.is_none());
        assert_eq!(
            app.session_coordinator.pending_request_count(session_id),
            Some(1)
        );
        app.supersede_active_session();
    }

    #[test]
    fn reachable_no_speech_completions_never_dispatch_or_paste() {
        for stop_reason in [
            CaptureStopReason::MaximumDuration,
            CaptureStopReason::Explicit,
        ] {
            let mut app = test_app();
            app.transcript = "keep prior transcript".to_owned();
            let session_id = app
                .session_coordinator
                .begin(SessionPurpose::Dictation)
                .unwrap();
            app.pending_recording = Some(PendingRecording {
                session_id,
                source: RecordingSource::Transcribe,
                stop_requested: false,
                max_duration_seconds: 30,
                latency: LatencyTrace::started_at(Instant::now(), TriggerObservation::HotkeyPoll),
                capture_diagnostics: CaptureDiagnosticContext::default(),
                abandon: Arc::new(AtomicBool::new(false)),
            });
            app.tx
                .send(AppEvent::CaptureReady {
                    session_id,
                    result: Ok(RecordingSession::simulated(None, stop_reason)),
                })
                .unwrap();
            app.poll_events();
            if stop_reason == CaptureStopReason::Explicit {
                app.stop_recording();
            } else {
                app.active_recording.as_ref().unwrap().session.stop();
            }

            for _ in 0..100 {
                app.poll_recording();
                if app.active_recording.is_none() {
                    break;
                }
                thread::sleep(Duration::from_millis(5));
            }

            assert!(app.active_recording.is_none());
            assert_eq!(app.status, TranscriptionStatus::Idle);
            assert_eq!(
                app.status_message,
                "Microphone signal was silent or too low. Check its hardware mute and gain, move closer, verify the selected input, and try again."
            );
            assert_eq!(app.transcript, "keep prior transcript");
            assert!(app.pending_output.is_none());
            assert_eq!(
                app.session_coordinator.last_terminal().unwrap().outcome,
                crate::core::TerminalOutcome::Cancelled
            );
            assert!(
                app.latest_latency
                    .as_ref()
                    .unwrap()
                    .stop_requested_at
                    .is_some()
            );
        }
    }

    #[test]
    fn no_speech_feedback_distinguishes_low_input_from_short_non_speech_audio() {
        let low = no_speech_feedback(
            Some(audio::MIN_SPEECH_ACTIVATION_RMS / 10.0),
            Some("Microphone (fifine  Microphone)"),
            audio::MIN_SPEECH_ACTIVATION_RMS,
        );
        assert_eq!(
            low.status_message,
            "FIFINE microphone signal was silent or too low. Tap its top mute control, turn up the physical gain knob, move closer, and try again."
        );
        assert_eq!(
            low.overlay_message,
            "Microphone signal too low — check mute and gain"
        );

        let non_silent = no_speech_feedback(
            Some(audio::MIN_SPEECH_ACTIVATION_RMS),
            Some("Microphone Array"),
            audio::MIN_SPEECH_ACTIVATION_RMS,
        );
        assert_eq!(
            non_silent.status_message,
            "No speech detected; nothing was pasted."
        );
        assert_eq!(non_silent.overlay_message, "No speech detected");
    }

    #[test]
    fn input_sensitivity_uses_its_threshold_for_low_input_feedback() {
        let mut config = AppConfig::default();
        config.recording.manual_activation_rms = 0.03;

        let feedback = no_speech_feedback(
            Some(0.02),
            Some("Microphone Array"),
            diagnostic_activation_floor(&config),
        );

        assert!(feedback.status_message.contains("silent or too low"));
    }

    #[test]
    fn sensitivity_slider_endpoints_remain_valid_capture_thresholds() {
        assert!(
            (dbfs_to_rms(INPUT_LEVEL_MIN_DBFS) - config::settings::MIN_MANUAL_ACTIVATION_RMS).abs()
                < 1e-9
        );
        assert_eq!(
            dbfs_to_rms(INPUT_LEVEL_MAX_DBFS),
            config::settings::MAX_MANUAL_ACTIVATION_RMS
        );
    }

    #[test]
    fn input_level_mapping_handles_silence_and_round_trips_positions() {
        assert_eq!(rms_to_dbfs(0.0), INPUT_LEVEL_MIN_DBFS);
        assert_eq!(rms_to_dbfs(f32::NAN), INPUT_LEVEL_MIN_DBFS);
        assert!(rms_to_dbfs(0.0).is_finite());
        assert_eq!(dbfs_to_slider_position(INPUT_LEVEL_MIN_DBFS), 0.0);
        assert_eq!(dbfs_to_slider_position(INPUT_LEVEL_MAX_DBFS), 1.0);

        for position in [0.0_f32, 0.2, 0.5, 0.8, 1.0] {
            let round_trip = dbfs_to_slider_position(slider_position_to_dbfs(position));
            assert!((round_trip - position).abs() < 1e-6);
        }
    }

    #[test]
    fn microphone_level_envelope_attacks_fast_releases_slowly_and_resets_stale_input() {
        let base = Instant::now();
        let mut envelope = MicrophoneLevelEnvelope::default();
        let attack = envelope.update(1.0, Some(1), true, base);
        assert!(
            attack > 0.5,
            "attack should move most of the way in one meter frame"
        );

        let release = envelope.update(0.0, Some(2), true, base + METER_REPAINT_DELAY);
        assert!(
            release > 0.4,
            "release should settle more slowly than attack"
        );
        assert!(release < attack);

        let stale = envelope.update(
            1.0,
            Some(2),
            true,
            base + INPUT_LEVEL_STALE_AFTER + Duration::from_millis(81),
        );
        assert!(stale < release, "a stale unchanged sample must decay");
        let settled = envelope.update(
            1.0,
            Some(2),
            true,
            base + INPUT_LEVEL_STALE_AFTER + Duration::from_secs(3),
        );
        assert_eq!(settled, 0.0);
    }

    #[test]
    fn stale_microphone_test_ready_is_discarded_without_replacing_the_current_request() {
        let mut app = test_app();
        app.microphone_test = MicrophoneTest::Starting {
            request_id: 2,
            stop_requested: false,
            cancellation: CaptureCancellation::new(),
        };
        app.tx
            .send(AppEvent::MicrophoneTestReady {
                request_id: 1,
                result: Ok(RecordingSession::simulated(
                    None,
                    CaptureStopReason::Explicit,
                )),
            })
            .unwrap();

        app.poll_events();

        assert!(matches!(
            app.microphone_test,
            MicrophoneTest::Starting {
                request_id: 2,
                stop_requested: false,
                ..
            }
        ));
        assert_eq!(app.session_coordinator.phase(), DictationPhase::Idle);
    }

    #[test]
    fn microphone_test_ready_keeps_capture_outside_dictation_lifecycle() {
        let mut app = test_app();
        app.microphone_test = MicrophoneTest::Starting {
            request_id: 1,
            stop_requested: false,
            cancellation: CaptureCancellation::new(),
        };
        app.tx
            .send(AppEvent::MicrophoneTestReady {
                request_id: 1,
                result: Ok(RecordingSession::simulated(
                    None,
                    CaptureStopReason::Explicit,
                )),
            })
            .unwrap();

        app.poll_events();

        assert!(matches!(app.microphone_test, MicrophoneTest::Active { .. }));
        assert!(app.pending_recording.is_none());
        assert!(app.active_recording.is_none());
        assert_eq!(app.session_coordinator.phase(), DictationPhase::Idle);
        app.stop_microphone_test();
    }

    #[test]
    fn microphone_monitor_defers_retained_audio_playback_until_teardown() {
        let mut app = test_app();
        app.microphone_test = MicrophoneTest::Active {
            session: RecordingSession::simulated(None, CaptureStopReason::Explicit),
        };

        app.apply_history_action(HistoryPageAction::Play(7));

        assert!(app.playing_history_id.is_none());
        assert_eq!(app.deferred_history_playback, Some(7));
        assert!(matches!(
            app.microphone_test,
            MicrophoneTest::Stopping { .. }
        ));
        assert_eq!(app.status_message, "Preparing audio playback");
    }

    #[test]
    fn cancelled_monitor_startup_keeps_audio_owners_excluded_until_confirmation() {
        let mut app = test_app();
        let cancellation = CaptureCancellation::new();
        app.microphone_test = MicrophoneTest::Starting {
            request_id: 12,
            stop_requested: false,
            cancellation: cancellation.clone(),
        };

        app.stop_microphone_test();
        assert!(cancellation.is_cancelled());
        app.apply_history_action(HistoryPageAction::Play(7));
        assert_eq!(app.deferred_history_playback, Some(7));
        app.start_recording(RecordingSource::Transcribe);
        assert!(app.deferred_recording_start.is_some());
        assert!(app.deferred_history_playback.is_none());
        assert!(app.pending_recording.is_none());
        assert!(app.active_recording.is_none());

        app.stop_recording();
        app.tx
            .send(AppEvent::MicrophoneTestReady {
                request_id: 12,
                result: Err(CaptureError::StartupCancelled),
            })
            .unwrap();
        app.poll_events();

        assert!(matches!(app.microphone_test, MicrophoneTest::Idle));
        assert!(app.microphone_test_error.is_none());
    }

    #[test]
    fn second_toggle_cancels_a_recording_deferred_for_monitor_teardown() {
        let mut app = test_app();
        app.microphone_test = MicrophoneTest::Active {
            session: RecordingSession::simulated(None, CaptureStopReason::Explicit),
        };

        app.toggle_recording();
        assert!(app.deferred_recording_start.is_some());
        assert_eq!(app.recording_source(), Some(RecordingSource::Transcribe));

        app.toggle_recording();
        assert!(app.deferred_recording_start.is_none());
        assert_eq!(app.status_message, "Recording cancelled");
        assert!(app.pending_recording.is_none());
        assert!(app.active_recording.is_none());
    }

    #[test]
    fn hold_release_cancels_a_recording_deferred_for_monitor_teardown() {
        let mut app = test_app();
        app.microphone_test = MicrophoneTest::Active {
            session: RecordingSession::simulated(None, CaptureStopReason::Explicit),
        };
        app.start_recording_at(
            RecordingSource::Transcribe,
            Instant::now(),
            TriggerObservation::HotkeyPoll,
        );

        assert_eq!(
            hotkey_recording_action(
                HotkeyMode::HoldToTalk,
                HotkeyEvent::Released,
                app.recording_source(),
            ),
            Some(HotkeyRecordingAction::Stop)
        );
        app.stop_recording();

        assert!(app.deferred_recording_start.is_none());
        assert!(app.pending_recording.is_none());
        assert!(app.active_recording.is_none());
    }

    #[test]
    fn quit_clears_deferred_audio_before_monitor_teardown_can_dispatch_it() {
        let ctx = egui::Context::default();
        let mut app = test_app();
        app.microphone_test = MicrophoneTest::Active {
            session: RecordingSession::simulated(None, CaptureStopReason::Explicit),
        };
        app.deferred_recording_start = Some(DeferredRecordingStart {
            source: RecordingSource::Transcribe,
            activation_at: Instant::now(),
            trigger_observation: TriggerObservation::AppAction,
        });
        app.deferred_history_playback = Some(7);

        app.apply_tray_command(TrayCommand::Quit, &ctx);
        app.poll_microphone_test();

        assert!(app.quit_requested);
        assert!(app.deferred_recording_start.is_none());
        assert!(app.deferred_history_playback.is_none());
        assert!(app.pending_recording.is_none());
        assert!(app.active_recording.is_none());
        assert!(app.playing_history_id.is_none());
    }

    #[test]
    fn monitor_failure_is_retry_gated_and_device_change_resets_the_source() {
        let mut app = test_app();
        app.microphone_test = MicrophoneTest::Starting {
            request_id: 3,
            stop_requested: false,
            cancellation: CaptureCancellation::new(),
        };
        app.tx
            .send(AppEvent::MicrophoneTestReady {
                request_id: 3,
                result: Err(CaptureError::StartupCancelled),
            })
            .unwrap();
        app.poll_events();
        assert!(app.microphone_monitor_retry_required);
        let sequence = app.microphone_test_sequence;
        app.ensure_microphone_monitor();
        assert_eq!(app.microphone_test_sequence, sequence);

        let session = RecordingSession::simulated(None, CaptureStopReason::Explicit);
        session.set_simulated_telemetry(LevelSnapshot {
            rms: 0.08,
            peak: 0.15,
        });
        app.microphone_test = MicrophoneTest::Active { session };
        let now = Instant::now();
        app.microphone_level_envelope
            .update(0.08, Some(1), true, now);
        app.apply_settings_screen_action(ScreenAction::SetAudioDevice(Some(
            "Replacement microphone".to_owned(),
        )));

        assert_eq!(
            app.config.recording.audio_input_device_name.as_deref(),
            Some("Replacement microphone")
        );
        assert!(matches!(
            app.microphone_test,
            MicrophoneTest::Stopping { .. }
        ));
        assert!(!app.microphone_monitor_retry_required);
        assert!(app.microphone_level_envelope.last_revision.is_none());
    }

    #[test]
    fn settings_microphone_retry_clears_the_failure_gate_before_restarting() {
        let mut app = test_app();
        app.microphone_test_error = Some("Microphone permission denied".to_owned());
        app.microphone_monitor_retry_required = true;
        app.playing_history_id = Some(7);
        let sequence = app.microphone_test_sequence;

        app.apply_settings_screen_action(ScreenAction::RetryMicrophone);

        assert!(app.microphone_test_error.is_none());
        assert!(!app.microphone_monitor_retry_required);
        assert_eq!(app.microphone_test_sequence, sequence);
        assert!(matches!(app.microphone_test, MicrophoneTest::Idle));
    }

    #[test]
    fn no_speech_feedback_uses_capture_start_device_and_threshold_after_settings_drift() {
        let mut config = AppConfig::default();
        config.recording.manual_activation_rms = 0.03;
        config.recording.audio_input_device_name = Some("FIFINE A8".to_owned());
        let diagnostics = CaptureDiagnosticContext::from_config(&config);

        config.recording.manual_activation_rms = 0.001;
        config.recording.audio_input_device_name = Some("Different microphone".to_owned());
        let feedback = no_speech_feedback_for_capture(Some(0.02), &diagnostics);

        assert_eq!(diagnostics.activation_floor, 0.03);
        assert_eq!(diagnostics.input_device_name.as_deref(), Some("FIFINE A8"));
        assert!(feedback.status_message.starts_with("FIFINE microphone"));
    }

    #[test]
    fn live_input_sensitivity_updates_capture_diagnostics() {
        let mut app = test_app();
        app.active_recording = Some(ActiveRecording {
            session_id: SessionId(901),
            session: RecordingSession::simulated(None, CaptureStopReason::Explicit),
            source: RecordingSource::Transcribe,
            stop_requested: false,
            started_at: Instant::now(),
            max_duration_seconds: 30,
            latency: LatencyTrace::started_at(Instant::now(), TriggerObservation::AppAction),
            capture_diagnostics: CaptureDiagnosticContext::from_config(&app.config),
        });
        app.pending_recording = Some(PendingRecording {
            session_id: SessionId(902),
            source: RecordingSource::Transcribe,
            stop_requested: false,
            max_duration_seconds: 30,
            latency: LatencyTrace::started_at(Instant::now(), TriggerObservation::AppAction),
            capture_diagnostics: CaptureDiagnosticContext::from_config(&app.config),
            abandon: Arc::new(AtomicBool::new(false)),
        });

        app.config.recording.manual_activation_rms = 0.025;
        app.apply_input_sensitivity_threshold();

        assert_eq!(
            app.active_recording
                .as_ref()
                .unwrap()
                .capture_diagnostics
                .activation_floor,
            0.025
        );
        assert_eq!(
            app.pending_recording
                .as_ref()
                .unwrap()
                .capture_diagnostics
                .activation_floor,
            0.025
        );
    }

    #[test]
    fn capture_ready_adopts_the_latest_input_sensitivity() {
        let mut app = test_app();
        let session_id = app
            .session_coordinator
            .begin(SessionPurpose::Dictation)
            .unwrap();
        app.pending_recording = Some(PendingRecording {
            session_id,
            source: RecordingSource::Transcribe,
            stop_requested: false,
            max_duration_seconds: 30,
            latency: LatencyTrace::started_at(Instant::now(), TriggerObservation::AppAction),
            capture_diagnostics: CaptureDiagnosticContext::from_config(&app.config),
            abandon: Arc::new(AtomicBool::new(false)),
        });
        app.config.recording.manual_activation_rms = 0.025;
        app.tx
            .send(AppEvent::CaptureReady {
                session_id,
                result: Ok(RecordingSession::simulated(
                    None,
                    CaptureStopReason::Explicit,
                )),
            })
            .unwrap();

        app.poll_events();

        let active = app.active_recording.as_ref().unwrap();
        assert_eq!(active.session.manual_activation_threshold(), 0.025);
        assert_eq!(active.capture_diagnostics.activation_floor, 0.025);
        app.stop_and_discard_active_recording();
    }

    #[test]
    fn typed_recording_settings_keep_sensitivity_gating_when_endpointing_is_off() {
        let mut config = AppConfig::default();
        config.recording.vad_enabled = false;
        config.recording.speech_confirmation_ms = 175;
        config.recording.internal_pause_ms = 525;
        config.recording.endpoint_silence_ms = 975;
        config.recording.pre_roll_ms = 300;
        config.recording.post_roll_ms = 225;

        let options = capture_options_from_config(&config);

        assert!(
            options.vad_enabled,
            "input sensitivity must continue to gate accepted speech"
        );
        assert!(!options.endpointing_enabled);
        assert_eq!(options.vad.speech_confirmation, Duration::from_millis(175));
        assert_eq!(options.vad.pause, Duration::from_millis(525));
        assert_eq!(options.vad.endpoint, Duration::from_millis(975));
        assert_eq!(options.vad.pre_roll, Duration::from_millis(300));
        assert_eq!(options.vad.post_roll, Duration::from_millis(225));
        assert_eq!(options.intent, CaptureIntent::Dictation);

        config.recording.manual_activation_rms = 0.025;
        let manual = capture_options_from_config(&config);
        assert_eq!(
            manual.sensitivity,
            Sensitivity::Manual {
                activation_rms: 0.025
            }
        );
    }

    #[test]
    fn silence_endpointing_is_limited_to_toggle_mode() {
        let mut config = AppConfig::default();
        config.recording.vad_enabled = true;

        config.recording.hotkey_mode = HotkeyMode::Toggle;
        let toggle = capture_options_from_config(&config);
        assert!(toggle.vad_enabled);
        assert!(toggle.endpointing_enabled);

        config.recording.hotkey_mode = HotkeyMode::HoldToTalk;
        let hold = capture_options_from_config(&config);
        assert!(hold.vad_enabled, "hold mode still uses VAD for trimming");
        assert!(
            !hold.endpointing_enabled,
            "hold mode must wait for shortcut release or the duration limit"
        );
    }

    #[test]
    fn preload_completion_is_preserved_on_both_sides_of_capture_ready() {
        for preload_first in [true, false] {
            let mut app = test_app();
            let session_id = app
                .session_coordinator
                .begin(SessionPurpose::Dictation)
                .unwrap();
            let model_id = ModelId::new("whisper_cpp_base_en");
            app.session_coordinator
                .model_load_started(session_id, model_id.clone())
                .unwrap();
            app.pending_recording = Some(PendingRecording {
                session_id,
                source: RecordingSource::Transcribe,
                stop_requested: false,
                max_duration_seconds: 30,
                latency: LatencyTrace::started_at(Instant::now(), TriggerObservation::HotkeyPoll),
                capture_diagnostics: CaptureDiagnosticContext::default(),
                abandon: Arc::new(AtomicBool::new(false)),
            });
            let preload_event = AppEvent::ModelPreloadFinished {
                session_id,
                model_id,
                load_duration_ms: 7,
            };
            let capture_event = AppEvent::CaptureReady {
                session_id,
                result: Ok(RecordingSession::simulated(
                    Some(test_prepared_audio()),
                    CaptureStopReason::Explicit,
                )),
            };
            if preload_first {
                app.tx.send(preload_event).unwrap();
                app.tx.send(capture_event).unwrap();
            } else {
                app.tx.send(capture_event).unwrap();
                app.tx.send(preload_event).unwrap();
            }

            app.poll_events();

            let active = app.active_recording.as_ref().unwrap();
            assert!(active.latency.model_loaded_at.is_some());
            assert!(matches!(
                app.session_coordinator.active().unwrap().model_load(),
                crate::core::ModelLoadState::Ready { .. }
            ));
            app.stop_and_discard_active_recording();
        }
    }

    #[test]
    fn latency_summary_reports_observed_phases_and_total() {
        let base = Instant::now();
        let trace = LatencyTrace {
            activation_at: base,
            trigger_observation: TriggerObservation::AppAction,
            overlay_visible_at: None,
            recorder_started_at: Some(base + Duration::from_millis(10)),
            first_meter_update_at: Some(base + Duration::from_millis(15)),
            model_load_started_at: Some(base + Duration::from_millis(20)),
            model_loaded_at: Some(base + Duration::from_millis(70)),
            first_partial_at: None,
            stop_requested_at: Some(base + Duration::from_millis(100)),
            capture_finalized_at: Some(base + Duration::from_millis(140)),
            transcription_dispatched_at: Some(base + Duration::from_millis(150)),
            transcription_job_completed_at: Some(base + Duration::from_millis(650)),
            final_text_ready_at: Some(base + Duration::from_millis(650)),
            ui_result_at: Some(base + Duration::from_millis(660)),
            output_started_at: Some(base + Duration::from_millis(660)),
            target_activated_at: None,
            paste_completed_at: Some(base + Duration::from_millis(735)),
            output_completed_at: Some(base + Duration::from_millis(735)),
            model_id: Some("whisper_cpp_base_en".into()),
            resolved_backend: Some("transcribe-cpp".into()),
            compute_backend: Some("CPU".into()),
            streaming_mode: Some("rolling".into()),
            cold_or_warm: Some("cold".into()),
            reported_model_load_ms: Some(50),
            audio_duration_ms: Some(500),
            processing_duration_ms: Some(500),
            maximum_input_rms: Some(0.25),
            maximum_input_peak: Some(0.75),
            capture_diagnostics: None,
        };

        assert_eq!(
            trace.summary_lines(),
            vec![
                "App action to recorder ready: 10 ms",
                "App action to first meter update: 15 ms",
                "Model load: 50 ms",
                "Stop to audio finalized: 40 ms",
                "Transcription job: 500 ms",
                "Stop to final text: 550 ms",
                "STT done to UI update: 10 ms",
                "Final text ready to paste complete: 85 ms",
                "Focused-app output: 75 ms",
                "Total observed: 735 ms",
            ]
        );
    }

    #[test]
    fn diagnostic_snapshot_attributes_only_observed_session_phases() {
        let base = Instant::now();
        let mut trace = LatencyTrace::started_at(base, TriggerObservation::HotkeyPoll);
        trace.observe_session_context(Some("whisper_cpp_base_en".into()), StreamingMode::Rolling);
        trace.overlay_visible_at = Some(base + Duration::from_millis(5));
        trace.recorder_started_at = Some(base + Duration::from_millis(10));
        trace.first_meter_update_at = Some(base + Duration::from_millis(25));
        trace.stop_requested_at = Some(base + Duration::from_millis(510));
        trace.capture_finalized_at = Some(base + Duration::from_millis(530));
        trace.final_text_ready_at = Some(base + Duration::from_millis(710));
        trace.paste_completed_at = Some(base + Duration::from_millis(760));
        trace.output_completed_at = Some(base + Duration::from_millis(770));
        trace.observe_capture_metrics(&CaptureMetrics {
            duration: Duration::from_millis(520),
            stop_trigger_elapsed: Duration::from_millis(500),
            speech_trigger_elapsed: Some(Duration::from_millis(150)),
            source_sample_rate: 48_000,
            source_channels: 2,
            source_frames: 24_960,
            prepared_frames: 8_320,
            maximum_input_rms: 0.2,
            maximum_input_peak: 0.8,
            dropped_samples: 0,
            stream_restarts: 0,
        });

        let snapshot =
            trace.diagnostic_snapshot(SessionId(41), DiagnosticSessionOutcome::Completed, None);

        assert_eq!(snapshot.session_id, 41);
        assert_eq!(snapshot.trigger, "hotkey_poll");
        assert_eq!(snapshot.streaming_mode.as_deref(), Some("rolling"));
        assert_eq!(snapshot.metrics.hotkey_to_overlay_visible_ms, Some(5));
        assert_eq!(snapshot.metrics.hotkey_to_capture_started_ms, Some(10));
        assert_eq!(snapshot.metrics.hotkey_to_first_meter_update_ms, Some(25));
        assert_eq!(snapshot.metrics.maximum_input_rms, Some(0.2));
        assert_eq!(snapshot.metrics.maximum_input_peak, Some(0.8));
        assert_eq!(snapshot.metrics.recording_duration_ms, Some(500));
        assert_eq!(snapshot.metrics.stop_to_capture_finalized_ms, Some(20));
        assert_eq!(snapshot.metrics.recording_end_to_final_text_ms, Some(200));
        assert_eq!(snapshot.metrics.final_text_to_paste_ms, Some(50));
        assert_eq!(snapshot.metrics.final_text_to_output_completed_ms, Some(60));
        assert_eq!(snapshot.metrics.total_end_to_end_ms, Some(770));
        assert_eq!(snapshot.metrics.speech_start_detected_ms, None);
        assert_eq!(snapshot.metrics.post_processing_ms, None);

        let failed = trace.diagnostic_snapshot(
            SessionId(41),
            DiagnosticSessionOutcome::Failed,
            Some(DiagnosticFailureStage::Output),
        );
        assert_eq!(failed.outcome, DiagnosticSessionOutcome::Failed);
        assert_eq!(failed.failure_stage, Some(DiagnosticFailureStage::Output));
    }

    #[test]
    fn failed_transcription_has_no_final_text_or_paste_metric() {
        let base = Instant::now();
        let trace = LatencyTrace {
            activation_at: base,
            trigger_observation: TriggerObservation::HotkeyPoll,
            overlay_visible_at: None,
            recorder_started_at: Some(base + Duration::from_millis(10)),
            first_meter_update_at: None,
            model_load_started_at: None,
            model_loaded_at: None,
            first_partial_at: None,
            stop_requested_at: Some(base + Duration::from_millis(100)),
            capture_finalized_at: Some(base + Duration::from_millis(140)),
            transcription_dispatched_at: Some(base + Duration::from_millis(150)),
            transcription_job_completed_at: Some(base + Duration::from_millis(650)),
            final_text_ready_at: None,
            ui_result_at: Some(base + Duration::from_millis(660)),
            output_started_at: None,
            target_activated_at: None,
            paste_completed_at: None,
            output_completed_at: None,
            model_id: Some("whisper_cpp_base_en".into()),
            resolved_backend: Some("transcribe-cpp".into()),
            compute_backend: Some("CPU".into()),
            streaming_mode: Some("final_only".into()),
            cold_or_warm: None,
            reported_model_load_ms: None,
            audio_duration_ms: Some(500),
            processing_duration_ms: None,
            maximum_input_rms: Some(0.2),
            maximum_input_peak: Some(0.8),
            capture_diagnostics: None,
        };

        let summary = trace.summary_lines();
        assert!(
            summary
                .iter()
                .any(|line| line == "Hotkey dequeued to recorder ready: 10 ms")
        );
        assert!(
            summary
                .iter()
                .any(|line| line == "Transcription job: 500 ms")
        );
        assert!(!summary.iter().any(|line| line.contains("final text")));
        assert!(!summary.iter().any(|line| line.contains("paste")));
    }

    #[test]
    fn repaint_delay_is_fast_only_for_active_work() {
        let mut app = test_app();
        assert_eq!(app.next_repaint_delay(), IDLE_REPAINT_DELAY);

        app.status = TranscriptionStatus::Transcribing;
        assert_eq!(app.next_repaint_delay(), ACTIVE_REPAINT_DELAY);

        app.status = TranscriptionStatus::Idle;
        app.playground_pending = 1;
        assert_eq!(app.next_repaint_delay(), ACTIVE_REPAINT_DELAY);

        app.playground_pending = 0;
        app.model_downloads.insert(
            "whisper_cpp_base_en".to_owned(),
            ModelInstallStatus::Downloading {
                downloaded_bytes: 42,
                total_bytes: None,
                bytes_per_second: None,
            },
        );
        assert_eq!(app.next_repaint_delay(), ACTIVE_REPAINT_DELAY);
    }

    #[test]
    fn performance_modes_are_layman_facing() {
        let labels = AccelerationPreference::ALL
            .into_iter()
            .map(AccelerationPreference::label)
            .collect::<Vec<_>>();

        assert_eq!(labels, vec!["Auto", "GPU", "CPU only"]);
    }

    #[test]
    fn playground_cleanup_releases_shared_audio_even_when_debug_mode_is_set() {
        let mut app = test_app();
        app.config.developer.debug_mode = true;
        let session_id = SessionId(7);
        let request_id = RequestId(9);
        seed_test_request(
            &mut app,
            RecordingSource::Playground,
            session_id,
            request_id,
            "test-model",
        );
        app.playground_pending = 1;
        let audio = test_prepared_audio();
        let released = Arc::downgrade(&audio);
        app.playground_runs.insert(
            session_id,
            PlaygroundRunState {
                pending_requests: HashMap::from([(request_id, "test-model".to_owned())]),
                _audio: audio,
            },
        );

        app.cleanup_after_job(RecordingSource::Playground, session_id, request_id);

        assert!(released.upgrade().is_none());
    }

    #[test]
    fn correlation_identifiers_are_monotonic() {
        let mut app = test_app();
        let first = app
            .session_coordinator
            .begin(SessionPurpose::Dictation)
            .unwrap();
        assert_eq!(first, SessionId(1));
        app.session_coordinator.cancel(first).unwrap();
        let second = app
            .session_coordinator
            .begin(SessionPurpose::Dictation)
            .unwrap();
        assert_eq!(second, SessionId(2));
        app.session_coordinator.capture_started(second).unwrap();
        app.session_coordinator.capture_finalized(second).unwrap();
        assert_eq!(
            app.session_coordinator
                .start_request(second, ModelId::new("first"))
                .unwrap(),
            RequestId(1)
        );
        assert_eq!(
            app.session_coordinator
                .start_request(second, ModelId::new("second"))
                .unwrap(),
            RequestId(2)
        );
    }

    #[test]
    fn model_preload_is_dispatched_while_capture_is_active() {
        let mut app = test_app();
        let session_id = app
            .session_coordinator
            .begin(SessionPurpose::Dictation)
            .unwrap();
        app.session_coordinator.capture_started(session_id).unwrap();
        let model = config::configured_models(&app.config)
            .into_iter()
            .find(|model| model.id == "whisper_cpp_base_en")
            .unwrap();

        app.start_model_preload(session_id, model);

        assert!(matches!(
            app.session_coordinator.active().unwrap().model_load(),
            crate::core::ModelLoadState::Loading { .. }
        ));
    }

    #[test]
    fn stale_preload_completion_cannot_change_the_active_session() {
        let mut app = test_app();
        let session_id = app
            .session_coordinator
            .begin(SessionPurpose::Dictation)
            .unwrap();
        app.session_coordinator.capture_started(session_id).unwrap();
        let model_id = ModelId::new("whisper_cpp_base_en");
        app.session_coordinator
            .model_load_started(session_id, model_id.clone())
            .unwrap();
        app.tx
            .send(AppEvent::ModelPreloadFinished {
                session_id: SessionId(session_id.0 + 1),
                model_id: model_id.clone(),
                load_duration_ms: 1,
            })
            .unwrap();
        app.tx
            .send(AppEvent::ModelPreloadFinished {
                session_id,
                model_id,
                load_duration_ms: 2,
            })
            .unwrap();

        app.poll_events();

        assert!(matches!(
            app.session_coordinator.active().unwrap().model_load(),
            crate::core::ModelLoadState::Ready { .. }
        ));
    }

    #[test]
    fn current_normal_success_is_applied_and_completed() {
        let mut app = test_app();
        app.config.output.auto_insert_transcript = false;
        app.status = TranscriptionStatus::Transcribing;
        seed_test_request(
            &mut app,
            RecordingSource::Transcribe,
            SessionId(1),
            RequestId(10),
            "whisper_cpp_base_en",
        );
        let completed_at = Instant::now() - Duration::from_millis(10);
        let mut latency = LatencyTrace::started_at(
            completed_at - Duration::from_secs(1),
            TriggerObservation::AppAction,
        );
        latency.transcription_job_completed_at = Some(completed_at);
        app.tx
            .send(AppEvent::TranscriptionDone {
                source: RecordingSource::Transcribe,
                session_id: SessionId(1),
                request_id: RequestId(10),
                result: Box::new(test_transcription_outcome(
                    SessionId(1),
                    RequestId(10),
                    "accepted result",
                )),
                latency: Some(latency),
            })
            .unwrap();

        app.poll_events();

        assert_eq!(app.transcript, "accepted result");
        assert_eq!(app.raw_transcript, "accepted result");
        assert_eq!(app.status, TranscriptionStatus::Idle);
        assert!(app.status_message.contains("finished in 42 ms"));
        assert_eq!(app.session_coordinator.active_session_id(), None);
        let latency = app.latest_latency.as_ref().unwrap();
        assert_eq!(latency.final_text_ready_at, Some(completed_at));
        assert!(latency.output_started_at.is_none());
        assert!(latency.target_activated_at.is_none());
        assert!(latency.paste_completed_at.is_none());
    }

    #[test]
    fn empty_final_result_is_clean_no_speech_and_never_arms_output() {
        let mut app = test_app();
        let history_root = std::env::temp_dir().join(format!(
            "scribe-no-speech-history-{}-{}",
            std::process::id(),
            NEXT_TEST_SESSION.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&history_root);
        let history_store = HistoryStore::open(&history_root, HistoryRetentionPolicy::default())
            .expect("history store");
        let history_record = history_store
            .create_pending(
                NewHistoryEntry {
                    raw_text: String::new(),
                    model_id: "whisper_cpp_base_en".into(),
                    source_app: None,
                    metrics: HistoryMetrics::default(),
                },
                Some(test_prepared_audio().as_ref()),
            )
            .expect("pending history");
        app.history_store = Some(history_store.clone());
        app.config.output.auto_insert_transcript = true;
        app.status = TranscriptionStatus::Transcribing;
        app.transcript = "previous final".to_owned();
        app.raw_transcript = "previous raw".to_owned();
        let session_id = SessionId(1);
        let request_id = RequestId(10);
        seed_test_request(
            &mut app,
            RecordingSource::Transcribe,
            session_id,
            request_id,
            "whisper_cpp_base_en",
        );
        app.history_requests.insert(
            (session_id, request_id),
            HistoryRequestContext {
                id: history_record.id,
                kind: HistoryRequestKind::Dictation,
            },
        );
        app.overlay_controller
            .begin_session(session_id, NativeOverlayMode::Live);
        let now = Instant::now();
        let mut latency =
            LatencyTrace::started_at(now - Duration::from_secs(1), TriggerObservation::AppAction);
        latency.transcription_job_completed_at = Some(now - Duration::from_millis(10));
        latency.final_text_ready_at = Some(now - Duration::from_millis(9));
        latency.output_started_at = Some(now - Duration::from_millis(8));
        latency.target_activated_at = Some(now - Duration::from_millis(7));
        latency.paste_completed_at = Some(now - Duration::from_millis(6));
        latency.output_completed_at = Some(now - Duration::from_millis(5));
        app.tx
            .send(AppEvent::TranscriptionDone {
                source: RecordingSource::Transcribe,
                session_id,
                request_id,
                result: Box::new(test_transcription_outcome(session_id, request_id, " \n\t")),
                latency: Some(latency),
            })
            .unwrap();

        app.poll_events();
        app.poll_pending_output_with(|_, _, _| -> text_output::TextOutputResult {
            panic!("empty final text must not reach output")
        });

        assert_eq!(app.transcript, "previous final");
        assert_eq!(app.raw_transcript, "previous raw");
        assert!(app.pending_output.is_none());
        assert_eq!(app.status, TranscriptionStatus::Idle);
        assert_eq!(
            app.status_message,
            "No speech detected; nothing was pasted."
        );
        assert_eq!(
            app.session_coordinator.last_terminal().unwrap().outcome,
            crate::core::TerminalOutcome::Cancelled
        );
        let latency = app.latest_latency.as_ref().unwrap();
        assert!(latency.transcription_job_completed_at.is_some());
        assert!(latency.ui_result_at.is_some());
        assert!(latency.final_text_ready_at.is_none());
        assert!(latency.output_started_at.is_none());
        assert!(latency.target_activated_at.is_none());
        assert!(latency.paste_completed_at.is_none());
        assert!(latency.output_completed_at.is_none());
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let page = history_store.search(HistoryQuery::default()).unwrap();
            if page.records.is_empty() {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "no-speech history was not discarded"
            );
            thread::sleep(Duration::from_millis(10));
        }
        drop(app);
        drop(history_store);
        let _ = std::fs::remove_dir_all(history_root);
    }

    #[test]
    fn automatic_output_is_deferred_until_after_a_pasting_frame() {
        let mut app = test_app();
        app.config.output.auto_insert_transcript = true;
        app.status = TranscriptionStatus::Transcribing;
        let session_id = SessionId(1);
        let request_id = RequestId(10);
        seed_test_request(
            &mut app,
            RecordingSource::Transcribe,
            session_id,
            request_id,
            "whisper_cpp_base_en",
        );
        app.overlay_controller
            .begin_session(session_id, NativeOverlayMode::Live);
        app.tx
            .send(AppEvent::TranscriptionDone {
                source: RecordingSource::Transcribe,
                session_id,
                request_id,
                result: Box::new(test_transcription_outcome(
                    session_id,
                    request_id,
                    "accepted result",
                )),
                latency: None,
            })
            .unwrap();

        app.poll_events();
        app.sync_overlay_state();

        assert!(app.pending_output.is_some());
        assert_eq!(app.session_coordinator.phase(), DictationPhase::Output);
        assert_eq!(app.overlay_controller.state().phase, OverlayPhase::Pasting);
    }

    #[test]
    fn dictation_failure_schedules_overlay_cleanup() {
        let mut app = test_app();
        let session_id = SessionId(4);
        seed_test_session(&mut app, RecordingSource::Transcribe, session_id);
        app.overlay_controller
            .begin_session(session_id, NativeOverlayMode::Live);

        app.fail_dictation_session(session_id, "capture failed");

        assert_eq!(app.session_coordinator.active_session_id(), None);
        assert_eq!(app.overlay_controller.state().phase, OverlayPhase::Error);
        assert!(app.overlay_hide_at.is_some());
        assert_eq!(app.status_message, "capture failed");
    }

    #[test]
    fn stale_overlay_completion_cannot_hide_a_newer_session() {
        let mut app = test_app();
        let current = SessionId(12);
        app.begin_overlay_session(current, NativeOverlayMode::Live, None);

        app.finish_overlay_success(SessionId(11));
        app.finish_overlay_error(SessionId(11), "stale failure");

        assert_eq!(app.overlay_controller.state().session_id, Some(current));
        assert_eq!(
            app.overlay_controller.state().phase,
            OverlayPhase::Preparing
        );
        assert!(app.overlay_hide_at.is_none());
    }

    #[test]
    fn expired_overlay_deadline_hides_viewport_and_retires_target() {
        let mut app = test_app();
        let session_id = SessionId(13);
        app.begin_overlay_session(
            session_id,
            NativeOverlayMode::Live,
            Some(CapturedTarget::for_test(77, 88)),
        );
        app.finish_overlay_success(session_id);
        app.overlay_hide_at = Some(Instant::now() - Duration::from_millis(1));

        app.sync_overlay_state();

        assert_eq!(app.overlay_controller.state().phase, OverlayPhase::Hidden);
        assert!(!app.captured_targets.contains_key(&session_id));
        assert!(app.overlay_hide_at.is_none());
    }

    #[test]
    fn pending_output_is_applied_exactly_once_at_app_boundary() {
        use std::cell::Cell;

        let mut app = test_app();
        let session_id = SessionId(14);
        let request_id = RequestId(15);
        let model_id = ModelId::new("whisper_cpp_base_en");
        seed_test_request(
            &mut app,
            RecordingSource::Transcribe,
            session_id,
            request_id,
            model_id.as_str(),
        );
        app.session_coordinator
            .complete_request(session_id, request_id, &model_id)
            .unwrap();
        app.session_coordinator.begin_output(session_id).unwrap();
        app.overlay_controller
            .begin_session(session_id, NativeOverlayMode::Live);
        app.pending_output = Some(PendingOutput {
            session_id,
            history_id: None,
            transcript: "once".to_owned(),
            completion_message: "Complete".to_owned(),
            config: app.config.clone(),
            latency: None,
        });
        let calls = Cell::new(0_u32);

        app.poll_pending_output_with(|_, _, _| {
            calls.set(calls.get() + 1);
            text_output::TextOutputResult::Inserted
        });
        app.poll_pending_output_with(|_, _, _| {
            calls.set(calls.get() + 1);
            text_output::TextOutputResult::Inserted
        });

        assert_eq!(calls.get(), 1);
        assert!(app.pending_output.is_none());
        assert_eq!(app.session_coordinator.active_session_id(), None);
        assert_eq!(
            app.session_coordinator.last_terminal().unwrap().outcome,
            crate::core::TerminalOutcome::Completed
        );
    }

    #[test]
    fn armed_history_repaste_is_consumed_exactly_once() {
        use std::cell::Cell;

        let mut app = test_app();
        app.armed_history_repaste = Some(ArmedHistoryRepaste {
            id: 42,
            text: "paste once".to_owned(),
            expires_at: Instant::now() + Duration::from_secs(30),
        });
        let calls = Cell::new(0_u32);

        assert!(app.consume_armed_history_repaste_with(
            Instant::now(),
            || None,
            |text, config, target| {
                calls.set(calls.get() + 1);
                assert_eq!(text, "paste once");
                assert!(config.output.auto_insert_transcript);
                assert!(target.is_none());
                text_output::TextOutputResult::CopiedOnly(
                    text_output::CopyOnlyReason::TargetUnavailable,
                )
            },
        ));
        assert!(!app.consume_armed_history_repaste_with(
            Instant::now(),
            || None,
            |_, _, _| -> text_output::TextOutputResult {
                panic!("a consumed repaste must never output again")
            },
        ));
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn active_session_clears_armed_repaste_without_output() {
        use std::cell::Cell;

        let mut app = test_app();
        seed_test_request(
            &mut app,
            RecordingSource::Transcribe,
            SessionId(70),
            RequestId(71),
            "whisper_cpp_base_en",
        );
        app.armed_history_repaste = Some(ArmedHistoryRepaste {
            id: 42,
            text: "private old text".to_owned(),
            expires_at: Instant::now() + Duration::from_secs(30),
        });
        let calls = Cell::new(0_u32);

        assert!(!app.consume_armed_history_repaste_with(
            Instant::now(),
            || None,
            |_, _, _| {
                calls.set(calls.get() + 1);
                text_output::TextOutputResult::Inserted
            },
        ));
        assert_eq!(calls.get(), 0);
        assert!(app.armed_history_repaste.is_none());
    }

    #[test]
    fn history_completion_uses_immutable_final_text_and_config() {
        let mut app = test_app();
        app.config.output.auto_insert_transcript = true;
        let output_config = app.config.clone();
        let session_id = SessionId(72);
        let request_id = RequestId(73);
        let model_id = ModelId::new("whisper_cpp_base_en");
        seed_test_request(
            &mut app,
            RecordingSource::Transcribe,
            session_id,
            request_id,
            model_id.as_str(),
        );
        app.session_coordinator
            .complete_request(session_id, request_id, &model_id)
            .unwrap();
        app.transcript = "edited after finalization".to_owned();

        app.finish_transcription_after_history(PendingHistoryCompletion {
            session_id,
            history_id: 17,
            kind: HistoryRequestKind::Dictation,
            transcript: "accepted final".to_owned(),
            output_config,
            completion_message: "Complete".to_owned(),
            latency: None,
        });

        assert_eq!(
            app.pending_output
                .as_ref()
                .map(|pending| pending.transcript.as_str()),
            Some("accepted final")
        );
    }

    #[test]
    fn deleting_an_entry_clears_its_armed_repaste() {
        let mut app = test_app();
        app.armed_history_repaste = Some(ArmedHistoryRepaste {
            id: 19,
            text: "private old text".to_owned(),
            expires_at: Instant::now() + Duration::from_secs(30),
        });

        app.delete_history_entry(19);

        assert!(app.armed_history_repaste.is_none());
    }

    #[test]
    fn any_history_mutation_clears_armed_repaste_before_retention() {
        let mut app = test_app();
        app.armed_history_repaste = Some(ArmedHistoryRepaste {
            id: 23,
            text: "private old text".to_owned(),
            expires_at: Instant::now() + Duration::from_secs(30),
        });

        app.start_history_mutation("test mutation", |_| Ok(()));

        assert!(app.armed_history_repaste.is_none());
    }

    #[test]
    fn retention_is_deferred_while_retry_owns_history_row() {
        let mut app = test_app();
        app.history_requests.insert(
            (SessionId(80), RequestId(81)),
            HistoryRequestContext {
                id: 20,
                kind: HistoryRequestKind::Retry,
            },
        );
        app.leased_history_retry_ids.insert(20);
        let policy = HistoryRetentionPolicy {
            max_unpinned_entries: 1,
            transcript_retention_days: Some(1),
            audio_retention_days: Some(1),
        };

        app.start_history_retention_mutation(policy);

        assert_eq!(app.pending_history_retention_policy, Some(policy));
        assert!(app.history_mutation_in_flight.is_none());
        assert!(app.has_active_work());

        app.tx
            .send(AppEvent::HistoryRetryTerminalPersisted {
                history_id: 20,
                retry_lease_released: false,
                result: Err("release was not acknowledged".into()),
            })
            .unwrap();
        app.poll_events();
        assert!(app.history_retry_is_active());

        app.tx
            .send(AppEvent::HistoryRetryTerminalPersisted {
                history_id: 20,
                retry_lease_released: true,
                result: Ok(()),
            })
            .unwrap();
        app.poll_events();
        assert!(!app.history_retry_is_active());
    }

    #[test]
    fn transient_retry_release_failure_is_retried_until_acknowledged() {
        let mut attempts = 0;
        let mut pauses = Vec::new();
        let (released, result) = retry_release_until_acknowledged(
            || {
                attempts += 1;
                if attempts < 3 {
                    RetryReleaseAttempt::Retryable("queue temporarily full".into())
                } else {
                    RetryReleaseAttempt::Acknowledged {
                        retention_error: None,
                    }
                }
            },
            |duration| pauses.push(duration),
        );

        assert!(released);
        assert_eq!(result, Ok(()));
        assert_eq!(attempts, 3);
        assert_eq!(
            pauses,
            vec![Duration::from_millis(25), Duration::from_millis(50)]
        );
    }

    #[test]
    fn retention_error_does_not_revoke_retry_release_acknowledgement() {
        let (released, result) = retry_release_until_acknowledged(
            || RetryReleaseAttempt::Acknowledged {
                retention_error: Some("injected retention failure".into()),
            },
            |_| panic!("an acknowledged release must not retry"),
        );

        assert!(released);
        assert!(result.unwrap_err().contains("retention failed"));
    }

    #[test]
    fn accepted_retry_release_waits_for_a_late_acknowledgement() {
        let (reply, receiver) = crossbeam_channel::bounded(1);
        let sender = thread::spawn(move || {
            thread::sleep(Duration::from_millis(25));
            reply
                .send(crate::history::RetryReleaseAcknowledgement::default())
                .unwrap();
        });

        let result = observe_accepted_retry_release(receiver);
        sender.join().unwrap();
        assert!(matches!(
            result,
            RetryReleaseAttempt::Acknowledged {
                retention_error: None
            }
        ));
    }

    #[test]
    fn mutation_invalidates_in_flight_history_page_before_controls_reenable() {
        let mut app = test_app();
        let history_root = std::env::temp_dir().join(format!(
            "scribe-history-query-generation-{}-{}",
            std::process::id(),
            NEXT_TEST_SESSION.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&history_root);
        let store = HistoryStore::open(&history_root, HistoryRetentionPolicy::default()).unwrap();
        let record = store
            .create_pending(
                NewHistoryEntry {
                    raw_text: "fresh".into(),
                    model_id: "model".into(),
                    source_app: None,
                    metrics: HistoryMetrics::default(),
                },
                None,
            )
            .unwrap();
        store
            .complete(
                record.id,
                CompletedHistoryEntry {
                    raw_text: "fresh".into(),
                    final_text: "fresh".into(),
                    metrics: HistoryMetrics::default(),
                },
            )
            .unwrap();
        app.history_store = Some(store.clone());
        app.history_loading = true;
        app.active_history_query = Some(7);
        app.request_history_page(false);
        assert!(app.history_refresh_pending);
        app.tx
            .send(AppEvent::HistoryPageLoaded {
                query_id: 7,
                append: false,
                search: String::new(),
                result: Ok(HistoryPage {
                    records: Vec::new(),
                    next: None,
                }),
            })
            .unwrap();
        app.poll_events();
        assert!(app.history_loading);
        assert!(app.active_history_query.is_some());

        let deadline = Instant::now() + Duration::from_secs(2);
        while app.history_loading {
            app.poll_events();
            assert!(
                Instant::now() < deadline,
                "fresh history query did not finish"
            );
            thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(app.history_records.len(), 1);
        assert_eq!(app.history_records[0].id, record.id);
        drop(app);
        drop(store);
        let _ = std::fs::remove_dir_all(history_root);
    }

    #[test]
    fn recording_is_blocked_until_history_playback_is_terminal() {
        let mut app = test_app();
        app.playing_history_id = Some(21);
        app.history_playback_stopping = true;

        app.start_recording_at(
            RecordingSource::Transcribe,
            Instant::now(),
            TriggerObservation::AppAction,
        );

        assert!(app.pending_recording.is_none());
        assert_eq!(app.session_coordinator.active_session_id(), None);
        assert!(app.status_message.contains("Stop retained-audio playback"));
    }

    #[test]
    fn stop_during_playback_path_loading_keeps_ownership_until_path_result() {
        let mut app = test_app();
        app.history_playback = Some(PlaybackService::new().expect("playback worker"));
        app.playing_history_id = Some(22);

        app.apply_history_action(HistoryPageAction::StopPlayback);

        assert_eq!(app.playing_history_id, Some(22));
        assert!(app.history_playback_stopping);
        app.tx
            .send(AppEvent::HistoryPlaybackPathReady {
                history_id: 22,
                result: Ok(Some(PathBuf::from("must-not-open.wav"))),
            })
            .unwrap();
        app.poll_events();
        assert!(app.playing_history_id.is_none());
        assert!(!app.history_playback_stopping);
    }

    #[test]
    fn expired_history_repaste_does_not_output() {
        let mut app = test_app();
        let now = Instant::now();
        app.armed_history_repaste = Some(ArmedHistoryRepaste {
            id: 7,
            text: "expired".to_owned(),
            expires_at: now,
        });

        assert!(!app.consume_armed_history_repaste_with(
            now,
            || None,
            |_, _, _| -> text_output::TextOutputResult {
                panic!("expired repaste must not output")
            },
        ));
        assert!(app.armed_history_repaste.is_none());
    }

    #[test]
    fn completed_history_retry_never_arms_automatic_output() {
        let mut app = test_app();
        app.config.output.auto_insert_transcript = true;
        app.transcript = "retried transcript".to_owned();
        let session_id = SessionId(61);
        let request_id = RequestId(62);
        let model_id = ModelId::new("whisper_cpp_base_en");
        seed_test_request(
            &mut app,
            RecordingSource::Transcribe,
            session_id,
            request_id,
            model_id.as_str(),
        );
        app.session_coordinator
            .complete_request(session_id, request_id, &model_id)
            .unwrap();

        app.finish_transcription_after_history(PendingHistoryCompletion {
            session_id,
            history_id: 9,
            kind: HistoryRequestKind::Retry,
            transcript: "retried transcript".to_owned(),
            output_config: app.config.clone(),
            completion_message: "Retry finished".to_owned(),
            latency: None,
        });

        assert!(app.pending_output.is_none());
        assert_eq!(app.session_coordinator.active_session_id(), None);
        assert!(app.status_message.contains("nothing was pasted"));
    }

    #[test]
    fn clearing_transcript_cancels_correlated_output_and_retires_active_status() {
        let mut app = test_app();
        let session_id = SessionId(14);
        let request_id = RequestId(15);
        let model_id = ModelId::new("whisper_cpp_base_en");
        seed_test_request(
            &mut app,
            RecordingSource::Transcribe,
            session_id,
            request_id,
            model_id.as_str(),
        );
        app.session_coordinator
            .complete_request(session_id, request_id, &model_id)
            .unwrap();
        app.session_coordinator.begin_output(session_id).unwrap();
        app.begin_overlay_session(
            session_id,
            NativeOverlayMode::Live,
            Some(CapturedTarget::for_test(11, 22)),
        );
        app.transcript = "cleaned final".to_owned();
        app.raw_transcript = "raw private text".to_owned();
        app.status = TranscriptionStatus::Transcribing;
        app.pending_output = Some(PendingOutput {
            session_id,
            history_id: None,
            transcript: "final text edited after it was queued".to_owned(),
            completion_message: "Complete".to_owned(),
            config: app.config.clone(),
            latency: None,
        });

        app.clear_transcript_history();
        app.poll_pending_output_with(|_, _, _| -> text_output::TextOutputResult {
            panic!("cleared content must not be pasted")
        });

        assert!(app.transcript.is_empty());
        assert!(app.raw_transcript.is_empty());
        assert!(app.pending_output.is_none());
        assert!(!app.captured_targets.contains_key(&session_id));
        assert_eq!(app.status, TranscriptionStatus::Idle);
        assert_eq!(app.effective_status(), TranscriptionStatus::Idle);
        assert!(!app.has_active_work());
        assert_eq!(
            app.session_coordinator.last_terminal().unwrap().outcome,
            crate::core::TerminalOutcome::Cancelled
        );
    }

    #[test]
    fn discarded_pending_output_records_a_cancelled_diagnostic() {
        let mut app = test_app();
        let session_id = SessionId(14);
        let request_id = RequestId(15);
        let model_id = ModelId::new("whisper_cpp_base_en");
        seed_test_request(
            &mut app,
            RecordingSource::Transcribe,
            session_id,
            request_id,
            model_id.as_str(),
        );
        app.session_coordinator
            .complete_request(session_id, request_id, &model_id)
            .unwrap();
        app.session_coordinator.begin_output(session_id).unwrap();
        app.pending_output = Some(PendingOutput {
            session_id,
            history_id: None,
            transcript: "discarded".to_owned(),
            completion_message: "Complete".to_owned(),
            config: app.config.clone(),
            latency: Some(LatencyTrace::started_at(
                Instant::now(),
                TriggerObservation::AppAction,
            )),
        });

        app.stop_and_discard_active_recording();

        let diagnostics_root = std::env::temp_dir().join(format!(
            "scribe-discarded-output-diagnostics-{}-{}",
            std::process::id(),
            NEXT_TEST_SESSION.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&diagnostics_root);
        let report = diagnostics::export_redacted(&diagnostics_root, &app.diagnostics).unwrap();
        let report: serde_json::Value = serde_json::from_slice(&fs::read(report).unwrap()).unwrap();
        assert_eq!(report["sessions"][0]["outcome"], "cancelled");
        assert!(report["sessions"][0]["failure_stage"].is_null());
        let _ = fs::remove_dir_all(diagnostics_root);
    }

    #[test]
    fn successful_paste_with_restore_failure_keeps_timing_and_never_offers_retry() {
        let mut app = test_app();
        let session_id = SessionId(14);
        let request_id = RequestId(15);
        let model_id = ModelId::new("whisper_cpp_base_en");
        seed_test_request(
            &mut app,
            RecordingSource::Transcribe,
            session_id,
            request_id,
            model_id.as_str(),
        );
        app.session_coordinator
            .complete_request(session_id, request_id, &model_id)
            .unwrap();
        app.session_coordinator.begin_output(session_id).unwrap();
        app.overlay_controller
            .begin_session(session_id, NativeOverlayMode::Live);
        let activation = Instant::now();
        let paste = activation + Duration::from_millis(1);
        app.pending_output = Some(PendingOutput {
            session_id,
            history_id: None,
            transcript: "once".to_owned(),
            completion_message: "Complete".to_owned(),
            config: app.config.clone(),
            latency: Some(LatencyTrace::started_at(
                activation,
                TriggerObservation::HotkeyPoll,
            )),
        });

        app.poll_pending_output_with(|_, _, _| text_output::TextOutputOutcome {
            result: text_output::TextOutputResult::InsertedClipboardRestoreFailed(
                "restore failed".to_owned(),
            ),
            timing: text_output::TextOutputTiming {
                target_activated_at: Some(activation),
                paste_completed_at: Some(paste),
            },
        });

        let latency = app.latest_latency.as_ref().unwrap();
        assert_eq!(latency.target_activated_at, Some(activation));
        assert_eq!(latency.paste_completed_at, Some(paste));
        assert_eq!(app.status, TranscriptionStatus::Idle);
        assert_eq!(
            app.overlay_controller
                .state()
                .error
                .as_ref()
                .unwrap()
                .recovery,
            OverlayRecovery::None
        );
        assert_eq!(
            app.session_coordinator.last_terminal().unwrap().outcome,
            crate::core::TerminalOutcome::Completed
        );
    }

    #[test]
    fn blocked_preview_timeout_fails_without_final_pass_or_paste_and_reaps_worker() {
        use std::cell::Cell;
        use std::sync::{Condvar, Mutex, mpsc};

        let mut app = test_app();
        let session_id = app
            .session_coordinator
            .begin(SessionPurpose::Dictation)
            .unwrap();
        let model_id = ModelId::new("whisper_cpp_base_en");
        let request_id = app
            .session_coordinator
            .start_preview(session_id, model_id.clone())
            .unwrap();
        app.session_coordinator.capture_started(session_id).unwrap();
        app.session_coordinator
            .request_stop(session_id, StopReason::Explicit)
            .unwrap();
        app.overlay_controller
            .begin_session(session_id, NativeOverlayMode::Live);

        let identity = StreamIdentity {
            session_id,
            request_id,
            model_id,
            sequence: 0,
        };
        let (started_tx, started_rx) = mpsc::channel();
        let release = Arc::new((Mutex::new(false), Condvar::new()));
        let worker_release = Arc::clone(&release);
        let (publisher, preview) = RollingPreviewHandle::simulated(identity, move |snapshot| {
            started_tx.send(snapshot.identity.sequence).unwrap();
            let (lock, wake) = &*worker_release;
            let mut released = lock.lock().unwrap();
            while !*released {
                released = wake.wait(released).unwrap();
            }
            Ok(StreamUpdate::default())
        })
        .unwrap();
        assert!(publisher.publish_window(0, vec![0.0; 1_600]).unwrap());
        assert_eq!(started_rx.recv_timeout(Duration::from_secs(1)).unwrap(), 1);
        assert!(publisher.publish_window(0, vec![0.0; 1_600]).unwrap());
        app.rolling_preview = Some(preview);

        let started_at = Instant::now();
        let capture = FinishedCapture {
            session_id,
            source: RecordingSource::Transcribe,
            result: Ok(CaptureCompletion {
                audio: Some(test_prepared_audio()),
                stop_reason: CaptureStopReason::Explicit,
                metrics: CaptureMetrics {
                    duration: Duration::from_millis(100),
                    stop_trigger_elapsed: Duration::from_millis(100),
                    speech_trigger_elapsed: None,
                    source_sample_rate: 16_000,
                    source_channels: 1,
                    source_frames: 1_600,
                    prepared_frames: 1_600,
                    maximum_input_rms: 0.2,
                    maximum_input_peak: 0.4,
                    dropped_samples: 0,
                    stream_restarts: 0,
                },
            }),
            stop_requested: true,
            started_at,
            latency: LatencyTrace::started_at(started_at, TriggerObservation::HotkeyPoll),
            capture_diagnostics: CaptureDiagnosticContext::default(),
        };
        assert!(app.begin_preview_drain(
            session_id,
            PreviewDrainAction::FinishCapture(Box::new(capture))
        ));

        let poll_started = Instant::now();
        app.poll_preview_drain_at(poll_started, || panic!("cancelled before grace"));
        assert!(poll_started.elapsed() < Duration::from_millis(50));
        assert!(app.pending_preview_drain.is_some());
        assert!(app.capture_is_active());

        app.start_recording_at(
            RecordingSource::Transcribe,
            Instant::now(),
            TriggerObservation::HotkeyPoll,
        );
        assert_eq!(
            app.session_coordinator.active_session_id(),
            Some(session_id)
        );

        let cancel_calls = Cell::new(0_u32);
        let cancel_at =
            app.pending_preview_drain.as_ref().unwrap().closed_at + PREVIEW_FINISH_GRACE;
        app.poll_preview_drain_at(cancel_at, || cancel_calls.set(cancel_calls.get() + 1));
        app.poll_preview_drain_at(cancel_at + Duration::from_millis(1), || {
            cancel_calls.set(cancel_calls.get() + 1)
        });
        assert_eq!(cancel_calls.get(), 1);
        assert!(app.pending_preview_drain.is_some());
        app.poll_preview_drain_at(cancel_at + PREVIEW_CANCEL_ACK_WARNING, || {
            panic!("cancellation must only be requested once")
        });
        assert!(app.pending_preview_drain.is_some());
        assert!(app.capture_is_active());
        assert_eq!(app.session_coordinator.active_session_id(), None);
        assert_eq!(
            app.session_coordinator.last_terminal().unwrap().outcome,
            crate::core::TerminalOutcome::Failed
        );
        assert!(app.pending_output.is_none());
        assert!(
            app.status_message
                .contains("final transcription and paste were cancelled")
        );

        {
            let (lock, wake) = &*release;
            *lock.lock().unwrap() = true;
            wake.notify_one();
        }
        for _ in 0..100 {
            app.poll_preview_drain_at(Instant::now(), || panic!("duplicate cancellation"));
            if app.pending_preview_drain.is_none() {
                break;
            }
            thread::sleep(Duration::from_millis(2));
        }

        assert!(app.pending_preview_drain.is_none());
        assert!(!app.capture_is_active());
        assert!(
            app.status_message
                .contains("final transcription and paste were cancelled")
        );
        assert!(
            started_rx.recv_timeout(Duration::from_millis(20)).is_err(),
            "the pending snapshot should be dropped when preview closes"
        );
        app.poll_preview_drain_at(Instant::now(), || panic!("already finalized"));
        assert!(
            app.status_message
                .contains("final transcription and paste were cancelled")
        );
    }

    #[test]
    fn new_overlay_session_retires_the_previous_target() {
        let mut app = test_app();
        let previous_session = SessionId(8);
        app.begin_overlay_session(
            previous_session,
            NativeOverlayMode::Live,
            Some(CapturedTarget::for_test(44, 55)),
        );

        app.begin_overlay_session(SessionId(9), NativeOverlayMode::Live, None);

        assert!(!app.captured_targets.contains_key(&previous_session));
        assert_eq!(
            app.overlay_controller.state().session_id,
            Some(SessionId(9))
        );
    }

    #[test]
    fn current_normal_failure_is_applied_and_completed() {
        let mut app = test_app();
        app.status = TranscriptionStatus::Transcribing;
        seed_test_request(
            &mut app,
            RecordingSource::Transcribe,
            SessionId(1),
            RequestId(10),
            "whisper_cpp_base_en",
        );
        app.tx
            .send(AppEvent::TranscriptionFailed {
                source: RecordingSource::Transcribe,
                session_id: SessionId(1),
                request_id: RequestId(10),
                model_id: "whisper_cpp_base_en".to_owned(),
                message: "runtime stopped".to_owned(),
                latency: None,
            })
            .unwrap();

        app.poll_events();

        assert_eq!(app.status, TranscriptionStatus::Error);
        assert_eq!(app.status_message, "runtime stopped");
        assert_eq!(app.session_coordinator.active_session_id(), None);
    }

    #[test]
    fn current_playground_result_finishes_and_cleans_audio_once() {
        let mut app = test_app();
        let model_id = app.playground_cards[0].descriptor.id.as_str().to_owned();
        let session_id = SessionId(3);
        let request_id = RequestId(30);
        app.status = TranscriptionStatus::Transcribing;
        seed_test_request(
            &mut app,
            RecordingSource::Playground,
            session_id,
            request_id,
            &model_id,
        );
        app.playground_pending = 1;
        let audio = test_prepared_audio();
        let released = Arc::downgrade(&audio);
        app.playground_runs.insert(
            session_id,
            PlaygroundRunState {
                pending_requests: HashMap::from([(request_id, model_id.clone())]),
                _audio: audio,
            },
        );
        app.tx
            .send(AppEvent::TranscriptionDone {
                source: RecordingSource::Playground,
                session_id,
                request_id,
                result: Box::new(test_transcription_outcome_for_model(
                    session_id,
                    request_id,
                    &model_id,
                    "accepted playground result",
                )),
                latency: None,
            })
            .unwrap();

        app.poll_events();

        assert!(released.upgrade().is_none());
        assert_eq!(app.status, TranscriptionStatus::Idle);
        assert_eq!(app.playground_pending, 0);
        assert_eq!(app.session_coordinator.active_session_id(), None);
        assert!(!app.playground_runs.contains_key(&session_id));
        let card = app
            .playground_cards
            .iter()
            .find(|card| card.descriptor.id.as_str() == model_id)
            .unwrap();
        assert_eq!(card.transcript, "accepted playground result");
    }

    #[test]
    fn current_playground_failure_finishes_and_cleans_audio_once() {
        let mut app = test_app();
        let model_id = app.playground_cards[0].descriptor.id.as_str().to_owned();
        let session_id = SessionId(4);
        let request_id = RequestId(40);
        app.status = TranscriptionStatus::Transcribing;
        seed_test_request(
            &mut app,
            RecordingSource::Playground,
            session_id,
            request_id,
            &model_id,
        );
        app.playground_pending = 1;
        let audio = test_prepared_audio();
        let released = Arc::downgrade(&audio);
        app.playground_runs.insert(
            session_id,
            PlaygroundRunState {
                pending_requests: HashMap::from([(request_id, model_id.clone())]),
                _audio: audio,
            },
        );
        app.tx
            .send(AppEvent::TranscriptionFailed {
                source: RecordingSource::Playground,
                session_id,
                request_id,
                model_id: model_id.clone(),
                message: "expected failure".to_owned(),
                latency: None,
            })
            .unwrap();

        app.poll_events();

        assert!(released.upgrade().is_none());
        assert_eq!(app.status, TranscriptionStatus::Error);
        assert_eq!(app.status_message, "expected failure");
        assert_eq!(app.playground_pending, 0);
        assert_eq!(app.session_coordinator.active_session_id(), None);
        assert!(!app.playground_runs.contains_key(&session_id));
        let card = app
            .playground_cards
            .iter()
            .find(|card| card.descriptor.id.as_str() == model_id)
            .unwrap();
        assert_eq!(
            card.status,
            ModelRuntimeStatus::Error("expected failure".to_owned())
        );
    }

    #[test]
    fn comparison_runs_one_model_at_a_time_and_keeps_success_after_failure() {
        let mut app = test_app();
        app.transcript = "keep active transcript".to_owned();
        let first_id = app.playground_cards[0].descriptor.id.as_str().to_owned();
        let second_id = "comparison_second_model".to_owned();
        let mut second_card = app.playground_cards[0].clone();
        second_card.descriptor.id = ModelId::new(&second_id);
        app.playground_cards.push(second_card);
        for card in &mut app.playground_cards {
            card.status = ModelRuntimeStatus::Ready;
            card.audio_duration_ms = Some(1_000);
        }

        let session_id = SessionId(44);
        let first_request = RequestId(440);
        let second_request = RequestId(441);
        app.session_coordinator.seed_active_for_test(
            session_id,
            SessionPurpose::Comparison,
            [
                (first_request, ModelId::new(&first_id)),
                (second_request, ModelId::new(&second_id)),
            ],
        );
        app.playground_pending = 2;
        app.comparison_run_model_ids = Some(vec![first_id.clone(), second_id.clone()]);
        app.comparison_started_at = Some(Instant::now());
        app.model_comparison
            .selected_model_ids
            .extend([first_id.clone(), second_id.clone()]);
        app.model_comparison.reference_draft = "hello world".to_owned();
        app.model_comparison.reference_transcript = Some("hello world".to_owned());
        let audio = test_prepared_audio();
        let released = Arc::downgrade(&audio);
        app.playground_runs.insert(
            session_id,
            PlaygroundRunState {
                pending_requests: HashMap::from([
                    (first_request, first_id.clone()),
                    (second_request, second_id.clone()),
                ]),
                _audio: audio,
            },
        );

        app.tx
            .send(AppEvent::PlaygroundModelStarted {
                session_id,
                request_id: first_request,
                model_id: first_id.clone(),
            })
            .unwrap();
        app.poll_events();
        assert_eq!(app.playground_cards[0].status, ModelRuntimeStatus::Running);
        assert_eq!(app.playground_cards[1].status, ModelRuntimeStatus::Ready);

        app.tx
            .send(AppEvent::TranscriptionFailed {
                source: RecordingSource::Playground,
                session_id,
                request_id: first_request,
                model_id: first_id.clone(),
                message: "first failed".to_owned(),
                latency: None,
            })
            .unwrap();
        app.tx
            .send(AppEvent::PlaygroundModelStarted {
                session_id,
                request_id: second_request,
                model_id: second_id.clone(),
            })
            .unwrap();
        app.poll_events();
        assert!(matches!(
            app.playground_cards[0].status,
            ModelRuntimeStatus::Error(_)
        ));
        assert_eq!(app.playground_cards[1].status, ModelRuntimeStatus::Running);
        assert_eq!(app.playground_pending, 1);
        assert!(released.upgrade().is_some());

        app.tx
            .send(AppEvent::TranscriptionDone {
                source: RecordingSource::Playground,
                session_id,
                request_id: second_request,
                result: Box::new(test_transcription_outcome_for_model(
                    session_id,
                    second_request,
                    &second_id,
                    "hello world",
                )),
                latency: None,
            })
            .unwrap();
        app.poll_events();
        app.sync_model_comparison_state();

        assert!(released.upgrade().is_none());
        assert_eq!(app.playground_pending, 0);
        assert_eq!(app.model_comparison.phase, ComparisonPhase::Complete);
        assert_eq!(app.transcript, "keep active transcript");
        assert_eq!(app.model_comparison.results.len(), 2);
        assert_eq!(
            app.model_comparison.results[0].1.phase,
            ComparisonResultPhase::Error
        );
        assert_eq!(app.model_comparison.results[1].1.word_error_rate, Some(0.0));
    }

    #[test]
    fn comparison_projection_recomputes_wer_only_for_changed_revisions() {
        let mut app = test_app();
        let first_id = app.playground_cards[0].descriptor.id.as_str().to_owned();
        let second_id = "comparison-cache-second".to_owned();
        let mut second_card = app.playground_cards[0].clone();
        second_card.descriptor.id = ModelId::new(&second_id);
        app.playground_cards.push(second_card);
        app.playground_cards[0].status = ModelRuntimeStatus::Ready;
        app.playground_cards[0].transcript = "hello brave world".to_owned();
        app.playground_cards[0].latency_ms = Some(100);
        app.playground_cards[1].status = ModelRuntimeStatus::Ready;
        app.playground_cards[1].transcript = "hello word".to_owned();
        app.playground_cards[1].latency_ms = Some(120);
        app.comparison_run_model_ids = Some(vec![first_id.clone(), second_id.clone()]);
        app.comparison_started_at = Some(Instant::now());
        app.set_comparison_reference(Some("hello world".to_owned()));

        app.sync_model_comparison_state();

        assert_eq!(app.comparison_wer_compute_count, 2);
        assert_eq!(app.comparison_output_replacement_count, 2);
        assert_eq!(
            app.model_comparison.results[0].1.word_error_rate,
            Some(benchmark::calculate_wer("hello world", "hello brave world") as f32)
        );
        let output_pointers = app
            .model_comparison
            .results
            .iter()
            .map(|(_, result)| result.output.as_ref().unwrap().as_ptr())
            .collect::<Vec<_>>();

        app.sync_model_comparison_state();

        assert_eq!(app.comparison_wer_compute_count, 2);
        assert_eq!(app.comparison_output_replacement_count, 2);
        assert_eq!(
            app.model_comparison
                .results
                .iter()
                .map(|(_, result)| result.output.as_ref().unwrap().as_ptr())
                .collect::<Vec<_>>(),
            output_pointers
        );

        app.playground_cards[0].transcript = "hello world".to_owned();
        app.mark_comparison_output_changed(&first_id);
        app.sync_model_comparison_state();

        assert_eq!(app.comparison_wer_compute_count, 3);
        assert_eq!(app.comparison_output_replacement_count, 3);
        assert_eq!(app.model_comparison.results[0].1.word_error_rate, Some(0.0));

        app.set_comparison_reference(Some("hello brave world".to_owned()));
        app.sync_model_comparison_state();

        assert_eq!(app.comparison_wer_compute_count, 5);
        assert_eq!(app.comparison_output_replacement_count, 3);
        assert_eq!(
            app.model_comparison.results[0].1.word_error_rate,
            Some(benchmark::calculate_wer("hello brave world", "hello world") as f32)
        );
        assert_eq!(
            app.model_comparison.results[1].1.word_error_rate,
            Some(benchmark::calculate_wer("hello brave world", "hello word") as f32)
        );
    }

    #[test]
    fn comparison_projection_drops_cached_output_when_a_card_is_removed() {
        let mut app = test_app();
        let removed_id = "comparison-removed-model".to_owned();
        let mut removed_card = app.playground_cards[0].clone();
        removed_card.descriptor.id = ModelId::new(&removed_id);
        removed_card.transcript = "cached output".to_owned();
        removed_card.latency_ms = Some(50);
        app.playground_cards.push(removed_card);
        app.comparison_run_model_ids = Some(vec![removed_id.clone()]);
        app.set_comparison_reference(Some("cached output".to_owned()));
        app.sync_model_comparison_state();
        assert_eq!(
            app.model_comparison.results[0].1.output.as_deref(),
            Some("cached output")
        );

        app.refresh_playground_cards_from_config();
        app.sync_model_comparison_state();

        assert!(app.model_comparison.results[0].1.output.is_none());
        assert!(app.model_comparison.results[0].1.word_error_rate.is_none());
    }

    #[test]
    fn models_render_clears_reference_focus_requests_and_notice_after_one_frame() {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        configure_stitch_style(&ctx);
        let mut app = test_app();
        app.model_comparison.expanded = true;
        app.model_comparison.reference_editor_visible = true;
        app.model_comparison.focus_reference_editor = true;

        let render = |ctx: &egui::Context, app: &mut LocalTranscriberApp| {
            ctx.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1_180.0, 815.0),
                    )),
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default()
                        .frame(content_panel_frame(ctx))
                        .show(ctx, |ui| app.ui_models(ui));
                },
            )
        };

        let output = render(&ctx, &mut app);
        let update = output.platform_output.accesskit_update.as_ref().unwrap();
        assert_eq!(
            update
                .nodes
                .iter()
                .find(|(id, _)| *id == update.focus)
                .and_then(|(_, node)| node.name()),
            Some("Reference transcript")
        );
        assert!(!app.model_comparison.focus_reference_editor);

        app.model_comparison.reference_editor_visible = false;
        app.model_comparison.reference_draft = "spoken words".into();
        app.model_comparison.reference_transcript = Some("spoken words".into());
        app.model_comparison.restore_reference_action_focus = true;
        app.model_comparison.reference_notice = Some("Reference transcript applied.".into());
        let output = render(&ctx, &mut app);
        let update = output.platform_output.accesskit_update.as_ref().unwrap();
        assert_eq!(
            update
                .nodes
                .iter()
                .find(|(id, _)| *id == update.focus)
                .and_then(|(_, node)| node.name()),
            Some("Edit reference")
        );
        assert!(update.nodes.iter().any(|(_, node)| {
            node.name() == Some("Reference transcript applied.")
                && node.live() == Some(egui::accesskit::Live::Polite)
                && node.is_live_atomic()
        }));
        assert!(!app.model_comparison.restore_reference_action_focus);
        assert_eq!(app.model_comparison.reference_notice, None);
    }

    #[test]
    fn live_model_projection_exposes_details_actions_for_ready_models() {
        let mut app = test_app();
        let base_fixture = install_test_catalog_model(&mut app, "whisper_cpp_base_en");

        let selected = app.transcribe_screen_models();
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].display_name, "Whisper Tiny — English");
        assert_eq!(selected[0].variant_label, "tiny.en");

        app.config.general.selected_default_model = "not-selected".to_owned();
        app.remote_catalog.invalidate_local_models();
        app.rebuild_model_inventory_projection();

        let ready = app
            .model_management_catalog()
            .into_iter()
            .find(|model| model.id == "whisper_cpp_tiny_en")
            .expect("tiny model should be projected");
        assert!(ready.installed);
        assert!(ready.ready);
        assert!(!ready.active);
        assert_eq!(ready.primary_action_label, "Use this model");
        assert!(ready.primary_action_enabled);
        assert!(!ready.primary_action_repairs_runtime);
        assert_eq!(ready.primary_action_disabled_reason, None);
        assert_eq!(ready.display_name, "Whisper Tiny — English");
        assert_eq!(ready.variant_label, "tiny.en");

        assert!(
            app.model_management_catalog()
                .iter()
                .any(|model| model.id == "whisper_cpp_base_en" && model.installed),
            "an installed retained-compatibility model must remain manageable"
        );

        app.config.general.selected_default_model = "whisper_cpp_tiny_en".to_owned();
        app.remote_catalog.invalidate_local_models();
        app.rebuild_model_inventory_projection();
        let active = app
            .model_management_catalog()
            .into_iter()
            .find(|model| model.id == "whisper_cpp_tiny_en")
            .expect("active tiny model should be projected");
        assert!(active.active);
        assert_eq!(active.primary_action_label, "Active");
        assert!(!active.primary_action_enabled);
        assert_eq!(
            active.primary_action_disabled_reason.as_deref(),
            Some("This model is already active.")
        );
        let _ = fs::remove_file(base_fixture);
    }

    #[test]
    fn live_model_projection_exposes_repair_for_an_installed_runtime_failure() {
        let app = test_app();
        let descriptor = app
            .transcription_service
            .model_descriptor(&ModelId::new("whisper_cpp_tiny_en"))
            .unwrap();
        let model = SttModelInfo {
            id: "installed-without-provider".to_owned(),
            name: "Local compatibility model".to_owned(),
            backend: "Unavailable runtime".to_owned(),
            description: "Installed model with a missing runtime provider.".to_owned(),
            expected_ram: "1 GB".to_owned(),
            accuracy_tier: "Unknown".to_owned(),
            speed_tier: "Unknown".to_owned(),
            local_path: None,
            install_status: ModelInstallStatus::Installed,
            download_model: None,
        };

        let projected = app.model_management_view_model(&model, Some(&descriptor));

        assert!(projected.installed);
        assert!(!projected.ready);
        assert_eq!(projected.primary_action_label, "Repair runtime");
        assert!(projected.primary_action_repairs_runtime);
        assert!(!projected.primary_action_enabled);
        assert_eq!(
            projected.primary_action_disabled_reason.as_deref(),
            Some("This model has no compatible local provider.")
        );
    }

    #[test]
    fn descriptorless_installed_compatibility_model_remains_manageable_and_selectable() {
        let mut app = test_app();
        let id = "vosk_small_en";
        let root = std::env::temp_dir().join(format!(
            "scribe-app-vosk-{}-{}",
            std::process::id(),
            NEXT_TEST_SESSION.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&root);
        for directory in ["am", "conf", "graph"] {
            fs::create_dir_all(root.join(directory)).unwrap();
        }
        fs::write(root.join("am").join("final.mdl"), b"model").unwrap();
        fs::write(root.join("conf").join("model.conf"), b"conf").unwrap();
        fs::write(root.join("graph").join("HCLG.fst"), b"graph").unwrap();
        app.config
            .general
            .model_paths
            .insert(id.to_owned(), root.clone());
        config::normalize_config(&mut app.config);
        app.remote_catalog.invalidate_local_models();
        app.rebuild_model_inventory_projection();
        assert!(
            app.transcription_service
                .model_descriptor(&ModelId::new(id))
                .is_err(),
            "the fixture must exercise the descriptor-less compatibility path"
        );

        let projected = app
            .model_management_catalog()
            .into_iter()
            .find(|model| model.id == id)
            .expect("installed compatibility model must remain visible");
        assert!(projected.installed);
        assert_eq!(projected.display_name, "Vosk small English");
        assert_eq!(projected.variant_label, "small.en");
        assert_eq!(projected.compatibility, ModelCompatibility::Incompatible);

        app.model_downloads
            .insert(id.to_owned(), ModelInstallStatus::InstallingRuntime);
        app.remote_catalog.invalidate_local_models();
        app.rebuild_model_inventory_projection();
        let repairing = app
            .model_management_catalog()
            .into_iter()
            .find(|model| model.id == id)
            .expect("compatibility model must remain visible during runtime repair");
        assert!(repairing.installed);
        assert!(!repairing.ready);
        assert_eq!(repairing.download_state, ModelDownloadState::Verifying);
        assert_eq!(repairing.primary_action_label, "Repair runtime");

        app.model_downloads.insert(
            id.to_owned(),
            ModelInstallStatus::RuntimeError("runtime repair failed".into()),
        );
        app.remote_catalog.invalidate_local_models();
        app.rebuild_model_inventory_projection();
        let failed = app
            .model_management_catalog()
            .into_iter()
            .find(|model| model.id == id)
            .expect("compatibility model must remain visible after runtime repair fails");
        assert!(failed.installed);
        assert!(!failed.ready);
        assert_eq!(failed.download_state, ModelDownloadState::Failed);
        assert_eq!(failed.primary_action_label, "Repair runtime");

        app.config.general.selected_default_model = id.to_owned();
        assert_eq!(app.selected_model_ui_label(), "Vosk small English");
        let selected = app.transcribe_screen_models();
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].id, id);
        assert_eq!(selected[0].display_name, "Vosk small English");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn initial_runtime_preparation_does_not_claim_the_model_artifact_is_installed() {
        let mut app = test_app();
        let id = "whisper_cpp_base_en";
        let mut model = config::configured_models(&app.config)
            .into_iter()
            .find(|model| model.id == id)
            .unwrap();
        model.local_path = None;
        model.install_status = ModelInstallStatus::NotInstalled;
        let descriptor = app
            .transcription_service
            .model_descriptor(&ModelId::new(id))
            .unwrap();
        app.model_downloads
            .insert(id.to_owned(), ModelInstallStatus::InstallingRuntime);

        let projected = app.model_management_view_model(&model, Some(&descriptor));

        assert!(!projected.installed);
        assert!(!projected.ready);
        assert_eq!(projected.download_state, ModelDownloadState::Verifying);
        assert_eq!(projected.primary_action_label, "Not installed");
    }

    #[test]
    fn blocked_model_selection_keeps_the_details_dialog_open() {
        let mut app = test_app();
        let selected = app.config.general.selected_default_model.clone();
        app.model_management.dialog = Some(ModelDialog::Details("whisper_cpp_tiny_en".into()));
        app.artifact_recovery_error = Some("Resolve artifact recovery first.".into());

        app.apply_model_management_action(ScreenAction::SelectModel("whisper_cpp_tiny_en".into()));

        assert_eq!(app.config.general.selected_default_model, selected);
        assert_eq!(
            app.model_management.dialog,
            Some(ModelDialog::Details("whisper_cpp_tiny_en".into()))
        );
        assert_eq!(app.status_message, "Resolve artifact recovery first.");
        assert_eq!(app.model_management.restore_details_focus, None);
    }

    #[test]
    fn live_compare_selection_rejects_a_fifth_model_and_locks_while_busy() {
        let mut app = test_app();
        app.model_comparison
            .selected_model_ids
            .extend(["one", "two", "three", "four"].map(str::to_owned));

        app.apply_model_management_action(ScreenAction::ToggleComparisonModel("five".into()));

        assert!(!app.model_comparison.selected_model_ids.contains("five"));
        assert_eq!(
            app.model_comparison.selection_feedback.as_deref(),
            Some("A comparison can include at most four models.")
        );

        app.model_comparison.phase = ComparisonPhase::Recording;
        app.apply_model_management_action(ScreenAction::ToggleComparisonModel("one".into()));
        assert!(app.model_comparison.selected_model_ids.contains("one"));
        assert_eq!(
            app.model_comparison.selection_feedback.as_deref(),
            Some("Model selection is locked during a comparison.")
        );
    }

    #[test]
    fn comparison_start_does_not_supersede_active_dictation_or_output() {
        for output_pending in [false, true] {
            let mut app = test_app();
            let session_id = SessionId(44);
            let request_id = RequestId(440);
            let model_id = ModelId::new("whisper_cpp_base_en");
            app.transcript = "keep transcript".to_owned();
            app.raw_transcript = "keep raw transcript".to_owned();
            app.status = TranscriptionStatus::Transcribing;
            app.model_comparison
                .selected_model_ids
                .extend(["one".to_owned(), "two".to_owned()]);
            seed_test_request(
                &mut app,
                RecordingSource::Transcribe,
                session_id,
                request_id,
                model_id.as_str(),
            );
            if output_pending {
                app.session_coordinator
                    .complete_request(session_id, request_id, &model_id)
                    .unwrap();
                app.session_coordinator.begin_output(session_id).unwrap();
                app.pending_output = Some(PendingOutput {
                    session_id,
                    history_id: None,
                    transcript: "queued output".to_owned(),
                    completion_message: "Complete".to_owned(),
                    config: app.config.clone(),
                    latency: None,
                });
            }
            let phase = app.session_coordinator.phase();
            let history = app.history_records.clone();

            app.apply_model_management_action(ScreenAction::StartComparison);

            assert_eq!(
                app.session_coordinator.active_session_id(),
                Some(session_id)
            );
            assert_eq!(
                app.session_coordinator.active_purpose(),
                Some(SessionPurpose::Dictation)
            );
            assert_eq!(app.session_coordinator.phase(), phase);
            assert_eq!(app.transcript, "keep transcript");
            assert_eq!(app.raw_transcript, "keep raw transcript");
            assert_eq!(app.history_records, history);
            assert_eq!(
                app.pending_output
                    .as_ref()
                    .map(|pending| pending.session_id),
                output_pending.then_some(session_id)
            );
            assert!(app.comparison_run_model_ids.is_none());
            assert_eq!(app.model_comparison.phase, ComparisonPhase::Idle);
            assert_eq!(
                app.model_comparison.start_disabled_reason.as_deref(),
                Some("Finish the current dictation before starting a comparison.")
            );
        }
    }

    #[test]
    fn mismatched_service_ids_are_rejected_without_output() {
        let mut app = test_app();
        app.config.output.auto_insert_transcript = false;
        app.transcript = "preserve me".to_owned();
        app.status = TranscriptionStatus::Transcribing;
        seed_test_request(
            &mut app,
            RecordingSource::Transcribe,
            SessionId(4),
            RequestId(40),
            "whisper_cpp_base_en",
        );
        app.tx
            .send(AppEvent::TranscriptionDone {
                source: RecordingSource::Transcribe,
                session_id: SessionId(4),
                request_id: RequestId(40),
                result: Box::new(test_transcription_outcome(
                    SessionId(4),
                    RequestId(41),
                    "must not be applied",
                )),
                latency: None,
            })
            .unwrap();

        app.poll_events();

        assert_eq!(app.transcript, "preserve me");
        assert_eq!(app.status, TranscriptionStatus::Error);
        assert!(app.status_message.contains("mismatched correlation IDs"));
        assert_eq!(app.session_coordinator.active_session_id(), None);
    }

    #[test]
    fn playground_correlation_error_is_not_overwritten_by_cleanup() {
        let mut app = test_app();
        let model_id = app.playground_cards[0].descriptor.id.as_str().to_owned();
        let session_id = SessionId(5);
        let request_id = RequestId(50);
        app.status = TranscriptionStatus::Transcribing;
        seed_test_request(
            &mut app,
            RecordingSource::Playground,
            session_id,
            request_id,
            &model_id,
        );
        app.playground_pending = 1;
        let audio = test_prepared_audio();
        let released = Arc::downgrade(&audio);
        app.playground_runs.insert(
            session_id,
            PlaygroundRunState {
                pending_requests: HashMap::from([(request_id, model_id.clone())]),
                _audio: audio,
            },
        );
        app.tx
            .send(AppEvent::TranscriptionDone {
                source: RecordingSource::Playground,
                session_id,
                request_id,
                result: Box::new(test_transcription_outcome_for_model(
                    session_id,
                    RequestId(51),
                    &model_id,
                    "must not be applied",
                )),
                latency: None,
            })
            .unwrap();

        app.poll_events();

        assert!(released.upgrade().is_none());
        assert_eq!(app.status, TranscriptionStatus::Error);
        assert!(app.status_message.contains("mismatched correlation IDs"));
        assert_eq!(app.playground_pending, 0);
        assert_eq!(app.session_coordinator.active_session_id(), None);
        let card = app
            .playground_cards
            .iter()
            .find(|card| card.descriptor.id.as_str() == model_id)
            .unwrap();
        assert!(matches!(card.status, ModelRuntimeStatus::Error(_)));
    }

    #[test]
    fn playground_wrong_model_result_is_rejected() {
        let mut app = test_app();
        let expected_model_id = app.playground_cards[0].descriptor.id.as_str().to_owned();
        let session_id = SessionId(6);
        let request_id = RequestId(60);
        app.status = TranscriptionStatus::Transcribing;
        seed_test_request(
            &mut app,
            RecordingSource::Playground,
            session_id,
            request_id,
            &expected_model_id,
        );
        app.playground_pending = 1;
        let audio = test_prepared_audio();
        let released = Arc::downgrade(&audio);
        app.playground_runs.insert(
            session_id,
            PlaygroundRunState {
                pending_requests: HashMap::from([(request_id, expected_model_id.clone())]),
                _audio: audio,
            },
        );
        app.tx
            .send(AppEvent::TranscriptionDone {
                source: RecordingSource::Playground,
                session_id,
                request_id,
                result: Box::new(test_transcription_outcome_for_model(
                    session_id,
                    request_id,
                    "wrong-model",
                    "must not be applied",
                )),
                latency: None,
            })
            .unwrap();

        app.poll_events();

        assert!(released.upgrade().is_none());
        assert_eq!(app.status, TranscriptionStatus::Error);
        assert!(app.status_message.contains("wrong model"));
        let card = app
            .playground_cards
            .iter()
            .find(|card| card.descriptor.id.as_str() == expected_model_id)
            .unwrap();
        assert!(matches!(card.status, ModelRuntimeStatus::Error(_)));
        assert!(card.transcript.is_empty());
    }

    #[test]
    fn stale_normal_events_cannot_replace_text_or_status() {
        let mut app = test_app();
        app.transcript = "newer transcript".to_owned();
        app.status = TranscriptionStatus::Listening;
        app.status_message = "newer session is listening".to_owned();
        seed_test_request(
            &mut app,
            RecordingSource::Transcribe,
            SessionId(2),
            RequestId(20),
            "whisper_cpp_base_en",
        );

        app.tx
            .send(AppEvent::TranscriptionDone {
                source: RecordingSource::Transcribe,
                session_id: SessionId(1),
                request_id: RequestId(10),
                result: Box::new(test_transcription_outcome(
                    SessionId(1),
                    RequestId(10),
                    "obsolete success",
                )),
                latency: None,
            })
            .unwrap();
        app.tx
            .send(AppEvent::TranscriptionFailed {
                source: RecordingSource::Transcribe,
                session_id: SessionId(1),
                request_id: RequestId(11),
                model_id: "whisper_cpp_base_en".to_owned(),
                message: "obsolete failure".to_owned(),
                latency: None,
            })
            .unwrap();

        app.poll_events();

        assert_eq!(app.transcript, "newer transcript");
        assert_eq!(app.status, TranscriptionStatus::Listening);
        assert_eq!(app.status_message, "newer session is listening");
        assert_eq!(
            app.session_coordinator.active_session_id(),
            Some(SessionId(2))
        );
    }

    #[test]
    fn cross_source_completion_cannot_overwrite_the_newer_session() {
        let mut app = test_app();
        app.transcript = "preserve me".to_owned();
        app.status = TranscriptionStatus::Listening;
        app.status_message = "Playground is listening".to_owned();
        seed_test_session(&mut app, RecordingSource::Playground, SessionId(2));

        app.tx
            .send(AppEvent::TranscriptionDone {
                source: RecordingSource::Transcribe,
                session_id: SessionId(1),
                request_id: RequestId(10),
                result: Box::new(test_transcription_outcome(
                    SessionId(1),
                    RequestId(10),
                    "obsolete normal result",
                )),
                latency: None,
            })
            .unwrap();

        app.poll_events();

        assert_eq!(app.transcript, "preserve me");
        assert_eq!(app.status, TranscriptionStatus::Listening);
        assert_eq!(app.status_message, "Playground is listening");
        assert_eq!(
            app.session_coordinator.active_session_id(),
            Some(SessionId(2))
        );
    }

    #[test]
    fn stale_playground_completion_cannot_overwrite_newer_normal_session() {
        let stale_audio = test_prepared_audio();
        let released = Arc::downgrade(&stale_audio);
        let mut app = test_app();
        app.transcript = "preserve me".to_owned();
        app.status = TranscriptionStatus::Listening;
        app.status_message = "Normal dictation is listening".to_owned();
        seed_test_request(
            &mut app,
            RecordingSource::Transcribe,
            SessionId(2),
            RequestId(20),
            "whisper_cpp_base_en",
        );
        app.playground_runs.insert(
            SessionId(1),
            PlaygroundRunState {
                pending_requests: HashMap::from([(
                    RequestId(10),
                    "whisper_cpp_base_en".to_owned(),
                )]),
                _audio: stale_audio,
            },
        );
        app.tx
            .send(AppEvent::TranscriptionDone {
                source: RecordingSource::Playground,
                session_id: SessionId(1),
                request_id: RequestId(10),
                result: Box::new(test_transcription_outcome(
                    SessionId(1),
                    RequestId(10),
                    "obsolete playground result",
                )),
                latency: None,
            })
            .unwrap();

        app.poll_events();

        assert!(released.upgrade().is_none());
        assert_eq!(app.transcript, "preserve me");
        assert_eq!(app.status, TranscriptionStatus::Listening);
        assert_eq!(app.status_message, "Normal dictation is listening");
        assert_eq!(
            app.session_coordinator.active_session_id(),
            Some(SessionId(2))
        );

        app.tx
            .send(AppEvent::TranscriptionFailed {
                source: RecordingSource::Playground,
                session_id: SessionId(1),
                request_id: RequestId(10),
                model_id: "whisper_cpp_base_en".to_owned(),
                message: "obsolete playground failure".to_owned(),
                latency: None,
            })
            .unwrap();
        app.poll_events();

        assert_eq!(app.transcript, "preserve me");
        assert_eq!(app.status, TranscriptionStatus::Listening);
        assert_eq!(app.status_message, "Normal dictation is listening");
    }

    #[test]
    fn stale_playground_result_cleans_only_its_own_audio() {
        let old_audio = test_prepared_audio();
        let old_released = Arc::downgrade(&old_audio);
        let current_audio = test_prepared_audio();
        let current_retained = Arc::downgrade(&current_audio);

        let mut app = test_app();
        app.playground_cards[0].transcript = "current result".to_owned();
        app.playground_cards[0].status = ModelRuntimeStatus::Running;
        seed_test_request(
            &mut app,
            RecordingSource::Playground,
            SessionId(2),
            RequestId(20),
            "whisper_cpp_base_en",
        );
        app.playground_pending = 1;
        app.playground_runs.insert(
            SessionId(1),
            PlaygroundRunState {
                pending_requests: HashMap::from([
                    (RequestId(10), "whisper_cpp_base_en".to_owned()),
                    (RequestId(11), "whisper_cpp_base_en".to_owned()),
                ]),
                _audio: old_audio,
            },
        );
        app.playground_runs.insert(
            SessionId(2),
            PlaygroundRunState {
                pending_requests: HashMap::from([(
                    RequestId(20),
                    "whisper_cpp_base_en".to_owned(),
                )]),
                _audio: current_audio,
            },
        );
        app.tx
            .send(AppEvent::TranscriptionDone {
                source: RecordingSource::Playground,
                session_id: SessionId(1),
                request_id: RequestId(10),
                result: Box::new(test_transcription_outcome(
                    SessionId(1),
                    RequestId(10),
                    "obsolete playground result",
                )),
                latency: None,
            })
            .unwrap();

        app.poll_events();

        assert!(old_released.upgrade().is_some());
        assert_eq!(app.playground_runs[&SessionId(1)].pending_requests.len(), 1);
        app.tx
            .send(AppEvent::TranscriptionFailed {
                source: RecordingSource::Playground,
                session_id: SessionId(1),
                request_id: RequestId(11),
                model_id: "whisper_cpp_base_en".to_owned(),
                message: "obsolete playground failure".to_owned(),
                latency: None,
            })
            .unwrap();
        app.poll_events();

        assert!(old_released.upgrade().is_none());
        assert!(current_retained.upgrade().is_some());
        assert!(!app.playground_runs.contains_key(&SessionId(1)));
        assert!(app.playground_runs.contains_key(&SessionId(2)));
        assert_eq!(app.playground_pending, 1);
        assert_eq!(app.playground_cards[0].transcript, "current result");
        assert_eq!(app.playground_cards[0].status, ModelRuntimeStatus::Running);
    }

    #[test]
    fn superseding_playground_resets_cards_before_new_capture_can_fail() {
        let audio = test_prepared_audio();
        let released = Arc::downgrade(&audio);
        let mut app = test_app();
        app.playground_cards[0].status = ModelRuntimeStatus::Running;
        seed_test_request(
            &mut app,
            RecordingSource::Playground,
            SessionId(7),
            RequestId(70),
            "whisper_cpp_base_en",
        );
        app.playground_runs.insert(
            SessionId(7),
            PlaygroundRunState {
                pending_requests: HashMap::from([(
                    RequestId(70),
                    "whisper_cpp_base_en".to_owned(),
                )]),
                _audio: audio,
            },
        );
        app.playground_pending = 1;

        app.supersede_active_session();

        assert_eq!(app.session_coordinator.active_session_id(), None);
        assert_eq!(app.playground_pending, 0);
        assert!(released.upgrade().is_none());
        assert_ne!(app.playground_cards[0].status, ModelRuntimeStatus::Running);
    }

    #[test]
    fn active_session_does_not_hide_an_actionable_error_badge() {
        let mut app = test_app();
        seed_test_request(
            &mut app,
            RecordingSource::Transcribe,
            SessionId(8),
            RequestId(80),
            "whisper_cpp_base_en",
        );
        app.status = TranscriptionStatus::Error;
        app.status_message = "Failed to save settings".to_owned();

        assert_eq!(app.effective_status(), TranscriptionStatus::Error);
    }

    #[test]
    fn tray_state_tracks_visible_recording_and_transcript_changes() {
        let idle = tray_ui_state(false, "  \n ");
        assert_eq!(
            idle,
            TrayUiState {
                is_recording: false,
                has_transcript: false
            }
        );

        let recording = tray_ui_state(true, "  \n ");
        assert_eq!(
            recording,
            TrayUiState {
                is_recording: true,
                has_transcript: false
            }
        );

        let with_transcript = tray_ui_state(false, "hello");
        assert_eq!(
            with_transcript,
            TrayUiState {
                is_recording: false,
                has_transcript: true
            }
        );
    }

    #[test]
    fn tray_state_syncs_only_when_cached_state_changes() {
        let idle = tray_ui_state(false, "");
        let recording = tray_ui_state(true, "");
        let with_transcript = tray_ui_state(false, "hello");

        assert!(tray_state_needs_sync(None, idle));
        assert!(!tray_state_needs_sync(Some(idle), idle));
        assert!(tray_state_needs_sync(Some(idle), recording));
        assert!(tray_state_needs_sync(Some(idle), with_transcript));
        assert!(!tray_state_needs_sync(
            Some(with_transcript),
            tray_ui_state(false, "  hello  ")
        ));
    }

    #[test]
    fn page_uses_wide_available_body_width() {
        let body_width = render_page_body_width(2048.0);

        assert!(
            body_width > 1600.0,
            "page body width still looks artificially capped: {body_width}"
        );
    }

    #[test]
    fn viewport_background_paints_full_screen_width() {
        let output = render_viewport_background(2048.0);
        let max_painted_x = max_visible_painted_x(&output);

        assert!(
            max_painted_x >= 2047.0,
            "viewport background did not cover full width: {max_painted_x}"
        );
    }

    #[test]
    fn playground_result_editor_keeps_fixed_layout_height() {
        let allocated_height = render_playground_result_editor_height(960.0);

        assert!(
            (allocated_height - PLAYGROUND_RESULT_HEIGHT).abs() <= 1.0,
            "result editor allocated {allocated_height}, expected {PLAYGROUND_RESULT_HEIGHT}"
        );
    }

    fn render_page_body_width(width: f32) -> f32 {
        let ctx = egui::Context::default();
        configure_stitch_style(&ctx);
        ctx.set_visuals(stitch_visuals(ThemeMode::Light));

        let raw_input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(width, 760.0),
            )),
            ..Default::default()
        };
        let mut observed_width = 0.0;

        let _ = ctx.run(raw_input, |ctx| {
            let mut current_tab = Tab::Debug;
            show_test_navigation(ctx, &mut current_tab);
            egui::CentralPanel::default()
                .frame(content_panel_frame(ctx))
                .show(ctx, |ui| {
                    show_route_scroll(ui, UiRoute::Debug, |ui| {
                        page(
                            ui,
                            "Model Playground",
                            TranscriptionStatus::Idle,
                            "Ready",
                            |ui| {
                                observed_width = usable_width(ui);
                            },
                        );
                    });
                });
        });

        observed_width
    }

    fn render_viewport_background(width: f32) -> egui::FullOutput {
        let ctx = egui::Context::default();
        configure_stitch_style(&ctx);
        ctx.set_visuals(stitch_visuals(ThemeMode::Light));

        let raw_input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(width, 760.0),
            )),
            ..Default::default()
        };

        ctx.run(raw_input, |ctx| {
            paint_viewport_background(ctx);
        })
    }

    fn render_playground_result_editor_height(width: f32) -> f32 {
        let ctx = egui::Context::default();
        configure_stitch_style(&ctx);
        ctx.set_visuals(stitch_visuals(ThemeMode::Light));

        let raw_input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(width, 760.0),
            )),
            ..Default::default()
        };
        let transcript = "wrapped result line\n".repeat(48);
        let mut allocated_height = 0.0;

        let _ = ctx.run(raw_input, |ctx| {
            egui::CentralPanel::default()
                .frame(content_panel_frame(ctx))
                .show(ctx, |ui| {
                    allocated_height = playground_result_editor(ui, "test-result", &transcript)
                        .rect
                        .height();
                });
        });

        allocated_height
    }

    fn render_models_page(width: f32) -> egui::FullOutput {
        let ctx = egui::Context::default();
        configure_stitch_style(&ctx);
        ctx.set_visuals(stitch_visuals(ThemeMode::Light));

        let raw_input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(width, 760.0),
            )),
            ..Default::default()
        };
        let mut app = test_app();

        ctx.run(raw_input, |ctx| {
            show_test_navigation(ctx, &mut app.current_tab);
            egui::CentralPanel::default()
                .frame(content_panel_frame(ctx))
                .show(ctx, |ui| {
                    show_route_scroll(ui, UiRoute::Models, |ui| app.ui_models(ui))
                });
        })
    }

    fn render_app_tab(tab: Tab, width: f32) -> egui::FullOutput {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        configure_stitch_style(&ctx);
        ctx.set_visuals(stitch_visuals(ThemeMode::Light));

        let raw_input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(width, 760.0),
            )),
            ..Default::default()
        };
        let mut app = test_app();
        app.current_tab = tab;
        if tab == Tab::Advanced {
            app.settings_tab = SettingsTab::Advanced;
            app.current_tab = Tab::General;
        }

        ctx.run(raw_input, |ctx| {
            show_test_navigation(ctx, &mut app.current_tab);
            egui::CentralPanel::default()
                .frame(content_panel_frame(ctx))
                .show(ctx, |ui| match app.current_tab {
                    Tab::Transcribe => {
                        show_route_scroll(ui, UiRoute::Transcribe, |ui| app.ui_transcribe(ui))
                    }
                    Tab::General => {
                        show_route_scroll(ui, UiRoute::Settings(app.settings_tab), |ui| {
                            app.ui_general_settings(ui)
                        })
                    }
                    Tab::Models => show_route_scroll(ui, UiRoute::Models, |ui| app.ui_models(ui)),
                    Tab::History => {
                        show_route_scroll(ui, UiRoute::History, |ui| app.ui_history(ui))
                    }
                    Tab::Advanced => unreachable!("advanced navigation is routed to Settings"),
                    Tab::About => show_route_scroll(ui, UiRoute::About, |ui| app.ui_about(ui)),
                    Tab::Debug => show_route_scroll(ui, UiRoute::Debug, |ui| app.ui_playground(ui)),
                });
        })
    }

    fn render_debug_route_with_input(
        ctx: &egui::Context,
        app: &mut LocalTranscriberApp,
        width: f32,
        height: f32,
        events: Vec<egui::Event>,
        time: Option<f64>,
    ) -> egui::FullOutput {
        ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(width, height),
                )),
                events,
                time,
                focused: true,
                ..Default::default()
            },
            |ctx| {
                show_test_navigation(ctx, &mut app.current_tab);
                egui::CentralPanel::default()
                    .frame(content_panel_frame(ctx))
                    .show(ctx, |ui| {
                        show_route_scroll(ui, UiRoute::Debug, |ui| app.ui_playground(ui))
                    });
            },
        )
    }

    fn selector_raw_input(events: Vec<egui::Event>) -> egui::RawInput {
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(840.0, 760.0),
            )),
            events,
            ..Default::default()
        }
    }

    fn render_selector(
        ctx: &egui::Context,
        app: &mut LocalTranscriberApp,
        events: Vec<egui::Event>,
    ) -> egui::FullOutput {
        ctx.run(selector_raw_input(events), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                ui.label("Playground behind selector");
            });
            app.ui_playground_model_selector(ctx);
        })
    }

    fn render_playground(
        ctx: &egui::Context,
        app: &mut LocalTranscriberApp,
        events: Vec<egui::Event>,
    ) -> egui::FullOutput {
        ctx.run(selector_raw_input(events), |ctx| {
            egui::CentralPanel::default()
                .frame(content_panel_frame(ctx))
                .show(ctx, |ui| app.ui_playground(ui));
        })
    }

    fn accesskit_control_id(output: &egui::FullOutput, name: &str) -> egui::accesskit::NodeId {
        let update = output.platform_output.accesskit_update.as_ref().unwrap();
        update
            .nodes
            .iter()
            .find(|(_, node)| node.name() == Some(name))
            .map(|(id, _)| *id)
            .unwrap()
    }

    fn accesskit_control_id_with_prefix(
        output: &egui::FullOutput,
        name_prefix: &str,
    ) -> egui::accesskit::NodeId {
        let update = output.platform_output.accesskit_update.as_ref().unwrap();
        update
            .nodes
            .iter()
            .find(|(_, node)| {
                node.name()
                    .is_some_and(|name| name.starts_with(name_prefix))
            })
            .map(|(id, _)| *id)
            .unwrap()
    }

    fn render_app_tab_repeatedly(tab: Tab, width: f32, frames: usize) -> Vec<f32> {
        let ctx = egui::Context::default();
        configure_stitch_style(&ctx);
        ctx.set_visuals(stitch_visuals(ThemeMode::Light));
        let mut app = test_app();
        app.current_tab = tab;
        if tab == Tab::Advanced {
            app.settings_tab = SettingsTab::Advanced;
            app.current_tab = Tab::General;
        }
        let mut max_x_by_frame = Vec::new();

        for _ in 0..frames {
            let raw_input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(width, 760.0),
                )),
                ..Default::default()
            };
            let output = ctx.run(raw_input, |ctx| {
                show_test_navigation(ctx, &mut app.current_tab);
                egui::CentralPanel::default()
                    .frame(content_panel_frame(ctx))
                    .show(ctx, |ui| match app.current_tab {
                        Tab::Transcribe => {
                            show_route_scroll(ui, UiRoute::Transcribe, |ui| app.ui_transcribe(ui))
                        }
                        Tab::General => {
                            show_route_scroll(ui, UiRoute::Settings(app.settings_tab), |ui| {
                                app.ui_general_settings(ui)
                            })
                        }
                        Tab::Models => {
                            show_route_scroll(ui, UiRoute::Models, |ui| app.ui_models(ui))
                        }
                        Tab::History => {
                            show_route_scroll(ui, UiRoute::History, |ui| app.ui_history(ui))
                        }
                        Tab::Advanced => unreachable!("advanced navigation is routed to Settings"),
                        Tab::About => show_route_scroll(ui, UiRoute::About, |ui| app.ui_about(ui)),
                        Tab::Debug => {
                            show_route_scroll(ui, UiRoute::Debug, |ui| app.ui_playground(ui))
                        }
                    });
            });
            max_x_by_frame.push(max_painted_x(&output));
        }

        max_x_by_frame
    }

    fn show_test_navigation(ctx: &egui::Context, current_tab: &mut Tab) {
        show_navigation(ctx, current_tab, true);
    }

    fn max_visible_painted_x(output: &egui::FullOutput) -> f32 {
        output
            .shapes
            .iter()
            .map(|shape| {
                shape
                    .shape
                    .visual_bounding_rect()
                    .intersect(shape.clip_rect)
                    .max
                    .x
            })
            .fold(0.0_f32, f32::max)
    }

    fn max_painted_x(output: &egui::FullOutput) -> f32 {
        output
            .shapes
            .iter()
            .map(|shape| shape.shape.visual_bounding_rect().max.x)
            .fold(0.0_f32, f32::max)
    }

    fn test_prepared_audio() -> Arc<PreparedAudio> {
        Arc::new(PreparedAudio {
            samples: vec![0.0; 1_600],
            sample_rate: crate::prepared_audio::PREPARED_SAMPLE_RATE,
            source_sample_rate: crate::prepared_audio::PREPARED_SAMPLE_RATE,
            source_channels: 1,
            source_frames: 1_600,
        })
    }

    fn test_local_gguf_import_job(
        job_id: u64,
        cancellation: InstallCancellation,
    ) -> LocalGgufImportJob {
        let (completed, completion) = bounded(1);
        completed.send(()).unwrap();
        LocalGgufImportJob {
            job_id,
            cancellation,
            completion,
            worker: None,
        }
    }

    fn test_app() -> LocalTranscriberApp {
        let mut config = AppConfig::default();
        // Keep Playground event tests independent from whichever legacy model
        // happens to be installed on the developer machine. The default
        // catalog artifact is now GGUF, so a tiny local test file is enough
        // to exercise card/session state without invoking native loading.
        let fixture = std::env::temp_dir().join(format!(
            "scribe-app-default-tiny-{}-{}.gguf",
            std::process::id(),
            NEXT_TEST_SESSION.fetch_add(1, Ordering::Relaxed)
        ));
        fs::write(&fixture, b"test-only placeholder").unwrap();
        config
            .general
            .model_paths
            .insert("whisper_cpp_tiny_en".to_owned(), fixture.clone());
        config::normalize_config(&mut config);
        let (tx, rx) = unbounded();

        let transcription_service = TranscriptionService::new(config.clone());
        let playground_cards = cards_from_config(&config, &transcription_service);
        let mut app = LocalTranscriberApp {
            hotkey_input: config.recording.hotkey.clone(),
            model_search: String::new(),
            model_language_filter: ModelLanguageFilter::default(),
            remote_catalog_filters: RemoteCatalogFilters::default(),
            remote_catalog_sort: RemoteCatalogSort::default(),
            model_import_path: String::new(),
            remote_catalog: RemoteCatalogState::default(),
            audio_devices: Vec::new(),
            microphone_test: MicrophoneTest::Idle,
            microphone_test_sequence: 0,
            microphone_test_error: None,
            microphone_monitor_retry_required: false,
            microphone_level_envelope: MicrophoneLevelEnvelope::default(),
            deferred_recording_start: None,
            deferred_history_playback: None,
            capturing_hotkey: false,
            model_downloads: HashMap::new(),
            runtime_jobs: HashMap::new(),
            artifact_installations: HashMap::new(),
            local_gguf_import: None,
            artifact_recovery_error: None,
            playground_cards,
            playground_selector_draft: None,
            playground_selector_return_focus: None,
            playground_selector_header_focus: None,
            playground_selector_needs_initial_focus: false,
            playground_reference_transcript: String::new(),
            playground_reference_user_edited: false,
            playground_ranking_mode: RankingMode::Balanced,
            hotkey_service: HotkeyService::new(&config.recording.hotkey),
            config,
            config_path: None,
            settings_store: None,
            current_tab: Tab::Models,
            settings_tab: SettingsTab::General,
            models_show_comparison: false,
            model_comparison: ModelComparisonState::default(),
            comparison_run_model_ids: None,
            comparison_started_at: None,
            comparison_reference_revision: 0,
            comparison_output_revisions: HashMap::new(),
            comparison_projection_cache: HashMap::new(),
            comparison_wer_compute_count: 0,
            comparison_output_replacement_count: 0,
            model_management: ModelManagementState::default(),
            status: TranscriptionStatus::Idle,
            transcript: String::new(),
            raw_transcript: String::new(),
            status_message: "Ready".to_owned(),
            active_recording: None,
            pending_recording: None,
            pending_output: None,
            history_requests: HashMap::new(),
            leased_history_retry_ids: HashSet::new(),
            history_store: None,
            history_records: Vec::new(),
            history_next: None,
            history_search: String::new(),
            history_applied_search: String::new(),
            history_loading: false,
            history_query_sequence: 0,
            active_history_query: None,
            history_refresh_pending: false,
            history_error: None,
            history_delete_confirmation: None,
            history_confirmation_focus_pending: false,
            history_search_focus_pending: false,
            history_mutation_sequence: 0,
            history_mutation_in_flight: None,
            pending_history_retention_policy: None,
            armed_history_repaste: None,
            history_playback: None,
            playing_history_id: None,
            history_playback_stopping: false,
            rolling_preview: None,
            pending_preview_drain: None,
            transcription_service,
            tx,
            rx,
            playground_pending: 0,
            session_coordinator: SessionCoordinator::default(),
            playground_runs: HashMap::new(),
            latest_latency: None,
            diagnostics: DiagnosticsStore::default(),
            test_gguf_fixture: Some(fixture),
            captured_targets: HashMap::new(),
            overlay_controller: OverlayController::new(false),
            overlay_hide_at: None,
            tray_service: None,
            last_tray_state: None,
            window_hidden_to_tray: false,
            quit_requested: false,
        };
        app.rebuild_model_inventory_projection();
        app
    }

    fn install_test_catalog_model(app: &mut LocalTranscriberApp, model_id: &str) -> PathBuf {
        let fixture = std::env::temp_dir().join(format!(
            "scribe-app-{model_id}-{}-{}.gguf",
            std::process::id(),
            NEXT_TEST_SESSION.fetch_add(1, Ordering::Relaxed)
        ));
        fs::write(&fixture, b"test-only placeholder").unwrap();
        app.config
            .general
            .model_paths
            .insert(model_id.to_owned(), fixture.clone());
        config::normalize_config(&mut app.config);
        app.remote_catalog.invalidate_local_models();
        app.rebuild_model_inventory_projection();
        fixture
    }

    #[test]
    fn trusted_catalog_event_updates_only_backend_owned_catalog_state() {
        let mut app = test_app();
        app.remote_catalog.loading = true;
        let snapshot = ModelInventorySnapshot::from_trusted_records(
            2,
            CatalogSource::Network,
            vec![remote_catalog_model(
                "handy-computer/event-fixture",
                "Event fixture",
                &["en"],
                false,
                1,
            )],
        )
        .unwrap();
        app.remote_catalog.active_refresh_generation = Some(2);
        app.tx
            .send(AppEvent::RemoteCatalogLoaded {
                generation: 2,
                result: Ok(snapshot),
            })
            .unwrap();

        app.poll_events();

        assert!(!app.remote_catalog.loading);
        assert!(app.remote_catalog.error.is_none());
        assert_eq!(
            app.remote_catalog
                .snapshot
                .as_ref()
                .map(ModelInventorySnapshot::source),
            Some(crate::huggingface_catalog::CatalogSource::Network)
        );
    }

    #[test]
    fn stale_or_failed_refresh_preserves_snapshot_until_current_success() {
        let mut app = test_app();
        let initial = app.remote_catalog.snapshot.as_ref().unwrap().clone();
        let refreshed = |revision| {
            ModelInventorySnapshot::from_trusted_records(
                revision,
                CatalogSource::Network,
                vec![remote_catalog_model(
                    "handy-computer/refresh-fixture",
                    "Refresh fixture",
                    &["en"],
                    true,
                    revision,
                )],
            )
            .unwrap()
        };

        app.remote_catalog.loading = true;
        app.remote_catalog.active_refresh_generation = Some(3);
        app.tx
            .send(AppEvent::RemoteCatalogLoaded {
                generation: 2,
                result: Ok(refreshed(2)),
            })
            .unwrap();
        app.poll_events();
        assert!(app.remote_catalog.loading);
        assert!(initial.shares_records_with(app.remote_catalog.snapshot.as_ref().unwrap()));

        app.tx
            .send(AppEvent::RemoteCatalogLoaded {
                generation: 3,
                result: Err("offline".to_owned()),
            })
            .unwrap();
        app.poll_events();
        assert!(!app.remote_catalog.loading);
        assert!(initial.shares_records_with(app.remote_catalog.snapshot.as_ref().unwrap()));
        assert_eq!(app.remote_catalog.error.as_deref(), Some("offline"));

        app.remote_catalog.loading = true;
        app.remote_catalog.active_refresh_generation = Some(4);
        app.tx
            .send(AppEvent::RemoteCatalogLoaded {
                generation: 4,
                result: Ok(refreshed(4)),
            })
            .unwrap();
        app.poll_events();
        let current = app.remote_catalog.snapshot.as_ref().unwrap();
        assert_eq!(current.revision(), 4);
        assert!(!initial.shares_records_with(current));
        assert!(app.remote_catalog.error.is_none());
    }

    #[test]
    fn models_open_search_sort_and_unchanged_paints_use_cached_inventory() {
        let ctx = egui::Context::default();
        configure_stitch_style(&ctx);
        let mut app = test_app();
        let initial_local_builds = app.remote_catalog.local_models_build_count;
        let initial_local_models = Arc::clone(&app.remote_catalog.local_models);
        let initial_revision = app.remote_catalog.snapshot.as_ref().unwrap().revision();

        for _ in 0..3 {
            let _ = ctx.run(Default::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| app.ui_models(ui));
            });
        }
        app.apply_model_management_action(ScreenAction::SetRemoteCatalogQuery("tiny".into()));
        app.remote_catalog_filters.recommended_only = true;
        app.remote_catalog_sort = RemoteCatalogSort::Smallest;
        let _ = app.remote_catalog_view();
        let _ = app.remote_catalog_view();

        assert_eq!(app.remote_catalog.catalog_io_request_count, 0);
        assert_eq!(app.remote_catalog.disk_probe_count, 0);
        assert_eq!(
            app.remote_catalog.local_models_build_count,
            initial_local_builds
        );
        assert!(Arc::ptr_eq(
            &initial_local_models,
            &app.remote_catalog.local_models
        ));
        assert_eq!(
            app.remote_catalog.snapshot.as_ref().unwrap().revision(),
            initial_revision
        );
    }

    fn remote_catalog_model(
        id: &str,
        display_name: &str,
        languages: &[&str],
        recommended: bool,
        variant_size_bytes: u64,
    ) -> RemoteModel {
        RemoteModel {
            id: id.to_owned(),
            revision: "a".repeat(40),
            display_name: display_name.to_owned(),
            description: format!("{display_name} description"),
            languages: languages
                .iter()
                .map(|language| (*language).to_owned())
                .collect(),
            recommended,
            trust: crate::huggingface_catalog::ModelTrust::TrustedPublisher,
            compatibility: crate::huggingface_catalog::ModelCompatibility::Experimental(
                "Fixture only".to_owned(),
            ),
            variants: vec![crate::huggingface_catalog::RemoteModelVariant {
                id: "q4".to_owned(),
                filename: "fixture.gguf".to_owned(),
                size_bytes: variant_size_bytes,
                expected_sha256: "b".repeat(64),
            }],
        }
    }

    #[test]
    fn remote_catalog_browse_filters_and_sorts_only_from_available_metadata() {
        const MIB: u64 = 1024 * 1024;
        let models = vec![
            remote_catalog_model(
                "handy-computer/compact",
                "Compact",
                &["en"],
                false,
                320 * MIB,
            ),
            remote_catalog_model(
                "handy-computer/multilingual",
                "Multilingual",
                &["en", "es"],
                true,
                800 * MIB,
            ),
            remote_catalog_model(
                "handy-computer/large",
                "Large",
                &["multilingual"],
                true,
                2 * 1024 * MIB,
            ),
        ];
        let mut config = AppConfig::default();
        let installed = config::ManagedRemoteModelInstall {
            repository: "handy-computer/compact".to_owned(),
            revision: "a".repeat(40),
            filename: "fixture.gguf".to_owned(),
            ..Default::default()
        };
        config.general.managed_remote_models.insert(
            config::managed_remote_model_id(
                &installed.repository,
                &installed.revision,
                &installed.filename,
            )
            .unwrap(),
            installed,
        );

        let installed_only = filtered_remote_models(
            &models,
            &config,
            "",
            RemoteCatalogFilters {
                installed_only: true,
                ..Default::default()
            },
            RemoteCatalogSort::Name,
            ModelLanguageFilter::All,
        );
        assert_eq!(
            installed_only
                .iter()
                .map(|model| model.id.as_str())
                .collect::<Vec<_>>(),
            vec!["handy-computer/compact"]
        );

        let multilingual_standard = filtered_remote_models(
            &models,
            &config,
            "es",
            RemoteCatalogFilters {
                multilingual_only: true,
                size_tier: RemoteCatalogSizeTier::Standard,
                ..Default::default()
            },
            RemoteCatalogSort::Name,
            ModelLanguageFilter::All,
        );
        assert_eq!(
            multilingual_standard
                .iter()
                .map(|model| model.id.as_str())
                .collect::<Vec<_>>(),
            vec!["handy-computer/multilingual"]
        );

        let recommended_first = filtered_remote_models(
            &models,
            &config,
            "",
            RemoteCatalogFilters::default(),
            RemoteCatalogSort::Recommended,
            ModelLanguageFilter::All,
        );
        assert_eq!(
            recommended_first
                .iter()
                .map(|model| model.id.as_str())
                .collect::<Vec<_>>(),
            vec![
                "handy-computer/large",
                "handy-computer/multilingual",
                "handy-computer/compact"
            ]
        );

        let smallest_first = filtered_remote_models(
            &models,
            &config,
            "",
            RemoteCatalogFilters::default(),
            RemoteCatalogSort::Smallest,
            ModelLanguageFilter::All,
        );
        assert_eq!(
            smallest_first
                .iter()
                .map(|model| model.id.as_str())
                .collect::<Vec<_>>(),
            vec![
                "handy-computer/compact",
                "handy-computer/multilingual",
                "handy-computer/large"
            ]
        );
    }

    #[test]
    fn remote_catalog_projection_is_bounded_cached_and_truthful_at_ten_thousand_models() {
        let mut app = test_app();
        app.remote_catalog.snapshot = Some(
            ModelInventorySnapshot::from_records_unchecked_for_projection(
                2,
                CatalogSource::Network,
                (0..10_000)
                    .map(|index| {
                        remote_catalog_model(
                            &format!("handy-computer/catalog-{index:05}"),
                            &format!("Catalog model {index:05}"),
                            &["en"],
                            false,
                            64 * 1024 * 1024,
                        )
                    })
                    .collect(),
            ),
        );

        let first = app.remote_catalog_view();

        assert_eq!(first.entries.len(), REMOTE_CATALOG_VISIBLE_LIMIT);
        assert!(first.status.message.contains(
            "Showing 100 of 10000 matching models (10000 total). Refine search or filters"
        ));
        assert_eq!(app.remote_catalog.projection_build_count, 1);
        assert_eq!(app.remote_catalog.disk_probe_count, 0);

        let second = app.remote_catalog_view();

        assert_eq!(second.entries, first.entries);
        assert_eq!(app.remote_catalog.projection_build_count, 1);
        assert_eq!(app.remote_catalog.disk_probe_count, 0);

        let failed_model_id = config::managed_remote_model_id(
            "handy-computer/catalog-00000",
            &"a".repeat(40),
            "fixture.gguf",
        )
        .unwrap();
        app.fail_model_install(&failed_model_id, "synthetic install failure".to_owned());
        let failed = app.remote_catalog_view();

        assert_eq!(app.remote_catalog.projection_build_count, 2);
        assert_eq!(app.remote_catalog.disk_probe_count, 0);
        assert!(
            failed.entries[0].variants[0]
                .actions
                .iter()
                .any(|action| action.label == "Resume")
        );

        app.apply_model_management_action(ScreenAction::SetRemoteCatalogQuery(
            "catalog model 09999".to_owned(),
        ));
        let refined = app.remote_catalog_view();

        assert_eq!(refined.entries.len(), 1);
        assert!(
            refined
                .status
                .message
                .contains("Showing 1 of 10000 models.")
        );
        assert_eq!(app.remote_catalog.projection_build_count, 3);
        assert_eq!(app.remote_catalog.disk_probe_count, 0);

        app.apply_model_management_action(ScreenAction::SetRemoteCatalogQuery(
            "no catalog model has this phrase".to_owned(),
        ));
        let empty = app.remote_catalog_view();

        assert!(empty.entries.is_empty());
        assert!(empty.status.message.contains("Showing 0 of 10000 models."));
        assert_eq!(app.remote_catalog.projection_build_count, 4);
        assert_eq!(app.remote_catalog.disk_probe_count, 0);
    }

    #[test]
    fn shared_models_catalog_revalidates_import_and_install_lifecycle_tokens() {
        let mut app = test_app();
        app.remote_catalog.snapshot = Some(
            ModelInventorySnapshot::from_trusted_records(
                2,
                CatalogSource::Network,
                vec![remote_catalog_model(
                    "handy-computer/action-fixture",
                    "Action fixture",
                    &["en", "es"],
                    true,
                    320 * 1024 * 1024,
                )],
            )
            .unwrap(),
        );

        app.apply_model_management_action(ScreenAction::SetRemoteCatalogQuery("action".into()));
        app.remote_catalog_filters = RemoteCatalogFilters {
            installed_only: true,
            recommended_only: true,
            multilingual_only: true,
            size_tier: RemoteCatalogSizeTier::Compact,
        };
        app.remote_catalog_sort = RemoteCatalogSort::Largest;
        assert_eq!(app.model_search, "action");
        assert_eq!(
            app.remote_catalog_filters,
            RemoteCatalogFilters {
                installed_only: true,
                recommended_only: true,
                multilingual_only: true,
                size_tier: RemoteCatalogSizeTier::Compact,
            }
        );
        assert_eq!(app.remote_catalog_sort, RemoteCatalogSort::Largest);

        app.apply_model_management_action(ScreenAction::ValidateAndImportLocalGguf);
        assert_eq!(
            app.status_message,
            "Enter the path to a local .gguf file to import."
        );
        app.apply_model_management_action(ScreenAction::SetLocalGgufImportPath(
            "C:\\Models\\local.gguf".into(),
        ));
        assert_eq!(app.model_import_path, "C:\\Models\\local.gguf");
        let local_cancellation = InstallCancellation::default();
        let local_job_id = 41;
        app.local_gguf_import = Some(test_local_gguf_import_job(
            local_job_id,
            local_cancellation.clone(),
        ));
        let local_view = app.remote_catalog_view().local_import;
        assert_eq!(local_view.path, "C:\\Models\\local.gguf");
        assert!(local_view.in_progress);
        assert!(!local_view.import_enabled);
        app.finish_local_gguf_import(
            local_job_id + 1,
            Err("stale completion must be ignored".into()),
        );
        assert_eq!(
            app.local_gguf_import.as_ref().map(|job| job.job_id),
            Some(local_job_id)
        );
        app.apply_model_management_action(ScreenAction::CancelLocalGgufImport);
        assert!(local_cancellation.is_cancelled());
        app.finish_local_gguf_import(
            local_job_id,
            Err("completion delivered after cancellation".into()),
        );
        assert_eq!(app.status, TranscriptionStatus::Idle);
        assert_eq!(
            app.status_message,
            "Local GGUF import was cancelled. The source file was left unchanged."
        );
        assert!(app.local_gguf_import.is_none());

        app.apply_model_management_action(ScreenAction::RetryRemoteCatalog);
        assert!(app.remote_catalog.force_refresh_requested);

        app.remote_catalog_filters.installed_only = false;
        let view = app.remote_catalog_view();
        let install = &view.entries[0].variants[0].actions[0];
        assert!(matches!(
            &install.kind,
            RemoteCatalogActionKind::Install {
                remote_model_id,
                variant_id,
            } if remote_model_id == "handy-computer/action-fixture" && variant_id == "q4"
        ));

        let cancellation = InstallCancellation::default();
        app.artifact_installations
            .insert("managed-action-fixture".into(), (1, cancellation.clone()));
        app.apply_model_management_action(ScreenAction::CancelRemoteCatalogInstall(
            "managed-action-fixture".into(),
        ));
        assert!(cancellation.is_cancelled());

        app.apply_model_management_action(ScreenAction::InstallRemoteCatalogVariant {
            remote_model_id: "handy-computer/action-fixture".into(),
            variant_id: "stale-variant".into(),
        });
        assert_eq!(app.status, TranscriptionStatus::Error);
        assert!(
            app.status_message
                .contains("no longer in the validated snapshot")
        );
    }

    #[test]
    fn shutdown_cancels_artifact_and_local_gguf_imports() {
        let mut app = test_app();
        let artifact_cancellation = InstallCancellation::default();
        let local_cancellation = InstallCancellation::default();
        app.artifact_installations.insert(
            "managed-shutdown-fixture".to_owned(),
            (1, artifact_cancellation.clone()),
        );
        app.local_gguf_import = Some(test_local_gguf_import_job(42, local_cancellation.clone()));

        app.cancel_installations_for_shutdown();

        assert!(artifact_cancellation.is_cancelled());
        assert!(local_cancellation.is_cancelled());
        assert!(app.local_gguf_import.is_none());
    }

    #[test]
    fn local_gguf_shutdown_wait_is_bounded_before_worker_detach() {
        let cancellation = InstallCancellation::default();
        let (_completion_tx, completion) = bounded(1);
        let mut job = LocalGgufImportJob {
            job_id: 91,
            cancellation: cancellation.clone(),
            completion,
            worker: None,
        };
        let expected_timeout = Duration::from_millis(37);
        let observed_timeout = std::cell::Cell::new(None);

        let completed = job.cancel_and_wait_with(expected_timeout, |_, timeout| {
            observed_timeout.set(Some(timeout));
            false
        });

        assert!(!completed);
        assert!(cancellation.is_cancelled());
        assert_eq!(observed_timeout.get(), Some(expected_timeout));
    }

    #[test]
    fn unknown_settings_labels_are_no_ops() {
        let mut app = test_app();
        app.config.general.theme_mode = ThemeMode::Dark;
        app.config.overlay.mode = OverlayMode::Minimal;
        app.config.streaming.mode = StreamingMode::Rolling;
        app.config.performance.acceleration_preference = AccelerationPreference::Cpu;
        app.config.overlay.position = OverlayPosition::Top;
        app.config.history.mode = HistoryMode::TranscriptAndAudio;

        for action in [
            ScreenAction::SetTheme("unknown".into()),
            ScreenAction::SetOverlayMode("unknown".into()),
            ScreenAction::SetStreamingMode("unknown".into()),
            ScreenAction::SetAcceleration("unknown".into()),
            ScreenAction::SetOverlayPosition("unknown".into()),
            ScreenAction::SetHistoryMode("unknown".into()),
        ] {
            app.apply_settings_screen_action(action);
        }

        assert_eq!(app.config.general.theme_mode, ThemeMode::Dark);
        assert_eq!(app.config.overlay.mode, OverlayMode::Minimal);
        assert_eq!(app.config.streaming.mode, StreamingMode::Rolling);
        assert_eq!(
            app.config.performance.acceleration_preference,
            AccelerationPreference::Cpu
        );
        assert_eq!(app.config.overlay.position, OverlayPosition::Top);
        assert_eq!(app.config.history.mode, HistoryMode::TranscriptAndAudio);
    }

    #[test]
    fn shared_settings_actions_cancel_capture_and_exit_disabled_debug() {
        let mut app = test_app();
        app.apply_settings_screen_action(ScreenAction::ChangeShortcut);
        assert!(app.capturing_hotkey);
        app.apply_settings_screen_action(ScreenAction::ChangeShortcut);
        assert!(!app.capturing_hotkey);
        assert_eq!(app.status_message, "Hotkey capture cancelled.");

        app.config.developer.debug_mode = true;
        app.current_tab = Tab::Debug;
        app.apply_settings_screen_action(ScreenAction::SetDebugMode(false));
        assert!(!app.config.developer.debug_mode);
        assert_eq!(app.current_tab, Tab::General);
        assert_eq!(app.settings_tab, SettingsTab::Advanced);
    }

    #[test]
    fn preview_service_snapshot_uses_current_acceleration_settings() {
        let mut app = test_app();
        app.config.performance.acceleration_preference = AccelerationPreference::Cpu;

        assert_eq!(
            app.current_transcription_service()
                .configured_acceleration_preference(),
            AccelerationPreference::Cpu
        );

        app.config.performance.acceleration_preference = AccelerationPreference::Gpu;
        assert_eq!(
            app.current_transcription_service()
                .configured_acceleration_preference(),
            AccelerationPreference::Gpu
        );
    }

    fn seed_test_request(
        app: &mut LocalTranscriberApp,
        source: RecordingSource,
        session_id: SessionId,
        request_id: RequestId,
        model_id: &str,
    ) {
        app.session_coordinator.seed_active_for_test(
            session_id,
            source.purpose(),
            [(request_id, ModelId::new(model_id))],
        );
    }

    fn seed_test_session(
        app: &mut LocalTranscriberApp,
        source: RecordingSource,
        session_id: SessionId,
    ) {
        app.session_coordinator.seed_active_for_test(
            session_id,
            source.purpose(),
            std::iter::empty(),
        );
    }

    fn test_transcription_outcome(
        session_id: SessionId,
        request_id: RequestId,
        text: &str,
    ) -> TranscriptionOutcome {
        test_transcription_outcome_for_model(session_id, request_id, "whisper_cpp_base_en", text)
    }

    fn test_transcription_outcome_for_model(
        session_id: SessionId,
        request_id: RequestId,
        model_id: &str,
        text: &str,
    ) -> TranscriptionOutcome {
        TranscriptionOutcome {
            session_id,
            request_id,
            model_id: model_id.into(),
            model_name: "whisper.cpp base.en".to_owned(),
            backend_label: "whisper.cpp".to_owned(),
            transcript: crate::transcription::Transcript {
                text: text.to_owned(),
                segments: Vec::new(),
                detected_language: None,
                duration_ms: None,
            },
            stdout: String::new(),
            stderr: String::new(),
            processing_duration_ms: Some(42),
            resolved_acceleration: None,
            model_load_duration_ms: None,
            warm_model_reused: false,
        }
    }

    fn test_model() -> SttModelInfo {
        SttModelInfo {
            id: "whisper_cpp_base_en".to_owned(),
            name: "whisper.cpp base.en".to_owned(),
            backend: "whisper.cpp".to_owned(),
            description:
                "Recommended first-run local English model with a better speed/quality balance."
                    .to_owned(),
            expected_ram: "1 GB".to_owned(),
            accuracy_tier: "Good accuracy".to_owned(),
            speed_tier: "Fast speed".to_owned(),
            local_path: Some(PathBuf::from(
                "/home/tyhuang/Projects/whisper.cpp/models/ggml-base.en.bin",
            )),
            install_status: ModelInstallStatus::Installed,
            download_model: Some("base.en".to_owned()),
        }
    }

    fn write_vosk_runtime(root: &Path) -> PathBuf {
        write_vosk_runtime_with_revision(root, 3)
    }

    fn write_vosk_runtime_with_revision(root: &Path, runner_revision: u32) -> PathBuf {
        let executable = root.join("bin").join(runtime_wrapper_name("scribe-vosk"));
        let runner = root.join("bin").join("vosk_runner.py");
        let manifest = root.join("runtime-manifest.json");
        let python = if cfg!(windows) {
            root.join("venv").join("Scripts").join("python.exe")
        } else {
            root.join("venv").join("bin").join("python")
        };
        fs::create_dir_all(executable.parent().unwrap()).unwrap();
        fs::create_dir_all(python.parent().unwrap()).unwrap();
        fs::write(&executable, b"vosk runtime").unwrap();
        fs::write(runner, b"runner").unwrap();
        fs::write(
            manifest,
            format!(r#"{{"runner_revision":{runner_revision}}}"#),
        )
        .unwrap();
        fs::write(python, b"python").unwrap();
        executable
    }

    fn write_sherpa_family_runtime(root: &Path, runtime_id: &str, wrapper: &str) -> PathBuf {
        let executable = root.join("bin").join(runtime_wrapper_name(wrapper));
        let runner = root.join("bin").join("sherpa_onnx_runner.py");
        let manifest = root.join("runtime-manifest.json");
        let python = if cfg!(windows) {
            root.join("venv").join("Scripts").join("python.exe")
        } else {
            root.join("venv").join("bin").join("python")
        };
        fs::create_dir_all(executable.parent().unwrap()).unwrap();
        fs::create_dir_all(python.parent().unwrap()).unwrap();
        fs::write(&executable, b"sherpa runtime").unwrap();
        fs::write(runner, b"runner").unwrap();
        fs::write(
            manifest,
            format!(
                r#"{{"runtime_id":"{runtime_id}","runner_revision":2,"versions":{{"numpy":"2.3.2"}}}}"#
            ),
        )
        .unwrap();
        fs::write(python, b"python").unwrap();
        executable
    }

    fn runtime_wrapper_name(wrapper: &str) -> String {
        if cfg!(windows) {
            format!("{wrapper}.bat")
        } else {
            wrapper.to_owned()
        }
    }

    fn expected_runtime_install_action(_backend: &str) -> RuntimeActionState {
        if cfg!(unix) {
            RuntimeActionState {
                kind: RuntimeActionKind::Install,
                enabled: true,
                disabled_tooltip: None,
            }
        } else {
            RuntimeActionState {
                kind: RuntimeActionKind::Install,
                enabled: false,
                disabled_tooltip: Some(missing_runtime_source_message()),
            }
        }
    }

    fn managed_runtime_with_version(
        path: PathBuf,
        version: Option<&str>,
    ) -> config::ManagedRuntimeInstall {
        let mut install = config::ManagedRuntimeInstall::new(path);
        install.version = version.map(str::to_owned);
        install
    }

    #[test]
    fn installation_failures_preserve_recovery_required_classification() {
        let normal = InstallJobFailure::from(InstallError::Failed("retryable".to_owned()));
        assert!(!normal.recovery_required);
        let recovery = InstallJobFailure::from(InstallError::RecoveryRequired(
            "filesystem state is ambiguous".to_owned(),
        ));
        assert!(recovery.recovery_required);
        assert!(recovery.message.contains("recovery required"));
    }

    #[test]
    fn runtime_jobs_dedupe_queued_models_and_failure_message_explains_packaging() {
        let mut job = RuntimeInstallJob::default();
        assert!(queue_runtime_model(
            &mut job.download_model_ids,
            "model-a".to_owned()
        ));
        assert!(!queue_runtime_model(
            &mut job.download_model_ids,
            "model-a".to_owned()
        ));
        assert!(queue_runtime_model(
            &mut job.download_model_ids,
            "model-b".to_owned()
        ));
        assert_eq!(job.download_model_ids, ["model-a", "model-b"]);
        assert!(queue_runtime_model(
            &mut job.repair_model_ids,
            "installed-model".to_owned()
        ));
        assert_eq!(job.repair_model_ids, ["installed-model"]);
        assert!(missing_runtime_source_message().contains("packaged or staged build"));
    }

    #[test]
    fn runtime_package_root_requires_an_explicit_bin_layout() {
        let root = std::env::temp_dir().join(format!(
            "scribe-runtime-package-root-layout-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        let packaged = root
            .join("package")
            .join("bin")
            .join(runtime_wrapper_name("scribe-vosk"));
        let direct_directory = root.join("scribe-vosk-directory");
        fs::create_dir_all(packaged.parent().unwrap()).unwrap();
        fs::create_dir_all(&direct_directory).unwrap();
        fs::write(&packaged, b"runtime").unwrap();

        assert_eq!(runtime_package_root(&packaged), Some(root.join("package")));
        assert_eq!(runtime_package_root(&direct_directory), None);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn runtime_install_rejects_a_direct_sibling_executable_without_copying_its_parent() {
        let root = std::env::temp_dir().join(format!(
            "scribe-runtime-direct-sibling-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let executable = root.join(runtime_wrapper_name("scribe-vosk"));
        let unrelated = root.join("unrelated.marker");
        let target = root.join("managed");
        fs::write(&executable, b"standalone executable").unwrap();
        fs::write(&unrelated, b"unrelated").unwrap();

        let err = install_runtime_files_to("vosk", &executable, &target).unwrap_err();

        assert!(err.contains("determine runtime package root"));
        assert_eq!(fs::read(unrelated).unwrap(), b"unrelated");
        assert!(!target.exists());
        assert!(!fs::read_dir(&root).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".managed.installing-")
        }));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn runtime_install_rejects_target_and_stage_nested_in_source_package() {
        let root = std::env::temp_dir().join(format!(
            "scribe-runtime-nested-target-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        let source_root = root.join("package");
        let executable = write_vosk_runtime(&source_root);
        let package_marker = source_root.join("package.marker");
        let target = source_root.join("managed");
        let previous_marker = target.join("previous.marker");
        fs::create_dir_all(&target).unwrap();
        fs::write(&package_marker, b"package").unwrap();
        fs::write(&previous_marker, b"previous").unwrap();

        let err = install_runtime_files_to("vosk", &executable, &target).unwrap_err();

        assert!(err.contains("cannot overlap the managed runtime"));
        assert!(executable.is_file());
        assert_eq!(fs::read(&package_marker).unwrap(), b"package");
        assert_eq!(fs::read(&previous_marker).unwrap(), b"previous");
        assert!(!fs::read_dir(&source_root).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".managed.installing-")
        }));

        let missing_stage = source_root.join(".outside-managed.installing-test");
        let stage_err = validate_runtime_copy_paths(
            &source_root,
            &root.join("outside-managed"),
            &missing_stage,
        )
        .unwrap_err();
        assert!(stage_err.contains("staging target"));
        assert!(!missing_stage.exists());

        let missing = source_root.join("missing");
        fs::create_dir_all(&missing).unwrap();
        let aliased_target = missing.join("..").join("aliased-managed");
        let alias_err = install_runtime_files_to("vosk", &executable, &aliased_target).unwrap_err();
        assert!(alias_err.contains("cannot overlap the managed runtime"));
        assert_eq!(fs::read(&package_marker).unwrap(), b"package");
        assert_eq!(fs::read(&previous_marker).unwrap(), b"previous");
        assert!(!source_root.join("aliased-managed").exists());
        assert!(!fs::read_dir(&source_root).unwrap().any(|entry| {
            let name = entry.unwrap().file_name();
            let name = name.to_string_lossy();
            name.starts_with(".aliased-managed.installing-")
                || name.starts_with(".aliased-managed.backup-")
        }));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn runtime_install_rejects_target_ancestor_before_mutating_its_tree() {
        let target = std::env::temp_dir().join(format!(
            "scribe-runtime-target-ancestor-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&target);
        let source_root = target.join("package");
        let executable = write_vosk_runtime(&source_root);
        let package_marker = source_root.join("package.marker");
        let sibling_marker = target.join("sibling.marker");
        fs::write(&package_marker, b"package").unwrap();
        fs::write(&sibling_marker, b"sibling").unwrap();

        let err = match install_runtime_files_to("vosk", &executable, &target) {
            Err(err) => err,
            Ok(replacement) => {
                replacement.rollback().unwrap();
                let _ = fs::remove_dir_all(&target);
                panic!("ancestor target was activated before overlap rejection");
            }
        };

        assert!(err.contains("cannot overlap the managed runtime"));
        assert!(target.is_dir());
        assert!(executable.is_file());
        assert_eq!(fs::read(package_marker).unwrap(), b"package");
        assert_eq!(fs::read(sibling_marker).unwrap(), b"sibling");
        let transaction_prefix = format!(".{}.", target.file_name().unwrap().to_string_lossy());
        assert!(
            !fs::read_dir(target.parent().unwrap())
                .unwrap()
                .any(|entry| {
                    let name = entry.unwrap().file_name();
                    let name = name.to_string_lossy();
                    name.starts_with(&transaction_prefix)
                        && (name.contains(".installing-") || name.contains(".backup-"))
                })
        );
        let _ = fs::remove_dir_all(target);
    }

    #[test]
    fn runtime_install_copies_an_explicit_bin_package_layout() {
        let root =
            std::env::temp_dir().join(format!("scribe-runtime-bin-package-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let source_root = root.join("package");
        let executable = write_vosk_runtime(&source_root);
        fs::write(source_root.join("package.marker"), b"package").unwrap();
        let target = root.join("managed");

        let replacement = install_runtime_files_to("vosk", &executable, &target).unwrap();

        assert_eq!(
            replacement.installed_path,
            target.join("bin").join(runtime_wrapper_name("scribe-vosk"))
        );
        assert_eq!(fs::read(target.join("package.marker")).unwrap(), b"package");
        replacement.commit().unwrap();
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn runtime_install_rejects_package_root_and_recursive_entry_symlinks() {
        use std::os::unix::fs::symlink;

        let root = std::env::temp_dir().join(format!(
            "scribe-runtime-package-symlink-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        let source_root = root.join("package");
        let executable = write_vosk_runtime(&source_root);
        let linked_root = root.join("linked-package");
        symlink(&source_root, &linked_root).unwrap();
        let linked_executable = linked_root
            .join("bin")
            .join(runtime_wrapper_name("scribe-vosk"));

        let root_err =
            install_runtime_files_to("vosk", &linked_executable, &root.join("managed-from-link"))
                .unwrap_err();
        assert!(root_err.contains("symbolic link or reparse point"));

        symlink(&executable, source_root.join("linked-entry")).unwrap();
        let entry_err =
            install_runtime_files_to("vosk", &executable, &root.join("managed")).unwrap_err();
        assert!(entry_err.contains("symbolic link or reparse point"));
        assert!(!root.join("managed").exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn managed_runtime_is_never_selected_as_packaged_source_across_backends() {
        let root = std::env::temp_dir().join(format!(
            "scribe-runtime-source-selection-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        for (backend, runtime_id, executable) in [
            ("whisper.cpp", "whisper_cpp", "bin/whisper-cli"),
            (
                "faster-whisper",
                "faster_whisper",
                "bin/scribe-faster-whisper",
            ),
            ("Vosk", "vosk", "bin/scribe-vosk"),
            ("sherpa-onnx", "sherpa_onnx", "bin/scribe-sherpa-onnx"),
        ] {
            let current = root
                .join("managed-runtimes")
                .join(runtime_id)
                .join(executable);
            let staged = root
                .join("staged-runtimes")
                .join(runtime_id)
                .join(executable);
            fs::create_dir_all(current.parent().unwrap()).unwrap();
            fs::create_dir_all(staged.parent().unwrap()).unwrap();
            fs::write(&current, b"current").unwrap();
            fs::write(&staged, b"staged").unwrap();
            let mut config = AppConfig::default();
            config.general.managed_runtimes.insert(
                runtime_id.to_owned(),
                config::ManagedRuntimeInstall::new(current.clone()),
            );
            let mut model = test_model();
            model.backend = backend.to_owned();

            assert_eq!(
                runtime_install_source_from_candidates(&config, &model, Some(current), None),
                None,
                "{backend} must not update from its managed install"
            );
            assert!(matches!(
                runtime_install_source_from_candidates(&config, &model, Some(staged), None),
                Some(RuntimeInstallSource::Packaged(_))
            ));
        }
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn stale_runtime_without_newer_source_does_not_offer_update() {
        let runtime_root = std::env::temp_dir().join(format!(
            "scribe-runtime-no-update-source-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&runtime_root);
        let executable = write_vosk_runtime(&runtime_root.join("vosk"));
        let mut config = AppConfig::default();
        config.general.managed_runtimes.insert(
            "vosk".to_owned(),
            managed_runtime_with_version(executable, Some("0.3.44")),
        );
        let mut model = test_model();
        model.backend = "Vosk".to_owned();
        let provider = compatibility_bridge::provider_for_model(&model).unwrap();

        let action = runtime_action_state_for_source(&config, &model, provider, false);

        assert_eq!(action.kind, RuntimeActionKind::Uninstall);
        assert!(action.enabled);
        let _ = fs::remove_dir_all(runtime_root);
    }

    #[test]
    fn failed_staged_validation_preserves_previous_runtime() {
        let root = std::env::temp_dir().join(format!(
            "scribe-runtime-transaction-failure-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        let target_root = root.join("managed-vosk");
        let previous_executable = write_vosk_runtime(&target_root);
        fs::write(target_root.join("previous.marker"), b"previous").unwrap();
        let invalid_source = root.join("invalid-source");
        let invalid_executable = invalid_source
            .join("bin")
            .join(runtime_wrapper_name("scribe-vosk"));
        fs::create_dir_all(invalid_executable.parent().unwrap()).unwrap();
        fs::write(&invalid_executable, b"invalid").unwrap();

        let result = install_runtime_files_to("vosk", &invalid_executable, &target_root);

        assert!(result.is_err());
        assert!(target_root.join("previous.marker").is_file());
        assert!(crate::stt::vosk::is_vosk_runtime_usable(
            &previous_executable
        ));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn runtime_record_and_files_roll_back_together_before_continuation() {
        let root = std::env::temp_dir().join(format!(
            "scribe-runtime-transaction-rollback-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        let target_root = root.join("managed-vosk");
        let previous_executable = write_vosk_runtime(&target_root);
        fs::write(target_root.join("previous.marker"), b"previous").unwrap();
        let source_root = root.join("staged-vosk");
        let source_executable = write_vosk_runtime(&source_root);
        fs::write(source_root.join("new.marker"), b"new").unwrap();

        let replacement =
            install_runtime_files_to("vosk", &source_executable, &target_root).unwrap();
        let mut config = AppConfig::default();
        let mut previous_record = config::ManagedRuntimeInstall::new(previous_executable.clone());
        previous_record.source = Some("previous".to_owned());
        config
            .general
            .managed_runtimes
            .insert("vosk".to_owned(), previous_record.clone());
        let mut new_record = config::ManagedRuntimeInstall::new(replacement.installed_path.clone());
        new_record.source = Some("replacement".to_owned());
        assert!(!runtime_metadata_matches(&config, "vosk", &new_record));
        let replaced = apply_runtime_record(&mut config, "vosk", new_record.clone());
        assert!(runtime_metadata_matches(&config, "vosk", &new_record));

        rollback_runtime_record(&mut config, "vosk", replaced);
        replacement.rollback().unwrap();

        assert_eq!(
            config.general.managed_runtimes.get("vosk"),
            Some(&previous_record)
        );
        assert!(target_root.join("previous.marker").is_file());
        assert!(!target_root.join("new.marker").exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn runtime_persistence_gates_download_continuation() {
        let mut config = AppConfig::default();
        let previous = config::ManagedRuntimeInstall::new(PathBuf::from("previous-runtime"));
        config
            .general
            .managed_runtimes
            .insert("vosk".to_owned(), previous.clone());
        let replacement = config::ManagedRuntimeInstall::new(PathBuf::from("replacement-runtime"));
        let job = RuntimeInstallJob {
            download_model_ids: vec!["queued-model".to_owned()],
            repair_model_ids: Vec::new(),
        };
        let persistence_attempted = std::cell::Cell::new(false);

        let failed = persist_runtime_install(
            &mut config,
            "vosk",
            replacement.clone(),
            job.clone(),
            |saved| {
                persistence_attempted.set(true);
                assert!(runtime_metadata_matches(saved, "vosk", &replacement));
                Err("disk full".to_owned())
            },
        );

        assert!(persistence_attempted.get());
        assert_eq!(config.general.managed_runtimes.get("vosk"), Some(&previous));
        assert!(matches!(
            failed,
            RuntimePersistenceTransition::Failed {
                job: RuntimeInstallJob {
                    download_model_ids,
                    ..
                },
                ..
            } if download_model_ids == ["queued-model"]
        ));

        persistence_attempted.set(false);
        let persisted =
            persist_runtime_install(&mut config, "vosk", replacement.clone(), job, |saved| {
                assert!(runtime_metadata_matches(saved, "vosk", &replacement));
                persistence_attempted.set(true);
                Ok(())
            });
        assert!(persistence_attempted.get());
        assert!(matches!(
            persisted,
            RuntimePersistenceTransition::Persisted(RuntimeInstallJob {
                download_model_ids,
                ..
            }) if download_model_ids == ["queued-model"]
        ));
    }

    #[test]
    fn runtime_uninstall_error_preserves_managed_metadata() {
        let mut config = AppConfig::default();
        let install = config::ManagedRuntimeInstall::new(PathBuf::from("managed-runtime"));
        config
            .general
            .managed_runtimes
            .insert("vosk".to_owned(), install.clone());

        assert!(
            apply_runtime_uninstall_result(
                &mut config,
                "vosk",
                Err("runtime is locked".to_owned()),
            )
            .is_err()
        );
        assert_eq!(config.general.managed_runtimes.get("vosk"), Some(&install));

        assert_eq!(
            apply_runtime_uninstall_result(&mut config, "vosk", Ok(false)),
            Ok(false)
        );
        assert!(!config.general.managed_runtimes.contains_key("vosk"));
    }

    #[test]
    fn model_install_activation_only_replaces_an_unrunnable_active_model() {
        assert!(!should_activate_installed_model(true));
        assert!(should_activate_installed_model(false));
    }

    #[test]
    fn playground_membership_adds_once_and_uninstall_cleanup_removes_model() {
        let mut config = AppConfig::default();
        set_model_selected(&mut config, "whisper_cpp_tiny_en", true);
        set_model_selected(&mut config, "whisper_cpp_tiny_en", true);
        assert_eq!(
            config.general.playground_selected_models,
            ["whisper_cpp_tiny_en"]
        );
        set_model_selected(&mut config, "whisper_cpp_tiny_en", false);
        assert!(config.general.playground_selected_models.is_empty());
    }

    #[test]
    fn playground_selector_draft_opens_cancels_and_stays_closed_while_busy() {
        let mut app = test_app();
        app.config.general.model_storage_dir = std::env::temp_dir().join(format!(
            "scribe-selector-state-missing-models-{}",
            std::process::id()
        ));
        app.config.general.managed_models.clear();
        app.config.general.model_paths.clear();
        app.config.general.playground_selected_models = vec!["whisper_cpp_tiny_en".to_owned()];

        app.open_playground_selector(None);
        assert_eq!(
            app.playground_selector_draft,
            Some(vec!["whisper_cpp_tiny_en".to_owned()])
        );
        app.playground_selector_draft.as_mut().unwrap().clear();
        app.close_playground_selector(&egui::Context::default());
        assert!(app.playground_selector_draft.is_none());
        assert_eq!(
            app.config.general.playground_selected_models,
            ["whisper_cpp_tiny_en"]
        );

        app.playground_pending = 1;
        app.open_playground_selector(None);
        assert!(app.playground_selector_draft.is_none());

        let before_apply = app.config.general.playground_selected_models.clone();
        app.playground_selector_draft = Some(vec!["whisper_cpp_base_en".to_owned()]);
        app.apply_playground_selector(&egui::Context::default());
        assert_eq!(app.config.general.playground_selected_models, before_apply);

        apply_playground_selector_draft(&mut app.config, vec!["whisper_cpp_base_en".to_owned()]);
        assert!(app.config.general.playground_selected_models.is_empty());
    }

    #[test]
    fn selector_escape_dismisses_rendered_window_without_persisting_draft() {
        let ctx = egui::Context::default();
        configure_stitch_style(&ctx);
        let mut app = test_app();
        app.config.general.playground_selected_models = vec!["whisper_cpp_tiny_en".to_owned()];
        let opener_id = egui::Id::new("selector-test-opener");
        app.open_playground_selector(Some(opener_id));
        app.playground_selector_draft.as_mut().unwrap().clear();

        render_selector(&ctx, &mut app, Vec::new());
        render_selector(
            &ctx,
            &mut app,
            vec![egui::Event::Key {
                key: egui::Key::Escape,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
        );

        assert!(app.playground_selector_draft.is_none());
        assert_eq!(
            app.config.general.playground_selected_models,
            ["whisper_cpp_tiny_en"]
        );
        assert_eq!(ctx.memory(|memory| memory.focused()), Some(opener_id));
    }

    #[test]
    fn selector_apply_button_prunes_uninstalled_draft_ids() {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        configure_stitch_style(&ctx);
        let mut app = test_app();
        app.config.general.model_storage_dir = std::env::temp_dir().join(format!(
            "scribe-missing-selector-models-{}",
            std::process::id()
        ));
        app.config.general.playground_selected_models = vec!["whisper_cpp_base_en".to_owned()];
        app.open_playground_selector(None);

        let output = render_selector(&ctx, &mut app, Vec::new());
        let apply_id = accesskit_control_id(&output, "Apply model selection");
        render_selector(
            &ctx,
            &mut app,
            vec![egui::Event::AccessKitActionRequest(
                egui::accesskit::ActionRequest {
                    action: egui::accesskit::Action::Default,
                    target: apply_id,
                    data: None,
                },
            )],
        );

        assert!(app.playground_selector_draft.is_none());
        assert!(app.config.general.playground_selected_models.is_empty());
    }

    #[test]
    fn selector_empty_state_explains_how_to_install_models() {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        configure_stitch_style(&ctx);
        let mut app = test_app();
        app.config.general.model_storage_dir = std::env::temp_dir().join(format!(
            "scribe-selector-empty-state-{}",
            std::process::id()
        ));
        app.config.general.managed_models.clear();
        app.config.general.model_paths.clear();
        app.config.general.playground_selected_models.clear();
        app.open_playground_selector(None);

        let output = render_selector(&ctx, &mut app, Vec::new());
        let update = output.platform_output.accesskit_update.as_ref().unwrap();
        assert!(update.nodes.iter().any(|(_, node)| {
            node.name() == Some(
                "No installed models yet. Install a model from Models, then return here to select it.",
            )
        }));
    }

    #[test]
    fn selector_window_honors_minimum_content_width() {
        let ctx = egui::Context::default();
        configure_stitch_style(&ctx);
        let mut app = test_app();
        app.config.general.model_storage_dir =
            std::env::temp_dir().join(format!("scribe-selector-min-width-{}", std::process::id()));
        app.config.general.managed_models.clear();
        app.config.general.model_paths.clear();
        app.config.general.playground_selected_models.clear();
        app.open_playground_selector(None);

        render_selector(&ctx, &mut app, Vec::new());

        let selector_rect = ctx
            .memory(|memory| memory.area_rect(egui::Id::new("Choose models to test")))
            .expect("selector window should have an area rect after rendering");
        assert!(
            selector_rect.width() >= 432.0,
            "selector width {} should honor the 432 px minimum",
            selector_rect.width()
        );
    }

    #[test]
    fn empty_state_apply_restores_focus_to_persistent_header_control() {
        let root = std::env::temp_dir().join(format!(
            "scribe-selector-focus-models-{}",
            std::process::id()
        ));
        let model_dir = root.join("whisper.cpp");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&model_dir).unwrap();
        let tiny_path = root.join("gguf").join("whisper-tiny.en-Q4_K_M.gguf");
        fs::create_dir_all(tiny_path.parent().unwrap()).unwrap();
        fs::write(&tiny_path, b"tiny").unwrap();

        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        configure_stitch_style(&ctx);
        let mut app = test_app();
        app.config.general.model_storage_dir = root.clone();
        app.config.general.managed_models.clear();
        app.config.general.model_paths.clear();
        app.config.general.playground_selected_models.clear();
        config::normalize_config(&mut app.config);
        app.refresh_playground_cards_from_config();

        let output = render_playground(&ctx, &mut app, Vec::new());
        let empty_opener = accesskit_control_id(&output, "Choose Models");
        let output = render_playground(
            &ctx,
            &mut app,
            vec![egui::Event::AccessKitActionRequest(
                egui::accesskit::ActionRequest {
                    action: egui::accesskit::Action::Default,
                    target: empty_opener,
                    data: None,
                },
            )],
        );
        let checkbox = accesskit_control_id_with_prefix(&output, "Whisper Tiny — English;");
        let apply = accesskit_control_id(&output, "Apply model selection");
        render_playground(
            &ctx,
            &mut app,
            vec![
                egui::Event::AccessKitActionRequest(egui::accesskit::ActionRequest {
                    action: egui::accesskit::Action::Default,
                    target: checkbox,
                    data: None,
                }),
                egui::Event::AccessKitActionRequest(egui::accesskit::ActionRequest {
                    action: egui::accesskit::Action::Default,
                    target: apply,
                    data: None,
                }),
            ],
        );
        let output = render_playground(&ctx, &mut app, Vec::new());

        let header_focus = app.playground_selector_header_focus.unwrap();
        assert_eq!(ctx.memory(|memory| memory.focused()), Some(header_focus));
        assert!(
            output
                .platform_output
                .accesskit_update
                .as_ref()
                .unwrap()
                .nodes
                .iter()
                .any(|(_, node)| node.name() == Some("Choose models to test"))
        );
        assert_eq!(
            app.config.general.playground_selected_models,
            ["whisper_cpp_tiny_en"]
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn keyboard_reorder_swaps_selected_neighbors_and_respects_bounds() {
        let mut order = vec!["a".to_owned(), "hidden".to_owned(), "b".to_owned()];
        let selected = vec!["a".to_owned(), "b".to_owned()];

        assert_eq!(
            move_selected_model_by(&mut order, &selected, "b", -1),
            Some(0)
        );
        assert_eq!(order, ["b", "hidden", "a"]);
        assert_eq!(move_selected_model_by(&mut order, &selected, "a", -1), None);
        assert_eq!(move_selected_model_by(&mut order, &selected, "b", 1), None);
    }

    #[test]
    fn refreshing_playground_cards_retains_selected_results_and_drops_removed_cards() {
        let root =
            std::env::temp_dir().join(format!("scribe-playground-cards-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let tiny_path = root.join("gguf").join("whisper-tiny.en-Q4_K_M.gguf");
        fs::create_dir_all(tiny_path.parent().unwrap()).unwrap();
        fs::write(&tiny_path, b"tiny").unwrap();

        let mut app = test_app();
        app.config.general.model_storage_dir = root.clone();
        app.config
            .general
            .model_paths
            .insert("whisper_cpp_tiny_en".to_owned(), tiny_path);
        app.config.general.playground_selected_models = vec!["whisper_cpp_tiny_en".to_owned()];
        app.config.general.playground_model_order = vec!["whisper_cpp_tiny_en".to_owned()];
        config::normalize_config(&mut app.config);
        app.refresh_playground_cards_from_config();
        app.playground_cards[0].transcript = "retained".to_owned();

        app.refresh_playground_cards_from_config();
        assert_eq!(app.playground_cards.len(), 1);
        assert_eq!(app.playground_cards[0].transcript, "retained");

        app.config.general.playground_selected_models.clear();
        app.refresh_playground_cards_from_config();
        assert!(app.playground_cards.is_empty());

        app.config
            .general
            .playground_selected_models
            .push("whisper_cpp_tiny_en".to_owned());
        app.refresh_playground_cards_from_config();
        assert_eq!(app.playground_cards.len(), 1);
        assert!(app.playground_cards[0].transcript.is_empty());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn playground_run_requires_a_selection_and_ready_cards() {
        let mut app = test_app();
        app.config.general.playground_selected_models.clear();
        app.playground_cards.clear();
        assert!(
            app.playground_run_block_reason()
                .is_some_and(|message| message.contains("Choose models"))
        );

        let base_path = std::env::temp_dir().join(format!(
            "scribe-playground-not-ready-{}-{}.bin",
            std::process::id(),
            NEXT_TEST_SESSION.fetch_add(1, Ordering::Relaxed)
        ));
        fs::write(&base_path, b"test-only installed model").unwrap();
        app.config
            .general
            .model_paths
            .insert("whisper_cpp_base_en".to_owned(), base_path.clone());
        app.config.general.playground_selected_models = vec!["whisper_cpp_base_en".to_owned()];
        app.config.general.playground_model_order = vec!["whisper_cpp_base_en".to_owned()];
        config::normalize_config(&mut app.config);
        app.transcription_service = app.transcription_service.with_config(app.config.clone());
        app.refresh_playground_cards_from_config();
        let selected = app.playground_selected_models();
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].install_status, ModelInstallStatus::Installed);
        assert_ne!(
            runtime_status_for_model(&app.config, &selected[0]),
            ModelRuntimeStatus::Ready
        );
        assert!(
            app.playground_run_block_reason()
                .is_some_and(|message| message.contains("not ready"))
        );
        let _ = fs::remove_file(base_path);
    }

    #[test]
    fn runtime_ready_bypasses_preparation_and_failures_fan_out_to_queued_models() {
        assert!(!runtime_needs_preparation(&ModelRuntimeStatus::Ready));
        assert!(runtime_needs_preparation(
            &ModelRuntimeStatus::MissingConfiguration
        ));

        let mut app = test_app();
        app.runtime_jobs.insert(
            "whisper_cpp".to_owned(),
            RuntimeInstallJob {
                download_model_ids: vec!["model-a".to_owned(), "model-b".to_owned()],
                repair_model_ids: vec!["installed-model".to_owned()],
            },
        );
        app.fail_runtime_job("whisper_cpp", "runtime copy failed".to_owned());
        assert_eq!(
            app.model_downloads.get("model-a"),
            Some(&ModelInstallStatus::Error("runtime copy failed".to_owned()))
        );
        assert_eq!(
            app.model_downloads.get("model-b"),
            Some(&ModelInstallStatus::Error("runtime copy failed".to_owned()))
        );
        assert_eq!(
            app.model_downloads.get("installed-model"),
            Some(&ModelInstallStatus::RuntimeError(
                "runtime copy failed".to_owned()
            ))
        );
        assert!(!app.runtime_jobs.contains_key("whisper_cpp"));
    }

    #[test]
    fn busy_runtime_disables_maintenance() {
        let model = test_model();
        let busy = runtime_action_state_with_busy(&AppConfig::default(), &model, true);
        assert!(!busy.enabled);
        assert!(
            busy.disabled_tooltip
                .as_deref()
                .is_some_and(|message| message.contains("already being prepared"))
        );
    }

    #[test]
    fn active_runtime_consumers_disable_update_and_remove_actions() {
        let update = RuntimeActionState {
            kind: RuntimeActionKind::Update,
            enabled: true,
            disabled_tooltip: None,
        };
        for (activity, expected) in [
            (
                RuntimeConsumerActivity {
                    recording: true,
                    ..Default::default()
                },
                "active recording",
            ),
            (
                RuntimeConsumerActivity {
                    transcribing: true,
                    ..Default::default()
                },
                "transcription",
            ),
            (
                RuntimeConsumerActivity {
                    playground_jobs: true,
                    ..Default::default()
                },
                "Playground jobs",
            ),
            (
                RuntimeConsumerActivity {
                    model_download: true,
                    ..Default::default()
                },
                "model download",
            ),
        ] {
            let blocked = restrict_runtime_action(update.clone(), false, activity);
            assert!(!blocked.enabled);
            assert!(
                blocked
                    .disabled_tooltip
                    .as_deref()
                    .is_some_and(|tooltip| tooltip.contains(expected))
            );
        }

        let remove = RuntimeActionState {
            kind: RuntimeActionKind::Uninstall,
            ..update.clone()
        };
        assert!(
            !restrict_runtime_action(
                remove,
                false,
                RuntimeConsumerActivity {
                    recording: true,
                    ..Default::default()
                },
            )
            .enabled
        );
        let install = RuntimeActionState {
            kind: RuntimeActionKind::Install,
            ..update
        };
        assert!(
            restrict_runtime_action(
                install,
                false,
                RuntimeConsumerActivity {
                    recording: true,
                    ..Default::default()
                },
            )
            .enabled
        );
    }

    #[test]
    fn model_download_activity_matches_the_shared_runtime() {
        let config = AppConfig::default();
        let model = config::configured_models(&config)
            .into_iter()
            .find(|model| crate::stt::provider_for_backend(&model.backend).is_some())
            .unwrap();
        let runtime_id = crate::stt::provider_for_backend(&model.backend)
            .unwrap()
            .runtime_id;
        let mut downloads = HashMap::new();
        downloads.insert(
            model.id,
            ModelInstallStatus::Downloading {
                downloaded_bytes: 1,
                total_bytes: None,
                bytes_per_second: None,
            },
        );

        assert!(model_download_uses_runtime(&config, &downloads, runtime_id));
        assert!(!model_download_uses_runtime(
            &config,
            &downloads,
            "different-runtime"
        ));
    }

    #[test]
    fn capture_numeric_controls_have_programmatic_accessible_names() {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        configure_stitch_style(&ctx);
        ctx.set_visuals(stitch_visuals(ThemeMode::Light));
        let mut app = test_app();
        app.current_tab = Tab::General;
        app.settings_tab = SettingsTab::Recording;
        app.config.recording.vad_enabled = true;
        let output = ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1_024.0, 1_600.0),
                )),
                ..Default::default()
            },
            |ctx| {
                show_test_navigation(ctx, &mut app.current_tab);
                egui::CentralPanel::default()
                    .frame(content_panel_frame(ctx))
                    .show(ctx, |ui| app.ui_general_settings(ui));
            },
        );
        let update = output.platform_output.accesskit_update.unwrap();
        let spin_buttons = update
            .nodes
            .iter()
            .filter(|(_, node)| node.role() == egui::accesskit::Role::SpinButton)
            .collect::<Vec<_>>();

        for expected in [
            "Speech confirmation ms",
            "Internal pause ms",
            "End after silence ms",
            "Pre-roll ms",
            "Post-roll ms",
        ] {
            let label_id = update
                .nodes
                .iter()
                .find(|(_, node)| node.name() == Some(expected))
                .map(|(id, _)| *id)
                .unwrap_or_else(|| panic!("missing AccessKit label {expected:?}"));
            assert!(
                spin_buttons
                    .iter()
                    .any(|(_, node)| node.labelled_by().contains(&label_id)),
                "no spin button is programmatically labelled by {expected:?}"
            );
        }
    }

    #[test]
    fn input_sensitivity_is_the_only_accessible_microphone_testing_control() {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        configure_stitch_style(&ctx);
        ctx.set_visuals(stitch_visuals(ThemeMode::Light));
        let mut app = test_app();
        app.current_tab = Tab::General;
        app.settings_tab = SettingsTab::Recording;
        app.playing_history_id = Some(1);

        let output = ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1_024.0, 1_600.0),
                )),
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default()
                    .frame(content_panel_frame(ctx))
                    .show(ctx, |ui| app.ui_general_settings(ui));
            },
        );
        let update = output.platform_output.accesskit_update.unwrap();
        assert!(update.nodes.iter().any(|(_, node)| {
            node.role() == egui::accesskit::Role::Heading && node.name() == Some("Audio input")
        }));

        let microphone_label_id = update
            .nodes
            .iter()
            .find(|(_, node)| node.name() == Some("Device"))
            .map(|(id, _)| *id)
            .expect("missing Device label");
        assert!(update.nodes.iter().any(|(_, node)| {
            node.role() == egui::accesskit::Role::ComboBox
                && node.labelled_by().contains(&microphone_label_id)
        }));

        let sensitivity_label_id = update
            .nodes
            .iter()
            .find(|(_, node)| {
                node.role() == egui::accesskit::Role::StaticText
                    && node.name() == Some("Input level")
            })
            .map(|(id, _)| *id)
            .expect("missing Input level label");
        let slider = update
            .nodes
            .iter()
            .map(|(_, node)| node)
            .find(|node| {
                node.role() == egui::accesskit::Role::Slider
                    && node.name() == Some("Input level sensitivity")
            })
            .expect("missing accessible input sensitivity slider");
        assert!(slider.labelled_by().contains(&sensitivity_label_id));
        assert_eq!(slider.min_numeric_value(), Some(0.0));
        assert_eq!(slider.max_numeric_value(), Some(100.0));
        assert!(slider.numeric_value().is_some());
        assert!(
            slider
                .description()
                .is_some_and(|description| description.contains("Left and Right arrow keys"))
        );

        for forbidden in [
            "Test microphone",
            "Voice activation",
            "Microphone input level",
            "Voice detected",
            "Clipping",
        ] {
            assert!(
                !update
                    .nodes
                    .iter()
                    .any(|(_, node)| node.name() == Some(forbidden))
            );
        }
        assert!(!update.nodes.iter().any(|(_, node)| {
            node.name()
                .is_some_and(|name| name.contains("dBFS") || name.contains("RMS"))
        }));
    }

    #[test]
    fn input_sensitivity_avoids_live_meter_announcements_and_remains_usable() {
        fn render(
            ctx: &egui::Context,
            app: &mut LocalTranscriberApp,
        ) -> egui::accesskit::TreeUpdate {
            ctx.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1_024.0, 1_600.0),
                    )),
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default()
                        .frame(content_panel_frame(ctx))
                        .show(ctx, |ui| app.ui_general_settings(ui));
                },
            )
            .platform_output
            .accesskit_update
            .unwrap()
        }

        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        configure_stitch_style(&ctx);
        let session = RecordingSession::simulated(None, CaptureStopReason::Explicit);
        session.set_simulated_telemetry(LevelSnapshot {
            rms: 0.08,
            peak: 0.15,
        });
        let mut app = test_app();
        app.settings_tab = SettingsTab::Recording;
        app.microphone_test = MicrophoneTest::Active { session };
        let update = render(&ctx, &mut app);
        assert!(update.nodes.iter().any(|(_, node)| {
            node.role() == egui::accesskit::Role::Slider
                && node.name() == Some("Input level sensitivity")
        }));
        assert!(update.nodes.iter().any(|(_, node)| {
            node.role() == egui::accesskit::Role::Slider
                && node
                    .description()
                    .is_some_and(|description| description.contains("Input detected"))
        }));
        assert!(!update.nodes.iter().any(|(_, node)| {
            node.live().is_some()
                && node.name().is_some_and(|name| {
                    name.contains("microphone")
                        || name.contains("Voice detected")
                        || name.contains("Clipping")
                        || name.contains("RMS")
                })
        }));

        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        configure_stitch_style(&ctx);
        let mut app = test_app();
        app.settings_tab = SettingsTab::Recording;
        app.microphone_test_error = Some("Microphone permission denied".to_owned());
        app.microphone_monitor_retry_required = true;
        let update = render(&ctx, &mut app);
        assert!(update.nodes.iter().any(|(_, node)| {
            node.role() == egui::accesskit::Role::Slider
                && node.name() == Some("Input level sensitivity")
        }));
        assert!(update.nodes.iter().any(|(_, node)| {
            node.role() == egui::accesskit::Role::Slider
                && node
                    .description()
                    .is_some_and(|description| description.contains("No input detected"))
        }));
        assert!(!update.nodes.iter().any(|(_, node)| {
            node.name() == Some("Voice detected") || node.name() == Some("Clipping")
        }));
        assert!(update.nodes.iter().any(|(_, node)| {
            node.role() == egui::accesskit::Role::Alert
                && node.name() == Some("Microphone access error")
        }));
        assert!(
            update.nodes.iter().any(|(_, node)| {
                node.name() == Some("Scribe couldn’t access your microphone.")
            })
        );
        assert!(update.nodes.iter().any(|(_, node)| {
            node.role() == egui::accesskit::Role::Button && node.name() == Some("Try again")
        }));
    }

    #[test]
    fn transcribe_recording_does_not_render_a_second_microphone_meter() {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        configure_stitch_style(&ctx);
        let mut app = test_app();
        app.active_recording = Some(ActiveRecording {
            session_id: SessionId(903),
            session: RecordingSession::simulated(None, CaptureStopReason::Explicit),
            source: RecordingSource::Transcribe,
            stop_requested: false,
            started_at: Instant::now(),
            max_duration_seconds: 30,
            latency: LatencyTrace::started_at(Instant::now(), TriggerObservation::AppAction),
            capture_diagnostics: CaptureDiagnosticContext::from_config(&app.config),
        });

        let output = ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1_024.0, 1_100.0),
                )),
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default()
                    .frame(content_panel_frame(ctx))
                    .show(ctx, |ui| app.ui_transcribe(ui));
            },
        );
        let update = output.platform_output.accesskit_update.unwrap();
        assert!(!update.nodes.iter().any(|(_, node)| {
            node.name().is_some_and(|name| {
                name.contains("Input RMS") || name.contains("input level") || name.contains("peak")
            })
        }));
    }

    #[test]
    fn transcript_and_queued_output_state_are_accessible() {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        configure_stitch_style(&ctx);
        ctx.set_visuals(stitch_visuals(ThemeMode::Light));
        let mut app = test_app();
        app.current_tab = Tab::Transcribe;
        app.transcript = "final text".to_owned();
        app.pending_output = Some(PendingOutput {
            session_id: SessionId(1),
            history_id: None,
            transcript: app.transcript.clone(),
            completion_message: "Complete".to_owned(),
            config: app.config.clone(),
            latency: None,
        });

        let output = ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1_024.0, 1_100.0),
                )),
                ..Default::default()
            },
            |ctx| {
                show_test_navigation(ctx, &mut app.current_tab);
                egui::CentralPanel::default()
                    .frame(content_panel_frame(ctx))
                    .show(ctx, |ui| app.ui_transcribe(ui));
            },
        );
        let update = output.platform_output.accesskit_update.unwrap();
        assert!(update.nodes.iter().any(|(_, node)| {
            node.role() == egui::accesskit::Role::Group && node.name() == Some("Transcript panel")
        }));
        assert!(update.nodes.iter().any(|(_, node)| {
            node.role() == egui::accesskit::Role::StaticText && node.name() == Some("final text")
        }));
        assert!(update.nodes.iter().any(|(_, node)| {
            node.role() == egui::accesskit::Role::Button && node.name() == Some("Clear")
        }));
    }

    #[test]
    fn page_and_history_card_titles_are_semantic_headings() {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let records = [HistoryRecord {
            id: 1,
            created_at_ms: 1,
            updated_at_ms: 1,
            completed_at_ms: Some(1),
            status: HistoryStatus::Completed,
            raw_text: "raw final".to_owned(),
            final_text: Some("clean final".to_owned()),
            model_id: "whisper_cpp_base_en".to_owned(),
            metrics: HistoryMetrics::default(),
            pinned: false,
            source_app: None,
            audio_path: None,
            failure: None,
            retry_count: 0,
            output_outcome: None,
        }];
        let output = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                page(ui, "History", TranscriptionStatus::Idle, "Ready", |ui| {
                    history_page(
                        ui,
                        HistoryPageState {
                            search: &mut String::new(),
                            records: &records,
                            has_more: false,
                            loading: false,
                            error: None,
                            confirm_delete: Some(1),
                            work_active: false,
                            playing: None,
                            playback_stopping: false,
                            armed_repaste: None,
                            focus_search: false,
                            focus_delete_confirmation: true,
                        },
                    );
                });
            });
        });
        let update = output.platform_output.accesskit_update.unwrap();

        assert!(update.nodes.iter().any(|(_, node)| {
            node.role() == egui::accesskit::Role::Heading && node.name() == Some("History")
        }));
        assert!(update.nodes.iter().any(|(_, node)| {
            node.role() == egui::accesskit::Role::Heading
                && node
                    .name()
                    .is_some_and(|name| name.starts_with("Completed - "))
        }));
        for expected in ["Copy", "Paste again", "Pin", "Delete entry"] {
            assert!(update.nodes.iter().any(|(_, node)| {
                node.role() == egui::accesskit::Role::Button && node.name() == Some(expected)
            }));
        }
        let group = update
            .nodes
            .iter()
            .find(|(_, node)| {
                node.role() == egui::accesskit::Role::Group
                    && node
                        .name()
                        .is_some_and(|name| name.contains("model whisper_cpp_base_en"))
            })
            .expect("missing contextual history group");
        let contextual_heading_id = update
            .nodes
            .iter()
            .find(|(_, node)| {
                node.role() == egui::accesskit::Role::Heading
                    && node
                        .name()
                        .is_some_and(|name| name.starts_with("Completed - "))
            })
            .map(|(id, _)| *id)
            .expect("missing contextual history heading");
        let delete_id = update
            .nodes
            .iter()
            .find(|(_, node)| {
                node.role() == egui::accesskit::Role::Button && node.name() == Some("Delete entry")
            })
            .map(|(id, _)| *id)
            .expect("missing history delete action");
        assert!(group.1.children().contains(&contextual_heading_id));
        assert!(group.1.children().contains(&delete_id));
        assert!(update.nodes.iter().any(|(_, node)| {
            node.name() == Some("1 history entries loaded")
                && node.live() == Some(egui::accesskit::Live::Polite)
                && node.is_live_atomic()
        }));
        assert!(update.nodes.iter().any(|(_, node)| {
            node.role() == egui::accesskit::Role::Button
                && node.name() == Some("Delete entry")
                && node
                    .description()
                    .is_some_and(|description| description.contains("whisper_cpp_base_en"))
        }));
        let focused = update
            .nodes
            .iter()
            .find(|(id, _)| *id == update.focus)
            .map(|(_, node)| node);
        assert_eq!(focused.and_then(|node| node.name()), Some("Cancel"));
        let disclosure = update
            .nodes
            .iter()
            .map(|(_, node)| node)
            .find(|node| node.name() == Some("Raw transcript"))
            .expect("missing raw transcript disclosure");
        assert_eq!(disclosure.is_expanded(), Some(false));
    }

    #[test]
    fn faster_whisper_large_v3_has_progress_total() {
        let model = config::configured_models(&AppConfig::default())
            .into_iter()
            .find(|model| model.id == "faster_whisper_large_v3")
            .unwrap();

        assert_eq!(model_storage_estimate(&model), "~3.1 GB");
        assert_eq!(
            model_download_total_bytes(&model),
            Some((3.1_f64 * 1024.0 * 1024.0 * 1024.0).round() as u64)
        );
    }

    #[test]
    fn normalized_descriptors_expose_only_evidence_backed_device_capabilities() {
        let service = TranscriptionService::new(AppConfig::default());
        let descriptors = service.model_descriptors();

        assert!(!descriptors.is_empty());
        assert!(
            descriptors
                .iter()
                .all(|descriptor| descriptor.capabilities.cpu)
        );
        assert!(
            descriptors
                .iter()
                .all(|descriptor| !descriptor.capabilities.gpu)
        );
    }

    #[test]
    fn playground_cards_are_sourced_only_from_service_descriptors() {
        let root =
            std::env::temp_dir().join(format!("scribe-neutral-playground-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let primary = root.join("primary.gguf");
        let legacy = root.join("legacy");
        fs::create_dir_all(legacy.join("am")).unwrap();
        fs::create_dir_all(legacy.join("conf")).unwrap();
        fs::create_dir_all(legacy.join("graph")).unwrap();
        fs::write(&primary, b"model").unwrap();
        fs::write(legacy.join("am/final.mdl"), b"model").unwrap();
        fs::write(legacy.join("conf/model.conf"), b"config").unwrap();
        fs::write(legacy.join("graph/HCLG.fst"), b"graph").unwrap();

        let mut config = AppConfig::default();
        config
            .general
            .model_paths
            .insert("whisper_cpp_tiny_en".to_owned(), primary);
        config
            .general
            .model_paths
            .insert("vosk_small_en".to_owned(), legacy);
        config.general.playground_selected_models =
            vec!["whisper_cpp_tiny_en".to_owned(), "vosk_small_en".to_owned()];
        let service = TranscriptionService::new(config.clone());

        let cards = cards_from_config(&config, &service);

        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].descriptor.id.as_str(), "whisper_cpp_tiny_en");
        assert_eq!(cards[0].descriptor.display_name, "Whisper Tiny — English");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn vosk_small_en_has_progress_total_and_managed_download() {
        let model = config::configured_models(&AppConfig::default())
            .into_iter()
            .find(|model| model.id == "vosk_small_en")
            .unwrap();

        assert_eq!(model_storage_estimate(&model), "~50 MB");
        assert_eq!(
            model.download_model.as_deref(),
            Some("vosk-model-small-en-us-0.15")
        );
        assert_eq!(model_download_total_bytes(&model), Some(40 * 1024 * 1024));
    }

    #[test]
    fn sherpa_family_models_have_progress_totals_and_managed_downloads() {
        let models = config::configured_models(&AppConfig::default());
        let sherpa = models
            .iter()
            .find(|model| model.id == "sherpa_onnx_zipformer_small")
            .unwrap();
        let moonshine = models.iter().find(|model| model.id == "moonshine").unwrap();
        let parakeet = models
            .iter()
            .find(|model| model.id == "parakeet_0_6b")
            .unwrap();

        assert_eq!(
            sherpa.download_model.as_deref(),
            Some("sherpa-onnx-zipformer-small-en-2023-06-26")
        );
        assert_eq!(model_download_total_bytes(sherpa), Some(85 * 1024 * 1024));
        assert_eq!(
            moonshine.download_model.as_deref(),
            Some("sherpa-onnx-moonshine-tiny-en-quantized-2026-02-27")
        );
        assert_eq!(model_storage_estimate(moonshine), "~35 MB");
        assert_eq!(
            model_download_total_bytes(moonshine),
            Some(35 * 1024 * 1024)
        );
        assert_eq!(
            parakeet.download_model.as_deref(),
            Some("sherpa-onnx-nemo-parakeet-unified-en-0.6b-int8-non-streaming")
        );
        assert_eq!(model_storage_estimate(parakeet), "~640 MB");
        assert_eq!(
            model_download_total_bytes(parakeet),
            Some(650 * 1024 * 1024)
        );
    }

    #[test]
    fn runtime_action_state_explains_supported_and_unsupported_runtimes() {
        let runtime_root =
            std::env::temp_dir().join(format!("scribe-runtime-action-{}", std::process::id()));
        let _ = fs::remove_dir_all(&runtime_root);

        let whisper = test_model();
        let mut config = AppConfig::default();
        let whisper_runtime = runtime_root
            .join("whisper_cpp")
            .join("bin")
            .join("whisper-cli");
        fs::create_dir_all(whisper_runtime.parent().unwrap()).unwrap();
        fs::write(&whisper_runtime, b"whisper runtime").unwrap();
        config.general.managed_runtimes.insert(
            "whisper_cpp".to_owned(),
            managed_runtime_with_version(whisper_runtime, None),
        );

        assert_eq!(
            runtime_action_state(&config, &whisper),
            RuntimeActionState {
                kind: RuntimeActionKind::Uninstall,
                enabled: true,
                disabled_tooltip: None,
            }
        );

        let mut faster_whisper = whisper;
        faster_whisper.id = "faster_whisper_tiny_en".to_owned();
        faster_whisper.backend = "faster-whisper".to_owned();
        faster_whisper.download_model = Some("tiny.en".to_owned());
        let action = runtime_action_state(&AppConfig::default(), &faster_whisper);

        assert_eq!(
            action,
            expected_runtime_install_action(&faster_whisper.backend)
        );

        let mut vosk = test_model();
        vosk.id = "vosk_small_en".to_owned();
        vosk.name = "Vosk small English".to_owned();
        vosk.backend = "Vosk".to_owned();
        vosk.download_model = Some("vosk-model-small-en-us-0.15".to_owned());

        assert_eq!(
            runtime_action_state(&AppConfig::default(), &vosk),
            expected_runtime_install_action(&vosk.backend)
        );

        config.general.managed_runtimes.clear();
        config.general.managed_runtimes.insert(
            "vosk".to_owned(),
            managed_runtime_with_version(
                write_vosk_runtime(&runtime_root.join("vosk")),
                Some("0.3.45"),
            ),
        );

        assert_eq!(
            runtime_action_state(&config, &vosk),
            RuntimeActionState {
                kind: RuntimeActionKind::Uninstall,
                enabled: true,
                disabled_tooltip: None,
            }
        );

        let managed_models = [
            (
                "sherpa_onnx_zipformer_small",
                "sherpa-onnx",
                "sherpa_onnx",
                "scribe-sherpa-onnx",
                "sherpa-onnx-zipformer-small-en-2023-06-26",
            ),
            (
                "moonshine",
                "Moonshine",
                "moonshine",
                "scribe-moonshine",
                "sherpa-onnx-moonshine-tiny-en-quantized-2026-02-27",
            ),
            (
                "parakeet_0_6b",
                "Parakeet",
                "parakeet",
                "scribe-parakeet",
                "sherpa-onnx-nemo-parakeet-unified-en-0.6b-int8-non-streaming",
            ),
        ];
        for (model_id, backend, runtime_id, wrapper, download_model) in managed_models {
            let mut model = test_model();
            model.id = model_id.to_owned();
            model.backend = backend.to_owned();
            model.download_model = Some(download_model.to_owned());

            assert_eq!(
                runtime_action_state(&AppConfig::default(), &model),
                expected_runtime_install_action(&model.backend),
                "{backend} should be installable"
            );

            config.general.managed_runtimes.clear();
            config.general.managed_runtimes.insert(
                runtime_id.to_owned(),
                managed_runtime_with_version(
                    write_sherpa_family_runtime(
                        &runtime_root.join(runtime_id),
                        runtime_id,
                        wrapper,
                    ),
                    Some("1.13.3"),
                ),
            );

            assert_eq!(
                runtime_action_state(&config, &model),
                RuntimeActionState {
                    kind: RuntimeActionKind::Uninstall,
                    enabled: true,
                    disabled_tooltip: None,
                },
                "{backend} should detect installed runtime"
            );
        }

        let mut unsupported = test_model();
        unsupported.backend = "Unsupported".to_owned();
        let unsupported_action = runtime_action_state(&AppConfig::default(), &unsupported);
        assert_eq!(unsupported_action.kind, RuntimeActionKind::Install);
        assert!(!unsupported_action.enabled);
        assert!(
            unsupported_action
                .disabled_tooltip
                .as_deref()
                .is_some_and(|tooltip| tooltip.contains("no compatible local provider"))
        );

        let _ = fs::remove_dir_all(runtime_root);
    }

    #[test]
    fn runtime_action_state_ignores_stale_runtime_metadata() {
        let runtime_root =
            std::env::temp_dir().join(format!("scribe-stale-runtime-{}", std::process::id()));
        let _ = fs::remove_dir_all(&runtime_root);

        let mut config = AppConfig::default();
        config.general.managed_runtimes.insert(
            "faster_whisper".to_owned(),
            config::ManagedRuntimeInstall::new(PathBuf::from(
                "/tmp/scribe-runtimes/missing/bin/scribe-faster-whisper",
            )),
        );
        let mut model = test_model();
        model.id = "faster_whisper_tiny_en".to_owned();
        model.backend = "faster-whisper".to_owned();
        model.download_model = Some("tiny.en".to_owned());

        let action = runtime_action_state(&config, &model);

        assert_eq!(action.kind, RuntimeActionKind::Install);
        assert_eq!(action, expected_runtime_install_action(&model.backend));

        config.general.managed_runtimes.clear();
        config.general.managed_runtimes.insert(
            "vosk".to_owned(),
            config::ManagedRuntimeInstall::new(write_vosk_runtime_with_revision(
                &runtime_root.join("vosk"),
                2,
            )),
        );
        model.backend = "Vosk".to_owned();
        model.id = "vosk_small_en".to_owned();
        model.download_model = Some("vosk-model-small-en-us-0.15".to_owned());

        let action = runtime_action_state(&config, &model);

        assert_eq!(action.kind, RuntimeActionKind::Install);
        assert_eq!(action, expected_runtime_install_action(&model.backend));
        let _ = fs::remove_dir_all(runtime_root);
    }

    #[test]
    fn runtime_version_state_detects_current_stale_and_unknown_installs() {
        let mut model = test_model();
        model.backend = "Vosk".to_owned();
        let provider = compatibility_bridge::provider_for_model(&model).unwrap();
        let mut config = AppConfig::default();

        config.general.managed_runtimes.insert(
            "vosk".to_owned(),
            managed_runtime_with_version(PathBuf::from("/tmp/scribe/vosk"), Some("0.3.45")),
        );
        assert_eq!(
            runtime_version_state(&config, provider),
            RuntimeVersionState::Current("0.3.45".to_owned())
        );

        config.general.managed_runtimes.insert(
            "vosk".to_owned(),
            managed_runtime_with_version(PathBuf::from("/tmp/scribe/vosk"), Some("0.3.44")),
        );
        assert_eq!(
            runtime_version_state(&config, provider),
            RuntimeVersionState::UpdateAvailable {
                installed: Some("0.3.44".to_owned()),
                available: "0.3.45".to_owned(),
            }
        );

        config.general.managed_runtimes.insert(
            "vosk".to_owned(),
            managed_runtime_with_version(PathBuf::from("/tmp/scribe/vosk"), None),
        );
        assert_eq!(
            runtime_version_state(&config, provider),
            RuntimeVersionState::UpdateAvailable {
                installed: None,
                available: "0.3.45".to_owned(),
            }
        );
    }

    #[test]
    fn runtime_action_state_offers_update_for_stale_version_when_source_exists() {
        let runtime_root =
            std::env::temp_dir().join(format!("scribe-runtime-update-{}", std::process::id()));
        let _ = fs::remove_dir_all(&runtime_root);
        let mut config = AppConfig::default();
        let mut model = test_model();
        model.id = "vosk_small_en".to_owned();
        model.backend = "Vosk".to_owned();
        model.download_model = Some("vosk-model-small-en-us-0.15".to_owned());
        config.general.managed_runtimes.insert(
            "vosk".to_owned(),
            managed_runtime_with_version(
                write_vosk_runtime(&runtime_root.join("vosk")),
                Some("0.3.44"),
            ),
        );

        let action = runtime_action_state(&config, &model);

        if runtime_install_source(&config, &model).is_some() {
            assert_eq!(action.kind, RuntimeActionKind::Update);
        } else {
            assert_eq!(action.kind, RuntimeActionKind::Uninstall);
        }
        assert!(action.enabled);
        let _ = fs::remove_dir_all(runtime_root);
    }

    #[test]
    fn managed_runtime_install_record_reads_manifest_metadata() {
        let runtime_root =
            std::env::temp_dir().join(format!("scribe-runtime-manifest-{}", std::process::id()));
        let _ = fs::remove_dir_all(&runtime_root);
        let executable = runtime_root.join("bin").join("scribe-vosk");
        let manifest = runtime_root.join("runtime-manifest.json");
        fs::create_dir_all(executable.parent().unwrap()).unwrap();
        fs::write(&executable, b"runtime").unwrap();
        fs::write(
            manifest,
            r#"{"version":"0.3.45","sha256":"abc123","dependencies":{"vosk":"0.3.45"}}"#,
        )
        .unwrap();

        let install = managed_runtime_install_record(executable, "packaged-runtime");

        assert_eq!(install.version.as_deref(), Some("0.3.45"));
        assert_eq!(install.sha256.as_deref(), Some("abc123"));
        assert_eq!(install.source.as_deref(), Some("packaged-runtime"));
        let _ = fs::remove_dir_all(runtime_root);
    }

    #[test]
    fn development_runtime_installs_require_debug_build_or_opt_in() {
        assert!(development_runtime_installs_enabled_for(true, None));
        assert!(!development_runtime_installs_enabled_for(false, None));
        assert!(development_runtime_installs_enabled_for(false, Some("1")));
        assert!(development_runtime_installs_enabled_for(
            false,
            Some("true")
        ));
        assert!(!development_runtime_installs_enabled_for(false, Some("0")));
    }

    #[cfg(unix)]
    #[test]
    fn development_runtime_script_installs_expected_executable() {
        use std::os::unix::fs::PermissionsExt;

        let root =
            std::env::temp_dir().join(format!("scribe-runtime-script-test-{}", std::process::id()));
        let script = root.join("bundle-test-runtime.sh");
        let destination = root.join("runtime");
        let executable = destination.join("bin").join("scribe-test");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        fs::write(
            &script,
            "#!/usr/bin/env bash\nset -euo pipefail\nmkdir -p \"$SCRIBE_TEST_RUNTIME_DEST/bin\"\nprintf '#!/usr/bin/env bash\\n' > \"$SCRIBE_TEST_RUNTIME_DEST/bin/scribe-test\"\nchmod 755 \"$SCRIBE_TEST_RUNTIME_DEST/bin/scribe-test\"\n",
        )
        .unwrap();
        let mut permissions = fs::metadata(&script).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&script, permissions).unwrap();

        let installed = build_development_runtime_package(
            "test",
            "test",
            DevelopmentRuntimePackage {
                script,
                destination_env: "SCRIBE_TEST_RUNTIME_DEST",
                destination_root: destination,
                executable_path: executable.clone(),
            },
        )
        .unwrap();

        assert_eq!(installed.installed_path, executable);
        assert!(installed.installed_path.exists());
        installed.commit().unwrap();
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn development_runtime_script_rejects_broken_python_sidecar() {
        use std::os::unix::fs::PermissionsExt;

        let root = std::env::temp_dir().join(format!(
            "scribe-broken-python-runtime-test-{}",
            std::process::id()
        ));
        let script = root.join("bundle-broken-runtime.sh");
        let destination = root.join("runtime");
        let executable = destination.join("bin").join("scribe-faster-whisper");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(executable.parent().unwrap()).unwrap();
        fs::write(&executable, b"runtime").unwrap();
        fs::write(
            destination.join("bin").join("faster_whisper_runner.py"),
            b"runner",
        )
        .unwrap();
        fs::write(&script, "#!/usr/bin/env bash\nexit 0\n").unwrap();
        let mut permissions = fs::metadata(&script).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&script, permissions).unwrap();

        let err = build_development_runtime_package(
            "faster_whisper",
            "faster-whisper",
            DevelopmentRuntimePackage {
                script,
                destination_env: "SCRIBE_TEST_RUNTIME_DEST",
                destination_root: destination,
                executable_path: executable,
            },
        )
        .unwrap_err();

        assert!(err.contains("usable runtime"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn faster_whisper_model_needs_runtime_instead_of_placeholder_backend_message() {
        let mut model = test_model();
        model.id = "faster_whisper_tiny_en".to_owned();
        model.name = "faster-whisper tiny.en".to_owned();
        model.backend = "faster-whisper".to_owned();
        model.local_path = Some(PathBuf::from("/tmp/scribe-fw-tiny"));
        model.install_status = ModelInstallStatus::Installed;
        model.download_model = Some("tiny.en".to_owned());

        let status = runtime_status_for_model(&AppConfig::default(), &model);

        assert_eq!(status, ModelRuntimeStatus::MissingConfiguration);
        assert!(!setup_message_for_status(&status).contains("choose a whisper.cpp model"));
    }

    #[test]
    fn embedded_gguf_model_is_ready_without_a_runtime_package() {
        let mut model = config::configured_models(&AppConfig::default())
            .into_iter()
            .find(|model| model.id == "whisper_cpp_tiny_en")
            .unwrap();
        model.install_status = ModelInstallStatus::Installed;

        assert_eq!(
            runtime_status_for_model(&AppConfig::default(), &model),
            ModelRuntimeStatus::Ready
        );
    }

    #[test]
    fn vosk_model_needs_runtime_instead_of_placeholder_backend_message() {
        let mut model = test_model();
        model.id = "vosk_small_en".to_owned();
        model.name = "Vosk small English".to_owned();
        model.backend = "Vosk".to_owned();
        model.local_path = Some(PathBuf::from("/tmp/scribe-vosk-small"));
        model.install_status = ModelInstallStatus::Installed;
        model.download_model = Some("vosk-model-small-en-us-0.15".to_owned());

        let status = runtime_status_for_model(&AppConfig::default(), &model);

        assert_eq!(status, ModelRuntimeStatus::MissingConfiguration);
        assert!(!setup_message_for_status(&status).contains("choose a whisper.cpp model"));
    }

    #[test]
    fn runtime_uninstall_target_only_allows_app_runtime_storage() {
        let root = std::env::temp_dir().join(format!(
            "scribe-runtime-uninstall-{}-{}",
            std::process::id(),
            NEXT_TEST_SESSION.fetch_add(1, Ordering::Relaxed)
        ));
        let storage_dir = root.join("runtimes");
        let runtime_dir = storage_dir.join("whisper_cpp");
        let runtime_executable = runtime_dir.join("bin").join("whisper-cli");
        let sibling_runtime_file = storage_dir.join("legacy-whisper-cli");
        let external_runtime = root.join("external").join("whisper-cli");
        fs::create_dir_all(runtime_executable.parent().unwrap()).unwrap();
        fs::create_dir_all(external_runtime.parent().unwrap()).unwrap();
        fs::write(&runtime_executable, b"runtime").unwrap();
        fs::write(&sibling_runtime_file, b"legacy").unwrap();
        fs::write(&external_runtime, b"external").unwrap();

        assert_eq!(
            runtime_uninstall_target(&storage_dir, "whisper_cpp", &runtime_executable),
            Some(runtime_dir.clone())
        );
        assert_eq!(
            runtime_uninstall_target(&storage_dir, "whisper_cpp", &sibling_runtime_file),
            Some(sibling_runtime_file)
        );
        assert_eq!(
            runtime_uninstall_target(&storage_dir, "whisper_cpp", &external_runtime),
            None
        );
        assert_eq!(
            runtime_uninstall_target(
                &storage_dir,
                "whisper_cpp",
                &runtime_dir.join("bin").join("..").join("whisper-cli")
            ),
            None
        );
        assert_eq!(
            runtime_uninstall_target(&storage_dir, "../external", &runtime_executable),
            None
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn runtime_uninstall_target_rejects_symlink_escape() {
        use std::os::unix::fs::symlink;

        let root = std::env::temp_dir().join(format!(
            "scribe-runtime-uninstall-link-{}-{}",
            std::process::id(),
            NEXT_TEST_SESSION.fetch_add(1, Ordering::Relaxed)
        ));
        let storage_dir = root.join("runtimes");
        let external = root.join("external");
        fs::create_dir_all(&storage_dir).unwrap();
        fs::create_dir_all(&external).unwrap();
        fs::write(external.join("whisper-cli"), b"external").unwrap();
        let linked_runtime = storage_dir.join("whisper_cpp");
        symlink(&external, &linked_runtime).unwrap();

        assert_eq!(
            runtime_uninstall_target(
                &storage_dir,
                "whisper_cpp",
                &linked_runtime.join("whisper-cli")
            ),
            None
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn uninstall_removes_managed_model_file_and_selects_next_installed_model() {
        let temp_dir =
            std::env::temp_dir().join(format!("scribe-uninstall-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&temp_dir);
        let model_dir = temp_dir.join("whisper.cpp");
        fs::create_dir_all(&model_dir).unwrap();
        let base_path = model_dir.join("ggml-base.en.bin");
        let small_path = model_dir.join("ggml-small.en.bin");
        fs::write(&base_path, b"base").unwrap();
        fs::write(&small_path, b"small").unwrap();

        let mut config = AppConfig::default();
        config.general.selected_default_model = "whisper_cpp_base_en".to_owned();
        config.general.model_storage_dir = temp_dir.clone();
        config.general.managed_models.insert(
            "whisper_cpp_base_en".to_owned(),
            config::ManagedModelInstall::new(base_path.clone()),
        );
        config.general.managed_models.insert(
            "whisper_cpp_small_en".to_owned(),
            config::ManagedModelInstall::new(small_path.clone()),
        );

        let removal =
            ManagedRemoval::stage(&base_path, std::slice::from_ref(&base_path), "0".repeat(64))
                .unwrap();
        assert!(removal.removed_files());
        removal.commit().unwrap();
        assert!(!base_path.exists());
        config.general.managed_models.remove("whisper_cpp_base_en");
        select_first_installed_model(&mut config);

        assert_eq!(
            config.general.selected_default_model,
            "whisper_cpp_small_en"
        );

        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn uninstall_clears_active_model_when_no_installed_models_remain() {
        let temp_dir =
            std::env::temp_dir().join(format!("scribe-empty-models-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&temp_dir);
        let mut config = AppConfig::default();
        config.general.selected_default_model = "whisper_cpp_base_en".to_owned();
        config.general.model_storage_dir = temp_dir.clone();

        select_first_installed_model(&mut config);

        assert!(config.general.selected_default_model.is_empty());
        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn uninstall_does_not_delete_external_migrated_model_paths() {
        let temp_dir =
            std::env::temp_dir().join(format!("scribe-external-model-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&temp_dir);
        fs::create_dir_all(&temp_dir).unwrap();
        let app_storage = temp_dir.join("app-models");
        let external_path = temp_dir.join("external").join("ggml-base.en.bin");
        fs::create_dir_all(external_path.parent().unwrap()).unwrap();
        fs::write(&external_path, b"external").unwrap();

        let mut model_paths = HashMap::new();
        model_paths.insert("whisper_cpp_base_en".to_owned(), external_path.clone());
        let mut config = AppConfig::default();
        config.general.model_storage_dir = app_storage;
        config.general.model_paths = model_paths;
        assert!(!is_app_managed_model_path(&config, &external_path));
        assert!(external_path.exists());

        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn removing_an_imported_model_only_removes_its_scribe_owned_receipt() {
        let root = std::env::temp_dir().join(format!(
            "scribe-imported-removal-test-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        let storage = root.join("scribe-models");
        let external = root.join("external").join("imported.gguf");
        fs::create_dir_all(external.parent().unwrap()).unwrap();
        fs::write(&external, b"external").unwrap();
        let model_id = ModelId::new("local-aaaaaaaaaaaaaaaaaaaaaaaa");
        let receipt = installed_manifest::imported_manifest_path_for(&storage, &model_id);
        fs::create_dir_all(receipt.parent().unwrap()).unwrap();
        fs::write(&receipt, b"scribe receipt").unwrap();

        let mut config = AppConfig::default();
        config.general.model_storage_dir = storage;
        assert!(is_app_managed_model_path(&config, &receipt));
        assert!(!is_app_managed_model_path(&config, &external));

        let removal =
            ManagedRemoval::stage(&receipt, std::slice::from_ref(&receipt), "0".repeat(64))
                .unwrap();
        removal.commit().unwrap();

        assert!(!receipt.exists());
        assert_eq!(fs::read(&external).unwrap(), b"external");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn local_gguf_import_rejects_sources_inside_scribe_storage() {
        let root = std::env::temp_dir().join(format!(
            "scribe-import-owned-source-test-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        let storage = root.join("scribe-models");
        let source = storage.join("owned.gguf");
        fs::create_dir_all(&storage).unwrap();
        fs::write(&source, b"not an importable runtime fixture").unwrap();

        let fingerprint =
            fingerprint_file_cancellable(&source, &InstallCancellation::default()).unwrap();
        let error = reject_import_source_in_model_storage(&fingerprint.canonical_path, &storage)
            .unwrap_err();

        assert!(error.contains("outside Scribe's managed model storage"));
        assert!(!storage.join("imported-receipts").exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn local_gguf_import_recheck_rejects_same_size_changed_bytes() {
        let root = std::env::temp_dir().join(format!(
            "scribe-import-fingerprint-recheck-test-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        let source = root.join("external").join("imported.gguf");
        fs::create_dir_all(source.parent().unwrap()).unwrap();
        fs::write(&source, b"original").unwrap();
        let before =
            fingerprint_file_cancellable(&source, &InstallCancellation::default()).unwrap();
        fs::write(&source, b"changed!").unwrap();
        let after = fingerprint_file_cancellable(&source, &InstallCancellation::default()).unwrap();

        assert_eq!(after.size_bytes, before.size_bytes);
        assert!(ensure_local_gguf_fingerprint_unchanged(&before, &after).is_err());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn local_gguf_final_fingerprint_honors_cancellation() {
        let root = std::env::temp_dir().join(format!(
            "scribe-import-fingerprint-cancel-test-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        let source = root.join("external").join("imported.gguf");
        fs::create_dir_all(source.parent().unwrap()).unwrap();
        fs::write(&source, b"fixture").unwrap();
        let cancellation = InstallCancellation::default();
        cancellation.cancel();

        let result = fingerprint_file_cancellable(&source, &cancellation);

        assert!(matches!(result, Err(InstallError::Cancelled { .. })));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn local_gguf_ui_completion_performs_no_source_file_read_or_fingerprint() {
        let source = include_str!("app.rs");
        let start = source
            .find("    fn finish_local_gguf_import(")
            .expect("local GGUF completion boundary exists");
        let end = source[start..]
            .find("\n    fn uninstall_model(")
            .map(|offset| start + offset)
            .expect("local GGUF completion boundary remains scoped");
        let completion = &source[start..end];

        for forbidden in [
            "fingerprint_file_cancellable",
            "fs::read",
            "fs::metadata",
            "fs::canonicalize",
        ] {
            assert!(
                !completion.contains(forbidden),
                "UI completion must not call source I/O boundary {forbidden}"
            );
        }
        assert!(completion.contains("self.rebuild_local_models_after_committed_change();"));
        assert!(
            completion
                .find("installed_manifest::persist_manifest_at")
                .unwrap()
                < completion
                    .find("self.rebuild_local_models_after_committed_change();")
                    .unwrap()
        );
    }

    #[test]
    fn successful_runtime_removal_paths_rebuild_inventory_only_after_commit() {
        let source = include_str!("app.rs");
        let start = source.find("    fn uninstall_runtime(").unwrap();
        let end = source[start..]
            .find("\n    fn refresh_playground_runtime_statuses(")
            .map(|offset| start + offset)
            .unwrap();
        let uninstall = &source[start..end];
        let legacy_start = uninstall.find("        let Some(provider)").unwrap();
        let (managed, legacy) = uninstall.split_at(legacy_start);
        let refresh = "self.rebuild_local_models_after_committed_change();";

        assert_eq!(uninstall.matches(refresh).count(), 2);
        assert!(managed.find("removal.commit()").unwrap() < managed.find(refresh).unwrap());
        assert!(managed.find("config::save_config").unwrap() < managed.find(refresh).unwrap());
        assert!(legacy.find("config::save_config").unwrap() < legacy.find(refresh).unwrap());
    }

    #[test]
    fn committed_inventory_change_replaces_the_cached_local_projection() {
        let mut app = test_app();
        let before = Arc::clone(&app.remote_catalog.local_models);
        let builds = app.remote_catalog.local_models_build_count;
        app.config.general.selected_default_model = "not-selected".to_owned();

        app.rebuild_local_models_after_committed_change();

        assert_eq!(app.remote_catalog.local_models_build_count, builds + 1);
        assert!(!Arc::ptr_eq(&before, &app.remote_catalog.local_models));
        assert!(
            app.remote_catalog
                .local_models
                .iter()
                .find(|model| model.id == "whisper_cpp_tiny_en")
                .is_some_and(|model| !model.active)
        );
    }

    #[test]
    fn preview_updates_are_overlay_only_and_reject_stale_or_wrong_model_events() {
        let mut app = test_app();
        app.transcript = "previous final".to_owned();
        let session_id = app
            .session_coordinator
            .begin(SessionPurpose::Dictation)
            .unwrap();
        app.session_coordinator.capture_started(session_id).unwrap();
        let model_id = ModelId::new("whisper_cpp_base_en");
        let request_id = app
            .session_coordinator
            .start_preview(session_id, model_id.clone())
            .unwrap();
        app.overlay_controller
            .begin_session(session_id, NativeOverlayMode::Live);

        let event =
            |sequence, model_id: ModelId, committed: &str, tentative: &str| PreviewEvent::Update {
                identity: StreamIdentity {
                    session_id,
                    request_id,
                    model_id,
                    sequence,
                },
                update: StreamUpdate {
                    committed: committed.to_owned(),
                    tentative: tentative.to_owned(),
                },
            };
        app.apply_rolling_preview_event(event(1, model_id.clone(), "hello", "world"));

        assert_eq!(app.transcript, "previous final");
        assert!(app.pending_output.is_none());
        assert_eq!(app.overlay_controller.state().transcript.committed, "hello");
        assert_eq!(app.overlay_controller.state().transcript.tentative, "world");

        app.apply_rolling_preview_event(event(1, model_id.clone(), "stale", "replacement"));
        app.apply_rolling_preview_event(event(2, ModelId::new("wrong"), "wrong", "model"));
        assert_eq!(app.overlay_controller.state().transcript.committed, "hello");
        assert_eq!(app.overlay_controller.state().transcript.tentative, "world");
        assert_eq!(app.transcript, "previous final");
        assert!(app.pending_output.is_none());
    }

    #[test]
    fn late_preview_after_close_cannot_replace_final_or_duplicate_output() {
        use std::cell::Cell;

        let mut app = test_app();
        let session_id = app
            .session_coordinator
            .begin(SessionPurpose::Dictation)
            .unwrap();
        app.session_coordinator.capture_started(session_id).unwrap();
        let model_id = ModelId::new("whisper_cpp_base_en");
        let preview_id = app
            .session_coordinator
            .start_preview(session_id, model_id.clone())
            .unwrap();
        app.overlay_controller
            .begin_session(session_id, NativeOverlayMode::Live);
        app.apply_rolling_preview_event(PreviewEvent::Update {
            identity: StreamIdentity {
                session_id,
                request_id: preview_id,
                model_id: model_id.clone(),
                sequence: 1,
            },
            update: StreamUpdate {
                committed: "preview".to_owned(),
                tentative: "draft".to_owned(),
            },
        });
        app.session_coordinator
            .request_stop(session_id, StopReason::Explicit)
            .unwrap();
        app.session_coordinator
            .finish_preview(session_id, preview_id, &model_id)
            .unwrap();
        app.session_coordinator
            .capture_finalized(session_id)
            .unwrap();
        let final_request = app
            .session_coordinator
            .start_request(session_id, model_id.clone())
            .unwrap();

        app.apply_rolling_preview_event(PreviewEvent::Update {
            identity: StreamIdentity {
                session_id,
                request_id: preview_id,
                model_id: model_id.clone(),
                sequence: 2,
            },
            update: StreamUpdate {
                committed: "late".to_owned(),
                tentative: "replacement".to_owned(),
            },
        });
        assert_eq!(
            app.overlay_controller.state().transcript.committed,
            "preview"
        );
        assert_eq!(app.overlay_controller.state().transcript.tentative, "draft");

        app.session_coordinator
            .complete_request(session_id, final_request, &model_id)
            .unwrap();
        app.session_coordinator.begin_output(session_id).unwrap();
        app.transcript = "final text".to_owned();
        app.overlay_controller
            .replace_with_final(session_id, app.transcript.clone());
        app.pending_output = Some(PendingOutput {
            session_id,
            history_id: None,
            transcript: app.transcript.clone(),
            completion_message: "Complete".to_owned(),
            config: app.config.clone(),
            latency: None,
        });
        let calls = Cell::new(0_u32);
        for _ in 0..2 {
            app.poll_pending_output_with(|text, _, _| {
                assert_eq!(text, "final text");
                calls.set(calls.get() + 1);
                text_output::TextOutputResult::Inserted
            });
        }

        assert_eq!(calls.get(), 1);
        assert_eq!(
            app.overlay_controller.state().transcript.committed,
            "final text"
        );
        assert!(
            app.overlay_controller
                .state()
                .transcript
                .tentative
                .is_empty()
        );
    }

    #[test]
    fn playground_and_final_only_mode_never_start_rolling_preview() {
        assert!(rolling_preview_enabled(
            RecordingSource::Transcribe,
            StreamingMode::Auto
        ));
        assert!(rolling_preview_enabled(
            RecordingSource::Transcribe,
            StreamingMode::Rolling
        ));
        assert!(!rolling_preview_enabled(
            RecordingSource::Transcribe,
            StreamingMode::FinalOnly
        ));
        assert!(!rolling_preview_enabled(
            RecordingSource::Playground,
            StreamingMode::Auto
        ));
    }
}
