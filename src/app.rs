use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use arboard::Clipboard;
use crossbeam_channel::{Receiver, Sender, unbounded};
use eframe::egui::{
    self, Align, Button, Color32, ComboBox, FontFamily, FontId, Frame, Layout, Margin, RichText,
    Rounding, ScrollArea, Stroke, TextEdit, TextStyle, Ui, Vec2, ViewportCommand,
};

use crate::audio::{self, RecordingSession};
use crate::benchmark::{
    self, BenchmarkMetric, BenchmarkModelInput, BenchmarkModelResult, RankingMode,
};
use crate::config::{self, AppConfig, HotkeyMode, ThemeMode, WhisperComputeMode};
use crate::hotkey::{HotkeyEvent, HotkeyService};
use crate::managed_downloads;
use crate::models::{
    ModelInstallStatus, ModelRuntimeStatus, SttModelInfo, TranscriptResult, TranscriptionStatus,
    format_bytes,
};
use crate::runtime_catalog;
use crate::stt;
use crate::text_output;
use crate::tray::{TrayCommand, TrayService};

const ACTIVE_REPAINT_DELAY: Duration = Duration::from_millis(100);
const IDLE_REPAINT_DELAY: Duration = Duration::from_millis(500);

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
    SetEnabled(String, bool),
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
    Select,
    Active,
}

impl ModelPrimaryAction {
    fn label(self) -> &'static str {
        match self {
            Self::Install => "Install",
            Self::Retry => "Retry",
            Self::Installing => "Installing",
            Self::Select => "Select",
            Self::Active => "Active",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RuntimeActionKind {
    Install,
    Uninstall,
}

impl RuntimeActionKind {
    fn label(self) -> &'static str {
        match self {
            Self::Install => "Install Runtime",
            Self::Uninstall => "Uninstall Runtime",
        }
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

fn model_action_state(
    model: &SttModelInfo,
    install_status: &ModelInstallStatus,
    selected: bool,
) -> ModelActionState {
    match install_status {
        ModelInstallStatus::Installed => ModelActionState {
            primary: if selected {
                ModelPrimaryAction::Active
            } else {
                ModelPrimaryAction::Select
            },
            primary_enabled: !selected,
            show_uninstall: supports_managed_uninstall(model, install_status),
        },
        ModelInstallStatus::Downloading { .. } => ModelActionState {
            primary: ModelPrimaryAction::Installing,
            primary_enabled: false,
            show_uninstall: false,
        },
        ModelInstallStatus::Error(_) => ModelActionState {
            primary: ModelPrimaryAction::Retry,
            primary_enabled: supports_managed_install(model),
            show_uninstall: false,
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
        ModelPrimaryAction::Installing => Some("This model is still downloading.".to_owned()),
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
        ModelPrimaryAction::Install => {
            Some("This model does not have a managed installer.".to_owned())
        }
        ModelPrimaryAction::Active => Some("This model cannot be selected right now.".to_owned()),
    }
}

fn runtime_action_state(config: &AppConfig, model: &SttModelInfo) -> RuntimeActionState {
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

    if has_managed_runtime_install(config, provider) {
        return RuntimeActionState {
            kind: RuntimeActionKind::Uninstall,
            enabled: true,
            disabled_tooltip: None,
        };
    }

    if runtime_install_source(config, model).is_some() {
        RuntimeActionState {
            kind: RuntimeActionKind::Install,
            enabled: true,
            disabled_tooltip: None,
        }
    } else {
        RuntimeActionState {
            kind: RuntimeActionKind::Install,
            enabled: false,
            disabled_tooltip: Some(format!(
                "No packaged {} runtime was found. Install a build that bundles this runtime.",
                model.backend
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
) -> Option<RuntimeInstallSource> {
    packaged_runtime_path(config, model)
        .map(RuntimeInstallSource::Packaged)
        .or_else(|| {
            development_runtime_package(config, model).map(RuntimeInstallSource::DevelopmentScript)
        })
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
    active_recording: Option<ActiveRecording>,
    tx: Sender<AppEvent>,
    rx: Receiver<AppEvent>,
    playground_cards: Vec<PlaygroundCardState>,
    playground_reference_transcript: String,
    playground_reference_user_edited: bool,
    playground_ranking_mode: RankingMode,
    playground_pending: usize,
    playground_audio_path: Option<PathBuf>,
    latest_latency: Option<LatencyTrace>,
    hotkey_service: HotkeyService,
    tray_service: Option<TrayService>,
    last_tray_state: Option<TrayUiState>,
    quit_requested: bool,
}

impl LocalTranscriberApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        configure_stitch_style(&cc.egui_ctx);

        let (mut config, config_path, status_message) = match config::load_config() {
            Ok((config, path)) => (config, Some(path), "Ready".to_owned()),
            Err(err) => (
                AppConfig::default(),
                None,
                format!("Config error: {err}. Using defaults."),
            ),
        };
        config::normalize_config(&mut config);
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
            playground_cards: cards_from_config(&config),
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

    fn playground_enabled_models(&self) -> Vec<SttModelInfo> {
        config::playground_enabled_models(&self.config)
    }

    fn save_config(&mut self) {
        config::normalize_config(&mut self.config);
        match config::save_config(&self.config) {
            Ok(()) => {
                if self.config_path.is_none() {
                    self.config_path = config::config_file_path().ok();
                }
                self.status_message = "Settings saved".to_owned();
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
            || self
                .model_downloads
                .values()
                .any(|status| matches!(status, ModelInstallStatus::Downloading { .. }))
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
            PlaygroundAction::SetEnabled(model_id, enabled) => {
                let was_active_model = model_id == self.config.selected_default_model;
                set_model_enabled(&mut self.config, &model_id, enabled);
                if !enabled {
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
                    if was_active_model && !self.playground_reference_user_edited {
                        self.playground_reference_transcript.clear();
                    }
                }
                self.save_config();
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
                }
            }
        }
    }

    fn start_recording(&mut self, source: RecordingSource) {
        if self.active_recording.is_some() {
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
        if close_requested
            && self.config.close_to_tray
            && self.tray_service.is_some()
            && !self.quit_requested
        {
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
                        self.config.managed_models.insert(
                            model_id.clone(),
                            config::ManagedModelInstall::app_managed(path, "managed-download"),
                        );
                        self.record_packaged_runtime_metadata(&model);
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
        let models = config::configured_models_for_playground(&self.config)
            .into_iter()
            .filter(|model| model.enabled)
            .collect::<Vec<_>>();
        if models.is_empty() {
            self.status = TranscriptionStatus::Error;
            self.status_message =
                "Enable at least one model before running the playground".to_owned();
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

    fn set_all_models_enabled(&mut self, enabled: bool) {
        set_all_models_enabled(&mut self.config, enabled);
        self.save_config();
        if !enabled {
            self.clear_playground_results(true);
        }
        self.status_message = if enabled {
            "Enabled all models".to_owned()
        } else {
            "Disabled all models".to_owned()
        };
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

    fn start_model_download(&mut self, model: &SttModelInfo) {
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

    fn install_runtime(&mut self, model: &SttModelInfo) {
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

        let Some(source) = runtime_install_source(&self.config, model) else {
            self.status = TranscriptionStatus::Error;
            self.status_message = format!(
                "No packaged {} runtime was found in this build.",
                model.backend
            );
            return;
        };

        let (path, source_label) = match source {
            RuntimeInstallSource::Packaged(packaged_path) => (
                install_runtime_files(provider.runtime_id, &packaged_path),
                "packaged-runtime",
            ),
            RuntimeInstallSource::DevelopmentScript(package) => (
                build_development_runtime_package(provider.runtime_id, &model.backend, package),
                "development-script",
            ),
        };
        let path = match path {
            Ok(path) => path,
            Err(message) => {
                self.status = TranscriptionStatus::Error;
                self.status_message = message;
                return;
            }
        };

        self.config.managed_runtimes.insert(
            provider.runtime_id.to_owned(),
            config::ManagedRuntimeInstall::app_managed(path, source_label),
        );
        self.save_config();
        self.refresh_playground_runtime_statuses();
        self.status = TranscriptionStatus::Idle;
        self.status_message = format!("Installed {} runtime.", model.backend);
    }

    fn uninstall_runtime(&mut self, model: &SttModelInfo) {
        let Some(provider) = stt::provider_for_backend(&model.backend) else {
            self.status = TranscriptionStatus::Error;
            self.status_message = format!("{} is not a supported STT backend.", model.backend);
            return;
        };

        let removal = uninstall_runtime_files(&self.config, provider.runtime_id);
        self.config.managed_runtimes.remove(provider.runtime_id);
        self.save_config();
        self.refresh_playground_runtime_statuses();
        self.status = TranscriptionStatus::Idle;
        self.status_message = match removal {
            Ok(true) => format!("Uninstalled {} runtime.", model.backend),
            Ok(false) => format!("Removed {} runtime from Scribe.", model.backend),
            Err(message) => format!("Removed {} runtime from Scribe. {message}", model.backend),
        };
    }

    fn refresh_playground_runtime_statuses(&mut self) {
        for card in &mut self.playground_cards {
            card.status = runtime_status_for_model(&self.config, &card.model);
        }
    }

    fn record_packaged_runtime_metadata(&mut self, model: &SttModelInfo) {
        let Some(provider) = stt::provider_for_backend(&model.backend) else {
            return;
        };
        if !provider.runtime_install_supported {
            return;
        }
        if let Some(path) = packaged_runtime_path(&self.config, model) {
            self.config.managed_runtimes.insert(
                provider.runtime_id.to_owned(),
                config::ManagedRuntimeInstall::app_managed(path, "packaged-runtime"),
            );
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
        let colors = theme_palette(ctx);
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

        egui::SidePanel::left("navigation")
            .frame(
                Frame::none()
                    .fill(colors.sidebar_bg)
                    .stroke(Stroke::new(1.0, colors.border))
                    .inner_margin(Margin::symmetric(14.0, 16.0)),
            )
            .resizable(false)
            .exact_width(200.0)
            .show(ctx, |ui| {
                ui.label(
                    RichText::new("Scribe")
                        .font(FontId::proportional(20.0))
                        .color(colors.primary)
                        .strong(),
                );
                ui.label(RichText::new("Local-First STT").small().weak());
                ui.add_space(22.0);
                nav_button(ui, &mut self.current_tab, Tab::Transcribe);
                nav_button(ui, &mut self.current_tab, Tab::Models);
                nav_button(ui, &mut self.current_tab, Tab::Playground);
                nav_button(ui, &mut self.current_tab, Tab::Settings);
                ui.with_layout(Layout::bottom_up(Align::LEFT), |ui| {
                    sidebar_link(ui, "Privacy");
                    sidebar_link(ui, "Help");
                });
            });

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

            ui.columns(2, |columns| {
                summary_card(
                    &mut columns[0],
                    "Current Model",
                    |ui| {
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
                    },
                    |ui| {
                        if ui.add(small_button(ui, "Change")).clicked() {
                            requested_tab = Some(Tab::Models);
                        }
                    },
                );

                summary_card(
                    &mut columns[1],
                    "Hotkey",
                    |ui| {
                        ui.label(body_strong(&hotkey));
                    },
                    |ui| {
                        if ui.add(small_button(ui, "Edit")).clicked() {
                            requested_tab = Some(Tab::Settings);
                        }
                    },
                );
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
                    ui.label(mut_text(setup_message));
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
            recessed_panel(ui, 110.0, |ui| {
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
                    let record_button = primary_button(ui, button_text);
                    if add_enabled_button(
                        ui,
                        listening || ready,
                        record_button,
                        disabled_tooltip.as_deref(),
                    )
                    .clicked()
                    {
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
                            RichText::new("System audio & microphone active")
                                .small()
                                .weak(),
                        );
                    }
                });
            });

            ui.add_space(12.0);
            transcript_panel(ui, status, |ui| {
                ui.horizontal(|ui| {
                    ui.label(section_heading("Transcript"));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ready_dot(ui, self.status);
                    });
                });
                ui.add_space(10.0);
                ui.add(
                    TextEdit::multiline(&mut self.transcript)
                        .desired_rows(14)
                        .desired_width(usable_width(ui))
                        .hint_text("Your transcription appears here..."),
                );
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
            panel(ui, |ui| {
                ui.label(section_heading("Runtimes"));
                ui.label(mut_text(
                    "Install each backend runtime independently. Model rows show the runtime they use and estimated storage.",
                ));
                wrapped_label(
                    ui,
                    mut_text(format!(
                        "Storage: models in {} · runtimes in {}",
                        config::model_storage_dir(&self.config).display(),
                        config::runtime_storage_dir().display()
                    )),
                );
                ui.add_space(8.0);
                for model in runtime_representative_models(&self.config) {
                    let runtime_status = stt::provider_for_backend(&model.backend)
                        .map(|provider| provider.runtime_status(&self.config))
                        .unwrap_or_else(|| {
                            ModelRuntimeStatus::Error(format!(
                                "unsupported STT backend: {}",
                                model.backend
                            ))
                        });
                    let action_state = runtime_action_state(&self.config, &model);
                    ui.horizontal_wrapped(|ui| {
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
                        });
                        ui.with_layout(Layout::right_to_left(Align::TOP), |ui| {
                            let runtime_button = small_button(ui, action_state.kind.label());
                            let response = add_enabled_button(
                                ui,
                                action_state.enabled,
                                runtime_button,
                                action_state.disabled_tooltip.as_deref(),
                            );
                            if response.clicked() {
                                runtime_action = Some((model.clone(), action_state.kind));
                            }
                            badge(
                                ui,
                                &runtime_status.to_string(),
                                runtime_chip_tone(&runtime_status),
                            );
                            badge(
                                ui,
                                &format!("Runtime {}", runtime_storage_estimate(&model.backend)),
                                ChipTone::Neutral,
                            );
                        });
                    });
                    ui.add_space(6.0);
                }
            });

            if let Some((model, kind)) = runtime_action {
                match kind {
                    RuntimeActionKind::Install => self.install_runtime(&model),
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
                let action_state = model_action_state(&model, &install_status, selected);
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
                    let primary_button = match action_state.primary {
                        ModelPrimaryAction::Select | ModelPrimaryAction::Active => {
                            primary_small_button(ui, action_state.primary.label())
                        }
                        _ => small_button(ui, action_state.primary.label()),
                    };
                    if add_enabled_button(
                        ui,
                        action_state.primary_enabled,
                        primary_button,
                        primary_disabled_tooltip.as_deref(),
                    )
                    .clicked()
                    {
                        match action_state.primary {
                            ModelPrimaryAction::Select => select_default = true,
                            ModelPrimaryAction::Install | ModelPrimaryAction::Retry => {
                                start_install = true;
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

            empty_import_panel(ui, |ui| {
                if ui.add(small_button(ui, "Import Custom Model")).clicked() {
                    self.status_message =
                        "Custom model import is not available in this build.".to_owned();
                }
            });
        });
    }

    fn ui_playground(&mut self, ui: &mut Ui) {
        let status = self.status;
        let status_message = self.status_message.clone();
        page(ui, "Model Playground", status, &status_message, |ui| {
            panel(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    let text = if self.active_recording.is_some() {
                        "Stop Recording"
                    } else {
                        "Start Test Recording"
                    };
                    if ui.add(primary_small_button(ui, text)).clicked() {
                        if self.active_recording.is_some() {
                            self.stop_recording();
                        } else {
                            self.start_recording(RecordingSource::Playground);
                        }
                    }
                    if ui.add(small_button(ui, "Clear Results")).clicked() {
                        self.clear_playground_results(true);
                    }
                    if ui.add(small_button(ui, "Enable All")).clicked() {
                        self.set_all_models_enabled(true);
                    }
                    if ui.add(small_button(ui, "Disable All")).clicked() {
                        self.set_all_models_enabled(false);
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        badge(
                            ui,
                            &format!("{} enabled", self.playground_enabled_models().len()),
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
                    ui.label(section_heading("Enabled Models"));
                    ui.label(mut_text(
                        "Performance comparison based on current system hardware.",
                    ));
                });
            });
            ui.add_space(8.0);
            let mut pending_actions = Vec::new();
            if self.playground_cards.is_empty() {
                panel(ui, |ui| {
                    ui.label(mut_text("No models are configured."));
                });
            }
            for card_state in &mut self.playground_cards {
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
                ui.add_space(8.0);
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
                    ui.label("Paste delay ms");
                    if ui
                        .add(egui::DragValue::new(&mut paste_delay).clamp_range(1..=1000))
                        .changed()
                    {
                        self.config.paste_delay_ms = paste_delay.max(1) as u64;
                        self.save_config();
                    }
                });
            });

            ui.add_space(12.0);
            card(ui, |ui| {
                ui.label(section_heading("Shortcuts"));
                ui.add_space(8.0);
                ui.horizontal_wrapped(|ui| {
                    ui.label("Record toggle");
                    ui.add(
                        TextEdit::singleline(&mut self.hotkey_input)
                            .desired_width(width_before_trailing(ui, 154.0, 96.0)),
                    );
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
                ui.horizontal_wrapped(|ui| {
                    let before = self.config.hotkey_mode;
                    ui.label("Hotkey mode");
                    ComboBox::from_id_source("hotkey-mode")
                        .selected_text(self.config.hotkey_mode.label())
                        .show_ui(ui, |ui| {
                            for mode in HotkeyMode::ALL {
                                ui.selectable_value(
                                    &mut self.config.hotkey_mode,
                                    mode,
                                    mode.label(),
                                );
                            }
                        });
                    if before != self.config.hotkey_mode {
                        self.save_config();
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
                    ui.label("Transcription device");
                    let mut compute_mode = self.config.whisper_compute_mode;
                    ComboBox::from_id_source("transcription-device-mode")
                        .selected_text(compute_mode.label())
                        .show_ui(ui, |ui| {
                            for mode in WhisperComputeMode::ALL {
                                let enabled =
                                    mode != WhisperComputeMode::PreferGpu || prefer_gpu_available;
                                ui.add_enabled_ui(enabled, |ui| {
                                    ui.selectable_value(&mut compute_mode, mode, mode.label());
                                });
                            }
                        });
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
                            ui.label("GPU device");
                            let mut selected_device = self.config.whisper_gpu_device.to_string();
                            ComboBox::from_id_source("transcription-device-picker")
                                .selected_text(
                                    devices
                                        .iter()
                                        .find(|device| device.id == selected_device)
                                        .map(|device| device.name.as_str())
                                        .unwrap_or("Auto"),
                                )
                                .show_ui(ui, |ui| {
                                    for device in &devices {
                                        ui.selectable_value(
                                            &mut selected_device,
                                            device.id.clone(),
                                            &device.name,
                                        );
                                    }
                                });
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
                    ui.label("Microphone");
                    ComboBox::from_id_source("audio-input-device")
                        .selected_text(
                            self.config
                                .audio_input_device_name
                                .as_deref()
                                .unwrap_or("OS default"),
                        )
                        .show_ui(ui, |ui| {
                            ui.selectable_value(
                                &mut self.config.audio_input_device_name,
                                None,
                                "OS default",
                            );
                            for device in &self.audio_devices {
                                ui.selectable_value(
                                    &mut self.config.audio_input_device_name,
                                    Some(device.clone()),
                                    device,
                                );
                            }
                        });
                    if before != self.config.audio_input_device_name {
                        self.save_config();
                    }
                    if ui.add(small_button(ui, "Refresh")).clicked() {
                        self.refresh_audio_devices();
                    }
                });
                let mut max_duration = self.config.max_recording_seconds as i32;
                ui.horizontal_wrapped(|ui| {
                    ui.label("Max recording seconds");
                    if ui
                        .add(egui::DragValue::new(&mut max_duration).clamp_range(1..=600))
                        .changed()
                    {
                        self.config.max_recording_seconds = max_duration.max(1) as u32;
                        self.save_config();
                    }
                });
            });

            ui.add_space(12.0);
            card(ui, |ui| {
                ui.label(section_heading("Appearance"));
                ui.add_space(8.0);
                ui.horizontal_wrapped(|ui| {
                    let before = self.config.theme_mode;
                    ui.label("Theme");
                    ComboBox::from_id_source("theme-mode")
                        .selected_text(self.config.theme_mode.label())
                        .show_ui(ui, |ui| {
                            for mode in ThemeMode::ALL {
                                ui.selectable_value(
                                    &mut self.config.theme_mode,
                                    mode,
                                    mode.label(),
                                );
                            }
                        });
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
                        ui.label(mut_text(line));
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
            shell_bg: Color32::from_rgb(247, 249, 251),
            content_bg: Color32::from_rgb(247, 249, 251),
            sidebar_bg: Color32::WHITE,
            card_bg: Color32::WHITE,
            panel_bg: Color32::from_rgb(248, 250, 252),
            active_card_bg: Color32::from_rgb(239, 246, 255),
            text: Color32::from_rgb(29, 33, 42),
            muted_text: Color32::from_rgb(85, 95, 109),
            border: Color32::from_rgb(226, 232, 240),
            border_strong: Color32::from_rgb(203, 213, 225),
            primary: Color32::from_rgb(6, 10, 18),
            accent: Color32::from_rgb(37, 99, 235),
            success: Color32::from_rgb(22, 163, 74),
            warning: Color32::from_rgb(202, 138, 4),
            error: Color32::from_rgb(220, 38, 38),
            primary_button_bg: Color32::from_rgb(6, 10, 18),
            primary_button_text: Color32::WHITE,
        }
    }

    fn dark() -> Self {
        Self {
            shell_bg: Color32::from_rgb(15, 18, 24),
            content_bg: Color32::from_rgb(15, 18, 24),
            sidebar_bg: Color32::from_rgb(20, 24, 32),
            card_bg: Color32::from_rgb(26, 31, 41),
            panel_bg: Color32::from_rgb(22, 27, 36),
            active_card_bg: Color32::from_rgb(25, 42, 68),
            text: Color32::from_rgb(236, 241, 247),
            muted_text: Color32::from_rgb(156, 166, 179),
            border: Color32::from_rgb(53, 61, 76),
            border_strong: Color32::from_rgb(76, 86, 104),
            primary: Color32::from_rgb(247, 250, 252),
            accent: Color32::from_rgb(96, 165, 250),
            success: Color32::from_rgb(74, 222, 128),
            warning: Color32::from_rgb(251, 191, 36),
            error: Color32::from_rgb(248, 113, 113),
            primary_button_bg: Color32::from_rgb(37, 99, 235),
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
    style.spacing.interact_size = Vec2::new(24.0, 28.0);
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
                ui.label(mut_text(status_message));
            }
            ui.add_space(14.0);
            let body_width = usable_width(ui);
            ScrollArea::vertical()
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
        },
    );
}

fn content_panel_frame(ctx: &egui::Context) -> Frame {
    let colors = theme_palette(ctx);
    Frame::none()
        .fill(colors.content_bg)
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

fn model_search_filter_control(ui: &mut Ui, search: &mut String) {
    ui.vertical(|ui| {
        ui.label(label_caps("Search"));
        ui.add_sized(
            [190.0, 28.0],
            TextEdit::singleline(search).hint_text("Search models..."),
        );
    });
}

fn model_backend_filter_control(
    ui: &mut Ui,
    id_source: &'static str,
    selected_backend: &mut String,
    backends: &[String],
) {
    ui.vertical(|ui| {
        ui.label(label_caps("Filter Backend"));
        ComboBox::from_id_source(id_source)
            .selected_text(if selected_backend == "All" {
                "All Backends"
            } else {
                selected_backend.as_str()
            })
            .width(150.0)
            .show_ui(ui, |ui| {
                ui.selectable_value(selected_backend, "All".to_owned(), "All backends");
                for backend in backends {
                    ui.selectable_value(selected_backend, backend.clone(), backend);
                }
            });
    });
}

fn recessed_panel(ui: &mut Ui, min_height: f32, add_contents: impl FnOnce(&mut Ui)) {
    let colors = ui_palette(ui);
    full_width_frame(
        ui,
        Frame::none()
            .fill(colors.panel_bg)
            .stroke(Stroke::new(1.0, colors.border_strong))
            .rounding(Rounding::same(8.0))
            .inner_margin(Margin::same(18.0)),
        |ui| {
            ui.set_min_height(min_height);
            ui.centered_and_justified(add_contents);
        },
    );
}

fn transcript_panel(ui: &mut Ui, _status: TranscriptionStatus, add_contents: impl FnOnce(&mut Ui)) {
    card(ui, add_contents);
}

fn summary_card(
    ui: &mut Ui,
    title: &str,
    body: impl FnOnce(&mut Ui),
    actions: impl FnOnce(&mut Ui),
) {
    full_width_frame(ui, card_frame(ui), |ui| {
        ui.horizontal_top(|ui| {
            ui.vertical(|ui| {
                ui.label(label_caps(title));
                ui.add_space(6.0);
                body(ui);
            });
            ui.with_layout(Layout::right_to_left(Align::TOP), actions);
        });
    });
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
            let actions_width = 92.0;
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
                        if matches!(install_status, ModelInstallStatus::Downloading { .. }) {
                            ui.add_space(8.0);
                            download_progress_bar(ui, install_status);
                        }
                        ui.add_space(8.0);
                        tag_row(ui, |ui| {
                            badge(ui, &model.backend, ChipTone::Neutral);
                            badge(ui, &format!("Runtime {}", model.backend), ChipTone::Neutral);
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
        ModelInstallStatus::Downloading { .. } => 0,
        ModelInstallStatus::Error(_) => 1,
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
    wrapped_label(ui, body_strong(&text));
    ui.add_space(4.0);
    paint_download_progress_track(ui, progress);
    if let Some(detail) = download_progress_detail(install_status) {
        ui.add_space(4.0);
        wrapped_label(ui, mut_text(detail));
    }
}

fn paint_download_progress_track(ui: &mut Ui, progress: Option<f32>) {
    let colors = ui_palette(ui);
    let width = usable_width(ui).max(1.0);
    let height = 14.0;
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, height), egui::Sense::hover());
    let track_fill = if ui.visuals().dark_mode {
        Color32::from_rgb(35, 41, 52)
    } else {
        Color32::from_rgb(229, 231, 235)
    };

    ui.painter()
        .rect_filled(rect, Rounding::same(7.0), track_fill);

    if let Some(progress) = progress {
        let fill_width = (rect.width() * progress.clamp(0.0, 1.0)).clamp(0.0, rect.width());
        if fill_width > 0.0 {
            let mut fill_rect = rect;
            fill_rect.max.x = fill_rect.min.x + fill_width;
            ui.painter()
                .rect_filled(fill_rect, Rounding::same(7.0), colors.accent);
        }
    }

    ui.painter()
        .rect_stroke(rect, Rounding::same(7.0), Stroke::new(1.0, colors.border));
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
        ModelInstallStatus::Installed => "100% Completed".to_owned(),
        ModelInstallStatus::Error(_) => "Failed".to_owned(),
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
                Some(speed) => format!("{transferred} · {}/s", format_bytes(speed)),
                None => transferred,
            })
        }
        ModelInstallStatus::Installed => Some("Installed".to_owned()),
        ModelInstallStatus::Error(message) => Some(message.clone()),
        ModelInstallStatus::Missing => Some("Missing file".to_owned()),
        ModelInstallStatus::NotInstalled => None,
    }
}

fn empty_import_panel(ui: &mut Ui, actions: impl FnOnce(&mut Ui)) {
    let colors = ui_palette(ui);
    full_width_frame(
        ui,
        Frame::none()
            .fill(colors.panel_bg)
            .stroke(Stroke::new(1.0, colors.border_strong))
            .rounding(Rounding::same(6.0))
            .inner_margin(Margin::same(18.0)),
        |ui| {
            ui.vertical_centered(|ui| {
                ui.label(section_heading("Custom Model"));
                ui.label(mut_text("Custom imports are planned for a later build."));
                ui.add_space(8.0);
                actions(ui);
            });
        },
    );
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
) -> Vec<PlaygroundAction> {
    let mut actions = Vec::new();

    ui.scope(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        let move_width = 28.0;
        let actions_width = 132.0;
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
                        card_state.model.id.clone(),
                        &card_state.model.name,
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
                },
            );
            ui.add_space(gap);
            ui.allocate_ui_with_layout(
                Vec2::new(actions_width, 0.0),
                Layout::top_down(Align::RIGHT),
                |ui| {
                    set_exact_width(ui, actions_width);
                    if ui.add(small_button(ui, "Clear")).clicked() {
                        actions.push(PlaygroundAction::Clear(card_state.model.id.clone()));
                    }
                    let mut enabled = card_state.model.enabled;
                    if ui
                        .checkbox(&mut enabled, "Run")
                        .on_hover_text(
                            "Turn this off to keep the model here but skip it during playground runs.",
                        )
                        .changed()
                    {
                        actions.push(PlaygroundAction::SetEnabled(
                            card_state.model.id.clone(),
                            enabled,
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
                ui.label(section_heading("Benchmark Scores"));
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

fn nav_button(ui: &mut Ui, current_tab: &mut Tab, tab: Tab) {
    let colors = ui_palette(ui);
    let selected = *current_tab == tab;
    let response = ui.add_sized(
        [ui.available_width(), 34.0],
        Button::new(RichText::new(tab.label()).color(if selected {
            colors.text
        } else {
            colors.muted_text
        }))
        .fill(if selected {
            colors.card_bg
        } else {
            colors.shell_bg
        })
        .stroke(if selected {
            Stroke::new(1.0, colors.border_strong)
        } else {
            Stroke::NONE
        })
        .rounding(Rounding::same(6.0)),
    );
    if response.clicked() {
        *current_tab = tab;
    }
}

fn sidebar_link(ui: &mut Ui, label: &str) {
    ui.label(RichText::new(label).small().weak());
}

fn primary_button<'a>(ui: &Ui, label: &'a str) -> Button<'a> {
    let colors = ui_palette(ui);
    Button::new(
        RichText::new(label)
            .color(colors.primary_button_text)
            .strong(),
    )
    .fill(colors.primary_button_bg)
    .stroke(Stroke::new(1.0, colors.primary_button_bg))
    .rounding(Rounding::same(24.0))
    .min_size(Vec2::new(190.0, 46.0))
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
    .min_size(Vec2::new(72.0, 30.0))
}

fn small_button<'a>(ui: &Ui, label: &'a str) -> Button<'a> {
    let colors = ui_palette(ui);
    Button::new(RichText::new(label).color(colors.text))
        .fill(colors.card_bg)
        .stroke(Stroke::new(1.0, colors.border_strong))
        .rounding(Rounding::same(5.0))
        .min_size(Vec2::new(68.0, 30.0))
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
            colors.success,
            Color32::from_rgb(240, 253, 244),
            Stroke::new(1.0, Color32::from_rgb(187, 247, 208)),
        ),
        ChipTone::Warning if ui.visuals().dark_mode => (
            colors.warning,
            Color32::from_rgb(69, 50, 18),
            Stroke::new(1.0, Color32::from_rgb(117, 83, 25)),
        ),
        ChipTone::Warning => (
            colors.warning,
            Color32::from_rgb(254, 252, 232),
            Stroke::new(1.0, Color32::from_rgb(254, 240, 138)),
        ),
        ChipTone::Error if ui.visuals().dark_mode => (
            colors.error,
            Color32::from_rgb(78, 29, 36),
            Stroke::new(1.0, Color32::from_rgb(127, 45, 55)),
        ),
        ChipTone::Error => (
            colors.error,
            Color32::from_rgb(254, 242, 242),
            Stroke::new(1.0, Color32::from_rgb(254, 202, 202)),
        ),
        ChipTone::Active if ui.visuals().dark_mode => (
            colors.accent,
            Color32::from_rgb(24, 48, 84),
            Stroke::new(1.0, Color32::from_rgb(42, 86, 143)),
        ),
        ChipTone::Active => (
            colors.accent,
            Color32::from_rgb(219, 234, 254),
            Stroke::new(1.0, Color32::from_rgb(191, 219, 254)),
        ),
    }
}

fn install_chip_tone(status: &ModelInstallStatus) -> ChipTone {
    match status {
        ModelInstallStatus::Installed => ChipTone::Success,
        ModelInstallStatus::Downloading { .. } => ChipTone::Active,
        ModelInstallStatus::Missing | ModelInstallStatus::Error(_) => ChipTone::Error,
        ModelInstallStatus::NotInstalled => ChipTone::Warning,
    }
}

fn runtime_chip_tone(status: &ModelRuntimeStatus) -> ChipTone {
    match status {
        ModelRuntimeStatus::Ready => ChipTone::Success,
        ModelRuntimeStatus::Running => ChipTone::Active,
        ModelRuntimeStatus::Disabled
        | ModelRuntimeStatus::NotImplemented
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
        ModelRuntimeStatus::Disabled => "Enable this model before transcribing.".to_owned(),
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

fn model_install_detail(
    model: &SttModelInfo,
    install_status: &ModelInstallStatus,
) -> Option<String> {
    let base = format!(
        "Runtime: {} · Model storage: {}",
        model.backend,
        model_storage_estimate(model)
    );
    match install_status {
        ModelInstallStatus::NotInstalled if !supports_managed_install(model) => {
            Some(format!("{base} · Installer unavailable in this build."))
        }
        ModelInstallStatus::Downloading { .. } => {
            Some(format!("{base} · {}", install_status.label()))
        }
        ModelInstallStatus::Missing => Some(format!(
            "{base} · The configured model path is missing or incomplete. Reinstall to use this model."
        )),
        ModelInstallStatus::Error(message) => Some(format!("{base} · Install failed: {message}")),
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
    format!("Used by: {used_by}. Runtime storage: {storage}. {status_detail}")
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

fn uninstall_runtime_files(config: &AppConfig, runtime_id: &str) -> Result<bool, String> {
    let Some(install) = config.managed_runtimes.get(runtime_id) else {
        return Ok(false);
    };
    let Some(target) =
        runtime_uninstall_target(&config::runtime_storage_dir(), runtime_id, &install.path)
    else {
        return Ok(false);
    };
    if !target.exists() {
        return Ok(false);
    }

    let result = if target.is_dir() {
        fs::remove_dir_all(&target)
    } else {
        fs::remove_file(&target)
    };
    result.map_err(|err| format!("Could not delete {}: {err}", target.display()))?;
    Ok(true)
}

fn build_development_runtime_package(
    runtime_id: &str,
    backend: &str,
    package: DevelopmentRuntimePackage,
) -> Result<PathBuf, String> {
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

    Ok(package.executable_path)
}

fn install_runtime_files(runtime_id: &str, packaged_executable: &Path) -> Result<PathBuf, String> {
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
    let target_root = config::runtime_storage_dir().join(runtime_id);
    if source_root == target_root {
        if installed_runtime_executable_usable(runtime_id, packaged_executable) {
            return Ok(packaged_executable.to_path_buf());
        }
        return Err(format!(
            "Runtime install found an unusable runtime at {}.",
            packaged_executable.display()
        ));
    }

    if target_root.exists() {
        let result = if target_root.is_dir() {
            fs::remove_dir_all(&target_root)
        } else {
            fs::remove_file(&target_root)
        };
        result.map_err(|err| format!("Could not replace {}: {err}", target_root.display()))?;
    }
    copy_dir_all(&source_root, &target_root)?;
    let installed_executable = target_root.join(relative_executable);
    if !installed_runtime_executable_usable(runtime_id, &installed_executable) {
        return Err(format!(
            "Runtime install did not create a usable runtime at {}.",
            installed_executable.display()
        ));
    }
    Ok(installed_executable)
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
    let parent = executable.parent()?;
    if parent.file_name().is_some_and(|name| name == "bin") {
        parent.parent().map(Path::to_path_buf)
    } else if executable.is_dir() {
        Some(executable.to_path_buf())
    } else {
        parent.parent().map(Path::to_path_buf)
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
        let file_type = entry
            .file_type()
            .map_err(|err| format!("Could not inspect {}: {err}", source_path.display()))?;
        if file_type.is_dir() {
            copy_dir_all(&source_path, &target_path)?;
        } else {
            fs::copy(&source_path, &target_path).map_err(|err| {
                format!(
                    "Could not copy {} to {}: {err}",
                    source_path.display(),
                    target_path.display()
                )
            })?;
        }
    }
    Ok(())
}

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

fn set_model_enabled(config: &mut AppConfig, model_id: &str, enabled: bool) {
    if enabled {
        if !config
            .playground_enabled_models
            .iter()
            .any(|id| id == model_id)
        {
            config.playground_enabled_models.push(model_id.to_owned());
        }
    } else {
        config.playground_enabled_models.retain(|id| id != model_id);
    }
}

fn set_all_models_enabled(config: &mut AppConfig, enabled: bool) {
    if enabled {
        config.playground_enabled_models = config::configured_models(config)
            .into_iter()
            .map(|model| model.id)
            .collect();
    } else {
        config.playground_enabled_models.clear();
    }
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

fn cards_from_config(config: &AppConfig) -> Vec<PlaygroundCardState> {
    config::configured_models_for_playground(config)
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
    if !model.enabled {
        return ModelRuntimeStatus::Disabled;
    }

    let Some(provider) = stt::provider_for_backend(&model.backend) else {
        return ModelRuntimeStatus::Error(format!("unsupported STT backend: {}", model.backend));
    };

    match provider.model_install_status(model) {
        ModelInstallStatus::Installed => provider.runtime_status(config),
        ModelInstallStatus::Downloading { .. } => ModelRuntimeStatus::Downloading,
        ModelInstallStatus::NotInstalled => ModelRuntimeStatus::NotInstalled,
        ModelInstallStatus::Missing => ModelRuntimeStatus::MissingConfiguration,
        ModelInstallStatus::Error(message) => ModelRuntimeStatus::Error(message),
    }
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
        for tab in [Tab::Transcribe, Tab::Models, Tab::Playground, Tab::Settings] {
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
    fn active_model_can_stay_pinned_when_disabled() {
        let mut config = AppConfig::default();
        let active_model = config.selected_default_model.clone();

        set_model_enabled(&mut config, &active_model, false);
        config::normalize_config(&mut config);

        assert_eq!(config.selected_default_model, active_model);
        assert!(
            !config
                .playground_enabled_models
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
    fn set_all_models_enabled_toggles_every_catalog_model() {
        let mut config = AppConfig::default();

        set_all_models_enabled(&mut config, false);
        config::normalize_config(&mut config);
        assert!(config.playground_enabled_models.is_empty());
        assert!(
            config::configured_models(&config)
                .iter()
                .all(|model| !model.enabled)
        );

        set_all_models_enabled(&mut config, true);
        config::normalize_config(&mut config);
        assert!(!config.playground_enabled_models.is_empty());
        assert!(
            config::configured_models(&config)
                .iter()
                .all(|model| model.enabled)
        );
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

    fn render_app_tab(tab: Tab, width: f32) -> egui::FullOutput {
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
        let colors = theme_palette(ctx);
        egui::SidePanel::left("test-navigation")
            .frame(
                Frame::none()
                    .fill(colors.sidebar_bg)
                    .stroke(Stroke::new(1.0, colors.border))
                    .inner_margin(Margin::symmetric(14.0, 16.0)),
            )
            .resizable(false)
            .exact_width(200.0)
            .show(ctx, |ui| {
                ui.label("Scribe");
                nav_button(ui, current_tab, Tab::Transcribe);
                nav_button(ui, current_tab, Tab::Models);
                nav_button(ui, current_tab, Tab::Playground);
                nav_button(ui, current_tab, Tab::Settings);
            });
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
            playground_cards: cards_from_config(&config),
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
            enabled: true,
        }
    }

    fn write_vosk_runtime(root: &Path) -> PathBuf {
        write_vosk_runtime_with_revision(root, 3)
    }

    fn write_vosk_runtime_with_revision(root: &Path, runner_revision: u32) -> PathBuf {
        let executable = root.join("bin").join("scribe-vosk");
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
        let executable = root.join("bin").join(wrapper);
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
            Some("256 MB / 1.0 GB · 4 MB/s".to_owned())
        );
    }

    #[test]
    fn download_progress_bar_paints_proportional_fill_in_full_track() {
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

        let track_fill = Color32::from_rgb(229, 231, 235);
        let track = output
            .shapes
            .iter()
            .filter_map(|shape| match &shape.shape {
                egui::Shape::Rect(rect) if rect.fill == track_fill => Some(rect.rect),
                _ => None,
            })
            .max_by(|a, b| a.width().total_cmp(&b.width()))
            .unwrap();
        let fill = output
            .shapes
            .iter()
            .filter_map(|shape| match &shape.shape {
                egui::Shape::Rect(rect) if rect.fill == ThemePalette::light().accent => {
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
            config::ManagedRuntimeInstall::new(whisper_runtime),
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
            RuntimeActionState {
                kind: RuntimeActionKind::Install,
                enabled: true,
                disabled_tooltip: None,
            }
        );

        let mut vosk = test_model();
        vosk.id = "vosk_small_en".to_owned();
        vosk.name = "Vosk small English".to_owned();
        vosk.backend = "Vosk".to_owned();
        vosk.download_model = Some("vosk-model-small-en-us-0.15".to_owned());

        assert_eq!(
            runtime_action_state(&AppConfig::default(), &vosk),
            RuntimeActionState {
                kind: RuntimeActionKind::Install,
                enabled: true,
                disabled_tooltip: None,
            }
        );

        config.managed_runtimes.clear();
        config.managed_runtimes.insert(
            "vosk".to_owned(),
            config::ManagedRuntimeInstall::new(write_vosk_runtime(&runtime_root.join("vosk"))),
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
                RuntimeActionState {
                    kind: RuntimeActionKind::Install,
                    enabled: true,
                    disabled_tooltip: None,
                },
                "{backend} should be installable"
            );

            config.managed_runtimes.clear();
            config.managed_runtimes.insert(
                runtime_id.to_owned(),
                config::ManagedRuntimeInstall::new(write_sherpa_family_runtime(
                    &runtime_root.join(runtime_id),
                    runtime_id,
                    wrapper,
                )),
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
        assert!(action.enabled);

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
        assert!(action.enabled);
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

        assert_eq!(installed, executable);
        assert!(installed.exists());
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
