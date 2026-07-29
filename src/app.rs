use std::collections::HashMap;
use std::env;
use std::fs;
use std::fs::{File, OpenOptions, TryLockError};
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use arboard::Clipboard;
use crossbeam_channel::{Receiver, Sender, unbounded};
use eframe::egui::{
    self, Align, Button, Color32, ComboBox, FontFamily, FontId, Frame, Layout, Margin, RichText,
    Rounding, ScrollArea, Stroke, TextEdit, TextStyle, Ui, Vec2, ViewportCommand,
};
use serde::{Deserialize, Serialize};

use crate::audio::{self, RecordingSession};
use crate::benchmark::{
    self, BenchmarkMetric, BenchmarkModelInput, BenchmarkModelResult, RankingMode,
};
use crate::config::{self, AppConfig, HotkeyMode, ThemeMode, WhisperComputeMode};
use crate::durable_fs;
use crate::hotkey::{HotkeyEvent, HotkeyService};
use crate::managed_downloads;
use crate::models::{
    ModelInstallStatus, ModelRuntimeStatus, SttModelInfo, TranscriptResult, TranscriptionStatus,
    format_bytes,
};
use crate::runtime_artifacts::{self, RuntimeArtifact, RuntimeDevicePack};
use crate::runtime_catalog;
use crate::stt;
use crate::text_output;
use crate::tray::{TrayCommand, TrayService};

const ACTIVE_REPAINT_DELAY: Duration = Duration::from_millis(100);
const IDLE_REPAINT_DELAY: Duration = Duration::from_millis(500);
const RECORD_STATE_MOTION_SECONDS: f32 = 0.18;
const RECORD_HOVER_MOTION_SECONDS: f32 = 0.12;
const RECORD_PRESS_MOTION_SECONDS: f32 = 0.08;
const RECORDING_DURATION_PRESETS: [(u32, &str); 7] = [
    (30, "0.5 minutes"),
    (60, "1 minute"),
    (5 * 60, "5 minutes"),
    (10 * 60, "10 minutes"),
    (30 * 60, "30 minutes"),
    (60 * 60, "60 minutes"),
    (120 * 60, "120 minutes"),
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Tab {
    Transcribe,
    Models,
    Playground,
    Settings,
}

impl Tab {
    fn label(self) -> &'static str {
        match self {
            Self::Transcribe => "Transcribe",
            Self::Models => "Models",
            Self::Playground => "Model Playground",
            Self::Settings => "Settings",
        }
    }
}

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
        "playground" | "model-playground" | "model playground" => Some(Tab::Playground),
        "settings" => Some(Tab::Settings),
        _ => None,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RecordingSource {
    Transcribe,
    Playground,
}

struct ActiveRecording {
    session: RecordingSession,
    source: RecordingSource,
    stop_requested: bool,
    started_at: Instant,
    max_duration_seconds: u32,
    latency: LatencyTrace,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct TrayUiState {
    is_recording: bool,
    has_transcript: bool,
}

#[derive(Clone, Debug)]
struct LatencyTrace {
    activation_at: Instant,
    recorder_started_at: Option<Instant>,
    stop_requested_at: Option<Instant>,
    wav_finalized_at: Option<Instant>,
    transcription_dispatched_at: Option<Instant>,
    transcription_completed_at: Option<Instant>,
    ui_result_at: Option<Instant>,
    paste_completed_at: Option<Instant>,
}

impl LatencyTrace {
    fn started_now() -> Self {
        Self {
            activation_at: Instant::now(),
            recorder_started_at: None,
            stop_requested_at: None,
            wav_finalized_at: None,
            transcription_dispatched_at: None,
            transcription_completed_at: None,
            ui_result_at: None,
            paste_completed_at: None,
        }
    }

    fn summary_lines(&self) -> Vec<String> {
        let mut lines = Vec::new();
        if let Some(duration) = duration_between(Some(self.activation_at), self.recorder_started_at)
        {
            lines.push(format!("Activation to recorder ready: {duration}"));
        }
        if let Some(duration) = duration_between(self.stop_requested_at, self.wav_finalized_at) {
            lines.push(format!("Stop to WAV finalized: {duration}"));
        }
        if let Some(duration) = duration_between(
            self.transcription_dispatched_at,
            self.transcription_completed_at,
        ) {
            lines.push(format!("Transcription job: {duration}"));
        }
        if let Some(duration) = duration_between(self.transcription_completed_at, self.ui_result_at)
        {
            lines.push(format!("STT done to UI update: {duration}"));
        }
        if let Some(duration) = duration_between(self.ui_result_at, self.paste_completed_at) {
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
        self.paste_completed_at
            .or(self.ui_result_at)
            .or(self.transcription_completed_at)
            .or(self.wav_finalized_at)
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

#[derive(Clone, Debug)]
struct PlaygroundCardState {
    model: SttModelInfo,
    status: ModelRuntimeStatus,
    transcript: String,
    latency_ms: Option<u128>,
    audio_duration_ms: Option<u128>,
    peak_ram_mb: Option<f64>,
    peak_vram_mb: Option<f64>,
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
    TranscriptionDone {
        source: RecordingSource,
        result: TranscriptResult,
        latency: Option<LatencyTrace>,
    },
    TranscriptionFailed {
        source: RecordingSource,
        model_id: String,
        message: String,
        latency: Option<LatencyTrace>,
    },
    ModelDownloadProgress {
        model_id: String,
        downloaded_bytes: u64,
        total_bytes: Option<u64>,
        bytes_per_second: Option<u64>,
    },
    ModelDownloadDone {
        model_id: String,
        path: PathBuf,
    },
    ModelDownloadFailed {
        model_id: String,
        message: String,
    },
    RuntimeInstallDone {
        runtime_id: String,
        backend: String,
        replacement: Box<RuntimeReplacement>,
        install: config::ManagedRuntimeInstall,
    },
    RuntimeInstallFailed {
        runtime_id: String,
        message: String,
    },
}

fn send_model_download_progress(
    tx: &Sender<AppEvent>,
    progress: managed_downloads::ModelDownloadProgress,
) {
    let _ = tx.send(AppEvent::ModelDownloadProgress {
        model_id: progress.model_id,
        downloaded_bytes: progress.downloaded_bytes,
        total_bytes: progress.total_bytes,
        bytes_per_second: progress.bytes_per_second,
    });
}

fn send_model_download_result(
    tx: &Sender<AppEvent>,
    model_id: String,
    result: Result<PathBuf, String>,
) {
    match result {
        Ok(path) => {
            let _ = tx.send(AppEvent::ModelDownloadDone { model_id, path });
        }
        Err(message) => {
            let _ = tx.send(AppEvent::ModelDownloadFailed { model_id, message });
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ModelPrimaryAction {
    Install,
    Retry,
    Installing,
    Repair,
    Select,
    Active,
}

impl ModelPrimaryAction {
    fn label(self) -> &'static str {
        match self {
            Self::Install => "Install",
            Self::Retry => "Retry",
            Self::Installing => "Installing",
            Self::Repair => "Repair",
            Self::Select => "Select",
            Self::Active => "Active",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RuntimeActionKind {
    Install,
    Update,
    Uninstall,
}

fn runtime_action_label(kind: RuntimeActionKind, backend: &str, busy: bool) -> String {
    if busy {
        return format!("Preparing {backend} runtime");
    }
    let action = match kind {
        RuntimeActionKind::Install => "Install",
        RuntimeActionKind::Update => "Update",
        RuntimeActionKind::Uninstall => "Remove",
    };
    format!("{action} {backend} runtime")
}

fn model_primary_action_label(
    action: ModelPrimaryAction,
    model: &SttModelInfo,
    install_status: &ModelInstallStatus,
) -> String {
    match action {
        ModelPrimaryAction::Repair => format!("Repair {} runtime", model.backend),
        ModelPrimaryAction::Installing
            if matches!(install_status, ModelInstallStatus::InstallingRuntime) =>
        {
            format!("Preparing {} runtime", model.backend)
        }
        _ => action.label().to_owned(),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ModelActionState {
    primary: ModelPrimaryAction,
    primary_enabled: bool,
    show_uninstall: bool,
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
    Persisted {
        job: RuntimeInstallJob,
        durability_warning: Option<String>,
    },
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

#[derive(Debug)]
struct RuntimeReplacement {
    installed_path: PathBuf,
    target_root: PathBuf,
    backup_root: Option<PathBuf>,
    runtime_id: String,
    persistence_install: Option<Option<config::ManagedRuntimeInstall>>,
    _lock: RuntimeInstallLock,
}

#[derive(Debug)]
struct RuntimeUninstallOutcome {
    removed_files: bool,
    durability_warning: Option<String>,
}

#[derive(Debug)]
struct RuntimeInstallLock {
    _file: File,
    previous_install: Option<config::ManagedRuntimeInstall>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum RuntimeTransactionPhase {
    Prepared,
    BackedUp,
    Activated,
    AwaitingPersistence,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct RuntimeTransactionJournal {
    version: u32,
    runtime_id: String,
    phase: RuntimeTransactionPhase,
    had_previous_runtime: bool,
    previous_install: Option<config::ManagedRuntimeInstall>,
    new_install: Option<config::ManagedRuntimeInstall>,
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
        .managed_runtimes
        .insert(runtime_id.to_owned(), install)
}

#[cfg(test)]
fn rollback_runtime_record(
    config: &mut AppConfig,
    runtime_id: &str,
    previous: Option<config::ManagedRuntimeInstall>,
) {
    match previous {
        Some(install) => {
            config
                .managed_runtimes
                .insert(runtime_id.to_owned(), install);
        }
        None => {
            config.managed_runtimes.remove(runtime_id);
        }
    }
}

fn persist_runtime_install(
    config: &mut AppConfig,
    runtime_id: &str,
    install: config::ManagedRuntimeInstall,
    job: RuntimeInstallJob,
    persist: impl FnOnce(&AppConfig) -> Result<config::ConfigSaveOutcome, String>,
) -> RuntimePersistenceTransition {
    let mut candidate = config.clone();
    apply_runtime_record(&mut candidate, runtime_id, install);
    config::normalize_config(&mut candidate);
    match persist(&candidate) {
        Ok(committed) => {
            *config = committed.config;
            RuntimePersistenceTransition::Persisted {
                job,
                durability_warning: committed.durability_warning,
            }
        }
        Err(err) => RuntimePersistenceTransition::Failed {
            job,
            message: format!("Failed to persist the installed runtime: {err}"),
        },
    }
}

fn runtime_metadata_matches(
    config: &AppConfig,
    runtime_id: &str,
    install: &config::ManagedRuntimeInstall,
) -> bool {
    config.managed_runtimes.get(runtime_id) == Some(install)
}

fn missing_runtime_source_message(backend: &str, device: RuntimeDevicePack) -> String {
    format!(
        "No trusted {device} runtime artifact for {backend} is published for {}. Use a release bundle that includes it, or ask the release operator to publish and embed matching metadata.",
        config::current_platform_key(),
        device = device.as_str().to_uppercase(),
    )
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
    Remote(RuntimeArtifact),
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
    platform: Option<String>,
    device: Option<String>,
    cuda_bundled: Option<bool>,
}

#[cfg(test)]
fn model_action_state(
    model: &SttModelInfo,
    install_status: &ModelInstallStatus,
    selected: bool,
) -> ModelActionState {
    model_action_state_with_runtime(model, install_status, selected, true)
}

fn model_action_state_with_runtime(
    model: &SttModelInfo,
    install_status: &ModelInstallStatus,
    selected: bool,
    runtime_ready: bool,
) -> ModelActionState {
    match install_status {
        ModelInstallStatus::Installed => ModelActionState {
            primary: if !runtime_ready {
                ModelPrimaryAction::Repair
            } else if selected {
                ModelPrimaryAction::Active
            } else {
                ModelPrimaryAction::Select
            },
            primary_enabled: !selected || !runtime_ready,
            show_uninstall: supports_managed_uninstall(model, install_status),
        },
        ModelInstallStatus::Downloading { .. } | ModelInstallStatus::InstallingRuntime => {
            ModelActionState {
                primary: ModelPrimaryAction::Installing,
                primary_enabled: false,
                show_uninstall: false,
            }
        }
        ModelInstallStatus::Error(_) => ModelActionState {
            primary: ModelPrimaryAction::Retry,
            primary_enabled: supports_managed_install(model),
            show_uninstall: false,
        },
        ModelInstallStatus::RuntimeError(_) => ModelActionState {
            primary: ModelPrimaryAction::Repair,
            primary_enabled: true,
            show_uninstall: true,
        },
        ModelInstallStatus::Missing | ModelInstallStatus::NotInstalled => ModelActionState {
            primary: ModelPrimaryAction::Install,
            primary_enabled: supports_managed_install(model),
            show_uninstall: false,
        },
    }
}

fn model_primary_disabled_tooltip(
    model: &SttModelInfo,
    install_status: &ModelInstallStatus,
    selected: bool,
    action_state: &ModelActionState,
) -> Option<String> {
    if action_state.primary_enabled {
        return None;
    }

    match action_state.primary {
        ModelPrimaryAction::Active if selected => {
            Some("This model is already the active transcription model.".to_owned())
        }
        ModelPrimaryAction::Installing => Some("This model is still being installed.".to_owned()),
        ModelPrimaryAction::Install | ModelPrimaryAction::Retry
            if !supports_managed_install(model) =>
        {
            Some(format!(
                "Managed downloads are not available for {} models in this build.",
                model.backend
            ))
        }
        ModelPrimaryAction::Retry => match install_status {
            ModelInstallStatus::Error(message) => Some(format!("Install failed: {message}")),
            _ => Some("Retry is not available for this model.".to_owned()),
        },
        ModelPrimaryAction::Select => Some("Install this model before selecting it.".to_owned()),
        ModelPrimaryAction::Repair => match install_status {
            ModelInstallStatus::RuntimeError(message) => {
                Some(format!("Runtime repair failed: {message}"))
            }
            _ => Some("The backend runtime is not ready for this installed model.".to_owned()),
        },
        ModelPrimaryAction::Install => {
            Some("This model does not have a managed installer.".to_owned())
        }
        ModelPrimaryAction::Active => Some("This model cannot be selected right now.".to_owned()),
    }
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
    restrict_runtime_action(state, &model.backend, busy, activity)
}

fn restrict_runtime_action(
    mut state: RuntimeActionState,
    backend: &str,
    busy: bool,
    activity: RuntimeConsumerActivity,
) -> RuntimeActionState {
    if busy {
        state.enabled = false;
        state.disabled_tooltip = Some(format!(
            "The shared {} runtime is already being prepared.",
            backend
        ));
    } else if matches!(
        state.kind,
        RuntimeActionKind::Update | RuntimeActionKind::Uninstall
    ) && let Some(reason) = runtime_consumer_block_reason(backend, activity)
    {
        state.enabled = false;
        state.disabled_tooltip = Some(reason);
    }
    state
}

fn runtime_consumer_block_reason(
    backend: &str,
    activity: RuntimeConsumerActivity,
) -> Option<String> {
    if activity.recording {
        Some(format!(
            "Stop the active recording before changing the shared {backend} runtime."
        ))
    } else if activity.transcribing {
        Some(format!(
            "Wait for transcription to finish before changing the shared {backend} runtime."
        ))
    } else if activity.playground_jobs {
        Some(format!(
            "Wait for Playground jobs to finish before changing the shared {backend} runtime."
        ))
    } else if activity.model_download {
        Some(format!(
            "Wait for the {backend} model download to finish before changing its runtime."
        ))
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
        ) && stt::provider_for_backend(&model.backend)
            .is_some_and(|provider| provider.runtime_id == runtime_id)
    })
}

#[cfg(test)]
fn apply_runtime_uninstall_result(
    config: &mut AppConfig,
    runtime_id: &str,
    removal: Result<bool, String>,
) -> Result<bool, String> {
    let removed_files = removal?;
    config.managed_runtimes.remove(runtime_id);
    Ok(removed_files)
}

fn runtime_action_state_inner(config: &AppConfig, model: &SttModelInfo) -> RuntimeActionState {
    let Some(provider) = stt::provider_for_backend(&model.backend) else {
        return RuntimeActionState {
            kind: RuntimeActionKind::Install,
            enabled: false,
            disabled_tooltip: Some(format!("{} is not a supported STT backend.", model.backend)),
        };
    };

    if !provider.runtime_install_supported {
        return RuntimeActionState {
            kind: RuntimeActionKind::Install,
            enabled: false,
            disabled_tooltip: Some(format!(
                "The managed {} runtime installer is not bundled in this build.",
                model.backend
            )),
        };
    }

    match runtime_install_source(config, model) {
        Ok(source) => runtime_action_state_for_source(config, model, provider, source.is_some()),
        Err(_) if has_managed_runtime_install(config, provider) => {
            runtime_action_state_for_source(config, model, provider, false)
        }
        Err(message) => RuntimeActionState {
            kind: RuntimeActionKind::Install,
            enabled: false,
            disabled_tooltip: Some(format!("Trusted runtime catalog unavailable: {message}")),
        },
    }
}

fn runtime_action_state_for_source(
    config: &AppConfig,
    model: &SttModelInfo,
    provider: &stt::SttProviderAdapter,
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
            disabled_tooltip: Some(missing_runtime_source_message(
                &model.backend,
                runtime_device_pack(config, model),
            )),
        }
    }
}

fn supports_managed_install(model: &SttModelInfo) -> bool {
    stt::provider_for_backend(&model.backend)
        .is_some_and(|provider| provider.can_install_model(model))
}

fn supports_managed_uninstall(model: &SttModelInfo, install_status: &ModelInstallStatus) -> bool {
    stt::provider_for_backend(&model.backend).is_some_and(|provider| {
        let mut model = model.clone();
        model.install_status = install_status.clone();
        provider.can_uninstall_model(&model)
    })
}

fn has_managed_runtime_install(config: &AppConfig, provider: &stt::SttProviderAdapter) -> bool {
    resolve_managed_runtime_executable(config, provider).is_some()
}

fn runtime_needs_update(config: &AppConfig, provider: &stt::SttProviderAdapter) -> bool {
    matches!(
        runtime_version_state(config, provider),
        RuntimeVersionState::UpdateAvailable { .. }
    )
}

fn runtime_version_state(
    config: &AppConfig,
    provider: &stt::SttProviderAdapter,
) -> RuntimeVersionState {
    let available = runtime_artifacts::embedded_artifact(
        provider.runtime_id,
        runtime_version_device_pack(config, provider),
    )
    .ok()
    .flatten()
    .map(|artifact| artifact.version)
    .or_else(|| {
        runtime_catalog::runtime_version_for_runtime_id(provider.runtime_id).map(str::to_owned)
    });
    let Some(available) = available else {
        return RuntimeVersionState::NotTracked;
    };
    let Some(install) = config.managed_runtimes.get(provider.runtime_id) else {
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
            available,
        },
        None => RuntimeVersionState::UpdateAvailable {
            installed: None,
            available,
        },
    }
}

fn runtime_version_device_pack(
    config: &AppConfig,
    provider: &stt::SttProviderAdapter,
) -> RuntimeDevicePack {
    runtime_device_pack_for_provider(config, provider)
}

fn resolve_managed_runtime_executable(
    config: &AppConfig,
    provider: &stt::SttProviderAdapter,
) -> Option<PathBuf> {
    let root = config::managed_runtime_path(config, provider.backend)?;
    match provider.backend {
        "whisper.cpp" => {
            stt::whisper_cpp::resolve_whisper_cpp_executable_from_candidates([], [root], [])
        }
        "faster-whisper" => {
            stt::faster_whisper::resolve_faster_whisper_executable_from_candidates([], [root], [])
        }
        "Vosk" => stt::vosk::resolve_vosk_executable_from_candidates([], [root], []),
        "sherpa-onnx" | "Moonshine" | "Parakeet" => {
            stt::sherpa_onnx::resolve_executable_from_candidates(
                provider.runtime_id,
                [],
                [root],
                [],
            )
        }
        _ => None,
    }
}

fn packaged_runtime_path(config: &AppConfig, model: &SttModelInfo) -> Option<PathBuf> {
    match model.backend.as_str() {
        "whisper.cpp" => stt::whisper_cpp::resolve_whisper_cpp_packaged_executable(config),
        "faster-whisper" => stt::faster_whisper::resolve_faster_whisper_packaged_executable(config),
        "Vosk" => stt::vosk::resolve_vosk_packaged_executable(config),
        "sherpa-onnx" | "Moonshine" | "Parakeet" => {
            stt::sherpa_onnx::resolve_packaged_executable_for_backend(config, &model.backend)
        }
        _ => None,
    }
}

fn runtime_install_source(
    config: &AppConfig,
    model: &SttModelInfo,
) -> Result<Option<RuntimeInstallSource>, String> {
    let device = runtime_device_pack(config, model);
    let remote = runtime_artifacts::embedded_artifact(
        &runtime_catalog::runtime_id_for_backend(&model.backend),
        device,
    )?;
    Ok(runtime_install_source_from_candidates(
        config,
        model,
        packaged_runtime_path(config, model).filter(|path| packaged_runtime_device(path) == device),
        remote,
        development_runtime_package(config, model),
    ))
}

fn runtime_install_source_from_candidates(
    config: &AppConfig,
    model: &SttModelInfo,
    packaged: Option<PathBuf>,
    remote: Option<RuntimeArtifact>,
    development: Option<DevelopmentRuntimePackage>,
) -> Option<RuntimeInstallSource> {
    packaged
        .filter(|path| runtime_source_is_staged(config, model, path))
        .map(RuntimeInstallSource::Packaged)
        .or_else(|| remote.map(RuntimeInstallSource::Remote))
        .or_else(|| development.map(RuntimeInstallSource::DevelopmentScript))
}

fn runtime_device_pack(config: &AppConfig, model: &SttModelInfo) -> RuntimeDevicePack {
    let Some(provider) = stt::provider_for_backend(&model.backend) else {
        return RuntimeDevicePack::Cpu;
    };
    runtime_device_pack_for_provider(config, provider)
}

fn runtime_device_pack_for_provider(
    config: &AppConfig,
    provider: &stt::SttProviderAdapter,
) -> RuntimeDevicePack {
    let supports_gpu = runtime_catalog::backend_spec_for_runtime_id(provider.runtime_id)
        .is_some_and(|spec| spec.device_support.supports_gpu());
    let maintaining_gpu = config.whisper_compute_mode == WhisperComputeMode::Auto
        && config
            .managed_runtimes
            .get(provider.runtime_id)
            .is_some_and(|install| install.device.as_deref() == Some("gpu"));
    if supports_gpu
        && (config.whisper_compute_mode == WhisperComputeMode::PreferGpu || maintaining_gpu)
    {
        RuntimeDevicePack::Gpu
    } else {
        RuntimeDevicePack::Cpu
    }
}

fn packaged_runtime_device(path: &Path) -> RuntimeDevicePack {
    runtime_manifest_metadata(path)
        .and_then(|metadata| {
            metadata.device.or_else(|| {
                metadata
                    .cuda_bundled
                    .map(|cuda| if cuda { "gpu" } else { "cpu" }.to_owned())
            })
        })
        .filter(|device| device.eq_ignore_ascii_case("gpu"))
        .map(|_| RuntimeDevicePack::Gpu)
        .unwrap_or(RuntimeDevicePack::Cpu)
}

fn runtime_source_is_staged(config: &AppConfig, model: &SttModelInfo, path: &Path) -> bool {
    if path_is_within(path, &config::runtime_storage_dir()) {
        return false;
    }
    let Some(package_root) = runtime_package_root(path) else {
        return false;
    };

    let Some(provider) = stt::provider_for_backend(&model.backend) else {
        return false;
    };
    let Some(current) = config.managed_runtimes.get(provider.runtime_id) else {
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
    let provider = stt::provider_for_backend(&model.backend)?;
    let spec = runtime_catalog::development_runtime_spec(provider.runtime_id)?;
    let script = find_development_bundle_script(spec.script_name)?;
    let destination_root = config::runtime_storage_dir().join(provider.runtime_id);
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

fn reduced_motion_enabled() -> bool {
    let value = env::var("SCRIBE_REDUCED_MOTION").ok();
    reduced_motion_enabled_for(value.as_deref())
}

fn reduced_motion_enabled_for(value: Option<&str>) -> bool {
    value.is_some_and(env_flag_value_enabled)
}

pub struct LocalTranscriberApp {
    config: AppConfig,
    config_path: Option<PathBuf>,
    current_tab: Tab,
    status: TranscriptionStatus,
    transcript: String,
    status_message: String,
    hotkey_input: String,
    model_search: String,
    model_backend_filter: String,
    audio_devices: Vec<String>,
    capturing_hotkey: bool,
    model_downloads: HashMap<String, ModelInstallStatus>,
    runtime_jobs: HashMap<String, RuntimeInstallJob>,
    active_recording: Option<ActiveRecording>,
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
    playground_audio_path: Option<PathBuf>,
    latest_latency: Option<LatencyTrace>,
    hotkey_service: HotkeyService,
    tray_service: Option<TrayService>,
    last_tray_state: Option<TrayUiState>,
    reduced_motion: bool,
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
        if let Err(error) = audio::cleanup_stale_recording_artifacts() {
            eprintln!("could not clean stale Scribe recording artifacts: {error}");
            status_message = format!("{status_message} Recording cleanup warning: {error}");
        }
        if let Err(message) = recover_managed_runtime_transactions(&mut config) {
            status_message = format!("{status_message} Runtime recovery warning: {message}");
        }
        cc.egui_ctx.set_visuals(stitch_visuals(resolve_theme_mode(
            config.theme_mode,
            cc.integration_info.system_theme,
        )));

        let (tx, rx) = unbounded();
        let mut app = Self {
            hotkey_input: config.hotkey.clone(),
            model_search: String::new(),
            model_backend_filter: "All".to_owned(),
            audio_devices: Vec::new(),
            capturing_hotkey: false,
            model_downloads: HashMap::new(),
            runtime_jobs: HashMap::new(),
            playground_cards: cards_from_config(&config),
            playground_selector_draft: None,
            playground_selector_return_focus: None,
            playground_selector_header_focus: None,
            playground_selector_needs_initial_focus: false,
            playground_reference_transcript: String::new(),
            playground_reference_user_edited: false,
            playground_ranking_mode: RankingMode::Balanced,
            hotkey_service: HotkeyService::new(&config.hotkey),
            config,
            config_path,
            current_tab: initial_tab(),
            status: TranscriptionStatus::Idle,
            transcript: String::new(),
            status_message,
            active_recording: None,
            tx,
            rx,
            playground_pending: 0,
            playground_audio_path: None,
            latest_latency: None,
            tray_service: None,
            last_tray_state: None,
            reduced_motion: reduced_motion_enabled(),
            quit_requested: false,
        };

        let initial_tray_state = TrayUiState {
            is_recording: false,
            has_transcript: false,
        };
        match TrayService::new(
            initial_tray_state.is_recording,
            initial_tray_state.has_transcript,
        ) {
            Ok(tray_service) => {
                app.tray_service = Some(tray_service);
                app.last_tray_state = Some(initial_tray_state);
            }
            Err(err) => {
                app.status_message = format!("Tray unavailable: {err}");
            }
        }

        if let Some(err) = &app.hotkey_service.last_error {
            app.status_message = format!("Hotkey unavailable: {err}");
        }

        app
    }

    fn selected_model(&self) -> Option<SttModelInfo> {
        config::selected_model(&self.config)
    }

    fn playground_selected_models(&self) -> Vec<SttModelInfo> {
        config::playground_selected_installed_models(&self.config)
    }

    fn save_config(&mut self) {
        config::normalize_config(&mut self.config);
        #[cfg(test)]
        if self.config_path.is_none() {
            self.status_message = "Settings saved".to_owned();
            self.refresh_playground_cards_from_config();
            return;
        }
        match config::save_config_merging_managed_runtimes(&self.config) {
            Ok(committed) => {
                self.config = committed.config;
                if self.config_path.is_none() {
                    self.config_path = config::config_file_path().ok();
                }
                self.status_message = committed.durability_warning.map_or_else(
                    || "Settings saved".to_owned(),
                    |warning| format!("Settings saved with a durability warning: {warning}"),
                );
            }
            Err(err) => {
                self.status = TranscriptionStatus::Error;
                self.status_message = format!("Failed to save settings: {err}");
            }
        }
        self.refresh_playground_cards_from_config();
    }

    fn refresh_playground_cards_from_config(&mut self) {
        let existing_cards = std::mem::take(&mut self.playground_cards);
        let mut existing_by_id = existing_cards
            .into_iter()
            .map(|card| (card.model.id.clone(), card))
            .collect::<HashMap<_, _>>();

        self.playground_cards = cards_from_config(&self.config)
            .into_iter()
            .map(|mut card| {
                if let Some(mut existing) = existing_by_id.remove(&card.model.id) {
                    existing.model = card.model;
                    existing.status = runtime_status_for_model(&self.config, &existing.model);
                    existing
                } else {
                    card.status = runtime_status_for_model(&self.config, &card.model);
                    card
                }
            })
            .collect();
    }

    fn next_repaint_delay(&self) -> Duration {
        if self.has_active_work() {
            ACTIVE_REPAINT_DELAY
        } else {
            // Hotkey and tray events are integrated from update(), so idle still polls slowly.
            IDLE_REPAINT_DELAY
        }
    }

    fn has_active_work(&self) -> bool {
        self.active_recording.is_some()
            || self.playground_pending > 0
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

    fn apply_playground_action(&mut self, action: PlaygroundAction) {
        match action {
            PlaygroundAction::Clear(model_id) => {
                let clearing_active_model = model_id == self.config.selected_default_model;
                if let Some(card) = self
                    .playground_cards
                    .iter_mut()
                    .find(|card| card.model.id == model_id)
                {
                    card.transcript.clear();
                    card.latency_ms = None;
                    card.audio_duration_ms = None;
                    card.peak_ram_mb = None;
                    card.peak_vram_mb = None;
                }
                if clearing_active_model && !self.playground_reference_user_edited {
                    self.playground_reference_transcript.clear();
                }
            }
            PlaygroundAction::MoveBy { model_id, offset } => {
                let selected_ids = self
                    .playground_cards
                    .iter()
                    .map(|card| card.model.id.clone())
                    .collect::<Vec<_>>();
                if let Some(position) = move_selected_model_by(
                    &mut self.config.playground_model_order,
                    &selected_ids,
                    &model_id,
                    offset,
                ) {
                    let model_name = self
                        .playground_cards
                        .iter()
                        .find(|card| card.model.id == model_id)
                        .map(|card| card.model.name.clone())
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
                        &mut self.config.playground_model_order,
                        &dragged_id,
                        &target_id,
                    );
                    self.save_config();
                    if let Some(position) = self
                        .playground_cards
                        .iter()
                        .position(|card| card.model.id == dragged_id)
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
        if self.active_recording.is_some() {
            return;
        }
        if source == RecordingSource::Playground
            && let Some(message) = self.playground_run_block_reason()
        {
            self.status = TranscriptionStatus::Error;
            self.status_message = message;
            return;
        }
        let mut latency = LatencyTrace::started_now();

        if source == RecordingSource::Transcribe {
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
        }

        if source == RecordingSource::Playground {
            self.reset_playground_for_run();
        }

        match audio::start_recording(
            self.config.max_recording_seconds,
            self.config.audio_input_device_name.clone(),
        ) {
            Ok(session) => {
                latency.recorder_started_at = Some(Instant::now());
                let path = session.audio_path.display().to_string();
                self.active_recording = Some(ActiveRecording {
                    session,
                    source,
                    stop_requested: false,
                    started_at: Instant::now(),
                    max_duration_seconds: self.config.max_recording_seconds,
                    latency,
                });
                self.status = TranscriptionStatus::Listening;
                self.status_message = format!("Listening. Temporary WAV: {path}");
            }
            Err(err) => {
                self.status = TranscriptionStatus::Error;
                self.status_message = format!("Microphone failed: {err}");
            }
        }
    }

    fn stop_recording(&mut self) {
        if let Some(active) = self.active_recording.as_mut()
            && !active.stop_requested
        {
            active.session.stop();
            active.stop_requested = true;
            active.latency.stop_requested_at = Some(Instant::now());
            self.status_message = "Stopping recording".to_owned();
        }
    }

    fn toggle_recording(&mut self) {
        if self.active_recording.is_some() {
            self.stop_recording();
        } else {
            self.start_recording(RecordingSource::Transcribe);
        }
    }

    fn poll_recording(&mut self) {
        let finished = self.active_recording.as_ref().and_then(|active| {
            active
                .session
                .try_finish()
                .map(|result| (active.source, result))
        });

        if let Some((source, result)) = finished {
            let mut active = self
                .active_recording
                .take()
                .expect("finished recording should still be active");
            active.latency.wav_finalized_at = Some(Instant::now());
            match result {
                Ok(audio_path) => {
                    self.status = TranscriptionStatus::Transcribing;
                    self.status_message = format!("Transcribing {}", audio_path.display());
                    match source {
                        RecordingSource::Transcribe => {
                            self.dispatch_default_transcription(audio_path, active.latency)
                        }
                        RecordingSource::Playground => {
                            self.dispatch_playground_transcriptions(audio_path)
                        }
                    }
                }
                Err(message) => {
                    self.status = TranscriptionStatus::Error;
                    self.status_message = format!("Recording failed: {message}");
                }
            }
        }
    }

    fn poll_hotkey(&mut self) {
        for event in self.hotkey_service.poll_events() {
            match hotkey_recording_action(
                self.config.hotkey_mode,
                event,
                self.active_recording.as_ref().map(|active| active.source),
            ) {
                Some(HotkeyRecordingAction::StartTranscribe) => {
                    self.start_recording(RecordingSource::Transcribe)
                }
                Some(HotkeyRecordingAction::Stop) => self.stop_recording(),
                Some(HotkeyRecordingAction::Toggle) => self.toggle_recording(),
                None => {}
            }
        }
    }

    fn poll_tray(&mut self, ctx: &egui::Context) {
        let Some(tray_service) = &self.tray_service else {
            return;
        };
        if let Some(command) = tray_service.poll_command() {
            self.apply_tray_command(command, ctx);
        }
    }

    fn sync_tray_state(&mut self) {
        let Some(tray_service) = &self.tray_service else {
            self.last_tray_state = None;
            return;
        };

        let current = tray_ui_state(self.active_recording.is_some(), &self.transcript);
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
                ctx.send_viewport_cmd(ViewportCommand::Close);
            }
        }
    }

    fn handle_close_request(&mut self, ctx: &egui::Context) {
        let close_requested = ctx.input(|input| input.viewport().close_requested());
        if should_hide_on_close(
            close_requested,
            self.config.close_to_tray,
            self.tray_service.is_some(),
            self.quit_requested,
        ) {
            ctx.send_viewport_cmd(ViewportCommand::CancelClose);
            self.hide_window(ctx);
        }
    }

    fn hide_window(&mut self, ctx: &egui::Context) {
        ctx.send_viewport_cmd(ViewportCommand::Visible(false));
        self.status_message = "Scribe is running in the tray".to_owned();
    }

    fn show_window(&mut self, ctx: &egui::Context) {
        ctx.send_viewport_cmd(ViewportCommand::Visible(true));
        ctx.send_viewport_cmd(ViewportCommand::Focus);
        self.status_message = "Scribe window restored".to_owned();
    }

    fn copy_transcript_to_clipboard(&mut self) {
        if self.transcript.trim().is_empty() {
            self.status_message = "No transcript to copy".to_owned();
            return;
        }

        match Clipboard::new().and_then(|mut clipboard| clipboard.set_text(self.transcript.clone()))
        {
            Ok(()) => self.status_message = "Transcript copied".to_owned(),
            Err(err) => {
                self.status_message = format!("Clipboard failed: {err}");
            }
        }
    }

    fn poll_events(&mut self) {
        while let Ok(event) = self.rx.try_recv() {
            match event {
                AppEvent::TranscriptionDone {
                    source,
                    result,
                    latency,
                } => {
                    let mut latency = latency.map(|mut latency| {
                        latency.ui_result_at = Some(Instant::now());
                        latency
                    });
                    match source {
                        RecordingSource::Transcribe => {
                            let segment_count = result.segments.len();
                            let timed_segments = result
                                .segments
                                .iter()
                                .filter(|segment| {
                                    segment.start_ms.is_some() || segment.end_ms.is_some()
                                })
                                .count();
                            let segment_text_bytes = result
                                .segments
                                .iter()
                                .map(|segment| segment.text.len())
                                .sum::<usize>();
                            let stdout_bytes = result.stdout.len();
                            let stderr_bytes = result.stderr.len();
                            self.transcript = result.text.clone();
                            self.status = TranscriptionStatus::Idle;
                            let completion_message = format!(
                                "{} via {} finished in {} ms ({} segment(s), {} timed, {} text bytes, {} stdout bytes, {} stderr bytes)",
                                result.model_name,
                                result.backend,
                                result.duration_ms.unwrap_or_default(),
                                segment_count,
                                timed_segments,
                                segment_text_bytes,
                                stdout_bytes,
                                stderr_bytes
                            );
                            if self.config.auto_insert_transcript {
                                let output_result = text_output::write_to_focused_app(
                                    &self.transcript,
                                    &self.config,
                                );
                                if let Some(latency) = latency.as_mut() {
                                    latency.paste_completed_at = Some(Instant::now());
                                }
                                self.status_message = format!(
                                    "{completion_message}. {}",
                                    output_result.status_message()
                                );
                            } else {
                                self.status_message = completion_message;
                            }
                            self.latest_latency = latency;
                        }
                        RecordingSource::Playground => {
                            self.apply_playground_result(result);
                        }
                    }
                    self.cleanup_after_job(source);
                }
                AppEvent::TranscriptionFailed {
                    source,
                    model_id,
                    message,
                    latency,
                } => {
                    if let Some(mut latency) = latency {
                        latency.ui_result_at = Some(Instant::now());
                        self.latest_latency = Some(latency);
                    }
                    match source {
                        RecordingSource::Transcribe => {
                            self.status = TranscriptionStatus::Error;
                            self.status_message = message;
                        }
                        RecordingSource::Playground => {
                            if let Some(card) = self
                                .playground_cards
                                .iter_mut()
                                .find(|card| card.model.id == model_id)
                            {
                                card.status = ModelRuntimeStatus::Error(message.clone());
                                card.transcript.clear();
                                card.latency_ms = None;
                            }
                            self.status_message = message;
                        }
                    }
                    self.cleanup_after_job(source);
                }
                AppEvent::ModelDownloadProgress {
                    model_id,
                    downloaded_bytes,
                    total_bytes,
                    bytes_per_second,
                } => {
                    self.model_downloads.insert(
                        model_id,
                        ModelInstallStatus::Downloading {
                            downloaded_bytes,
                            total_bytes,
                            bytes_per_second,
                        },
                    );
                }
                AppEvent::ModelDownloadDone { model_id, path } => {
                    self.model_downloads
                        .insert(model_id.clone(), ModelInstallStatus::Installed);
                    if let Some(model) = config::configured_models(&self.config)
                        .into_iter()
                        .find(|model| model.id == model_id)
                    {
                        let active_model_is_runnable =
                            self.selected_model().is_some_and(|active| {
                                runtime_status_for_model(&self.config, &active)
                                    == ModelRuntimeStatus::Ready
                            });
                        self.config.managed_models.insert(
                            model_id.clone(),
                            config::ManagedModelInstall::app_managed(path, "managed-download"),
                        );
                        set_model_selected(&mut self.config, &model_id, true);
                        if should_activate_installed_model(active_model_is_runnable) {
                            self.config.selected_default_model = model_id.clone();
                            self.config.last_used_backend = model.backend;
                        }
                    }
                    self.save_config();
                    self.status = TranscriptionStatus::Idle;
                    self.status_message = match config::configured_models(&self.config)
                        .into_iter()
                        .find(|model| model.id == model_id)
                        .map(|model| runtime_status_for_model(&self.config, &model))
                    {
                        Some(ModelRuntimeStatus::Ready) => "Model installed and ready.".to_owned(),
                        _ => {
                            "Model installed. Install its managed runtime from Models before transcribing."
                                .to_owned()
                        }
                    };
                }
                AppEvent::ModelDownloadFailed { model_id, message } => {
                    self.model_downloads
                        .insert(model_id, ModelInstallStatus::Error(message.clone()));
                    self.status = TranscriptionStatus::Error;
                    self.status_message = format!("Download failed: {message}");
                }
                AppEvent::RuntimeInstallDone {
                    runtime_id,
                    backend,
                    mut replacement,
                    install,
                } => {
                    let new_runtime = install;
                    let job = self.runtime_jobs.remove(&runtime_id).unwrap_or_default();
                    if let Err(message) = replacement.prepare_persistence(Some(&new_runtime)) {
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
                    let (job, config_durability_warning) = match persist_runtime_install(
                        &mut self.config,
                        &runtime_id,
                        new_runtime.clone(),
                        job,
                        |candidate| {
                            config::save_config_with_runtime_update(
                                candidate,
                                Some((&runtime_id, Some(new_runtime.clone()))),
                            )
                            .map_err(|err| err.to_string())
                        },
                    ) {
                        RuntimePersistenceTransition::Persisted {
                            job,
                            durability_warning,
                        } => (job, durability_warning),
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
                    let config_durability_pending = config_durability_warning.is_some();
                    let cleanup_warning =
                        finalize_runtime_transaction(*replacement, config_durability_warning);
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
                    self.status_message = if config_durability_pending {
                        cleanup_warning.map(|warning| {
                            format!(
                                "{backend} runtime is ready, but configuration durability is unconfirmed; startup recovery will verify it: {warning}"
                            )
                        })
                    } else {
                        cleanup_warning.map(|warning| {
                            format!(
                                "{backend} runtime is ready. Old runtime backup cleanup warning: {warning}"
                            )
                        })
                    }
                    .unwrap_or_else(
                        || format!("{backend} runtime is ready."),
                    );
                }
                AppEvent::RuntimeInstallFailed {
                    runtime_id,
                    message,
                } => self.fail_runtime_job(&runtime_id, message),
            }
        }
    }

    fn cleanup_after_job(&mut self, source: RecordingSource) {
        match source {
            RecordingSource::Transcribe => {
                self.status = if self.status == TranscriptionStatus::Error {
                    TranscriptionStatus::Error
                } else {
                    TranscriptionStatus::Idle
                };
            }
            RecordingSource::Playground => {
                self.playground_pending = self.playground_pending.saturating_sub(1);
                if self.playground_pending == 0 {
                    self.status = TranscriptionStatus::Idle;
                    self.status_message = "Model playground finished".to_owned();
                    if let Some(path) = self.playground_audio_path.take() {
                        let _ = fs::remove_file(path);
                    }
                }
            }
        }
    }

    fn dispatch_default_transcription(&mut self, audio_path: PathBuf, mut latency: LatencyTrace) {
        let Some(model) = self.selected_model() else {
            self.status = TranscriptionStatus::Error;
            self.status_message = "No default model selected".to_owned();
            let _ = fs::remove_file(audio_path);
            return;
        };

        latency.transcription_dispatched_at = Some(Instant::now());
        let config = self.config.clone();
        let tx = self.tx.clone();

        thread::spawn(move || {
            let result = stt::transcribe_with_config(&config, audio_path.clone(), model.clone());
            latency.transcription_completed_at = Some(Instant::now());
            let _ = fs::remove_file(&audio_path);

            match result {
                Ok(result) => {
                    let _ = tx.send(AppEvent::TranscriptionDone {
                        source: RecordingSource::Transcribe,
                        result,
                        latency: Some(latency),
                    });
                }
                Err(err) => {
                    let _ = tx.send(AppEvent::TranscriptionFailed {
                        source: RecordingSource::Transcribe,
                        model_id: model.id,
                        message: err.to_string(),
                        latency: Some(latency),
                    });
                }
            }
        });
    }

    fn dispatch_playground_transcriptions(&mut self, audio_path: PathBuf) {
        let models = self.playground_selected_models();
        if let Some(message) = self.playground_run_block_reason() {
            self.status = TranscriptionStatus::Error;
            self.status_message = message;
            let _ = fs::remove_file(audio_path);
            return;
        }

        self.playground_audio_path = Some(audio_path.clone());
        self.playground_pending = models.len();
        let audio_duration_ms = audio::wav_duration_ms(&audio_path);
        let config = self.config.clone();

        for model in models {
            if let Some(card) = self
                .playground_cards
                .iter_mut()
                .find(|card| card.model.id == model.id)
            {
                card.status = ModelRuntimeStatus::Running;
                card.transcript.clear();
                card.latency_ms = None;
                card.audio_duration_ms = audio_duration_ms;
                card.peak_ram_mb = None;
                card.peak_vram_mb = None;
            }

            let tx = self.tx.clone();
            let config = config.clone();
            let audio_path = audio_path.clone();
            thread::spawn(move || {
                match stt::transcribe_with_config(&config, audio_path, model.clone()) {
                    Ok(result) => {
                        let _ = tx.send(AppEvent::TranscriptionDone {
                            source: RecordingSource::Playground,
                            result,
                            latency: None,
                        });
                    }
                    Err(err) => {
                        let _ = tx.send(AppEvent::TranscriptionFailed {
                            source: RecordingSource::Playground,
                            model_id: model.id,
                            message: err.to_string(),
                            latency: None,
                        });
                    }
                }
            });
        }
    }

    fn reset_playground_for_run(&mut self) {
        self.playground_cards = cards_from_config(&self.config).into_iter().collect();
        for card in &mut self.playground_cards {
            card.status = runtime_status_for_model(&self.config, &card.model);
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
        self.playground_audio_path = None;
    }

    fn apply_playground_result(&mut self, result: TranscriptResult) {
        let is_active_model = result.model_id == self.config.selected_default_model;
        let transcript = result.text;
        if let Some(card) = self
            .playground_cards
            .iter_mut()
            .find(|card| card.model.id == result.model_id)
        {
            card.status = ModelRuntimeStatus::Ready;
            card.transcript = transcript.clone();
            card.latency_ms = result.duration_ms;
        }
        if is_active_model && !self.playground_reference_user_edited {
            self.playground_reference_transcript = transcript;
        }
    }

    fn clear_playground_results(&mut self, clear_reference: bool) {
        for card in &mut self.playground_cards {
            card.transcript.clear();
            card.latency_ms = None;
            card.audio_duration_ms = None;
            card.peak_ram_mb = None;
            card.peak_vram_mb = None;
            card.status = runtime_status_for_model(&self.config, &card.model);
        }
        if clear_reference {
            self.playground_reference_transcript.clear();
            self.playground_reference_user_edited = false;
        }
    }

    fn playground_selector_busy(&self) -> bool {
        self.active_recording.is_some() || self.playground_pending > 0
    }

    fn open_playground_selector(&mut self, opener_id: Option<egui::Id>) {
        if !self.playground_selector_busy() {
            self.playground_selector_draft = Some(self.config.playground_selected_models.clone());
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
        if self.playground_cards.is_empty() {
            return Some(if self.config.playground_selected_models.is_empty() {
                "Choose models to test before starting a test recording.".to_owned()
            } else {
                "Install the selected Playground models before starting a test recording."
                    .to_owned()
            });
        }
        self.playground_cards
            .iter()
            .find(|card| card.status != ModelRuntimeStatus::Ready)
            .map(|card| {
                format!(
                    "{} is not ready. Repair or install its runtime from Models before running the Playground.",
                    card.model.name
                )
            })
    }

    fn active_playground_output(&self) -> Option<(String, String)> {
        self.playground_cards
            .iter()
            .find(|card| {
                card.model.id == self.config.selected_default_model
                    && !card.transcript.trim().is_empty()
            })
            .map(|card| (card.model.name.clone(), card.transcript.clone()))
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
                model_id: card.model.id.clone(),
                model_name: card.model.name.clone(),
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
                self.config.hotkey = self.hotkey_input.clone();
                self.save_config();
                self.status_message = format!("Registered hotkey {}", self.config.hotkey);
            }
            Err(err) => {
                self.status = TranscriptionStatus::Error;
                self.status_message = format!("Failed to register hotkey: {err}");
            }
        }
    }

    fn apply_theme(&self, ctx: &egui::Context, frame: &eframe::Frame) {
        ctx.set_visuals(stitch_visuals(resolve_theme_mode(
            self.config.theme_mode,
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

    fn select_model_as_default(&mut self, model: &SttModelInfo) {
        self.config.selected_default_model = model.id.clone();
        self.config.last_used_backend = model.backend.clone();
        self.save_config();
    }

    fn effective_install_status(&self, model: &SttModelInfo) -> ModelInstallStatus {
        self.model_downloads
            .get(&model.id)
            .cloned()
            .unwrap_or_else(|| model.install_status.clone())
    }

    fn runtime_consumer_activity(&self, runtime_id: &str) -> RuntimeConsumerActivity {
        RuntimeConsumerActivity {
            recording: self.active_recording.is_some(),
            transcribing: self.status == TranscriptionStatus::Transcribing,
            playground_jobs: self.playground_pending > 0,
            model_download: model_download_uses_runtime(
                &self.config,
                &self.model_downloads,
                runtime_id,
            ),
        }
    }

    fn start_model_download(&mut self, model: &SttModelInfo) {
        let Some(provider) = stt::provider_for_backend(&model.backend) else {
            self.fail_model_install(
                &model.id,
                format!("{} is not a supported STT backend.", model.backend),
            );
            return;
        };

        if !runtime_needs_preparation(&provider.runtime_status(&self.config)) {
            self.start_model_download_only(model);
            return;
        }

        self.request_runtime_install(model, RuntimeJobIntent::DownloadModel(model.id.clone()));
    }

    fn start_model_download_only(&mut self, model: &SttModelInfo) {
        let Some(download_model) = model.download_model.clone() else {
            self.status = TranscriptionStatus::Error;
            self.status_message = format!("{} does not have a supported download.", model.name);
            return;
        };

        if !supports_managed_install(model) {
            self.status = TranscriptionStatus::Error;
            self.status_message = format!(
                "Managed installer for {} is not available in this build.",
                model.name
            );
            return;
        }

        let Some(destination) = config::downloaded_model_path(&self.config, model) else {
            self.status = TranscriptionStatus::Error;
            self.status_message = "No model storage directory is configured.".to_owned();
            return;
        };

        let expected_total_bytes = model_download_total_bytes(model);
        self.model_downloads.insert(
            model.id.clone(),
            ModelInstallStatus::Downloading {
                downloaded_bytes: 0,
                total_bytes: expected_total_bytes,
                bytes_per_second: None,
            },
        );
        self.status = TranscriptionStatus::Idle;
        self.status_message = format!("Downloading {}...", model.name);

        let tx = self.tx.clone();
        let model_id = model.id.clone();
        match model.backend.as_str() {
            "faster-whisper" => {
                let Some(runtime) =
                    stt::faster_whisper::resolve_faster_whisper_executable(&self.config)
                else {
                    self.status = TranscriptionStatus::Error;
                    self.status_message =
                        "Install the faster-whisper runtime before downloading this model."
                            .to_owned();
                    self.model_downloads.remove(&model.id);
                    return;
                };
                thread::spawn(move || {
                    let progress = |progress| send_model_download_progress(&tx, progress);
                    let result = managed_downloads::download_faster_whisper_model(
                        &runtime,
                        &download_model,
                        &destination,
                        &model_id,
                        expected_total_bytes,
                        &progress,
                    );
                    send_model_download_result(&tx, model_id, result);
                });
            }
            "Vosk" => {
                let Some(runtime) = stt::vosk::resolve_vosk_executable(&self.config) else {
                    self.status = TranscriptionStatus::Error;
                    self.status_message =
                        "Install the Vosk runtime before downloading this model.".to_owned();
                    self.model_downloads.remove(&model.id);
                    return;
                };
                thread::spawn(move || {
                    let progress = |progress| send_model_download_progress(&tx, progress);
                    let result = managed_downloads::download_vosk_model(
                        &runtime,
                        &download_model,
                        &destination,
                        &model_id,
                        expected_total_bytes,
                        &progress,
                    );
                    send_model_download_result(&tx, model_id, result);
                });
            }
            "sherpa-onnx" | "Moonshine" | "Parakeet" => {
                let Some(runtime) =
                    stt::sherpa_onnx::resolve_executable_for_backend(&self.config, &model.backend)
                else {
                    self.status = TranscriptionStatus::Error;
                    self.status_message = format!(
                        "Install the {} runtime before downloading this model.",
                        model.backend
                    );
                    self.model_downloads.remove(&model.id);
                    return;
                };
                let model_for_download = model.clone();
                thread::spawn(move || {
                    let progress = |progress| send_model_download_progress(&tx, progress);
                    let result = managed_downloads::download_sherpa_model(
                        &runtime,
                        &model_for_download,
                        &download_model,
                        &destination,
                        &model_id,
                        expected_total_bytes,
                        &progress,
                    );
                    send_model_download_result(&tx, model_id, result);
                });
            }
            "whisper.cpp" => {
                thread::spawn(move || {
                    let progress = |progress| send_model_download_progress(&tx, progress);
                    let result = managed_downloads::download_whisper_cpp_model(
                        &download_model,
                        &destination,
                        &model_id,
                        expected_total_bytes,
                        &progress,
                    );
                    send_model_download_result(&tx, model_id, result);
                });
            }
            backend => {
                self.status = TranscriptionStatus::Error;
                self.status_message = format!("Managed downloader for {backend} is not available.");
                self.model_downloads.remove(&model.id);
            }
        }
    }

    fn uninstall_model(&mut self, model: &SttModelInfo) {
        let removal = uninstall_model_files(&self.config, model);
        self.model_downloads.remove(&model.id);
        self.config.managed_models.remove(&model.id);
        self.config.model_paths.remove(&model.id);
        set_model_selected(&mut self.config, &model.id, false);

        if self.config.selected_default_model == model.id {
            select_first_installed_model(&mut self.config);
        }

        self.save_config();
        self.status = TranscriptionStatus::Idle;
        self.status_message = match removal {
            Ok(true) => format!("Uninstalled {}.", model.name),
            Ok(false) => format!("Removed {} from Scribe.", model.name),
            Err(message) => format!("Removed {} from Scribe. {message}", model.name),
        };
    }

    fn request_runtime_install(&mut self, model: &SttModelInfo, intent: RuntimeJobIntent) {
        let Some(provider) = stt::provider_for_backend(&model.backend) else {
            self.status = TranscriptionStatus::Error;
            self.status_message = format!("{} is not a supported STT backend.", model.backend);
            return;
        };

        if !provider.runtime_install_supported {
            self.status = TranscriptionStatus::Error;
            self.status_message = format!(
                "Managed runtime installer for {} is not available in this build.",
                model.backend
            );
            return;
        }

        let source = match runtime_install_source(&self.config, model) {
            Ok(Some(source)) => source,
            Ok(None) => {
                let message = missing_runtime_source_message(
                    &model.backend,
                    runtime_device_pack(&self.config, model),
                );
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
            }
            Err(message) => {
                self.status = TranscriptionStatus::Error;
                self.status_message = format!("Trusted runtime catalog unavailable: {message}");
                return;
            }
        };

        if let Some(job) = self.runtime_jobs.get_mut(provider.runtime_id) {
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
        self.runtime_jobs
            .insert(provider.runtime_id.to_owned(), job);
        self.status = TranscriptionStatus::Idle;
        self.status_message = format!("Preparing {} runtime...", model.backend);

        let tx = self.tx.clone();
        let runtime_id = provider.runtime_id.to_owned();
        let backend = model.backend.clone();
        thread::spawn(move || {
            let (result, source, artifact) = match source {
                RuntimeInstallSource::Packaged(packaged_path) => (
                    install_runtime_files(&runtime_id, &packaged_path),
                    "packaged-runtime",
                    None,
                ),
                RuntimeInstallSource::Remote(artifact) => (
                    install_remote_runtime_artifact(&runtime_id, &artifact),
                    "trusted-release-artifact",
                    Some(artifact),
                ),
                RuntimeInstallSource::DevelopmentScript(package) => (
                    build_development_runtime_package(&runtime_id, &backend, package),
                    "development-script",
                    None,
                ),
            };
            match result {
                Ok(replacement) => {
                    let install = managed_runtime_install_record(
                        replacement.installed_path.clone(),
                        source,
                        artifact.as_ref(),
                    );
                    let _ = tx.send(AppEvent::RuntimeInstallDone {
                        runtime_id,
                        backend,
                        replacement: Box::new(replacement),
                        install,
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
        self.model_downloads.insert(
            model_id.to_owned(),
            ModelInstallStatus::Error(message.clone()),
        );
        self.status = TranscriptionStatus::Error;
        self.status_message = message;
    }

    fn fail_runtime_job(&mut self, runtime_id: &str, message: String) {
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
        let Some(provider) = stt::provider_for_backend(&model.backend) else {
            self.status = TranscriptionStatus::Error;
            self.status_message = format!("{} is not a supported STT backend.", model.backend);
            return;
        };

        let outcome = match uninstall_runtime_transaction(&mut self.config, provider.runtime_id) {
            Ok(outcome) => outcome,
            Err(message) => {
                self.status = TranscriptionStatus::Error;
                self.status_message =
                    format!("Could not uninstall {} runtime. {message}", model.backend);
                return;
            }
        };
        if self.config_path.is_none() {
            self.config_path = config::config_file_path().ok();
        }
        self.refresh_playground_runtime_statuses();
        self.status = TranscriptionStatus::Idle;
        let success = match outcome.removed_files {
            true => format!("Uninstalled {} runtime.", model.backend),
            false => format!("Removed {} runtime from Scribe.", model.backend),
        };
        self.status_message = outcome
            .durability_warning
            .map_or(success.clone(), |warning| {
                format!("{success} Durability warning; startup recovery will verify it: {warning}")
            });
    }

    fn refresh_playground_runtime_statuses(&mut self) {
        for card in &mut self.playground_cards {
            card.status = runtime_status_for_model(&self.config, &card.model);
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
        self.poll_recording();
        self.poll_events();
        self.sync_tray_state();

        navigation_rail(ctx, &mut self.current_tab);

        egui::CentralPanel::default()
            .frame(content_panel_frame(ctx))
            .show(ctx, |ui| match self.current_tab {
                Tab::Transcribe => self.ui_transcribe(ui),
                Tab::Models => self.ui_models(ui),
                Tab::Playground => self.ui_playground(ui),
                Tab::Settings => self.ui_settings(ui),
            });

        ctx.request_repaint_after(self.next_repaint_delay());
    }
}

impl LocalTranscriberApp {
    fn ui_transcribe(&mut self, ui: &mut Ui) {
        let status = self.status;
        let status_message = self.status_message.clone();
        page(ui, "Transcribe", status, &status_message, |ui| {
            let selected_model = self.selected_model();
            let runtime_status = selected_model
                .as_ref()
                .map(|model| runtime_status_for_model(&self.config, model));
            let ready = runtime_status == Some(ModelRuntimeStatus::Ready);
            let selected_model_summary = selected_model.as_ref().map(|model| {
                (
                    model.name.clone(),
                    model.backend.clone(),
                    self.effective_install_status(model),
                )
            });
            let hotkey = self.config.hotkey.clone();
            let mut requested_tab = None;

            panel(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.vertical(|ui| {
                        ui.label(label_caps("Current Model"));
                        if let Some((name, backend, install_status)) = &selected_model_summary {
                            ui.label(body_strong(name));
                            ui.horizontal_wrapped(|ui| {
                                badge(ui, backend, ChipTone::Neutral);
                                badge(
                                    ui,
                                    &install_status.label(),
                                    install_chip_tone(install_status),
                                );
                            });
                        } else {
                            ui.label(body_strong("No model selected"));
                        }
                    });
                    ui.add_space(12.0);
                    if ui.add(small_button(ui, "Change")).clicked() {
                        requested_tab = Some(Tab::Models);
                    }
                    ui.separator();
                    ui.vertical(|ui| {
                        ui.label(label_caps("Hotkey"));
                        ui.label(body_strong(&hotkey));
                    });
                    if ui.add(small_button(ui, "Edit")).clicked() {
                        requested_tab = Some(Tab::Settings);
                    }
                });
            });
            if let Some(tab) = requested_tab {
                self.current_tab = tab;
            }

            if !ready && self.active_recording.is_none() {
                ui.add_space(12.0);
                panel(ui, |ui| {
                    let setup_message = runtime_status
                        .as_ref()
                        .map(setup_message_for_status)
                        .unwrap_or_else(|| {
                            "Choose a local whisper.cpp model to start transcribing.".to_owned()
                        });
                    ui.label(section_heading("Setup required"));
                    wrapped_label(ui, mut_text(setup_message));
                    ui.add_space(10.0);
                    ui.horizontal_wrapped(|ui| {
                        for model_id in ["whisper_cpp_base_en", "whisper_cpp_tiny_en"] {
                            if let Some(model) = config::configured_models(&self.config)
                                .into_iter()
                                .find(|model| model.id == model_id)
                            {
                                let label = if model_id == "whisper_cpp_base_en" {
                                    "Download base.en"
                                } else {
                                    "Download tiny.en"
                                };
                                if ui.add(small_button(ui, label)).clicked() {
                                    self.select_model_as_default(&model);
                                    self.start_model_download(&model);
                                }
                            }
                        }
                        if ui.add(small_button(ui, "Manage models")).clicked() {
                            self.current_tab = Tab::Models;
                        }
                    });
                });
            }

            ui.add_space(12.0);
            recessed_panel(ui, 132.0, |ui| {
                ui.centered_and_justified(|ui| {
                    ui.vertical_centered(|ui| {
                        let listening = self.active_recording.is_some();
                        let button_text = if listening {
                            "Stop Listening"
                        } else {
                            "Start Listening"
                        };
                        let disabled_tooltip = if listening || ready {
                            None
                        } else {
                            Some(
                                runtime_status
                                    .as_ref()
                                    .map(setup_message_for_status)
                                    .unwrap_or_else(|| {
                                        "Choose or install a local model before transcribing."
                                            .to_owned()
                                    }),
                            )
                        };
                        let response = add_enabled_button(
                            ui,
                            listening || ready,
                            record_button(ui, listening),
                            disabled_tooltip.as_deref(),
                        );
                        paint_record_motion(ui, &response, listening, self.reduced_motion);
                        response.widget_info(|| {
                            egui::WidgetInfo::labeled(egui::WidgetType::Button, button_text)
                        });
                        response.clone().on_hover_text(button_text);
                        if response.clicked() {
                            self.toggle_recording();
                        }
                        ui.add_space(8.0);
                        if let Some(active) = &self.active_recording {
                            let elapsed = active.started_at.elapsed().as_secs_f32();
                            let total = active.max_duration_seconds.max(1) as f32;
                            ui.add(
                                egui::ProgressBar::new((elapsed / total).clamp(0.0, 1.0))
                                    .desired_width(220.0)
                                    .text(recording_timer_text(active)),
                            );
                        } else if ready {
                            ui.label(
                                RichText::new("Ready to listen - system audio & microphone active")
                                    .small()
                                    .weak(),
                            );
                        }
                    });
                });
            });

            ui.add_space(12.0);
            transcript_panel(ui, |ui| {
                let label_id = ui
                    .horizontal(|ui| {
                        let label = ui.label(section_heading("Transcript"));
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            ready_dot(ui, self.status);
                        });
                        label.id
                    })
                    .inner;
                ui.add_space(10.0);
                let response = ui.add(
                    TextEdit::multiline(&mut self.transcript)
                        .desired_rows(14)
                        .desired_width(usable_width(ui))
                        .hint_text("Your transcription appears here..."),
                );
                set_control_accessibility(ui, &response, label_id, "Transcript");
                ui.add_space(10.0);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.add(small_button(ui, "Clear")).clicked() {
                        self.transcript.clear();
                    }
                    if ui.add(small_button(ui, "Copy")).clicked() {
                        self.copy_transcript_to_clipboard();
                    }
                });
            });
        });
    }

    fn ui_models(&mut self, ui: &mut Ui) {
        let backends = stt::provider_adapters()
            .iter()
            .map(|provider| provider.backend.to_owned())
            .collect::<Vec<_>>();

        let status = self.status;
        let status_message = self.status_message.clone();
        page(ui, "Models Catalog", status, &status_message, |ui| {
            panel(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    model_search_filter_control(ui, &mut self.model_search);
                    ui.add_space(10.0);
                    model_backend_filter_control(
                        ui,
                        "model-backend-filter",
                        &mut self.model_backend_filter,
                        &backends,
                    );
                });
            });

            ui.add_space(12.0);
            let download_rows = current_download_rows(&self.config, &self.model_downloads);
            if !download_rows.is_empty() {
                panel(ui, |ui| {
                    ui.label(section_heading("Downloads"));
                    ui.add_space(8.0);
                    for (index, (model, install_status)) in download_rows.iter().enumerate() {
                        download_summary_row(ui, model, install_status);
                        if index + 1 < download_rows.len() {
                            ui.add_space(8.0);
                        }
                    }
                });
                ui.add_space(12.0);
            }

            let mut runtime_action = None;
            let runtime_maintenance = egui::CollapsingHeader::new("Runtime maintenance")
                .default_open(false)
                .show(ui, |ui| panel(ui, |ui| {
                wrapped_label(
                    ui,
                    mut_text(
                    "Install, update, or remove backend runtimes. Models prepare a missing runtime automatically.",
                    ),
                );
                wrapped_label(
                    ui,
                    mut_text(format!(
                        "Storage: models in {} | runtimes in {}",
                        config::model_storage_dir(&self.config).display(),
                        config::runtime_storage_dir().display()
                    )),
                );
                ui.add_space(8.0);
                for model in runtime_representative_models(&self.config) {
                    let provider = stt::provider_for_backend(&model.backend);
                    let runtime_busy = provider.is_some_and(|provider| {
                        self.runtime_jobs.contains_key(provider.runtime_id)
                    });
                    let consumer_activity = provider
                        .map(|provider| self.runtime_consumer_activity(provider.runtime_id))
                        .unwrap_or_default();
                    let runtime_status = provider
                        .map(|provider| provider.runtime_status(&self.config))
                        .unwrap_or_else(|| {
                            ModelRuntimeStatus::Error(format!(
                                "unsupported STT backend: {}",
                                model.backend
                            ))
                        });
                    let action_state = runtime_action_state_with_activity(
                        &self.config,
                        &model,
                        runtime_busy,
                        consumer_activity,
                    );
                    ui.vertical(|ui| {
                        ui.label(body_strong(&format!("{} runtime", model.backend)));
                        wrapped_label(
                            ui,
                            mut_text(runtime_detail_text(
                                &self.config,
                                &model,
                                &runtime_status,
                            )),
                        );
                        ui.add_space(6.0);
                        tag_row(ui, |ui| {
                            badge(
                                ui,
                                &runtime_status.to_string(),
                                runtime_chip_tone(&runtime_status),
                            );
                            if let Some((label, tone)) = runtime_version_badge(&self.config, &model)
                            {
                                badge(ui, &label, tone);
                            }
                            badge(
                                ui,
                                &format!("Runtime {}", runtime_storage_estimate(&model.backend)),
                                ChipTone::Neutral,
                            );
                        });
                        ui.add_space(6.0);
                        ui.horizontal_wrapped(|ui| {
                            let label = runtime_action_label(
                                action_state.kind,
                                &model.backend,
                                runtime_busy,
                            );
                            let runtime_button = small_button(ui, &label);
                            let response = add_enabled_button(
                                ui,
                                action_state.enabled,
                                runtime_button,
                                action_state.disabled_tooltip.as_deref(),
                            )
                            .on_hover_text(format!(
                                "Manage the shared {} backend runtime used by {} models.",
                                model.backend, model.backend
                            ));
                            if response.clicked() {
                                runtime_action = Some((model.clone(), action_state.kind));
                            }
                        });
                    });
                    ui.add_space(6.0);
                }
            }));
            set_collapsing_header_accessibility(ui.ctx(), &runtime_maintenance);

            if let Some((model, kind)) = runtime_action {
                match kind {
                    RuntimeActionKind::Install | RuntimeActionKind::Update => {
                        self.request_runtime_install(&model, RuntimeJobIntent::Maintenance)
                    }
                    RuntimeActionKind::Uninstall => self.uninstall_runtime(&model),
                }
            }

            ui.add_space(12.0);
            let search = self.model_search.trim().to_ascii_lowercase();
            let models = config::configured_models(&self.config)
                .into_iter()
                .filter(|model| {
                    (self.model_backend_filter == "All"
                        || self.model_backend_filter == model.backend)
                        && (search.is_empty()
                            || model.name.to_ascii_lowercase().contains(&search)
                            || model.backend.to_ascii_lowercase().contains(&search))
                })
                .collect::<Vec<_>>();

            for model in models {
                let selected = self.config.selected_default_model == model.id;
                let install_status = self.effective_install_status(&model);
                let runtime_ready =
                    stt::provider_for_backend(&model.backend).is_some_and(|provider| {
                        provider.runtime_status(&self.config) == ModelRuntimeStatus::Ready
                    });
                let action_state = model_action_state_with_runtime(
                    &model,
                    &install_status,
                    selected,
                    runtime_ready,
                );
                let primary_disabled_tooltip = model_primary_disabled_tooltip(
                    &model,
                    &install_status,
                    selected,
                    &action_state,
                );
                let mut select_default = false;
                let mut start_install = false;
                let mut uninstall = false;

                model_catalog_row(ui, &model, &install_status, selected, |ui| {
                    let primary_label =
                        model_primary_action_label(action_state.primary, &model, &install_status);
                    let primary_button = match action_state.primary {
                        ModelPrimaryAction::Select | ModelPrimaryAction::Active => {
                            primary_small_button(ui, &primary_label)
                        }
                        _ => small_button(ui, &primary_label),
                    };
                    let primary_response = add_enabled_button(
                        ui,
                        action_state.primary_enabled,
                        primary_button,
                        primary_disabled_tooltip.as_deref(),
                    )
                    .on_hover_text(match action_state.primary {
                        ModelPrimaryAction::Repair => format!(
                            "Prepare the shared {} runtime for {} without downloading the model again.",
                            model.backend, model.name
                        ),
                        _ => format!("{} for {}.", primary_label, model.name),
                    });
                    if primary_response.clicked() {
                        match action_state.primary {
                            ModelPrimaryAction::Select => select_default = true,
                            ModelPrimaryAction::Install | ModelPrimaryAction::Retry => {
                                start_install = true;
                            }
                            ModelPrimaryAction::Repair => {
                                self.request_runtime_install(
                                    &model,
                                    RuntimeJobIntent::RepairModel(model.id.clone()),
                                );
                            }
                            ModelPrimaryAction::Installing | ModelPrimaryAction::Active => {}
                        }
                    }
                    if action_state.show_uninstall
                        && ui.add(small_button(ui, "Uninstall")).clicked()
                    {
                        uninstall = true;
                    }
                });

                if select_default {
                    self.select_model_as_default(&model);
                }
                if start_install {
                    self.start_model_download(&model);
                }
                if uninstall {
                    self.uninstall_model(&model);
                }
                ui.add_space(8.0);
            }
        });
    }

    fn ui_playground(&mut self, ui: &mut Ui) {
        let status = self.status;
        let status_message = self.status_message.clone();
        page(ui, "Model Playground", status, &status_message, |ui| {
            panel(ui, |ui| {
                let run_blocked = self.playground_run_block_reason();
                let selector_busy = self.playground_selector_busy();
                ui.horizontal_wrapped(|ui| {
                    let text = if self.active_recording.is_some() {
                        "Stop Recording"
                    } else {
                        "Start Test Recording"
                    };
                    let recording_button = add_enabled_button(
                        ui,
                        self.active_recording.is_some() || run_blocked.is_none(),
                        primary_small_button(ui, text),
                        run_blocked.as_deref(),
                    )
                    .on_hover_text("Record one audio sample and run every selected ready model.");
                    if recording_button.clicked() {
                        if self.active_recording.is_some() {
                            self.stop_recording();
                        } else {
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
                            &format!("{} selected", self.config.playground_selected_models.len()),
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
                    wrapped_label(ui, mut_text(&reason));
                }
                if selector_busy {
                    ui.add_space(4.0);
                    wrapped_label(
                        ui,
                        mut_text(
                            "Finish the current Playground recording or transcription before changing models.",
                        ),
                    );
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
                    &self.config.selected_default_model,
                );
            }

            ui.add_space(12.0);
            ui.horizontal_wrapped(|ui| {
                ui.vertical(|ui| {
                    ui.label(section_heading("Selected Models"));
                    wrapped_label(
                        ui,
                        mut_text("Performance comparison based on current system hardware."),
                    );
                });
            });
            ui.add_space(8.0);
            let mut pending_actions = Vec::new();
            if self.playground_cards.is_empty() {
                panel(ui, |ui| {
                    wrapped_label(
                        ui,
                        mut_text("No installed models are selected for Playground tests."),
                    );
                    let selector_busy = self.playground_selector_busy();
                    let choose_models = ui
                        .add_enabled(!selector_busy, primary_small_button(ui, "Choose Models"))
                        .on_hover_text("Select installed models to compare in the Playground.");
                    if choose_models.clicked() {
                        self.open_playground_selector(Some(choose_models.id));
                    }
                    if selector_busy {
                        wrapped_label(
                            ui,
                            mut_text("Finish active Playground work before changing models."),
                        );
                    }
                });
            }
            let card_count = self.playground_cards.len();
            for (card_index, card_state) in self.playground_cards.iter_mut().enumerate() {
                let model_id = card_state.model.id.clone();
                let is_active_model = model_id == self.config.selected_default_model;
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
                wrapped_label(
                    ui,
                    mut_text(
                        "Performance numbers are calculated locally. Duration includes model loading, inference, and post-processing for the current recording.",
                    ),
                );
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
        let installed_models = config::configured_models(&self.config)
            .into_iter()
            .filter(|model| model.install_status.is_runnable())
            .collect::<Vec<_>>();
        let request_initial_focus =
            std::mem::take(&mut self.playground_selector_needs_initial_focus);
        let mut open = true;
        let mut apply = false;
        let mut cancel = false;
        let screen_rect = ctx.screen_rect();
        let selector_margin = 16.0;
        let selector_max_size =
            (screen_rect.size() - Vec2::splat(selector_margin * 2.0)).max(Vec2::new(1.0, 1.0));
        let selector_width = selector_max_size.x.min(480.0);
        let selector_height = selector_max_size.y.min(440.0);
        let selector_pos = egui::pos2(
            screen_rect.center().x - selector_width * 0.5,
            screen_rect.top() + selector_margin,
        );
        egui::Area::new(egui::Id::new("playground-selector-shield"))
            .order(egui::Order::Background)
            .fixed_pos(screen_rect.min)
            .movable(false)
            .show(ctx, |ui| {
                let shield_rect = egui::Rect::from_min_size(egui::Pos2::ZERO, screen_rect.size());
                let mut shield_sense = egui::Sense::click_and_drag();
                shield_sense.focusable = false;
                ui.allocate_rect(shield_rect, shield_sense);
                ui.painter().rect_filled(
                    shield_rect,
                    Rounding::ZERO,
                    Color32::from_black_alpha(72),
                );
            });
        egui::Window::new("Choose models to test")
            .collapsible(false)
            .resizable(true)
            .default_width(selector_width)
            .default_height(selector_height)
            .min_width(432.0)
            .max_size(selector_max_size)
            .fixed_pos(selector_pos)
            .constrain_to(screen_rect.shrink(selector_margin))
            .open(&mut open)
            .show(ctx, |ui| {
                wrapped_label(
                    ui,
                    mut_text("Only installed models can be selected for Playground tests."),
                );
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
                            .map(|model| model.id.clone())
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
                    wrapped_label(
                        ui,
                        mut_text(
                            "No installed models yet. Install a model from Models, then return here to select it.",
                        ),
                    );
                } else {
                    #[cfg_attr(not(test), allow(unused_variables))]
                    let selector_scroll = egui::ScrollArea::vertical()
                        .max_height(320.0)
                        .show(ui, |ui| {
                            for model in &installed_models {
                                ui.horizontal_wrapped(|ui| {
                                    let mut selected = draft.iter().any(|id| id == &model.id);
                                    let readiness = runtime_status_for_model(&self.config, model);
                                    let checkbox = ui.add_enabled(
                                        !busy,
                                        egui::Checkbox::new(&mut selected, &model.name),
                                    );
                                    checkbox.widget_info(|| {
                                        let mut info = egui::WidgetInfo::selected(
                                            egui::WidgetType::Checkbox,
                                            selected,
                                            format!(
                                                "{}; backend {}; readiness {}",
                                                model.name, model.backend, readiness
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
                                    badge(ui, &model.backend, ChipTone::Neutral);
                                    badge(
                                        ui,
                                        &readiness.to_string(),
                                        runtime_chip_tone(&readiness),
                                    );
                                });
                            }
                        });
                    #[cfg(test)]
                    ctx.data_mut(|data| {
                        data.insert_temp(
                            egui::Id::new("test-selector-scroll-metrics"),
                            (
                                selector_scroll.id,
                                selector_scroll.content_size.y,
                                selector_scroll.inner_rect,
                                selector_scroll.state.offset.y,
                            ),
                        );
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

    fn ui_settings(&mut self, ui: &mut Ui) {
        let status = self.status;
        let status_message = self.status_message.clone();
        page(ui, "Settings", status, &status_message, |ui| {
            card(ui, |ui| {
                ui.label(section_heading("General"));
                ui.add_space(8.0);
                let mut close_to_tray = self.config.close_to_tray;
                if ui.checkbox(&mut close_to_tray, "Close to tray").changed() {
                    self.config.close_to_tray = close_to_tray;
                    self.save_config();
                }
                let mut auto_insert = self.config.auto_insert_transcript;
                if ui
                    .checkbox(&mut auto_insert, "Insert transcript into focused app")
                    .changed()
                {
                    self.config.auto_insert_transcript = auto_insert;
                    self.save_config();
                }
                ui.add_enabled_ui(self.config.auto_insert_transcript, |ui| {
                    let mut restore_clipboard = self.config.restore_clipboard_after_insert;
                    if ui
                        .checkbox(&mut restore_clipboard, "Restore clipboard after insert")
                        .changed()
                    {
                        self.config.restore_clipboard_after_insert = restore_clipboard;
                        self.save_config();
                    }
                    let mut paste_delay = self.config.paste_delay_ms as i32;
                    ui.horizontal_wrapped(|ui| {
                        let label = ui.label("Paste delay ms");
                        let response =
                            ui.add(egui::DragValue::new(&mut paste_delay).clamp_range(1..=1000));
                        set_control_accessibility(ui, &response, label.id, "Paste delay ms");
                        if response.changed() {
                            self.config.paste_delay_ms = paste_delay.max(1) as u64;
                            self.save_config();
                        }
                    });
                });
            });

            ui.add_space(12.0);
            card(ui, |ui| {
                let heading = ui.label(section_heading("Recording"));
                ui.ctx().accesskit_node_builder(heading.id, |builder| {
                    builder.set_role(egui::accesskit::Role::Heading);
                    builder.set_hierarchical_level(2);
                });
                ui.add_space(8.0);
                ui.horizontal_wrapped(|ui| {
                    let before = self.config.hotkey_mode;
                    let label = ui.label("Hotkey behavior");
                    let press_once = ui.radio_value(
                        &mut self.config.hotkey_mode,
                        HotkeyMode::Toggle,
                        "Press once",
                    );
                    let hold = ui.radio_value(
                        &mut self.config.hotkey_mode,
                        HotkeyMode::HoldToTalk,
                        "Hold",
                    );
                    let group = [press_once.id, hold.id];
                    set_radio_accessibility(
                        ui,
                        &press_once,
                        label.id,
                        "Hotkey behavior: Press once",
                        &group,
                    );
                    set_radio_accessibility(ui, &hold, label.id, "Hotkey behavior: Hold", &group);
                    if before != self.config.hotkey_mode {
                        self.save_config();
                    }
                });
                wrapped_label(
                    ui,
                    mut_text("Press once to start or stop, or hold the hotkey while speaking."),
                );
                ui.add_space(8.0);

                let before = self.config.max_recording_seconds;
                let preset_response = ui
                    .horizontal_wrapped(|ui| {
                        let label = ui.label("Duration preset");
                        let response = ComboBox::from_id_source("recording-duration-preset")
                            .selected_text(format_recording_duration(
                                self.config.max_recording_seconds,
                            ))
                            .width(140.0)
                            .show_ui(ui, |ui| {
                                for (seconds, text) in RECORDING_DURATION_PRESETS {
                                    ui.selectable_value(
                                        &mut self.config.max_recording_seconds,
                                        seconds,
                                        text,
                                    );
                                }
                            })
                            .response;
                        set_control_accessibility(
                            ui,
                            &response,
                            label.id,
                            "Recording duration preset",
                        );
                        response
                    })
                    .inner;
                let custom_response = ui
                    .horizontal_wrapped(|ui| {
                        let mut minutes =
                            recording_duration_minutes(self.config.max_recording_seconds);
                        let label = ui.label("Custom duration");
                        let response = ui.add(
                            egui::DragValue::new(&mut minutes)
                                .clamp_range(0.5..=120.0)
                                .speed(0.5)
                                .max_decimals(2)
                                .suffix(" minutes"),
                        );
                        set_control_accessibility(
                            ui,
                            &response,
                            label.id,
                            "Custom recording duration in minutes",
                        );
                        if response.changed() {
                            self.config.max_recording_seconds = recording_duration_seconds(minutes);
                        }
                        response
                    })
                    .inner;
                if self.active_recording.is_some() {
                    recording_duration_change_notice(ui, [&preset_response, &custom_response]);
                }
                if before != self.config.max_recording_seconds {
                    self.config.max_recording_seconds =
                        config::normalize_recording_duration(self.config.max_recording_seconds);
                    self.save_config();
                }
            });

            ui.add_space(12.0);
            card(ui, |ui| {
                ui.label(section_heading("Shortcuts"));
                ui.add_space(8.0);
                ui.horizontal_wrapped(|ui| {
                    let label = ui.label("Record toggle");
                    let response = ui.add(
                        TextEdit::singleline(&mut self.hotkey_input)
                            .desired_width(width_before_trailing(ui, 154.0, 96.0)),
                    );
                    set_control_accessibility(ui, &response, label.id, "Record toggle hotkey");
                    if ui.add(small_button(ui, "Apply")).clicked() {
                        self.apply_hotkey();
                    }
                    if ui
                        .add(small_button(
                            ui,
                            if self.capturing_hotkey {
                                "Listening..."
                            } else {
                                "Capture"
                            },
                        ))
                        .clicked()
                    {
                        self.capturing_hotkey = true;
                        self.status_message = "Press the new hotkey combination.".to_owned();
                    }
                });
            });

            ui.add_space(12.0);
            card(ui, |ui| {
                ui.label(section_heading("Performance"));
                ui.add_space(8.0);
                let active_device_support = selected_model_device_support(&self.config);
                let prefer_gpu_available = active_device_support.supports_gpu();
                ui.horizontal_wrapped(|ui| {
                    let label = ui.label("Transcription device");
                    let mut compute_mode = self.config.whisper_compute_mode;
                    let response = ComboBox::from_id_source("transcription-device-mode")
                        .selected_text(compute_mode.label())
                        .width(160.0)
                        .show_ui(ui, |ui| {
                            for mode in WhisperComputeMode::ALL {
                                let enabled =
                                    mode != WhisperComputeMode::PreferGpu || prefer_gpu_available;
                                ui.add_enabled_ui(enabled, |ui| {
                                    ui.selectable_value(&mut compute_mode, mode, mode.label());
                                });
                            }
                        })
                        .response;
                    set_control_accessibility(ui, &response, label.id, "Transcription device");
                    if compute_mode != self.config.whisper_compute_mode {
                        self.config.whisper_compute_mode = compute_mode;
                        self.save_config();
                    }
                });
                if !prefer_gpu_available {
                    ui.add_space(4.0);
                    wrapped_label(
                        ui,
                        mut_text(
                            "The active model backend is CPU-only. GPU mode is available for whisper.cpp and faster-whisper models.",
                        ),
                    );
                }
                if let Some(provider) = stt::provider_for_backend("whisper.cpp")
                    && provider.device_detection_supported
                {
                    let devices = provider.detect_devices(&self.config);
                    if devices.len() > 1 {
                        ui.horizontal_wrapped(|ui| {
                            let label = ui.label("GPU device");
                            let mut selected_device = self.config.whisper_gpu_device.to_string();
                            let response = ComboBox::from_id_source("transcription-device-picker")
                                .selected_text(
                                    devices
                                        .iter()
                                        .find(|device| device.id == selected_device)
                                        .map(|device| device.name.as_str())
                                        .unwrap_or("Auto"),
                                )
                                .wrap(true)
                                .height(DYNAMIC_COMBO_POPUP_MAX_HEIGHT)
                                .show_ui(ui, |ui| {
                                    prepare_dynamic_combo_popup(ui);
                                    for device in &devices {
                                        selectable_dynamic_combo_value(
                                            ui,
                                            &mut selected_device,
                                            device.id.clone(),
                                            &device.name,
                                        );
                                    }
                                })
                                .response;
                            set_control_accessibility(ui, &response, label.id, "GPU device");
                            if let Ok(device_index) = selected_device.parse::<u32>()
                                && device_index != self.config.whisper_gpu_device
                            {
                                self.config.whisper_gpu_device = device_index;
                                self.save_config();
                            }
                        });
                    }
                }
            });

            ui.add_space(12.0);
            card(ui, |ui| {
                ui.label(section_heading("Audio"));
                ui.add_space(8.0);
                ui.horizontal_wrapped(|ui| {
                    let before = self.config.audio_input_device_name.clone();
                    let label = ui.label("Microphone");
                    let response = ComboBox::from_id_source("audio-input-device")
                        .selected_text(
                            self.config
                                .audio_input_device_name
                                .as_deref()
                                .unwrap_or("OS default"),
                        )
                        .wrap(true)
                        .height(DYNAMIC_COMBO_POPUP_MAX_HEIGHT)
                        .show_ui(ui, |ui| {
                            prepare_dynamic_combo_popup(ui);
                            selectable_dynamic_combo_value(
                                ui,
                                &mut self.config.audio_input_device_name,
                                None,
                                "OS default",
                            );
                            for device in &self.audio_devices {
                                selectable_dynamic_combo_value(
                                    ui,
                                    &mut self.config.audio_input_device_name,
                                    Some(device.clone()),
                                    device,
                                );
                            }
                        })
                        .response;
                    set_control_accessibility(ui, &response, label.id, "Microphone");
                    if before != self.config.audio_input_device_name {
                        self.save_config();
                    }
                    if ui.add(small_button(ui, "Refresh")).clicked() {
                        self.refresh_audio_devices();
                    }
                });
            });

            ui.add_space(12.0);
            card(ui, |ui| {
                ui.label(section_heading("Appearance"));
                ui.add_space(8.0);
                ui.horizontal_wrapped(|ui| {
                    let before = self.config.theme_mode;
                    let label = ui.label("Theme");
                    let response = ComboBox::from_id_source("theme-mode")
                        .selected_text(self.config.theme_mode.label())
                        .width(140.0)
                        .show_ui(ui, |ui| {
                            for mode in ThemeMode::ALL {
                                ui.selectable_value(
                                    &mut self.config.theme_mode,
                                    mode,
                                    mode.label(),
                                );
                            }
                        })
                        .response;
                    set_control_accessibility(ui, &response, label.id, "Theme");
                    if before != self.config.theme_mode {
                        self.save_config();
                    }
                });
            });

            ui.add_space(12.0);
            card(ui, |ui| {
                ui.label(section_heading("Runtime"));
                ui.label(RichText::new("Models run only when transcription starts. No cloud speech service, account sync, or always-on listener is enabled.").weak());
                if let Some(latency) = &self.latest_latency {
                    ui.add_space(8.0);
                    ui.label(section_heading("Last Latency"));
                    for line in latency.summary_lines() {
                        wrapped_label(ui, mut_text(line));
                    }
                }
                if self.tray_service.is_none() {
                    ui.colored_label(
                        ui_palette(ui).error,
                        "Tray integration is unavailable in this desktop session.",
                    );
                }
                if let Some(notice) = text_output::paste_automation_notice() {
                    ui.colored_label(ui_palette(ui).warning, notice);
                }
            });
        });
    }
}

const PLAYGROUND_RESULT_HEIGHT: f32 = 92.0;
const MODEL_ACTION_COLUMN_WIDTH: f32 = 152.0;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ThemePalette {
    shell_bg: Color32,
    content_bg: Color32,
    sidebar_bg: Color32,
    card_bg: Color32,
    panel_bg: Color32,
    active_card_bg: Color32,
    text: Color32,
    muted_text: Color32,
    border: Color32,
    border_strong: Color32,
    primary: Color32,
    accent: Color32,
    success: Color32,
    warning: Color32,
    error: Color32,
    primary_button_bg: Color32,
    primary_button_text: Color32,
}

impl ThemePalette {
    fn from_visuals(visuals: &egui::Visuals) -> Self {
        if visuals.dark_mode {
            Self::dark()
        } else {
            Self::light()
        }
    }

    fn light() -> Self {
        Self {
            shell_bg: Color32::from_rgba_unmultiplied(255, 255, 255, 184),
            content_bg: Color32::from_rgb(238, 242, 246),
            sidebar_bg: Color32::from_rgba_unmultiplied(255, 255, 255, 196),
            card_bg: Color32::from_rgba_unmultiplied(255, 255, 255, 232),
            panel_bg: Color32::from_rgba_unmultiplied(255, 255, 255, 184),
            active_card_bg: Color32::from_rgba_unmultiplied(224, 235, 252, 224),
            text: Color32::from_rgb(24, 33, 43),
            muted_text: Color32::from_rgb(94, 107, 120),
            border: Color32::from_rgba_unmultiplied(121, 139, 157, 66),
            border_strong: Color32::from_rgba_unmultiplied(92, 110, 128, 104),
            primary: Color32::from_rgb(24, 33, 43),
            accent: Color32::from_rgb(50, 105, 199),
            success: Color32::from_rgb(47, 125, 88),
            warning: Color32::from_rgb(154, 106, 24),
            error: Color32::from_rgb(180, 65, 66),
            primary_button_bg: Color32::from_rgb(50, 105, 199),
            primary_button_text: Color32::WHITE,
        }
    }

    fn dark() -> Self {
        Self {
            shell_bg: Color32::from_rgba_unmultiplied(28, 38, 50, 238),
            content_bg: Color32::from_rgb(20, 27, 36),
            sidebar_bg: Color32::from_rgba_unmultiplied(31, 42, 54, 220),
            card_bg: Color32::from_rgba_unmultiplied(37, 49, 63, 238),
            panel_bg: Color32::from_rgba_unmultiplied(31, 42, 54, 220),
            active_card_bg: Color32::from_rgba_unmultiplied(42, 67, 103, 230),
            text: Color32::from_rgb(232, 238, 245),
            muted_text: Color32::from_rgb(174, 186, 199),
            border: Color32::from_rgba_unmultiplied(157, 177, 197, 82),
            border_strong: Color32::from_rgba_unmultiplied(195, 211, 226, 118),
            primary: Color32::from_rgb(232, 238, 245),
            accent: Color32::from_rgb(112, 164, 235),
            success: Color32::from_rgb(104, 198, 145),
            warning: Color32::from_rgb(232, 181, 83),
            error: Color32::from_rgb(239, 126, 127),
            primary_button_bg: Color32::from_rgb(50, 105, 199),
            primary_button_text: Color32::WHITE,
        }
    }
}

fn theme_palette(ctx: &egui::Context) -> ThemePalette {
    ThemePalette::from_visuals(&ctx.style().visuals)
}

fn ui_palette(ui: &Ui) -> ThemePalette {
    ThemePalette::from_visuals(ui.visuals())
}

#[derive(Clone, Copy)]
enum ChipTone {
    Neutral,
    Success,
    Warning,
    Error,
    Active,
}

fn configure_stitch_style(ctx: &egui::Context) {
    let mut style = (*ctx.style()).clone();
    style.spacing.item_spacing = Vec2::new(8.0, 8.0);
    style.spacing.button_padding = Vec2::new(10.0, 6.0);
    style.spacing.interact_size = Vec2::new(32.0, 36.0);
    style.text_styles.insert(
        TextStyle::Heading,
        FontId::new(24.0, FontFamily::Proportional),
    );
    style
        .text_styles
        .insert(TextStyle::Body, FontId::new(14.0, FontFamily::Proportional));
    style.text_styles.insert(
        TextStyle::Button,
        FontId::new(13.0, FontFamily::Proportional),
    );
    style.text_styles.insert(
        TextStyle::Small,
        FontId::new(12.0, FontFamily::Proportional),
    );
    ctx.set_style(style);
}

fn paint_viewport_background(ctx: &egui::Context) {
    let colors = theme_palette(ctx);
    ctx.layer_painter(egui::LayerId::background()).rect_filled(
        ctx.screen_rect(),
        0.0,
        colors.content_bg,
    );
}

fn navigation_rail(ctx: &egui::Context, current_tab: &mut Tab) {
    let colors = theme_palette(ctx);
    egui::SidePanel::left("navigation")
        .frame(
            Frame::none()
                .fill(colors.sidebar_bg)
                .stroke(Stroke::new(1.0, colors.border))
                .inner_margin(Margin::symmetric(8.0, 12.0)),
        )
        .resizable(false)
        .exact_width(60.0)
        .show(ctx, |ui| {
            ui.vertical_centered(|ui| {
                ui.label(
                    RichText::new("S")
                        .font(FontId::proportional(20.0))
                        .color(colors.primary)
                        .strong(),
                );
            });
            ui.add_space(20.0);
            nav_button(ui, current_tab, Tab::Transcribe);
            nav_button(ui, current_tab, Tab::Models);
            nav_button(ui, current_tab, Tab::Playground);
            nav_button(ui, current_tab, Tab::Settings);
        });
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
        Vec2::new(page_width, ui.available_height()),
        Layout::top_down(Align::LEFT),
        |ui| {
            set_exact_width(ui, page_width);
            ui.add_space(24.0);
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(title)
                        .font(FontId::proportional(24.0))
                        .color(ui_palette(ui).primary)
                        .strong(),
                );
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
            let body_width = usable_width(ui);
            let _scroll_output = ScrollArea::vertical()
                .id_source(("page-scroll", title))
                .max_width(body_width)
                .min_scrolled_width(body_width)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    with_usable_width_cap(ui, body_width, |ui| {
                        set_exact_width(ui, body_width);
                        add_contents(ui);
                    });
                });
            #[cfg(test)]
            ui.ctx().data_mut(|data| {
                data.insert_temp(
                    egui::Id::new(("test-page-scroll-metrics", title)),
                    (
                        _scroll_output.id,
                        _scroll_output.content_size.y,
                        _scroll_output.inner_rect.height(),
                    ),
                );
            });
        },
    );
}

fn content_panel_frame(ctx: &egui::Context) -> Frame {
    let colors = theme_palette(ctx);
    Frame::none()
        .fill(colors.shell_bg)
        .stroke(Stroke::new(1.0, colors.border))
        .rounding(Rounding::same(14.0))
        .shadow(egui::epaint::Shadow {
            offset: Vec2::new(0.0, 2.0),
            blur: 6.0,
            spread: 0.0,
            color: Color32::from_black_alpha(if ctx.style().visuals.dark_mode {
                72
            } else {
                24
            }),
        })
        .outer_margin(Margin::same(8.0))
        .inner_margin(Margin::symmetric(24.0, 0.0))
}

fn card(ui: &mut Ui, add_contents: impl FnOnce(&mut Ui)) {
    full_width_frame(ui, card_frame(ui), add_contents);
}

fn card_frame(ui: &Ui) -> Frame {
    let colors = ui_palette(ui);
    Frame::none()
        .fill(colors.card_bg)
        .stroke(Stroke::new(1.0, colors.border))
        .rounding(Rounding::same(10.0))
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
        .rounding(Rounding::same(10.0))
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

fn model_search_filter_control(ui: &mut Ui, search: &mut String) {
    ui.vertical(|ui| {
        let label = ui.label(label_caps("Search"));
        let response = ui.add_sized(
            [190.0, 36.0],
            TextEdit::singleline(search).hint_text("Search models..."),
        );
        set_control_accessibility(ui, &response, label.id, "Search models");
    });
}

fn set_control_accessibility(ui: &Ui, response: &egui::Response, label_id: egui::Id, name: &str) {
    response.clone().labelled_by(label_id);
    ui.ctx().accesskit_node_builder(response.id, |builder| {
        builder.set_name(name);
    });
}

fn set_radio_accessibility(
    ui: &Ui,
    response: &egui::Response,
    label_id: egui::Id,
    name: &str,
    group: &[egui::Id],
) {
    response.clone().labelled_by(label_id);
    ui.ctx().accesskit_node_builder(response.id, |builder| {
        builder.set_role(egui::accesskit::Role::RadioButton);
        builder.set_name(name);
        builder.set_radio_group(group.iter().map(|id| id.value().into()).collect::<Vec<_>>());
    });
}

fn recording_duration_change_notice(ui: &mut Ui, controls: [&egui::Response; 2]) {
    let notice = ui.add(
        egui::Label::new(mut_text(
            "Duration changes apply to the next recording; the current recording is unchanged.",
        ))
        .wrap(true),
    );
    ui.ctx().accesskit_node_builder(notice.id, |builder| {
        builder.set_role(egui::accesskit::Role::Status);
        builder.set_live(egui::accesskit::Live::Polite);
    });

    let notice_id = notice.id.value().into();
    for control in controls {
        ui.ctx().accesskit_node_builder(control.id, |builder| {
            builder.push_described_by(notice_id);
        });
    }
}

fn model_backend_filter_control(
    ui: &mut Ui,
    id_source: &'static str,
    selected_backend: &mut String,
    backends: &[String],
) {
    ui.vertical(|ui| {
        let label = ui.label(label_caps("Filter Backend"));
        let response = ComboBox::from_id_source(id_source)
            .selected_text(if selected_backend == "All" {
                "All Backends"
            } else {
                selected_backend.as_str()
            })
            .width(150.0)
            .height(DYNAMIC_COMBO_POPUP_MAX_HEIGHT)
            .show_ui(ui, |ui| {
                prepare_dynamic_combo_popup(ui);
                selectable_dynamic_combo_value(
                    ui,
                    selected_backend,
                    "All".to_owned(),
                    "All backends",
                );
                for backend in backends {
                    selectable_dynamic_combo_value(ui, selected_backend, backend.clone(), backend);
                }
            })
            .response;
        set_control_accessibility(ui, &response, label.id, "Filter model backend");
    });
}

const DYNAMIC_COMBO_POPUP_MAX_WIDTH: f32 = 480.0;
const DYNAMIC_COMBO_POPUP_MAX_HEIGHT: f32 = 320.0;

fn prepare_dynamic_combo_popup(ui: &mut Ui) {
    let available_screen_width = (ui.ctx().screen_rect().width() - 32.0).max(1.0);
    let popup_width = available_screen_width.min(DYNAMIC_COMBO_POPUP_MAX_WIDTH);
    ui.set_max_width(popup_width);
    ui.set_width(popup_width);
    ui.style_mut().wrap = Some(true);
}

fn selectable_dynamic_combo_value<T>(ui: &mut Ui, current: &mut T, value: T, label: &str)
where
    T: Clone + PartialEq,
{
    let selected = current == &value;
    let response = ui.add(egui::SelectableLabel::new(selected, label));
    if response.clicked() {
        *current = value;
    }
}

fn recessed_panel(
    ui: &mut Ui,
    min_height: f32,
    add_contents: impl FnOnce(&mut Ui),
) -> egui::InnerResponse<()> {
    let colors = ui_palette(ui);
    let response = full_width_frame(
        ui,
        Frame::none()
            .fill(colors.panel_bg)
            .stroke(Stroke::new(1.0, colors.border_strong))
            .rounding(Rounding::same(12.0))
            .inner_margin(Margin::same(18.0)),
        |ui| {
            ui.set_min_height(min_height);
            add_contents(ui);
        },
    );
    let rect = response.response.rect;
    let (highlight, shade) = recessed_edge_colors(ui.visuals().dark_mode);
    ui.painter().line_segment(
        [
            rect.left_top() + Vec2::new(10.0, 0.5),
            rect.right_top() + Vec2::new(-10.0, 0.5),
        ],
        Stroke::new(1.0, highlight),
    );
    ui.painter().line_segment(
        [
            rect.left_bottom() + Vec2::new(10.0, -0.5),
            rect.right_bottom() + Vec2::new(-10.0, -0.5),
        ],
        Stroke::new(1.0, shade),
    );
    response
}

fn recessed_edge_colors(dark_mode: bool) -> (Color32, Color32) {
    if dark_mode {
        (
            Color32::from_rgba_unmultiplied(205, 220, 235, 28),
            Color32::from_rgba_unmultiplied(4, 9, 15, 92),
        )
    } else {
        (
            Color32::from_rgba_unmultiplied(255, 255, 255, 214),
            Color32::from_rgba_unmultiplied(92, 110, 128, 44),
        )
    }
}

fn transcript_panel(ui: &mut Ui, add_contents: impl FnOnce(&mut Ui)) {
    recessed_panel(ui, 0.0, add_contents);
}

fn model_catalog_row(
    ui: &mut Ui,
    model: &SttModelInfo,
    install_status: &ModelInstallStatus,
    selected: bool,
    actions: impl FnOnce(&mut Ui),
) {
    full_width_frame(ui, model_card_frame(ui, selected), |ui| {
        ui.scope(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            let actions_width = MODEL_ACTION_COLUMN_WIDTH;
            let detail_width = (ui.available_width() - actions_width - 12.0).max(0.0);
            ui.horizontal_top(|ui| {
                ui.allocate_ui_with_layout(
                    Vec2::new(detail_width, 0.0),
                    Layout::top_down(Align::LEFT),
                    |ui| {
                        set_exact_width(ui, detail_width);
                        wrapped_label(ui, card_title(ui, &model.name, selected));
                        wrapped_label(ui, mut_text(&model.description));
                        if let Some(detail) = model_install_detail(model, install_status) {
                            ui.add_space(4.0);
                            wrapped_label(ui, mut_text(&detail));
                        }
                        if matches!(
                            install_status,
                            ModelInstallStatus::Downloading { .. }
                                | ModelInstallStatus::InstallingRuntime
                        ) {
                            ui.add_space(8.0);
                            download_progress_bar(ui, install_status);
                        }
                        ui.add_space(8.0);
                        tag_row(ui, |ui| {
                            badge(ui, &model.backend, ChipTone::Neutral);
                            badge(
                                ui,
                                &format!("Device {}", model_device_label(model)),
                                ChipTone::Neutral,
                            );
                            badge(
                                ui,
                                &format!("Model {}", model_storage_estimate(model)),
                                ChipTone::Neutral,
                            );
                            badge(
                                ui,
                                &format!("RAM {}", model.expected_ram),
                                ChipTone::Neutral,
                            );
                            badge(
                                ui,
                                &format!("{} speed", model.speed_tier),
                                ChipTone::Neutral,
                            );
                            badge(
                                ui,
                                &format!("{} accuracy", model.accuracy_tier),
                                ChipTone::Neutral,
                            );
                        });
                    },
                );
                ui.add_space(12.0);
                ui.allocate_ui_with_layout(
                    Vec2::new(actions_width, 0.0),
                    Layout::top_down(Align::RIGHT),
                    |ui| {
                        set_exact_width(ui, actions_width);
                        actions(ui);
                    },
                );
            });
        });
    });
}

fn current_download_rows(
    config: &AppConfig,
    downloads: &HashMap<String, ModelInstallStatus>,
) -> Vec<(SttModelInfo, ModelInstallStatus)> {
    let mut rows = config::configured_models(config)
        .into_iter()
        .filter_map(|model| {
            downloads
                .get(&model.id)
                .cloned()
                .map(|status| (model, status))
        })
        .filter(|(_, status)| !matches!(status, ModelInstallStatus::NotInstalled))
        .collect::<Vec<_>>();

    rows.sort_by_key(|(_, status)| match status {
        ModelInstallStatus::InstallingRuntime => 0,
        ModelInstallStatus::Downloading { .. } => 0,
        ModelInstallStatus::Error(_) => 1,
        ModelInstallStatus::RuntimeError(_) => 1,
        ModelInstallStatus::Installed => 2,
        ModelInstallStatus::Missing => 3,
        ModelInstallStatus::NotInstalled => 4,
    });
    rows
}

fn download_summary_row(ui: &mut Ui, model: &SttModelInfo, install_status: &ModelInstallStatus) {
    full_width_frame(
        ui,
        Frame::none().inner_margin(Margin::symmetric(0.0, 4.0)),
        |ui| {
            ui.horizontal_top(|ui| {
                ui.vertical(|ui| {
                    wrapped_label(ui, body_strong(&model.name));
                    tag_row(ui, |ui| {
                        badge(ui, &model.backend, ChipTone::Neutral);
                        badge(
                            ui,
                            &format!("Device {}", model_device_label(model)),
                            ChipTone::Neutral,
                        );
                        badge(
                            ui,
                            &install_status.label(),
                            install_chip_tone(install_status),
                        );
                    });
                    ui.add_space(6.0);
                    download_progress_bar(ui, install_status);
                });
            });
        },
    );
}

fn download_progress_bar(ui: &mut Ui, install_status: &ModelInstallStatus) {
    let progress = download_progress_fraction(install_status);
    let text = download_progress_bar_text(install_status);
    let indeterminate = progress.is_none()
        && matches!(
            install_status,
            ModelInstallStatus::InstallingRuntime | ModelInstallStatus::Downloading { .. }
        );
    let response = ui.add(
        egui::ProgressBar::new(progress.unwrap_or_default())
            .desired_width(usable_width(ui).max(1.0))
            .desired_height(18.0)
            .text(text)
            .animate(indeterminate),
    );
    if indeterminate {
        ui.ctx().accesskit_node_builder(response.id, |builder| {
            builder.clear_numeric_value();
            builder.clear_min_numeric_value();
            builder.clear_max_numeric_value();
        });
    }
    if let Some(detail) = download_progress_detail(install_status) {
        ui.add_space(4.0);
        wrapped_label(ui, mut_text(detail));
    }
}

fn set_collapsing_header_accessibility<R>(
    ctx: &egui::Context,
    response: &egui::containers::CollapsingResponse<R>,
) {
    ctx.accesskit_node_builder(response.header_response.id, |builder| {
        builder.set_expanded(response.body_response.is_some());
    });
}

fn download_progress_fraction(install_status: &ModelInstallStatus) -> Option<f32> {
    match install_status {
        ModelInstallStatus::Downloading {
            downloaded_bytes,
            total_bytes: Some(total_bytes),
            ..
        } if *total_bytes > 0 => {
            Some((*downloaded_bytes as f32 / *total_bytes as f32).clamp(0.0, 1.0))
        }
        ModelInstallStatus::Installed => Some(1.0),
        _ => None,
    }
}

fn download_progress_bar_text(install_status: &ModelInstallStatus) -> String {
    match install_status {
        ModelInstallStatus::Downloading {
            downloaded_bytes,
            total_bytes: Some(total_bytes),
            ..
        } if *total_bytes > 0 => {
            let percent =
                (*downloaded_bytes as f64 / *total_bytes as f64 * 100.0).clamp(0.0, 100.0);
            format!("{percent:.0}% Completed")
        }
        ModelInstallStatus::Downloading { .. } => "Downloading".to_owned(),
        ModelInstallStatus::InstallingRuntime => "Preparing runtime".to_owned(),
        ModelInstallStatus::Installed => "100% Completed".to_owned(),
        ModelInstallStatus::Error(_) => "Failed".to_owned(),
        ModelInstallStatus::RuntimeError(_) => "Runtime repair failed".to_owned(),
        ModelInstallStatus::Missing => "Missing".to_owned(),
        ModelInstallStatus::NotInstalled => "Not installed".to_owned(),
    }
}

fn download_progress_detail(install_status: &ModelInstallStatus) -> Option<String> {
    match install_status {
        ModelInstallStatus::Downloading {
            downloaded_bytes,
            total_bytes,
            bytes_per_second,
        } => {
            let transferred = match total_bytes {
                Some(total_bytes) if *total_bytes > 0 => {
                    let displayed_total = (*total_bytes).max(*downloaded_bytes);
                    format!(
                        "{} / {}",
                        format_bytes(*downloaded_bytes),
                        format_bytes(displayed_total)
                    )
                }
                _ => format_bytes(*downloaded_bytes),
            };
            Some(match bytes_per_second.filter(|speed| *speed > 0) {
                Some(speed) => format!("{transferred} | {}/s", format_bytes(speed)),
                None => transferred,
            })
        }
        ModelInstallStatus::Installed => Some("Installed".to_owned()),
        ModelInstallStatus::InstallingRuntime => {
            Some("Preparing the shared backend runtime before downloading this model.".to_owned())
        }
        ModelInstallStatus::Error(message) => Some(message.clone()),
        ModelInstallStatus::RuntimeError(message) => Some(message.clone()),
        ModelInstallStatus::Missing => Some("Missing file".to_owned()),
        ModelInstallStatus::NotInstalled => None,
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
    let mut drag_sense = egui::Sense::drag();
    drag_sense.focusable = false;
    let response = ui
        .interact(rect, drag_id, drag_sense)
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
        let gap = 12.0;
        ui.horizontal_top(|ui| {
            ui.allocate_ui_with_layout(
                Vec2::new(move_width, 0.0),
                Layout::top_down(Align::LEFT),
                |ui| {
                    set_exact_width(ui, move_width);
                    playground_drag_handle(
                        ui,
                        drag_id,
                        card_state.model.id.clone(),
                        &card_state.model.name,
                        is_active_model,
                    );
                },
            );
            ui.add_space(gap);
            ui.vertical(|ui| {
                wrapped_label(ui, card_title(ui, &card_state.model.name, is_active_model));
                wrapped_label(ui, mut_text(&card_state.model.description));
                ui.add_space(8.0);
                tag_row(ui, |ui| {
                    badge(ui, &card_state.model.backend, ChipTone::Neutral);
                    badge(
                        ui,
                        &format!("Device {}", model_device_label(&card_state.model)),
                        ChipTone::Neutral,
                    );
                    badge(
                        ui,
                        &card_state.model.install_status.label(),
                        install_chip_tone(&card_state.model.install_status),
                    );
                    badge(
                        ui,
                        &format!("{} speed", card_state.model.speed_tier),
                        ChipTone::Neutral,
                    );
                });
            });
        });

        ui.add_space(8.0);
        ui.horizontal_wrapped(|ui| {
            let move_up = ui.add_enabled(can_move_up, small_button(ui, "Move up"));
            move_up.widget_info(|| {
                let mut info = egui::WidgetInfo::labeled(
                    egui::WidgetType::Button,
                    format!("Move {} up", card_state.model.name),
                );
                info.enabled = can_move_up;
                info
            });
            if move_up.clicked() {
                actions.push(PlaygroundAction::MoveBy {
                    model_id: card_state.model.id.clone(),
                    offset: -1,
                });
            }
            let move_down = ui.add_enabled(can_move_down, small_button(ui, "Move down"));
            move_down.widget_info(|| {
                let mut info = egui::WidgetInfo::labeled(
                    egui::WidgetType::Button,
                    format!("Move {} down", card_state.model.name),
                );
                info.enabled = can_move_down;
                info
            });
            if move_down.clicked() {
                actions.push(PlaygroundAction::MoveBy {
                    model_id: card_state.model.id.clone(),
                    offset: 1,
                });
            }
            if ui.add(small_button(ui, "Clear")).clicked() {
                actions.push(PlaygroundAction::Clear(card_state.model.id.clone()));
            }
            badge(
                ui,
                &card_state.status.to_string(),
                runtime_chip_tone(&card_state.status),
            );
        });
    });

    if let Some(latency) = card_state.latency_ms {
        ui.label(RichText::new(format!("Latency: {latency} ms")).weak());
    }

    ui.add_space(6.0);
    playground_result_editor(ui, &card_state.model.id, &card_state.transcript);

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
                wrapped_label(ui, mut_text("Result"));
            } else {
                wrapped_label(ui, RichText::new(transcript));
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
                ui.label(section_heading("Benchmark Scores"));
                wrapped_label(
                    ui,
                    mut_text("Raw metric values are colored by relative score in this run."),
                );
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
                        ui.label(label_caps("Overall")).on_hover_text(
                            "Weighted normalized score for the selected ranking mode.",
                        );
                        for metric in &metrics {
                            ui.label(label_caps(metric.header()))
                                .on_hover_text(metric.tooltip());
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
                            );
                            for metric in &metrics {
                                let value = result.raw_metrics.value(*metric);
                                let score = result.normalized_scores.get(metric).copied();
                                benchmark_score_cell(
                                    ui,
                                    benchmark::format_metric_value(*metric, value),
                                    score,
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

fn benchmark_score_cell(ui: &mut Ui, label: String, score: Option<f64>) {
    let colors = ui_palette(ui);
    Frame::none()
        .fill(benchmark_heatmap_fill(ui, score))
        .stroke(Stroke::new(1.0, colors.border))
        .rounding(Rounding::same(4.0))
        .inner_margin(Margin::symmetric(8.0, 5.0))
        .show(ui, |ui| {
            ui.set_min_width(68.0);
            ui.label(RichText::new(label).strong());
        });
}

fn benchmark_heatmap_fill(ui: &Ui, score: Option<f64>) -> Color32 {
    let Some(score) = score else {
        return ui_palette(ui).panel_bg;
    };
    let score = score.clamp(0.0, 1.0);
    if score < 0.5 {
        let t = score / 0.5;
        lerp_color(
            Color32::from_rgb(254, 226, 226),
            Color32::from_rgb(254, 249, 195),
            t,
        )
    } else {
        let t = (score - 0.5) / 0.5;
        lerp_color(
            Color32::from_rgb(254, 249, 195),
            Color32::from_rgb(220, 252, 231),
            t,
        )
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

fn width_before_trailing(ui: &Ui, trailing_width: f32, min_width: f32) -> f32 {
    let available = usable_width(ui);
    if available <= trailing_width {
        return available.max(0.0);
    }
    (available - trailing_width)
        .max(min_width)
        .min(available - trailing_width)
}

fn paint_focus_ring(ui: &Ui, response: &egui::Response, rounding: f32) {
    if response.has_focus() {
        ui.painter().rect_stroke(
            response.rect.expand(2.0),
            Rounding::same(rounding + 2.0),
            Stroke::new(2.0, ui_palette(ui).accent),
        );
    }
}

struct FocusableButton<'a> {
    button: Button<'a>,
    rounding: f32,
}

impl egui::Widget for FocusableButton<'_> {
    fn ui(self, ui: &mut Ui) -> egui::Response {
        let response = ui.add(self.button);
        paint_focus_ring(ui, &response, self.rounding);
        response
    }
}

fn nav_button(ui: &mut Ui, current_tab: &mut Tab, tab: Tab) {
    let colors = ui_palette(ui);
    let selected = *current_tab == tab;
    let response = ui.add_sized(
        [ui.available_width(), 40.0],
        Button::new("")
            .fill(if selected {
                colors.active_card_bg
            } else {
                Color32::TRANSPARENT
            })
            .stroke(if selected {
                Stroke::new(1.0, colors.accent)
            } else {
                Stroke::NONE
            })
            .rounding(Rounding::same(8.0)),
    );
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::Button, selected, tab.label())
    });
    if selected {
        ui.painter().rect_filled(
            egui::Rect::from_center_size(
                egui::pos2(response.rect.left() + 3.0, response.rect.center().y),
                Vec2::new(3.0, 14.0),
            ),
            Rounding::same(1.5),
            colors.accent,
        );
    }
    paint_nav_icon(
        ui.painter(),
        response.rect.center(),
        tab,
        if selected {
            colors.accent
        } else {
            colors.muted_text
        },
    );
    paint_focus_ring(ui, &response, 8.0);
    response.clone().on_hover_text(tab.label());
    if response.clicked() {
        *current_tab = tab;
    }
}

fn paint_nav_icon(painter: &egui::Painter, center: egui::Pos2, tab: Tab, color: Color32) {
    let stroke = Stroke::new(1.8, color);
    match tab {
        Tab::Transcribe => {
            painter.circle_stroke(center + Vec2::new(0.0, -3.0), 4.0, stroke);
            painter.line_segment(
                [
                    center + Vec2::new(-6.0, -2.0),
                    center + Vec2::new(-6.0, 0.0),
                ],
                stroke,
            );
            painter.line_segment(
                [center + Vec2::new(6.0, -2.0), center + Vec2::new(6.0, 0.0)],
                stroke,
            );
            painter.add(egui::Shape::line(
                vec![
                    center + Vec2::new(-6.0, 0.0),
                    center + Vec2::new(-4.0, 4.0),
                    center + Vec2::new(0.0, 5.5),
                    center + Vec2::new(4.0, 4.0),
                    center + Vec2::new(6.0, 0.0),
                ],
                stroke,
            ));
            painter.line_segment(
                [center + Vec2::new(0.0, 5.5), center + Vec2::new(0.0, 8.0)],
                stroke,
            );
            painter.line_segment(
                [center + Vec2::new(-4.0, 8.0), center + Vec2::new(4.0, 8.0)],
                stroke,
            );
        }
        Tab::Models => {
            let points = vec![
                center + Vec2::new(0.0, -8.0),
                center + Vec2::new(7.0, -4.0),
                center + Vec2::new(7.0, 4.0),
                center + Vec2::new(0.0, 8.0),
                center + Vec2::new(-7.0, 4.0),
                center + Vec2::new(-7.0, -4.0),
            ];
            painter.add(egui::Shape::closed_line(points, stroke));
            painter.line_segment([center + Vec2::new(-7.0, -4.0), center], stroke);
            painter.line_segment([center, center + Vec2::new(7.0, -4.0)], stroke);
            painter.line_segment([center, center + Vec2::new(0.0, 8.0)], stroke);
        }
        Tab::Playground => {
            painter.line_segment(
                [
                    center + Vec2::new(-3.5, -8.0),
                    center + Vec2::new(3.5, -8.0),
                ],
                stroke,
            );
            painter.add(egui::Shape::closed_line(
                vec![
                    center + Vec2::new(-2.5, -8.0),
                    center + Vec2::new(-2.5, -2.0),
                    center + Vec2::new(-7.0, 7.0),
                    center + Vec2::new(7.0, 7.0),
                    center + Vec2::new(2.5, -2.0),
                    center + Vec2::new(2.5, -8.0),
                ],
                stroke,
            ));
            painter.line_segment(
                [center + Vec2::new(-5.0, 3.0), center + Vec2::new(5.0, 3.0)],
                stroke,
            );
        }
        Tab::Settings => {
            for (y, knob_x) in [(-6.0, -3.0), (0.0, 4.0), (6.0, -1.0)] {
                painter.line_segment(
                    [center + Vec2::new(-8.0, y), center + Vec2::new(8.0, y)],
                    stroke,
                );
                painter.circle_filled(center + Vec2::new(knob_x, y), 2.4, color);
            }
        }
    }
}

fn record_button<'a>(ui: &Ui, listening: bool) -> FocusableButton<'a> {
    let colors = ui_palette(ui);
    let label = if listening { "STOP" } else { "REC" };
    FocusableButton {
        button: Button::new(
            RichText::new(label)
                .size(11.0)
                .color(colors.primary_button_text)
                .strong(),
        )
        .fill(colors.primary_button_bg)
        .stroke(Stroke::new(1.0, colors.primary_button_bg))
        .rounding(Rounding::same(28.0))
        .min_size(Vec2::splat(56.0)),
        rounding: 28.0,
    }
}

fn paint_record_motion(ui: &Ui, response: &egui::Response, listening: bool, reduced_motion: bool) {
    if !response.enabled() || !ui.is_rect_visible(response.rect) {
        return;
    }

    let state_progress = record_motion_progress(
        ui.ctx(),
        response.id.with("recording-state"),
        listening,
        reduced_motion,
        RECORD_STATE_MOTION_SECONDS,
    );
    let hover_progress = record_motion_progress(
        ui.ctx(),
        response.id.with("hover"),
        response.hovered(),
        reduced_motion,
        RECORD_HOVER_MOTION_SECONDS,
    );
    let press_progress = record_motion_progress(
        ui.ctx(),
        response.id.with("press"),
        response.is_pointer_button_down_on(),
        reduced_motion,
        RECORD_PRESS_MOTION_SECONDS,
    );

    let colors = ui_palette(ui);
    let hover = ease_out_cubic(hover_progress);
    let press = ease_out_cubic(press_progress);
    let base_radius = response.rect.width().min(response.rect.height()) * 0.5;

    if hover > 0.0 || press > 0.0 {
        ui.painter().circle_stroke(
            response.rect.center(),
            base_radius - press * 1.25,
            Stroke::new(
                1.0 + press,
                colors
                    .accent
                    .gamma_multiply((0.22 + hover * 0.38).clamp(0.0, 1.0)),
            ),
        );
    }

    let pulse = bounded_transition_pulse(state_progress);
    if pulse > 0.0 {
        ui.painter().circle_stroke(
            response.rect.center(),
            base_radius + 4.0 + pulse * 2.0,
            Stroke::new(
                1.0 + pulse * 0.75,
                colors.accent.gamma_multiply(pulse * 0.42),
            ),
        );
    }
}

fn record_motion_progress(
    ctx: &egui::Context,
    id: egui::Id,
    target: bool,
    reduced_motion: bool,
    duration_seconds: f32,
) -> f32 {
    if reduced_motion {
        if target { 1.0 } else { 0.0 }
    } else {
        ctx.animate_bool_with_time(id, target, duration_seconds)
    }
}

fn ease_out_cubic(value: f32) -> f32 {
    let value = value.clamp(0.0, 1.0);
    1.0 - (1.0 - value).powi(3)
}

fn bounded_transition_pulse(progress: f32) -> f32 {
    let progress = progress.clamp(0.0, 1.0);
    4.0 * progress * (1.0 - progress)
}

fn primary_small_button<'a>(ui: &Ui, label: &'a str) -> FocusableButton<'a> {
    let colors = ui_palette(ui);
    FocusableButton {
        button: Button::new(
            RichText::new(label)
                .color(colors.primary_button_text)
                .strong(),
        )
        .fill(colors.primary_button_bg)
        .stroke(Stroke::new(1.0, colors.primary_button_bg))
        .rounding(Rounding::same(8.0))
        .wrap(true)
        .min_size(Vec2::new(72.0, 40.0)),
        rounding: 8.0,
    }
}

fn small_button<'a>(ui: &Ui, label: &'a str) -> FocusableButton<'a> {
    let colors = ui_palette(ui);
    FocusableButton {
        button: Button::new(RichText::new(label).color(colors.text))
            .fill(colors.card_bg)
            .stroke(Stroke::new(1.0, colors.border_strong))
            .rounding(Rounding::same(8.0))
            .wrap(true)
            .min_size(Vec2::new(68.0, 36.0)),
        rounding: 8.0,
    }
}

fn add_enabled_button<'a>(
    ui: &mut Ui,
    enabled: bool,
    button: FocusableButton<'a>,
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

fn ready_dot(ui: &mut Ui, status: TranscriptionStatus) {
    status_badge(ui, status);
}

fn badge(ui: &mut Ui, label: &str, tone: ChipTone) {
    let (text, fill, stroke) = chip_colors(ui, tone);
    Frame::none()
        .fill(fill)
        .stroke(stroke)
        .rounding(Rounding::same(4.0))
        .inner_margin(Margin::symmetric(8.0, 4.0))
        .show(ui, |ui| {
            ui.add(
                egui::Label::new(RichText::new(label).size(12.0).color(text).strong()).wrap(true),
            );
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

fn format_recording_duration(seconds: u32) -> String {
    let minutes = recording_duration_minutes(seconds);
    let mut value = format!("{minutes:.2}");
    while value.ends_with('0') {
        value.pop();
    }
    if value.ends_with('.') {
        value.pop();
    }
    format!("{value} minute{}", if seconds == 60 { "" } else { "s" })
}

fn recording_duration_minutes(seconds: u32) -> f64 {
    seconds as f64 / 60.0
}

fn recording_duration_seconds(minutes: f64) -> u32 {
    (minutes.clamp(0.5, 120.0) * 60.0).round() as u32
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
            "Download a local whisper.cpp model before transcribing.".to_owned()
        }
        ModelRuntimeStatus::Downloading => "The selected model is still downloading.".to_owned(),
        ModelRuntimeStatus::Running => "A transcription is already running.".to_owned(),
        ModelRuntimeStatus::NotImplemented => {
            "This backend runtime is not bundled yet; choose a whisper.cpp model.".to_owned()
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
    visuals.selection.stroke = Stroke::new(2.0, colors.accent);
    visuals.hyperlink_color = colors.accent;
    visuals.panel_fill = colors.content_bg;
    visuals.window_fill = colors.card_bg;
    visuals.extreme_bg_color = colors.panel_bg;
    visuals.widgets.noninteractive.bg_fill = colors.card_bg;
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, colors.border);
    visuals.widgets.inactive.bg_fill = colors.card_bg;
    visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, colors.border);
    visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, colors.border_strong);
    visuals.widgets.active.bg_stroke = Stroke::new(2.0, colors.accent);
    visuals.window_rounding = Rounding::same(12.0);
    visuals.window_stroke = Stroke::new(1.0, colors.border_strong);
    visuals.window_shadow = egui::epaint::Shadow {
        offset: Vec2::new(0.0, 3.0),
        blur: 12.0,
        spread: 0.0,
        color: Color32::from_black_alpha(if visuals.dark_mode { 96 } else { 38 }),
    };
    visuals.popup_shadow = visuals.window_shadow;
    visuals
}

fn model_install_detail(
    model: &SttModelInfo,
    install_status: &ModelInstallStatus,
) -> Option<String> {
    let base = format!("Model storage: {}", model_storage_estimate(model));
    match install_status {
        ModelInstallStatus::NotInstalled if !supports_managed_install(model) => {
            Some(format!("{base} | Installer unavailable in this build."))
        }
        ModelInstallStatus::Downloading { .. } => {
            Some(format!("{base} | {}", install_status.label()))
        }
        ModelInstallStatus::InstallingRuntime => {
            Some(format!("{base} | {}", install_status.label()))
        }
        ModelInstallStatus::Missing => Some(format!(
            "{base} | The configured model path is missing or incomplete. Reinstall to use this model."
        )),
        ModelInstallStatus::Error(message) => Some(format!("{base} | Install failed: {message}")),
        ModelInstallStatus::RuntimeError(message) => {
            Some(format!("{base} | Runtime repair failed: {message}"))
        }
        ModelInstallStatus::NotInstalled | ModelInstallStatus::Installed => Some(base),
    }
}

fn runtime_representative_models(config: &AppConfig) -> Vec<SttModelInfo> {
    let mut seen_backends = Vec::new();
    config::configured_models(config)
        .into_iter()
        .filter(|model| {
            if seen_backends
                .iter()
                .any(|backend: &String| backend == &model.backend)
            {
                false
            } else {
                seen_backends.push(model.backend.clone());
                true
            }
        })
        .collect()
}

fn runtime_detail_text(
    config: &AppConfig,
    model: &SttModelInfo,
    status: &ModelRuntimeStatus,
) -> String {
    let used_by = runtime_model_summary(config, &model.backend);
    let storage = runtime_storage_detail(&model.backend);
    let status_detail = match status {
        ModelRuntimeStatus::Ready => {
            format!("{} models can use this local runtime.", model.backend)
        }
        ModelRuntimeStatus::MissingConfiguration => format!(
            "{} models are installed separately, but this runtime is not configured.",
            model.backend
        ),
        ModelRuntimeStatus::NotImplemented => format!(
            "{} models are listed for comparison, but their managed runtime is not bundled yet.",
            model.backend
        ),
        ModelRuntimeStatus::Error(message) => message.clone(),
        _ => setup_message_for_status(status),
    };
    let version = runtime_version_detail(config, model)
        .map(|detail| format!(" {detail}"))
        .unwrap_or_default();
    format!("Used by: {used_by}. Runtime storage: {storage}. {status_detail}{version}")
}

fn runtime_model_summary(config: &AppConfig, backend: &str) -> String {
    let models = config::configured_models(config)
        .into_iter()
        .filter(|model| model.backend == backend)
        .map(|model| model.name)
        .collect::<Vec<_>>();
    let count = models.len();
    let preview = models.into_iter().take(3).collect::<Vec<_>>().join(", ");
    if count > 3 {
        format!("{preview}, +{} more", count - 3)
    } else if preview.is_empty() {
        "no catalog models".to_owned()
    } else {
        preview
    }
}

fn model_storage_estimate(model: &SttModelInfo) -> &'static str {
    runtime_catalog::model_storage_estimate(&model.id)
}

fn model_device_label(model: &SttModelInfo) -> &'static str {
    runtime_catalog::backend_spec(&model.backend)
        .map(|spec| spec.device_support.label())
        .unwrap_or("Unknown")
}

fn selected_model_device_support(config: &AppConfig) -> runtime_catalog::DeviceSupport {
    config::configured_models(config)
        .into_iter()
        .find(|model| model.id == config.selected_default_model)
        .and_then(|model| runtime_catalog::backend_spec(&model.backend))
        .map(|spec| spec.device_support)
        .unwrap_or(runtime_catalog::DeviceSupport::CpuOnly)
}

fn model_download_total_bytes(model: &SttModelInfo) -> Option<u64> {
    runtime_catalog::model_download_total_bytes(&model.id)
}

fn runtime_storage_estimate(backend: &str) -> &'static str {
    runtime_catalog::backend_spec(backend)
        .map(|spec| spec.runtime_storage_estimate)
        .unwrap_or("varies")
}

fn runtime_storage_detail(backend: &str) -> &'static str {
    runtime_catalog::backend_spec(backend)
        .map(|spec| spec.runtime_storage_detail)
        .unwrap_or("varies")
}

fn runtime_version_badge(config: &AppConfig, model: &SttModelInfo) -> Option<(String, ChipTone)> {
    let provider = stt::provider_for_backend(&model.backend)?;
    match runtime_version_state(config, provider) {
        RuntimeVersionState::NotTracked => None,
        RuntimeVersionState::Current(version) => {
            Some((format!("Version {version}"), ChipTone::Success))
        }
        RuntimeVersionState::UpdateAvailable { installed, .. } if installed.is_some() => Some((
            if runtime_install_source(config, model).is_ok_and(|source| source.is_some()) {
                "Update available"
            } else {
                "Update not staged"
            }
            .to_owned(),
            ChipTone::Warning,
        )),
        RuntimeVersionState::UpdateAvailable { .. } => {
            Some(("Version unknown".to_owned(), ChipTone::Warning))
        }
    }
}

fn runtime_version_detail(config: &AppConfig, model: &SttModelInfo) -> Option<String> {
    let provider = stt::provider_for_backend(&model.backend)?;
    match runtime_version_state(config, provider) {
        RuntimeVersionState::NotTracked => None,
        RuntimeVersionState::Current(version) => Some(format!("Runtime version: {version}.")),
        RuntimeVersionState::UpdateAvailable {
            installed: Some(installed),
            available,
        } => Some(
            if runtime_install_source(config, model).is_ok_and(|source| source.is_some()) {
                format!("Runtime update available: installed {installed}, available {available}.")
            } else {
                format!(
                    "Installed runtime {installed} is usable. This build does not include staged runtime {available} for an explicit update."
                )
            },
        ),
        RuntimeVersionState::UpdateAvailable {
            installed: None,
            available,
        } => Some(
            if runtime_install_source(config, model).is_ok_and(|source| source.is_some()) {
                format!("Runtime version is unknown; update to the staged {available} runtime.")
            } else {
                format!(
                    "Runtime version is unknown. This build does not include staged runtime {available} for an explicit update."
                )
            },
        ),
    }
}

fn uninstall_model_files(config: &AppConfig, model: &SttModelInfo) -> Result<bool, String> {
    let mut removed_any = false;
    for path in uninstall_candidate_paths(config, model) {
        if !path.exists() || !is_app_managed_model_path(config, &path) {
            continue;
        }
        let result = if path.is_dir() {
            fs::remove_dir_all(&path)
        } else {
            fs::remove_file(&path)
        };
        result.map_err(|err| format!("Could not delete {}: {err}", path.display()))?;
        removed_any = true;
    }
    Ok(removed_any)
}

fn uninstall_runtime_transaction(
    config: &mut AppConfig,
    runtime_id: &str,
) -> Result<RuntimeUninstallOutcome, String> {
    let target_root = config::runtime_storage_dir().join(runtime_id);
    uninstall_runtime_transaction_at(config, runtime_id, &target_root, |candidate| {
        config::save_config_with_runtime_update(candidate, Some((runtime_id, None)))
            .map_err(|err| err.to_string())
    })
}

fn uninstall_runtime_transaction_at(
    config: &mut AppConfig,
    runtime_id: &str,
    target_root: &Path,
    persist: impl FnOnce(&AppConfig) -> Result<config::ConfigSaveOutcome, String>,
) -> Result<RuntimeUninstallOutcome, String> {
    let install_lock = acquire_runtime_install_lock(runtime_id, target_root)?;
    let backup_root = runtime_transaction_path(target_root, "backup");
    let removed_files = target_root.exists();
    let previous = if removed_files {
        let mut journal = RuntimeTransactionJournal {
            version: 1,
            runtime_id: runtime_id.to_owned(),
            phase: RuntimeTransactionPhase::Prepared,
            had_previous_runtime: true,
            previous_install: install_lock.previous_install.clone(),
            new_install: None,
        };
        write_runtime_journal(target_root, &journal)?;
        if let Err(err) = durable_fs::rename(target_root, &backup_root, false) {
            return Err(format!(
                "Could not stage runtime removal {}: {err}",
                target_root.display()
            ));
        }
        journal.phase = RuntimeTransactionPhase::BackedUp;
        if let Err(message) = write_runtime_journal(target_root, &journal) {
            return Err(format!("Could not record runtime removal: {message}"));
        }
        Some(backup_root)
    } else {
        None
    };
    let mut removal = RuntimeReplacement {
        installed_path: target_root.to_path_buf(),
        target_root: target_root.to_path_buf(),
        backup_root: previous,
        runtime_id: runtime_id.to_owned(),
        persistence_install: None,
        _lock: install_lock,
    };
    if let Err(message) = removal.prepare_persistence(None) {
        let rollback_error = removal.rollback().err();
        return Err(message + rollback_error.as_deref().unwrap_or_default());
    }
    match persist(config) {
        Ok(committed) => {
            *config = committed.config;
            let durability_warning =
                finalize_runtime_transaction(removal, committed.durability_warning);
            Ok(RuntimeUninstallOutcome {
                removed_files,
                durability_warning,
            })
        }
        Err(err) => {
            let message = format!("Failed to persist runtime removal: {err}");
            match removal.rollback() {
                Ok(()) => Err(message),
                Err(rollback) => Err(format!("{message}. {rollback}")),
            }
        }
    }
}

fn build_development_runtime_package(
    runtime_id: &str,
    backend: &str,
    package: DevelopmentRuntimePackage,
) -> Result<RuntimeReplacement, String> {
    let install_lock = acquire_runtime_install_lock(runtime_id, &package.destination_root)?;
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
    if let Err(message) = build_development_runtime_into(runtime_id, backend, &staged_package) {
        let _ = remove_path_if_exists(&stage_root);
        return Err(message);
    }
    activate_staged_runtime(
        runtime_id,
        &package.destination_root,
        &stage_root,
        &relative_executable,
        install_lock,
    )
}

fn build_development_runtime_into(
    runtime_id: &str,
    backend: &str,
    package: &DevelopmentRuntimePackage,
) -> Result<(), String> {
    if let Some(parent) = package.destination_root.parent() {
        durable_fs::create_dir_all(parent)
            .map_err(|err| format!("Could not create {}: {err}", parent.display()))?;
    }

    let output = Command::new(&package.script)
        .env(package.destination_env, &package.destination_root)
        .output()
        .map_err(|err| format!("Could not run {}: {err}", package.script.display()))?;

    if !output.status.success() {
        return Err(format!(
            "Could not build {} runtime with {}: {}",
            backend,
            package.script.display(),
            command_output_message(&output.stdout, &output.stderr)
        ));
    }

    if !installed_runtime_executable_usable(runtime_id, &package.executable_path) {
        return Err(format!(
            "{} runtime build finished but did not create a usable runtime at {}.",
            backend,
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

fn install_remote_runtime_artifact(
    runtime_id: &str,
    artifact: &RuntimeArtifact,
) -> Result<RuntimeReplacement, String> {
    let target_root = config::runtime_storage_dir().join(runtime_id);
    let install_lock = acquire_runtime_install_lock(runtime_id, &target_root)?;
    let staged = runtime_artifacts::download_and_stage(artifact, &target_root)?;
    if !installed_runtime_executable_usable(runtime_id, &staged.entrypoint) {
        let _ = remove_path_if_exists(&staged.root);
        return Err(format!(
            "Verified runtime artifact did not create a usable entrypoint at {}.",
            staged.entrypoint.display()
        ));
    }
    if let Err(message) = smoke_validate_runtime(&staged.entrypoint) {
        let _ = remove_path_if_exists(&staged.root);
        return Err(message);
    }
    activate_staged_runtime(
        runtime_id,
        &target_root,
        &staged.root,
        &artifact.entrypoint,
        install_lock,
    )
}

fn smoke_validate_runtime(executable: &Path) -> Result<(), String> {
    let mut child = Command::new(executable)
        .arg("--help")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|err| {
            format!(
                "Could not start verified runtime {} for smoke validation: {err}",
                executable.display()
            )
        })?;
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let status = match child.try_wait() {
            Ok(status) => status,
            Err(err) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("Could not poll runtime smoke validation: {err}"));
            }
        };
        if let Some(status) = status {
            return if status.success() {
                Ok(())
            } else {
                Err(format!(
                    "Verified runtime {} failed its --help smoke validation with status {status}.",
                    executable.display()
                ))
            };
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!(
                "Verified runtime {} timed out during --help smoke validation.",
                executable.display()
            ));
        }
        thread::sleep(Duration::from_millis(25));
    }
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

    let install_lock = acquire_runtime_install_lock(runtime_id, target_root)?;
    let stage_root = runtime_transaction_path(target_root, "installing");
    validate_runtime_copy_paths(&source_root, target_root, &stage_root)?;
    remove_path_if_exists(&stage_root)?;
    if let Err(message) = copy_dir_all(&source_root, &stage_root) {
        let _ = remove_path_if_exists(&stage_root);
        return Err(message);
    }
    let staged_executable = stage_root.join(&relative_executable);
    if !installed_runtime_executable_usable(runtime_id, &staged_executable) {
        let _ = remove_path_if_exists(&stage_root);
        return Err(format!(
            "Runtime install did not create a usable runtime at {}.",
            staged_executable.display()
        ));
    }
    activate_staged_runtime(
        runtime_id,
        target_root,
        &stage_root,
        &relative_executable,
        install_lock,
    )
}

fn activate_staged_runtime(
    runtime_id: &str,
    target_root: &Path,
    stage_root: &Path,
    relative_executable: &Path,
    install_lock: RuntimeInstallLock,
) -> Result<RuntimeReplacement, String> {
    durable_fs::sync_tree(stage_root).map_err(|err| {
        format!(
            "Could not make staged runtime {} durable before activation: {err}",
            stage_root.display()
        )
    })?;
    let parent = target_root
        .parent()
        .ok_or_else(|| format!("Runtime target {} has no parent.", target_root.display()))?;
    durable_fs::create_dir_all(parent)
        .map_err(|err| format!("Could not create {}: {err}", parent.display()))?;

    let backup_root = runtime_transaction_path(target_root, "backup");
    remove_path_if_exists(&backup_root)?;
    let mut journal = RuntimeTransactionJournal {
        version: 1,
        runtime_id: runtime_id.to_owned(),
        phase: RuntimeTransactionPhase::Prepared,
        had_previous_runtime: target_root.exists(),
        previous_install: install_lock.previous_install.clone(),
        new_install: None,
    };
    write_runtime_journal(target_root, &journal)?;
    let previous = if target_root.exists() {
        if let Err(err) = durable_fs::rename(target_root, &backup_root, false) {
            return Err(format!(
                "Could not preserve existing runtime {}: {err}",
                target_root.display()
            ));
        }
        Some(backup_root.clone())
    } else {
        None
    };
    journal.phase = RuntimeTransactionPhase::BackedUp;
    if let Err(message) = write_runtime_journal(target_root, &journal) {
        return Err(format!(
            "Could not record the prepared runtime transaction: {message}"
        ));
    }

    if let Err(err) = durable_fs::rename(stage_root, target_root, false) {
        return Err(format!(
            "Could not activate staged runtime {}: {err}",
            stage_root.display()
        ));
    }
    journal.phase = RuntimeTransactionPhase::Activated;
    if let Err(message) = write_runtime_journal(target_root, &journal) {
        return Err(format!(
            "Could not record the activated runtime transaction: {message}"
        ));
    }

    Ok(RuntimeReplacement {
        installed_path: target_root.join(relative_executable),
        target_root: target_root.to_path_buf(),
        backup_root: previous,
        runtime_id: runtime_id.to_owned(),
        persistence_install: None,
        _lock: install_lock,
    })
}

impl RuntimeReplacement {
    fn prepare_persistence(
        &mut self,
        new_install: Option<&config::ManagedRuntimeInstall>,
    ) -> Result<(), String> {
        write_runtime_journal(
            &self.target_root,
            &RuntimeTransactionJournal {
                version: 1,
                runtime_id: self.runtime_id.clone(),
                phase: RuntimeTransactionPhase::AwaitingPersistence,
                had_previous_runtime: self.backup_root.is_some(),
                previous_install: self._lock.previous_install.clone(),
                new_install: new_install.cloned(),
            },
        )?;
        self.persistence_install = Some(new_install.cloned());
        Ok(())
    }

    fn commit(self) -> Result<(), String> {
        match self.persistence_install.as_ref() {
            Some(Some(install)) => {
                if install.path != self.installed_path
                    || !installed_runtime_executable_usable(&self.runtime_id, &install.path)
                {
                    return Err(format!(
                        "Refusing to finalize an invalid {} runtime at {}.",
                        self.runtime_id,
                        install.path.display()
                    ));
                }
            }
            Some(None) if self.target_root.exists() => {
                return Err(format!(
                    "Refusing to finalize {} runtime removal while {} still exists.",
                    self.runtime_id,
                    self.target_root.display()
                ));
            }
            None if !installed_runtime_executable_usable(
                &self.runtime_id,
                &self.installed_path,
            ) =>
            {
                return Err(format!(
                    "Refusing to finalize an invalid {} runtime at {}.",
                    self.runtime_id,
                    self.installed_path.display()
                ));
            }
            _ => {}
        }
        if let Some(backup_root) = self.backup_root {
            remove_path_if_exists(&backup_root)?;
        }
        remove_runtime_journal(&self.target_root)
    }

    fn rollback(self) -> Result<(), String> {
        restore_runtime_backup(
            &self.runtime_id,
            &self.target_root,
            self.backup_root.as_deref(),
            self._lock.previous_install.as_ref(),
        )?;
        remove_runtime_journal(&self.target_root)
    }
}

fn finalize_runtime_transaction(
    replacement: RuntimeReplacement,
    config_durability_warning: Option<String>,
) -> Option<String> {
    match config_durability_warning {
        Some(warning) => Some(warning),
        None => replacement.commit().err(),
    }
}

fn acquire_runtime_install_lock(
    runtime_id: &str,
    target_root: &Path,
) -> Result<RuntimeInstallLock, String> {
    #[cfg(test)]
    if !target_root.starts_with(config::runtime_storage_dir()) {
        return acquire_runtime_install_lock_with_timeout(
            runtime_id,
            target_root,
            None,
            Duration::from_secs(10),
        );
    }
    let file = lock_runtime_install(target_root, Duration::from_secs(10), runtime_id)?;
    let (mut persisted, _) = config::load_config()
        .map_err(|err| format!("Could not load configuration for runtime recovery: {err}"))?;
    config::normalize_config(&mut persisted);
    let current_install = persisted
        .managed_runtimes
        .get(runtime_id)
        .filter(|install| install.path.starts_with(target_root))
        .cloned();
    recover_runtime_transaction(runtime_id, target_root, current_install.as_ref())?;
    Ok(RuntimeInstallLock {
        _file: file,
        previous_install: current_install,
    })
}

#[cfg(test)]
fn acquire_runtime_install_lock_with_timeout(
    runtime_id: &str,
    target_root: &Path,
    current_install: Option<&config::ManagedRuntimeInstall>,
    timeout: Duration,
) -> Result<RuntimeInstallLock, String> {
    let file = lock_runtime_install(target_root, timeout, runtime_id)?;
    recover_runtime_transaction(runtime_id, target_root, current_install)?;
    Ok(RuntimeInstallLock {
        _file: file,
        previous_install: current_install.cloned(),
    })
}

fn lock_runtime_install(
    target_root: &Path,
    timeout: Duration,
    runtime_id: &str,
) -> Result<File, String> {
    let parent = target_root
        .parent()
        .ok_or_else(|| format!("Runtime target {} has no parent.", target_root.display()))?;
    durable_fs::create_dir_all(parent)
        .map_err(|err| format!("Could not create {}: {err}", parent.display()))?;
    let lock_path = runtime_transaction_path(target_root, "lock");
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(|err| format!("Could not open runtime lock {}: {err}", lock_path.display()))?;
    let deadline = Instant::now() + timeout;
    loop {
        match file.try_lock() {
            Ok(()) => break,
            Err(TryLockError::WouldBlock) if Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(50));
            }
            Err(TryLockError::WouldBlock) => {
                return Err(format!(
                    "Another Scribe process is installing the {runtime_id} runtime."
                ));
            }
            Err(TryLockError::Error(err)) => {
                return Err(format!(
                    "Could not lock runtime transaction {}: {err}",
                    lock_path.display()
                ));
            }
        }
    }
    Ok(file)
}

fn recover_managed_runtime_transactions(config: &mut AppConfig) -> Result<(), String> {
    let storage = config::runtime_storage_dir();
    let mut errors = Vec::new();
    for spec in runtime_catalog::backend_specs() {
        let target = storage.join(spec.runtime_id);
        if !runtime_recovery_needed(&target) {
            continue;
        }
        if let Err(message) = acquire_runtime_install_lock(spec.runtime_id, &target) {
            errors.push(format!("{}: {message}", spec.runtime_id));
        }
    }
    if errors.is_empty() {
        let (mut persisted, _) = config::load_config()
            .map_err(|err| format!("Could not reload configuration after recovery: {err}"))?;
        config::normalize_config(&mut persisted);
        *config = persisted;
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

fn recover_runtime_transaction(
    runtime_id: &str,
    target_root: &Path,
    current_install: Option<&config::ManagedRuntimeInstall>,
) -> Result<(), String> {
    let backup_root = runtime_transaction_path(target_root, "backup");
    if let Some(journal) = read_runtime_journal(runtime_id, target_root)? {
        match journal.phase {
            RuntimeTransactionPhase::AwaitingPersistence => {
                if current_install == journal.new_install.as_ref() {
                    if !runtime_committed_state_is_valid(
                        runtime_id,
                        target_root,
                        journal.new_install.as_ref(),
                    ) {
                        return Err(format!(
                            "Committed {runtime_id} runtime files do not match the transaction journal."
                        ));
                    }
                    remove_path_if_exists(&backup_root)?;
                    remove_runtime_journal(target_root)?;
                } else if current_install == journal.previous_install.as_ref() {
                    recover_runtime_rollback(runtime_id, target_root, &backup_root, &journal)?;
                    remove_runtime_journal(target_root)?;
                } else {
                    return Err(format!(
                        "Runtime transaction metadata for {runtime_id} does not match the persisted configuration."
                    ));
                }
            }
            RuntimeTransactionPhase::Prepared => {
                if backup_root.exists() {
                    restore_runtime_backup(
                        runtime_id,
                        target_root,
                        Some(&backup_root),
                        journal.previous_install.as_ref(),
                    )?;
                } else if journal.had_previous_runtime
                    && (!target_root.exists()
                        || !runtime_backup_is_usable(
                            runtime_id,
                            target_root,
                            target_root,
                            journal.previous_install.as_ref(),
                        ))
                {
                    return Err(format!(
                        "The previous {runtime_id} runtime is missing during recovery."
                    ));
                }
                remove_runtime_journal(target_root)?;
            }
            RuntimeTransactionPhase::BackedUp | RuntimeTransactionPhase::Activated => {
                recover_runtime_rollback(runtime_id, target_root, &backup_root, &journal)?;
                remove_runtime_journal(target_root)?;
            }
        }
    } else if backup_root.exists() {
        return Err(format!(
            "Found an unjournaled {runtime_id} runtime backup at {}; preserving it for manual recovery.",
            backup_root.display()
        ));
    }
    remove_path_if_exists(&runtime_transaction_path(target_root, "installing"))?;
    remove_path_if_exists(
        &runtime_transaction_path(target_root, "download").with_extension("zip.partial"),
    )?;
    Ok(())
}

fn runtime_committed_state_is_valid(
    runtime_id: &str,
    target_root: &Path,
    install: Option<&config::ManagedRuntimeInstall>,
) -> bool {
    match install {
        Some(install) => {
            install.path.starts_with(target_root)
                && installed_runtime_executable_usable(runtime_id, &install.path)
        }
        None => !target_root.exists(),
    }
}

fn recover_runtime_rollback(
    runtime_id: &str,
    target_root: &Path,
    backup_root: &Path,
    journal: &RuntimeTransactionJournal,
) -> Result<(), String> {
    if backup_root.exists() {
        return restore_runtime_backup(
            runtime_id,
            target_root,
            Some(backup_root),
            journal.previous_install.as_ref(),
        );
    }
    if journal.had_previous_runtime {
        if target_root.exists()
            && runtime_backup_is_usable(
                runtime_id,
                target_root,
                target_root,
                journal.previous_install.as_ref(),
            )
        {
            return Ok(());
        }
        return Err(format!(
            "The previous {runtime_id} runtime backup is missing or invalid."
        ));
    }
    remove_path_if_exists(target_root)
}

fn restore_runtime_backup(
    runtime_id: &str,
    target_root: &Path,
    backup_root: Option<&Path>,
    previous_install: Option<&config::ManagedRuntimeInstall>,
) -> Result<(), String> {
    if let Some(backup_root) = backup_root {
        if !runtime_backup_is_usable(runtime_id, target_root, backup_root, previous_install) {
            return Err(format!(
                "Refusing to restore an invalid {runtime_id} runtime backup at {}.",
                backup_root.display()
            ));
        }
        remove_path_if_exists(target_root)?;
        durable_fs::rename(backup_root, target_root, false).map_err(|err| {
            format!(
                "Could not restore previous runtime {}: {err}",
                target_root.display()
            )
        })?;
    } else {
        remove_path_if_exists(target_root)?;
    }
    Ok(())
}

fn runtime_backup_is_usable(
    runtime_id: &str,
    target_root: &Path,
    backup_root: &Path,
    previous_install: Option<&config::ManagedRuntimeInstall>,
) -> bool {
    if let Some(relative) =
        previous_install.and_then(|install| install.path.strip_prefix(target_root).ok())
    {
        return installed_runtime_executable_usable(runtime_id, &backup_root.join(relative));
    }
    let Some(spec) = runtime_catalog::backend_spec_for_runtime_id(runtime_id) else {
        return false;
    };
    let Some(runtime) = spec.development_runtime else {
        return backup_root.is_dir();
    };
    let candidate = backup_root.join(runtime.executable_relative_path);
    if installed_runtime_executable_usable(runtime_id, &candidate) {
        return true;
    }
    #[cfg(windows)]
    for extension in ["exe", "bat"] {
        if installed_runtime_executable_usable(runtime_id, &candidate.with_extension(extension)) {
            return true;
        }
    }
    false
}

fn runtime_recovery_needed(target_root: &Path) -> bool {
    runtime_transaction_path(target_root, "transaction").exists()
        || runtime_transaction_path(target_root, "transaction.next").exists()
        || runtime_transaction_path(target_root, "backup").exists()
        || runtime_transaction_path(target_root, "installing").exists()
        || runtime_transaction_path(target_root, "download")
            .with_extension("zip.partial")
            .exists()
}

fn read_runtime_journal(
    runtime_id: &str,
    target_root: &Path,
) -> Result<Option<RuntimeTransactionJournal>, String> {
    let next = runtime_transaction_path(target_root, "transaction.next");
    let current = runtime_transaction_path(target_root, "transaction");
    if next.exists() {
        match parse_runtime_journal(&next) {
            Ok(journal) => {
                validate_runtime_journal_identity(runtime_id, &next, &journal)?;
                return Ok(Some(journal));
            }
            Err(next_error) if current.exists() => {
                return parse_runtime_journal(&current)
                    .and_then(|journal| {
                        validate_runtime_journal_identity(runtime_id, &current, &journal)?;
                        Ok(journal)
                    })
                    .map(Some)
                    .map_err(|current_error| format!("{next_error} {current_error}"));
            }
            Err(next_error) => return Err(next_error),
        }
    }
    if !current.exists() {
        return Ok(None);
    }
    let journal = parse_runtime_journal(&current)?;
    validate_runtime_journal_identity(runtime_id, &current, &journal)?;
    Ok(Some(journal))
}

fn parse_runtime_journal(path: &Path) -> Result<RuntimeTransactionJournal, String> {
    let contents = fs::read_to_string(path).map_err(|err| {
        format!(
            "Could not read runtime transaction {}: {err}",
            path.display()
        )
    })?;
    let journal: RuntimeTransactionJournal = serde_json::from_str(&contents)
        .map_err(|err| format!("Runtime transaction {} is invalid: {err}", path.display()))?;
    Ok(journal)
}

fn validate_runtime_journal_identity(
    runtime_id: &str,
    path: &Path,
    journal: &RuntimeTransactionJournal,
) -> Result<(), String> {
    if journal.version != 1 || journal.runtime_id != runtime_id {
        return Err(format!(
            "Runtime transaction {} has an unexpected identity.",
            path.display()
        ));
    }
    Ok(())
}

fn write_runtime_journal(
    target_root: &Path,
    journal: &RuntimeTransactionJournal,
) -> Result<(), String> {
    let path = runtime_transaction_path(target_root, "transaction");
    let next = runtime_transaction_path(target_root, "transaction.next");
    remove_path_if_exists(&next)?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&next)
        .map_err(|err| {
            format!(
                "Could not create runtime transaction {}: {err}",
                next.display()
            )
        })?;
    serde_json::to_writer(&mut file, journal)
        .map_err(|err| format!("Could not serialize runtime transaction: {err}"))?;
    file.write_all(b"\n")
        .map_err(|err| format!("Could not write runtime transaction: {err}"))?;
    file.sync_all()
        .map_err(|err| format!("Could not sync runtime transaction: {err}"))?;
    drop(file);
    durable_fs::rename(&next, &path, true).map_err(|err| {
        format!(
            "Could not publish runtime transaction {}: {err}",
            path.display()
        )
    })
}

fn remove_runtime_journal(target_root: &Path) -> Result<(), String> {
    remove_path_if_exists(&runtime_transaction_path(target_root, "transaction.next"))?;
    remove_path_if_exists(&runtime_transaction_path(target_root, "transaction"))
}

fn runtime_transaction_path(target_root: &Path, phase: &str) -> PathBuf {
    let name = target_root
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("runtime");
    target_root.with_file_name(format!(".{name}.{phase}"))
}

fn remove_path_if_exists(path: &Path) -> Result<(), String> {
    durable_fs::remove(path).map_err(|err| format!("Could not remove {}: {err}", path.display()))
}

fn managed_runtime_install_record(
    path: PathBuf,
    source: &str,
    artifact: Option<&RuntimeArtifact>,
) -> config::ManagedRuntimeInstall {
    let mut install = config::ManagedRuntimeInstall::app_managed(path.clone(), source);
    if let Some(artifact) = artifact {
        install.source = Some(artifact.url.clone());
        install.version = Some(artifact.version.clone());
        install.sha256 = Some(artifact.sha256.clone());
        install.platform = Some(format!("{}-{}", artifact.os, artifact.arch));
        install.device = Some(artifact.device.as_str().to_owned());
        return install;
    }
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
        install.platform = metadata
            .platform
            .map(|platform| platform.trim().to_owned())
            .filter(|platform| !platform.is_empty())
            .or(install.platform);
        install.device = metadata
            .device
            .map(|device| device.trim().to_ascii_lowercase())
            .filter(|device| matches!(device.as_str(), "cpu" | "gpu"))
            .or_else(|| {
                metadata
                    .cuda_bundled
                    .map(|cuda| if cuda { "gpu" } else { "cpu" }.to_owned())
            });
    }
    if install.device.is_none() {
        install.device = Some("cpu".to_owned());
    }
    install
}

fn runtime_manifest_metadata(executable: &Path) -> Option<RuntimeManifestMetadata> {
    let manifest = runtime_package_root(executable)?.join("runtime-manifest.json");
    let contents = fs::read_to_string(manifest).ok()?;
    serde_json::from_str(&contents).ok()
}

fn installed_runtime_executable_usable(runtime_id: &str, executable: &Path) -> bool {
    match runtime_id {
        "faster_whisper" => stt::faster_whisper::is_faster_whisper_runtime_usable(executable),
        "vosk" => stt::vosk::is_vosk_runtime_usable(executable),
        "sherpa_onnx" | "moonshine" | "parakeet" => {
            stt::sherpa_onnx::is_sherpa_family_runtime_usable(runtime_id, executable)
        }
        _ => executable.exists(),
    }
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

#[cfg(test)]
fn runtime_uninstall_target(
    storage_dir: &Path,
    runtime_id: &str,
    installed_path: &Path,
) -> Option<PathBuf> {
    if runtime_id.is_empty() || installed_path.as_os_str().is_empty() {
        return None;
    }

    let runtime_dir = storage_dir.join(runtime_id);
    if installed_path.starts_with(&runtime_dir) {
        Some(runtime_dir)
    } else if installed_path.starts_with(storage_dir) {
        Some(installed_path.to_path_buf())
    } else {
        None
    }
}

fn uninstall_candidate_paths(config: &AppConfig, model: &SttModelInfo) -> Vec<PathBuf> {
    let mut paths = [
        config::managed_model_path(config, model),
        config::downloaded_model_path(config, model),
        model.local_path.clone(),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>();
    dedup_paths(&mut paths);
    paths
}

fn is_app_managed_model_path(config: &AppConfig, path: &Path) -> bool {
    path.starts_with(config::model_storage_dir(config))
}

fn dedup_paths(paths: &mut Vec<PathBuf>) {
    let mut seen = Vec::new();
    paths.retain(|path| {
        if seen.iter().any(|seen_path| seen_path == path) {
            false
        } else {
            seen.push(path.clone());
            true
        }
    });
}

fn select_first_installed_model(config: &mut AppConfig) {
    config.selected_default_model = config::configured_models(config)
        .into_iter()
        .find(|model| model.install_status.is_runnable())
        .map(|model| model.id)
        .unwrap_or_default();
}

fn set_model_selected(config: &mut AppConfig, model_id: &str, selected: bool) {
    if selected {
        if !config
            .playground_selected_models
            .iter()
            .any(|id| id == model_id)
        {
            config.playground_selected_models.push(model_id.to_owned());
        }
    } else {
        config
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
    config.playground_selected_models = draft
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

fn cards_from_config(config: &AppConfig) -> Vec<PlaygroundCardState> {
    config::playground_selected_installed_models(config)
        .into_iter()
        .map(|model| {
            let status = runtime_status_for_model(config, &model);
            PlaygroundCardState {
                model,
                status,
                transcript: String::new(),
                latency_ms: None,
                audio_duration_ms: None,
                peak_ram_mb: None,
                peak_vram_mb: None,
            }
        })
        .collect()
}

fn runtime_status_for_model(config: &AppConfig, model: &SttModelInfo) -> ModelRuntimeStatus {
    let Some(provider) = stt::provider_for_backend(&model.backend) else {
        return ModelRuntimeStatus::Error(format!("unsupported STT backend: {}", model.backend));
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

fn should_hide_on_close(
    close_requested: bool,
    close_to_tray: bool,
    tray_available: bool,
    quit_requested: bool,
) -> bool {
    close_requested && close_to_tray && tray_available && !quit_requested
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
        egui::Key::Insert => "Insert",
        egui::Key::Delete => "Delete",
        egui::Key::Home => "Home",
        egui::Key::End => "End",
        egui::Key::PageUp => "PageUp",
        egui::Key::PageDown => "PageDown",
        egui::Key::ArrowUp => "Up",
        egui::Key::ArrowDown => "Down",
        egui::Key::ArrowLeft => "Left",
        egui::Key::ArrowRight => "Right",
        egui::Key::Colon | egui::Key::Semicolon => "Semicolon",
        egui::Key::Comma => "Comma",
        egui::Key::Backslash | egui::Key::Pipe => "Backslash",
        egui::Key::Slash | egui::Key::Questionmark => "Slash",
        egui::Key::OpenBracket => "BracketLeft",
        egui::Key::CloseBracket => "BracketRight",
        egui::Key::Backtick => "Backquote",
        egui::Key::Minus => "Minus",
        egui::Key::Period => "Period",
        egui::Key::Plus | egui::Key::Equals => "Equal",
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
        egui::Key::F13 => "F13",
        egui::Key::F14 => "F14",
        egui::Key::F15 => "F15",
        egui::Key::F16 => "F16",
        egui::Key::F17 => "F17",
        egui::Key::F18 => "F18",
        egui::Key::F19 => "F19",
        egui::Key::F20 => "F20",
        egui::Key::F21 => "F21",
        egui::Key::F22 => "F22",
        egui::Key::F23 => "F23",
        egui::Key::F24 => "F24",
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

    #[test]
    fn recording_duration_labels_are_human_readable() {
        assert_eq!(format_recording_duration(1), "0.02 minutes");
        assert_eq!(format_recording_duration(30), "0.5 minutes");
        assert_eq!(format_recording_duration(60), "1 minute");
        assert_eq!(format_recording_duration(125), "2.08 minutes");
        assert_eq!(format_recording_duration(3_600), "60 minutes");
        assert_eq!(format_recording_duration(7_200), "120 minutes");
    }

    #[test]
    fn recording_duration_minutes_round_trip_exact_saved_seconds() {
        for seconds in [30, 60, 125, 600, 7_200] {
            assert_eq!(
                recording_duration_seconds(recording_duration_minutes(seconds)),
                seconds
            );
        }
        assert_eq!(recording_duration_seconds(0.1), 30);
        assert_eq!(recording_duration_seconds(121.0), 7_200);
    }

    #[test]
    fn start_tab_env_parser_accepts_known_tabs() {
        assert_eq!(tab_from_env_value("models"), Some(Tab::Models));
        assert_eq!(
            tab_from_env_value("model playground"),
            Some(Tab::Playground)
        );
        assert_eq!(tab_from_env_value("settings"), Some(Tab::Settings));
        assert_eq!(tab_from_env_value("unknown"), None);
    }

    #[test]
    fn reduced_motion_env_parser_accepts_only_explicit_opt_in_values() {
        for value in ["1", "true", "TRUE", " yes ", "on"] {
            assert!(reduced_motion_enabled_for(Some(value)), "{value}");
        }
        for value in ["0", "false", "no", "off", "", "sometimes"] {
            assert!(!reduced_motion_enabled_for(Some(value)), "{value}");
        }
        assert!(!reduced_motion_enabled_for(None));
    }

    #[test]
    fn record_motion_curves_are_bounded_and_settle_at_their_endpoints() {
        assert_eq!(ease_out_cubic(-1.0), 0.0);
        assert_eq!(ease_out_cubic(0.0), 0.0);
        assert_eq!(ease_out_cubic(1.0), 1.0);
        assert_eq!(ease_out_cubic(2.0), 1.0);
        assert_eq!(bounded_transition_pulse(0.0), 0.0);
        assert_eq!(bounded_transition_pulse(0.5), 1.0);
        assert_eq!(bounded_transition_pulse(1.0), 0.0);

        for step in 0..=100 {
            let progress = step as f32 / 100.0;
            assert!((0.0..=1.0).contains(&ease_out_cubic(progress)));
            assert!((0.0..=1.0).contains(&bounded_transition_pulse(progress)));
        }
    }

    #[test]
    fn model_catalog_row_paints_within_viewport_at_minimum_and_wide_widths() {
        for width in [840.0, 1440.0, 4096.0] {
            let output = render_model_catalog_row(width);
            let max_painted_x = max_visible_painted_x(&output);

            assert!(
                max_painted_x <= width + 1.0,
                "model catalog row painted beyond viewport: max_x={max_painted_x}, width={width}"
            );
        }
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
    fn app_shell_pages_paint_within_viewport_at_minimum_and_wide_widths() {
        for tab in [Tab::Models, Tab::Playground, Tab::Settings] {
            for (width, height) in [
                (840.0, 600.0),
                (1100.0, 760.0),
                (1440.0, 760.0),
                (4096.0, 760.0),
            ] {
                let output = render_app_tab_at_size(tab, width, height);
                let max_painted_x = max_visible_painted_x(&output);

                assert!(
                    max_painted_x <= width + 1.0,
                    "{tab:?} painted beyond viewport: max_x={max_painted_x}, size={width}x{height}"
                );
            }
        }
    }

    #[test]
    fn app_tab_interaction_rects_stay_within_minimum_width() {
        for tab in [Tab::Transcribe, Tab::Models, Tab::Playground, Tab::Settings] {
            let output = render_accessible_app_tab_at_size(tab, 840.0, 600.0);
            let update = output.platform_output.accesskit_update.unwrap();
            for (_, node) in &update.nodes {
                if node.supports_action(egui::accesskit::Action::Focus)
                    && let Some(bounds) = node.bounds()
                {
                    assert!(
                        bounds.x0 >= -1.0 && bounds.x1 <= 841.0,
                        "{tab:?} has an interaction rect outside 840px: {bounds:?}"
                    );
                    assert!(
                        bounds.x1 > bounds.x0,
                        "{tab:?} has an empty interaction rect"
                    );
                }
            }
        }
    }

    #[test]
    fn adversarial_copy_stays_readable_and_in_bounds_across_tabs_and_target_viewports() {
        for tab in [Tab::Transcribe, Tab::Models, Tab::Playground, Tab::Settings] {
            for (width, height) in [(840.0, 600.0), (1100.0, 760.0)] {
                let ctx = egui::Context::default();
                ctx.enable_accesskit();
                configure_stitch_style(&ctx);
                ctx.set_visuals(stitch_visuals(ThemeMode::Light));

                let mut app = adversarial_test_app();
                app.current_tab = tab;
                let output =
                    render_accessible_app_tab_frame(&ctx, &mut app, width, height, Vec::new());

                assert_no_visible_horizontal_overflow(&output, width, tab);
                assert_focusable_bounds_within_viewport(&output, width, tab);
                for sentinel in adversarial_sentinels_for_tab(tab) {
                    assert_accessible_text_contains(&output, &sentinel);
                    assert_target_text_is_not_horizontally_clipped(&output, &sentinel);
                }

                let page_title = match tab {
                    Tab::Transcribe => "Transcribe",
                    Tab::Models => "Models Catalog",
                    Tab::Playground => "Model Playground",
                    Tab::Settings => "Settings",
                };
                let (_, content_height, viewport_height) = page_scroll_metrics(&ctx, page_title);
                assert!(
                    content_height > viewport_height,
                    "{tab:?} long content should remain reachable through the page scroll area at {width}x{height}"
                );

                if tab == Tab::Settings {
                    let microphone =
                        accessible_bounds(&output, egui::accesskit::Role::ComboBox, "Microphone");
                    assert!(
                        microphone.x1 - microphone.x0 >= 240.0,
                        "long device selector was squeezed instead of using readable width: {microphone:?}"
                    );
                }
                if tab == Tab::Playground {
                    assert!(
                        output
                            .platform_output
                            .accesskit_update
                            .as_ref()
                            .is_some_and(|update| update.nodes.iter().any(|(_, node)| {
                                node.role() == egui::accesskit::Role::Button
                                    && node.name().is_some_and(|name| name.starts_with("Move "))
                                    && node.supports_action(egui::accesskit::Action::Focus)
                            })),
                        "Playground move controls must remain keyboard-focusable"
                    );
                }
            }
        }
    }

    #[test]
    fn model_selector_modal_stays_reachable_at_target_viewports_with_adversarial_page_copy() {
        for (width, height) in [(840.0, 600.0), (1100.0, 760.0)] {
            let ctx = egui::Context::default();
            ctx.enable_accesskit();
            configure_stitch_style(&ctx);
            ctx.set_visuals(stitch_visuals(ThemeMode::Light));

            let mut app = adversarial_test_app();
            let selector_root = install_runnable_selector_models(&mut app);
            app.current_tab = Tab::Playground;
            app.open_playground_selector(None);
            let _opening_frame =
                render_accessible_app_tab_frame(&ctx, &mut app, width, height, Vec::new());
            let output = render_accessible_app_tab_frame(&ctx, &mut app, width, height, Vec::new());

            assert_no_visible_horizontal_overflow(&output, width, Tab::Playground);
            let selector_rect = ctx
                .memory(|memory| memory.area_rect(egui::Id::new("Choose models to test")))
                .expect("selector should create a window area");
            assert!(
                selector_rect.min.x >= -1.0
                    && selector_rect.max.x <= width + 1.0
                    && selector_rect.min.y >= -1.0
                    && selector_rect.max.y <= height + 1.0,
                "selector window overflowed at {width}x{height}: {selector_rect:?}"
            );
            assert_focusable_bounds_within_viewport(&output, width, Tab::Playground);
            assert_accessible_text_contains(
                &output,
                "Only installed models can be selected for Playground tests.",
            );
            assert_target_text_is_not_horizontally_clipped(
                &output,
                "Only installed models can be selected for Playground tests.",
            );
            let apply_bounds = accessible_bounds(
                &output,
                egui::accesskit::Role::Button,
                "Apply model selection",
            );
            let cancel_bounds = accessible_bounds(
                &output,
                egui::accesskit::Role::Button,
                "Cancel model selection",
            );
            for (name, bounds) in [
                ("Apply model selection", apply_bounds),
                ("Cancel model selection", cancel_bounds),
            ] {
                assert_accesskit_rect_within_viewport(bounds, width, height, name);
                assert!(
                    bounds.y1 <= f64::from(height),
                    "selector footer {name} is not visible at {width}x{height}: {bounds:?}"
                );
            }
            let (_, selector_content_height, selector_viewport, initial_scroll_offset) =
                selector_scroll_metrics(&ctx);
            assert!(
                selector_content_height > selector_viewport.height(),
                "selector should scroll its runnable model list at {width}x{height}"
            );
            assert_eq!(
                initial_scroll_offset, 0.0,
                "selector should start at the first runnable model"
            );
            let last_model_name = crate::models::default_model_catalog()
                .into_iter()
                .last()
                .expect("model catalog should not be empty")
                .name;
            let _scrolled = render_accessible_app_tab_frame(
                &ctx,
                &mut app,
                width,
                height,
                vec![
                    egui::Event::PointerMoved(selector_viewport.center()),
                    egui::Event::Scroll(egui::vec2(0.0, -10_000.0)),
                ],
            );
            let (_, _, _, scrolled_offset) = selector_scroll_metrics(&ctx);
            assert!(
                scrolled_offset > 0.0,
                "selector did not respond to a downward scroll at {width}x{height}"
            );
            let max_scroll_offset = selector_content_height - selector_viewport.height();
            assert!(
                scrolled_offset >= max_scroll_offset - 1.0,
                "selector did not reach its final model at {width}x{height}: offset={scrolled_offset}, max={max_scroll_offset}"
            );
            let scrolled_output =
                render_accessible_app_tab_frame(&ctx, &mut app, width, height, Vec::new());
            assert_target_text_is_not_horizontally_clipped(&scrolled_output, &last_model_name);
            assert!(
                output
                    .platform_output
                    .accesskit_update
                    .as_ref()
                    .is_some_and(|update| update.nodes.iter().any(|(_, node)| {
                        node.role() == egui::accesskit::Role::Button
                            && node.name() == Some("Apply model selection")
                    })),
                "selector apply button should remain exposed to AccessKit at {width}x{height}"
            );
            let _ = fs::remove_dir_all(selector_root);
        }
    }

    #[test]
    fn opened_dynamic_combo_popups_wrap_option_copy_within_target_viewports() {
        for (width, height) in [(840.0, 600.0), (1100.0, 760.0)] {
            for (id_source, control_name, selected_wraps, option_sentinel) in [
                (
                    "test-long-microphone-popup",
                    "Microphone",
                    true,
                    long_layout_sentinel("microphone-option"),
                ),
                (
                    "test-long-gpu-popup",
                    "GPU device",
                    true,
                    long_layout_sentinel("gpu-option"),
                ),
                (
                    "test-long-backend-popup",
                    "Filter model backend",
                    false,
                    long_layout_sentinel("backend-option"),
                ),
            ] {
                let ctx = egui::Context::default();
                ctx.enable_accesskit();
                configure_stitch_style(&ctx);
                ctx.set_visuals(stitch_visuals(ThemeMode::Light));

                let alternate_option = format!("Alternate test option for {control_name}");
                let options = if selected_wraps {
                    vec![option_sentinel.clone(), alternate_option.clone()]
                } else {
                    vec![
                        "All backends".to_owned(),
                        option_sentinel.clone(),
                        alternate_option.clone(),
                    ]
                };
                let mut selected = if selected_wraps {
                    options[0].clone()
                } else {
                    "All backends".to_owned()
                };
                let _closed = render_test_dynamic_combo_popup(
                    &ctx,
                    id_source,
                    control_name,
                    selected_wraps,
                    &mut selected,
                    &options,
                    dynamic_combo_raw_input(egui::vec2(width, height), Vec::new()),
                );
                let combo_id = test_dynamic_combo_id(&ctx);
                ctx.memory_mut(|memory| {
                    memory.open_popup(combo_id.with("popup"));
                });
                let _opening_frame = render_test_dynamic_combo_popup(
                    &ctx,
                    id_source,
                    control_name,
                    selected_wraps,
                    &mut selected,
                    &options,
                    dynamic_combo_raw_input(egui::vec2(width, height), Vec::new()),
                );
                let opened = render_test_dynamic_combo_popup(
                    &ctx,
                    id_source,
                    control_name,
                    selected_wraps,
                    &mut selected,
                    &options,
                    dynamic_combo_raw_input(egui::vec2(width, height), Vec::new()),
                );

                assert_no_visible_horizontal_overflow(&opened, width, Tab::Settings);
                assert_focusable_bounds_within_viewport(&opened, width, Tab::Settings);
                assert_accessible_text_contains(&opened, &option_sentinel);
                let option_bounds = accessible_bounds(
                    &opened,
                    egui::accesskit::Role::ToggleButton,
                    &option_sentinel,
                );
                assert_accesskit_rect_within_viewport(
                    option_bounds,
                    width,
                    height,
                    &format!("{control_name} option"),
                );
                assert!(
                    option_bounds.y1 - option_bounds.y0 >= 36.0,
                    "{control_name} option lost its minimum target height: {option_bounds:?}"
                );
                assert_target_text_is_not_horizontally_clipped(&opened, &option_sentinel);

                let selected_before_activation = selected.clone();
                assert_accessible_selected_option(&opened, &selected_before_activation);
                let alternate_id = accesskit_control_id(&opened, &alternate_option);
                let _activated = render_test_dynamic_combo_popup(
                    &ctx,
                    id_source,
                    control_name,
                    selected_wraps,
                    &mut selected,
                    &options,
                    dynamic_combo_raw_input(
                        egui::vec2(width, height),
                        vec![egui::Event::AccessKitActionRequest(
                            egui::accesskit::ActionRequest {
                                action: egui::accesskit::Action::Default,
                                target: alternate_id,
                                data: None,
                            },
                        )],
                    ),
                );
                assert_eq!(selected, alternate_option);
                let after_activation = render_test_dynamic_combo_popup(
                    &ctx,
                    id_source,
                    control_name,
                    selected_wraps,
                    &mut selected,
                    &options,
                    dynamic_combo_raw_input(egui::vec2(width, height), Vec::new()),
                );
                assert_accessible_selected_option(&after_activation, &selected);
            }
        }
    }

    #[test]
    fn below_fold_controls_are_scroll_reachable_at_minimum_size() {
        for tab in [Tab::Transcribe, Tab::Models, Tab::Playground, Tab::Settings] {
            let ctx = egui::Context::default();
            ctx.enable_accesskit();
            configure_stitch_style(&ctx);
            ctx.set_visuals(stitch_visuals(ThemeMode::Light));
            let mut app = test_app();
            app.current_tab = tab;

            let initial = render_accessible_app_tab_frame(&ctx, &mut app, 840.0, 600.0, Vec::new());
            let update = initial.platform_output.accesskit_update.as_ref().unwrap();
            let below_fold = update
                .nodes
                .iter()
                .filter(|(_, node)| node.supports_action(egui::accesskit::Action::Focus))
                .filter_map(|(id, node)| node.bounds().map(|bounds| (*id, bounds)))
                .filter(|(_, bounds)| bounds.y0 >= 600.0)
                .max_by(|(_, left), (_, right)| left.y0.total_cmp(&right.y0));

            let (target_id, initial_bounds) = below_fold
                .unwrap_or_else(|| panic!("{tab:?} did not expose a below-fold focusable control"));

            let page_title = match tab {
                Tab::Transcribe => "Transcribe",
                Tab::Models => "Models Catalog",
                Tab::Playground => "Model Playground",
                Tab::Settings => "Settings",
            };
            let scroll_metrics = ctx
                .data_mut(|data| {
                    data.get_temp::<(egui::Id, f32, f32)>(egui::Id::new((
                        "test-page-scroll-metrics",
                        page_title,
                    )))
                })
                .unwrap_or_else(|| panic!("missing {tab:?} page scroll metrics"));
            assert!(
                scroll_metrics.1 > scroll_metrics.2,
                "{tab:?} has a below-fold control without overflowing its page scroll area"
            );
            let mut state = egui::scroll_area::State::load(&ctx, scroll_metrics.0)
                .unwrap_or_else(|| panic!("missing {tab:?} page scroll state"));
            state.offset.y = scroll_metrics.1 - scroll_metrics.2;
            state.store(&ctx, scroll_metrics.0);

            let scrolled =
                render_accessible_app_tab_frame(&ctx, &mut app, 840.0, 600.0, Vec::new());
            let scrolled_bounds = scrolled
                .platform_output
                .accesskit_update
                .as_ref()
                .unwrap()
                .nodes
                .iter()
                .find(|(id, _)| *id == target_id)
                .and_then(|(_, node)| node.bounds())
                .unwrap_or_else(|| panic!("missing scrolled {tab:?} control"));

            assert!(
                scrolled_bounds.y0 < initial_bounds.y0,
                "{tab:?} below-fold control did not move upward: {initial_bounds:?} -> {scrolled_bounds:?}"
            );
            assert!(
                scrolled_bounds.y0 < 600.0 && scrolled_bounds.y1 > 0.0,
                "{tab:?} below-fold control is not reachable after scrolling: {scrolled_bounds:?}"
            );
        }
    }

    #[test]
    fn model_pages_do_not_expand_across_repaints() {
        for tab in [Tab::Models, Tab::Playground] {
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
    fn light_and_dark_glass_surfaces_keep_text_contrast_after_compositing() {
        for palette in [ThemePalette::light(), ThemePalette::dark()] {
            let shell = source_over_color(palette.shell_bg, palette.content_bg);
            let card = source_over_color(palette.card_bg, shell);
            let panel = source_over_color(palette.panel_bg, shell);

            for surface in [card, panel] {
                assert!(contrast_ratio(palette.text, surface) >= 4.5);
                assert!(contrast_ratio(palette.muted_text, surface) >= 4.5);
            }
            assert!(contrast_ratio(palette.primary_button_text, palette.primary_button_bg) >= 4.5);
        }
    }

    #[test]
    fn linear_premultiplied_source_over_matches_known_half_alpha_mix() {
        let foreground = egui::Rgba::from_rgba_unmultiplied(1.0, 0.0, 0.0, 0.5);
        let background = egui::Rgba::from_rgb(0.0, 0.0, 1.0);
        let result = source_over_rgba(foreground, background).to_array();

        for (actual, expected) in result.into_iter().zip([0.5, 0.0, 0.5, 1.0]) {
            assert!((actual - expected).abs() <= f32::EPSILON);
        }
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

    fn source_over_color(foreground: Color32, background: Color32) -> Color32 {
        source_over_rgba(foreground.into(), background.into()).into()
    }

    fn source_over_rgba(foreground: egui::Rgba, background: egui::Rgba) -> egui::Rgba {
        foreground + background.multiply(1.0 - foreground.a())
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
        let active_model = config.selected_default_model.clone();

        set_model_selected(&mut config, &active_model, false);
        config::normalize_config(&mut config);

        assert_eq!(config.selected_default_model, active_model);
        assert!(
            !config
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
    fn close_to_tray_intercepts_only_a_normal_window_close() {
        assert!(should_hide_on_close(true, true, true, false));
        assert!(!should_hide_on_close(false, true, true, false));
        assert!(!should_hide_on_close(true, false, true, false));
        assert!(!should_hide_on_close(true, true, false, false));
        assert!(!should_hide_on_close(true, true, true, true));
    }

    #[test]
    fn captured_hotkeys_cover_extended_standard_keys() {
        let modifiers = egui::Modifiers {
            ctrl: true,
            alt: true,
            ..Default::default()
        };

        assert_eq!(
            hotkey_spec_from_key(egui::Key::F24, modifiers),
            Some("Ctrl+Alt+F24".to_owned())
        );
        assert_eq!(
            hotkey_spec_from_key(egui::Key::Home, modifiers),
            Some("Ctrl+Alt+Home".to_owned())
        );
        assert_eq!(
            hotkey_spec_from_key(egui::Key::Backtick, modifiers),
            Some("Ctrl+Alt+Backquote".to_owned())
        );
        assert_eq!(hotkey_spec_from_key(egui::Key::Copy, modifiers), None);
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
    fn latency_summary_reports_observed_phases_and_total() {
        let base = Instant::now();
        let trace = LatencyTrace {
            activation_at: base,
            recorder_started_at: Some(base + Duration::from_millis(10)),
            stop_requested_at: Some(base + Duration::from_millis(100)),
            wav_finalized_at: Some(base + Duration::from_millis(140)),
            transcription_dispatched_at: Some(base + Duration::from_millis(150)),
            transcription_completed_at: Some(base + Duration::from_millis(650)),
            ui_result_at: Some(base + Duration::from_millis(660)),
            paste_completed_at: Some(base + Duration::from_millis(735)),
        };

        assert_eq!(
            trace.summary_lines(),
            vec![
                "Activation to recorder ready: 10 ms",
                "Stop to WAV finalized: 40 ms",
                "Transcription job: 500 ms",
                "STT done to UI update: 10 ms",
                "Focused-app output: 75 ms",
                "Total observed: 735 ms",
            ]
        );
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
    fn record_motion_repaints_only_until_the_transition_settles() {
        let ctx = egui::Context::default();
        configure_stitch_style(&ctx);
        ctx.set_visuals(stitch_visuals(ThemeMode::Light));

        let _ = render_record_motion_frame(&ctx, false, false, 0.0);
        let transition = render_record_motion_frame(&ctx, true, false, 1.0 / 60.0);
        assert_eq!(root_repaint_delay(&transition), Duration::ZERO);

        for frame in 2..=20 {
            let _ = render_record_motion_frame(&ctx, true, false, f64::from(frame) / 60.0);
        }
        let settled = render_record_motion_frame(&ctx, true, false, 21.0 / 60.0);
        assert_eq!(root_repaint_delay(&settled), Duration::MAX);

        let stopping = render_record_motion_frame(&ctx, false, false, 22.0 / 60.0);
        assert_eq!(root_repaint_delay(&stopping), Duration::ZERO);
        for frame in 23..=41 {
            let _ = render_record_motion_frame(&ctx, false, false, f64::from(frame) / 60.0);
        }
        let stopped = render_record_motion_frame(&ctx, false, false, 42.0 / 60.0);
        assert_eq!(root_repaint_delay(&stopped), Duration::MAX);
    }

    #[test]
    fn reduced_motion_bypasses_interpolation_and_follow_up_repaints() {
        let ctx = egui::Context::default();
        configure_stitch_style(&ctx);
        ctx.set_visuals(stitch_visuals(ThemeMode::Light));

        let _ = render_record_motion_frame(&ctx, false, true, 0.0);
        let output = render_record_motion_frame(&ctx, true, true, 1.0 / 60.0);
        assert_eq!(root_repaint_delay(&output), Duration::MAX);
    }

    #[test]
    fn performance_modes_are_layman_facing() {
        let labels = WhisperComputeMode::ALL
            .into_iter()
            .map(WhisperComputeMode::label)
            .collect::<Vec<_>>();

        assert_eq!(labels, vec!["Auto", "Prefer GPU", "CPU only"]);
    }

    #[test]
    fn playground_cleanup_deletes_temp_audio_even_when_debug_mode_is_set() {
        let temp_path = std::env::temp_dir().join(format!(
            "scribe-playground-cleanup-{}.wav",
            std::process::id()
        ));
        fs::write(&temp_path, b"wav").unwrap();

        let mut app = test_app();
        app.config.debug_mode = true;
        app.playground_pending = 1;
        app.playground_audio_path = Some(temp_path.clone());

        app.cleanup_after_job(RecordingSource::Playground);

        assert!(!temp_path.exists());
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
    fn recessed_surface_stays_within_its_allocated_width() {
        for width in [840.0, 1440.0] {
            let (output, rect) = render_recessed_surface(width);
            assert!(rect.min.x >= 0.0);
            assert!(rect.max.x <= width + 1.0);
            assert!(rect.width() > 0.0);

            let (highlight, shade) = recessed_edge_colors(false);
            let edge = |color| {
                output
                    .shapes
                    .iter()
                    .find_map(|clipped| match &clipped.shape {
                        egui::Shape::LineSegment { points, stroke } if stroke.color == color => {
                            Some(*points)
                        }
                        _ => None,
                    })
                    .expect("recessed edge should be painted")
            };
            let top = edge(highlight);
            let bottom = edge(shade);
            assert!((top[0].y - (rect.top() + 0.5)).abs() <= f32::EPSILON);
            assert!((bottom[0].y - (rect.bottom() - 0.5)).abs() <= f32::EPSILON);
            for points in [top, bottom] {
                assert!((points[0].x - (rect.left() + 10.0)).abs() <= f32::EPSILON);
                assert!((points[1].x - (rect.right() - 10.0)).abs() <= f32::EPSILON);
            }
        }
    }

    #[test]
    fn compact_navigation_exposes_named_tab_buttons() {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        configure_stitch_style(&ctx);
        ctx.set_visuals(stitch_visuals(ThemeMode::Light));
        let mut current_tab = Tab::Transcribe;
        let output = ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(840.0, 760.0),
                )),
                ..Default::default()
            },
            |ctx| navigation_rail(ctx, &mut current_tab),
        );
        let update = output.platform_output.accesskit_update.unwrap();
        for tab in [Tab::Transcribe, Tab::Models, Tab::Playground, Tab::Settings] {
            assert!(
                update
                    .nodes
                    .iter()
                    .any(|(_, node)| node.name() == Some(tab.label())),
                "missing accessible navigation label for {}",
                tab.label()
            );
        }
    }

    #[test]
    fn transcribe_editor_and_actions_keep_top_flow_geometry_at_minimum_width() {
        let output = render_accessible_app_tab(Tab::Transcribe, 840.0);
        let update = output.platform_output.accesskit_update.unwrap();
        let find_bounds = |role, name| {
            update
                .nodes
                .iter()
                .find(|(_, node)| node.role() == role && node.name() == Some(name))
                .and_then(|(_, node)| node.bounds())
                .unwrap_or_else(|| panic!("missing {name} {role:?}"))
        };
        let editor = find_bounds(egui::accesskit::Role::MultilineTextInput, "Transcript");
        let header = find_bounds(egui::accesskit::Role::StaticText, "Transcript");
        let copy = find_bounds(egui::accesskit::Role::Button, "Copy");
        let clear = find_bounds(egui::accesskit::Role::Button, "Clear");

        assert!(
            editor.x1 - editor.x0 >= 600.0,
            "transcript editor is too narrow: {editor:?}"
        );
        assert!(editor.x0 >= 60.0 && editor.x1 <= 840.0);
        assert!(header.y1 <= editor.y0);
        assert!(copy.y0 >= editor.y1 && clear.y0 >= editor.y1);
        assert!(copy.x0 >= editor.x0 && clear.x1 <= editor.x1);

        let editor_node = update
            .nodes
            .iter()
            .find(|(_, node)| {
                node.role() == egui::accesskit::Role::MultilineTextInput
                    && node.name() == Some("Transcript")
            })
            .map(|(_, node)| node)
            .unwrap();
        assert!(!editor_node.labelled_by().is_empty());
    }

    #[test]
    fn custom_control_targets_use_stable_heights() {
        let ctx = egui::Context::default();
        configure_stitch_style(&ctx);
        ctx.set_visuals(stitch_visuals(ThemeMode::Light));
        let mut heights = Vec::new();
        let _ = ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(700.0, 180.0),
                )),
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        heights.push(ui.add(primary_small_button(ui, "Primary")).rect.height());
                        heights.push(ui.add(small_button(ui, "Secondary")).rect.height());
                        heights.push(ui.add(record_button(ui, false)).rect.height());
                        let mut value = String::new();
                        heights.push(
                            ui.add_sized([120.0, 36.0], TextEdit::singleline(&mut value))
                                .rect
                                .height(),
                        );
                        heights.push(
                            ComboBox::from_id_source("control-height-combo")
                                .selected_text("Value")
                                .show_ui(ui, |_| {})
                                .response
                                .rect
                                .height(),
                        );
                    });
                });
            },
        );

        assert!((heights[0] - 40.0).abs() <= 0.1);
        assert!((heights[1] - 36.0).abs() <= 0.1);
        assert!((heights[2] - 56.0).abs() <= 0.1);
        assert!((heights[3] - 36.0).abs() <= 0.1);
        assert!((heights[4] - 36.0).abs() <= 0.1);
    }

    #[test]
    fn focused_text_input_paints_the_two_pixel_accent_stroke() {
        let ctx = egui::Context::default();
        configure_stitch_style(&ctx);
        let visuals = stitch_visuals(ThemeMode::Light);
        let expected_stroke = Stroke::new(2.0, ThemePalette::light().accent);
        assert_eq!(visuals.selection.stroke, expected_stroke);
        ctx.set_visuals(visuals);

        let input_id = egui::Id::new("focused-text-input-stroke");
        ctx.memory_mut(|memory| memory.request_focus(input_id));
        let mut value = String::new();
        let output = ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(320.0, 120.0),
                )),
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    ui.add_sized([220.0, 36.0], TextEdit::singleline(&mut value).id(input_id));
                });
            },
        );

        assert!(output.shapes.iter().any(|clipped| {
            matches!(
                &clipped.shape,
                egui::Shape::Rect(rect) if rect.stroke == expected_stroke
            )
        }));
    }

    #[test]
    fn key_form_controls_have_accessible_roles_names_and_labels() {
        let transcribe = render_accessible_app_tab(Tab::Transcribe, 840.0);
        assert_named_control(
            &transcribe,
            egui::accesskit::Role::MultilineTextInput,
            "Transcript",
        );
        let models = render_accessible_app_tab(Tab::Models, 840.0);
        assert_named_control(&models, egui::accesskit::Role::TextInput, "Search models");
        let settings = render_accessible_app_tab(Tab::Settings, 840.0);
        for (role, name) in [
            (egui::accesskit::Role::SpinButton, "Paste delay ms"),
            (egui::accesskit::Role::TextInput, "Record toggle hotkey"),
            (egui::accesskit::Role::ComboBox, "Transcription device"),
            (egui::accesskit::Role::ComboBox, "Microphone"),
            (
                egui::accesskit::Role::SpinButton,
                "Custom recording duration in minutes",
            ),
            (egui::accesskit::Role::ComboBox, "Theme"),
        ] {
            assert_named_control(&settings, role, name);
        }
        let hotkey = settings
            .platform_output
            .accesskit_update
            .as_ref()
            .unwrap()
            .nodes
            .iter()
            .find(|(_, node)| {
                node.role() == egui::accesskit::Role::TextInput
                    && node.name() == Some("Record toggle hotkey")
            })
            .map(|(_, node)| node)
            .unwrap();
        assert!(hotkey.value().is_some_and(|value| !value.is_empty()));
    }

    #[test]
    fn recording_settings_expose_a_heading_and_exclusive_hotkey_radios() {
        let output = render_accessible_app_tab(Tab::Settings, 840.0);
        let update = output.platform_output.accesskit_update.as_ref().unwrap();

        let (_, heading) = update
            .nodes
            .iter()
            .find(|(_, node)| {
                node.role() == egui::accesskit::Role::Heading && node.name() == Some("Recording")
            })
            .expect("recording heading should be exposed");
        assert_eq!(heading.hierarchical_level(), Some(2));

        let (press_once_id, press_once) = update
            .nodes
            .iter()
            .find(|(_, node)| {
                node.role() == egui::accesskit::Role::RadioButton
                    && node.name() == Some("Hotkey behavior: Press once")
            })
            .expect("Press once should be an accessible radio button");
        let (hold_id, hold) = update
            .nodes
            .iter()
            .find(|(_, node)| {
                node.role() == egui::accesskit::Role::RadioButton
                    && node.name() == Some("Hotkey behavior: Hold")
            })
            .expect("Hold should be an accessible radio button");

        assert_eq!(press_once.checked(), Some(egui::accesskit::Checked::True));
        assert_eq!(hold.checked(), Some(egui::accesskit::Checked::False));
        assert_eq!(press_once.radio_group(), &[*press_once_id, *hold_id]);
        assert_eq!(hold.radio_group(), &[*press_once_id, *hold_id]);
    }

    #[test]
    fn recording_duration_notice_is_a_polite_status_and_describes_duration_controls() {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let mut custom_duration = 1.0;
        let mut duration_control_ids = None;
        let output = ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(640.0, 120.0),
                )),
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let preset = ui.button("Duration preset");
                    let custom = ui.add(egui::DragValue::new(&mut custom_duration));
                    recording_duration_change_notice(ui, [&preset, &custom]);
                    duration_control_ids = Some([preset.id, custom.id]);
                });
            },
        );
        let update = output.platform_output.accesskit_update.as_ref().unwrap();
        let (notice_id, notice) = update
            .nodes
            .iter()
            .find(|(_, node)| {
                node.role() == egui::accesskit::Role::Status
                    && node.name()
                        == Some(
                            "Duration changes apply to the next recording; the current recording is unchanged.",
                        )
            })
            .expect("duration change notice should be an accessible status");
        assert_eq!(notice.live(), Some(egui::accesskit::Live::Polite));

        for control_id in duration_control_ids.expect("duration control ids should be recorded") {
            let control_id = control_id.value().into();
            let (_, control) = update
                .nodes
                .iter()
                .find(|(id, _)| *id == control_id)
                .unwrap_or_else(|| panic!("missing duration control {control_id:?}"));
            assert!(control.described_by().contains(notice_id));
        }
    }

    #[test]
    fn playground_result_editor_keeps_fixed_layout_height() {
        let allocated_height = render_playground_result_editor_height(960.0);

        assert!(
            (allocated_height - PLAYGROUND_RESULT_HEIGHT).abs() <= 1.0,
            "result editor allocated {allocated_height}, expected {PLAYGROUND_RESULT_HEIGHT}"
        );
    }

    fn render_model_catalog_row(width: f32) -> egui::FullOutput {
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
            let mut current_tab = Tab::Models;
            show_test_navigation(ctx, &mut current_tab);
            egui::CentralPanel::default()
                .frame(content_panel_frame(ctx))
                .show(ctx, |ui| {
                    page(
                        ui,
                        "Models Catalog",
                        TranscriptionStatus::Idle,
                        "Ready",
                        |ui| {
                            panel(ui, |ui| {
                                ui.horizontal_wrapped(|ui| {
                                    let mut search = "whisper".to_owned();
                                    model_search_filter_control(ui, &mut search);
                                    ui.add_space(10.0);
                                    let mut backend = "All".to_owned();
                                    let backends = vec!["whisper.cpp".to_owned()];
                                    model_backend_filter_control(
                                        ui,
                                        "test-model-backend-filter",
                                        &mut backend,
                                        &backends,
                                    );
                                });
                            });

                            ui.add_space(12.0);
                            let model = test_model();
                            let install_status = ModelInstallStatus::Installed;

                            model_catalog_row(ui, &model, &install_status, true, |ui| {
                                let _ = ui.add_enabled(false, primary_small_button(ui, "Active"));
                                let _ = ui.add(small_button(ui, "Uninstall"));
                            });
                        },
                    );
                });
        })
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
            let mut current_tab = Tab::Playground;
            show_test_navigation(ctx, &mut current_tab);
            egui::CentralPanel::default()
                .frame(content_panel_frame(ctx))
                .show(ctx, |ui| {
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

    fn render_recessed_surface(width: f32) -> (egui::FullOutput, egui::Rect) {
        let ctx = egui::Context::default();
        configure_stitch_style(&ctx);
        ctx.set_visuals(stitch_visuals(ThemeMode::Light));
        let mut surface_rect = egui::Rect::NOTHING;

        let output = ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(width, 760.0),
                )),
                ..Default::default()
            },
            |ctx| {
                let mut current_tab = Tab::Transcribe;
                show_test_navigation(ctx, &mut current_tab);
                egui::CentralPanel::default()
                    .frame(content_panel_frame(ctx))
                    .show(ctx, |ui| {
                        surface_rect = recessed_panel(ui, 120.0, |_| {}).response.rect;
                    });
            },
        );

        (output, surface_rect)
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
                .show(ctx, |ui| app.ui_models(ui));
        })
    }

    fn render_app_tab_at_size(tab: Tab, width: f32, height: f32) -> egui::FullOutput {
        let ctx = egui::Context::default();
        configure_stitch_style(&ctx);
        ctx.set_visuals(stitch_visuals(ThemeMode::Light));

        let raw_input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(width, height),
            )),
            ..Default::default()
        };
        let mut app = test_app();
        app.current_tab = tab;

        ctx.run(raw_input, |ctx| {
            show_test_navigation(ctx, &mut app.current_tab);
            egui::CentralPanel::default()
                .frame(content_panel_frame(ctx))
                .show(ctx, |ui| match app.current_tab {
                    Tab::Transcribe => app.ui_transcribe(ui),
                    Tab::Models => app.ui_models(ui),
                    Tab::Playground => app.ui_playground(ui),
                    Tab::Settings => app.ui_settings(ui),
                });
        })
    }

    fn render_record_motion_frame(
        ctx: &egui::Context,
        listening: bool,
        reduced_motion: bool,
        time: f64,
    ) -> egui::FullOutput {
        ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(180.0, 120.0),
                )),
                time: Some(time),
                predicted_dt: 1.0 / 60.0,
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let response = ui.add(record_button(ui, listening));
                    paint_record_motion(ui, &response, listening, reduced_motion);
                });
            },
        )
    }

    fn root_repaint_delay(output: &egui::FullOutput) -> Duration {
        output
            .viewport_output
            .get(&egui::ViewportId::ROOT)
            .expect("root viewport output")
            .repaint_delay
    }

    fn render_accessible_app_tab(tab: Tab, width: f32) -> egui::FullOutput {
        render_accessible_app_tab_at_size(tab, width, 760.0)
    }

    fn render_accessible_app_tab_at_size(tab: Tab, width: f32, height: f32) -> egui::FullOutput {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        configure_stitch_style(&ctx);
        ctx.set_visuals(stitch_visuals(ThemeMode::Light));
        let mut app = test_app();
        app.current_tab = tab;

        render_accessible_app_tab_frame(&ctx, &mut app, width, height, Vec::new())
    }

    fn render_accessible_app_tab_frame(
        ctx: &egui::Context,
        app: &mut LocalTranscriberApp,
        width: f32,
        height: f32,
        events: Vec<egui::Event>,
    ) -> egui::FullOutput {
        ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(width, height),
                )),
                events,
                ..Default::default()
            },
            |ctx| {
                show_test_navigation(ctx, &mut app.current_tab);
                egui::CentralPanel::default()
                    .frame(content_panel_frame(ctx))
                    .show(ctx, |ui| match app.current_tab {
                        Tab::Transcribe => app.ui_transcribe(ui),
                        Tab::Models => app.ui_models(ui),
                        Tab::Playground => app.ui_playground(ui),
                        Tab::Settings => app.ui_settings(ui),
                    });
            },
        )
    }

    fn assert_named_control(output: &egui::FullOutput, role: egui::accesskit::Role, name: &str) {
        let node = output
            .platform_output
            .accesskit_update
            .as_ref()
            .unwrap()
            .nodes
            .iter()
            .find(|(_, node)| node.role() == role && node.name() == Some(name))
            .map(|(_, node)| node)
            .unwrap_or_else(|| panic!("missing accessible {role:?} named {name}"));
        assert!(!node.labelled_by().is_empty());
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
            .find(|(_, node)| {
                node.name() == Some(name) && node.role() != egui::accesskit::Role::StaticText
            })
            .map(|(id, _)| *id)
            .unwrap_or_else(|| panic!("missing interactive control {name:?} in AccessKit update"))
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
                        Tab::Transcribe => app.ui_transcribe(ui),
                        Tab::Models => app.ui_models(ui),
                        Tab::Playground => app.ui_playground(ui),
                        Tab::Settings => app.ui_settings(ui),
                    });
            });
            max_x_by_frame.push(max_painted_x(&output));
        }

        max_x_by_frame
    }

    fn show_test_navigation(ctx: &egui::Context, current_tab: &mut Tab) {
        navigation_rail(ctx, current_tab);
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

    fn assert_no_visible_horizontal_overflow(output: &egui::FullOutput, width: f32, tab: Tab) {
        let max_painted_x = max_visible_painted_x(output);
        assert!(
            max_painted_x <= width + 1.0,
            "{tab:?} visible paint overflowed at {width}px: max_x={max_painted_x}"
        );
    }

    fn assert_focusable_bounds_within_viewport(output: &egui::FullOutput, width: f32, tab: Tab) {
        let update = output
            .platform_output
            .accesskit_update
            .as_ref()
            .expect("AccessKit should be enabled for layout assertions");
        for (_, node) in &update.nodes {
            if node.supports_action(egui::accesskit::Action::Focus)
                && let Some(bounds) = node.bounds()
            {
                // egui's window-area bookkeeping node is an unnamed, zero-sized
                // focus target. Do not exempt any non-zero or named node.
                if node.role() == egui::accesskit::Role::Unknown
                    && node.name().is_none()
                    && bounds.x0 == bounds.x1
                    && bounds.y0 == bounds.y1
                {
                    continue;
                }
                assert!(
                    bounds.x0 >= -1.0
                        && bounds.x1 <= f64::from(width + 1.0)
                        && bounds.x1 > bounds.x0,
                    "{tab:?} focusable control overflowed at {width}px: role={:?}, name={:?}, bounds={bounds:?}",
                    node.role(),
                    node.name(),
                );
            }
        }
    }

    fn assert_accessible_text_contains(output: &egui::FullOutput, expected: &str) {
        let update = output
            .platform_output
            .accesskit_update
            .as_ref()
            .expect("AccessKit should be enabled for text assertions");
        let found = update.nodes.iter().any(|(_, node)| {
            node.name().is_some_and(|name| name.contains(expected))
                || node.value().is_some_and(|value| value.contains(expected))
        });
        let names = (!found).then(|| {
            update
                .nodes
                .iter()
                .filter_map(|(_, node)| node.name())
                .collect::<Vec<_>>()
        });
        assert!(
            found,
            "missing accessible long text containing {expected:?}; nodes={names:?}"
        );
    }

    fn assert_accessible_selected_option(output: &egui::FullOutput, expected: &str) {
        let update = output
            .platform_output
            .accesskit_update
            .as_ref()
            .expect("AccessKit should be enabled for option assertions");
        let node = update
            .nodes
            .iter()
            .find(|(_, node)| {
                node.role() == egui::accesskit::Role::ToggleButton && node.name() == Some(expected)
            })
            .map(|(_, node)| node)
            .unwrap_or_else(|| panic!("missing selectable option {expected:?}"));
        assert_eq!(
            node.checked(),
            Some(egui::accesskit::Checked::True),
            "current option {expected:?} must expose selected state"
        );
        assert_eq!(
            node.default_action_verb(),
            Some(egui::accesskit::DefaultActionVerb::Click),
            "current option {expected:?} must remain activatable"
        );
    }

    fn assert_target_text_is_not_horizontally_clipped(output: &egui::FullOutput, expected: &str) {
        let target_shapes = output
            .shapes
            .iter()
            .flat_map(|clipped| {
                target_text_shape_bounds(&clipped.shape, clipped.clip_rect, expected)
            })
            .collect::<Vec<_>>();
        assert!(
            !target_shapes.is_empty(),
            "target text was not painted: {expected:?}"
        );
        for (bounds, clip_rect) in target_shapes {
            assert!(
                bounds.min.x >= clip_rect.min.x - 1.0 && bounds.max.x <= clip_rect.max.x + 1.0,
                "target text was horizontally clipped instead of wrapped: {expected:?}; bounds={bounds:?}; clip={clip_rect:?}"
            );
        }
    }

    fn target_text_shape_bounds(
        shape: &egui::Shape,
        clip_rect: egui::Rect,
        expected: &str,
    ) -> Vec<(egui::Rect, egui::Rect)> {
        match shape {
            egui::Shape::Text(text) if text.galley.text().replace('\n', "").contains(expected) => {
                vec![(text.visual_bounding_rect(), clip_rect)]
            }
            egui::Shape::Vec(shapes) => shapes
                .iter()
                .flat_map(|shape| target_text_shape_bounds(shape, clip_rect, expected))
                .collect(),
            _ => Vec::new(),
        }
    }

    fn assert_accesskit_rect_within_viewport(
        bounds: egui::accesskit::Rect,
        width: f32,
        height: f32,
        name: &str,
    ) {
        assert!(
            bounds.x0 >= -1.0
                && bounds.y0 >= -1.0
                && bounds.x1 <= f64::from(width + 1.0)
                && bounds.y1 <= f64::from(height + 1.0)
                && bounds.x1 > bounds.x0
                && bounds.y1 > bounds.y0,
            "{name} exceeds the {width}x{height} viewport: {bounds:?}"
        );
    }

    fn accessible_bounds(
        output: &egui::FullOutput,
        role: egui::accesskit::Role,
        name: &str,
    ) -> egui::accesskit::Rect {
        output
            .platform_output
            .accesskit_update
            .as_ref()
            .and_then(|update| {
                update
                    .nodes
                    .iter()
                    .find(|(_, node)| node.role() == role && node.name() == Some(name))
                    .and_then(|(_, node)| node.bounds())
            })
            .unwrap_or_else(|| panic!("missing {role:?} named {name}"))
    }

    fn page_scroll_metrics(ctx: &egui::Context, title: &str) -> (egui::Id, f32, f32) {
        ctx.data_mut(|data| {
            data.get_temp::<(egui::Id, f32, f32)>(egui::Id::new((
                "test-page-scroll-metrics",
                title,
            )))
        })
        .unwrap_or_else(|| panic!("missing page scroll metrics for {title}"))
    }

    fn selector_scroll_metrics(ctx: &egui::Context) -> (egui::Id, f32, egui::Rect, f32) {
        ctx.data_mut(|data| {
            data.get_temp::<(egui::Id, f32, egui::Rect, f32)>(egui::Id::new(
                "test-selector-scroll-metrics",
            ))
        })
        .expect("missing selector scroll metrics")
    }

    fn render_test_dynamic_combo_popup(
        ctx: &egui::Context,
        id_source: &'static str,
        control_name: &str,
        selected_wraps: bool,
        selected: &mut String,
        options: &[String],
        raw_input: egui::RawInput,
    ) -> egui::FullOutput {
        ctx.run(raw_input, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let label = ui.label(control_name);
                let response = if selected_wraps {
                    ComboBox::from_id_source(id_source)
                        .selected_text(selected.as_str())
                        .wrap(true)
                        .height(DYNAMIC_COMBO_POPUP_MAX_HEIGHT)
                        .show_ui(ui, |ui| {
                            prepare_dynamic_combo_popup(ui);
                            for option in options {
                                selectable_dynamic_combo_value(
                                    ui,
                                    selected,
                                    option.clone(),
                                    option,
                                );
                            }
                        })
                        .response
                } else {
                    ComboBox::from_id_source(id_source)
                        .selected_text(selected.as_str())
                        .width(150.0)
                        .height(DYNAMIC_COMBO_POPUP_MAX_HEIGHT)
                        .show_ui(ui, |ui| {
                            prepare_dynamic_combo_popup(ui);
                            for option in options {
                                selectable_dynamic_combo_value(
                                    ui,
                                    selected,
                                    option.clone(),
                                    option,
                                );
                            }
                        })
                        .response
                };
                set_control_accessibility(ui, &response, label.id, control_name);
                ctx.data_mut(|data| {
                    data.insert_temp(egui::Id::new("test-dynamic-combo-id"), response.id);
                });
            });
        })
    }

    fn dynamic_combo_raw_input(viewport_size: Vec2, events: Vec<egui::Event>) -> egui::RawInput {
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, viewport_size)),
            events,
            ..Default::default()
        }
    }

    fn test_dynamic_combo_id(ctx: &egui::Context) -> egui::Id {
        ctx.data_mut(|data| data.get_temp(egui::Id::new("test-dynamic-combo-id")))
            .expect("dynamic combo should record its popup trigger id")
    }

    fn long_layout_sentinel(region: &str) -> String {
        format!("SCRIBE-{region}-{}", "unbroken-text-segment-".repeat(18))
    }

    fn adversarial_sentinels_for_tab(tab: Tab) -> Vec<String> {
        let status = long_layout_sentinel("status");
        match tab {
            Tab::Transcribe => vec![status],
            Tab::Models => vec![status, long_layout_sentinel("model-runtime")],
            Tab::Playground => vec![
                status,
                long_layout_sentinel("playground-model"),
                long_layout_sentinel("playground-result"),
            ],
            Tab::Settings => vec![status],
        }
    }

    fn install_runnable_selector_models(app: &mut LocalTranscriberApp) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "scribe-selector-runnable-models-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();

        for model in crate::models::default_model_catalog() {
            let path = write_runnable_selector_model(&root, &model);
            app.config.model_paths.insert(model.id, path);
        }
        root
    }

    fn write_runnable_selector_model(root: &Path, model: &SttModelInfo) -> PathBuf {
        let model_root = root.join(&model.id);
        match model.backend.as_str() {
            "whisper.cpp" => {
                let path = root.join(format!("{}.bin", model.id));
                fs::write(&path, b"model").unwrap();
                path
            }
            "faster-whisper" => {
                fs::create_dir_all(&model_root).unwrap();
                fs::write(model_root.join("model.bin"), b"model").unwrap();
                fs::write(model_root.join("config.json"), b"{}").unwrap();
                model_root
            }
            "Vosk" => {
                fs::create_dir_all(model_root.join("am")).unwrap();
                fs::create_dir_all(model_root.join("conf")).unwrap();
                fs::create_dir_all(model_root.join("graph")).unwrap();
                fs::write(model_root.join("am").join("final.mdl"), b"model").unwrap();
                fs::write(model_root.join("conf").join("model.conf"), b"model").unwrap();
                fs::write(model_root.join("graph").join("HCLG.fst"), b"model").unwrap();
                model_root
            }
            "sherpa-onnx" => {
                fs::create_dir_all(&model_root).unwrap();
                for file in [
                    "tokens.txt",
                    "encoder-epoch-99-avg-1.int8.onnx",
                    "decoder-epoch-99-avg-1.int8.onnx",
                    "joiner-epoch-99-avg-1.int8.onnx",
                ] {
                    fs::write(model_root.join(file), b"model").unwrap();
                }
                model_root
            }
            "Moonshine" => {
                fs::create_dir_all(&model_root).unwrap();
                for file in [
                    "tokens.txt",
                    "encoder_model.ort",
                    "decoder_model_merged.ort",
                ] {
                    fs::write(model_root.join(file), b"model").unwrap();
                }
                model_root
            }
            "Parakeet" => {
                fs::create_dir_all(&model_root).unwrap();
                for file in [
                    "tokens.txt",
                    "encoder.int8.onnx",
                    "decoder.int8.onnx",
                    "joiner.int8.onnx",
                ] {
                    fs::write(model_root.join(file), b"model").unwrap();
                }
                model_root
            }
            _ => unreachable!("catalog backend should have a selector test fixture"),
        }
    }

    fn adversarial_test_app() -> LocalTranscriberApp {
        let mut app = test_app();
        let status_sentinel = long_layout_sentinel("status");
        let path_sentinel = long_layout_sentinel("storage-path");
        let device_sentinel = long_layout_sentinel("microphone");
        let long_device = format!("Studio USB input device {device_sentinel}");
        let mut playground_model = test_model();
        playground_model.id = "adversarial-model".to_owned();
        playground_model.name = format!("Local model {}", long_layout_sentinel("playground-model"));
        playground_model.backend = format!("runtime backend {path_sentinel}");
        playground_model.description = format!(
            "A deliberately long model description keeps the runtime failure and path readable: {}",
            long_layout_sentinel("playground-description")
        );

        app.status = TranscriptionStatus::Error;
        app.status_message = format!(
            "Runtime preparation failed while validating {status_sentinel}. Review the full path before retrying."
        );
        app.model_downloads.insert(
            "whisper_cpp_base_en".to_owned(),
            ModelInstallStatus::RuntimeError(format!(
                "The managed runtime reported an actionable setup failure at {}.",
                long_layout_sentinel("model-runtime")
            )),
        );
        app.audio_devices = vec![long_device.clone()];
        app.config.audio_input_device_name = Some(long_device);
        app.config.model_storage_dir = PathBuf::from(&path_sentinel);
        app.playground_cards = vec![PlaygroundCardState {
            model: playground_model,
            status: ModelRuntimeStatus::Error(format!(
                "The selected runtime returned a detailed error for {}.",
                long_layout_sentinel("playground-runtime")
            )),
            transcript: format!(
                "A long playground result remains scrollable: {}",
                long_layout_sentinel("playground-result")
            ),
            latency_ms: Some(1_234),
            audio_duration_ms: None,
            peak_ram_mb: None,
            peak_vram_mb: None,
        }];
        app.config.playground_selected_models = vec!["adversarial-model".to_owned()];
        app
    }

    fn test_app() -> LocalTranscriberApp {
        let mut config = AppConfig::default();
        config::normalize_config(&mut config);
        let (tx, rx) = unbounded();

        LocalTranscriberApp {
            hotkey_input: config.hotkey.clone(),
            model_search: String::new(),
            model_backend_filter: "All".to_owned(),
            audio_devices: Vec::new(),
            capturing_hotkey: false,
            model_downloads: HashMap::new(),
            runtime_jobs: HashMap::new(),
            playground_cards: cards_from_config(&config),
            playground_selector_draft: None,
            playground_selector_return_focus: None,
            playground_selector_header_focus: None,
            playground_selector_needs_initial_focus: false,
            playground_reference_transcript: String::new(),
            playground_reference_user_edited: false,
            playground_ranking_mode: RankingMode::Balanced,
            hotkey_service: HotkeyService::new(&config.hotkey),
            config,
            config_path: None,
            current_tab: Tab::Models,
            status: TranscriptionStatus::Idle,
            transcript: String::new(),
            status_message: "Ready".to_owned(),
            active_recording: None,
            tx,
            rx,
            playground_pending: 0,
            playground_audio_path: None,
            latest_latency: None,
            tray_service: None,
            last_tray_state: None,
            reduced_motion: false,
            quit_requested: false,
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

    fn expected_runtime_install_action(backend: &str) -> RuntimeActionState {
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
                disabled_tooltip: Some(missing_runtime_source_message(
                    backend,
                    RuntimeDevicePack::Cpu,
                )),
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
    fn model_action_state_matches_install_select_uninstall_rules() {
        let mut whisper = test_model();
        whisper.install_status = ModelInstallStatus::NotInstalled;

        assert_eq!(
            model_action_state(&whisper, &ModelInstallStatus::NotInstalled, false),
            ModelActionState {
                primary: ModelPrimaryAction::Install,
                primary_enabled: true,
                show_uninstall: false,
            }
        );
        assert_eq!(
            model_action_state(&whisper, &ModelInstallStatus::Installed, false),
            ModelActionState {
                primary: ModelPrimaryAction::Select,
                primary_enabled: true,
                show_uninstall: true,
            }
        );
        assert_eq!(
            model_action_state(&whisper, &ModelInstallStatus::Installed, true),
            ModelActionState {
                primary: ModelPrimaryAction::Active,
                primary_enabled: false,
                show_uninstall: true,
            }
        );
        assert_eq!(
            model_action_state(
                &whisper,
                &ModelInstallStatus::Error("network failed".to_owned()),
                false,
            ),
            ModelActionState {
                primary: ModelPrimaryAction::Retry,
                primary_enabled: true,
                show_uninstall: false,
            }
        );

        let mut unavailable = whisper;
        unavailable.backend = "sherpa-onnx".to_owned();
        unavailable.download_model = None;
        assert_eq!(
            model_action_state(&unavailable, &ModelInstallStatus::NotInstalled, false),
            ModelActionState {
                primary: ModelPrimaryAction::Install,
                primary_enabled: false,
                show_uninstall: false,
            }
        );
    }

    #[test]
    fn installed_model_with_missing_runtime_offers_repair() {
        let model = test_model();
        assert_eq!(
            model_action_state_with_runtime(&model, &ModelInstallStatus::Installed, true, false),
            ModelActionState {
                primary: ModelPrimaryAction::Repair,
                primary_enabled: true,
                show_uninstall: true,
            }
        );
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
        assert!(
            missing_runtime_source_message("Vosk", RuntimeDevicePack::Cpu)
                .contains("No trusted CPU runtime artifact")
        );
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
        assert!(!runtime_mutation_artifacts_exist_for_test(&target));
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
        assert!(!runtime_mutation_artifacts_exist_for_test(&target));

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
        assert!(!runtime_mutation_artifacts_exist_for_test(&aliased_target));
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
        assert!(!runtime_mutation_artifacts_exist_for_test(&target));
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

    #[test]
    fn runtime_activation_does_not_start_when_stage_sync_fails() {
        for failure in [
            durable_fs::SyncTreeFailureKind::File,
            durable_fs::SyncTreeFailureKind::Directory,
        ] {
            let root = std::env::temp_dir().join(format!(
                "scribe-runtime-stage-sync-{failure:?}-{}",
                std::process::id()
            ));
            let _ = fs::remove_dir_all(&root);
            let target = root.join("vosk");
            let stage = runtime_transaction_path(&target, "installing");
            write_vosk_runtime_with_revision(&target, 3);
            let lock = acquire_runtime_install_lock_with_timeout(
                "vosk",
                &target,
                None,
                Duration::from_millis(10),
            )
            .unwrap();
            let staged_executable = write_vosk_runtime_with_revision(&stage, 4);
            let relative_executable = staged_executable
                .strip_prefix(&stage)
                .unwrap()
                .to_path_buf();
            let injected = durable_fs::inject_sync_tree_failure(failure);

            let error =
                activate_staged_runtime("vosk", &target, &stage, &relative_executable, lock)
                    .unwrap_err();
            drop(injected);

            assert!(error.contains("durable before activation"));
            assert_eq!(
                fs::read_to_string(target.join("runtime-manifest.json")).unwrap(),
                r#"{"runner_revision":3}"#
            );
            assert_eq!(
                fs::read_to_string(stage.join("runtime-manifest.json")).unwrap(),
                r#"{"runner_revision":4}"#
            );
            assert!(!runtime_transaction_exists_for_test(&target));
            assert!(!runtime_transaction_path(&target, "backup").exists());
            let _ = fs::remove_dir_all(root);
        }
    }

    #[test]
    fn runtime_recovery_restores_after_each_activation_rename() {
        for (case, phase, activate_new) in [
            (
                "after-backup-rename",
                RuntimeTransactionPhase::Prepared,
                false,
            ),
            (
                "after-stage-rename",
                RuntimeTransactionPhase::BackedUp,
                true,
            ),
        ] {
            let root = std::env::temp_dir().join(format!(
                "scribe-runtime-recovery-{case}-{}",
                std::process::id()
            ));
            let _ = fs::remove_dir_all(&root);
            let target = root.join("vosk");
            let stage = runtime_transaction_path(&target, "installing");
            let backup = runtime_transaction_path(&target, "backup");
            let old_executable = write_vosk_runtime_with_revision(&target, 3);
            write_vosk_runtime_with_revision(&stage, 4);
            let previous = config::ManagedRuntimeInstall::new(old_executable);
            write_runtime_journal(
                &target,
                &RuntimeTransactionJournal {
                    version: 1,
                    runtime_id: "vosk".to_owned(),
                    phase,
                    had_previous_runtime: true,
                    previous_install: Some(previous.clone()),
                    new_install: None,
                },
            )
            .unwrap();
            fs::rename(&target, &backup).unwrap();
            if activate_new {
                fs::rename(&stage, &target).unwrap();
            }

            recover_runtime_transaction("vosk", &target, Some(&previous)).unwrap();

            assert_eq!(
                fs::read_to_string(target.join("runtime-manifest.json")).unwrap(),
                r#"{"runner_revision":3}"#
            );
            assert!(!backup.exists());
            assert!(!stage.exists());
            assert!(!runtime_transaction_exists_for_test(&target));
            let _ = fs::remove_dir_all(root);
        }
    }

    #[test]
    fn runtime_recovery_uses_persisted_metadata_to_finalize_or_roll_back() {
        for persisted_new in [false, true] {
            let root = std::env::temp_dir().join(format!(
                "scribe-runtime-persistence-recovery-{persisted_new}-{}",
                std::process::id()
            ));
            let _ = fs::remove_dir_all(&root);
            let target = root.join("vosk");
            let backup = runtime_transaction_path(&target, "backup");
            let old_executable = write_vosk_runtime_with_revision(&backup, 3);
            let new_executable = write_vosk_runtime_with_revision(&target, 4);
            let mut previous = config::ManagedRuntimeInstall::new(
                target.join(old_executable.strip_prefix(&backup).unwrap()),
            );
            previous.source = Some("old".to_owned());
            let mut new = config::ManagedRuntimeInstall::new(new_executable);
            new.source = Some("new".to_owned());
            write_runtime_journal(
                &target,
                &RuntimeTransactionJournal {
                    version: 1,
                    runtime_id: "vosk".to_owned(),
                    phase: RuntimeTransactionPhase::AwaitingPersistence,
                    had_previous_runtime: true,
                    previous_install: Some(previous.clone()),
                    new_install: Some(new.clone()),
                },
            )
            .unwrap();

            let persisted = if persisted_new { &new } else { &previous };
            recover_runtime_transaction("vosk", &target, Some(persisted)).unwrap();

            let expected_revision = if persisted_new { 4 } else { 3 };
            assert_eq!(
                fs::read_to_string(target.join("runtime-manifest.json")).unwrap(),
                format!(r#"{{"runner_revision":{expected_revision}}}"#)
            );
            assert!(!backup.exists());
            assert!(!runtime_transaction_exists_for_test(&target));
            let _ = fs::remove_dir_all(root);
        }
    }

    #[test]
    fn runtime_recovery_is_idempotent_after_backup_restore() {
        let root = std::env::temp_dir().join(format!(
            "scribe-runtime-idempotent-recovery-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        let target = root.join("vosk");
        let executable = write_vosk_runtime_with_revision(&target, 3);
        let mut previous = config::ManagedRuntimeInstall::new(executable.clone());
        previous.source = Some("old".to_owned());
        let mut new = config::ManagedRuntimeInstall::new(executable);
        new.source = Some("new".to_owned());
        write_runtime_journal(
            &target,
            &RuntimeTransactionJournal {
                version: 1,
                runtime_id: "vosk".to_owned(),
                phase: RuntimeTransactionPhase::AwaitingPersistence,
                had_previous_runtime: true,
                previous_install: Some(previous.clone()),
                new_install: Some(new),
            },
        )
        .unwrap();

        recover_runtime_transaction("vosk", &target, Some(&previous)).unwrap();

        assert_eq!(
            fs::read_to_string(target.join("runtime-manifest.json")).unwrap(),
            r#"{"runner_revision":3}"#
        );
        assert!(!runtime_transaction_exists_for_test(&target));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn runtime_journal_falls_back_only_for_truncated_next_file() {
        let root = std::env::temp_dir().join(format!(
            "scribe-runtime-journal-fallback-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        let target = root.join("vosk");
        fs::create_dir_all(&root).unwrap();
        let current = RuntimeTransactionJournal {
            version: 1,
            runtime_id: "vosk".to_owned(),
            phase: RuntimeTransactionPhase::Prepared,
            had_previous_runtime: false,
            previous_install: None,
            new_install: None,
        };
        write_runtime_journal(&target, &current).unwrap();
        let next = runtime_transaction_path(&target, "transaction.next");
        fs::write(&next, b"{").unwrap();

        assert_eq!(
            read_runtime_journal("vosk", &target).unwrap(),
            Some(current.clone())
        );

        let wrong_identity = RuntimeTransactionJournal {
            runtime_id: "sherpa_onnx".to_owned(),
            ..current
        };
        fs::write(&next, serde_json::to_vec(&wrong_identity).unwrap()).unwrap();
        let error = read_runtime_journal("vosk", &target).unwrap_err();
        assert!(error.contains("unexpected identity"));
        assert!(next.exists());
        assert!(runtime_transaction_path(&target, "transaction").exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn runtime_recovery_preserves_unjournaled_backup() {
        let root = std::env::temp_dir().join(format!(
            "scribe-runtime-unjournaled-backup-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        let target = root.join("vosk");
        let backup = runtime_transaction_path(&target, "backup");
        write_vosk_runtime(&target);
        write_vosk_runtime(&backup);

        let error = recover_runtime_transaction("vosk", &target, None).unwrap_err();

        assert!(error.contains("unjournaled"));
        assert!(target.exists());
        assert!(backup.exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn runtime_recovery_cleans_stale_stage_and_partial_download() {
        let root = std::env::temp_dir().join(format!(
            "scribe-runtime-stale-cleanup-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        let target = root.join("vosk");
        let stage = runtime_transaction_path(&target, "installing");
        let partial = runtime_transaction_path(&target, "download").with_extension("zip.partial");
        fs::create_dir_all(&stage).unwrap();
        fs::write(&partial, b"partial").unwrap();

        recover_runtime_transaction("vosk", &target, None).unwrap();

        assert!(!stage.exists());
        assert!(!partial.exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn runtime_install_lock_is_exclusive_and_released_with_the_guard() {
        let root = std::env::temp_dir().join(format!("scribe-runtime-lock-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let target = root.join("vosk");
        let first = acquire_runtime_install_lock_with_timeout(
            "vosk",
            &target,
            None,
            Duration::from_millis(10),
        )
        .unwrap();
        let error = acquire_runtime_install_lock_with_timeout(
            "vosk",
            &target,
            None,
            Duration::from_millis(10),
        )
        .unwrap_err();
        assert!(error.contains("Another Scribe process"));
        drop(first);
        acquire_runtime_install_lock_with_timeout("vosk", &target, None, Duration::from_millis(10))
            .unwrap();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn runtime_journal_uses_the_record_loaded_under_the_runtime_lock() {
        let root = std::env::temp_dir().join(format!(
            "scribe-runtime-locked-record-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        let target = root.join("vosk");
        let executable = write_vosk_runtime(&target);
        let mut locked_previous = config::ManagedRuntimeInstall::new(executable.clone());
        locked_previous.source = Some("fresh-persisted".to_owned());
        let lock = acquire_runtime_install_lock_with_timeout(
            "vosk",
            &target,
            Some(&locked_previous),
            Duration::from_millis(10),
        )
        .unwrap();
        let mut replacement = RuntimeReplacement {
            installed_path: executable.clone(),
            target_root: target.clone(),
            backup_root: None,
            runtime_id: "vosk".to_owned(),
            persistence_install: None,
            _lock: lock,
        };
        let mut new = config::ManagedRuntimeInstall::new(executable);
        new.source = Some("new".to_owned());

        replacement.prepare_persistence(Some(&new)).unwrap();

        let journal = read_runtime_journal("vosk", &target).unwrap().unwrap();
        assert_eq!(journal.previous_install, Some(locked_previous));
        replacement.rollback().unwrap();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn config_durability_warning_leaves_runtime_transaction_for_recovery() {
        let root = std::env::temp_dir().join(format!(
            "scribe-runtime-durability-warning-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        let target = root.join("vosk");
        let stage = runtime_transaction_path(&target, "installing");
        let previous_executable = write_vosk_runtime(&target);
        let previous = config::ManagedRuntimeInstall::new(previous_executable);
        let lock = acquire_runtime_install_lock_with_timeout(
            "vosk",
            &target,
            Some(&previous),
            Duration::from_millis(10),
        )
        .unwrap();
        let staged_executable = write_vosk_runtime(&stage);
        let relative_executable = staged_executable
            .strip_prefix(&stage)
            .unwrap()
            .to_path_buf();
        let mut replacement =
            activate_staged_runtime("vosk", &target, &stage, &relative_executable, lock).unwrap();
        let current = config::ManagedRuntimeInstall::new(target.join(relative_executable));
        replacement.prepare_persistence(Some(&current)).unwrap();

        let warning = finalize_runtime_transaction(
            replacement,
            Some("injected config directory sync failure".to_owned()),
        )
        .unwrap();

        assert!(warning.contains("injected"));
        assert!(runtime_transaction_exists_for_test(&target));
        assert!(runtime_transaction_path(&target, "backup").exists());
        recover_runtime_transaction("vosk", &target, Some(&current)).unwrap();
        assert!(!runtime_transaction_exists_for_test(&target));
        assert!(!runtime_transaction_path(&target, "backup").exists());
        let _ = fs::remove_dir_all(root);
    }

    fn runtime_transaction_exists_for_test(target: &Path) -> bool {
        runtime_transaction_path(target, "transaction").exists()
            || runtime_transaction_path(target, "transaction.next").exists()
    }

    fn runtime_mutation_artifacts_exist_for_test(target: &Path) -> bool {
        runtime_transaction_exists_for_test(target)
            || runtime_transaction_path(target, "installing").exists()
            || runtime_transaction_path(target, "backup").exists()
            || runtime_transaction_path(target, "download")
                .with_extension("zip.partial")
                .exists()
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
            config.managed_runtimes.insert(
                runtime_id.to_owned(),
                config::ManagedRuntimeInstall::new(current.clone()),
            );
            let mut model = test_model();
            model.backend = backend.to_owned();

            assert_eq!(
                runtime_install_source_from_candidates(&config, &model, Some(current), None, None),
                None,
                "{backend} must not update from its managed install"
            );
            assert!(matches!(
                runtime_install_source_from_candidates(&config, &model, Some(staged), None, None),
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
        config.managed_runtimes.insert(
            "vosk".to_owned(),
            managed_runtime_with_version(executable, Some("0.3.44")),
        );
        let mut model = test_model();
        model.backend = "Vosk".to_owned();
        let provider = stt::provider_for_backend("Vosk").unwrap();

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
        assert!(stt::vosk::is_vosk_runtime_usable(&previous_executable));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn runtime_smoke_validation_executes_help_successfully() {
        smoke_validate_runtime(&std::env::current_exe().unwrap()).unwrap();
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
            .managed_runtimes
            .insert("vosk".to_owned(), previous_record.clone());
        let mut new_record = config::ManagedRuntimeInstall::new(replacement.installed_path.clone());
        new_record.source = Some("replacement".to_owned());
        assert!(!runtime_metadata_matches(&config, "vosk", &new_record));
        let replaced = apply_runtime_record(&mut config, "vosk", new_record.clone());
        assert!(runtime_metadata_matches(&config, "vosk", &new_record));

        rollback_runtime_record(&mut config, "vosk", replaced);
        replacement.rollback().unwrap();

        assert_eq!(config.managed_runtimes.get("vosk"), Some(&previous_record));
        assert!(target_root.join("previous.marker").is_file());
        assert!(!target_root.join("new.marker").exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn runtime_persistence_gates_download_continuation() {
        let mut config = AppConfig::default();
        let previous = config::ManagedRuntimeInstall::new(PathBuf::from("previous-runtime"));
        config
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
        assert_eq!(config.managed_runtimes.get("vosk"), Some(&previous));
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
                Ok(config::ConfigSaveOutcome {
                    config: saved.clone(),
                    durability_warning: None,
                })
            });
        assert!(persistence_attempted.get());
        assert!(matches!(
            persisted,
            RuntimePersistenceTransition::Persisted {
                job: RuntimeInstallJob {
                    download_model_ids,
                    ..
                },
                durability_warning: None,
            } if download_model_ids == ["queued-model"]
        ));
    }

    #[test]
    fn runtime_uninstall_error_preserves_managed_metadata() {
        let mut config = AppConfig::default();
        let install = config::ManagedRuntimeInstall::new(PathBuf::from("managed-runtime"));
        config
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
        assert_eq!(config.managed_runtimes.get("vosk"), Some(&install));

        assert_eq!(
            apply_runtime_uninstall_result(&mut config, "vosk", Ok(false)),
            Ok(false)
        );
        assert!(!config.managed_runtimes.contains_key("vosk"));
    }

    #[test]
    fn runtime_uninstall_holds_lock_and_rolls_files_back_when_persistence_fails() {
        let root = std::env::temp_dir().join(format!(
            "scribe-runtime-uninstall-rollback-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        let target = root.join("vosk");
        let executable = write_vosk_runtime(&target);
        let mut config = AppConfig::default();
        let install = config::ManagedRuntimeInstall::new(executable.clone());
        config
            .managed_runtimes
            .insert("vosk".to_owned(), install.clone());

        let error = uninstall_runtime_transaction_at(&mut config, "vosk", &target, |_| {
            let lock_error = acquire_runtime_install_lock_with_timeout(
                "vosk",
                &target,
                None,
                Duration::from_millis(10),
            )
            .unwrap_err();
            assert!(lock_error.contains("Another Scribe process"));
            Err("disk full".to_owned())
        })
        .unwrap_err();

        assert!(error.contains("disk full"));
        assert!(executable.exists());
        assert_eq!(config.managed_runtimes.get("vosk"), Some(&install));
        assert!(!runtime_transaction_exists_for_test(&target));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn runtime_uninstall_commits_files_and_merged_metadata_together() {
        let root = std::env::temp_dir().join(format!(
            "scribe-runtime-uninstall-commit-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        let target = root.join("vosk");
        let executable = write_vosk_runtime(&target);
        let mut config = AppConfig::default();
        config.managed_runtimes.insert(
            "vosk".to_owned(),
            config::ManagedRuntimeInstall::new(executable),
        );
        config.managed_runtimes.insert(
            "sherpa_onnx".to_owned(),
            config::ManagedRuntimeInstall::new(PathBuf::from("sherpa/bin/runtime")),
        );

        let removed = uninstall_runtime_transaction_at(&mut config, "vosk", &target, |candidate| {
            let mut committed = candidate.clone();
            committed.managed_runtimes.remove("vosk");
            Ok(config::ConfigSaveOutcome {
                config: committed,
                durability_warning: None,
            })
        })
        .unwrap();

        assert!(removed.removed_files);
        assert!(removed.durability_warning.is_none());
        assert!(!target.exists());
        assert!(!config.managed_runtimes.contains_key("vosk"));
        assert!(config.managed_runtimes.contains_key("sherpa_onnx"));
        assert!(!runtime_transaction_exists_for_test(&target));
        assert!(!runtime_transaction_path(&target, "backup").exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn runtime_uninstall_config_warning_defers_cleanup_to_recovery() {
        let root = std::env::temp_dir().join(format!(
            "scribe-runtime-uninstall-warning-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        let target = root.join("vosk");
        let executable = write_vosk_runtime(&target);
        let mut config = AppConfig::default();
        config.managed_runtimes.insert(
            "vosk".to_owned(),
            config::ManagedRuntimeInstall::new(executable),
        );

        let outcome = uninstall_runtime_transaction_at(&mut config, "vosk", &target, |candidate| {
            let mut committed = candidate.clone();
            committed.managed_runtimes.remove("vosk");
            Ok(config::ConfigSaveOutcome {
                config: committed,
                durability_warning: Some("injected config directory sync failure".to_owned()),
            })
        })
        .unwrap();

        assert!(outcome.removed_files);
        assert!(outcome.durability_warning.unwrap().contains("injected"));
        assert!(!target.exists());
        assert!(runtime_transaction_path(&target, "backup").exists());
        assert!(runtime_transaction_exists_for_test(&target));
        recover_runtime_transaction("vosk", &target, None).unwrap();
        assert!(!runtime_transaction_path(&target, "backup").exists());
        assert!(!runtime_transaction_exists_for_test(&target));
        let _ = fs::remove_dir_all(root);
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
        assert_eq!(config.playground_selected_models, ["whisper_cpp_tiny_en"]);
        set_model_selected(&mut config, "whisper_cpp_tiny_en", false);
        assert!(config.playground_selected_models.is_empty());
    }

    #[test]
    fn playground_selector_draft_opens_cancels_and_stays_closed_while_busy() {
        let mut app = test_app();
        app.config.model_storage_dir = std::env::temp_dir().join(format!(
            "scribe-selector-state-missing-models-{}",
            std::process::id()
        ));
        app.config.managed_models.clear();
        app.config.model_paths.clear();
        app.config.playground_selected_models = vec!["whisper_cpp_tiny_en".to_owned()];

        app.open_playground_selector(None);
        assert_eq!(
            app.playground_selector_draft,
            Some(vec!["whisper_cpp_tiny_en".to_owned()])
        );
        app.playground_selector_draft.as_mut().unwrap().clear();
        app.close_playground_selector(&egui::Context::default());
        assert!(app.playground_selector_draft.is_none());
        assert_eq!(
            app.config.playground_selected_models,
            ["whisper_cpp_tiny_en"]
        );

        app.playground_pending = 1;
        app.open_playground_selector(None);
        assert!(app.playground_selector_draft.is_none());

        let before_apply = app.config.playground_selected_models.clone();
        app.playground_selector_draft = Some(vec!["whisper_cpp_base_en".to_owned()]);
        app.apply_playground_selector(&egui::Context::default());
        assert_eq!(app.config.playground_selected_models, before_apply);

        apply_playground_selector_draft(&mut app.config, vec!["whisper_cpp_base_en".to_owned()]);
        assert!(app.config.playground_selected_models.is_empty());
    }

    #[test]
    fn selector_escape_dismisses_rendered_window_without_persisting_draft() {
        let ctx = egui::Context::default();
        configure_stitch_style(&ctx);
        let mut app = test_app();
        app.config.playground_selected_models = vec!["whisper_cpp_tiny_en".to_owned()];
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
            app.config.playground_selected_models,
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
        app.config.model_storage_dir = std::env::temp_dir().join(format!(
            "scribe-missing-selector-models-{}",
            std::process::id()
        ));
        app.config.playground_selected_models = vec!["whisper_cpp_base_en".to_owned()];
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
        assert!(app.config.playground_selected_models.is_empty());
    }

    #[test]
    fn selector_empty_state_explains_how_to_install_models() {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        configure_stitch_style(&ctx);
        let mut app = test_app();
        app.config.model_storage_dir = std::env::temp_dir().join(format!(
            "scribe-selector-empty-state-{}",
            std::process::id()
        ));
        app.config.managed_models.clear();
        app.config.model_paths.clear();
        app.config.playground_selected_models.clear();
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
        app.config.model_storage_dir =
            std::env::temp_dir().join(format!("scribe-selector-min-width-{}", std::process::id()));
        app.config.managed_models.clear();
        app.config.model_paths.clear();
        app.config.playground_selected_models.clear();
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
        fs::write(model_dir.join("ggml-tiny.en.bin"), b"tiny").unwrap();

        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        configure_stitch_style(&ctx);
        let mut app = test_app();
        app.config.model_storage_dir = root.clone();
        app.config.managed_models.clear();
        app.config.model_paths.clear();
        app.config.playground_selected_models.clear();
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
        let checkbox = accesskit_control_id_with_prefix(&output, "whisper.cpp tiny.en;");
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
            app.config.playground_selected_models,
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
        let model_dir = root.join("whisper.cpp");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&model_dir).unwrap();
        fs::write(model_dir.join("ggml-tiny.en.bin"), b"tiny").unwrap();
        fs::write(model_dir.join("ggml-base.en.bin"), b"base").unwrap();

        let mut app = test_app();
        app.config.model_storage_dir = root.clone();
        app.config.playground_selected_models = vec![
            "whisper_cpp_tiny_en".to_owned(),
            "whisper_cpp_base_en".to_owned(),
        ];
        app.config.playground_model_order = vec![
            "whisper_cpp_base_en".to_owned(),
            "whisper_cpp_tiny_en".to_owned(),
        ];
        config::normalize_config(&mut app.config);
        app.refresh_playground_cards_from_config();
        app.playground_cards[0].transcript = "retained".to_owned();

        app.config.playground_selected_models = vec!["whisper_cpp_base_en".to_owned()];
        app.refresh_playground_cards_from_config();
        assert_eq!(app.playground_cards.len(), 1);
        assert_eq!(app.playground_cards[0].transcript, "retained");

        app.config
            .playground_selected_models
            .push("whisper_cpp_tiny_en".to_owned());
        app.refresh_playground_cards_from_config();
        assert_eq!(app.playground_cards.len(), 2);
        assert!(app.playground_cards[1].transcript.is_empty());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn playground_run_requires_a_selection_and_ready_cards() {
        let mut app = test_app();
        app.config.playground_selected_models.clear();
        app.playground_cards.clear();
        assert!(
            app.playground_run_block_reason()
                .is_some_and(|message| message.contains("Choose models"))
        );

        app.playground_cards.push(PlaygroundCardState {
            model: test_model(),
            status: ModelRuntimeStatus::MissingConfiguration,
            transcript: String::new(),
            latency_ms: None,
            audio_duration_ms: None,
            peak_ram_mb: None,
            peak_vram_mb: None,
        });
        assert!(
            app.playground_run_block_reason()
                .is_some_and(|message| message.contains("not ready"))
        );
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
    fn busy_runtime_disables_maintenance_and_repair_keeps_model_installed() {
        let model = test_model();
        let busy = runtime_action_state_with_busy(&AppConfig::default(), &model, true);
        assert!(!busy.enabled);
        assert!(
            busy.disabled_tooltip
                .as_deref()
                .is_some_and(|message| message.contains("already being prepared"))
        );
        assert_eq!(
            model_action_state_with_runtime(
                &model,
                &ModelInstallStatus::RuntimeError("failed".to_owned()),
                true,
                false,
            ),
            ModelActionState {
                primary: ModelPrimaryAction::Repair,
                primary_enabled: true,
                show_uninstall: true,
            }
        );
        assert_eq!(
            model_primary_action_label(
                ModelPrimaryAction::Repair,
                &model,
                &ModelInstallStatus::RuntimeError("failed".to_owned()),
            ),
            "Repair whisper.cpp runtime"
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
            let blocked = restrict_runtime_action(update.clone(), "Vosk", false, activity);
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
                "Vosk",
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
                "Vosk",
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
            .find(|model| stt::provider_for_backend(&model.backend).is_some())
            .unwrap();
        let runtime_id = stt::provider_for_backend(&model.backend)
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
    fn disabled_model_actions_explain_why_they_are_disabled() {
        let whisper = test_model();
        let active_state = model_action_state(&whisper, &ModelInstallStatus::Installed, true);
        assert_eq!(
            model_primary_disabled_tooltip(
                &whisper,
                &ModelInstallStatus::Installed,
                true,
                &active_state,
            ),
            Some("This model is already the active transcription model.".to_owned())
        );

        let mut unavailable = whisper;
        unavailable.backend = "sherpa-onnx".to_owned();
        unavailable.download_model = None;
        let unavailable_state =
            model_action_state(&unavailable, &ModelInstallStatus::NotInstalled, false);
        let tooltip = model_primary_disabled_tooltip(
            &unavailable,
            &ModelInstallStatus::NotInstalled,
            false,
            &unavailable_state,
        )
        .unwrap();

        assert!(tooltip.contains("not available"));
        assert!(tooltip.contains("sherpa-onnx"));
    }

    #[test]
    fn download_progress_uses_known_total_for_fraction_and_labels() {
        let status = ModelInstallStatus::Downloading {
            downloaded_bytes: 256 * 1024 * 1024,
            total_bytes: Some(1024 * 1024 * 1024),
            bytes_per_second: Some(4 * 1024 * 1024),
        };

        assert_eq!(download_progress_fraction(&status), Some(0.25));
        assert_eq!(download_progress_bar_text(&status), "25% Completed");
        assert_eq!(
            download_progress_detail(&status),
            Some("256 MB / 1.0 GB | 4 MB/s".to_owned())
        );
    }

    #[test]
    fn semantic_progress_bar_paints_proportional_fill_in_full_track() {
        let ctx = egui::Context::default();
        configure_stitch_style(&ctx);
        ctx.set_visuals(stitch_visuals(ThemeMode::Light));
        let status = ModelInstallStatus::Downloading {
            downloaded_bytes: 256 * 1024 * 1024,
            total_bytes: Some(1024 * 1024 * 1024),
            bytes_per_second: None,
        };

        let output = ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(640.0, 220.0),
                )),
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default()
                    .frame(content_panel_frame(ctx))
                    .show(ctx, |ui| {
                        with_usable_width_cap(ui, 400.0, |ui| {
                            download_progress_bar(ui, &status);
                        });
                    });
            },
        );

        let track_fill = ThemePalette::light().panel_bg;
        let track = output
            .shapes
            .iter()
            .filter_map(|shape| match &shape.shape {
                egui::Shape::Rect(rect)
                    if rect.fill == track_fill && (rect.rect.height() - 18.0).abs() < 0.1 =>
                {
                    Some(rect.rect)
                }
                _ => None,
            })
            .max_by(|a, b| a.width().total_cmp(&b.width()))
            .unwrap();
        let fill = output
            .shapes
            .iter()
            .filter_map(|shape| match &shape.shape {
                egui::Shape::Rect(rect)
                    if rect.fill == ThemePalette::light().accent
                        && (rect.rect.height() - 18.0).abs() < 0.1 =>
                {
                    Some(rect.rect)
                }
                _ => None,
            })
            .max_by(|a, b| a.width().total_cmp(&b.width()))
            .unwrap();

        assert!(track.width() >= 390.0, "track width was {}", track.width());
        assert!(
            (fill.width() / track.width() - 0.25).abs() <= 0.01,
            "fill was {} of track {}",
            fill.width(),
            track.width()
        );
    }

    #[test]
    fn indeterminate_progress_has_no_accessible_numeric_value() {
        for status in [
            ModelInstallStatus::InstallingRuntime,
            ModelInstallStatus::Downloading {
                downloaded_bytes: 1024,
                total_bytes: None,
                bytes_per_second: None,
            },
        ] {
            let ctx = egui::Context::default();
            ctx.enable_accesskit();
            let output = ctx.run(egui::RawInput::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    download_progress_bar(ui, &status);
                });
            });
            let update = output.platform_output.accesskit_update.unwrap();
            let progress = update
                .nodes
                .iter()
                .map(|(_, node)| node)
                .find(|node| node.role() == egui::accesskit::Role::ProgressIndicator)
                .unwrap();

            assert_eq!(progress.numeric_value(), None);
            assert_eq!(progress.min_numeric_value(), None);
            assert_eq!(progress.max_numeric_value(), None);
        }
    }

    #[test]
    fn collapsing_header_exposes_expanded_accessibility_state() {
        for open in [false, true] {
            let ctx = egui::Context::default();
            ctx.enable_accesskit();
            let output = ctx.run(egui::RawInput::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let response = egui::CollapsingHeader::new("Runtime maintenance")
                        .default_open(open)
                        .show(ui, |ui| ui.label("Runtime controls"));
                    set_collapsing_header_accessibility(ctx, &response);
                });
            });
            let update = output.platform_output.accesskit_update.unwrap();
            let header = update
                .nodes
                .iter()
                .map(|(_, node)| node)
                .find(|node| node.name() == Some("Runtime maintenance"))
                .unwrap();

            assert_eq!(header.is_expanded(), Some(open));
        }
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
    fn device_labels_follow_backend_capabilities() {
        let mut config = AppConfig {
            selected_default_model: "faster_whisper_tiny_en".to_owned(),
            ..AppConfig::default()
        };
        let faster_whisper = config::configured_models(&config)
            .into_iter()
            .find(|model| model.id == "faster_whisper_tiny_en")
            .unwrap();

        assert_eq!(model_device_label(&faster_whisper), "CPU/GPU");
        assert!(selected_model_device_support(&config).supports_gpu());

        config.selected_default_model = "vosk_small_en".to_owned();
        let vosk = config::configured_models(&config)
            .into_iter()
            .find(|model| model.id == "vosk_small_en")
            .unwrap();

        assert_eq!(model_device_label(&vosk), "CPU");
        assert!(!selected_model_device_support(&config).supports_gpu());
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
        config.managed_runtimes.insert(
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

        config.managed_runtimes.clear();
        config.managed_runtimes.insert(
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
                "sherpa-onnx",
                "sherpa_onnx",
                "scribe-sherpa-onnx",
                "sherpa-onnx-zipformer-small-en-2023-06-26",
            ),
            (
                "Moonshine",
                "moonshine",
                "scribe-moonshine",
                "sherpa-onnx-moonshine-tiny-en-quantized-2026-02-27",
            ),
            (
                "Parakeet",
                "parakeet",
                "scribe-parakeet",
                "sherpa-onnx-nemo-parakeet-unified-en-0.6b-int8-non-streaming",
            ),
        ];
        for (backend, runtime_id, wrapper, download_model) in managed_models {
            let mut model = test_model();
            model.backend = backend.to_owned();
            model.download_model = Some(download_model.to_owned());

            assert_eq!(
                runtime_action_state(&AppConfig::default(), &model),
                expected_runtime_install_action(&model.backend),
                "{backend} should be installable"
            );

            config.managed_runtimes.clear();
            config.managed_runtimes.insert(
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
                .is_some_and(|tooltip| tooltip.contains("not a supported STT backend"))
        );

        let _ = fs::remove_dir_all(runtime_root);
    }

    #[test]
    fn runtime_action_state_ignores_stale_runtime_metadata() {
        let runtime_root =
            std::env::temp_dir().join(format!("scribe-stale-runtime-{}", std::process::id()));
        let _ = fs::remove_dir_all(&runtime_root);

        let mut config = AppConfig::default();
        config.managed_runtimes.insert(
            "faster_whisper".to_owned(),
            config::ManagedRuntimeInstall::new(PathBuf::from(
                "/tmp/scribe-runtimes/missing/bin/scribe-faster-whisper",
            )),
        );
        let mut model = test_model();
        model.backend = "faster-whisper".to_owned();
        model.download_model = Some("tiny.en".to_owned());

        let action = runtime_action_state(&config, &model);

        assert_eq!(action.kind, RuntimeActionKind::Install);
        assert_eq!(action, expected_runtime_install_action(&model.backend));

        config.managed_runtimes.clear();
        config.managed_runtimes.insert(
            "vosk".to_owned(),
            config::ManagedRuntimeInstall::new(write_vosk_runtime_with_revision(
                &runtime_root.join("vosk"),
                2,
            )),
        );
        model.backend = "Vosk".to_owned();
        model.download_model = Some("vosk-model-small-en-us-0.15".to_owned());

        let action = runtime_action_state(&config, &model);

        assert_eq!(action.kind, RuntimeActionKind::Install);
        assert_eq!(action, expected_runtime_install_action(&model.backend));
        let _ = fs::remove_dir_all(runtime_root);
    }

    #[test]
    fn runtime_version_state_detects_current_stale_and_unknown_installs() {
        let provider = stt::provider_for_backend("Vosk").unwrap();
        let mut config = AppConfig::default();

        config.managed_runtimes.insert(
            "vosk".to_owned(),
            managed_runtime_with_version(PathBuf::from("/tmp/scribe/vosk"), Some("0.3.45")),
        );
        assert_eq!(
            runtime_version_state(&config, provider),
            RuntimeVersionState::Current("0.3.45".to_owned())
        );

        config.managed_runtimes.insert(
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

        config.managed_runtimes.insert(
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
    fn auto_maintains_an_existing_stale_gpu_pack_without_requesting_gpu_for_cpu_only_setup() {
        let mut config = AppConfig::default();
        let model = test_model();
        assert_eq!(runtime_device_pack(&config, &model), RuntimeDevicePack::Cpu);

        let mut stale_gpu = config::ManagedRuntimeInstall::new(PathBuf::from("managed-gpu"));
        stale_gpu.source = Some("https://previous-release.invalid/whisper.zip".to_owned());
        stale_gpu.version = Some("old-version".to_owned());
        stale_gpu.sha256 = Some("a".repeat(64));
        stale_gpu.platform = Some(config::current_platform_key());
        stale_gpu.device = Some("gpu".to_owned());
        config
            .managed_runtimes
            .insert("whisper_cpp".to_owned(), stale_gpu);

        assert_eq!(runtime_device_pack(&config, &model), RuntimeDevicePack::Gpu);
        assert_eq!(
            runtime_version_device_pack(&config, stt::provider_for_backend("whisper.cpp").unwrap()),
            RuntimeDevicePack::Gpu
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
        config.managed_runtimes.insert(
            "vosk".to_owned(),
            managed_runtime_with_version(
                write_vosk_runtime(&runtime_root.join("vosk")),
                Some("0.3.44"),
            ),
        );

        let action = runtime_action_state(&config, &model);

        if runtime_install_source(&config, &model).is_ok_and(|source| source.is_some()) {
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

        let install = managed_runtime_install_record(executable, "packaged-runtime", None);

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
        let storage_dir = PathBuf::from("/tmp/scribe-runtimes");
        let runtime_dir = storage_dir.join("whisper_cpp");
        let runtime_executable = runtime_dir.join("bin").join("whisper-cli");
        let sibling_runtime_file = storage_dir.join("legacy-whisper-cli");
        let external_runtime = PathBuf::from("/opt/whisper.cpp/bin/whisper-cli");

        assert_eq!(
            runtime_uninstall_target(&storage_dir, "whisper_cpp", &runtime_executable),
            Some(runtime_dir)
        );
        assert_eq!(
            runtime_uninstall_target(&storage_dir, "whisper_cpp", &sibling_runtime_file),
            Some(sibling_runtime_file)
        );
        assert_eq!(
            runtime_uninstall_target(&storage_dir, "whisper_cpp", &external_runtime),
            None
        );
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

        let mut config = AppConfig {
            selected_default_model: "whisper_cpp_base_en".to_owned(),
            model_storage_dir: temp_dir.clone(),
            ..AppConfig::default()
        };
        config.managed_models.insert(
            "whisper_cpp_base_en".to_owned(),
            config::ManagedModelInstall::new(base_path.clone()),
        );
        config.managed_models.insert(
            "whisper_cpp_small_en".to_owned(),
            config::ManagedModelInstall::new(small_path.clone()),
        );

        let base_model = config::configured_models(&config)
            .into_iter()
            .find(|model| model.id == "whisper_cpp_base_en")
            .unwrap();

        assert!(uninstall_model_files(&config, &base_model).unwrap());
        assert!(!base_path.exists());
        config.managed_models.remove("whisper_cpp_base_en");
        select_first_installed_model(&mut config);

        assert_eq!(config.selected_default_model, "whisper_cpp_small_en");

        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn uninstall_clears_active_model_when_no_installed_models_remain() {
        let temp_dir =
            std::env::temp_dir().join(format!("scribe-empty-models-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&temp_dir);
        let mut config = AppConfig {
            selected_default_model: "whisper_cpp_base_en".to_owned(),
            model_storage_dir: temp_dir.clone(),
            ..AppConfig::default()
        };

        select_first_installed_model(&mut config);

        assert!(config.selected_default_model.is_empty());
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
        let config = AppConfig {
            model_storage_dir: app_storage,
            model_paths,
            ..AppConfig::default()
        };
        let model = config::configured_models(&config)
            .into_iter()
            .find(|model| model.id == "whisper_cpp_base_en")
            .unwrap();

        assert!(!uninstall_model_files(&config, &model).unwrap());
        assert!(external_path.exists());

        let _ = fs::remove_dir_all(&temp_dir);
    }
}
