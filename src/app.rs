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
    self, Align, Button, Color32, ComboBox, FontId, Frame, Layout, Margin, RichText, Rounding,
    ScrollArea, Stroke, TextEdit, Ui, Vec2, ViewportCommand,
};
use rfd::FileDialog;

use crate::audio::{self, RecordingSession};
use crate::config::{self, AppConfig, ThemeMode};
use crate::hotkey::HotkeyService;
use crate::models::{
    ModelInstallStatus, ModelRuntimeStatus, SttModelInfo, TranscriptResult, TranscriptionStatus,
    backend_capabilities, whisper_cpp_download_url,
};
use crate::stt;
use crate::text_output;
use crate::tray::{TrayCommand, TrayService};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Tab {
    Transcribe,
    Models,
    Playground,
    Settings,
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
}

#[derive(Clone, Debug)]
struct PlaygroundCardState {
    model: SttModelInfo,
    status: ModelRuntimeStatus,
    transcript: String,
    latency_ms: Option<u128>,
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
    },
    TranscriptionFailed {
        source: RecordingSource,
        model_id: String,
        message: String,
    },
    ModelDownloadProgress {
        model_id: String,
        downloaded_bytes: u64,
        total_bytes: Option<u64>,
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

pub struct LocalTranscriberApp {
    config: AppConfig,
    config_path: Option<PathBuf>,
    current_tab: Tab,
    status: TranscriptionStatus,
    transcript: String,
    status_message: String,
    hotkey_input: String,
    whisper_path_input: String,
    model_storage_dir_input: String,
    model_search: String,
    model_backend_filter: String,
    audio_devices: Vec<String>,
    capturing_hotkey: bool,
    model_path_inputs: HashMap<String, String>,
    model_downloads: HashMap<String, ModelInstallStatus>,
    active_recording: Option<ActiveRecording>,
    tx: Sender<AppEvent>,
    rx: Receiver<AppEvent>,
    playground_cards: Vec<PlaygroundCardState>,
    playground_pending: usize,
    playground_audio_path: Option<PathBuf>,
    hotkey_service: HotkeyService,
    tray_service: Option<TrayService>,
    quit_requested: bool,
}

impl LocalTranscriberApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        cc.egui_ctx.set_visuals(stitch_visuals(ThemeMode::Light));

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
            model_storage_dir_input: config::model_storage_dir(&config).display().to_string(),
            model_search: String::new(),
            model_backend_filter: "All".to_owned(),
            audio_devices: Vec::new(),
            capturing_hotkey: false,
            model_path_inputs: config
                .model_paths
                .iter()
                .map(|(id, path)| (id.clone(), path.display().to_string()))
                .collect(),
            model_downloads: HashMap::new(),
            playground_cards: cards_from_config(&config),
            hotkey_service: HotkeyService::new(&config.hotkey),
            config,
            config_path,
            current_tab: Tab::Transcribe,
            status: TranscriptionStatus::Idle,
            transcript: String::new(),
            status_message,
            active_recording: None,
            tx,
            rx,
            playground_pending: 0,
            playground_audio_path: None,
            tray_service: None,
            quit_requested: false,
        };

        match TrayService::new(false, false) {
            Ok(tray_service) => {
                app.tray_service = Some(tray_service);
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

    fn apply_playground_action(&mut self, action: PlaygroundAction) {
        match action {
            PlaygroundAction::Clear(model_id) => {
                if let Some(card) = self
                    .playground_cards
                    .iter_mut()
                    .find(|card| card.model.id == model_id)
                {
                    card.transcript.clear();
                    card.latency_ms = None;
                }
            }
            PlaygroundAction::SetEnabled(model_id, enabled) => {
                set_model_enabled(&mut self.config, &model_id, enabled);
                if !enabled {
                    if let Some(card) = self
                        .playground_cards
                        .iter_mut()
                        .find(|card| card.model.id == model_id)
                    {
                        card.transcript.clear();
                        card.latency_ms = None;
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
                let path = session.audio_path.display().to_string();
                self.active_recording = Some(ActiveRecording {
                    session,
                    source,
                    stop_requested: false,
                    started_at: Instant::now(),
                    max_duration_seconds: self.config.max_recording_seconds,
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
            self.active_recording = None;
            match result {
                Ok(audio_path) => {
                    self.status = TranscriptionStatus::Transcribing;
                    self.status_message = format!("Transcribing {}", audio_path.display());
                    match source {
                        RecordingSource::Transcribe => {
                            self.dispatch_default_transcription(audio_path)
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
        if self.hotkey_service.poll_pressed() {
            self.toggle_recording();
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

    fn sync_tray_state(&self) {
        if let Some(tray_service) = &self.tray_service {
            tray_service.set_recording(self.active_recording.is_some());
            tray_service.set_has_transcript(!self.transcript.trim().is_empty());
        }
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
                AppEvent::TranscriptionDone { source, result } => {
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
                                self.status_message = format!(
                                    "{completion_message}. {}",
                                    output_result.status_message()
                                );
                            } else {
                                self.status_message = completion_message;
                            }
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
                } => {
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
                AppEvent::ModelDownloadDone { model_id, path } => {
                    self.model_downloads
                        .insert(model_id.clone(), ModelInstallStatus::Installed);
                    self.config
                        .model_paths
                        .insert(model_id.clone(), path.clone());
                    self.model_path_inputs
                        .insert(model_id.clone(), path.display().to_string());
                    if self.config.selected_default_model == model_id
                        && !self.config.enabled_models.iter().any(|id| id == &model_id)
                    {
                        self.config.enabled_models.push(model_id.clone());
                    }
                    self.save_config();
                    self.status = TranscriptionStatus::Idle;
                    self.status_message = format!("Model installed at {}", path.display());
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

    fn dispatch_default_transcription(&mut self, audio_path: PathBuf) {
        let Some(model) = self.selected_model() else {
            self.status = TranscriptionStatus::Error;
            self.status_message = "No default model selected".to_owned();
            let _ = fs::remove_file(audio_path);
            return;
        };

        let config = self.config.clone();
        let tx = self.tx.clone();
        let delete_after = !self.config.debug_mode;

        thread::spawn(move || {
            let result = stt::transcribe_with_config(&config, audio_path.clone(), model.clone());
            if delete_after {
                let _ = fs::remove_file(&audio_path);
            }

            match result {
                Ok(result) => {
                    let _ = tx.send(AppEvent::TranscriptionDone {
                        source: RecordingSource::Transcribe,
                        result,
                    });
                }
                Err(err) => {
                    let _ = tx.send(AppEvent::TranscriptionFailed {
                        source: RecordingSource::Transcribe,
                        model_id: model.id,
                        message: err.to_string(),
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
                        });
                    }
                    Err(err) => {
                        let _ = tx.send(AppEvent::TranscriptionFailed {
                            source: RecordingSource::Playground,
                            model_id: model.id,
                            message: err.to_string(),
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
        }
        self.playground_pending = 0;
        self.playground_audio_path = None;
    }

    fn apply_playground_result(&mut self, result: TranscriptResult) {
        if let Some(card) = self
            .playground_cards
            .iter_mut()
            .find(|card| card.model.id == result.model_id)
        {
            card.status = ModelRuntimeStatus::Ready;
            card.transcript = result.text;
            card.latency_ms = result.duration_ms;
        }
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
        if !self.config.enabled_models.iter().any(|id| id == &model.id) {
            self.config.enabled_models.push(model.id.clone());
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
                    let _ = tx.send(AppEvent::ModelDownloadDone {
                        model_id,
                        path: destination,
                    });
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
        SHELL_BG.to_normalized_gamma_f32()
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.apply_theme(ctx);
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
            .frame(Frame::none().fill(SHELL_BG))
            .resizable(false)
            .exact_width(200.0)
            .show(ctx, |ui| {
                ui.add_space(20.0);
                ui.label(
                    RichText::new("Scribe")
                        .font(FontId::proportional(24.0))
                        .strong(),
                );
                ui.label(RichText::new("Local speech to text").color(MUTED_TEXT));
                ui.add_space(28.0);
                nav_button(ui, &mut self.current_tab, Tab::Transcribe, "Transcribe");
                nav_button(ui, &mut self.current_tab, Tab::Models, "Models");
                nav_button(ui, &mut self.current_tab, Tab::Playground, "Playground");
                nav_button(ui, &mut self.current_tab, Tab::Settings, "Settings");
                ui.with_layout(Layout::bottom_up(Align::LEFT), |ui| {
                    ui.label(RichText::new("Offline first").color(MUTED_TEXT));
                    ui.label(RichText::new("No cloud STT").color(MUTED_TEXT));
                });
            });

        egui::TopBottomPanel::bottom("footer")
            .frame(Frame::none().fill(SHELL_BG))
            .show(ctx, |ui| {
                ui.horizontal_wrapped(|ui| {
                    status_badge(ui, self.status);
                    ui.label(RichText::new(&self.status_message).color(MUTED_TEXT));
                });
            });

        egui::CentralPanel::default()
            .frame(Frame::none().fill(CONTENT_BG))
            .show(ctx, |ui| match self.current_tab {
                Tab::Transcribe => self.ui_transcribe(ui),
                Tab::Models => self.ui_models(ui),
                Tab::Playground => self.ui_playground(ui),
                Tab::Settings => self.ui_settings(ui),
            });

        ctx.request_repaint_after(Duration::from_millis(100));
    }
}

impl LocalTranscriberApp {
    fn ui_transcribe(&mut self, ui: &mut Ui) {
        page(ui, "Transcribe", |ui| {
            let selected_model = self.selected_model();
            let runtime_status = selected_model
                .as_ref()
                .map(|model| runtime_status_for_model(&self.config, model));
            let ready = runtime_status == Some(ModelRuntimeStatus::Ready);

            ui.columns(2, |columns| {
                card(&mut columns[0], |ui| {
                    ui.label(RichText::new("Current model").color(MUTED_TEXT));
                    if let Some(model) = &selected_model {
                        ui.add_space(4.0);
                        ui.heading(&model.name);
                        ui.horizontal_wrapped(|ui| {
                            badge(ui, &model.backend, ChipTone::Neutral);
                            badge(
                                ui,
                                &self.effective_install_status(model).label(),
                                install_chip_tone(&self.effective_install_status(model)),
                            );
                        });
                    } else {
                        ui.heading("No model selected");
                    }
                    ui.add_space(12.0);
                    if ui.button("Open Models").clicked() {
                        self.current_tab = Tab::Models;
                    }
                });

                card(&mut columns[1], |ui| {
                    ui.label(RichText::new("Hotkey").color(MUTED_TEXT));
                    ui.add_space(4.0);
                    ui.heading(&self.config.hotkey);
                    ui.add_space(12.0);
                    if ui.button("Edit in Settings").clicked() {
                        self.current_tab = Tab::Settings;
                    }
                });
            });

            if !ready && self.active_recording.is_none() {
                ui.add_space(12.0);
                card(ui, |ui| {
                    let setup_message = runtime_status
                        .as_ref()
                        .map(setup_message_for_status)
                        .unwrap_or_else(|| {
                            "Choose a local whisper.cpp model to start transcribing.".to_owned()
                        });
                    ui.heading("Setup required");
                    ui.label(RichText::new(setup_message).color(MUTED_TEXT));
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
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
                                if ui.button(label).clicked() {
                                    self.select_model_as_default(&model);
                                    self.start_model_download(&model);
                                }
                            }
                        }
                        if ui.button("Configure paths").clicked() {
                            self.current_tab = Tab::Models;
                        }
                    });
                });
            }

            ui.add_space(28.0);
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
                        RichText::new("Press the button or use the hotkey.").color(MUTED_TEXT),
                    );
                }
            });

            ui.add_space(28.0);
            card(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.heading("Transcript");
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.button("Clear").clicked() {
                            self.transcript.clear();
                        }
                        if ui.button("Copy").clicked() {
                            self.copy_transcript_to_clipboard();
                        }
                    });
                });
                ui.add_space(8.0);
                ui.add(
                    TextEdit::multiline(&mut self.transcript)
                        .desired_rows(20)
                        .desired_width(f32::INFINITY)
                        .hint_text("Transcript output will appear here"),
                );
            });
        });
    }

    fn ui_models(&mut self, ui: &mut Ui) {
        let all_models = config::configured_models(&self.config);
        let mut backends = all_models
            .iter()
            .map(|model| model.backend.clone())
            .collect::<Vec<_>>();
        backends.sort();
        backends.dedup();

        page(ui, "Models", |ui| {
            card(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label("whisper.cpp executable");
                    if ui
                        .text_edit_singleline(&mut self.whisper_path_input)
                        .changed()
                    {
                        self.config.whisper_executable_path =
                            path_from_input(&self.whisper_path_input);
                        self.save_config();
                    }
                    if ui.button("Browse").clicked() {
                        if let Some(path) = FileDialog::new().pick_file() {
                            self.whisper_path_input = path.display().to_string();
                            self.config.whisper_executable_path = Some(path);
                            self.save_config();
                        }
                    }
                });
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.label("Model storage");
                    if ui
                        .text_edit_singleline(&mut self.model_storage_dir_input)
                        .changed()
                    {
                        if let Some(path) = path_from_input(&self.model_storage_dir_input) {
                            self.config.model_storage_dir = path;
                            self.save_config();
                        }
                    }
                    if ui.button("Browse").clicked() {
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
                ui.horizontal(|ui| {
                    ui.label("Search");
                    ui.add(
                        TextEdit::singleline(&mut self.model_search)
                            .desired_width(220.0)
                            .hint_text("Model or backend"),
                    );
                    ComboBox::from_id_source("model-backend-filter")
                        .selected_text(&self.model_backend_filter)
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
                });
            });

            ui.add_space(12.0);
            let search = self.model_search.trim().to_ascii_lowercase();
            let models = all_models
                .into_iter()
                .filter(|model| {
                    (self.model_backend_filter == "All"
                        || self.model_backend_filter == model.backend)
                        && (search.is_empty()
                            || model.name.to_ascii_lowercase().contains(&search)
                            || model.backend.to_ascii_lowercase().contains(&search))
                })
                .collect::<Vec<_>>();

            ScrollArea::vertical().show(ui, |ui| {
                for model in models {
                    card(ui, |ui| {
                        ui.horizontal_top(|ui| {
                            let mut enabled = model.enabled;
                            if ui.checkbox(&mut enabled, "").changed() {
                                set_model_enabled(&mut self.config, &model.id, enabled);
                                self.save_config();
                            }

                            ui.vertical(|ui| {
                                ui.heading(&model.name);
                                ui.label(RichText::new(&model.description).color(MUTED_TEXT));
                                ui.add_space(4.0);
                                let install_status = self.effective_install_status(&model);
                                ui.horizontal_wrapped(|ui| {
                                    badge(ui, &model.backend, ChipTone::Neutral);
                                    badge(
                                        ui,
                                        &install_status.label(),
                                        install_chip_tone(&install_status),
                                    );
                                    badge(
                                        ui,
                                        &format!("RAM {}", model.expected_ram),
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
                            });

                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                let selected = self.config.selected_default_model == model.id;
                                if ui.selectable_label(selected, "Default").clicked() {
                                    self.select_model_as_default(&model);
                                }
                                let install_status = self.effective_install_status(&model);
                                let can_download = model.download_model.is_some()
                                    && !matches!(
                                        install_status,
                                        ModelInstallStatus::Downloading { .. }
                                            | ModelInstallStatus::Installed
                                    );
                                if ui
                                    .add_enabled(can_download, Button::new("Download"))
                                    .clicked()
                                {
                                    self.start_model_download(&model);
                                }
                            });
                        });

                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            ui.label("Model path");
                            let mut changed = false;
                            {
                                let input = self
                                    .model_path_inputs
                                    .entry(model.id.clone())
                                    .or_insert_with(|| {
                                        model
                                            .local_path
                                            .as_ref()
                                            .map(|path| path.display().to_string())
                                            .unwrap_or_default()
                                    });
                                changed |= ui.text_edit_singleline(input).changed();
                                if ui.button("Browse").clicked() {
                                    if let Some(path) = FileDialog::new().pick_file() {
                                        *input = path.display().to_string();
                                        changed = true;
                                    }
                                }
                            }
                            if changed {
                                let input = self
                                    .model_path_inputs
                                    .get(&model.id)
                                    .cloned()
                                    .unwrap_or_default();
                                if let Some(path) = path_from_input(&input) {
                                    self.config.model_paths.insert(model.id.clone(), path);
                                } else {
                                    self.config.model_paths.remove(&model.id);
                                }
                                self.save_config();
                            }
                        });
                    });
                    ui.add_space(8.0);
                }
            });
        });
    }

    fn ui_playground(&mut self, ui: &mut Ui) {
        page(ui, "Playground", |ui| {
            card(ui, |ui| {
                ui.horizontal(|ui| {
                    badge(
                        ui,
                        &format!("{} enabled", self.enabled_models().len()),
                        ChipTone::Neutral,
                    );
                    ui.label(
                        RichText::new("Record once and compare enabled local models.")
                            .color(MUTED_TEXT),
                    );
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let text = if self.active_recording.is_some() {
                            "Stop Recording"
                        } else {
                            "Start Test Recording"
                        };
                        if ui.add(Button::new(text)).clicked() {
                            if self.active_recording.is_some() {
                                self.stop_recording();
                            } else {
                                self.start_recording(RecordingSource::Playground);
                            }
                        }
                        if ui.button("Clear Results").clicked() {
                            for card in &mut self.playground_cards {
                                card.transcript.clear();
                                card.latency_ms = None;
                                card.status = runtime_status_for_model(&self.config, &card.model);
                            }
                        }
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
            let mut pending_actions = Vec::new();
            ScrollArea::vertical().show(ui, |ui| {
                if self.playground_cards.is_empty() {
                    ui.label("No models are configured.");
                }
                for card_state in &mut self.playground_cards {
                    let model_id = card_state.model.id.clone();
                    let drag_id = ui.id().with(("playground-card", &model_id));
                    let (inner, dropped_payload) =
                        ui.dnd_drop_zone::<String, _>(playground_card_frame(ui), |ui| {
                            ui.set_min_width(ui.available_width());
                            ui.dnd_drag_source(drag_id, model_id.clone(), |ui| {
                                playground_card_ui(ui, card_state)
                            })
                            .inner
                        });
                    pending_actions.extend(inner.inner);

                    if let Some(dragged_id) = dropped_payload {
                        pending_actions.push(PlaygroundAction::MoveBefore {
                            dragged_id: dragged_id.to_string(),
                            target_id: model_id,
                        });
                    }
                    ui.add_space(8.0);
                }
            });

            if let Some(action) = pending_actions.into_iter().next() {
                self.apply_playground_action(action);
            }
        });
    }

    fn ui_settings(&mut self, ui: &mut Ui) {
        page(ui, "Settings", |ui| {
            card(ui, |ui| {
                ui.heading("General");
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
                ui.horizontal(|ui| {
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
                ui.heading("Shortcuts");
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.label("Record toggle");
                    ui.text_edit_singleline(&mut self.hotkey_input);
                    if ui.button("Apply").clicked() {
                        self.apply_hotkey();
                    }
                    if ui
                        .button(if self.capturing_hotkey {
                            "Listening..."
                        } else {
                            "Capture"
                        })
                        .clicked()
                    {
                        self.capturing_hotkey = true;
                        self.status_message = "Press the new hotkey combination.".to_owned();
                    }
                });
            });

            ui.add_space(12.0);
            card(ui, |ui| {
                ui.heading("Paths");
                ui.add_space(8.0);
                if let Some(path) = &self.config_path {
                    ui.label(
                        RichText::new(format!("Config file: {}", path.display())).color(MUTED_TEXT),
                    );
                }
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.label("whisper.cpp executable");
                    if ui
                        .text_edit_singleline(&mut self.whisper_path_input)
                        .changed()
                    {
                        self.config.whisper_executable_path =
                            path_from_input(&self.whisper_path_input);
                        self.save_config();
                    }
                    if ui.button("Browse").clicked() {
                        if let Some(path) = FileDialog::new().pick_file() {
                            self.whisper_path_input = path.display().to_string();
                            self.config.whisper_executable_path = Some(path);
                            self.save_config();
                        }
                    }
                });
                ui.horizontal(|ui| {
                    ui.label("Model storage");
                    if ui
                        .text_edit_singleline(&mut self.model_storage_dir_input)
                        .changed()
                    {
                        if let Some(path) = path_from_input(&self.model_storage_dir_input) {
                            self.config.model_storage_dir = path;
                            self.save_config();
                        }
                    }
                    if ui.button("Browse").clicked() {
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
                ui.heading("Audio");
                ui.add_space(8.0);
                ui.horizontal(|ui| {
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
                    if ui.button("Refresh").clicked() {
                        self.refresh_audio_devices();
                    }
                });
                let mut max_duration = self.config.max_recording_seconds as i32;
                ui.horizontal(|ui| {
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
                ui.heading("Appearance");
                ui.add_space(8.0);
                ui.horizontal(|ui| {
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
                ui.heading("Runtime");
                ui.label(RichText::new("Models run only when transcription starts. No cloud speech service, account sync, or always-on listener is enabled.").color(MUTED_TEXT));
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
const CARD_BG: Color32 = Color32::WHITE;
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

fn page(ui: &mut Ui, title: &str, add_contents: impl FnOnce(&mut Ui)) {
    ui.add_space(24.0);
    ui.horizontal(|ui| {
        ui.heading(
            RichText::new(title)
                .font(FontId::proportional(26.0))
                .color(TEXT),
        );
    });
    ui.add_space(16.0);
    add_contents(ui);
}

fn card(ui: &mut Ui, add_contents: impl FnOnce(&mut Ui)) {
    card_frame().show(ui, |ui| {
        ui.set_min_width(ui.available_width());
        add_contents(ui);
    });
}

fn playground_card_frame(ui: &Ui) -> Frame {
    let _ = ui;
    card_frame()
}

fn card_frame() -> Frame {
    Frame::none()
        .fill(CARD_BG)
        .stroke(Stroke::new(1.0, BORDER))
        .rounding(Rounding::same(6.0))
        .inner_margin(Margin::same(14.0))
}

fn playground_card_ui(ui: &mut Ui, card_state: &mut PlaygroundCardState) -> Vec<PlaygroundAction> {
    let mut actions = Vec::new();
    ui.set_min_width(ui.available_width());

    ui.horizontal_top(|ui| {
        ui.label(RichText::new("≡").color(MUTED_TEXT));
        ui.vertical(|ui| {
            ui.heading(&card_state.model.name);
            ui.label(RichText::new(&card_state.model.description).color(MUTED_TEXT));
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
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
        });
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if ui.button("Clear").clicked() {
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
        });
    });

    if let Some(latency) = card_state.latency_ms {
        ui.label(RichText::new(format!("Latency: {latency} ms")).color(MUTED_TEXT));
    }

    ui.add_space(8.0);
    ui.add_sized(
        [ui.available_width(), 132.0],
        TextEdit::multiline(&mut card_state.transcript)
            .desired_rows(7)
            .desired_width(f32::INFINITY)
            .hint_text("Result"),
    );

    actions
}

fn nav_button(ui: &mut Ui, current_tab: &mut Tab, tab: Tab, label: &str) {
    let selected = *current_tab == tab;
    let response = ui.add_sized(
        [ui.available_width(), 34.0],
        Button::new(RichText::new(label).color(if selected { TEXT } else { MUTED_TEXT }))
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

fn primary_button(label: &str) -> Button<'_> {
    Button::new(RichText::new(label).color(Color32::WHITE).strong())
        .fill(PRIMARY)
        .stroke(Stroke::new(1.0, PRIMARY))
        .rounding(Rounding::same(24.0))
        .min_size(Vec2::new(190.0, 46.0))
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
    let (text, fill, stroke) = chip_colors(tone);
    ui.label(
        RichText::new(format!(" {label} "))
            .color(text)
            .background_color(fill),
    )
    .on_hover_text(label);
    let _ = stroke;
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
    format!("{elapsed}s elapsed · {remaining}s left")
}

fn setup_message_for_status(status: &ModelRuntimeStatus) -> String {
    match status {
        ModelRuntimeStatus::Ready => "Ready to transcribe.".to_owned(),
        ModelRuntimeStatus::MissingConfiguration => {
            "Set the whisper.cpp executable and a local model file before transcribing.".to_owned()
        }
        ModelRuntimeStatus::NotInstalled => {
            "Install a local whisper.cpp model or browse to an existing model file.".to_owned()
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

fn set_model_enabled(config: &mut AppConfig, model_id: &str, enabled: bool) {
    if enabled {
        if !config.enabled_models.iter().any(|id| id == model_id) {
            config.enabled_models.push(model_id.to_owned());
        }
    } else {
        config.enabled_models.retain(|id| id != model_id);
        if config.selected_default_model == model_id {
            config.selected_default_model = config
                .enabled_models
                .first()
                .cloned()
                .unwrap_or_else(|| "whisper_cpp_tiny_en".to_owned());
        }
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
