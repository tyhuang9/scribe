use std::collections::HashMap;
use std::fs;
use std::io::{Read, Write};
use std::path::Path;
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

use arboard::Clipboard;
use crossbeam_channel::{Receiver, Sender, unbounded};
use eframe::egui::{
    self, Align, Button, Color32, ComboBox, FontFamily, FontId, Frame, Layout, Margin, RichText,
    Rounding, ScrollArea, Stroke, TextEdit, TextStyle, Ui, Vec2, ViewportCommand,
};
use rfd::FileDialog;

use crate::audio::{self, RecordingSession};
use crate::benchmark::{
    self, BenchmarkMetric, BenchmarkModelInput, BenchmarkModelResult, RankingMode,
};
use crate::config::{self, AppConfig, HotkeyMode, ThemeMode, WhisperComputeMode};
use crate::hotkey::{HotkeyEvent, HotkeyService};
use crate::models::{
    ModelInstallStatus, ModelRuntimeStatus, SttModelInfo, TranscriptResult, TranscriptionStatus,
    backend_capabilities, whisper_cpp_download_url,
};
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
    },
    ModelDownloadDone {
        model_id: String,
    },
    ModelDownloadFailed {
        model_id: String,
        message: String,
    },
}

pub struct LocalTranscriberApp {
    config: AppConfig,
    config_path: Option<PathBuf>,
    current_tab: Tab,
    status: TranscriptionStatus,
    transcript: String,
    status_message: String,
    hotkey_input: String,
    whisper_path_input: String,
    whisper_cuda_backend_path_input: String,
    whisper_cuda_library_paths_input: String,
    model_storage_dir_input: String,
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
        cc.egui_ctx.set_visuals(stitch_visuals(ThemeMode::Light));
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

        let (tx, rx) = unbounded();
        let mut app = Self {
            hotkey_input: config.hotkey.clone(),
            whisper_path_input: config
                .whisper_executable_path
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_default(),
            whisper_cuda_backend_path_input: config
                .whisper_cuda_backend_path
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_default(),
            whisper_cuda_library_paths_input: path_list_input(&config.whisper_cuda_library_paths),
            model_storage_dir_input: config::model_storage_dir(&config).display().to_string(),
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

    fn enabled_models(&self) -> Vec<SttModelInfo> {
        config::enabled_models(&self.config)
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

    fn apply_whisper_path_input(&mut self) {
        if update_whisper_executable_path_from_input(&mut self.config, &self.whisper_path_input) {
            self.save_config();
        }
    }

    fn apply_cuda_backend_path_input(&mut self) {
        if update_cuda_backend_path_from_input(
            &mut self.config,
            &self.whisper_cuda_backend_path_input,
        ) {
            self.save_config();
        }
    }

    fn apply_cuda_library_paths_input(&mut self) {
        if update_cuda_library_paths_from_input(
            &mut self.config,
            &self.whisper_cuda_library_paths_input,
        ) {
            self.save_config();
        }
    }

    fn apply_model_storage_dir_input(&mut self) {
        if path_from_input(&self.model_storage_dir_input).is_none() {
            self.status = TranscriptionStatus::Error;
            self.status_message = "Model storage path is required.".to_owned();
            return;
        }
        if update_model_storage_dir_from_input(&mut self.config, &self.model_storage_dir_input) {
            self.save_config();
        }
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
        if let Some(active) = self.active_recording.as_mut() {
            if !active.stop_requested {
                active.session.stop();
                active.stop_requested = true;
                active.latency.stop_requested_at = Some(Instant::now());
                self.status_message = "Stopping recording".to_owned();
            }
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
                } => {
                    self.model_downloads.insert(
                        model_id,
                        ModelInstallStatus::Downloading {
                            downloaded_bytes,
                            total_bytes,
                        },
                    );
                }
                AppEvent::ModelDownloadDone { model_id } => {
                    self.model_downloads
                        .insert(model_id.clone(), ModelInstallStatus::Installed);
                    self.save_config();
                    self.status = TranscriptionStatus::Idle;
                    self.status_message = "Model downloaded and ready.".to_owned();
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
                    if !self.config.debug_mode {
                        if let Some(path) = self.playground_audio_path.take() {
                            let _ = fs::remove_file(path);
                        }
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
        let delete_after = !self.config.debug_mode;

        thread::spawn(move || {
            let result = stt::transcribe_with_config(&config, audio_path.clone(), model.clone());
            latency.transcription_completed_at = Some(Instant::now());
            if delete_after {
                let _ = fs::remove_file(&audio_path);
            }

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

    fn apply_theme(&self, ctx: &egui::Context) {
        ctx.set_visuals(stitch_visuals(self.config.theme_mode));
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
        if !self.config.playground_enabled_models.iter().any(|id| id == &model.id) {
            self.config.playground_enabled_models.push(model.id.clone());
        }
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

        if !backend_capabilities(&model.backend).supports_downloads {
            self.status = TranscriptionStatus::Error;
            self.status_message = format!("{} cannot be downloaded in this build.", model.name);
            return;
        }

        let Some(destination) = config::downloaded_model_path(&self.config, model) else {
            self.status = TranscriptionStatus::Error;
            self.status_message = "No model storage directory is configured.".to_owned();
            return;
        };

        self.model_downloads.insert(
            model.id.clone(),
            ModelInstallStatus::Downloading {
                downloaded_bytes: 0,
                total_bytes: None,
            },
        );
        self.status = TranscriptionStatus::Idle;
        self.status_message = format!("Downloading {}...", model.name);

        let tx = self.tx.clone();
        let model_id = model.id.clone();
        let url = whisper_cpp_download_url(&download_model);
        thread::spawn(move || {
            let result = download_model_file(&url, &destination, &tx, &model_id);
            match result {
                Ok(()) => {
                    let _ = tx.send(AppEvent::ModelDownloadDone { model_id });
                }
                Err(message) => {
                    let _ = tx.send(AppEvent::ModelDownloadFailed { model_id, message });
                }
            }
        });
    }
}

impl eframe::App for LocalTranscriberApp {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        CONTENT_BG.to_normalized_gamma_f32()
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.apply_theme(ctx);
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
                    .fill(SIDEBAR_BG)
                    .stroke(Stroke::new(1.0, BORDER))
                    .inner_margin(Margin::symmetric(14.0, 16.0)),
            )
            .resizable(false)
            .exact_width(200.0)
            .show(ctx, |ui| {
                ui.label(
                    RichText::new("Scribe")
                        .font(FontId::proportional(20.0))
                        .color(PRIMARY)
                        .strong(),
                );
                ui.label(RichText::new("Local-First STT").small().color(MUTED_TEXT));
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
            .frame(content_panel_frame())
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
                        if ui.add(small_button("Change")).clicked() {
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
                        if ui.add(small_button("Edit")).clicked() {
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
                                if ui.add(small_button(label)).clicked() {
                                    self.select_model_as_default(&model);
                                    self.start_model_download(&model);
                                }
                            }
                        }
                        if ui.add(small_button("Manage models")).clicked() {
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
                    if ui
                        .add_enabled(listening || ready, primary_button(button_text))
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
                                .color(MUTED_TEXT),
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
                    if ui.add(small_button("Clear")).clicked() {
                        self.transcript.clear();
                    }
                    if ui.add(small_button("Copy")).clicked() {
                        self.copy_transcript_to_clipboard();
                    }
                });
            });
        });
    }

    fn ui_models(&mut self, ui: &mut Ui) {
        let mut backends = config::configured_models(&self.config)
            .iter()
            .map(|model| model.backend.clone())
            .collect::<Vec<_>>();
        backends.sort();
        backends.dedup();

        let status = self.status;
        let status_message = self.status_message.clone();
        page(ui, "Models Catalog", status, &status_message, |ui| {
            panel(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.label(label_caps("Search"));
                    ui.add(
                        TextEdit::singleline(&mut self.model_search)
                            .desired_width(width_before_trailing(ui, 290.0, 120.0))
                            .hint_text("Search models..."),
                    );
                    ui.label(label_caps("Filter Backend:"));
                    ComboBox::from_id_source("model-backend-filter")
                        .selected_text(if self.model_backend_filter == "All" {
                            "All Backends"
                        } else {
                            &self.model_backend_filter
                        })
                        .width(130.0)
                        .show_ui(ui, |ui| {
                            ui.selectable_value(
                                &mut self.model_backend_filter,
                                "All".to_owned(),
                                "All backends",
                            );
                            for backend in &backends {
                                ui.selectable_value(
                                    &mut self.model_backend_filter,
                                    backend.clone(),
                                    backend,
                                );
                            }
                        });
                    if ui.add(small_button("Enable All")).clicked() {
                        self.set_all_models_enabled(true);
                    }
                    if ui.add(small_button("Disable All")).clicked() {
                        self.set_all_models_enabled(false);
                    }
                });
            });

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
                let supports_download = model.download_model.is_some()
                    && backend_capabilities(&model.backend).supports_downloads;
                let can_download = supports_download
                    && !matches!(
                        install_status,
                        ModelInstallStatus::Downloading { .. } | ModelInstallStatus::Installed
                    );
                let mut enabled_update = None;
                let mut select_default = false;
                let mut start_download = false;

                model_catalog_row(ui, &model, &install_status, selected, |ui| {
                    let mut enabled = model.enabled;
                    if ui.checkbox(&mut enabled, "Enabled").changed() {
                        enabled_update = Some(enabled);
                    }

                    if ui
                        .add_enabled(
                            !selected,
                            primary_small_button(if selected { "Active" } else { "Select" }),
                        )
                        .clicked()
                    {
                        select_default = true;
                    }

                    if supports_download {
                        let download_label = match &install_status {
                            ModelInstallStatus::Downloading { .. } => "Downloading",
                            ModelInstallStatus::Installed => "Installed",
                            ModelInstallStatus::Error(_) => "Retry",
                            ModelInstallStatus::Missing | ModelInstallStatus::NotInstalled => {
                                "Download"
                            }
                        };
                        if ui
                            .add_enabled(can_download, small_button(download_label))
                            .clicked()
                        {
                            start_download = true;
                        }
                    } else if !backend_capabilities(&model.backend).runnable {
                        let _ = ui.add_enabled(false, small_button("Planned"));
                    }
                });

                if let Some(enabled) = enabled_update {
                    set_model_enabled(&mut self.config, &model.id, enabled);
                    self.save_config();
                }
                if select_default {
                    self.select_model_as_default(&model);
                }
                if start_download {
                    self.start_model_download(&model);
                }
                ui.add_space(8.0);
            }

            empty_import_panel(ui, |ui| {
                if ui.add(small_button("Import Custom Model")).clicked() {
                    self.status_message = "Custom model import is not wired yet.".to_owned();
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
                    if ui.add(primary_small_button(text)).clicked() {
                        if self.active_recording.is_some() {
                            self.stop_recording();
                        } else {
                            self.start_recording(RecordingSource::Playground);
                        }
                    }
                    if ui.add(small_button("Clear Results")).clicked() {
                        self.clear_playground_results(true);
                    }
                    if ui.add(small_button("Enable All")).clicked() {
                        self.set_all_models_enabled(true);
                    }
                    if ui.add(small_button("Disable All")).clicked() {
                        self.set_all_models_enabled(false);
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        badge(
                            ui,
                            &format!("{} enabled", self.enabled_models().len()),
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
                        if ui.add(small_button("Use Active Output")).clicked()
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
                                    full_width_frame(ui, model_card_frame(is_active_model), |ui| {
                                        ui.dnd_drag_source(drag_id, model_id.clone(), |ui| {
                                            playground_card_ui(ui, card_state, is_active_model)
                                        })
                                        .inner
                                    })
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
                    if ui.add(small_button("Apply")).clicked() {
                        self.apply_hotkey();
                    }
                    if ui
                        .add(small_button(if self.capturing_hotkey {
                            "Listening..."
                        } else {
                            "Capture"
                        }))
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
                ui.label(section_heading("Paths"));
                ui.add_space(8.0);
                if let Some(path) = &self.config_path {
                    ui.label(
                        RichText::new(format!("Config file: {}", path.display())).color(MUTED_TEXT),
                    );
                }
                ui.add_space(8.0);
                ui.horizontal_wrapped(|ui| {
                    ui.label("whisper.cpp executable");
                    ui.add(
                        TextEdit::singleline(&mut self.whisper_path_input)
                            .desired_width(width_before_trailing(ui, 158.0, 64.0)),
                    );
                    if ui.add(small_button("Apply")).clicked() {
                        self.apply_whisper_path_input();
                    }
                    if ui.add(small_button("Browse")).clicked() {
                        if let Some(path) = FileDialog::new().pick_file() {
                            self.whisper_path_input = path.display().to_string();
                            self.config.whisper_executable_path = Some(path);
                            self.save_config();
                        }
                    }
                });
                ui.horizontal_wrapped(|ui| {
                    ui.label("whisper.cpp compute");
                    let mut compute_mode = self.config.whisper_compute_mode;
                    ComboBox::from_id_source("whisper-compute-mode")
                        .selected_text(compute_mode.label())
                        .show_ui(ui, |ui| {
                            for mode in WhisperComputeMode::ALL {
                                ui.selectable_value(&mut compute_mode, mode, mode.label());
                            }
                        });
                    if compute_mode != self.config.whisper_compute_mode {
                        self.config.whisper_compute_mode = compute_mode;
                        self.save_config();
                    }

                    ui.label("GPU device");
                    let mut gpu_device = self.config.whisper_gpu_device;
                    if ui
                        .add_enabled(
                            compute_mode.uses_gpu(),
                            egui::DragValue::new(&mut gpu_device).clamp_range(0..=16),
                        )
                        .changed()
                    {
                        self.config.whisper_gpu_device = gpu_device;
                        self.save_config();
                    }
                });
                ui.label(
                    RichText::new(
                        "CUDA mode requires a CUDA-capable whisper.cpp executable or a dynamic CUDA backend.",
                    )
                    .color(MUTED_TEXT),
                );
                ui.horizontal_wrapped(|ui| {
                    ui.label("CUDA backend");
                    ui.add_enabled(
                        self.config.whisper_compute_mode.uses_gpu(),
                        TextEdit::singleline(&mut self.whisper_cuda_backend_path_input)
                            .desired_width(width_before_trailing(ui, 158.0, 64.0)),
                    );
                    if ui
                        .add_enabled(
                            self.config.whisper_compute_mode.uses_gpu(),
                            small_button("Apply"),
                        )
                        .clicked()
                    {
                        self.apply_cuda_backend_path_input();
                    }
                    if ui
                        .add_enabled(
                            self.config.whisper_compute_mode.uses_gpu(),
                            small_button("Browse"),
                        )
                        .clicked()
                    {
                        if let Some(path) = FileDialog::new().pick_file() {
                            self.whisper_cuda_backend_path_input = path.display().to_string();
                            self.config.whisper_cuda_backend_path = Some(path);
                            self.save_config();
                        }
                    }
                });
                ui.horizontal_wrapped(|ui| {
                    ui.label("CUDA library dirs");
                    ui.add_enabled(
                        self.config.whisper_compute_mode.uses_gpu(),
                        TextEdit::singleline(&mut self.whisper_cuda_library_paths_input)
                            .desired_width(width_before_trailing(ui, 82.0, 96.0)),
                    );
                    if ui
                        .add_enabled(
                            self.config.whisper_compute_mode.uses_gpu(),
                            small_button("Apply"),
                        )
                        .clicked()
                    {
                        self.apply_cuda_library_paths_input();
                    }
                });
                ui.horizontal_wrapped(|ui| {
                    ui.label("Model storage");
                    ui.add(
                        TextEdit::singleline(&mut self.model_storage_dir_input)
                            .desired_width(width_before_trailing(ui, 158.0, 64.0)),
                    );
                    if ui.add(small_button("Apply")).clicked() {
                        self.apply_model_storage_dir_input();
                    }
                    if ui.add(small_button("Browse")).clicked() {
                        if let Some(path) = FileDialog::new().pick_folder() {
                            self.model_storage_dir_input = path.display().to_string();
                            self.config.model_storage_dir = path;
                            self.save_config();
                        }
                    }
                });
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
                    if ui.add(small_button("Refresh")).clicked() {
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
                let mut debug = self.config.debug_mode;
                if ui
                    .checkbox(&mut debug, "Keep temporary WAV files")
                    .changed()
                {
                    self.config.debug_mode = debug;
                    self.save_config();
                }
            });

            ui.add_space(12.0);
            card(ui, |ui| {
                ui.label(section_heading("Runtime"));
                ui.label(RichText::new("Models run only when transcription starts. No cloud speech service, account sync, or always-on listener is enabled.").color(MUTED_TEXT));
                if self.config.debug_mode {
                    if let Some(latency) = &self.latest_latency {
                        ui.add_space(8.0);
                        ui.label(section_heading("Last Latency"));
                        for line in latency.summary_lines() {
                            ui.label(mut_text(line));
                        }
                    }
                }
                if self.tray_service.is_none() {
                    ui.colored_label(
                        ERROR,
                        "Tray integration is unavailable in this desktop session.",
                    );
                }
                if let Some(notice) = text_output::paste_automation_notice() {
                    ui.colored_label(WARNING, notice);
                }
            });
        });
    }
}

const SHELL_BG: Color32 = Color32::from_rgb(247, 249, 251);
const CONTENT_BG: Color32 = Color32::from_rgb(247, 249, 251);
const SIDEBAR_BG: Color32 = Color32::WHITE;
const CARD_BG: Color32 = Color32::WHITE;
const ACTIVE_CARD_BG: Color32 = Color32::from_rgb(239, 246, 255);
const PLAYGROUND_RESULT_HEIGHT: f32 = 92.0;
const TEXT: Color32 = Color32::from_rgb(29, 33, 42);
const MUTED_TEXT: Color32 = Color32::from_rgb(85, 95, 109);
const BORDER: Color32 = Color32::from_rgb(226, 232, 240);
const BORDER_STRONG: Color32 = Color32::from_rgb(203, 213, 225);
const PRIMARY: Color32 = Color32::from_rgb(6, 10, 18);
const ACCENT: Color32 = Color32::from_rgb(37, 99, 235);
const SUCCESS: Color32 = Color32::from_rgb(22, 163, 74);
const WARNING: Color32 = Color32::from_rgb(202, 138, 4);
const ERROR: Color32 = Color32::from_rgb(220, 38, 38);

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
    ctx.layer_painter(egui::LayerId::background())
        .rect_filled(ctx.screen_rect(), 0.0, CONTENT_BG);
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
                        .color(PRIMARY)
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

fn content_panel_frame() -> Frame {
    Frame::none()
        .fill(CONTENT_BG)
        .inner_margin(Margin::symmetric(24.0, 0.0))
}

fn card(ui: &mut Ui, add_contents: impl FnOnce(&mut Ui)) {
    full_width_frame(ui, card_frame(), add_contents);
}

fn card_frame() -> Frame {
    Frame::none()
        .fill(CARD_BG)
        .stroke(Stroke::new(1.0, BORDER))
        .rounding(Rounding::same(6.0))
        .inner_margin(Margin::same(14.0))
}

fn model_card_frame(selected: bool) -> Frame {
    let fill = if selected { ACTIVE_CARD_BG } else { CARD_BG };
    let stroke = if selected {
        Stroke::new(1.5, ACCENT)
    } else {
        Stroke::new(1.0, BORDER)
    };
    Frame::none()
        .fill(fill)
        .stroke(stroke)
        .rounding(Rounding::same(6.0))
        .inner_margin(Margin::same(14.0))
}

fn configure_drop_zone_feedback(ui: &mut Ui) {
    let transparent = Color32::from_rgba_unmultiplied(0, 0, 0, 0);
    let transparent_stroke = Stroke::new(0.0, transparent);
    let widgets = &mut ui.visuals_mut().widgets;
    widgets.inactive.bg_fill = transparent;
    widgets.inactive.bg_stroke = transparent_stroke;
    widgets.active.bg_fill = ACTIVE_CARD_BG;
    widgets.active.bg_stroke = Stroke::new(1.5, ACCENT);
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
    full_width_frame(
        ui,
        Frame::none()
            .fill(Color32::from_rgb(248, 250, 252))
            .stroke(Stroke::new(1.0, BORDER))
            .rounding(Rounding::same(6.0))
            .inner_margin(Margin::same(14.0)),
        add_contents,
    );
}

fn recessed_panel(ui: &mut Ui, min_height: f32, add_contents: impl FnOnce(&mut Ui)) {
    full_width_frame(
        ui,
        Frame::none()
            .fill(Color32::from_rgb(248, 250, 252))
            .stroke(Stroke::new(1.0, BORDER_STRONG))
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
    full_width_frame(ui, card_frame(), |ui| {
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
    full_width_frame(ui, model_card_frame(selected), |ui| {
        ui.scope(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            let actions_width = 112.0;
            let detail_width = (ui.available_width() - actions_width - 12.0).max(0.0);
            ui.horizontal_top(|ui| {
                ui.allocate_ui_with_layout(
                    Vec2::new(detail_width, 0.0),
                    Layout::top_down(Align::LEFT),
                    |ui| {
                        set_exact_width(ui, detail_width);
                        wrapped_label(ui, card_title(&model.name, selected));
                        wrapped_label(ui, mut_text(&model.description));
                        ui.add_space(8.0);
                        tag_row(ui, |ui| {
                            badge(ui, &model.backend, ChipTone::Neutral);
                            badge(
                                ui,
                                &install_status.label(),
                                install_chip_tone(install_status),
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
                            if !backend_capabilities(&model.backend).runnable {
                                badge(ui, "Planned", ChipTone::Warning);
                            }
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

fn empty_import_panel(ui: &mut Ui, actions: impl FnOnce(&mut Ui)) {
    full_width_frame(
        ui,
        Frame::none()
            .fill(Color32::from_rgb(248, 250, 252))
            .stroke(Stroke::new(1.0, BORDER_STRONG))
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

fn playground_card_ui(
    ui: &mut Ui,
    card_state: &mut PlaygroundCardState,
    is_active_model: bool,
) -> Vec<PlaygroundAction> {
    let mut actions = Vec::new();

    ui.scope(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        let move_width = 38.0;
        let actions_width = 132.0;
        let gap = 12.0;
        let detail_width = (ui.available_width() - move_width - actions_width - gap * 2.0).max(0.0);
        ui.horizontal_top(|ui| {
            ui.allocate_ui_with_layout(
                Vec2::new(move_width, 0.0),
                Layout::top_down(Align::LEFT),
                |ui| {
                    set_exact_width(ui, move_width);
                    ui.label(label_caps("Move"));
                },
            );
            ui.add_space(gap);
            ui.allocate_ui_with_layout(
                Vec2::new(detail_width, 0.0),
                Layout::top_down(Align::LEFT),
                |ui| {
                    set_exact_width(ui, detail_width);
                    wrapped_label(ui, card_title(&card_state.model.name, is_active_model));
                    wrapped_label(ui, mut_text(&card_state.model.description));
                    ui.add_space(8.0);
                    tag_row(ui, |ui| {
                        badge(ui, &card_state.model.backend, ChipTone::Neutral);
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
                    if ui.add(small_button("Clear")).clicked() {
                        actions.push(PlaygroundAction::Clear(card_state.model.id.clone()));
                    }
                    let mut enabled = card_state.model.enabled;
                    let toggle_label = if enabled { "Enabled" } else { "Disabled" };
                    if ui.checkbox(&mut enabled, toggle_label).changed() {
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
        ui.label(RichText::new(format!("Latency: {latency} ms")).color(MUTED_TEXT));
    }

    ui.add_space(6.0);
    playground_result_editor(ui, &card_state.model.id, &card_state.transcript);

    actions
}

fn playground_result_editor(ui: &mut Ui, result_id: &str, transcript: &str) -> egui::Response {
    let width = usable_width(ui).max(0.0);
    let (rect, response) = ui.allocate_exact_size(
        Vec2::new(width, PLAYGROUND_RESULT_HEIGHT),
        egui::Sense::hover(),
    );

    ui.painter().rect_filled(rect, Rounding::same(4.0), CARD_BG);
    ui.painter()
        .rect_stroke(rect, Rounding::same(4.0), Stroke::new(1.0, BORDER));

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
                ui.label(RichText::new(transcript).color(TEXT));
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
    Frame::none()
        .fill(benchmark_heatmap_fill(score))
        .stroke(Stroke::new(1.0, BORDER))
        .rounding(Rounding::same(4.0))
        .inner_margin(Margin::symmetric(8.0, 5.0))
        .show(ui, |ui| {
            ui.set_min_width(68.0);
            ui.label(RichText::new(label).color(TEXT).strong());
        });
}

fn benchmark_heatmap_fill(score: Option<f64>) -> Color32 {
    let Some(score) = score else {
        return Color32::from_rgb(248, 250, 252);
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
    if let Some(cap) = ui.data(|data| data.get_temp::<f32>(usable_width_cap_id())) {
        if cap.is_finite() && cap > 0.0 {
            width = Some(cap);
        }
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
    let selected = *current_tab == tab;
    let response = ui.add_sized(
        [ui.available_width(), 34.0],
        Button::new(RichText::new(tab.label()).color(if selected { TEXT } else { MUTED_TEXT }))
            .fill(if selected { CARD_BG } else { SHELL_BG })
            .stroke(if selected {
                Stroke::new(1.0, BORDER_STRONG)
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
    ui.label(RichText::new(label).small().color(MUTED_TEXT));
}

fn primary_button(label: &str) -> Button<'_> {
    Button::new(RichText::new(label).color(Color32::WHITE).strong())
        .fill(PRIMARY)
        .stroke(Stroke::new(1.0, PRIMARY))
        .rounding(Rounding::same(24.0))
        .min_size(Vec2::new(190.0, 46.0))
}

fn primary_small_button(label: &str) -> Button<'_> {
    Button::new(RichText::new(label).color(Color32::WHITE).strong())
        .fill(PRIMARY)
        .stroke(Stroke::new(1.0, PRIMARY))
        .rounding(Rounding::same(5.0))
        .min_size(Vec2::new(82.0, 30.0))
}

fn small_button(label: &str) -> Button<'_> {
    Button::new(RichText::new(label).color(TEXT))
        .fill(CARD_BG)
        .stroke(Stroke::new(1.0, BORDER_STRONG))
        .rounding(Rounding::same(5.0))
        .min_size(Vec2::new(68.0, 30.0))
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
    let (text, fill, stroke) = chip_colors(tone);
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
        .color(MUTED_TEXT)
        .strong()
}

fn section_heading(label: &str) -> RichText {
    RichText::new(label)
        .font(FontId::proportional(16.0))
        .color(PRIMARY)
        .strong()
}

fn body_strong(label: &str) -> RichText {
    RichText::new(label)
        .font(FontId::proportional(15.0))
        .color(TEXT)
        .strong()
}

fn card_title(label: &str, active: bool) -> RichText {
    RichText::new(label)
        .font(FontId::proportional(15.0))
        .color(if active { ACCENT } else { TEXT })
        .strong()
}

fn mut_text(label: impl Into<String>) -> RichText {
    RichText::new(label.into()).color(MUTED_TEXT)
}

fn wrapped_label(ui: &mut Ui, text: RichText) {
    ui.add(egui::Label::new(text).wrap(true));
}

fn chip_colors(tone: ChipTone) -> (Color32, Color32, Stroke) {
    match tone {
        ChipTone::Neutral => (
            MUTED_TEXT,
            Color32::from_rgb(248, 250, 252),
            Stroke::new(1.0, BORDER),
        ),
        ChipTone::Success => (
            SUCCESS,
            Color32::from_rgb(240, 253, 244),
            Stroke::new(1.0, Color32::from_rgb(187, 247, 208)),
        ),
        ChipTone::Warning => (
            WARNING,
            Color32::from_rgb(254, 252, 232),
            Stroke::new(1.0, Color32::from_rgb(254, 240, 138)),
        ),
        ChipTone::Error => (
            ERROR,
            Color32::from_rgb(254, 242, 242),
            Stroke::new(1.0, Color32::from_rgb(254, 202, 202)),
        ),
        ChipTone::Active => (
            ACCENT,
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
            "Set the whisper.cpp executable and download the selected model before transcribing."
                .to_owned()
        }
        ModelRuntimeStatus::NotInstalled => {
            "Download a local whisper.cpp model before transcribing.".to_owned()
        }
        ModelRuntimeStatus::Downloading => "The selected model is still downloading.".to_owned(),
        ModelRuntimeStatus::Running => "A transcription is already running.".to_owned(),
        ModelRuntimeStatus::Disabled => "Enable this model before transcribing.".to_owned(),
        ModelRuntimeStatus::NotImplemented => {
            "This backend is planned metadata only; choose a whisper.cpp model.".to_owned()
        }
        ModelRuntimeStatus::Error(message) => message.clone(),
    }
}

fn stitch_visuals(theme_mode: ThemeMode) -> egui::Visuals {
    let mut visuals = match theme_mode {
        ThemeMode::Dark => egui::Visuals::dark(),
        ThemeMode::Light | ThemeMode::System => egui::Visuals::light(),
    };
    visuals.override_text_color = Some(match theme_mode {
        ThemeMode::Dark => Color32::from_rgb(236, 241, 247),
        ThemeMode::Light | ThemeMode::System => TEXT,
    });
    visuals.selection.bg_fill = ACCENT;
    visuals.hyperlink_color = ACCENT;
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, BORDER);
    visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, BORDER);
    visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, BORDER_STRONG);
    visuals.widgets.active.bg_stroke = Stroke::new(1.0, ACCENT);
    visuals
}

fn path_from_input(input: &str) -> Option<PathBuf> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(PathBuf::from(trimmed))
    }
}

fn update_whisper_executable_path_from_input(config: &mut AppConfig, input: &str) -> bool {
    let next = path_from_input(input);
    if config.whisper_executable_path == next {
        return false;
    }
    config.whisper_executable_path = next;
    true
}

fn update_cuda_backend_path_from_input(config: &mut AppConfig, input: &str) -> bool {
    let next = path_from_input(input);
    if config.whisper_cuda_backend_path == next {
        return false;
    }
    config.whisper_cuda_backend_path = next;
    true
}

fn update_cuda_library_paths_from_input(config: &mut AppConfig, input: &str) -> bool {
    let next = paths_from_list_input(input);
    if config.whisper_cuda_library_paths == next {
        return false;
    }
    config.whisper_cuda_library_paths = next;
    true
}

fn update_model_storage_dir_from_input(config: &mut AppConfig, input: &str) -> bool {
    let Some(next) = path_from_input(input) else {
        return false;
    };
    if config.model_storage_dir == next {
        return false;
    }
    config.model_storage_dir = next;
    true
}

fn path_list_input(paths: &[PathBuf]) -> String {
    std::env::join_paths(paths)
        .ok()
        .and_then(|paths| paths.into_string().ok())
        .unwrap_or_else(|| {
            paths
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(":")
        })
}

fn paths_from_list_input(input: &str) -> Vec<PathBuf> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        Vec::new()
    } else {
        std::env::split_paths(trimmed).collect()
    }
}

fn set_model_enabled(config: &mut AppConfig, model_id: &str, enabled: bool) {
    if enabled {
        if !config.playground_enabled_models.iter().any(|id| id == model_id) {
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
        ModelRuntimeStatus::Disabled
    } else if !backend_capabilities(&model.backend).runnable {
        ModelRuntimeStatus::NotImplemented
    } else if model.install_status.is_runnable() {
        let executable_ready = config
            .whisper_executable_path
            .as_ref()
            .is_some_and(|path| path.exists());
        if executable_ready {
            ModelRuntimeStatus::Ready
        } else {
            ModelRuntimeStatus::MissingConfiguration
        }
    } else {
        match &model.install_status {
            ModelInstallStatus::Installed => ModelRuntimeStatus::Ready,
            ModelInstallStatus::Downloading { .. } => ModelRuntimeStatus::Downloading,
            ModelInstallStatus::NotInstalled => ModelRuntimeStatus::NotInstalled,
            ModelInstallStatus::Missing => ModelRuntimeStatus::MissingConfiguration,
            ModelInstallStatus::Error(message) => ModelRuntimeStatus::Error(message.clone()),
        }
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

fn download_model_file(
    url: &str,
    destination: &Path,
    tx: &Sender<AppEvent>,
    model_id: &str,
) -> Result<(), String> {
    if destination.exists() {
        return Ok(());
    }

    let partial_path = destination.with_extension("bin.partial");
    let result = (|| {
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)
                .map_err(|err| format!("failed to create {}: {err}", parent.display()))?;
        }

        let response = ureq::get(url)
            .call()
            .map_err(|err| format!("request failed for {url}: {err}"))?;
        let total_bytes = response
            .header("content-length")
            .and_then(|value| value.parse::<u64>().ok());
        let mut reader = response.into_reader();
        let mut file = fs::File::create(&partial_path)
            .map_err(|err| format!("failed to create {}: {err}", partial_path.display()))?;
        let mut downloaded_bytes = 0_u64;
        let mut buffer = [0_u8; 64 * 1024];

        loop {
            let read = reader
                .read(&mut buffer)
                .map_err(|err| format!("download read failed: {err}"))?;
            if read == 0 {
                break;
            }
            file.write_all(&buffer[..read])
                .map_err(|err| format!("failed to write {}: {err}", partial_path.display()))?;
            downloaded_bytes += read as u64;
            let _ = tx.send(AppEvent::ModelDownloadProgress {
                model_id: model_id.to_owned(),
                downloaded_bytes,
                total_bytes,
            });
        }

        file.sync_all()
            .map_err(|err| format!("failed to finish {}: {err}", partial_path.display()))?;
        fs::rename(&partial_path, destination).map_err(|err| {
            format!(
                "failed to move {} to {}: {err}",
                partial_path.display(),
                destination.display()
            )
        })?;
        Ok(())
    })();

    if result.is_err() {
        let _ = fs::remove_file(&partial_path);
    }

    result
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
        let clear_color = eframe::App::clear_color(&app, &egui::Visuals::light());

        assert_eq!(clear_color, CONTENT_BG.to_normalized_gamma_f32());
        assert_eq!(clear_color[3], 1.0);
    }

    #[test]
    fn active_model_can_stay_pinned_when_disabled() {
        let mut config = AppConfig::default();
        let active_model = config.selected_default_model.clone();

        set_model_enabled(&mut config, &active_model, false);
        config::normalize_config(&mut config);

        assert_eq!(config.selected_default_model, active_model);
        assert!(!config.playground_enabled_models.iter().any(|id| id == &active_model));
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
            },
        );
        assert_eq!(app.next_repaint_delay(), ACTIVE_REPAINT_DELAY);
    }

    #[test]
    fn path_input_buffers_update_config_only_when_applied() {
        let mut app = test_app();
        let before = app.config.whisper_executable_path.clone();
        app.whisper_path_input = "/tmp/scribe-whisper".to_owned();
        assert_eq!(app.config.whisper_executable_path, before);

        assert!(update_whisper_executable_path_from_input(
            &mut app.config,
            &app.whisper_path_input
        ));
        assert_eq!(
            app.config.whisper_executable_path,
            Some(PathBuf::from("/tmp/scribe-whisper"))
        );
        assert!(!update_whisper_executable_path_from_input(
            &mut app.config,
            &app.whisper_path_input
        ));

        assert!(update_whisper_executable_path_from_input(
            &mut app.config,
            " "
        ));
        assert_eq!(app.config.whisper_executable_path, None);
    }

    #[test]
    fn path_apply_helpers_report_real_config_changes() {
        let mut config = AppConfig::default();
        let original_storage_dir = config.model_storage_dir.clone();

        assert!(update_cuda_backend_path_from_input(
            &mut config,
            "/tmp/libggml-cuda.so"
        ));
        assert_eq!(
            config.whisper_cuda_backend_path,
            Some(PathBuf::from("/tmp/libggml-cuda.so"))
        );

        let cuda_dirs = std::env::join_paths([
            PathBuf::from("/tmp/cuda-one"),
            PathBuf::from("/tmp/cuda-two"),
        ])
        .expect("test CUDA paths should join")
        .into_string()
        .expect("test CUDA paths should be UTF-8");
        assert!(update_cuda_library_paths_from_input(
            &mut config,
            &cuda_dirs
        ));
        assert_eq!(
            config.whisper_cuda_library_paths,
            vec![
                PathBuf::from("/tmp/cuda-one"),
                PathBuf::from("/tmp/cuda-two")
            ]
        );
        assert!(!update_cuda_library_paths_from_input(
            &mut config,
            &cuda_dirs
        ));

        assert!(!update_model_storage_dir_from_input(&mut config, " "));
        assert_eq!(config.model_storage_dir, original_storage_dir);
        assert!(update_model_storage_dir_from_input(
            &mut config,
            "/tmp/scribe-models"
        ));
        assert_eq!(
            config.model_storage_dir,
            PathBuf::from("/tmp/scribe-models")
        );
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
                .frame(content_panel_frame())
                .show(ctx, |ui| {
                    page(
                        ui,
                        "Models Catalog",
                        TranscriptionStatus::Idle,
                        "Ready",
                        |ui| {
                            panel(ui, |ui| {
                                ui.horizontal_wrapped(|ui| {
                                    ui.label(label_caps("Search"));
                                    let mut search = "whisper".to_owned();
                                    ui.add(
                                        TextEdit::singleline(&mut search)
                                            .desired_width(width_before_trailing(ui, 290.0, 120.0))
                                            .hint_text("Search models..."),
                                    );
                                    ui.label(label_caps("Filter Backend:"));
                                    ComboBox::from_id_source("test-model-backend-filter")
                                        .selected_text("All Backends")
                                        .width(130.0)
                                        .show_ui(ui, |ui| {
                                            ui.label("All backends");
                                        });
                                });
                            });

                            ui.add_space(12.0);
                            let model = test_model();
                            let install_status = ModelInstallStatus::Installed;
                            let mut enabled = true;

                            model_catalog_row(ui, &model, &install_status, true, |ui| {
                                let _ = ui.checkbox(&mut enabled, "Enabled");
                                let _ = ui.add_enabled(false, primary_small_button("Active"));
                                let _ = ui.add_enabled(false, small_button("Installed"));
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
                .frame(content_panel_frame())
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
                .frame(content_panel_frame())
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
                .frame(content_panel_frame())
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
                .frame(content_panel_frame())
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
                    .frame(content_panel_frame())
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
        egui::SidePanel::left("test-navigation")
            .frame(
                Frame::none()
                    .fill(SIDEBAR_BG)
                    .stroke(Stroke::new(1.0, BORDER))
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
            whisper_path_input: config
                .whisper_executable_path
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_default(),
            whisper_cuda_backend_path_input: config
                .whisper_cuda_backend_path
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_default(),
            whisper_cuda_library_paths_input: path_list_input(&config.whisper_cuda_library_paths),
            model_storage_dir_input: config::model_storage_dir(&config).display().to_string(),
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
}
