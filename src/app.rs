use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::thread;

use arboard::Clipboard;
use crossbeam_channel::{Receiver, Sender, unbounded};
use eframe::egui::{
    self, Align, Color32, FontId, Frame, Layout, Margin, RichText, ScrollArea, TextEdit, Ui,
    ViewportCommand,
};

use crate::audio::{self, RecordingSession};
use crate::config::{self, AppConfig};
use crate::hotkey::HotkeyService;
use crate::models::{ModelRuntimeStatus, SttModelInfo, TranscriptResult, TranscriptionStatus};
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
    model_path_inputs: HashMap<String, String>,
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
        cc.egui_ctx.set_visuals(egui::Visuals::light());

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
            model_path_inputs: config
                .model_paths
                .iter()
                .map(|(id, path)| (id.clone(), path.display().to_string()))
                .collect(),
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

        if source == RecordingSource::Playground {
            self.reset_playground_for_run();
        }

        match audio::start_recording(self.config.max_recording_seconds) {
            Ok(session) => {
                let path = session.audio_path.display().to_string();
                self.active_recording = Some(ActiveRecording {
                    session,
                    source,
                    stop_requested: false,
                });
                self.status = TranscriptionStatus::Listening;
                self.status_message = format!("Listening. Temporary WAV: {path}");
            }
            Err(err) => {
                self.status = TranscriptionStatus::Error;
                self.status_message = format!("Recording failed: {err}");
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
}

impl eframe::App for LocalTranscriberApp {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        Color32::from_rgb(248, 248, 248).to_normalized_gamma_f32()
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.handle_close_request(ctx);
        self.poll_tray(ctx);
        self.poll_hotkey();
        self.poll_recording();
        self.poll_events();
        self.sync_tray_state();

        let panel_fill = Color32::from_rgb(248, 248, 248);
        let content_fill = Color32::from_rgb(252, 252, 252);

        egui::TopBottomPanel::top("header")
            .frame(Frame::none().fill(panel_fill))
            .show(ctx, |ui| {
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Local Transcriber").font(FontId::proportional(24.0)));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        status_pill(ui, self.status);
                        if let Some(model) = self.selected_model() {
                            ui.label(format!("{} / {}", model.backend, model.name));
                        }
                    });
                });
                ui.add_space(8.0);
            });

        egui::SidePanel::left("navigation")
            .frame(Frame::none().fill(panel_fill))
            .resizable(false)
            .exact_width(180.0)
            .show(ctx, |ui| {
                ui.add_space(12.0);
                nav_button(ui, &mut self.current_tab, Tab::Transcribe, "Transcribe");
                nav_button(ui, &mut self.current_tab, Tab::Models, "Models");
                nav_button(
                    ui,
                    &mut self.current_tab,
                    Tab::Playground,
                    "Model Playground",
                );
                nav_button(ui, &mut self.current_tab, Tab::Settings, "Settings");
            });

        egui::TopBottomPanel::bottom("footer")
            .frame(Frame::none().fill(panel_fill))
            .show(ctx, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new(&self.status_message).color(Color32::DARK_GRAY));
                });
            });

        egui::CentralPanel::default()
            .frame(Frame::none().fill(content_fill))
            .show(ctx, |ui| match self.current_tab {
                Tab::Transcribe => self.ui_transcribe(ui),
                Tab::Models => self.ui_models(ui),
                Tab::Playground => self.ui_playground(ui),
                Tab::Settings => self.ui_settings(ui),
            });

        ctx.request_repaint_after(std::time::Duration::from_millis(100));
    }
}

impl LocalTranscriberApp {
    fn ui_transcribe(&mut self, ui: &mut Ui) {
        page(ui, "Transcribe", |ui| {
            card(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.label("Selected model");
                        if let Some(model) = self.selected_model() {
                            ui.heading(format!("{} / {}", model.backend, model.name));
                            ui.label(format!(
                                "RAM {} | Accuracy {} | Speed {} | {}",
                                model.expected_ram,
                                model.accuracy_tier,
                                model.speed_tier,
                                model.download_status
                            ));
                        } else {
                            ui.heading("No model selected");
                        }
                    });
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let button_text = if self.active_recording.is_some() {
                            "Stop listening"
                        } else {
                            "Start listening"
                        };
                        if ui.button(button_text).clicked() {
                            self.toggle_recording();
                        }
                    });
                });
                ui.separator();
                ui.horizontal(|ui| {
                    ui.label("Hotkey");
                    ui.text_edit_singleline(&mut self.hotkey_input);
                    if ui.button("Apply").clicked() {
                        self.apply_hotkey();
                    }
                });
            });

            ui.add_space(12.0);
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
                        .desired_rows(18)
                        .hint_text("Transcript output will appear here"),
                );
            });
        });
    }

    fn ui_models(&mut self, ui: &mut Ui) {
        let models = config::configured_models(&self.config);
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
                });
            });

            ui.add_space(12.0);
            ScrollArea::vertical().show(ui, |ui| {
                for model in models {
                    card(ui, |ui| {
                        ui.horizontal(|ui| {
                            let mut enabled = model.enabled;
                            if ui.checkbox(&mut enabled, "").changed() {
                                set_model_enabled(&mut self.config, &model.id, enabled);
                                self.save_config();
                            }

                            ui.vertical(|ui| {
                                ui.heading(&model.name);
                                ui.label(format!(
                                    "{} | RAM {} | Accuracy {} | Speed {}",
                                    model.backend,
                                    model.expected_ram,
                                    model.accuracy_tier,
                                    model.speed_tier
                                ));
                                ui.label(format!("Status: {}", model.download_status));
                            });

                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                let selected = self.config.selected_default_model == model.id;
                                if ui.selectable_label(selected, "Default").clicked() {
                                    self.config.selected_default_model = model.id.clone();
                                    if !self.config.enabled_models.iter().any(|id| id == &model.id)
                                    {
                                        self.config.enabled_models.push(model.id.clone());
                                    }
                                    self.config.last_used_backend = model.backend.clone();
                                    self.save_config();
                                }
                            });
                        });

                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            ui.label("Model path");
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
                            if ui.text_edit_singleline(input).changed() {
                                if let Some(path) = path_from_input(input) {
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
        page(ui, "Model Playground", |ui| {
            card(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(format!(
                        "{} model(s), {} enabled",
                        self.playground_cards.len(),
                        self.enabled_models().len()
                    ));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let text = if self.active_recording.is_some() {
                            "Stop test recording"
                        } else {
                            "Start test recording"
                        };
                        if ui.button(text).clicked() {
                            if self.active_recording.is_some() {
                                self.stop_recording();
                            } else {
                                self.start_recording(RecordingSource::Playground);
                            }
                        }
                    });
                });
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
                ui.heading("Local settings");
                if let Some(path) = &self.config_path {
                    ui.label(format!("Config file: {}", path.display()));
                }
                ui.add_space(8.0);

                let mut debug = self.config.debug_mode;
                if ui
                    .checkbox(&mut debug, "Keep temporary WAV files")
                    .changed()
                {
                    self.config.debug_mode = debug;
                    self.save_config();
                }

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

                ui.separator();
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
                if self.tray_service.is_none() {
                    ui.colored_label(
                        Color32::from_rgb(180, 40, 40),
                        "Tray integration is unavailable in this desktop session.",
                    );
                }
                if let Some(notice) = text_output::paste_automation_notice() {
                    ui.colored_label(Color32::from_rgb(150, 100, 20), notice);
                }

                ui.separator();
                ui.label("Runtime behavior");
                ui.label("Models are invoked only when transcription starts.");
                ui.label("Placeholder backends are listed for configuration but do not run yet.");
            });

            ui.add_space(12.0);
            card(ui, |ui| {
                ui.heading("Future hooks");
                ui.label("TODO: streaming partial transcription");
                ui.label("TODO: VAD for automatic stop detection");
                ui.label("TODO: voice commands such as scratch that");
                ui.label("TODO: local cleanup/reasoning pass");
                ui.label("TODO: model downloader");
                ui.label("TODO: native whisper.cpp library integration");
            });
        });
    }
}

fn page(ui: &mut Ui, title: &str, add_contents: impl FnOnce(&mut Ui)) {
    ui.add_space(16.0);
    ui.heading(title);
    ui.add_space(12.0);
    add_contents(ui);
}

fn card(ui: &mut Ui, add_contents: impl FnOnce(&mut Ui)) {
    Frame::group(ui.style())
        .inner_margin(Margin::same(12.0))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            add_contents(ui);
        });
}

fn playground_card_frame(ui: &Ui) -> Frame {
    Frame::group(ui.style()).inner_margin(Margin::same(12.0))
}

fn playground_card_ui(ui: &mut Ui, card_state: &mut PlaygroundCardState) -> Vec<PlaygroundAction> {
    let mut actions = Vec::new();
    ui.set_min_width(ui.available_width());

    ui.horizontal(|ui| {
        ui.label(RichText::new("≡").color(Color32::GRAY));
        ui.vertical(|ui| {
            ui.heading(&card_state.model.name);
            ui.label(format!(
                "{} | RAM {} | Accuracy {} | Speed {} | {}",
                card_state.model.backend,
                card_state.model.expected_ram,
                card_state.model.accuracy_tier,
                card_state.model.speed_tier,
                card_state.model.download_status
            ));
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
            ui.label(card_state.status.to_string());
        });
    });

    if let Some(latency) = card_state.latency_ms {
        ui.label(format!("Latency estimate: {latency} ms"));
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
    if ui.selectable_label(selected, label).clicked() {
        *current_tab = tab;
    }
}

fn status_pill(ui: &mut Ui, status: TranscriptionStatus) {
    let color = match status {
        TranscriptionStatus::Idle => Color32::from_rgb(58, 134, 86),
        TranscriptionStatus::Listening => Color32::from_rgb(24, 102, 196),
        TranscriptionStatus::Transcribing => Color32::from_rgb(150, 100, 20),
        TranscriptionStatus::Error => Color32::from_rgb(180, 40, 40),
    };
    ui.colored_label(color, RichText::new(status.to_string()).strong());
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
    } else if model.backend != "whisper.cpp" {
        ModelRuntimeStatus::NotImplemented
    } else if config.whisper_executable_path.is_none() || model.local_path.is_none() {
        ModelRuntimeStatus::MissingConfiguration
    } else {
        ModelRuntimeStatus::Ready
    }
}
