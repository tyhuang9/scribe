//! Shared, backend-neutral egui screen renderers.

use eframe::egui::{
    self, Align, Align2, ComboBox, Frame, Layout, Margin, Rect, RichText, Rounding, Sense, Stroke,
    Vec2,
};

use super::{
    controls::{
        ButtonTone, Icon, badge, button, card, focus_tooltip, icon_glyph, keycap, paint_focus_ring,
    },
    state::{
        ComparisonPhase, ComparisonResultPhase, ModelComparisonState, ModelDialog,
        ModelDownloadState, ModelManagementState, ModelSizeTier, ModelSpeedTier, ModelViewModel,
        RecordingMode, SettingsSaveState, SettingsTab, TranscriptionPhase, TranscriptionState,
        UiRoute,
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
    pub input_sensitivity_percent: u8,
    pub auto_insert_transcript: bool,
    pub output_label: String,
    pub show_restore_clipboard: bool,
    pub output_notice: Option<String>,
    pub restore_clipboard_after_insert: bool,
    pub paste_delay_ms: u64,
    pub active_model_label: String,
    pub hotkey_input: String,
    pub hotkey_capture_active: bool,
    pub hotkey_capture_status: Option<String>,
    pub theme_label: String,
    pub overlay_label: String,
    pub overlay_available: bool,
    pub vad_enabled: bool,
    pub speech_confirmation_ms: u32,
    pub internal_pause_ms: u32,
    pub endpoint_silence_ms: u32,
    pub pre_roll_ms: u32,
    pub post_roll_ms: u32,
    pub streaming_label: String,
    pub acceleration_label: String,
    pub gpu_available: bool,
    pub overlay_position_label: String,
    pub debug_mode: bool,
    pub history_mode_label: String,
    pub history_locked: bool,
    pub max_history_entries: u32,
    pub transcript_retention_days: Option<u32>,
    pub audio_retention_days: Option<u32>,
    pub store_application_identity: bool,
    pub diagnostics: Vec<String>,
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
            input_sensitivity_percent: 50,
            auto_insert_transcript: false,
            output_label: "Automatically insert final transcript".into(),
            show_restore_clipboard: cfg!(target_os = "windows"),
            output_notice: None,
            restore_clipboard_after_insert: true,
            paste_delay_ms: 60,
            active_model_label: "No model selected".into(),
            hotkey_input: "Ctrl+Shift+Space".into(),
            hotkey_capture_active: false,
            hotkey_capture_status: None,
            theme_label: "Light".into(),
            overlay_label: "Live".into(),
            overlay_available: true,
            vad_enabled: true,
            speech_confirmation_ms: 150,
            internal_pause_ms: 450,
            endpoint_silence_ms: 900,
            pre_roll_ms: 250,
            post_roll_ms: 200,
            streaming_label: "Auto".into(),
            acceleration_label: "Auto".into(),
            gpu_available: false,
            overlay_position_label: "Bottom".into(),
            debug_mode: false,
            history_mode_label: "Off".into(),
            history_locked: false,
            max_history_entries: 100,
            transcript_retention_days: None,
            audio_retention_days: None,
            store_application_identity: false,
            diagnostics: Vec::new(),
            save_state: SettingsSaveState::Clean,
        }
    }
}

pub(crate) struct ScreenView<'a> {
    pub route: UiRoute,
    pub transcription: &'a TranscriptionState,
    pub models: &'a [ModelViewModel],
    pub model_catalog: &'a [ModelViewModel],
    pub comparison: &'a ModelComparisonState,
    pub model_management: &'a ModelManagementState,
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
    StopComparison,
    EditComparisonReference(String),
    ApplyComparisonReference,
    ClearComparisonReference,
    SelectModel(String),
    InstallModel(String),
    CancelModelInstall(String),
    RepairModelRuntime(String),
    MaintainModelRuntime(String),
    ShowModelDetails(String),
    RequestModelRemoval(String),
    ConfirmModelRemoval(String),
    CloseModelDialog,
    SetSettingsTab(SettingsTab),
    SetCloseToTray(bool),
    OpenModelSettings,
    SetHotkeyInput(String),
    ApplyHotkey,
    SetTheme(String),
    SetOverlayMode(String),
    SetRecordingMode(RecordingMode),
    SetDurationSeconds(u32),
    ToggleProvisionalFeedback,
    SetAudioDevice(Option<String>),
    SetInputSensitivity(u8),
    RefreshDevices,
    ChangeShortcut,
    SetAutoInsertTranscript(bool),
    SetRestoreClipboardAfterInsert(bool),
    SetPasteDelayMs(u64),
    SetVadEnabled(bool),
    SetSpeechConfirmationMs(u32),
    SetInternalPauseMs(u32),
    SetEndpointSilenceMs(u32),
    SetPreRollMs(u32),
    SetPostRollMs(u32),
    SetStreamingMode(String),
    SetAcceleration(String),
    SetOverlayPosition(String),
    SetDebugMode(bool),
    SetHistoryMode(String),
    SetMaxHistoryEntries(u32),
    SetTranscriptRetentionDays(Option<u32>),
    SetAudioRetentionDays(Option<u32>),
    SetStoreApplicationIdentity(bool),
}

pub(crate) fn render_screen(ui: &mut egui::Ui, view: &ScreenView<'_>) -> ScreenAction {
    match view.route {
        UiRoute::Transcribe => transcribe(ui, view.transcription, view.models),
        UiRoute::Models => models(
            ui,
            view.models,
            view.model_catalog,
            view.comparison,
            view.model_management,
        ),
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
        TranscriptionPhase::RequestingMicrophone => {
            ui.spinner();
            ui.vertical(|ui| {
                let status = ui.label(RichText::new("Requesting microphone access…").strong());
                ui.ctx().accesskit_node_builder(status.id, |builder| {
                    builder.set_live(egui::accesskit::Live::Polite);
                    builder.set_live_atomic();
                });
                ui.label("Recording will start after access is granted.");
            });
        }
        TranscriptionPhase::ModelLoading => {
            ui.spinner();
            ui.vertical(|ui| {
                let status = ui.label(RichText::new("Loading speech model…").strong());
                ui.ctx().accesskit_node_builder(status.id, |builder| {
                    builder.set_live(egui::accesskit::Live::Polite);
                    builder.set_live_atomic();
                });
                ui.label("Recording will be available when the model is ready.");
            });
        }
        TranscriptionPhase::ModelError => {
            ui.vertical(|ui| {
                ui.label(
                    RichText::new("Speech model unavailable")
                        .strong()
                        .color(colors.error),
                );
                ui.label("Open model settings to repair or choose another model.");
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
            let transcript_heading = ui.label(RichText::new("Transcript").strong());
            ui.ctx().accesskit_node_builder(transcript_heading.id, |builder| {
                builder.set_role(egui::accesskit::Role::Heading);
            });
            ui.add_space(8.0);
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
                if state.phase == TranscriptionPhase::ModelError {
                    let response = neutral_notice(
                        ui,
                        "The selected speech model could not be loaded. Open model settings to repair it or choose another model.",
                    );
                    ui.ctx().accesskit_node_builder(response.id, |builder| {
                        builder.set_role(egui::accesskit::Role::Alert);
                        builder.set_live(egui::accesskit::Live::Assertive);
                        builder.set_live_atomic();
                    });
                    if button(ui, "Open model settings", ButtonTone::Danger).clicked() {
                        action = ScreenAction::ChangeModel;
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
                        let reason = if !enabled {
                            "Clear is unavailable while recording or finalizing the current transcript."
                        } else {
                            "Clear is unavailable until a final transcript exists."
                        };
                        ui.ctx().accesskit_node_builder(clear.id, |builder| {
                            builder.set_description(reason)
                        });
                        focus_tooltip(ui, &clear, reason);
                        clear.clone().on_hover_text(reason);
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

fn microphone_error_notice(ui: &mut egui::Ui, technical_detail: &str) -> ScreenAction {
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
                        ui.vertical(|ui| {
                            ui.set_max_width(message_width);
                            ui.label(
                                RichText::new("Scribe couldn’t access your microphone.")
                                    .strong()
                                    .color(colors.error_text),
                            );
                            let detail = technical_detail.trim();
                            if !detail.is_empty()
                                && detail != "Scribe couldn’t access your microphone."
                            {
                                ui.label(RichText::new(detail).small().color(colors.error_text));
                            }
                        });
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

fn comparison_surface_width(available_width: f32) -> f32 {
    available_width.max(0.0)
}

fn comparison_content_min_width(surface_width: f32, inner_margin: f32) -> f32 {
    (surface_width - inner_margin * 2.0).max(0.0)
}

fn models(
    ui: &mut egui::Ui,
    models: &[ModelViewModel],
    model_catalog: &[ModelViewModel],
    comparison: &ModelComparisonState,
    management: &ModelManagementState,
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
            if let Some(notice) = management.removal_notice.as_deref() {
                let response = ui.label(RichText::new(notice).color(colors.muted_text));
                ui.ctx().accesskit_node_builder(response.id, |builder| {
                    builder.set_live(egui::accesskit::Live::Polite);
                    builder.set_live_atomic();
                });
            }
        });
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let add_models = button(
                ui,
                format!("{}  Add models", icon_glyph(Icon::Plus)),
                ButtonTone::Primary,
            );
            if management.restore_add_focus {
                add_models.request_focus();
            }
            if add_models.clicked() {
                action = ScreenAction::AddModel;
            }
            let eligible_models = models
                .iter()
                .filter(|model| {
                    model.installed
                        && model.ready
                        && model.compatibility != super::state::ModelCompatibility::Incompatible
                })
                .count();
            let compare_disabled_reason = (eligible_models < 2)
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
    if models.is_empty() {
        card(ui, |ui| {
            ui.label(RichText::new("No installed models").strong());
            ui.label(
                RichText::new("Add a curated model to begin local transcription.")
                    .color(colors.muted_text),
            );
        });
        ui.add_space(8.0);
    }
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
            ui.add_space(8.0);
            ui.horizontal_wrapped(|ui| {
                let primary = ui
                    .add_enabled_ui(model.primary_action_enabled, |ui| {
                        button(ui, &model.primary_action_label, ButtonTone::Secondary)
                    })
                    .inner;
                if let Some(reason) = &model.primary_action_disabled_reason {
                    ui.ctx().accesskit_node_builder(primary.id, |builder| {
                        builder.set_description(reason.as_str());
                    });
                    focus_tooltip(ui, &primary, reason);
                    primary.clone().on_hover_text(reason);
                }
                if primary.clicked() {
                    action = if model.primary_action_repairs_runtime {
                        ScreenAction::RepairModelRuntime(model.id.clone())
                    } else {
                        ScreenAction::SelectModel(model.id.clone())
                    };
                }
                let details = button(ui, "Details", ButtonTone::Text);
                ui.ctx().accesskit_node_builder(details.id, |builder| {
                    builder.set_role(egui::accesskit::Role::Button);
                    builder.set_name(format!("Details for {}", model.display_name));
                });
                if management.restore_details_focus.as_deref() == Some(model.id.as_str()) {
                    details.request_focus();
                }
                if details.clicked() {
                    action = ScreenAction::ShowModelDetails(model.id.clone());
                }
                let remove_reason = if model.active {
                    Some("Select another ready model before removing the active model.")
                } else if model.custom {
                    Some("Custom model files are not managed by Scribe and will not be deleted.")
                } else if !model.removal_supported {
                    Some("This model is not an app-managed download and cannot be removed here.")
                } else {
                    None
                };
                let remove = ui.add_enabled(remove_reason.is_none(), egui::Button::new("Remove"));
                ui.ctx().accesskit_node_builder(remove.id, |builder| {
                    builder.set_role(egui::accesskit::Role::Button);
                    builder.set_name(format!("Remove {}", model.display_name));
                });
                if management.restore_remove_focus.as_deref() == Some(model.id.as_str()) {
                    remove.request_focus();
                }
                if let Some(reason) = remove_reason {
                    ui.ctx().accesskit_node_builder(remove.id, |builder| {
                        builder.set_description(reason);
                    });
                    ui.label(RichText::new(reason).small().color(colors.muted_text));
                    remove.clone().on_hover_text(reason);
                }
                if remove.clicked() {
                    action = ScreenAction::RequestModelRemoval(model.id.clone());
                }
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
    let comparison_width = comparison_surface_width(ui.available_width());
    let comparison_surface = Frame::none()
        .fill(colors.card_bg)
        .stroke(Stroke::new(1.0, colors.border))
        .rounding(Rounding::same(5.0))
        .inner_margin(Margin::same(16.0))
        .show(ui, |ui| {
            ui.set_min_width(comparison_content_min_width(comparison_width, 16.0));
            let header_min = ui.cursor().min;
            let mut toggle_clicked = false;
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
                let toggle = ui
                    .with_layout(Layout::right_to_left(Align::Center), |ui| {
                        button(
                            ui,
                            icon_glyph(if comparison.expanded {
                                Icon::ChevronUp
                            } else {
                                Icon::ChevronDown
                            }),
                            ButtonTone::Text,
                        )
                    })
                    .inner;
                ui.ctx().accesskit_node_builder(toggle.id, |builder| {
                    builder.set_role(egui::accesskit::Role::Button);
                    builder.set_name(toggle_name);
                    builder.set_expanded(comparison.expanded);
                });
                focus_tooltip(ui, &toggle, toggle_name);
                let toggle = toggle.on_hover_text(toggle_name);
                toggle_clicked = toggle.clicked();
            });
            let header_rect = Rect::from_min_max(header_min, ui.cursor().min);
            let header = ui.interact(
                header_rect,
                ui.make_persistent_id("comparison-header"),
                Sense::click(),
            );
            if header.clicked() && !toggle_clicked {
                action = ScreenAction::ToggleComparison;
            }
            if comparison.expanded {
                ui.add_space(12.0);
                ui.horizontal_wrapped(|ui| {
                    for model in models.iter().filter(|model| {
                        model.installed
                            && model.ready
                            && model.compatibility != super::state::ModelCompatibility::Incompatible
                    }) {
                        let selected = comparison.selected_model_ids.contains(&model.id);
                        let selection_disabled = matches!(
                            comparison.phase,
                            ComparisonPhase::Recording | ComparisonPhase::Processing
                        ) || (!selected
                            && comparison.selected_model_ids.len() >= 4);
                        let mut checked = selected;
                        let response = ui.add_enabled(
                            !selection_disabled,
                            egui::Checkbox::new(&mut checked, &model.display_name),
                        );
                        ui.ctx().accesskit_node_builder(response.id, |builder| {
                            builder.set_role(egui::accesskit::Role::CheckBox);
                            builder.set_name(model.display_name.as_str());
                        });
                        if selection_disabled {
                            let reason = if matches!(
                                comparison.phase,
                                ComparisonPhase::Recording | ComparisonPhase::Processing
                            ) {
                                "Model selection is locked during a comparison."
                            } else {
                                "A comparison can include at most four models."
                            };
                            ui.ctx().accesskit_node_builder(response.id, |builder| {
                                builder.set_description(reason);
                            });
                            response.clone().on_hover_text(reason);
                        }
                        if response.clicked() {
                            action = ScreenAction::ToggleComparisonModel(model.id.clone());
                        }
                    }
                    if comparison.phase == ComparisonPhase::Recording {
                        ui.label(format!(
                            "Recording {:.1}s",
                            comparison.recording_elapsed_ms as f32 / 1_000.0
                        ));
                        let stop =
                            ui.add_sized(Vec2::new(0.0, 44.0), egui::Button::new("Stop recording"));
                        ui.ctx().accesskit_node_builder(stop.id, |builder| {
                            builder.set_role(egui::accesskit::Role::Button);
                            builder.set_name("Stop comparison recording");
                        });
                        if stop.clicked() {
                            action = ScreenAction::StopComparison;
                        }
                    } else {
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
                            .min_size(Vec2::new(0.0, 44.0)),
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
                    }
                });
                if let Some(feedback) = comparison.selection_feedback.as_deref() {
                    ui.label(RichText::new(feedback).small().color(colors.warning));
                }
                ui.separator();
                ui.label(RichText::new("Reference transcript (optional)").strong());
                let mut reference_draft = comparison.reference_draft.clone();
                let reference = ui.add(
                    egui::TextEdit::multiline(&mut reference_draft)
                        .id_source("comparison-reference-transcript")
                        .hint_text("Paste the words that were spoken")
                        .desired_rows(3),
                );
                ui.ctx().accesskit_node_builder(reference.id, |builder| {
                    builder.set_name("Reference transcript");
                    builder.set_description(
                        "Optional reference text used to calculate word error rate after the run.",
                    );
                });
                if reference.changed() {
                    action = ScreenAction::EditComparisonReference(reference_draft);
                }
                ui.horizontal(|ui| {
                    if ui
                        .add_sized(Vec2::new(0.0, 44.0), egui::Button::new("Apply reference"))
                        .clicked()
                    {
                        action = ScreenAction::ApplyComparisonReference;
                    }
                    if ui
                        .add_sized(Vec2::new(0.0, 44.0), egui::Button::new("Clear reference"))
                        .clicked()
                    {
                        action = ScreenAction::ClearComparisonReference;
                    }
                    ui.label(
                        RichText::new(if comparison.reference_transcript.is_some() {
                            "Reference applied"
                        } else {
                            "No reference applied"
                        })
                        .small()
                        .color(colors.muted_text),
                    );
                });
                ui.separator();
                render_comparison_results(ui, models, comparison);
            }
        });
    ui.ctx().accesskit_node_builder(comparison_surface.response.id, |builder| {
        builder.set_role(egui::accesskit::Role::Group);
        builder.set_name("Model comparison surface");
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
    if management.dialog.is_some() {
        model_dialog_interaction_shield(ui.ctx());
    }
    if management.dialog.is_some() && ui.input(|input| input.key_pressed(egui::Key::Escape)) {
        return ScreenAction::CloseModelDialog;
    }
    match &management.dialog {
        Some(ModelDialog::Add) => {
            let mut open = true;
            let dialog = egui::Window::new("Add models")
                .collapsible(false)
                .resizable(false)
                .open(&mut open)
                .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
                .show(ui.ctx(), |ui| {
                    if let Some(reason) = &management.mutation_block_reason {
                        ui.label(RichText::new(reason).color(colors.warning));
                        ui.add_space(8.0);
                    }
                    for model in model_catalog {
                        card(ui, |ui| {
                            ui.label(RichText::new(&model.display_name).strong());
                            ui.label(RichText::new(&model.variant_label).small().color(colors.muted_text));
                            if let Some(description) = &model.description {
                                ui.label(description);
                            }
                            ui.horizontal_wrapped(|ui| {
                                ui.label(RichText::new(model_download_label(model)).color(colors.muted_text));
                                if let Some(total) = model.total_bytes {
                                    ui.label(RichText::new(format_bytes(total)).small().color(colors.muted_text));
                                }
                                let can_cancel = model.cancel_supported;
                                let install = ui.add_enabled(
                                    model.install_action_enabled,
                                    egui::Button::new(model_install_action_label(model.download_state)),
                                );
                                let install_reason = if !model.install_supported {
                                    Some("This model has no supported managed download in this build.")
                                } else {
                                    management.mutation_block_reason.as_deref()
                                };
                                if let Some(reason) = install_reason {
                                    ui.ctx().accesskit_node_builder(install.id, |builder| {
                                        builder.set_description(reason);
                                    });
                                    ui.label(RichText::new(reason).small().color(colors.muted_text));
                                    install.clone().on_hover_text(reason);
                                }
                                if model.download_state == ModelDownloadState::Downloading {
                                    let progress_value = model.total_bytes.filter(|total| *total > 0).map_or(0.0, |total| {
                                        (model.downloaded_bytes as f32 / total as f32).clamp(0.0, 1.0)
                                    });
                                    let progress = ui.add(egui::ProgressBar::new(progress_value).text(model_download_label(model)));
                                    ui.ctx().accesskit_node_builder(progress.id, |builder| {
                                        builder.set_role(egui::accesskit::Role::ProgressIndicator);
                                        builder.set_name(format!("{} installation progress", model.display_name));
                                        builder.set_description(model_download_label(model));
                                        builder.set_numeric_value(f64::from(progress_value * 100.0));
                                        builder.set_min_numeric_value(0.0);
                                        builder.set_max_numeric_value(100.0);
                                    });
                                }
                                if install.clicked() {
                                    action = ScreenAction::InstallModel(model.id.clone());
                                }
                                if ui.add_enabled(can_cancel, egui::Button::new("Cancel")).clicked() {
                                    action = ScreenAction::CancelModelInstall(model.id.clone());
                                }
                            });
                        });
                        ui.add_space(6.0);
                    }
                    let close = button(ui, "Close", ButtonTone::Secondary);
                    if management.focus_dialog_initial {
                        close.request_focus();
                    }
                    if close.clicked() {
                        action = ScreenAction::CloseModelDialog;
                    }
                });
            if let Some(dialog) = dialog {
                ui.ctx()
                    .accesskit_node_builder(dialog.response.id, |builder| {
                        builder.set_role(egui::accesskit::Role::Dialog);
                        builder.set_name("Add models");
                    });
            }
            if !open {
                action = ScreenAction::CloseModelDialog;
            }
        }
        Some(ModelDialog::Details(id)) => {
            if let Some(model) = model_catalog
                .iter()
                .chain(models.iter())
                .find(|model| &model.id == id)
            {
                let mut open = true;
                let dialog = egui::Window::new("Model details")
                    .collapsible(false)
                    .resizable(false)
                    .open(&mut open)
                    .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
                    .show(ui.ctx(), |ui| {
                        ui.label(RichText::new(&model.display_name).strong());
                        if let Some(description) = &model.description {
                            ui.label(description);
                        }
                        ui.label(format!("Status: {}", model_download_label(model)));
                        ui.label(format!("Languages: {}", model.language_summary));
                        if let Some(size) = model.total_bytes {
                            ui.label(format!("Download: {}", format_bytes(size)));
                        }
                        if let Some(size) = model.disk_bytes {
                            ui.label(format!("On disk: {}", format_bytes(size)));
                        }
                        if model.custom {
                            ui.label(
                                RichText::new(
                                    "This is a custom model. Scribe will not delete its files.",
                                )
                                .color(colors.muted_text),
                            );
                        }
                        ui.add_space(8.0);
                        let runtime_maintenance =
                            egui::CollapsingHeader::new("Runtime maintenance")
                                .default_open(false)
                                .show(ui, |ui| {
                                    ui.label(format!("Status: {}", model.runtime_status_label));
                                    if let Some(version) = &model.runtime_version_label {
                                        ui.label(version);
                                    }
                                    if let Some(storage) = &model.runtime_storage_label {
                                        ui.label(storage);
                                    }
                                    if let Some(detail) = &model.runtime_detail {
                                        ui.label(RichText::new(detail).color(colors.muted_text));
                                    }
                                    if let Some(label) = &model.runtime_action_label {
                                        let runtime_action = ui
                                            .add_enabled_ui(model.runtime_action_enabled, |ui| {
                                                button(ui, label, ButtonTone::Secondary)
                                            })
                                            .inner;
                                        if let Some(reason) = &model.runtime_action_disabled_reason
                                        {
                                            ui.ctx().accesskit_node_builder(
                                                runtime_action.id,
                                                |builder| builder.set_description(reason.as_str()),
                                            );
                                            focus_tooltip(ui, &runtime_action, reason);
                                            runtime_action.clone().on_hover_text(reason);
                                        }
                                        if runtime_action.clicked() {
                                            action = ScreenAction::MaintainModelRuntime(
                                                model.id.clone(),
                                            );
                                        }
                                    }
                                });
                        ui.ctx().accesskit_node_builder(
                            runtime_maintenance.header_response.id,
                            |builder| {
                                builder.set_expanded(runtime_maintenance.body_response.is_some());
                            },
                        );
                        ui.add_space(8.0);
                        let close = button(ui, "Close", ButtonTone::Secondary);
                        if management.focus_dialog_initial {
                            close.request_focus();
                        }
                        if close.clicked() {
                            action = ScreenAction::CloseModelDialog;
                        }
                    });
                if let Some(dialog) = dialog {
                    ui.ctx()
                        .accesskit_node_builder(dialog.response.id, |builder| {
                            builder.set_role(egui::accesskit::Role::Dialog);
                            builder.set_name(format!("Model details for {}", model.display_name));
                        });
                }
                if !open {
                    action = ScreenAction::CloseModelDialog;
                }
            }
        }
        Some(ModelDialog::Remove(id)) => {
            if let Some(model) = models.iter().find(|model| &model.id == id) {
                let mut open = true;
                let dialog = egui::Window::new("Remove model?")
                    .collapsible(false)
                    .resizable(false)
                    .open(&mut open)
                    .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
                    .show(ui.ctx(), |ui| {
                        ui.label(format!("Remove {} from Scribe?", model.display_name));
                        ui.label(RichText::new("Only Scribe-managed artifact files are removed. This cannot be undone.").color(colors.warning));
                        ui.horizontal(|ui| {
                            let cancel = button(ui, "Cancel", ButtonTone::Secondary);
                            if management.focus_dialog_initial {
                                cancel.request_focus();
                            }
                            if cancel.clicked() { action = ScreenAction::CloseModelDialog; }
                            if button(ui, "Remove", ButtonTone::Danger).clicked() { action = ScreenAction::ConfirmModelRemoval(model.id.clone()); }
                        });
                    });
                if let Some(dialog) = dialog {
                    ui.ctx()
                        .accesskit_node_builder(dialog.response.id, |builder| {
                            builder.set_role(egui::accesskit::Role::AlertDialog);
                            builder.set_name(format!("Remove {}", model.display_name));
                        });
                }
                if !open {
                    action = ScreenAction::CloseModelDialog;
                }
            }
        }
        None => {}
    }
    action
}

fn model_download_label(model: &ModelViewModel) -> String {
    match model.download_state {
        ModelDownloadState::Downloading => match model.total_bytes.filter(|total| *total > 0) {
            Some(total) => format!(
                "Downloading {:.0}%",
                model.downloaded_bytes as f64 / total as f64 * 100.0
            ),
            None => format!("Downloading {}", format_bytes(model.downloaded_bytes)),
        },
        ModelDownloadState::Queued => "Queued".to_owned(),
        ModelDownloadState::Verifying => "Verifying".to_owned(),
        ModelDownloadState::Extracting => "Extracting".to_owned(),
        ModelDownloadState::Installed => "Installed".to_owned(),
        ModelDownloadState::Failed => model
            .error_message
            .clone()
            .unwrap_or_else(|| "Install failed".to_owned()),
        ModelDownloadState::Cancelled => "Cancelled; partial download can be resumed.".to_owned(),
        ModelDownloadState::NotInstalled => "Not installed".to_owned(),
    }
}

/// egui 0.27 has no modal-window focus trap. This mirrors the established
/// Playground selector shield: it sits below the middle-layer window while
/// consuming pointer input intended for the background Models page.
fn model_dialog_interaction_shield(ctx: &egui::Context) {
    let screen_rect = ctx.screen_rect();
    egui::Area::new(egui::Id::new("models-dialog-interaction-shield"))
        .order(egui::Order::Background)
        .fixed_pos(screen_rect.min)
        .movable(false)
        .show(ctx, |ui| {
            let shield_rect = egui::Rect::from_min_size(egui::Pos2::ZERO, screen_rect.size());
            ui.allocate_rect(shield_rect, egui::Sense::click_and_drag());
            ui.painter().rect_filled(
                shield_rect,
                Rounding::ZERO,
                egui::Color32::from_black_alpha(72),
            );
        });
}

fn render_comparison_results(
    ui: &mut egui::Ui,
    models: &[ModelViewModel],
    comparison: &ModelComparisonState,
) {
    let colors = ui_palette(ui);
    let selected = models
        .iter()
        .filter(|model| comparison.selected_model_ids.contains(&model.id));
    let status = comparison_status(comparison);
    if let Some(status) = status.as_deref() {
        let response = ui.label(RichText::new(status).small().color(colors.muted_text));
        ui.ctx().accesskit_node_builder(response.id, |builder| {
            builder.set_live(egui::accesskit::Live::Polite);
            builder.set_live_atomic();
        });
    }

    if ui.available_width() < 720.0 {
        for model in selected {
            let result = comparison
                .results
                .iter()
                .find(|(id, _)| id == &model.id)
                .map(|(_, result)| result);
            let group = ui.group(|ui| {
                ui.label(RichText::new(&model.display_name).strong());
                ui.label(format!("Status: {}", comparison_result_status(result)));
                ui.label(format!("Duration: {}", comparison_duration(comparison)));
                ui.label(format!(
                    "Processing time: {}",
                    comparison_processing(result)
                ));
                ui.label(format!("Output: {}", comparison_output_summary(result)));
                ui.label(format!(
                    "Accuracy: {}",
                    comparison_accuracy(comparison, result)
                ));
                if let Some(rtf) = result.and_then(|result| result.realtime_factor) {
                    ui.label(RichText::new(format!("Real-time factor: {rtf:.2}x")).small());
                }
            });
            ui.ctx()
                .accesskit_node_builder(group.response.id, |builder| {
                    builder.set_role(egui::accesskit::Role::Group);
                    builder.set_name(format!("Comparison result for {}", model.display_name));
                });
        }
        return;
    }

    let table = ui.vertical(|ui| {
        ui.columns(5, |columns| {
            for (column, heading) in columns.iter_mut().zip([
                "Model",
                "Duration",
                "Processing time",
                "Output",
                "Accuracy",
            ]) {
                let response = column.label(RichText::new(heading).strong().small());
                column.ctx().accesskit_node_builder(response.id, |builder| {
                    builder.set_role(egui::accesskit::Role::ColumnHeader);
                    builder.set_name(heading);
                });
            }
        });
        for model in selected {
            let result = comparison
                .results
                .iter()
                .find(|(id, _)| id == &model.id)
                .map(|(_, result)| result);
            let row = ui.allocate_ui_with_layout(
                Vec2::new(ui.available_width(), 0.0),
                Layout::top_down(Align::LEFT),
                |ui| {
                    ui.columns(5, |columns| {
                        let cells = [
                            format!(
                                "{}\n{}",
                                model.display_name,
                                comparison_result_status(result)
                            ),
                            comparison_duration(comparison),
                            comparison_processing(result),
                            comparison_output_summary(result),
                            comparison_accuracy(comparison, result),
                        ];
                        for (column, cell) in columns.iter_mut().zip(cells) {
                            let response = column.label(cell);
                            column.ctx().accesskit_node_builder(response.id, |builder| {
                                builder.set_role(egui::accesskit::Role::Cell);
                            });
                        }
                    });
                },
            );
            ui.ctx().accesskit_node_builder(row.response.id, |builder| {
                builder.set_role(egui::accesskit::Role::Row);
                builder.set_name(format!("Comparison result for {}", model.display_name));
                if let Some(rtf) = result.and_then(|result| result.realtime_factor) {
                    builder.set_description(format!("Real-time factor: {rtf:.2}x"));
                }
            });
        }
    });
    ui.ctx()
        .accesskit_node_builder(table.response.id, |builder| {
            builder.set_role(egui::accesskit::Role::Table);
            builder.set_name("Model comparison results");
        });
}

fn comparison_status(comparison: &ModelComparisonState) -> Option<String> {
    match comparison.phase {
        ComparisonPhase::Recording => Some("Comparison recording in progress.".into()),
        ComparisonPhase::Processing => Some("Comparison processing in progress.".into()),
        ComparisonPhase::Complete => Some("Comparison results are ready.".into()),
        ComparisonPhase::Error => Some("Comparison finished with an error.".into()),
        ComparisonPhase::Idle if comparison.reference_transcript.is_some() => {
            Some("Reference transcript applied.".into())
        }
        ComparisonPhase::Idle => None,
    }
}

fn comparison_result_status(result: Option<&super::state::ComparisonResult>) -> &'static str {
    match result.map(|result| result.phase) {
        Some(ComparisonResultPhase::Pending) => "Queued",
        Some(ComparisonResultPhase::Processing) => "Processing",
        Some(ComparisonResultPhase::Complete) => "Complete",
        Some(ComparisonResultPhase::Error) => "Failed",
        None => "Not run",
    }
}

fn comparison_duration(comparison: &ModelComparisonState) -> String {
    comparison
        .audio_duration_ms
        .map_or("—".into(), |ms| format!("{:.1}s", ms as f32 / 1_000.0))
}

fn comparison_processing(result: Option<&super::state::ComparisonResult>) -> String {
    result
        .and_then(|result| result.processing_ms)
        .map_or("—".into(), |ms| format!("{:.1}s", ms as f32 / 1_000.0))
}

fn comparison_output_summary(result: Option<&super::state::ComparisonResult>) -> String {
    if let Some(error) = result.and_then(|result| result.error.as_deref()) {
        format!("Error: {error}")
    } else if let Some(output) = result.and_then(|result| result.output.as_deref()) {
        output.to_owned()
    } else {
        "No data".into()
    }
}

fn comparison_accuracy(
    comparison: &ModelComparisonState,
    result: Option<&super::state::ComparisonResult>,
) -> String {
    match (
        comparison.reference_transcript.as_deref(),
        result.and_then(|result| result.word_error_rate),
    ) {
        (Some(reference), Some(rate)) if !reference.trim().is_empty() => {
            format!("{:.0}% accuracy", ((1.0 - rate).clamp(0.0, 1.0)) * 100.0)
        }
        _ => "Add a reference transcript to measure".into(),
    }
}

fn model_install_action_label(state: ModelDownloadState) -> &'static str {
    match state {
        ModelDownloadState::Cancelled => "Resume",
        ModelDownloadState::Failed => "Retry",
        _ => "Install",
    }
}

fn comparison_start_disabled_reason(comparison: &ModelComparisonState) -> Option<&str> {
    if let Some(reason) = comparison.start_disabled_reason.as_deref() {
        Some(reason)
    } else if matches!(
        comparison.phase,
        super::state::ComparisonPhase::Recording | super::state::ComparisonPhase::Processing
    ) {
        Some("Wait for the current comparison to finish before starting another.")
    } else if comparison.selected_model_ids.len() < 2 {
        Some("Select at least two installed models before starting a comparison.")
    } else if comparison.selected_model_ids.len() > 4 {
        Some("Select no more than four installed models for a comparison.")
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
    for (tab, tab_id) in tab_ids {
        if tab == active_tab {
            ui.ctx().accesskit_node_builder(tab_id, |builder| {
                builder.push_controlled(panel.response.id.value().into());
            });
        }
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
    let recording_locked = matches!(
        state.phase,
        TranscriptionPhase::Listening | TranscriptionPhase::Finalizing
    );
    card(ui, |ui| {
        ui.label(RichText::new("Recording behavior").strong());
        ui.add_space(12.0);
        if recording_locked {
            let notice = ui.label(
                RichText::new("Finish recording before changing recording settings.")
                    .color(colors.muted_text),
            );
            ui.ctx().accesskit_node_builder(notice.id, |builder| {
                builder.set_live(egui::accesskit::Live::Polite);
                builder.set_live_atomic();
            });
        }
        ui.add_enabled_ui(!recording_locked, |ui| {
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
                                memory
                                    .request_focus(ui.make_persistent_id(("recording-mode", next)))
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
            setting_row(ui, "Duration limit", |ui, label_id| {
                let mut duration = settings.duration_seconds;
                ComboBox::from_id_source("duration-limit")
                    .selected_text(&settings.duration_label)
                    .width(240.0)
                    .show_ui(ui, |ui| {
                        for seconds in [15, 30, 60, 120, 300, 600] {
                            ui.selectable_value(
                                &mut duration,
                                seconds,
                                format!("{seconds} seconds"),
                            );
                        }
                    })
                    .response
                    .labelled_by(label_id);
                if duration != settings.duration_seconds {
                    *action = ScreenAction::SetDurationSeconds(duration);
                }
            });
            setting_row(ui, "Visual feedback", |ui, _| {
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
            ui.separator();
            let mut vad_enabled = settings.vad_enabled;
            if ui
                .checkbox(&mut vad_enabled, "Stop after speech ends in Toggle mode")
                .changed()
            {
                *action = ScreenAction::SetVadEnabled(vad_enabled);
            }
            if vad_enabled {
                for (label, value, action_for) in [
                    ("Speech confirmation ms", settings.speech_confirmation_ms, 0),
                    ("Internal pause ms", settings.internal_pause_ms, 1),
                    ("End after silence ms", settings.endpoint_silence_ms, 2),
                    ("Pre-roll ms", settings.pre_roll_ms, 3),
                    ("Post-roll ms", settings.post_roll_ms, 4),
                ] {
                    ui.horizontal(|ui| {
                        let label_response = ui.add_sized(
                            [270.0, 40.0],
                            egui::Label::new(RichText::new(label).color(ui_palette(ui).muted_text)),
                        );
                        let mut edited = value as i64;
                        if ui
                            .add(egui::DragValue::new(&mut edited).clamp_range(0..=5_000))
                            .labelled_by(label_response.id)
                            .changed()
                        {
                            *action = match action_for {
                                0 => ScreenAction::SetSpeechConfirmationMs(edited.max(50) as u32),
                                1 => ScreenAction::SetInternalPauseMs(edited.max(100) as u32),
                                2 => ScreenAction::SetEndpointSilenceMs(edited as u32),
                                3 => ScreenAction::SetPreRollMs(edited as u32),
                                _ => ScreenAction::SetPostRollMs(edited as u32),
                            };
                        }
                    });
                    ui.add_space(8.0);
                    ui.separator();
                    ui.add_space(8.0);
                }
            }
        });
    });
    ui.add_space(16.0);
    card(ui, |ui| {
        let audio_heading = ui.label(RichText::new("Audio input").strong());
        ui.ctx()
            .accesskit_node_builder(audio_heading.id, |builder| {
                builder.set_role(egui::accesskit::Role::Heading);
            });
        ui.add_space(12.0);
        ui.add_enabled_ui(!recording_locked, |ui| {
            setting_row(ui, "Device", |ui, label_id| {
                let mut selected = settings.selected_audio_device.clone();
                ComboBox::from_id_source("audio-device")
                    .selected_text(&settings.device_label)
                    .width(360.0)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut selected, None, "OS default");
                        for device in &settings.audio_devices {
                            ui.selectable_value(&mut selected, Some(device.clone()), device);
                        }
                    })
                    .response
                    .labelled_by(label_id);
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
            setting_row(ui, "Input sensitivity", |ui, label_id| {
                let mut percent = settings.input_sensitivity_percent;
                let sensitivity = ui
                    .add_sized(
                        [320.0, 40.0],
                        egui::Slider::new(&mut percent, 0..=100)
                            .show_value(false),
                    )
                    .labelled_by(label_id);
                ui.ctx().accesskit_node_builder(sensitivity.id, |builder| {
                    builder.set_name("Input sensitivity");
                    builder.set_description(
                        "Minimum microphone level treated as speech. Use Left and Right arrow keys to adjust.",
                    );
                });
                if sensitivity.changed() {
                    *action = ScreenAction::SetInputSensitivity(percent);
                }
            });
        });
    });
    ui.add_space(16.0);
    card(ui, |ui| {
        ui.label(RichText::new("Shortcut").strong());
        ui.add_space(12.0);
        ui.add_enabled_ui(!recording_locked, |ui| {
            setting_row(ui, "Global record hotkey", |ui, _| {
                for key in state
                    .hotkey
                    .split('+')
                    .map(str::trim)
                    .filter(|key| !key.is_empty())
                {
                    keycap(ui, key);
                }
                let capture_name = if settings.hotkey_capture_active {
                    "Cancel hotkey capture"
                } else {
                    "Change shortcut"
                };
                let capture = button(ui, capture_name, ButtonTone::Secondary);
                ui.ctx().accesskit_node_builder(capture.id, |builder| {
                    builder.set_name(capture_name);
                    builder.set_selected(settings.hotkey_capture_active);
                });
                if capture.clicked() {
                    *action = ScreenAction::ChangeShortcut;
                }
            });
            if let Some(status) = &settings.hotkey_capture_status {
                let response = ui.label(status);
                ui.ctx().accesskit_node_builder(response.id, |builder| {
                    builder.set_live(egui::accesskit::Live::Polite);
                    builder.set_live_atomic();
                });
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
    ui.add_space(16.0);
    card(ui, |ui| {
        ui.label(RichText::new("Active model").strong());
        ui.horizontal(|ui| {
            ui.label(&settings.active_model_label);
            if button(ui, "Manage models", ButtonTone::Secondary).clicked() {
                *action = ScreenAction::OpenModelSettings;
            }
        });
    });
    ui.add_space(16.0);
    card(ui, |ui| {
        ui.label(RichText::new("Shortcuts").strong());
        let mut hotkey = settings.hotkey_input.clone();
        ui.horizontal(|ui| {
            let label = ui.label("Record toggle");
            ui.add(egui::TextEdit::singleline(&mut hotkey).desired_width(240.0))
                .labelled_by(label.id);
            if button(ui, "Apply", ButtonTone::Secondary).clicked() {
                *action = ScreenAction::ApplyHotkey;
            } else if hotkey != settings.hotkey_input {
                *action = ScreenAction::SetHotkeyInput(hotkey);
            }
            let capture_name = if settings.hotkey_capture_active {
                "Cancel hotkey capture"
            } else {
                "Capture hotkey"
            };
            let capture = button(ui, capture_name, ButtonTone::Secondary);
            ui.ctx().accesskit_node_builder(capture.id, |builder| {
                builder.set_name(capture_name);
                builder.set_selected(settings.hotkey_capture_active);
            });
            if capture.clicked() {
                *action = ScreenAction::ChangeShortcut;
            }
        });
        if let Some(status) = &settings.hotkey_capture_status {
            let response = ui.label(status);
            ui.ctx().accesskit_node_builder(response.id, |builder| {
                builder.set_live(egui::accesskit::Live::Polite);
                builder.set_live_atomic();
            });
        }
    });
    ui.add_space(16.0);
    card(ui, |ui| {
        ui.label(RichText::new("Appearance").strong());
        let mut theme = settings.theme_label.clone();
        setting_row(ui, "Theme", |ui, label_id| {
            ComboBox::from_id_source("theme-mode")
                .selected_text(&theme)
                .show_ui(ui, |ui| {
                    for value in ["Light", "Dark", "System"] {
                        ui.selectable_value(&mut theme, value.to_owned(), value);
                    }
                })
                .response
                .labelled_by(label_id);
        });
        if theme != settings.theme_label {
            *action = ScreenAction::SetTheme(theme);
        }
        let mut overlay = settings.overlay_label.clone();
        setting_row(ui, "Dictation overlay", |ui, label_id| {
            ui.add_enabled_ui(settings.overlay_available, |ui| {
                ComboBox::from_id_source("overlay-mode")
                    .selected_text(&overlay)
                    .show_ui(ui, |ui| {
                        for value in ["Live", "Minimal", "Off"] {
                            ui.selectable_value(&mut overlay, value.to_owned(), value);
                        }
                    })
                    .response
                    .labelled_by(label_id);
            });
        });
        if overlay != settings.overlay_label {
            *action = ScreenAction::SetOverlayMode(overlay);
        }
        if !settings.overlay_available {
            ui.label(RichText::new("The overlay is unavailable because focus safety is not verified on this platform.").color(ui_palette(ui).warning));
        }
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
            .checkbox(&mut auto_insert, &settings.output_label)
            .changed()
        {
            *action = ScreenAction::SetAutoInsertTranscript(auto_insert);
        }
        ui.add_enabled_ui(auto_insert, |ui| {
            if settings.show_restore_clipboard {
                let mut restore = settings.restore_clipboard_after_insert;
                if ui
                    .checkbox(&mut restore, "Restore clipboard after insert")
                    .changed()
                {
                    *action = ScreenAction::SetRestoreClipboardAfterInsert(restore);
                }
                ui.horizontal(|ui| {
                    let label = ui.label("Paste delay ms");
                    let mut delay = settings.paste_delay_ms as i64;
                    if ui
                        .add(egui::DragValue::new(&mut delay).clamp_range(1..=1_000))
                        .labelled_by(label.id)
                        .changed()
                    {
                        *action = ScreenAction::SetPasteDelayMs(delay as u64);
                    }
                });
            } else if let Some(notice) = &settings.output_notice {
                ui.label(RichText::new(notice).color(ui_palette(ui).muted_text));
            }
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
    ui.add_space(16.0);
    card(ui, |ui| {
        ui.label(RichText::new("Live transcription").strong());
        let mut streaming = settings.streaming_label.clone();
        setting_row(ui, "Mode", |ui, label_id| {
            ComboBox::from_id_source("streaming-mode")
                .selected_text(&streaming)
                .show_ui(ui, |ui| {
                    for value in ["Auto", "Rolling preview", "Final text only"] {
                        ui.selectable_value(&mut streaming, value.to_owned(), value);
                    }
                })
                .response
                .labelled_by(label_id);
        });
        if streaming != settings.streaming_label {
            *action = ScreenAction::SetStreamingMode(streaming);
        }
    });
    ui.add_space(16.0);
    card(ui, |ui| {
        ui.label(RichText::new("Performance").strong());
        let mut acceleration = settings.acceleration_label.clone();
        setting_row(ui, "Transcription device", |ui, label_id| {
            ComboBox::from_id_source("advanced-transcription-device-mode")
                .selected_text(&acceleration)
                .show_ui(ui, |ui| {
                    for value in ["Auto", "GPU", "CPU only"] {
                        ui.add_enabled_ui(value != "GPU" || settings.gpu_available, |ui| {
                            ui.selectable_value(&mut acceleration, value.to_owned(), value);
                        });
                    }
                })
                .response
                .labelled_by(label_id);
        });
        if acceleration != settings.acceleration_label {
            *action = ScreenAction::SetAcceleration(acceleration);
        }
    });
    ui.add_space(16.0);
    card(ui, |ui| {
        ui.label(RichText::new("Overlay").strong());
        let mut position = settings.overlay_position_label.clone();
        setting_row(ui, "Overlay position", |ui, label_id| {
            ComboBox::from_id_source("overlay-position")
                .selected_text(&position)
                .show_ui(ui, |ui| {
                    for value in ["Top", "Bottom"] {
                        ui.selectable_value(&mut position, value.to_owned(), value);
                    }
                })
                .response
                .labelled_by(label_id);
        });
        if position != settings.overlay_position_label {
            *action = ScreenAction::SetOverlayPosition(position);
        }
    });
    ui.add_space(16.0);
    card(ui, |ui| {
        ui.label(RichText::new("History and privacy").strong());
        if settings.history_locked {
            let notice = ui.label(
                RichText::new(
                    "History retention is locked while a retained-audio retry owns its row.",
                )
                .color(ui_palette(ui).warning),
            );
            ui.ctx().accesskit_node_builder(notice.id, |builder| {
                builder.set_live(egui::accesskit::Live::Polite);
                builder.set_live_atomic();
            });
        }
        ui.add_enabled_ui(!settings.history_locked, |ui| {
            let mut mode = settings.history_mode_label.clone();
            setting_row(ui, "History storage", |ui, label_id| { let response = ComboBox::from_id_source("history-storage-mode").selected_text(&mode).show_ui(ui, |ui| { for value in ["Off", "Transcript only", "Transcript and audio"] { ui.selectable_value(&mut mode, value.to_owned(), value); } }).response.labelled_by(label_id); describe_history_lock(ui, &response, settings.history_locked); });
            if mode != settings.history_mode_label { *action = ScreenAction::SetHistoryMode(mode.clone()); }
            if mode != "Off" {
                let mut maximum = settings.max_history_entries as i64;
                setting_row(ui, "Maximum unpinned entries", |ui, label_id| { let response = ui.add(egui::DragValue::new(&mut maximum).clamp_range(1..=1_000)).labelled_by(label_id); describe_history_lock(ui, &response, settings.history_locked); if response.changed() { *action = ScreenAction::SetMaxHistoryEntries(maximum as u32); } });
                optional_retention_control(ui, "Transcript age limit", "Keep transcripts until deleted", settings.transcript_retention_days, settings.history_locked, action, ScreenAction::SetTranscriptRetentionDays);
                if mode == "Transcript and audio" {
                    optional_retention_control(ui, "Audio age limit", "Keep retained audio until its entry is deleted", settings.audio_retention_days, settings.history_locked, action, ScreenAction::SetAudioRetentionDays);
                }
                let mut identity = settings.store_application_identity;
                let identity_control = ui.checkbox(&mut identity, "Store coarse application identity with new entries").on_hover_text("Stores only a coarse local application label, never a window title or document name.");
                describe_history_lock(ui, &identity_control, settings.history_locked);
                if identity_control.changed() { *action = ScreenAction::SetStoreApplicationIdentity(identity); }
            }
        });
    });
    ui.add_space(16.0);
    card(ui, |ui| {
        ui.label(RichText::new("Developer").strong());
        let mut enabled = settings.debug_mode;
        if ui
            .checkbox(&mut enabled, "Enable local model Playground")
            .changed()
        {
            *action = ScreenAction::SetDebugMode(enabled);
        }
    });
    if !settings.diagnostics.is_empty() {
        ui.add_space(16.0);
        card(ui, |ui| {
            ui.label(RichText::new("Diagnostics").strong());
            for line in &settings.diagnostics {
                ui.label(line);
            }
        });
    }
}

fn optional_retention_control(
    ui: &mut egui::Ui,
    label: &str,
    unlimited_label: &str,
    configured_days: Option<u32>,
    history_locked: bool,
    action: &mut ScreenAction,
    update: impl FnOnce(Option<u32>) -> ScreenAction + Copy,
) {
    let mut limited = configured_days.is_some();
    let limit = ui.checkbox(&mut limited, label);
    describe_history_lock(ui, &limit, history_locked);
    if limit.changed() {
        *action = update(limited.then_some(configured_days.unwrap_or(30)));
    }
    if limited {
        let mut days = configured_days.unwrap_or(30) as i64;
        setting_row(ui, "Days", |ui, label_id| {
            let response = ui
                .add(egui::DragValue::new(&mut days).clamp_range(1..=3_650))
                .labelled_by(label_id);
            describe_history_lock(ui, &response, history_locked);
            if response.changed() {
                *action = update(Some(days as u32));
            }
        });
    } else {
        ui.label(RichText::new(unlimited_label).color(ui_palette(ui).muted_text));
    }
}

fn describe_history_lock(ui: &egui::Ui, response: &egui::Response, locked: bool) {
    if locked {
        ui.ctx().accesskit_node_builder(response.id, |builder| {
            builder
                .set_description("Unavailable while a retained-audio retry owns its history row.");
        });
    }
}

fn setting_row(ui: &mut egui::Ui, label: &str, contents: impl FnOnce(&mut egui::Ui, egui::Id)) {
    ui.horizontal(|ui| {
        let label = ui.add_sized(
            [270.0, 40.0],
            egui::Label::new(RichText::new(label).color(ui_palette(ui).muted_text)),
        );
        contents(ui, label.id);
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
    fn comparison_surface_uses_all_available_width_at_preferred_and_compact_sizes() {
        for available_width in [860.0, 640.0] {
            assert_eq!(comparison_surface_width(available_width), available_width);
            assert_eq!(
                comparison_content_min_width(available_width, 16.0) + 32.0,
                available_width
            );
        }
    }

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
                        model_catalog: &[],
                        comparison: &comparison,
                        model_management: &Default::default(),
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
                        model_catalog: &[],
                        comparison: &comparison,
                        model_management: &Default::default(),
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
    fn busy_and_model_setup_phases_never_offer_start_or_clear() {
        for (phase, expected_status) in [
            (
                TranscriptionPhase::RequestingMicrophone,
                "Requesting microphone access…",
            ),
            (TranscriptionPhase::ModelLoading, "Loading speech model…"),
            (TranscriptionPhase::ModelError, "Speech model unavailable"),
        ] {
            let state = TranscriptionState {
                phase,
                selected_model_id: Some("base.en".into()),
                committed_transcript: "Keep this text.".into(),
                ..Default::default()
            };
            let output = render_transcribe(&state, &[]);
            let nodes = &output.platform_output.accesskit_update.unwrap().nodes;
            assert!(
                nodes
                    .iter()
                    .any(|(_, node)| node.name() == Some(expected_status))
            );
            assert!(
                !nodes
                    .iter()
                    .any(|(_, node)| node.name() == Some("Start recording"))
            );
        }

        for phase in [
            TranscriptionPhase::Listening,
            TranscriptionPhase::Finalizing,
        ] {
            let state = TranscriptionState {
                phase,
                selected_model_id: Some("base.en".into()),
                committed_transcript: "Keep this text.".into(),
                ..Default::default()
            };
            let output = render_transcribe(&state, &[]);
            let nodes = &output.platform_output.accesskit_update.unwrap().nodes;
            assert!(nodes.iter().any(|(_, node)| {
                node.name() == Some("Clear")
                    && node.description()
                        == Some(
                            "Clear is unavailable while recording or finalizing the current transcript.",
                        )
            }));
        }
    }

    #[test]
    fn microphone_error_uses_canonical_primary_copy_and_secondary_detail() {
        let state = TranscriptionState {
            phase: TranscriptionPhase::MicrophoneError,
            selected_model_id: Some("base.en".into()),
            notice: Some("Microphone failed: device disconnected".into()),
            ..Default::default()
        };
        let output = render_transcribe(&state, &[]);
        let nodes = &output.platform_output.accesskit_update.unwrap().nodes;
        assert!(
            nodes.iter().any(|(_, node)| {
                node.name() == Some("Scribe couldn’t access your microphone.")
            })
        );
        assert!(
            nodes
                .iter()
                .any(|(_, node)| { node.name() == Some("Microphone failed: device disconnected") })
        );
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
                        model_catalog: &[],
                        comparison: &comparison,
                        model_management: &Default::default(),
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
                        model_catalog: &[],
                        comparison: &comparison,
                        model_management: &Default::default(),
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
        assert!(
            update
                .nodes
                .iter()
                .any(|(_, node)| node.name() == Some("Reference transcript"))
        );
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

        comparison.phase = super::super::state::ComparisonPhase::Complete;
        comparison.selected_model_ids.extend(
            ["small.en", "medium.en", "large.en"]
                .into_iter()
                .map(str::to_owned),
        );
        assert_eq!(
            comparison_start_disabled_reason(&comparison),
            Some("Select no more than four installed models for a comparison.")
        );
    }

    #[test]
    fn add_model_dialog_uses_resumable_install_labels_accessibly() {
        for (state, label) in [
            (ModelDownloadState::Cancelled, "Resume"),
            (ModelDownloadState::Failed, "Retry"),
            (ModelDownloadState::NotInstalled, "Install"),
        ] {
            let ctx = egui::Context::default();
            ctx.enable_accesskit();
            let model = ModelViewModel {
                id: "base.en".into(),
                display_name: "base.en".into(),
                install_supported: true,
                install_action_enabled: true,
                download_state: state,
                ..Default::default()
            };
            let output = ctx.run(Default::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    render_screen(
                        ui,
                        &ScreenView {
                            route: UiRoute::Models,
                            transcription: &Default::default(),
                            models: &[],
                            model_catalog: &[model],
                            comparison: &Default::default(),
                            model_management: &ModelManagementState {
                                dialog: Some(ModelDialog::Add),
                                ..Default::default()
                            },
                            recording_settings: &Default::default(),
                        },
                    )
                });
            });
            assert!(
                output
                    .platform_output
                    .accesskit_update
                    .unwrap()
                    .nodes
                    .iter()
                    .any(|(_, node)| node.role() == egui::accesskit::Role::Button
                        && node.name() == Some(label))
            );
        }
    }

    #[test]
    fn model_dialogs_expose_progress_values_and_disclosure_state() {
        let downloading = ModelViewModel {
            id: "base.en".into(),
            display_name: "whisper.cpp base.en".into(),
            install_supported: true,
            install_action_enabled: false,
            download_state: ModelDownloadState::Downloading,
            downloaded_bytes: 50,
            total_bytes: Some(100),
            ..Default::default()
        };
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let output = ctx.run(Default::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                render_screen(
                    ui,
                    &ScreenView {
                        route: UiRoute::Models,
                        transcription: &Default::default(),
                        models: &[],
                        model_catalog: &[downloading],
                        comparison: &Default::default(),
                        model_management: &ModelManagementState {
                            dialog: Some(ModelDialog::Add),
                            ..Default::default()
                        },
                        recording_settings: &Default::default(),
                    },
                );
            });
        });
        let nodes = &output.platform_output.accesskit_update.unwrap().nodes;
        assert!(nodes.iter().any(|(_, node)| {
            node.role() == egui::accesskit::Role::Dialog && node.name() == Some("Add models")
        }));
        assert!(nodes.iter().any(|(_, node)| {
            node.role() == egui::accesskit::Role::ProgressIndicator
                && node.name() == Some("whisper.cpp base.en installation progress")
                && node.numeric_value() == Some(50.0)
                && node.min_numeric_value() == Some(0.0)
                && node.max_numeric_value() == Some(100.0)
        }));

        let details = ModelViewModel {
            id: "base.en".into(),
            display_name: "whisper.cpp base.en".into(),
            ..Default::default()
        };
        let output = ctx.run(Default::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                render_screen(
                    ui,
                    &ScreenView {
                        route: UiRoute::Models,
                        transcription: &Default::default(),
                        models: &[],
                        model_catalog: &[details],
                        comparison: &Default::default(),
                        model_management: &ModelManagementState {
                            dialog: Some(ModelDialog::Details("base.en".into())),
                            ..Default::default()
                        },
                        recording_settings: &Default::default(),
                    },
                );
            });
        });
        assert!(
            output
                .platform_output
                .accesskit_update
                .unwrap()
                .nodes
                .iter()
                .any(|(_, node)| {
                    node.name() == Some("Runtime maintenance") && node.is_expanded() == Some(false)
                })
        );
    }

    #[test]
    fn model_row_actions_are_contextual_and_disabled_remove_is_explained() {
        let model = ModelViewModel {
            id: "base.en".into(),
            display_name: "whisper.cpp base.en".into(),
            active: true,
            ..Default::default()
        };
        let comparison = ModelComparisonState {
            expanded: true,
            ..Default::default()
        };
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let output = ctx.run(Default::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                render_screen(
                    ui,
                    &ScreenView {
                        route: UiRoute::Models,
                        transcription: &Default::default(),
                        models: &[model],
                        model_catalog: &[],
                        comparison: &comparison,
                        model_management: &Default::default(),
                        recording_settings: &Default::default(),
                    },
                );
            });
        });
        let nodes = &output.platform_output.accesskit_update.unwrap().nodes;
        assert!(nodes.iter().any(|(_, node)| {
            node.role() == egui::accesskit::Role::Button
                && node.name() == Some("Details for whisper.cpp base.en")
        }));
        assert!(nodes.iter().any(|(_, node)| {
            node.role() == egui::accesskit::Role::Button
                && node.name() == Some("Remove whisper.cpp base.en")
                && node.description()
                    == Some("Select another ready model before removing the active model.")
        }));
        assert!(nodes.iter().any(|(_, node)| {
            node.name() == Some("Collapse comparison") && node.is_expanded() == Some(true)
        }));
    }

    #[test]
    fn comparison_empty_cells_render_an_em_dash() {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let comparison = ModelComparisonState {
            expanded: true,
            selected_model_ids: ["base.en".to_owned()].into_iter().collect(),
            ..Default::default()
        };
        let model = ModelViewModel {
            id: "base.en".into(),
            display_name: "base.en".into(),
            installed: true,
            ready: true,
            ..Default::default()
        };
        let output = ctx.run(Default::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                render_screen(
                    ui,
                    &ScreenView {
                        route: UiRoute::Models,
                        transcription: &Default::default(),
                        models: &[model],
                        model_catalog: &[],
                        comparison: &comparison,
                        model_management: &Default::default(),
                        recording_settings: &Default::default(),
                    },
                )
            });
        });
        let dashes = output
            .platform_output
            .accesskit_update
            .unwrap()
            .nodes
            .iter()
            .filter(|(_, node)| node.name() == Some("—"))
            .count();
        assert!(dashes >= 2);
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
    fn recording_controls_explain_why_changes_are_disabled_while_busy() {
        let state = TranscriptionState {
            phase: TranscriptionPhase::Listening,
            ..Default::default()
        };
        let settings_view = RecordingSettingsView::default();
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let output = ctx.run(Default::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let _ = settings(ui, SettingsTab::Recording, &state, &settings_view);
            });
        });
        assert!(
            output
                .platform_output
                .accesskit_update
                .unwrap()
                .nodes
                .iter()
                .any(|(_, node)| {
                    node.name() == Some("Finish recording before changing recording settings.")
                })
        );
    }

    #[test]
    fn advanced_tab_exposes_history_privacy_and_developer_controls() {
        let settings_view = RecordingSettingsView {
            history_mode_label: "Transcript and audio".into(),
            max_history_entries: 250,
            transcript_retention_days: Some(90),
            audio_retention_days: Some(30),
            diagnostics: vec!["Tray integration is unavailable in this desktop session.".into()],
            ..Default::default()
        };
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let output = ctx.run(Default::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let _ = settings(
                    ui,
                    SettingsTab::Advanced,
                    &TranscriptionState::default(),
                    &settings_view,
                );
            });
        });
        let nodes = &output.platform_output.accesskit_update.unwrap().nodes;
        for name in [
            "History and privacy",
            "History storage",
            "Maximum unpinned entries",
            "Transcript age limit",
            "Audio age limit",
            "Store coarse application identity with new entries",
            "Enable local model Playground",
            "Diagnostics",
        ] {
            assert!(
                nodes.iter().any(|(_, node)| node.name() == Some(name)),
                "missing {name}"
            );
        }
    }

    #[test]
    fn output_controls_use_platform_contract_and_bounded_delay() {
        let mut settings_view = RecordingSettingsView {
            auto_insert_transcript: true,
            output_label: "Copy final transcript to clipboard automatically".into(),
            show_restore_clipboard: false,
            output_notice: Some("Clipboard-only fallback".into()),
            ..Default::default()
        };
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let output = ctx.run(Default::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let _ = settings(
                    ui,
                    SettingsTab::Output,
                    &TranscriptionState::default(),
                    &settings_view,
                );
            });
        });
        let nodes = &output.platform_output.accesskit_update.unwrap().nodes;
        assert!(nodes.iter().any(
            |(_, node)| node.name() == Some("Copy final transcript to clipboard automatically")
        ));
        assert!(
            nodes
                .iter()
                .any(|(_, node)| node.name() == Some("Clipboard-only fallback"))
        );
        settings_view.paste_delay_ms = 1_000;
        assert_eq!(settings_view.paste_delay_ms, 1_000);
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
        assert_eq!(
            nodes
                .iter()
                .filter(|(_, node)| {
                    node.role() == egui::accesskit::Role::Tab && node.controls().contains(&panel.0)
                })
                .count(),
            1
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
    fn settings_inputs_are_labelled_by_their_visible_rows() {
        for tab in [
            SettingsTab::General,
            SettingsTab::Recording,
            SettingsTab::Output,
            SettingsTab::Advanced,
        ] {
            let output = render_route(UiRoute::Settings(tab));
            let nodes = &output.platform_output.accesskit_update.unwrap().nodes;
            for (_, node) in nodes.iter().filter(|(_, node)| {
                matches!(
                    node.role(),
                    egui::accesskit::Role::ComboBox
                        | egui::accesskit::Role::SpinButton
                        | egui::accesskit::Role::TextInput
                        | egui::accesskit::Role::ProgressIndicator
                )
            }) {
                assert!(
                    !node.labelled_by().is_empty() || node.name().is_some(),
                    "unlabelled {:?} on {tab:?}",
                    node.role()
                );
            }
        }
    }

    #[test]
    fn recording_settings_exposes_one_sensitivity_slider_without_live_input_meter() {
        use egui::accesskit::Role;

        let settings_view = RecordingSettingsView {
            input_sensitivity_percent: 42,
            ..Default::default()
        };
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let output = ctx.run(Default::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let _ = settings(
                    ui,
                    SettingsTab::Recording,
                    &TranscriptionState::default(),
                    &settings_view,
                );
            });
        });
        let nodes = &output.platform_output.accesskit_update.unwrap().nodes;
        let sliders = nodes
            .iter()
            .filter(|(_, node)| {
                node.role() == Role::Slider && node.name() == Some("Input sensitivity")
            })
            .collect::<Vec<_>>();
        assert_eq!(sliders.len(), 1);
        let slider = &sliders[0].1;
        assert_eq!(slider.min_numeric_value(), Some(0.0));
        assert_eq!(slider.max_numeric_value(), Some(100.0));
        assert_eq!(slider.numeric_value(), Some(42.0));
        assert!(nodes.iter().any(|(_, node)| {
            node.role() == Role::Heading && node.name() == Some("Audio input")
        }));
        assert!(
            !nodes
                .iter()
                .any(|(_, node)| node.role() == Role::ProgressIndicator)
        );
    }

    #[test]
    fn active_hotkey_capture_and_history_lock_are_announced() {
        let state = TranscriptionState::default();
        let settings_view = RecordingSettingsView {
            hotkey_capture_active: true,
            hotkey_capture_status: Some(
                "Press the new hotkey combination. Press Capture again to cancel.".into(),
            ),
            history_locked: true,
            history_mode_label: "Transcript and audio".into(),
            ..Default::default()
        };
        for tab in [SettingsTab::General, SettingsTab::Advanced] {
            let ctx = egui::Context::default();
            ctx.enable_accesskit();
            let output = ctx.run(Default::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let _ = settings(ui, tab, &state, &settings_view);
                });
            });
            let nodes = &output.platform_output.accesskit_update.unwrap().nodes;
            if tab == SettingsTab::General {
                assert!(nodes.iter().any(|(_, node)| {
                    node.name() == Some("Cancel hotkey capture") && node.is_selected() == Some(true)
                }));
            } else {
                assert!(nodes.iter().any(|(_, node)| {
                    node.name()
                        == Some(
                            "History retention is locked while a retained-audio retry owns its row.",
                        )
                        && node.live() == Some(egui::accesskit::Live::Polite)
                        && node.is_live_atomic()
                }));
                assert!(nodes.iter().any(|(_, node)| {
                    node.role() == egui::accesskit::Role::ComboBox
                        && node.description()
                            == Some(
                                "Unavailable while a retained-audio retry owns its history row.",
                            )
                }));
            }
        }
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
