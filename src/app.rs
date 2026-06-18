use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::thread;

use arboard::Clipboard;
use crossbeam_channel::{Receiver, Sender, unbounded};
use eframe::egui::{
    self, Align, Color32, FontId, Frame, Layout, Margin, RichText, ScrollArea, TextEdit, Ui,
};

use crate::audio::{self, RecordingSession};
use crate::config::{self, AppConfig};
use crate::hotkey::HotkeyService;
use crate::models::{ModelRuntimeStatus, SttModelInfo, TranscriptResult, TranscriptionStatus};
use crate::stt;

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
        };

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
        self.playground_cards = cards_from_config(&self.config);
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
                            self.status_message = format!(
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
        let models = self.enabled_models();
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
        self.playground_cards = cards_from_config(&self.config)
            .into_iter()
            .filter(|card| card.model.enabled)
            .collect();
        for card in &mut self.playground_cards {
            card.status = ModelRuntimeStatus::Ready;
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
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_hotkey();
        self.poll_recording();
        self.poll_events();

        egui::TopBottomPanel::top("header").show(ctx, |ui| {
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

        egui::TopBottomPanel::bottom("footer").show(ctx, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(&self.status_message).color(Color32::DARK_GRAY));
            });
        });

        egui::CentralPanel::default().show(ctx, |ui| match self.current_tab {
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
                            match Clipboard::new().and_then(|mut clipboard| {
                                clipboard.set_text(self.transcript.clone())
                            }) {
                                Ok(()) => self.status_message = "Transcript copied".to_owned(),
                                Err(err) => {
                                    self.status_message = format!("Clipboard failed: {err}");
                                }
                            }
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
                    ui.label(format!("{} enabled model(s)", self.enabled_models().len()));
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
            ScrollArea::vertical().show(ui, |ui| {
                if self.playground_cards.is_empty() {
                    ui.label("Enable at least one model on the Models page.");
                }
                for card_state in &mut self.playground_cards {
                    card(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.vertical(|ui| {
                                ui.heading(&card_state.model.name);
                                ui.label(&card_state.model.backend);
                            });
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if ui.button("Clear").clicked() {
                                    card_state.transcript.clear();
                                }
                                ui.label(card_state.status.to_string());
                            });
                        });
                        if let Some(latency) = card_state.latency_ms {
                            ui.label(format!("Latency estimate: {latency} ms"));
                        }
                        ui.add(
                            TextEdit::multiline(&mut card_state.transcript)
                                .desired_rows(5)
                                .hint_text("Result"),
                        );
                    });
                    ui.add_space(8.0);
                }
            });
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
                ui.label("TODO: insert transcript into the active application");
                ui.label("TODO: model downloader and tray-only mode");
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
            add_contents(ui);
        });
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

fn cards_from_config(config: &AppConfig) -> Vec<PlaygroundCardState> {
    config::enabled_models(config)
        .into_iter()
        .map(|model| {
            let status = if !model.enabled {
                ModelRuntimeStatus::Disabled
            } else if model.backend != "whisper.cpp" {
                ModelRuntimeStatus::NotImplemented
            } else if config.whisper_executable_path.is_none() || model.local_path.is_none() {
                ModelRuntimeStatus::MissingConfiguration
            } else {
                ModelRuntimeStatus::Ready
            };
            PlaygroundCardState {
                model,
                status,
                transcript: String::new(),
                latency_ms: None,
            }
        })
        .collect()
}
