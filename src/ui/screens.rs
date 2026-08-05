//! Shared, backend-neutral egui screen renderers.

use eframe::egui::{
    self, Align, ComboBox, Frame, Grid, Layout, Margin, RichText, Rounding, Stroke, Vec2,
};

use super::{
    controls::{
        ButtonTone, Icon, badge, button, card, icon_glyph, keycap, notice, paint_focus_ring,
    },
    state::{
        ModelComparisonState, ModelSizeTier, ModelSpeedTier, ModelViewModel, RecordingMode,
        SettingsSaveState, SettingsTab, TranscriptionPhase, TranscriptionState, UiRoute,
    },
    ui_palette,
};

#[derive(Clone, Debug)]
pub(crate) struct RecordingSettingsView {
    pub duration_label: String,
    pub provisional_feedback: bool,
    pub device_label: String,
    pub input_level: f32,
    pub save_state: SettingsSaveState,
}

impl Default for RecordingSettingsView {
    fn default() -> Self {
        Self {
            duration_label: "30 seconds".into(),
            provisional_feedback: true,
            device_label: "OS default".into(),
            input_level: 0.0,
            save_state: SettingsSaveState::Clean,
        }
    }
}

pub(crate) struct ScreenView<'a> {
    pub route: UiRoute,
    pub transcription: &'a TranscriptionState,
    pub models: &'a [ModelViewModel],
    pub comparison: &'a ModelComparisonState,
    pub recording_settings: &'a RecordingSettingsView,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ScreenAction {
    None,
    AddModel,
    ChangeModel,
    StartRecording,
    StopRecording,
    OpenAudioSettings,
    RetryMicrophone,
    ClearTranscript,
    CopyTranscript,
    ToggleComparison,
    ToggleComparisonModel(String),
    StartComparison,
    SetSettingsTab(SettingsTab),
    SetRecordingMode(RecordingMode),
    ToggleProvisionalFeedback,
    RefreshDevices,
    ChangeShortcut,
}

pub(crate) fn render_screen(ui: &mut egui::Ui, view: &ScreenView<'_>) -> ScreenAction {
    match view.route {
        UiRoute::Transcribe => transcribe(ui, view.transcription, view.models),
        UiRoute::Models => models(ui, view.models, view.comparison),
        UiRoute::Settings(tab) => settings(ui, tab, view.transcription, view.recording_settings),
        UiRoute::History => placeholder(
            ui,
            "History",
            "Local dictation history remains available in production.",
        ),
        UiRoute::About => placeholder(
            ui,
            "About",
            "Scribe keeps audio and transcripts on this device.",
        ),
        UiRoute::Debug => placeholder(
            ui,
            "Debug",
            "Debug tools are available only when explicitly enabled.",
        ),
    }
}

fn header(ui: &mut egui::Ui, title: &str, subtitle: &str) {
    let response = ui.label(RichText::new(title).size(30.0).strong());
    ui.ctx().accesskit_node_builder(response.id, |builder| {
        builder.set_role(egui::accesskit::Role::Heading)
    });
    ui.label(RichText::new(subtitle).color(ui_palette(ui).muted_text));
    ui.add_space(24.0);
}

fn selected_model_name<'a>(state: &TranscriptionState, models: &'a [ModelViewModel]) -> &'a str {
    state
        .selected_model_id
        .as_deref()
        .and_then(|id| {
            models
                .iter()
                .find(|model| model.id == id)
                .map(|model| model.display_name.as_str())
        })
        .unwrap_or("No model selected")
}

fn selector_row(
    ui: &mut egui::Ui,
    state: &TranscriptionState,
    models: &[ModelViewModel],
) -> ScreenAction {
    let name = selected_model_name(state, models);
    let no_model = state.phase == TranscriptionPhase::NoModel;
    let mut action = ScreenAction::None;
    ui.horizontal(|ui| {
        let model_width = (ui.available_width() - 300.0).max(260.0);
        Frame::none()
            .fill(ui_palette(ui).card_bg)
            .stroke(Stroke::new(1.0, ui_palette(ui).border))
            .rounding(Rounding::same(5.0))
            .inner_margin(Margin::same(16.0))
            .show(ui, |ui| {
                ui.set_min_width(model_width - 32.0);
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(icon_glyph(Icon::Cpu))
                            .size(20.0)
                            .color(ui_palette(ui).muted_text),
                    );
                    ui.label(RichText::new(name).strong());
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let response = button(
                            ui,
                            if no_model { "Select" } else { "Change" },
                            ButtonTone::Text,
                        );
                        if response.clicked() {
                            action = if no_model {
                                ScreenAction::AddModel
                            } else {
                                ScreenAction::ChangeModel
                            };
                        }
                    });
                });
            });
        Frame::none()
            .fill(ui_palette(ui).card_bg)
            .stroke(Stroke::new(1.0, ui_palette(ui).border))
            .rounding(Rounding::same(5.0))
            .inner_margin(Margin::same(16.0))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(icon_glyph(Icon::Keyboard))
                            .size(18.0)
                            .color(ui_palette(ui).muted_text),
                    );
                    ui.label("Hotkey:");
                    for key in state
                        .hotkey
                        .split('+')
                        .map(str::trim)
                        .filter(|key| !key.is_empty())
                    {
                        keycap(ui, key);
                    }
                });
            });
    });
    action
}

fn transcript_frame(ui: &mut egui::Ui, state: &TranscriptionState) -> ScreenAction {
    let colors = ui_palette(ui);
    let mut action = ScreenAction::None;
    Frame::none().fill(colors.card_bg).stroke(Stroke::new(1.0, colors.border)).rounding(Rounding::same(5.0)).show(ui, |ui| {
        ui.set_min_height(530.0);
        if state.phase != TranscriptionPhase::NoModel {
            Frame::none().fill(colors.panel_bg).inner_margin(Margin::symmetric(30.0, 20.0)).show(ui, |ui| {
                ui.horizontal(|ui| match state.phase {
                    TranscriptionPhase::Listening => {
                        let stop = button(ui, format!("{}  Stop recording", icon_glyph(Icon::Stop)), ButtonTone::Danger);
                        if stop.clicked() { action = ScreenAction::StopRecording; }
                        ui.vertical(|ui| { let status = ui.label(RichText::new("Listening").strong().color(colors.error)); ui.ctx().accesskit_node_builder(status.id, |builder| { builder.set_live(egui::accesskit::Live::Polite); builder.set_live_atomic(); }); ui.label(format_elapsed(state.elapsed_ms)); });
                    }
                    TranscriptionPhase::Finalizing => { ui.spinner(); ui.vertical(|ui| { let status = ui.label(RichText::new("Finalizing transcript…").strong()); ui.ctx().accesskit_node_builder(status.id, |builder| { builder.set_live(egui::accesskit::Live::Polite); builder.set_live_atomic(); }); ui.label("This may take a moment."); }); }
                    _ => {
                        let start = button(ui, format!("{}  Start recording", icon_glyph(Icon::Microphone)), ButtonTone::Primary);
                        if start.clicked() { action = ScreenAction::StartRecording; }
                        ui.label(match state.recording_mode { RecordingMode::Hold => format!("Hold {} to record", state.hotkey), RecordingMode::PressOnce => format!("Press {} to toggle", state.hotkey) });
                    }
                });
            });
            ui.separator();
        }
        if state.phase == TranscriptionPhase::NoModel {
            ui.with_layout(Layout::top_down(Align::Center), |ui| {
                ui.add_space(130.0);
                Frame::none().fill(colors.panel_bg).rounding(Rounding::same(8.0)).inner_margin(Margin::same(12.0)).show(ui, |ui| { ui.label(RichText::new(icon_glyph(Icon::Models)).size(30.0).color(colors.muted_text)); });
                ui.add_space(12.0);
                ui.label(RichText::new("Add a speech model to start transcribing").size(18.0).strong());
                ui.label("Your audio stays on this device.");
                ui.add_space(12.0);
                if button(ui, "Add model", ButtonTone::Primary).clicked() { action = ScreenAction::AddModel; }
            });
        } else {
        ui.add_space(16.0);
        if let Some(text) = &state.notice {
            let response = notice(ui, text, state.phase == TranscriptionPhase::MicrophoneError);
            ui.ctx().accesskit_node_builder(response.id, |builder| { builder.set_live(if state.phase == TranscriptionPhase::MicrophoneError { egui::accesskit::Live::Assertive } else { egui::accesskit::Live::Polite }); builder.set_live_atomic(); });
            if state.phase == TranscriptionPhase::MicrophoneError {
                ui.horizontal(|ui| {
                    if button(ui, "Open audio settings", ButtonTone::Text).clicked() { action = ScreenAction::OpenAudioSettings; }
                    if button(ui, "Try again", ButtonTone::Secondary).clicked() { action = ScreenAction::RetryMicrophone; }
                });
            }
            ui.add_space(12.0);
        }
        if state.committed_transcript.trim().is_empty() { ui.label(RichText::new("Your transcript will appear here.").color(colors.tertiary_text)); } else { let response = ui.label(&state.committed_transcript); ui.ctx().accesskit_node_builder(response.id, |builder| { builder.set_live(egui::accesskit::Live::Polite); builder.set_live_atomic(); }); }
        if !state.provisional_transcript.is_empty() { ui.add_space(8.0); ui.label(RichText::new(&state.provisional_transcript).italics().color(colors.tertiary_text)); }
        ui.add_space(10.0);
        if let Some(model_id) = &state.selected_model_id { badge(ui, &model_id.to_ascii_uppercase(), None); }
        let remaining = (220.0 - ui.min_rect().height()).max(24.0);
        ui.add_space(remaining);
        ui.separator();
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let enabled = !matches!(state.phase, TranscriptionPhase::Listening | TranscriptionPhase::Finalizing);
            let copy = ui.add_enabled(enabled && !state.committed_transcript.is_empty(), egui::Button::new(format!("{}  Copy", icon_glyph(Icon::Copy))).min_size(Vec2::new(96.0, 40.0)));
            if !copy.enabled() { ui.ctx().accesskit_node_builder(copy.id, |builder| builder.set_description("Copy is unavailable while recording or until a final transcript exists.")); }
            if copy.clicked() { action = ScreenAction::CopyTranscript; }
            let clear = ui.add_enabled(enabled && !state.committed_transcript.is_empty(), egui::Button::new("Clear").min_size(Vec2::new(72.0, 40.0)));
            if !clear.enabled() { ui.ctx().accesskit_node_builder(clear.id, |builder| builder.set_description("Clear is unavailable while recording or until a final transcript exists.")); }
            if clear.clicked() { action = ScreenAction::ClearTranscript; }
        });
        }
    });
    action
}

fn transcribe(
    ui: &mut egui::Ui,
    state: &TranscriptionState,
    models: &[ModelViewModel],
) -> ScreenAction {
    header(ui, "Transcribe", "Audio stays on this device.");
    let action = selector_row(ui, state, models);
    ui.add_space(12.0);
    let panel_action = transcript_frame(ui, state);
    ui.add_space(14.0);
    ui.horizontal_centered(|ui| {
        ui.label(
            RichText::new(format!(
                "{}  Silence is ignored and won’t replace your transcript.",
                icon_glyph(Icon::Info)
            ))
            .color(ui_palette(ui).muted_text),
        );
    });
    if action == ScreenAction::None {
        panel_action
    } else {
        action
    }
}

fn metadata(ui: &mut egui::Ui, icon: Icon, text: &str) {
    ui.label(
        RichText::new(format!("{}  {text}", icon_glyph(icon)))
            .small()
            .color(ui_palette(ui).muted_text),
    );
}

fn models(
    ui: &mut egui::Ui,
    models: &[ModelViewModel],
    comparison: &ModelComparisonState,
) -> ScreenAction {
    let colors = ui_palette(ui);
    let mut action = ScreenAction::None;
    ui.horizontal(|ui| {
        ui.vertical(|ui| {
            let response = ui.label(RichText::new("Models").size(30.0).strong());
            ui.ctx().accesskit_node_builder(response.id, |builder| {
                builder.set_role(egui::accesskit::Role::Heading)
            });
            ui.label(
                RichText::new("Manage the speech models available on this device.")
                    .color(colors.muted_text),
            );
        });
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if button(
                ui,
                format!("{}  Add models", icon_glyph(Icon::Plus)),
                ButtonTone::Primary,
            )
            .clicked()
            {
                action = ScreenAction::AddModel;
            }
            if button(
                ui,
                format!(
                    "Compare  {}",
                    icon_glyph(if comparison.expanded {
                        Icon::ChevronUp
                    } else {
                        Icon::ChevronDown
                    })
                ),
                ButtonTone::Secondary,
            )
            .clicked()
            {
                action = ScreenAction::ToggleComparison;
            }
        });
    });
    ui.add_space(24.0);
    for model in models {
        card(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(&model.display_name).strong());
                badge(
                    ui,
                    if model.active { "Active" } else { "Installed" },
                    model.active.then_some(colors.success),
                );
                if model.recommended {
                    badge(ui, "Recommended", None);
                }
                if let Some(ram) = model.estimated_ram_bytes {
                    metadata(ui, Icon::Cpu, &format!("{}MB RAM", ram / 1_000_000));
                }
                metadata(ui, Icon::Globe, &model.language_summary);
                metadata(ui, Icon::Gauge, speed_label(model.speed_tier));
                metadata(ui, Icon::Folder, size_label(model.size_tier));
            });
        });
        ui.add_space(8.0);
    }
    ui.add_space((140.0 - (models.len() as f32 * 64.0)).max(8.0));
    Frame::none()
        .fill(colors.card_bg)
        .stroke(Stroke::new(1.0, colors.border))
        .rounding(Rounding::same(5.0))
        .inner_margin(Margin::same(16.0))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.label(RichText::new("Compare installed models").strong());
                    ui.label(
                        RichText::new("Comparison measures speed and output on this computer.")
                            .color(colors.muted_text),
                    );
                });
                if button(
                    ui,
                    icon_glyph(if comparison.expanded {
                        Icon::ChevronUp
                    } else {
                        Icon::ChevronDown
                    }),
                    ButtonTone::Text,
                )
                .clicked()
                {
                    action = ScreenAction::ToggleComparison;
                }
            });
            if comparison.expanded {
                ui.add_space(12.0);
                ui.horizontal_wrapped(|ui| {
                    for model in models {
                        let mut checked = comparison.selected_model_ids.contains(&model.id);
                        let response = ui.checkbox(&mut checked, &model.display_name);
                        ui.ctx().accesskit_node_builder(response.id, |builder| {
                            builder.set_role(egui::accesskit::Role::CheckBox);
                            builder.set_name(model.display_name.as_str());
                        });
                        if response.clicked() {
                            action = ScreenAction::ToggleComparisonModel(model.id.clone());
                        }
                    }
                    if button(
                        ui,
                        format!("{}  Start test recording", icon_glyph(Icon::Microphone)),
                        ButtonTone::Primary,
                    )
                    .clicked()
                    {
                        action = ScreenAction::StartComparison;
                    }
                });
                ui.separator();
                Grid::new("comparison-results")
                    .striped(true)
                    .min_col_width(115.0)
                    .show(ui, |ui| {
                        for heading in
                            ["Model", "Duration", "Processing time", "Output", "Accuracy"]
                        {
                            ui.label(RichText::new(heading).strong().small());
                        }
                        ui.end_row();
                        for model in models {
                            let result = comparison
                                .results
                                .iter()
                                .find(|(id, _)| id == &model.id)
                                .map(|(_, result)| result);
                            ui.label(&model.variant_label);
                            ui.label(
                                comparison.audio_duration_ms.map_or("—".into(), |ms| {
                                    format!("{:.1}s", ms as f32 / 1_000.0)
                                }),
                            );
                            ui.label(
                                result
                                    .and_then(|r| r.processing_ms)
                                    .map_or("—".into(), |ms| {
                                        format!("{:.1}s", ms as f32 / 1_000.0)
                                    }),
                            );
                            ui.label(
                                result
                                    .and_then(|r| r.output.as_deref())
                                    .unwrap_or("No data"),
                            );
                            ui.label(
                                match (
                                    comparison.reference_transcript.as_deref(),
                                    result.and_then(|r| r.word_error_rate),
                                ) {
                                    (Some(_), Some(rate)) => {
                                        format!("{:.0}% accuracy", (1.0 - rate) * 100.0)
                                    }
                                    _ => "Add a reference transcript to measure".into(),
                                },
                            );
                            ui.end_row();
                        }
                    });
            }
        });
    let used_bytes: u64 = models
        .iter()
        .map(|model| model.disk_bytes)
        .collect::<Option<Vec<_>>>()
        .map(|values| values.into_iter().sum())
        .unwrap_or_default();
    if used_bytes > 0 {
        ui.add_space(12.0);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.label(
                RichText::new(format!(
                    "{}  Storage: {} used",
                    icon_glyph(Icon::Storage),
                    format_bytes(used_bytes)
                ))
                .small()
                .color(colors.muted_text)
                .strong(),
            );
        });
    }
    action
}

fn settings(
    ui: &mut egui::Ui,
    active_tab: SettingsTab,
    state: &TranscriptionState,
    settings: &RecordingSettingsView,
) -> ScreenAction {
    let colors = ui_palette(ui);
    let mut action = ScreenAction::None;
    ui.horizontal(|ui| {
        let heading = ui.label(RichText::new("Settings").size(30.0).strong());
        ui.ctx().accesskit_node_builder(heading.id, |builder| {
            builder.set_role(egui::accesskit::Role::Heading)
        });
        let status = match settings.save_state {
            SettingsSaveState::Saving => "Saving…",
            SettingsSaveState::Failed => "Couldn’t save changes",
            SettingsSaveState::Saved => "Changes saved",
            _ => "Changes save automatically",
        };
        let response = ui.label(
            RichText::new(format!("{}  {status}", icon_glyph(Icon::Info))).color(colors.muted_text),
        );
        if matches!(
            settings.save_state,
            SettingsSaveState::Saving | SettingsSaveState::Saved | SettingsSaveState::Failed
        ) {
            ui.ctx().accesskit_node_builder(response.id, |builder| {
                builder.set_live(egui::accesskit::Live::Polite);
                builder.set_live_atomic();
            });
        }
    });
    ui.add_space(20.0);
    let tabs = ui.horizontal(|ui| {
        for (tab, label) in [
            (SettingsTab::General, "General"),
            (SettingsTab::Recording, "Recording"),
            (SettingsTab::Output, "Output"),
            (SettingsTab::Advanced, "Advanced"),
        ] {
            let response = ui.selectable_label(tab == active_tab, label);
            ui.ctx().accesskit_node_builder(response.id, |builder| {
                builder.set_role(egui::accesskit::Role::Tab);
                builder.set_name(label);
                builder.set_selected(tab == active_tab);
            });
            paint_focus_ring(ui, &response, Rounding::same(2.0));
            if response.clicked() {
                action = ScreenAction::SetSettingsTab(tab);
            }
            if response.has_focus() && ui.input(|input| input.key_pressed(egui::Key::ArrowRight)) {
                action = ScreenAction::SetSettingsTab(next_tab(tab));
            }
            if response.has_focus() && ui.input(|input| input.key_pressed(egui::Key::ArrowLeft)) {
                action = ScreenAction::SetSettingsTab(previous_tab(tab));
            }
        }
    });
    ui.ctx()
        .accesskit_node_builder(tabs.response.id, |builder| {
            builder.set_role(egui::accesskit::Role::TabList);
            builder.set_name("Settings sections");
        });
    ui.separator();
    ui.add_space(16.0);
    let panel = card(ui, |ui| {
        ui.label(RichText::new("Recording behavior").strong());
        ui.add_space(12.0);
        let mode_group = ui.horizontal(|ui| {
            ui.add_sized(
                [270.0, 40.0],
                egui::Label::new(RichText::new("Mode").color(colors.muted_text)),
            );
            for (mode, label) in [
                (RecordingMode::PressOnce, "Press once"),
                (RecordingMode::Hold, "Hold"),
            ] {
                let response = ui.selectable_label(state.recording_mode == mode, label);
                ui.ctx().accesskit_node_builder(response.id, |builder| {
                    builder.set_role(egui::accesskit::Role::RadioButton);
                    builder.set_name(label);
                    builder.set_selected(state.recording_mode == mode);
                });
                if response.clicked() {
                    action = ScreenAction::SetRecordingMode(mode);
                }
                if response.has_focus()
                    && ui.input(|input| {
                        input.key_pressed(egui::Key::ArrowRight)
                            || input.key_pressed(egui::Key::ArrowLeft)
                    })
                {
                    action = ScreenAction::SetRecordingMode(if mode == RecordingMode::PressOnce {
                        RecordingMode::Hold
                    } else {
                        RecordingMode::PressOnce
                    });
                }
            }
        });
        ui.ctx()
            .accesskit_node_builder(mode_group.response.id, |builder| {
                builder.set_role(egui::accesskit::Role::RadioGroup);
                builder.set_name("Recording mode");
            });
        ui.add_space(8.0);
        ui.separator();
        ui.add_space(8.0);
        setting_row(ui, "Duration limit", |ui| {
            ComboBox::from_id_source("duration-limit")
                .selected_text(&settings.duration_label)
                .width(240.0)
                .show_ui(ui, |ui| {
                    ui.label("30 seconds");
                });
        });
        setting_row(ui, "Visual feedback", |ui| {
            let mut enabled = settings.provisional_feedback;
            let response = ui.checkbox(&mut enabled, "Show provisional words while recording");
            if response.clicked() {
                action = ScreenAction::ToggleProvisionalFeedback;
            }
            ui.label(
                RichText::new("Improves visual feedback but may use more CPU.")
                    .small()
                    .color(colors.muted_text),
            );
        });
    });
    ui.ctx().accesskit_node_builder(panel.id, |builder| {
        builder.set_role(egui::accesskit::Role::TabPanel);
        builder.set_name("Recording settings");
    });
    ui.add_space(16.0);
    card(ui, |ui| {
        ui.label(RichText::new("Audio input").strong());
        ui.add_space(12.0);
        setting_row(ui, "Device", |ui| {
            ComboBox::from_id_source("audio-device")
                .selected_text(&settings.device_label)
                .width(360.0)
                .show_ui(ui, |ui| {
                    ui.label(&settings.device_label);
                });
            let refresh = button(
                ui,
                format!("{}  Refresh devices", icon_glyph(Icon::Refresh)),
                ButtonTone::Text,
            );
            ui.ctx().accesskit_node_builder(refresh.id, |builder| {
                builder.set_role(egui::accesskit::Role::Button);
                builder.set_name("Refresh devices");
            });
            if refresh.clicked() {
                action = ScreenAction::RefreshDevices;
            }
        });
        setting_row(ui, "Input level", |ui| {
            ui.label(RichText::new(icon_glyph(Icon::Microphone)).size(18.0));
            ui.add(egui::ProgressBar::new(settings.input_level).desired_width(320.0));
        });
    });
    ui.add_space(16.0);
    card(ui, |ui| {
        ui.label(RichText::new("Shortcut").strong());
        ui.add_space(12.0);
        setting_row(ui, "Global record hotkey", |ui| {
            for key in state
                .hotkey
                .split('+')
                .map(str::trim)
                .filter(|key| !key.is_empty())
            {
                keycap(ui, key);
            }
            if button(ui, "Change shortcut", ButtonTone::Secondary).clicked() {
                action = ScreenAction::ChangeShortcut;
            }
        });
    });
    action
}

fn setting_row(ui: &mut egui::Ui, label: &str, contents: impl FnOnce(&mut egui::Ui)) {
    ui.horizontal(|ui| {
        ui.add_sized(
            [270.0, 40.0],
            egui::Label::new(RichText::new(label).color(ui_palette(ui).muted_text)),
        );
        contents(ui);
    });
    ui.add_space(8.0);
    ui.separator();
    ui.add_space(8.0);
}

fn next_tab(tab: SettingsTab) -> SettingsTab {
    match tab {
        SettingsTab::General => SettingsTab::Recording,
        SettingsTab::Recording => SettingsTab::Output,
        SettingsTab::Output => SettingsTab::Advanced,
        SettingsTab::Advanced => SettingsTab::General,
    }
}
fn previous_tab(tab: SettingsTab) -> SettingsTab {
    match tab {
        SettingsTab::General => SettingsTab::Advanced,
        SettingsTab::Recording => SettingsTab::General,
        SettingsTab::Output => SettingsTab::Recording,
        SettingsTab::Advanced => SettingsTab::Output,
    }
}
fn format_bytes(bytes: u64) -> String {
    if bytes >= 1_000_000_000 {
        format!("{:.1}GB", bytes as f64 / 1_000_000_000.0)
    } else {
        format!("{}MB", bytes / 1_000_000)
    }
}
fn placeholder(ui: &mut egui::Ui, title: &str, message: &str) -> ScreenAction {
    header(ui, title, message);
    card(ui, |ui| {
        ui.label(message);
    });
    ScreenAction::None
}
fn format_elapsed(elapsed_ms: u64) -> String {
    format!(
        "{:02}:{:02}",
        elapsed_ms / 60_000,
        (elapsed_ms / 1_000) % 60
    )
}
fn speed_label(tier: ModelSpeedTier) -> &'static str {
    match tier {
        ModelSpeedTier::VeryFast => "Very Fast",
        ModelSpeedTier::Fast => "Fast",
        ModelSpeedTier::Balanced => "Balanced Speed",
        ModelSpeedTier::AccurateSlow => "Accurate, slower",
        ModelSpeedTier::Unknown => "Speed unknown",
    }
}
fn size_label(tier: ModelSizeTier) -> &'static str {
    match tier {
        ModelSizeTier::Tiny => "Tiny Size",
        ModelSizeTier::Small => "Small Size",
        ModelSizeTier::Base => "Base Size",
        ModelSizeTier::Medium => "Medium Size",
        ModelSizeTier::Large => "Large Size",
        ModelSizeTier::Unknown => "Size unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn elapsed_display_is_deterministic() {
        assert_eq!(format_elapsed(8_000), "00:08");
    }
}
