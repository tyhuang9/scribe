//! Shared, backend-neutral egui screen renderers.

use eframe::egui::{
    self, Align, ComboBox, Frame, Grid, Layout, Margin, RichText, Rounding, Stroke, Vec2,
};

use super::{
    controls::{
        ButtonTone, Icon, badge, button, card, focus_tooltip, icon_glyph, keycap, paint_focus_ring,
    },
    state::{
        ModelComparisonState, ModelSizeTier, ModelSpeedTier, ModelViewModel, RecordingMode,
        SettingsSaveState, SettingsTab, TranscriptionPhase, TranscriptionState, UiRoute,
    },
    ui_palette,
};

#[derive(Clone, Debug)]
pub(crate) struct RecordingSettingsView {
    pub close_to_tray: bool,
    pub duration_seconds: u32,
    pub duration_label: String,
    pub provisional_feedback: bool,
    pub selected_audio_device: Option<String>,
    pub audio_devices: Vec<String>,
    pub device_label: String,
    pub input_level: f32,
    pub auto_insert_transcript: bool,
    pub restore_clipboard_after_insert: bool,
    pub paste_delay_ms: u64,
    pub save_state: SettingsSaveState,
}

impl Default for RecordingSettingsView {
    fn default() -> Self {
        Self {
            close_to_tray: true,
            duration_seconds: 30,
            duration_label: "30 seconds".into(),
            provisional_feedback: true,
            selected_audio_device: None,
            audio_devices: Vec::new(),
            device_label: "OS default".into(),
            input_level: 0.0,
            auto_insert_transcript: false,
            restore_clipboard_after_insert: true,
            paste_delay_ms: 60,
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
    SetCloseToTray(bool),
    SetRecordingMode(RecordingMode),
    SetDurationSeconds(u32),
    ToggleProvisionalFeedback,
    SetAudioDevice(Option<String>),
    RefreshDevices,
    ChangeShortcut,
    SetAutoInsertTranscript(bool),
    SetRestoreClipboardAfterInsert(bool),
    SetPasteDelayMs(u64),
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
    let disabled_reason = model_selector_disabled_reason(state.phase);
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
                        let response = ui
                            .add_enabled_ui(disabled_reason.is_none(), |ui| {
                                button(
                                    ui,
                                    if no_model { "Select" } else { "Change" },
                                    ButtonTone::Text,
                                )
                            })
                            .inner;
                        if let Some(reason) = disabled_reason {
                            ui.ctx().accesskit_node_builder(response.id, |builder| {
                                builder.set_description(reason);
                            });
                            focus_tooltip(ui, &response, reason);
                            response.clone().on_hover_text(reason);
                        }
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

fn model_selector_disabled_reason(phase: TranscriptionPhase) -> Option<&'static str> {
    match phase {
        TranscriptionPhase::RequestingMicrophone => {
            Some("Model selection is unavailable while microphone access is being requested.")
        }
        TranscriptionPhase::Listening => Some("Model selection is unavailable while recording."),
        TranscriptionPhase::Finalizing => {
            Some("Model selection is unavailable while finalizing the current transcript.")
        }
        _ => None,
    }
}

fn recording_square_button(
    ui: &mut egui::Ui,
    icon: Icon,
    accessible_name: &str,
    fill: egui::Color32,
    foreground: egui::Color32,
    stroke: Stroke,
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(50.0), egui::Sense::click());
    ui.painter().rect(rect, Rounding::same(8.0), fill, stroke);
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        icon_glyph(icon),
        egui::FontId::proportional(22.0),
        foreground,
    );
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, accessible_name));
    ui.ctx().accesskit_node_builder(response.id, |builder| {
        builder.set_role(egui::accesskit::Role::Button);
        builder.set_name(accessible_name);
    });
    paint_focus_ring(ui, &response, Rounding::same(8.0));
    focus_tooltip(ui, &response, accessible_name);
    response.on_hover_text(accessible_name)
}

fn recording_status_header(ui: &mut egui::Ui, state: &TranscriptionState) -> ScreenAction {
    let colors = ui_palette(ui);
    let mut action = ScreenAction::None;
    ui.horizontal(|ui| match state.phase {
        TranscriptionPhase::Listening => {
            let stop = recording_square_button(
                ui,
                Icon::Stop,
                "Stop recording",
                colors.error_pale,
                colors.error_text,
                Stroke::NONE,
            );
            if stop.clicked() {
                action = ScreenAction::StopRecording;
            }
            ui.vertical(|ui| {
                ui.horizontal(|ui| {
                    let (dot_rect, _) =
                        ui.allocate_exact_size(Vec2::splat(8.0), egui::Sense::hover());
                    ui.painter()
                        .circle_filled(dot_rect.center(), 4.0, colors.error);
                    let status = ui.label(RichText::new("Listening").strong().color(colors.error));
                    ui.ctx().accesskit_node_builder(status.id, |builder| {
                        builder.set_live(egui::accesskit::Live::Polite);
                        builder.set_live_atomic();
                    });
                });
                ui.label(format_elapsed(state.elapsed_ms));
            });
        }
        TranscriptionPhase::Finalizing => {
            ui.spinner();
            ui.vertical(|ui| {
                let status = ui.label(RichText::new("Finalizing transcript…").strong());
                ui.ctx().accesskit_node_builder(status.id, |builder| {
                    builder.set_live(egui::accesskit::Live::Polite);
                    builder.set_live_atomic();
                });
                ui.label("This may take a moment.");
            });
        }
        _ => {
            let start = recording_square_button(
                ui,
                Icon::Microphone,
                "Start recording",
                colors.primary_button_bg,
                colors.primary_button_text,
                Stroke::NONE,
            );
            if start.clicked() {
                action = ScreenAction::StartRecording;
            }
            ui.vertical(|ui| {
                ui.label(RichText::new("Start recording").strong());
                ui.label(match state.recording_mode {
                    RecordingMode::Hold => format!("Hold {} to record", state.hotkey),
                    RecordingMode::PressOnce => format!("Press {} to toggle", state.hotkey),
                });
            });
        }
    });
    action
}

fn no_model_empty_state(ui: &mut egui::Ui) -> ScreenAction {
    let colors = ui_palette(ui);
    let mut action = ScreenAction::None;
    ui.with_layout(Layout::top_down(Align::Center), |ui| {
        ui.add_space(130.0);
        Frame::none()
            .fill(colors.panel_bg)
            .rounding(Rounding::same(8.0))
            .inner_margin(Margin::same(12.0))
            .show(ui, |ui| {
                ui.label(
                    RichText::new(icon_glyph(Icon::Models))
                        .size(30.0)
                        .color(colors.muted_text),
                );
            });
        ui.add_space(12.0);
        ui.label(
            RichText::new("Add a speech model to start transcribing")
                .size(18.0)
                .strong(),
        );
        ui.label("Your audio stays on this device.");
        ui.add_space(12.0);
        if button(ui, "Add model", ButtonTone::Primary).clicked() {
            action = ScreenAction::AddModel;
        }
    });
    action
}

fn no_model_recovery_callout(ui: &mut egui::Ui) -> ScreenAction {
    let colors = ui_palette(ui);
    let mut action = ScreenAction::None;
    Frame::none()
        .fill(colors.panel_bg)
        .stroke(Stroke::new(1.0, colors.border))
        .rounding(Rounding::same(5.0))
        .inner_margin(Margin::same(12.0))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(icon_glyph(Icon::Models))
                        .size(24.0)
                        .color(colors.muted_text),
                );
                ui.vertical(|ui| {
                    ui.label(RichText::new("Add a speech model to continue transcribing").strong());
                    ui.label(
                        RichText::new("Your existing transcript is still available below.")
                            .color(colors.muted_text),
                    );
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if button(ui, "Add model", ButtonTone::Primary).clicked() {
                        action = ScreenAction::AddModel;
                    }
                });
            });
        });
    action
}

fn transcript_frame(ui: &mut egui::Ui, state: &TranscriptionState) -> ScreenAction {
    let colors = ui_palette(ui);
    let mut action = ScreenAction::None;
    Frame::none()
        .fill(colors.card_bg)
        .stroke(Stroke::new(1.0, colors.border))
        .rounding(Rounding::same(5.0))
        .show(ui, |ui| {
            ui.set_min_height(530.0);
            if state.phase != TranscriptionPhase::NoModel {
                Frame::none()
                    .fill(colors.panel_bg)
                    .inner_margin(Margin::symmetric(30.0, 20.0))
                    .show(ui, |ui| {
                        action = recording_status_header(ui, state);
                    });
                ui.separator();
            }

            let has_committed_transcript = !state.committed_transcript.trim().is_empty();
            if state.phase == TranscriptionPhase::NoModel && !has_committed_transcript {
                action = no_model_empty_state(ui);
            } else {
                ui.add_space(16.0);
                if state.phase == TranscriptionPhase::NoModel {
                    let recovery_action = no_model_recovery_callout(ui);
                    if recovery_action != ScreenAction::None {
                        action = recovery_action;
                    }
                    ui.add_space(16.0);
                }
                if let Some(text) = &state.notice {
                    if state.phase == TranscriptionPhase::MicrophoneError {
                        let alert_action = microphone_error_notice(ui, text);
                        if alert_action != ScreenAction::None {
                            action = alert_action;
                        }
                    } else {
                        let response = neutral_notice(ui, text);
                        ui.ctx().accesskit_node_builder(response.id, |builder| {
                            builder.set_live(egui::accesskit::Live::Polite);
                            builder.set_live_atomic();
                        });
                    }
                    ui.add_space(12.0);
                }
                if state.committed_transcript.trim().is_empty() {
                    ui.label(
                        RichText::new("Your transcript will appear here.")
                            .color(colors.tertiary_text),
                    );
                } else {
                    let response = ui.label(&state.committed_transcript);
                    ui.ctx().accesskit_node_builder(response.id, |builder| {
                        builder.set_live(egui::accesskit::Live::Polite);
                        builder.set_live_atomic();
                    });
                }
                if !state.provisional_transcript.is_empty() {
                    ui.add_space(8.0);
                    ui.label(
                        RichText::new(&state.provisional_transcript)
                            .italics()
                            .color(colors.tertiary_text),
                    );
                }
                ui.add_space(10.0);
                if let Some(model_id) = &state.selected_model_id {
                    badge(ui, &model_id.to_ascii_uppercase(), None);
                }
                let remaining = (220.0 - ui.min_rect().height()).max(24.0);
                ui.add_space(remaining);
                ui.separator();
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let enabled = !matches!(
                        state.phase,
                        TranscriptionPhase::Listening | TranscriptionPhase::Finalizing
                    );
                    let copy = ui.add_enabled(
                        enabled && has_committed_transcript,
                        egui::Button::new(format!("{}  Copy", icon_glyph(Icon::Copy)))
                            .min_size(Vec2::new(96.0, 40.0)),
                    );
                    if !copy.enabled() {
                        ui.ctx().accesskit_node_builder(copy.id, |builder| {
                            builder.set_description("Copy is unavailable while recording or until a final transcript exists.")
                        });
                    }
                    if copy.clicked() {
                        action = ScreenAction::CopyTranscript;
                    }
                    let clear = ui.add_enabled(
                        enabled && has_committed_transcript,
                        egui::Button::new("Clear").min_size(Vec2::new(72.0, 40.0)),
                    );
                    if !clear.enabled() {
                        ui.ctx().accesskit_node_builder(clear.id, |builder| {
                            builder.set_description("Clear is unavailable while recording or until a final transcript exists.")
                        });
                    }
                    if clear.clicked() {
                        action = ScreenAction::ClearTranscript;
                    }
                });
            }
        });
    action
}

fn neutral_notice(ui: &mut egui::Ui, text: &str) -> egui::Response {
    let colors = ui_palette(ui);
    let width = ui.available_width();
    ui.allocate_ui_with_layout(Vec2::new(width, 0.0), Layout::top_down(Align::LEFT), |ui| {
        Frame::none()
            .fill(colors.panel_bg)
            .stroke(Stroke::new(1.0, colors.border))
            .rounding(Rounding::same(5.0))
            .inner_margin(Margin::same(12.0))
            .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.label(
                        RichText::new(icon_glyph(Icon::Info))
                            .size(18.0)
                            .color(colors.neutral_notice_text),
                    );
                    ui.label(RichText::new(text).color(colors.neutral_notice_text));
                });
            });
    })
    .response
}

fn microphone_error_notice(ui: &mut egui::Ui, text: &str) -> ScreenAction {
    let colors = ui_palette(ui);
    let width = ui.available_width();
    let mut action = ScreenAction::None;
    let alert_id = ui.make_persistent_id("microphone-error-alert");
    let ctx = ui.ctx().clone();
    ctx.accesskit_node_builder(alert_id, |builder| {
        builder.set_role(egui::accesskit::Role::Alert);
        builder.set_name("Microphone access error");
        builder.set_live(egui::accesskit::Live::Assertive);
        builder.set_live_atomic();
    });
    ctx.with_accessibility_parent(alert_id, || {
        ui.allocate_ui_with_layout(Vec2::new(width, 0.0), Layout::top_down(Align::LEFT), |ui| {
            Frame::none()
                .fill(colors.error_pale)
                .stroke(Stroke::new(1.0, colors.error_border))
                .rounding(Rounding::same(5.0))
                .inner_margin(Margin::same(12.0))
                .show(ui, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(
                            RichText::new(icon_glyph(Icon::MicrophoneOff))
                                .size(18.0)
                                .color(colors.error_text),
                        );
                        let message_width = (ui.available_width() - 300.0).max(180.0);
                        ui.add_sized(
                            [message_width, 40.0],
                            egui::Label::new(RichText::new(text).color(colors.error_text))
                                .wrap(true),
                        );
                        let open_settings = button(ui, "Open audio settings", ButtonTone::Text);
                        if open_settings.clicked() {
                            action = ScreenAction::OpenAudioSettings;
                        }
                        let retry = button(ui, "Try again", ButtonTone::Danger);
                        if retry.clicked() {
                            action = ScreenAction::RetryMicrophone;
                        }
                    });
                });
        });
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

fn model_family_name(model: &ModelViewModel) -> &str {
    model
        .display_name
        .strip_suffix(model.variant_label.as_str())
        .map(str::trim_end)
        .filter(|name| !name.is_empty())
        .unwrap_or(model.display_name.as_str())
}

fn models_footer_spacer(
    remaining_height: f32,
    comparison_expanded: bool,
    has_storage_footer: bool,
) -> f32 {
    let comparison_height = if comparison_expanded { 330.0 } else { 92.0 };
    let storage_height = if has_storage_footer { 40.0 } else { 0.0 };
    (remaining_height - comparison_height - storage_height).max(16.0)
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
            let compare_disabled_reason = (models.len() < 2)
                .then_some("Install at least two compatible models to compare them.");
            let compare = ui
                .add_enabled_ui(compare_disabled_reason.is_none(), |ui| {
                    button(ui, "Compare", ButtonTone::Secondary)
                })
                .inner;
            if let Some(reason) = compare_disabled_reason {
                ui.ctx().accesskit_node_builder(compare.id, |builder| {
                    builder.set_description(reason);
                });
                focus_tooltip(ui, &compare, reason);
                compare.clone().on_hover_text(reason);
            }
            if compare.clicked() {
                action = ScreenAction::ToggleComparison;
            }
        });
    });
    ui.add_space(24.0);
    for model in models {
        card(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.vertical(|ui| {
                    ui.set_min_width(160.0);
                    ui.label(RichText::new(model_family_name(model)).strong());
                    ui.label(
                        RichText::new(&model.variant_label)
                            .small()
                            .color(colors.muted_text),
                    );
                });
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
    let used_bytes: u64 = models
        .iter()
        .map(|model| model.disk_bytes)
        .collect::<Option<Vec<_>>>()
        .map(|values| values.into_iter().sum())
        .unwrap_or_default();
    let visible_remaining_height = (ui.clip_rect().bottom() - ui.cursor().top()).max(0.0);
    ui.add_space(models_footer_spacer(
        visible_remaining_height,
        comparison.expanded,
        used_bytes > 0,
    ));
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
                let toggle_name = if comparison.expanded {
                    "Collapse comparison"
                } else {
                    "Expand comparison"
                };
                let toggle = button(
                    ui,
                    icon_glyph(if comparison.expanded {
                        Icon::ChevronUp
                    } else {
                        Icon::ChevronDown
                    }),
                    ButtonTone::Text,
                );
                ui.ctx().accesskit_node_builder(toggle.id, |builder| {
                    builder.set_role(egui::accesskit::Role::Button);
                    builder.set_name(toggle_name);
                });
                focus_tooltip(ui, &toggle, toggle_name);
                let toggle = toggle.on_hover_text(toggle_name);
                if toggle.clicked() {
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
                    let disabled_reason = comparison_start_disabled_reason(comparison);
                    let start = ui.add_enabled(
                        disabled_reason.is_none(),
                        egui::Button::new(
                            RichText::new(format!(
                                "{}  Start test recording",
                                icon_glyph(Icon::Microphone)
                            ))
                            .color(colors.primary_button_text),
                        )
                        .fill(colors.primary_button_bg)
                        .min_size(Vec2::new(0.0, 40.0)),
                    );
                    ui.ctx().accesskit_node_builder(start.id, |builder| {
                        builder.set_role(egui::accesskit::Role::Button);
                        builder.set_name("Start test recording");
                    });
                    if let Some(reason) = disabled_reason {
                        ui.ctx().accesskit_node_builder(start.id, |builder| {
                            builder.set_description(reason);
                        });
                        start.clone().on_hover_text(reason);
                        ui.label(RichText::new(reason).small().color(colors.muted_text));
                    }
                    if start.clicked() {
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

fn comparison_start_disabled_reason(comparison: &ModelComparisonState) -> Option<&'static str> {
    if matches!(
        comparison.phase,
        super::state::ComparisonPhase::Recording | super::state::ComparisonPhase::Processing
    ) {
        Some("Wait for the current comparison to finish before starting another.")
    } else if comparison.selected_model_ids.len() < 2 {
        Some("Select at least two installed models before starting a comparison.")
    } else {
        None
    }
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
    let mut tab_ids = Vec::new();
    let mut tab_responses = Vec::new();
    let mut focus_tab = None;
    let tab_list_id = ui.make_persistent_id("settings-tab-list");
    let ctx = ui.ctx().clone();
    ctx.accesskit_node_builder(tab_list_id, |builder| {
        builder.set_role(egui::accesskit::Role::TabList);
        builder.set_name("Settings sections");
    });
    ctx.with_accessibility_parent(tab_list_id, || {
        ui.horizontal(|ui| {
            for (tab, label) in [
                (SettingsTab::General, "General"),
                (SettingsTab::Recording, "Recording"),
                (SettingsTab::Output, "Output"),
                (SettingsTab::Advanced, "Advanced"),
            ] {
                let response = tab_control(ui, tab, label, tab == active_tab);
                tab_ids.push((tab, response.id));
                tab_responses.push((tab, response.clone()));
                if response.clicked() {
                    action = ScreenAction::SetSettingsTab(tab);
                }
            }
        });
    });
    if tab_responses
        .iter()
        .any(|(_, response)| response.has_focus())
    {
        if ui.input(|input| input.key_pressed(egui::Key::ArrowRight)) {
            focus_tab = Some(next_tab(active_tab));
        } else if ui.input(|input| input.key_pressed(egui::Key::ArrowLeft)) {
            focus_tab = Some(previous_tab(active_tab));
        }
    }
    if let Some(target) = focus_tab {
        tab_responses
            .iter()
            .find(|(tab, _)| *tab == target)
            .expect("settings tab target is rendered")
            .1
            .request_focus();
        action = ScreenAction::SetSettingsTab(target);
    }
    ui.separator();
    ui.add_space(16.0);
    let panel = ui.allocate_ui_with_layout(
        Vec2::new(ui.available_width(), 0.0),
        Layout::top_down(Align::LEFT),
        |ui| match active_tab {
            SettingsTab::Recording => recording_settings_panel(ui, state, settings, &mut action),
            SettingsTab::General => general_settings_panel(ui, settings, &mut action),
            SettingsTab::Output => output_settings_panel(ui, settings, &mut action),
            SettingsTab::Advanced => advanced_settings_panel(ui, settings, &mut action),
        },
    );
    ui.ctx()
        .accesskit_node_builder(panel.response.id, |builder| {
            builder.set_role(egui::accesskit::Role::TabPanel);
            builder.set_name(match active_tab {
                SettingsTab::General => "General settings",
                SettingsTab::Recording => "Recording settings",
                SettingsTab::Output => "Output settings",
                SettingsTab::Advanced => "Advanced settings",
            });
        });
    let selected_tab_id = tab_ids
        .iter()
        .copied()
        .find_map(|(tab, id)| (tab == active_tab).then_some(id))
        .expect("selected settings tab is rendered");
    for (_, tab_id) in tab_ids {
        ui.ctx().accesskit_node_builder(tab_id, |builder| {
            builder.push_controlled(panel.response.id.value().into());
        });
    }
    ui.ctx()
        .accesskit_node_builder(panel.response.id, |builder| {
            builder.push_labelled_by(selected_tab_id.value().into());
        });
    action
}

fn tab_id(_: &egui::Ui, tab: SettingsTab) -> egui::Id {
    egui::Id::new(("settings-tab", tab))
}

fn tab_control(ui: &mut egui::Ui, tab: SettingsTab, label: &str, selected: bool) -> egui::Response {
    let colors = ui_palette(ui);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(96.0, 40.0), egui::Sense::hover());
    let response = ui.interact(rect, tab_id(ui, tab), egui::Sense::click());
    let fill = if selected {
        colors.active_card_bg
    } else if response.hovered() {
        colors.panel_bg
    } else {
        egui::Color32::TRANSPARENT
    };
    ui.painter().rect_filled(rect, Rounding::same(4.0), fill);
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        label,
        egui::FontId::proportional(14.0),
        colors.text,
    );
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, label));
    ui.ctx().accesskit_node_builder(response.id, |builder| {
        builder.set_role(egui::accesskit::Role::Tab);
        builder.set_name(label);
        builder.set_selected(selected);
    });
    paint_focus_ring(ui, &response, Rounding::same(4.0));
    response
}

fn radio_control(
    ui: &mut egui::Ui,
    mode: RecordingMode,
    label: &str,
    checked: bool,
) -> egui::Response {
    let colors = ui_palette(ui);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(112.0, 40.0), egui::Sense::hover());
    let response = ui.interact(
        rect,
        ui.make_persistent_id(("recording-mode", mode)),
        egui::Sense::click(),
    );
    let fill = if checked {
        colors.active_card_bg
    } else if response.hovered() {
        colors.panel_bg
    } else {
        egui::Color32::TRANSPARENT
    };
    ui.painter().rect_filled(rect, Rounding::same(4.0), fill);
    ui.painter().circle_stroke(
        rect.left_center() + Vec2::new(12.0, 0.0),
        7.0,
        Stroke::new(1.5, colors.muted_text),
    );
    if checked {
        ui.painter().circle_filled(
            rect.left_center() + Vec2::new(12.0, 0.0),
            4.0,
            colors.accent,
        );
    }
    ui.painter().text(
        rect.left_center() + Vec2::new(26.0, 0.0),
        egui::Align2::LEFT_CENTER,
        label,
        egui::FontId::proportional(14.0),
        colors.text,
    );
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, label));
    ui.ctx().accesskit_node_builder(response.id, |builder| {
        builder.set_role(egui::accesskit::Role::RadioButton);
        builder.set_name(label);
        builder.set_checked(if checked {
            egui::accesskit::Checked::True
        } else {
            egui::accesskit::Checked::False
        });
    });
    paint_focus_ring(ui, &response, Rounding::same(4.0));
    response
}

fn recording_settings_panel(
    ui: &mut egui::Ui,
    state: &TranscriptionState,
    settings: &RecordingSettingsView,
    action: &mut ScreenAction,
) {
    let colors = ui_palette(ui);
    card(ui, |ui| {
        ui.label(RichText::new("Recording behavior").strong());
        ui.add_space(12.0);
        let mut radio_ids = Vec::new();
        let radio_group_id = ui.make_persistent_id("recording-mode-group");
        let ctx = ui.ctx().clone();
        ctx.accesskit_node_builder(radio_group_id, |builder| {
            builder.set_role(egui::accesskit::Role::RadioGroup);
            builder.set_name("Recording mode");
        });
        ctx.with_accessibility_parent(radio_group_id, || {
            ui.horizontal(|ui| {
                ui.add_sized(
                    [270.0, 40.0],
                    egui::Label::new(RichText::new("Mode").color(colors.muted_text)),
                );
                for (mode, label) in [
                    (RecordingMode::PressOnce, "Press once"),
                    (RecordingMode::Hold, "Hold"),
                ] {
                    let response = radio_control(ui, mode, label, state.recording_mode == mode);
                    radio_ids.push(response.id);
                    if response.clicked() {
                        *action = ScreenAction::SetRecordingMode(mode);
                    }
                    if response.has_focus()
                        && ui.input(|input| {
                            input.key_pressed(egui::Key::ArrowRight)
                                || input.key_pressed(egui::Key::ArrowLeft)
                        })
                    {
                        let next = if mode == RecordingMode::PressOnce {
                            RecordingMode::Hold
                        } else {
                            RecordingMode::PressOnce
                        };
                        ui.memory_mut(|memory| {
                            memory.request_focus(ui.make_persistent_id(("recording-mode", next)))
                        });
                        *action = ScreenAction::SetRecordingMode(next);
                    }
                }
            });
        });
        let radio_group = radio_ids
            .iter()
            .map(|id| id.value().into())
            .collect::<Vec<_>>();
        for id in radio_ids {
            ui.ctx().accesskit_node_builder(id, |builder| {
                builder.set_radio_group(radio_group.clone());
            });
        }
        ui.add_space(8.0);
        ui.separator();
        ui.add_space(8.0);
        setting_row(ui, "Duration limit", |ui| {
            let mut duration = settings.duration_seconds;
            ComboBox::from_id_source("duration-limit")
                .selected_text(&settings.duration_label)
                .width(240.0)
                .show_ui(ui, |ui| {
                    for seconds in [15, 30, 60, 120, 300, 600] {
                        ui.selectable_value(&mut duration, seconds, format!("{seconds} seconds"));
                    }
                });
            if duration != settings.duration_seconds {
                *action = ScreenAction::SetDurationSeconds(duration);
            }
        });
        setting_row(ui, "Visual feedback", |ui| {
            let mut enabled = settings.provisional_feedback;
            let response = ui.checkbox(&mut enabled, "Show provisional words while recording");
            if response.clicked() {
                *action = ScreenAction::ToggleProvisionalFeedback;
            }
            ui.label(
                RichText::new("Improves visual feedback but may use more CPU.")
                    .small()
                    .color(colors.muted_text),
            );
        });
    });
    ui.add_space(16.0);
    card(ui, |ui| {
        ui.label(RichText::new("Audio input").strong());
        ui.add_space(12.0);
        setting_row(ui, "Device", |ui| {
            let mut selected = settings.selected_audio_device.clone();
            ComboBox::from_id_source("audio-device")
                .selected_text(&settings.device_label)
                .width(360.0)
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut selected, None, "OS default");
                    for device in &settings.audio_devices {
                        ui.selectable_value(&mut selected, Some(device.clone()), device);
                    }
                });
            if selected != settings.selected_audio_device {
                *action = ScreenAction::SetAudioDevice(selected);
            }
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
                *action = ScreenAction::RefreshDevices;
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
                *action = ScreenAction::ChangeShortcut;
            }
        });
    });
}

fn general_settings_panel(
    ui: &mut egui::Ui,
    settings: &RecordingSettingsView,
    action: &mut ScreenAction,
) {
    card(ui, |ui| {
        ui.label(RichText::new("General settings").strong());
        ui.add_space(8.0);
        let mut close_to_tray = settings.close_to_tray;
        if ui.checkbox(&mut close_to_tray, "Close to tray").changed() {
            *action = ScreenAction::SetCloseToTray(close_to_tray);
        }
        ui.label(
            RichText::new("Scribe keeps audio and transcripts on this device.")
                .color(ui_palette(ui).muted_text),
        );
    });
}

fn output_settings_panel(
    ui: &mut egui::Ui,
    settings: &RecordingSettingsView,
    action: &mut ScreenAction,
) {
    card(ui, |ui| {
        ui.label(RichText::new("Output settings").strong());
        ui.add_space(8.0);
        let mut auto_insert = settings.auto_insert_transcript;
        if ui
            .checkbox(&mut auto_insert, "Automatically insert final transcript")
            .changed()
        {
            *action = ScreenAction::SetAutoInsertTranscript(auto_insert);
        }
        ui.add_enabled_ui(auto_insert, |ui| {
            let mut restore = settings.restore_clipboard_after_insert;
            if ui
                .checkbox(&mut restore, "Restore clipboard after insert")
                .changed()
            {
                *action = ScreenAction::SetRestoreClipboardAfterInsert(restore);
            }
            ui.horizontal(|ui| {
                let label = ui.label("Paste delay (ms)");
                let mut delay = settings.paste_delay_ms as i64;
                if ui
                    .add(egui::DragValue::new(&mut delay).clamp_range(1..=1_000))
                    .labelled_by(label.id)
                    .changed()
                {
                    *action = ScreenAction::SetPasteDelayMs(delay as u64);
                }
            });
        });
    });
}

fn advanced_settings_panel(
    ui: &mut egui::Ui,
    settings: &RecordingSettingsView,
    action: &mut ScreenAction,
) {
    card(ui, |ui| {
        ui.label(RichText::new("Advanced settings").strong());
        ui.add_space(8.0);
        let mut preview = settings.provisional_feedback;
        if ui
            .checkbox(&mut preview, "Use live provisional preview")
            .changed()
        {
            *action = ScreenAction::ToggleProvisionalFeedback;
        }
        ui.label(
            RichText::new(
                "Preview text remains inside Scribe until final transcription completes.",
            )
            .color(ui_palette(ui).muted_text),
        );
    });
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

    fn render_route(route: UiRoute) -> egui::FullOutput {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let state = TranscriptionState {
            phase: TranscriptionPhase::Ready,
            ..Default::default()
        };
        let settings = RecordingSettingsView::default();
        let comparison = ModelComparisonState::default();
        ctx.run(Default::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                render_screen(
                    ui,
                    &ScreenView {
                        route,
                        transcription: &state,
                        models: &[],
                        comparison: &comparison,
                        recording_settings: &settings,
                    },
                )
            });
        })
    }

    fn render_transcribe(
        state: &TranscriptionState,
        models: &[ModelViewModel],
    ) -> egui::FullOutput {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let settings = RecordingSettingsView::default();
        let comparison = ModelComparisonState::default();
        ctx.run(Default::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                render_screen(
                    ui,
                    &ScreenView {
                        route: UiRoute::Transcribe,
                        transcription: state,
                        models,
                        comparison: &comparison,
                        recording_settings: &settings,
                    },
                )
            });
        })
    }

    #[test]
    fn elapsed_display_is_deterministic() {
        assert_eq!(format_elapsed(8_000), "00:08");
    }

    #[test]
    fn reference_recording_controls_have_named_square_actions() {
        for (phase, expected_name) in [
            (TranscriptionPhase::Ready, "Start recording"),
            (TranscriptionPhase::Listening, "Stop recording"),
        ] {
            let state = TranscriptionState {
                phase,
                selected_model_id: Some("base.en".into()),
                ..Default::default()
            };
            let output = render_transcribe(&state, &[]);
            let nodes = &output.platform_output.accesskit_update.unwrap().nodes;
            assert!(nodes.iter().any(|(_, node)| {
                node.role() == egui::accesskit::Role::Button && node.name() == Some(expected_name)
            }));
        }
    }

    #[test]
    fn model_selector_explains_why_it_is_disabled_during_unsafe_phases() {
        for (phase, reason) in [
            (
                TranscriptionPhase::Listening,
                "Model selection is unavailable while recording.",
            ),
            (
                TranscriptionPhase::Finalizing,
                "Model selection is unavailable while finalizing the current transcript.",
            ),
        ] {
            let state = TranscriptionState {
                phase,
                selected_model_id: Some("base.en".into()),
                ..Default::default()
            };
            let models = vec![ModelViewModel {
                id: "base.en".into(),
                display_name: "whisper.cpp base.en".into(),
                variant_label: "base.en".into(),
                ..Default::default()
            }];
            let output = render_transcribe(&state, &models);
            let nodes = &output.platform_output.accesskit_update.unwrap().nodes;
            assert!(nodes.iter().any(|(_, node)| {
                node.name() == Some("Change") && node.description() == Some(reason)
            }));
        }
    }

    #[test]
    fn no_model_recovery_keeps_committed_transcript_visible() {
        let state = TranscriptionState {
            phase: TranscriptionPhase::NoModel,
            committed_transcript: "Keep this transcript after model removal.".into(),
            ..Default::default()
        };
        let output = render_transcribe(&state, &[]);
        let nodes = &output.platform_output.accesskit_update.unwrap().nodes;
        assert!(nodes.iter().any(|(_, node)| {
            node.name() == Some("Add a speech model to continue transcribing")
        }));
        assert!(
            nodes.iter().any(|(_, node)| {
                node.name() == Some("Keep this transcript after model removal.")
            })
        );
    }

    #[test]
    fn model_rows_separate_family_and_variant_labels() {
        let model = ModelViewModel {
            display_name: "whisper.cpp base.en".into(),
            variant_label: "base.en".into(),
            ..Default::default()
        };
        assert_eq!(model_family_name(&model), "whisper.cpp");

        let custom = ModelViewModel {
            display_name: "Local model".into(),
            variant_label: "custom-v1".into(),
            ..Default::default()
        };
        assert_eq!(model_family_name(&custom), "Local model");
    }

    #[test]
    fn model_footer_spacer_uses_remaining_height_and_panel_state() {
        assert_eq!(models_footer_spacer(500.0, false, true), 368.0);
        assert_eq!(models_footer_spacer(500.0, true, true), 130.0);
        assert_eq!(models_footer_spacer(100.0, true, true), 16.0);
    }

    #[test]
    fn microphone_error_alert_groups_message_and_recovery_actions() {
        let mut state = TranscriptionState {
            phase: TranscriptionPhase::MicrophoneError,
            notice: Some("Scribe couldn’t access your microphone".into()),
            ..Default::default()
        };
        state.selected_model_id = Some("base.en".into());
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let settings = RecordingSettingsView::default();
        let comparison = ModelComparisonState::default();
        let output = ctx.run(Default::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                render_screen(
                    ui,
                    &ScreenView {
                        route: UiRoute::Transcribe,
                        transcription: &state,
                        models: &[],
                        comparison: &comparison,
                        recording_settings: &settings,
                    },
                )
            });
        });
        let nodes = &output.platform_output.accesskit_update.unwrap().nodes;
        let alert = nodes
            .iter()
            .find(|(_, node)| {
                node.role() == egui::accesskit::Role::Alert
                    && node.name() == Some("Microphone access error")
            })
            .unwrap();
        for name in ["Open audio settings", "Try again"] {
            let button = nodes
                .iter()
                .find(|(_, node)| {
                    node.role() == egui::accesskit::Role::Button && node.name() == Some(name)
                })
                .unwrap();
            assert!(accesskit_descends_from(nodes, alert.0, button.0));
        }
    }

    fn accesskit_descends_from(
        nodes: &[(egui::accesskit::NodeId, egui::accesskit::Node)],
        ancestor: egui::accesskit::NodeId,
        target: egui::accesskit::NodeId,
    ) -> bool {
        let mut pending = vec![ancestor];
        while let Some(id) = pending.pop() {
            let Some((_, node)) = nodes.iter().find(|(node_id, _)| *node_id == id) else {
                continue;
            };
            if node.children().contains(&target) {
                return true;
            }
            pending.extend(node.children());
        }
        false
    }

    #[test]
    fn comparison_start_explains_its_two_model_requirement_when_disabled() {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let state = TranscriptionState::default();
        let settings = RecordingSettingsView::default();
        let comparison = ModelComparisonState {
            expanded: true,
            ..Default::default()
        };
        let output = ctx.run(Default::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                render_screen(
                    ui,
                    &ScreenView {
                        route: UiRoute::Models,
                        transcription: &state,
                        models: &[],
                        comparison: &comparison,
                        recording_settings: &settings,
                    },
                )
            });
        });
        let update = output.platform_output.accesskit_update.unwrap();
        assert!(update.nodes.iter().any(|(_, node)| {
            node.name() == Some("Start test recording")
                && node.description()
                    == Some("Select at least two installed models before starting a comparison.")
        }));
        assert!(update.nodes.iter().any(|(_, node)| {
            node.role() == egui::accesskit::Role::Button
                && node.name() == Some("Collapse comparison")
        }));
    }

    #[test]
    fn comparison_start_reason_distinguishes_busy_from_insufficient_selection() {
        let mut comparison = ModelComparisonState::default();
        assert_eq!(
            comparison_start_disabled_reason(&comparison),
            Some("Select at least two installed models before starting a comparison.")
        );

        comparison.selected_model_ids.insert("base.en".into());
        comparison.selected_model_ids.insert("tiny.en".into());
        comparison.phase = super::super::state::ComparisonPhase::Processing;
        assert_eq!(
            comparison_start_disabled_reason(&comparison),
            Some("Wait for the current comparison to finish before starting another.")
        );
    }

    #[test]
    fn settings_panels_match_the_selected_tab() {
        for (tab, expected, absent) in [
            (
                SettingsTab::General,
                "General settings",
                "Recording behavior",
            ),
            (
                SettingsTab::Recording,
                "Recording behavior",
                "Output settings",
            ),
            (SettingsTab::Output, "Output settings", "Recording behavior"),
            (
                SettingsTab::Advanced,
                "Advanced settings",
                "Recording behavior",
            ),
        ] {
            let output = render_route(UiRoute::Settings(tab));
            let nodes = &output.platform_output.accesskit_update.unwrap().nodes;
            assert!(
                nodes
                    .iter()
                    .any(|(_, node)| node.role() == egui::accesskit::Role::TabPanel
                        && node.name()
                            == Some(match tab {
                                SettingsTab::General => "General settings",
                                SettingsTab::Recording => "Recording settings",
                                SettingsTab::Output => "Output settings",
                                SettingsTab::Advanced => "Advanced settings",
                            }))
            );
            assert!(nodes.iter().any(|(_, node)| node.name() == Some(expected)));
            assert!(!nodes.iter().any(|(_, node)| node.name() == Some(absent)));
        }
    }

    #[test]
    fn custom_tabs_and_radios_have_only_their_native_selection_semantics() {
        let output = render_route(UiRoute::Settings(SettingsTab::Recording));
        let nodes = &output.platform_output.accesskit_update.unwrap().nodes;
        assert!(
            nodes
                .iter()
                .any(|(_, node)| node.role() == egui::accesskit::Role::TabList)
        );
        assert!(
            nodes
                .iter()
                .any(|(_, node)| node.role() == egui::accesskit::Role::RadioGroup)
        );
        let selected_tab = nodes
            .iter()
            .find(|(_, node)| {
                node.role() == egui::accesskit::Role::Tab
                    && node.name() == Some("Recording")
                    && node.is_selected() == Some(true)
            })
            .unwrap();
        let panel = nodes
            .iter()
            .find(|(_, node)| node.role() == egui::accesskit::Role::TabPanel)
            .unwrap();
        assert!(
            nodes
                .iter()
                .filter(|(_, node)| node.role() == egui::accesskit::Role::Tab)
                .all(|(_, node)| node.controls().contains(&panel.0))
        );
        assert!(panel.1.labelled_by().contains(&selected_tab.0));
        assert!(nodes.iter().any(|(_, node)| {
            node.role() == egui::accesskit::Role::RadioButton
                && node.name() == Some("Press once")
                && node.checked() == Some(egui::accesskit::Checked::True)
                && node.radio_group().len() == 2
        }));
    }

    #[test]
    fn repeated_arrow_keys_move_settings_tab_selection_and_focus() {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let state = TranscriptionState::default();
        let settings_view = RecordingSettingsView::default();
        let mut active = SettingsTab::General;
        let _ = ctx.run(
            egui::RawInput {
                focused: true,
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let _ = settings(ui, active, &state, &settings_view);
                    ui.memory_mut(|memory| memory.request_focus(tab_id(ui, active)));
                });
            },
        );
        let _ = ctx.run(
            egui::RawInput {
                focused: true,
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let _ = settings(ui, active, &state, &settings_view);
                });
            },
        );
        for expected in [SettingsTab::Recording, SettingsTab::Output] {
            let mut arrow_action = ScreenAction::None;
            let _ = ctx.run(
                egui::RawInput {
                    focused: true,
                    events: vec![egui::Event::Key {
                        key: egui::Key::ArrowRight,
                        physical_key: None,
                        pressed: true,
                        repeat: false,
                        modifiers: egui::Modifiers::NONE,
                    }],
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        arrow_action = settings(ui, active, &state, &settings_view);
                    });
                },
            );
            assert_eq!(arrow_action, ScreenAction::SetSettingsTab(expected));
            active = expected;
            let output = ctx.run(
                egui::RawInput {
                    focused: true,
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        let _ = settings(ui, active, &state, &settings_view);
                    });
                },
            );
            assert!(
                output
                    .platform_output
                    .accesskit_update
                    .unwrap()
                    .nodes
                    .iter()
                    .any(|(_, node)| {
                        node.role() == egui::accesskit::Role::Tab
                            && node.is_selected() == Some(true)
                            && node.name()
                                == Some(match active {
                                    SettingsTab::General => "General",
                                    SettingsTab::Recording => "Recording",
                                    SettingsTab::Output => "Output",
                                    SettingsTab::Advanced => "Advanced",
                                })
                    })
            );
        }
    }
}
