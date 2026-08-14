//! Shared, backend-neutral egui screen renderers.

use std::{collections::HashSet, path::Path};

use eframe::egui::{
    self, Align, Align2, Color32, ComboBox, Frame, Layout, Margin, RichText, Rounding, ScrollArea,
    Sense, Stroke, Vec2,
};

use super::{
    about_page,
    controls::{
        ButtonTone, Icon, button, card, focus_tooltip, icon_glyph, keycap, paint_focus_ring,
    },
    state::{
        ComparisonPhase, ComparisonResultPhase, ModelCardKey, ModelComparisonState, ModelDialog,
        ModelDownloadState, ModelLanguageFilter, ModelManagementState, ModelSizeTier,
        ModelSpeedTier, ModelViewModel, RecordingMode, RemoteCatalogActionKind,
        RemoteCatalogActionView, RemoteCatalogEntryView, RemoteCatalogStatusKind,
        RemoteCatalogVariantView, RemoteCatalogView, SettingsSaveState, SettingsTab,
        TranscriptionPhase, TranscriptionState, UiRoute,
    },
    ui_palette,
};

const TRANSCRIPT_PANEL_PREFERRED_MIN_HEIGHT: f32 = 565.0;
const TRANSCRIPT_PANEL_MIN_HEIGHT: f32 = 272.0;
const MODEL_REQUIRED_CONTENT_HEIGHT: f32 = 176.0;
const COMPACT_SELECTOR_BREAKPOINT: f32 = 880.0;
const SELECTOR_CARD_VERTICAL_MARGIN: f32 = 0.0;
const SELECTOR_CONTROL_HEIGHT: f32 = 44.0;
const SELECTOR_VISUAL_HEIGHT: f32 = 36.0;
const SELECTOR_ACTION_WIDTH: f32 = 72.0;
// The selected-model card is deliberately only 36px tall, so a 32px action
// makes the two surfaces read as one oversized control. Keep the 44px target
// while reducing this particular button's painted height.
const SELECTOR_ACTION_VISUAL_HEIGHT: f32 = 28.0;
const TRANSCRIPT_FOOTER_INSET: f32 = 16.0;
const TRANSCRIPT_BODY_PADDING: f32 = 26.0;
const TRANSCRIPT_BODY_VERTICAL_PADDING: f32 = 24.0;
const TRANSCRIPT_STATUS_VERTICAL_PADDING: f32 = 13.0;
const TRANSCRIPT_STATUS_CONTENT_HEIGHT: f32 = 54.0;
const TRANSCRIPT_STATUS_SPINNER_SLOT: f32 = 44.0;
const TRANSCRIPT_STATUS_SPINNER_SIZE: f32 = 26.0;
const MICROPHONE_ACCESS_ERROR: &str = "Scribe couldn’t access your microphone.";

const ROUTE_TOP_INSET: f32 = 28.0;
const ROUTE_HORIZONTAL_INSET: f32 = 28.0;
const ROUTE_BOTTOM_INSET: f32 = 16.0;
const SETTINGS_COMPACT_BREAKPOINT: f32 = 620.0;
const SETTINGS_LABEL_COLUMN_WIDTH: f32 = 270.0;
const LIVE_TRANSCRIPTION_PREVIEW_SWITCH_ID: &str = "live-transcription-preview-switch";
const LIVE_TRANSCRIPTION_PREVIEW_DESCRIPTION: &str =
    "Temporary local live text is replaced by the final transcription.";
const CLOSE_TO_TRAY_SWITCH_ID: &str = "close-to-tray-switch";
const CLOSE_TO_TRAY_DESCRIPTION: &str =
    "When the system tray is available, closing the window hides Scribe instead of quitting.";
const AUTO_INSERT_TRANSCRIPT_SWITCH_ID: &str = "auto-insert-transcript-switch";
const RESTORE_CLIPBOARD_SWITCH_ID: &str = "restore-clipboard-after-insert-switch";
const RESTORE_CLIPBOARD_DESCRIPTION: &str =
    "Restore the clipboard contents that existed before Scribe inserted the transcript.";
const STOP_AFTER_SPEECH_SWITCH_ID: &str = "stop-after-speech-ends-switch";
const STOP_AFTER_SPEECH_DESCRIPTION: &str =
    "In Press once mode, stop recording after the configured silence. Hold mode is unaffected.";
const VOICE_DETECTION_LOCKED_DESCRIPTION: &str =
    "Finish recording before changing voice detection settings.";
const LIMIT_TRANSCRIPT_AGE_SWITCH_ID: &str = "limit-transcript-age-switch";
const LIMIT_TRANSCRIPT_AGE_DESCRIPTION: &str =
    "Delete unpinned transcripts after the configured number of days. Pinned entries are kept.";
const LIMIT_AUDIO_AGE_SWITCH_ID: &str = "limit-audio-age-switch";
const LIMIT_AUDIO_AGE_DESCRIPTION: &str =
    "Remove unpinned audio after the configured number of days. Pinned entries are kept.";
const STORE_APPLICATION_IDENTITY_SWITCH_ID: &str = "store-application-identity-switch";
const STORE_APPLICATION_IDENTITY_DESCRIPTION: &str =
    "Store only a coarse local app label with new entries, never a window title or document name.";
const ENABLE_MODEL_PLAYGROUND_SWITCH_ID: &str = "enable-model-playground-switch";
const ENABLE_MODEL_PLAYGROUND_DESCRIPTION: &str = "Enable the local model Playground for installed-model testing. Disabling it closes the Playground.";
const ROUTE_FOCUSED_CONTROL_SCROLL: &str = "route-focused-control-scroll";
#[cfg(test)]
const ROUTE_SCROLL_DIAGNOSTICS: &str = "route-scroll-diagnostics";
const COMPARISON_BODY_FOCUSED_CONTROL_SCROLL: &str = "comparison-body-focused-control-scroll";
#[cfg(test)]
const COMPARISON_BODY_SCROLL_DIAGNOSTICS: &str = "comparison-body-scroll-diagnostics";

pub(crate) fn scroll_focused_control_into_view(ui: &egui::Ui, response: &egui::Response) {
    let requested = response.has_focus()
        || ui.input(|input| {
            input.has_accesskit_action_request(response.id, egui::accesskit::Action::Focus)
        });
    if requested {
        ui.data_mut(|data| {
            data.insert_temp(
                egui::Id::new(ROUTE_FOCUSED_CONTROL_SCROLL),
                (response.id, response.rect),
            )
        });
        response.scroll_to_me(Some(Align::Center));
    }
}

fn scroll_focused_comparison_body_control(ui: &egui::Ui, response: &egui::Response) {
    if response.has_focus()
        || ui.input(|input| {
            input.has_accesskit_action_request(response.id, egui::accesskit::Action::Focus)
        })
    {
        ui.data_mut(|data| {
            data.insert_temp(
                egui::Id::new(COMPARISON_BODY_FOCUSED_CONTROL_SCROLL),
                (response.id, response.rect),
            )
        });
        response.scroll_to_me(Some(Align::Center));
    }
}

fn current_content_width(ui: &egui::Ui) -> f32 {
    let available = ui.available_rect_before_wrap();
    let clip = ui.clip_rect();
    let viewport = ui.ctx().screen_rect();
    (available.max.x.min(clip.max.x).min(viewport.max.x)
        - available.min.x.max(clip.min.x).max(viewport.min.x))
    .max(0.0)
}

fn transcript_panel_height(ui: &egui::Ui) -> f32 {
    let helper_height = ui.text_style_height(&egui::TextStyle::Body);
    let remaining_height = ui.clip_rect().max.y
        - ui.available_rect_before_wrap().min.y
        - ui.spacing().item_spacing.y
        - helper_height
        - 8.0;
    remaining_height.clamp(
        TRANSCRIPT_PANEL_MIN_HEIGHT,
        TRANSCRIPT_PANEL_PREFERRED_MIN_HEIGHT,
    )
}

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
    pub input_level_percent: u8,
    pub microphone_error: Option<String>,
    pub auto_insert_transcript: bool,
    pub show_restore_clipboard: bool,
    pub output_notice: Option<String>,
    pub restore_clipboard_after_insert: bool,
    pub paste_delay_ms: u64,
    pub active_model_label: String,
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
    pub focus_playground_open: bool,
    pub history_mode_label: String,
    pub history_locked: bool,
    pub max_history_entries: u32,
    pub transcript_retention_days: Option<u32>,
    pub audio_retention_days: Option<u32>,
    pub store_application_identity: bool,
    pub diagnostics: Vec<String>,
    pub about_model_directory: String,
    pub about_settings_path: Option<String>,
    pub can_export_diagnostics: bool,
    pub diagnostic_session_count: usize,
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
            input_level_percent: 0,
            microphone_error: None,
            auto_insert_transcript: false,
            show_restore_clipboard: cfg!(target_os = "windows"),
            output_notice: None,
            restore_clipboard_after_insert: true,
            paste_delay_ms: 60,
            active_model_label: "No model selected".into(),
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
            focus_playground_open: false,
            history_mode_label: "Off".into(),
            history_locked: false,
            max_history_entries: 100,
            transcript_retention_days: None,
            audio_retention_days: None,
            store_application_identity: false,
            diagnostics: Vec::new(),
            about_model_directory: "Unavailable".into(),
            about_settings_path: None,
            can_export_diagnostics: false,
            diagnostic_session_count: 0,
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
    pub model_language_filter: ModelLanguageFilter,
    pub remote_catalog: &'a RemoteCatalogView,
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
    ShowComparisonReferenceEditor,
    HideComparisonReferenceEditor,
    EditComparisonReference(String),
    ApplyComparisonReference,
    ClearComparisonReference,
    SelectModel(String),
    InstallModel(String),
    UpgradeModel(String),
    CancelModelInstall(String),
    DiscardModelPartial(String),
    RepairModelRuntime(String),
    MaintainModelRuntime(String),
    ToggleModelCardDetails(ModelCardKey),
    RequestModelRemoval(String),
    ConfirmModelRemoval(String),
    CloseModelDialog,
    AcknowledgeModelRemovalFocus,
    SetLocalGgufImportPath(String),
    ValidateAndImportLocalGguf,
    CancelLocalGgufImport,
    SetRemoteCatalogQuery(String),
    SetModelLanguageFilter(ModelLanguageFilter),
    ToggleInstalledModels,
    ToggleAvailableModels,
    RetryRemoteCatalog,
    InstallRemoteCatalogVariant {
        remote_model_id: String,
        variant_id: String,
    },
    CancelRemoteCatalogInstall(String),
    UseRemoteCatalogModel(String),
    RemoveRemoteCatalogModel(String),
    DiscardRemoteCatalogPartial {
        remote_model_id: String,
        variant_id: String,
    },
    SetSettingsTab(SettingsTab),
    SetCloseToTray(bool),
    OpenModelSettings,
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
    OpenDeveloperPlayground,
    ExportRedactedDiagnostics,
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
            view.model_language_filter,
            view.remote_catalog,
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

/// The central route owns scrolling so the vertical track stays at the edge of
/// the central viewport rather than the edge of an inset content column.
pub(crate) fn show_route_scroll<T>(
    ui: &mut egui::Ui,
    route: UiRoute,
    add_contents: impl FnOnce(&mut egui::Ui) -> T,
) -> T {
    let route_width = ui.available_width();
    let viewport_id = egui::Id::new(("route-viewport", route));
    ui.data_mut(|data| data.insert_temp(viewport_id, ui.max_rect()));
    let scroll = ScrollArea::vertical()
        .id_source(("route-scroll", route))
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.set_width(route_width);
            ui.add_space(ROUTE_TOP_INSET);
            let content = Frame::none()
                .inner_margin(Margin::symmetric(ROUTE_HORIZONTAL_INSET, 0.0))
                .show(ui, |ui| {
                    ui.set_width((route_width - ROUTE_HORIZONTAL_INSET * 2.0).max(0.0));
                    let content = add_contents(ui);
                    ui.add_space(ROUTE_BOTTOM_INSET);
                    content
                })
                .inner;
            if route != UiRoute::Models
                && let Some((id, rect)) = ui.data(|data| {
                    data.get_temp::<(egui::Id, egui::Rect)>(egui::Id::new(
                        ROUTE_FOCUSED_CONTROL_SCROLL,
                    ))
                })
                && ui.ctx().memory(|memory| memory.focused()) == Some(id)
            {
                ui.scroll_to_rect(rect, Some(Align::Center));
            }
            content
        });
    if route == UiRoute::Models {
        let focused_control_scroll = ui.data_mut(|data| {
            let key = egui::Id::new(ROUTE_FOCUSED_CONTROL_SCROLL);
            let value = data.get_temp::<(egui::Id, egui::Rect)>(key);
            data.remove::<(egui::Id, egui::Rect)>(key);
            value
        });
        if let Some((id, rect)) = focused_control_scroll
            && ui.ctx().memory(|memory| memory.focused()) == Some(id)
        {
            let mut state = scroll.state;
            let mut visible_rect = scroll.inner_rect;
            if let Some(dock_rect) = ui.data(|data| {
                data.get_temp::<egui::Rect>(egui::Id::new("models-comparison-dock-rect"))
            }) {
                visible_rect.max.y = visible_rect
                    .max
                    .y
                    .min(dock_rect.top() - MODEL_LIST_TO_DOCK_GAP);
            }
            if rect.top() < visible_rect.top() {
                state.offset.y -= visible_rect.top() - rect.top();
            } else if rect.bottom() > visible_rect.bottom() {
                state.offset.y += rect.bottom() - visible_rect.bottom();
            }
            state.offset.y = state.offset.y.clamp(
                0.0,
                (scroll.content_size.y - scroll.inner_rect.height()).max(0.0),
            );
            state.store(ui.ctx(), scroll.id);
        }
    }
    #[cfg(test)]
    ui.data_mut(|data| {
        data.insert_temp(
            egui::Id::new(ROUTE_SCROLL_DIAGNOSTICS),
            (
                scroll.id,
                scroll.state.offset,
                scroll.content_size,
                scroll.inner_rect,
            ),
        );
    });
    scroll.inner
}

pub(crate) fn screen_action_for_remote_catalog_action(
    action: &RemoteCatalogActionKind,
) -> ScreenAction {
    match action {
        RemoteCatalogActionKind::Install {
            remote_model_id,
            variant_id,
        } => ScreenAction::InstallRemoteCatalogVariant {
            remote_model_id: remote_model_id.clone(),
            variant_id: variant_id.clone(),
        },
        RemoteCatalogActionKind::Cancel { model_id } => {
            ScreenAction::CancelRemoteCatalogInstall(model_id.clone())
        }
        RemoteCatalogActionKind::Use { model_id } => {
            ScreenAction::UseRemoteCatalogModel(model_id.clone())
        }
        RemoteCatalogActionKind::Remove { model_id } => {
            ScreenAction::RemoveRemoteCatalogModel(model_id.clone())
        }
        RemoteCatalogActionKind::DiscardPartial {
            remote_model_id,
            variant_id,
        } => ScreenAction::DiscardRemoteCatalogPartial {
            remote_model_id: remote_model_id.clone(),
            variant_id: variant_id.clone(),
        },
    }
}

fn header(ui: &mut egui::Ui, title: &str, subtitle: &str) {
    let response = ui.label(RichText::new(title).size(30.0).strong());
    ui.ctx().accesskit_node_builder(response.id, |builder| {
        builder.set_role(egui::accesskit::Role::Heading);
        builder.set_bounds(accesskit_rect(response.rect));
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
    let available_width = current_content_width(ui);
    let hotkey_width = (available_width * 0.28).clamp(220.0, 280.0);
    let gap = ui.spacing().item_spacing.x;
    let model_width = available_width - hotkey_width - gap;
    let compact = available_width < COMPACT_SELECTOR_BREAKPOINT;
    ui.allocate_ui_with_layout(
        Vec2::new(available_width, 0.0),
        if compact {
            Layout::top_down(Align::LEFT)
        } else {
            Layout::left_to_right(Align::TOP)
        },
        |ui| {
            let model_width = if compact {
                available_width
            } else {
                model_width
            };
            let model_card_id = ui.make_persistent_id("selected-model-card");
            let card_height = SELECTOR_CONTROL_HEIGHT + SELECTOR_CARD_VERTICAL_MARGIN * 2.0;
            let (model_card_rect, _) =
                ui.allocate_exact_size(Vec2::new(model_width, card_height), egui::Sense::hover());
            let model_card_visual_rect = egui::Rect::from_center_size(
                model_card_rect.center(),
                Vec2::new(model_card_rect.width(), SELECTOR_VISUAL_HEIGHT),
            );
            let model_card_frame = Frame::none()
                .fill(ui_palette(ui).card_bg)
                .stroke(Stroke::new(1.0, ui_palette(ui).border))
                .rounding(Rounding::same(5.0));
            ui.painter()
                .add(model_card_frame.paint(model_card_visual_rect));
            let content_rect = egui::Rect::from_min_max(
                egui::pos2(
                    model_card_visual_rect.min.x + 16.0,
                    model_card_visual_rect.min.y + SELECTOR_CARD_VERTICAL_MARGIN,
                ),
                egui::pos2(
                    model_card_visual_rect.max.x - 16.0,
                    model_card_visual_rect.max.y - SELECTOR_CARD_VERTICAL_MARGIN,
                ),
            );
            let action_visual_slot = egui::Rect::from_min_max(
                egui::pos2(
                    (content_rect.max.x - SELECTOR_ACTION_WIDTH).max(content_rect.min.x),
                    content_rect.min.y,
                ),
                content_rect.max,
            );
            let action_rect = egui::Rect::from_min_max(
                egui::pos2(action_visual_slot.left(), model_card_rect.top()),
                egui::pos2(action_visual_slot.right(), model_card_rect.bottom()),
            );
            let label_rect = egui::Rect::from_min_max(
                content_rect.min,
                egui::pos2(action_visual_slot.min.x, content_rect.max.y),
            );
            let mut label_ui = ui.child_ui(label_rect, Layout::left_to_right(Align::Center));
            label_ui.label(
                RichText::new(icon_glyph(Icon::Cpu))
                    .size(20.0)
                    .color(ui_palette(&label_ui).muted_text),
            );
            label_ui.label(RichText::new(name).strong());
            let action_label = if no_model { "Select" } else { "Change" };
            let was_enabled = ui.is_enabled();
            ui.set_enabled(was_enabled && disabled_reason.is_none());
            let response = ui.interact(
                action_rect,
                ui.make_persistent_id("selected-model-action"),
                egui::Sense::click(),
            );
            ui.set_enabled(was_enabled);
            let colors = ui_palette(ui);
            let hovered = response.enabled() && response.hovered();
            let action_visual_rect = egui::Rect::from_center_size(
                action_rect.center(),
                Vec2::new(SELECTOR_ACTION_WIDTH, SELECTOR_ACTION_VISUAL_HEIGHT),
            );
            ui.painter().rect(
                action_visual_rect,
                Rounding::same(5.0),
                if hovered {
                    colors.panel_bg
                } else {
                    egui::Color32::TRANSPARENT
                },
                if hovered {
                    Stroke::new(1.0, colors.border)
                } else {
                    Stroke::NONE
                },
            );
            ui.painter().text(
                action_visual_rect.center(),
                Align2::CENTER_CENTER,
                action_label,
                egui::FontId::proportional(13.0),
                if response.enabled() {
                    colors.text
                } else {
                    colors.muted_text
                },
            );
            response
                .widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, action_label));
            ui.ctx().accesskit_node_builder(response.id, |builder| {
                builder.set_role(egui::accesskit::Role::Button);
                builder.set_name(action_label);
                builder.set_bounds(egui::accesskit::Rect {
                    x0: action_rect.min.x.into(),
                    y0: action_rect.min.y.into(),
                    x1: action_rect.max.x.into(),
                    y1: action_rect.max.y.into(),
                });
                if !response.enabled() {
                    builder.set_disabled();
                }
            });
            paint_focus_ring(ui, &response, Rounding::same(5.0));
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
            ui.ctx().accesskit_node_builder(model_card_id, |builder| {
                builder.set_role(egui::accesskit::Role::Group);
                builder.set_name("Selected model");
                builder.set_bounds(egui::accesskit::Rect {
                    x0: model_card_rect.min.x.into(),
                    y0: model_card_rect.min.y.into(),
                    x1: model_card_rect.max.x.into(),
                    y1: model_card_rect.max.y.into(),
                });
            });
            if compact {
                ui.add_space(ui.spacing().item_spacing.y);
            } else {
                ui.add_space(gap);
            }
            let hotkey_width = if compact {
                available_width
            } else {
                hotkey_width
            };
            let hotkey_card_id = ui.make_persistent_id("recording-hotkey-card");
            let (hotkey_card_rect, _) =
                ui.allocate_exact_size(Vec2::new(hotkey_width, card_height), egui::Sense::hover());
            let hotkey_card_visual_rect = egui::Rect::from_center_size(
                hotkey_card_rect.center(),
                Vec2::new(hotkey_card_rect.width(), SELECTOR_VISUAL_HEIGHT),
            );
            let hotkey_card_frame = Frame::none()
                .fill(if no_model {
                    ui_palette(ui).disabled_bg
                } else {
                    ui_palette(ui).card_bg
                })
                .stroke(Stroke::new(1.0, ui_palette(ui).border))
                .rounding(Rounding::same(5.0));
            ui.painter()
                .add(hotkey_card_frame.paint(hotkey_card_visual_rect));
            let hotkey_content_rect = egui::Rect::from_min_max(
                egui::pos2(
                    hotkey_card_visual_rect.min.x + 16.0,
                    hotkey_card_visual_rect.min.y + SELECTOR_CARD_VERTICAL_MARGIN,
                ),
                egui::pos2(
                    hotkey_card_visual_rect.max.x - 16.0,
                    hotkey_card_visual_rect.max.y - SELECTOR_CARD_VERTICAL_MARGIN,
                ),
            );
            let mut hotkey_content_ui =
                ui.child_ui(hotkey_content_rect, Layout::left_to_right(Align::Center));
            hotkey_content_ui.add_enabled_ui(!no_model, |ui| {
                ui.label(
                    RichText::new(icon_glyph(Icon::Keyboard))
                        .size(18.0)
                        .color(ui_palette(ui).muted_text),
                );
                ui.label("Hotkey:");
                let mut keys = state
                    .hotkey
                    .split('+')
                    .map(str::trim)
                    .filter(|key| !key.is_empty())
                    .peekable();
                while let Some(key) = keys.next() {
                    keycap(ui, key);
                    if keys.peek().is_some() {
                        ui.label(RichText::new("+").color(ui_palette(ui).muted_text));
                    }
                }
            });
            ui.ctx().accesskit_node_builder(hotkey_card_id, |builder| {
                builder.set_role(egui::accesskit::Role::Group);
                builder.set_name("Recording hotkey");
                builder.set_bounds(egui::accesskit::Rect {
                    x0: hotkey_card_rect.min.x.into(),
                    y0: hotkey_card_rect.min.y.into(),
                    x1: hotkey_card_rect.max.x.into(),
                    y1: hotkey_card_rect.max.y.into(),
                });
            });
        },
    );
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

fn status_spinner(ui: &mut egui::Ui, accessible_name: &str) {
    let (slot, response) = ui.allocate_exact_size(
        Vec2::splat(TRANSCRIPT_STATUS_SPINNER_SLOT),
        egui::Sense::hover(),
    );
    ui.ctx().accesskit_node_builder(response.id, |builder| {
        builder.set_role(egui::accesskit::Role::ProgressIndicator);
        builder.set_name(accessible_name);
    });
    ui.painter().text(
        slot.center(),
        Align2::CENTER_CENTER,
        egui_phosphor::regular::CIRCLE_NOTCH,
        egui::FontId::proportional(TRANSCRIPT_STATUS_SPINNER_SIZE),
        ui_palette(ui).muted_text,
    );
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
                ui.spacing_mut().interact_size.y = 0.0;
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
            status_spinner(ui, "Finalizing transcript progress");
            ui.vertical(|ui| {
                ui.spacing_mut().interact_size.y = 0.0;
                let status = ui.label(RichText::new("Finalizing transcript…").strong());
                ui.ctx().accesskit_node_builder(status.id, |builder| {
                    builder.set_live(egui::accesskit::Live::Polite);
                    builder.set_live_atomic();
                });
                ui.label("This may take a moment.");
            });
        }
        TranscriptionPhase::RequestingMicrophone => {
            status_spinner(ui, "Requesting microphone access progress");
            ui.vertical(|ui| {
                ui.spacing_mut().interact_size.y = 0.0;
                let status = ui.label(RichText::new("Requesting microphone access…").strong());
                ui.ctx().accesskit_node_builder(status.id, |builder| {
                    builder.set_live(egui::accesskit::Live::Polite);
                    builder.set_live_atomic();
                });
                ui.label("Recording will start after access is granted.");
            });
        }
        TranscriptionPhase::ModelLoading => {
            status_spinner(ui, "Loading speech model progress");
            ui.vertical(|ui| {
                ui.spacing_mut().interact_size.y = 0.0;
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
                ui.spacing_mut().interact_size.y = 0.0;
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
                ui.spacing_mut().interact_size.y = 0.0;
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

fn no_model_empty_state(ui: &mut egui::Ui, panel_height: f32) -> ScreenAction {
    let colors = ui_palette(ui);
    let mut action = ScreenAction::None;
    // Center the intrinsic control stack in the bounded transcript panel.
    ui.add_space(((panel_height - MODEL_REQUIRED_CONTENT_HEIGHT) / 2.0).max(24.0));
    let empty_state = ui.with_layout(Layout::top_down(Align::Center), |ui| {
        let (icon_rect, _) = ui.allocate_exact_size(Vec2::splat(68.0), egui::Sense::hover());
        ui.painter().rect(
            icon_rect,
            Rounding::same(8.0),
            colors.panel_bg,
            Stroke::NONE,
        );
        ui.painter().text(
            icon_rect.center(),
            Align2::CENTER_CENTER,
            icon_glyph(Icon::Models),
            egui::FontId::proportional(30.0),
            colors.muted_text,
        );
        ui.add_space(12.0);
        ui.label(
            RichText::new("Add a speech model to start transcribing")
                .size(18.0)
                .strong(),
        );
        ui.label("Your audio stays on this device.");
        ui.add_space(12.0);
        let add_model = button(ui, "Add model", ButtonTone::Primary);
        if add_model.clicked() {
            action = ScreenAction::AddModel;
        }
    });
    let empty_state_id = ui.make_persistent_id("model-required-empty-state");
    ui.ctx().accesskit_node_builder(empty_state_id, |builder| {
        builder.set_role(egui::accesskit::Role::Group);
        builder.set_name("Model required empty state");
        builder.set_bounds(egui::accesskit::Rect {
            x0: empty_state.response.rect.min.x.into(),
            y0: empty_state.response.rect.min.y.into(),
            x1: empty_state.response.rect.max.x.into(),
            y1: empty_state.response.rect.max.y.into(),
        });
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

fn transcript_frame(
    ui: &mut egui::Ui,
    state: &TranscriptionState,
    panel_height: f32,
) -> ScreenAction {
    let colors = ui_palette(ui);
    let mut action = ScreenAction::None;
    let width = current_content_width(ui);
    let transcript_panel_id = ui.make_persistent_id("transcript-panel");
    let transcript_panel = ui.allocate_ui_with_layout(
        Vec2::new(width, 0.0),
        Layout::top_down(Align::LEFT),
        |ui| {
            ui.set_width(width);
            Frame::none()
                .fill(colors.card_bg)
                .stroke(Stroke::new(1.0, colors.border))
                .rounding(Rounding::same(5.0))
                .show(ui, |ui| {
            ui.set_width(width);
            ui.set_min_height(panel_height);
            let panel_bottom = ui.min_rect().bottom();
            if state.phase != TranscriptionPhase::NoModel {
                let status_width = ui.available_width();
                let status_strip_id = ui.make_persistent_id("recording-status-strip");
                let status_strip = Frame::none()
                    .fill(colors.panel_bg)
                    .inner_margin(Margin::symmetric(
                        TRANSCRIPT_BODY_PADDING,
                        TRANSCRIPT_STATUS_VERTICAL_PADDING,
                    ))
                    .show(ui, |ui| {
                        let status_content_width =
                            (status_width - TRANSCRIPT_BODY_PADDING * 2.0).max(0.0);
                        ui.set_min_width(status_content_width);
                        let (status_content_rect, _) = ui.allocate_exact_size(
                            Vec2::new(status_content_width, TRANSCRIPT_STATUS_CONTENT_HEIGHT),
                            egui::Sense::hover(),
                        );
                        ui.allocate_ui_at_rect(status_content_rect, |ui| {
                            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                                action = recording_status_header(ui, state);
                            });
                        });
                    });
                ui.ctx().accesskit_node_builder(status_strip_id, |builder| {
                    builder.set_role(egui::accesskit::Role::Group);
                    builder.set_name("Recording status");
                    builder.set_bounds(egui::accesskit::Rect {
                        x0: status_strip.response.rect.min.x.into(),
                        y0: status_strip.response.rect.min.y.into(),
                        x1: status_strip.response.rect.max.x.into(),
                        y1: status_strip.response.rect.max.y.into(),
                    });
                });
                let separator_gap = ui.spacing().item_spacing.y;
                ui.add_space(-separator_gap);
                ui.separator();
                ui.add_space(-separator_gap);
            }

            let has_committed_transcript = !state.committed_transcript.trim().is_empty();
            if state.phase == TranscriptionPhase::NoModel && !has_committed_transcript {
                action = no_model_empty_state(ui, panel_height);
            } else {
                Frame::none()
                    .inner_margin(Margin::symmetric(
                        TRANSCRIPT_BODY_PADDING,
                        TRANSCRIPT_BODY_VERTICAL_PADDING,
                    ))
                    .show(ui, |ui| {
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
                            let response = if state.provisional_transcript.is_empty() {
                                ui.label(&state.committed_transcript)
                            } else {
                                let mut transcript = egui::text::LayoutJob::default();
                                let body_format = egui::TextFormat {
                                    font_id: egui::TextStyle::Body.resolve(ui.style()),
                                    color: colors.text,
                                    ..Default::default()
                                };
                                transcript.append(
                                    &state.committed_transcript,
                                    0.0,
                                    body_format.clone(),
                                );
                                transcript.append(" ", 0.0, body_format);
                                transcript.append(
                                    &state.provisional_transcript,
                                    0.0,
                                    egui::TextFormat {
                                        font_id: egui::TextStyle::Body.resolve(ui.style()),
                                        color: colors.tertiary_text,
                                        italics: true,
                                        ..Default::default()
                                    },
                                );
                                ui.label(transcript)
                            };
                            ui.ctx().accesskit_node_builder(response.id, |builder| {
                                builder.set_live(egui::accesskit::Live::Polite);
                                builder.set_live_atomic();
                            });
                        }
                        if state.committed_transcript.trim().is_empty()
                            && !state.provisional_transcript.is_empty()
                        {
                            ui.add_space(8.0);
                            ui.label(
                                RichText::new(&state.provisional_transcript)
                                    .italics()
                                    .color(colors.tertiary_text),
                            );
                        }
                        if state.last_successful_capture_ms.is_some()
                            || state.selected_model_id.is_some()
                        {
                            ui.add_space(10.0);
                            ui.horizontal(|ui| {
                                if let Some(capture_ms) = state.last_successful_capture_ms {
                                    transcript_metadata_chip(
                                        ui,
                                        &format_relative_capture_time(capture_ms),
                                    );
                                }
                                if let Some(model_id) = &state.selected_model_id {
                                    transcript_metadata_chip(ui, &model_id.to_ascii_uppercase());
                                }
                            });
                        }
                    });
                let footer_height = 40.0;
                let separator_footprint = ui.spacing().item_spacing.y * 2.0;
                let footer_top = (panel_bottom - TRANSCRIPT_FOOTER_INSET - footer_height)
                    - separator_footprint;
                let footer_top = footer_top.max(ui.cursor().top() + 24.0);
                ui.add_space((footer_top - ui.cursor().top()).max(0.0));
                ui.separator();
                ui.allocate_ui_with_layout(
                    Vec2::new((width - TRANSCRIPT_FOOTER_INSET).max(0.0), 0.0),
                    Layout::right_to_left(Align::Center),
                    |ui| {
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
                    },
                );
                }
            })
            .response
        },
    );
    ui.ctx()
        .accesskit_node_builder(transcript_panel_id, |builder| {
            builder.set_role(egui::accesskit::Role::Group);
            builder.set_name("Transcript panel");
            builder.set_bounds(egui::accesskit::Rect {
                x0: transcript_panel.inner.rect.min.x.into(),
                y0: transcript_panel.inner.rect.min.y.into(),
                x1: transcript_panel.inner.rect.max.x.into(),
                y1: transcript_panel.inner.rect.max.y.into(),
            });
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
                ui.set_min_width((width - 24.0).max(0.0));
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

fn transcript_metadata_chip(ui: &mut egui::Ui, text: &str) {
    let colors = ui_palette(ui);
    let galley = ui.painter().layout_no_wrap(
        text.to_owned(),
        egui::TextStyle::Small.resolve(ui.style()),
        colors.muted_text,
    );
    let (rect, response) = ui.allocate_exact_size(
        Vec2::new(galley.size().x + 14.0, 26.0),
        egui::Sense::hover(),
    );
    ui.painter().rect(
        rect,
        Rounding::same(3.0),
        colors.panel_bg,
        Stroke::new(1.0, colors.border),
    );
    ui.painter().galley(
        rect.center() - galley.size() * 0.5,
        galley,
        colors.muted_text,
    );
    ui.ctx().accesskit_node_builder(response.id, |builder| {
        builder.set_role(egui::accesskit::Role::StaticText);
        builder.set_name(text);
        builder.set_bounds(egui::accesskit::Rect {
            x0: rect.min.x.into(),
            y0: rect.min.y.into(),
            x1: rect.max.x.into(),
            y1: rect.max.y.into(),
        });
    });
}

fn format_relative_capture_time(capture_age_ms: u64) -> String {
    let minutes = capture_age_ms / 60_000;
    match minutes {
        0 => "JUST NOW".to_owned(),
        1 => "1 MIN AGO".to_owned(),
        minutes => format!("{minutes} MINS AGO"),
    }
}

fn is_repeated_microphone_error(detail: &str) -> bool {
    let normalized = detail
        .trim()
        .trim_end_matches(['.', '!', '…'])
        .trim()
        .replace('’', "'")
        .to_ascii_lowercase();
    normalized == "scribe couldn't access your microphone"
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
                    ui.set_min_width((width - 24.0).max(0.0));
                    let detail = technical_detail.trim();
                    let has_distinct_detail =
                        !detail.is_empty() && !is_repeated_microphone_error(detail);
                    let row_height = if has_distinct_detail { 64.0 } else { 44.0 };
                    let (row, _) = ui.allocate_exact_size(
                        Vec2::new((width - 24.0).max(0.0), row_height),
                        egui::Sense::hover(),
                    );
                    let retry_size = Vec2::new(80.0, 44.0);
                    let open_settings_size = Vec2::new(152.0, 44.0);
                    let retry_rect = egui::Rect::from_min_size(
                        egui::pos2(
                            row.max.x - retry_size.x,
                            row.center().y - retry_size.y / 2.0,
                        ),
                        retry_size,
                    );
                    let open_settings_rect = egui::Rect::from_min_size(
                        egui::pos2(
                            retry_rect.min.x - ui.spacing().item_spacing.x - open_settings_size.x,
                            row.center().y - open_settings_size.y / 2.0,
                        ),
                        open_settings_size,
                    );
                    let icon_rect = egui::Rect::from_min_size(row.min, Vec2::new(26.0, row_height));
                    ui.painter().text(
                        icon_rect.center(),
                        Align2::CENTER_CENTER,
                        icon_glyph(Icon::MicrophoneOff),
                        egui::FontId::proportional(18.0),
                        colors.error_text,
                    );
                    let message_rect = egui::Rect::from_min_max(
                        egui::pos2(icon_rect.max.x + ui.spacing().item_spacing.x, row.min.y),
                        egui::pos2(
                            open_settings_rect.min.x - ui.spacing().item_spacing.x,
                            row.max.y,
                        ),
                    );
                    ui.allocate_ui_at_rect(message_rect, |ui| {
                        ui.set_max_width(message_rect.width());
                        ui.label(
                            RichText::new(MICROPHONE_ACCESS_ERROR)
                                .strong()
                                .color(colors.error_text),
                        );
                        if has_distinct_detail {
                            ui.label(RichText::new(detail).small().color(colors.error_text));
                        }
                    });
                    let open_settings = ui.put(
                        open_settings_rect,
                        egui::Button::new(RichText::new("Open audio settings").color(colors.text))
                            .fill(egui::Color32::TRANSPARENT)
                            .stroke(Stroke::NONE)
                            .rounding(Rounding::same(5.0))
                            .min_size(open_settings_size),
                    );
                    paint_focus_ring(ui, &open_settings, Rounding::same(5.0));
                    if open_settings.clicked() {
                        action = ScreenAction::OpenAudioSettings;
                    }
                    let retry = ui.put(
                        retry_rect,
                        egui::Button::new(
                            RichText::new("Try again").color(colors.danger_button_text),
                        )
                        .fill(colors.error)
                        .stroke(Stroke::NONE)
                        .rounding(Rounding::same(5.0))
                        .min_size(retry_size),
                    );
                    paint_focus_ring(ui, &retry, Rounding::same(5.0));
                    if retry.clicked() {
                        action = ScreenAction::RetryMicrophone;
                    }
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
    let panel_action = transcript_frame(ui, state, transcript_panel_height(ui));
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

#[allow(dead_code)]
fn metadata(ui: &mut egui::Ui, icon: Icon, text: &str) {
    ui.label(
        RichText::new(format!("{}  {text}", icon_glyph(icon)))
            .small()
            .color(ui_palette(ui).muted_text),
    );
}

fn installed_model_badge_size(ui: &egui::Ui, text: &str, text_color: Color32) -> Vec2 {
    let font = egui::FontId::proportional(12.0);
    let text_width = ui
        .painter()
        .layout_no_wrap(text.to_owned(), font, text_color)
        .size()
        .x;
    Vec2::new(8.0 + 6.0 + 6.0 + text_width + 8.0, 22.0)
}

fn paint_installed_model_badge(
    ui: &mut egui::Ui,
    text: &str,
    rect: egui::Rect,
    dot_color: Color32,
    text_color: Color32,
) {
    let colors = ui_palette(ui);
    let font = egui::FontId::proportional(12.0);
    ui.painter()
        .rect_filled(rect, Rounding::same(999.0), colors.disabled_bg);
    ui.painter().circle_filled(
        egui::pos2(rect.left() + 11.0, rect.center().y),
        3.0,
        dot_color,
    );
    ui.painter().text(
        egui::pos2(rect.left() + 20.0, rect.center().y),
        Align2::LEFT_CENTER,
        text,
        font,
        text_color,
    );
    let response = ui.interact(rect, ui.make_persistent_id("active-badge"), Sense::hover());
    ui.ctx().accesskit_node_builder(response.id, |builder| {
        builder.set_role(egui::accesskit::Role::StaticText);
        builder.set_name(text);
        builder.set_bounds(accesskit_rect(rect));
    });
}

const MODEL_COMPARISON_BOTTOM_GAP: f32 = 24.0;
const MODEL_LIST_TO_DOCK_GAP: f32 = 24.0;
const MODEL_COMPARISON_COLLAPSED_HEIGHT: f32 = 82.0;
const COMPARISON_TABLE_MIN_WIDTH: f32 = 1_000.0;
const MODEL_CARD_GAP: f32 = 8.0;
const MODEL_CARD_HORIZONTAL_INSET: f32 = 16.0;
const MODEL_CARD_VERTICAL_INSET: f32 = 8.0;
const MODEL_RATING_METER_WIDTH: f32 = 62.0;
const MODEL_CARD_COMPACT_BREAKPOINT: f32 = 620.0;
const MODEL_CARD_SHADOW_GUTTER: f32 = 6.0;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ModelCardVisualState {
    Idle,
    Active,
}

fn model_card_visual_style(
    colors: super::theme::ThemePalette,
    state: ModelCardVisualState,
) -> (Color32, Stroke, egui::epaint::Shadow) {
    match state {
        ModelCardVisualState::Idle => (
            colors.card_bg,
            Stroke::new(1.0, colors.border),
            egui::epaint::Shadow {
                offset: Vec2::new(0.0, 1.0),
                blur: 6.0,
                spread: 0.0,
                color: Color32::from_black_alpha(20),
            },
        ),
        ModelCardVisualState::Active => (
            colors.panel_bg,
            Stroke::new(2.0, colors.accent),
            egui::epaint::Shadow {
                offset: Vec2::new(0.0, 6.0),
                blur: 18.0,
                spread: 1.0,
                color: Color32::from_black_alpha(48),
            },
        ),
    }
}
#[derive(Clone, Copy)]
enum ModelCard<'a> {
    Local(&'a ModelViewModel),
    Remote(&'a RemoteCatalogEntryView, &'a RemoteCatalogVariantView),
}

impl ModelCard<'_> {
    fn key(&self) -> ModelCardKey {
        match *self {
            Self::Local(model) => ModelCardKey::Local(model.id.clone()),
            Self::Remote(entry, variant) => ModelCardKey::Remote {
                entry_id: entry.id.clone(),
                variant_id: variant.id.clone(),
            },
        }
    }

    fn matches_key(&self, key: &ModelCardKey) -> bool {
        match (*self, key) {
            (Self::Local(model), ModelCardKey::Local(id)) => model.id == *id,
            (
                Self::Remote(entry, variant),
                ModelCardKey::Remote {
                    entry_id,
                    variant_id,
                },
            ) => entry.id == *entry_id && variant.id == *variant_id,
            _ => false,
        }
    }
}

struct ModelCardRenderResult {
    action: ScreenAction,
    restored_remove_focus: bool,
}

struct ModelSectionFocus<'a> {
    expanded: Option<&'a ModelCardKey>,
    can_replace_active: bool,
    restore_remove_focus: Option<&'a str>,
}

fn local_model_matches(
    model: &ModelViewModel,
    query: &str,
    language_filter: ModelLanguageFilter,
) -> bool {
    language_filter.matches(&model.languages)
        && (query.is_empty()
            || model.display_name.to_ascii_lowercase().contains(query)
            || model.variant_label.to_ascii_lowercase().contains(query)
            || model.language_summary.to_ascii_lowercase().contains(query)
            || model
                .description
                .as_deref()
                .is_some_and(|description| description.to_ascii_lowercase().contains(query)))
}

fn build_model_card_lists<'a>(
    models: &'a [ModelViewModel],
    model_catalog: &'a [ModelViewModel],
    remote_catalog: &'a RemoteCatalogView,
    language_filter: ModelLanguageFilter,
) -> (Vec<ModelCard<'a>>, Vec<ModelCard<'a>>) {
    let query = remote_catalog.query.trim().to_ascii_lowercase();
    let installed = models
        .iter()
        .filter(|model| model.installed && local_model_matches(model, &query, language_filter))
        .map(ModelCard::Local)
        .collect();
    let known_ids = model_catalog
        .iter()
        .chain(models.iter())
        .map(|model| model.id.as_str())
        .collect::<HashSet<_>>();
    let mut available = model_catalog
        .iter()
        .filter(|model| !model.installed && local_model_matches(model, &query, language_filter))
        .map(ModelCard::Local)
        .collect::<Vec<_>>();
    available.extend(remote_catalog.entries.iter().flat_map(|entry| {
        entry.variants.iter().filter_map(|variant| {
            let duplicates_local = variant
                .normalized_model_id
                .as_deref()
                .is_some_and(|id| known_ids.contains(id))
                || variant
                    .managed_model_id
                    .as_deref()
                    .is_some_and(|id| known_ids.contains(id));
            (!duplicates_local).then_some(ModelCard::Remote(entry, variant))
        })
    }));
    (installed, available)
}

fn formatted_language_summary(languages: &[String]) -> String {
    let codes = languages
        .iter()
        .filter_map(|language| {
            let normalized = language.trim().to_ascii_lowercase();
            let code = match normalized.as_str() {
                "en" | "english" => "EN",
                "es" | "spanish" => "ES",
                "ja" | "japanese" => "JA",
                "ko" | "korean" => "KO",
                "zh" | "chinese" | "mandarin" => "ZH",
                "fr" | "french" => "FR",
                "de" | "german" => "DE",
                "pt" | "portuguese" => "PT",
                "it" | "italian" => "IT",
                "ru" | "russian" => "RU",
                "ar" | "arabic" => "AR",
                code if (2..=3).contains(&code.len())
                    && code.bytes().all(|byte| byte.is_ascii_alphabetic()) =>
                {
                    return Some(code.to_ascii_uppercase());
                }
                _ => return None,
            };
            Some(code.to_owned())
        })
        .fold(Vec::new(), |mut unique, code| {
            if !unique.contains(&code) {
                unique.push(code);
            }
            unique
        });
    match codes.len() {
        0 => "—".to_owned(),
        1..=3 => codes.join(","),
        _ => "Multilingual".to_owned(),
    }
}

fn model_language_summary(languages: &[String]) -> (&'static str, String) {
    let summary = formatted_language_summary(languages);
    if summary == "\u{2014}" {
        ("Languages unavailable", summary)
    } else {
        ("Languages", summary)
    }
}

const MODEL_DESCRIPTION_FADE_WIDTH: f32 = 28.0;
const MODEL_DESCRIPTION_FADE_STEPS: usize = 4;

fn description_fade_alpha(step: usize) -> u8 {
    (((step + 1) * u8::MAX as usize) / MODEL_DESCRIPTION_FADE_STEPS) as u8
}

fn description_overflows(ui: &egui::Ui, description: &str, width: f32) -> bool {
    ui.painter()
        .layout_no_wrap(
            description.to_owned(),
            egui::TextStyle::Small.resolve(ui.style()),
            ui_palette(ui).muted_text,
        )
        .size()
        .x
        > width
}

fn render_model_description_preview(
    ui: &mut egui::Ui,
    description: &str,
    width: f32,
    left_inset: f32,
) -> Option<egui::Rect> {
    let colors = ui_palette(ui);
    let content_width = (width - left_inset).max(0.0);
    let overflow = description_overflows(ui, description, content_width);
    let preview = ui
        .allocate_ui_with_layout(
            Vec2::new(width, 18.0),
            Layout::left_to_right(Align::Center),
            |ui| {
                ui.add_space(left_inset);
                ui.allocate_ui_with_layout(
                    Vec2::new(content_width, 18.0),
                    Layout::left_to_right(Align::Center),
                    |ui| {
                        ui.set_width(content_width);
                        ui.add(
                            egui::Label::new(
                                RichText::new(description).small().color(colors.muted_text),
                            )
                            .truncate(true),
                        )
                    },
                )
                .inner
            },
        )
        .inner;
    overflow.then_some(preview.rect)
}

fn render_model_description(
    ui: &mut egui::Ui,
    description: &str,
    width: f32,
    left_inset: f32,
    expanded: bool,
) -> Option<egui::Rect> {
    if !expanded {
        return render_model_description_preview(ui, description, width, left_inset);
    }
    let colors = ui_palette(ui);
    let content_width = (width - left_inset).max(0.0);
    ui.horizontal_top(|ui| {
        ui.add_space(left_inset);
        ui.allocate_ui_with_layout(
            Vec2::new(content_width, 0.0),
            Layout::top_down(Align::LEFT),
            |ui| {
                ui.set_width(content_width);
                ui.label(RichText::new(description).small().color(colors.muted_text));
            },
        );
    });
    None
}

fn description_fade_color(surface: Color32, step: usize) -> Color32 {
    Color32::from_rgba_premultiplied(
        surface.r(),
        surface.g(),
        surface.b(),
        description_fade_alpha(step),
    )
}

fn paint_description_fade(ui: &egui::Ui, rect: egui::Rect, surface: Color32) {
    let fade = MODEL_DESCRIPTION_FADE_WIDTH.min(rect.width());
    let band_width = fade / MODEL_DESCRIPTION_FADE_STEPS as f32;
    for step in 0..MODEL_DESCRIPTION_FADE_STEPS {
        let x0 = rect.right() - fade + band_width * step as f32;
        let x1 = if step + 1 == MODEL_DESCRIPTION_FADE_STEPS {
            rect.right()
        } else {
            x0 + band_width
        };
        ui.painter().rect_filled(
            egui::Rect::from_min_max(egui::pos2(x0, rect.top()), egui::pos2(x1, rect.bottom())),
            Rounding::ZERO,
            description_fade_color(surface, step),
        );
    }
}

fn normalized_languages(languages: &[String]) -> Vec<String> {
    languages
        .iter()
        .map(|language| friendly_language_name(language))
        .filter(|language| !language.is_empty())
        .fold(Vec::new(), |mut unique, language| {
            if !unique
                .iter()
                .any(|existing: &String| existing.eq_ignore_ascii_case(&language))
            {
                unique.push(language);
            }
            unique
        })
}

fn friendly_language_name(language: &str) -> String {
    match language.trim().to_ascii_lowercase().as_str() {
        "en" | "english" => "English".to_owned(),
        "es" | "spanish" => "Spanish".to_owned(),
        "ja" | "japanese" => "Japanese".to_owned(),
        "ko" | "korean" => "Korean".to_owned(),
        "zh" | "chinese" | "mandarin" => "Mandarin".to_owned(),
        "fr" | "french" => "French".to_owned(),
        "de" | "german" => "German".to_owned(),
        "pt" | "portuguese" => "Portuguese".to_owned(),
        "it" | "italian" => "Italian".to_owned(),
        "ru" | "russian" => "Russian".to_owned(),
        "ar" | "arabic" => "Arabic".to_owned(),
        other => other.to_owned(),
    }
}

fn speed_rating(tier: ModelSpeedTier) -> Option<(u8, &'static str)> {
    match tier {
        ModelSpeedTier::VeryFast => Some((5, "Very fast")),
        ModelSpeedTier::Fast => Some((4, "Fast")),
        ModelSpeedTier::Balanced => Some((3, "Balanced")),
        ModelSpeedTier::AccurateSlow => Some((2, "Slow")),
        ModelSpeedTier::Unknown => None,
    }
}

fn accuracy_rating(guidance: &str) -> Option<(u8, &'static str)> {
    match guidance.trim().to_ascii_lowercase().as_str() {
        "basic" | "basic accuracy" => Some((1, "Basic")),
        "fair" | "fair accuracy" => Some((2, "Fair")),
        "good" | "good accuracy" => Some((3, "Good")),
        "better" | "better accuracy" | "high" | "high accuracy" => Some((4, "High")),
        "highest" | "highest accuracy" => Some((5, "Highest")),
        _ => None,
    }
}

fn rating_meter(
    ui: &mut egui::Ui,
    name: &str,
    rating: Option<(u8, &'static str)>,
    show_label: bool,
) {
    let colors = ui_palette(ui);
    let accessible_name = rating.map_or_else(
        || format!("{name}: Not rated"),
        |(value, label)| format!("{name}: {label} ({} of 5)", value.min(5)),
    );
    let content_height = if show_label { 28.0 } else { 18.0 };
    ui.allocate_ui_with_layout(
        Vec2::new(MODEL_RATING_METER_WIDTH, content_height),
        Layout::top_down(Align::Min),
        |ui| {
            if show_label {
                let (label_rect, _) = ui
                    .allocate_exact_size(Vec2::new(MODEL_RATING_METER_WIDTH, 14.0), Sense::hover());
                ui.painter().text(
                    label_rect.left_top(),
                    Align2::LEFT_TOP,
                    name.to_ascii_uppercase(),
                    egui::TextStyle::Small.resolve(ui.style()),
                    colors.muted_text,
                );
                #[cfg(test)]
                ui.ctx().accesskit_node_builder(
                    ui.make_persistent_id(("model-metric-label", &accessible_name)),
                    |builder| {
                        builder.set_role(egui::accesskit::Role::StaticText);
                        builder.set_name(format!("{accessible_name} visible label"));
                        builder.set_bounds(accesskit_rect(label_rect));
                    },
                );
            }
            let (rect, response) =
                ui.allocate_exact_size(Vec2::new(MODEL_RATING_METER_WIDTH, 18.0), Sense::hover());
            let filled = rating.map_or(0.0, |(value, _)| f32::from(value.min(5)) / 5.0);
            let track = egui::Rect::from_center_size(rect.center(), Vec2::new(rect.width(), 7.0));
            ui.painter()
                .rect_filled(track, Rounding::same(3.5), colors.meter_track);
            if filled > 0.0 {
                let fill = egui::Rect::from_min_size(
                    track.min,
                    Vec2::new(track.width() * filled, track.height()),
                );
                ui.painter().rect_filled(
                    fill,
                    Rounding::same(3.5),
                    colors.meter_rating(rating.unwrap().0),
                );
            }
            response.widget_info(|| {
                egui::WidgetInfo::labeled(egui::WidgetType::Label, accessible_name.clone())
            });
            ui.ctx().accesskit_node_builder(response.id, |builder| {
                builder.set_role(egui::accesskit::Role::Meter);
                builder.set_name(accessible_name.clone());
                if let Some((value, _)) = rating {
                    builder.set_min_numeric_value(0.0);
                    builder.set_max_numeric_value(5.0);
                    builder.set_numeric_value(f64::from(value.min(5)));
                }
            });
        },
    );
}

/// Paint a rating inside an already allocated model-grid cell. The row grid is
/// absolute, so the meter must not allocate from a parent flow layout: doing
/// so lets its label consume width intended for the next logical column.
fn model_row_description(card: ModelCard<'_>) -> String {
    match card {
        ModelCard::Local(model)
            if matches!(
                model.download_state,
                ModelDownloadState::Downloading
                    | ModelDownloadState::Verifying
                    | ModelDownloadState::Failed
                    | ModelDownloadState::Cancelled
            ) =>
        {
            model_download_label(model)
        }
        ModelCard::Local(model) => model
            .description
            .clone()
            .unwrap_or_else(|| "Local speech-to-text model.".to_owned()),
        ModelCard::Remote(entry, variant) => model_download_progress_presentation(card)
            .map(|progress| progress.display_text)
            .or_else(|| {
                variant
                    .status_label
                    .clone()
                    .filter(|status| !status.trim().is_empty())
            })
            .unwrap_or_else(|| entry.description.clone()),
    }
}

fn remote_primary_action(variant: &RemoteCatalogVariantView) -> Option<&RemoteCatalogActionView> {
    variant.actions.iter().find(|action| {
        matches!(
            action.kind,
            RemoteCatalogActionKind::Install { .. } | RemoteCatalogActionKind::Use { .. }
        )
    })
}

fn local_model_primary_action(model: &ModelViewModel) -> ScreenAction {
    if model.primary_action_installs_upgrade {
        ScreenAction::UpgradeModel(model.id.clone())
    } else if model.primary_action_repairs_runtime {
        ScreenAction::RepairModelRuntime(model.id.clone())
    } else {
        ScreenAction::SelectModel(model.id.clone())
    }
}

struct ModelLifecyclePresentation<'a> {
    action: ScreenAction,
    icon: Icon,
    label: String,
    accessible_name: String,
    enabled: bool,
    disabled_reason: Option<&'a str>,
    compact_size: Option<String>,
    tone: ModelLifecycleTone,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ModelLifecycleTone {
    Standard,
    InverseFilled,
    DestructiveOutline,
}

fn model_lifecycle_presentation<'a>(
    card: ModelCard<'a>,
    can_replace_active: bool,
) -> ModelLifecyclePresentation<'a> {
    match card {
        ModelCard::Local(model) if model.download_state == ModelDownloadState::Downloading => {
            ModelLifecyclePresentation {
                action: ScreenAction::CancelModelInstall(model.id.clone()),
                icon: Icon::Close,
                label: "Cancel".into(),
                accessible_name: format!("Cancel {} download", model.display_name),
                enabled: model.cancel_supported,
                disabled_reason: model.primary_action_disabled_reason.as_deref(),
                compact_size: None,
                tone: ModelLifecycleTone::Standard,
            }
        }
        ModelCard::Local(model)
            if matches!(
                model.download_state,
                ModelDownloadState::Queued
                    | ModelDownloadState::Verifying
                    | ModelDownloadState::Extracting
            ) =>
        {
            ModelLifecyclePresentation {
                action: ScreenAction::None,
                icon: Icon::Spinner,
                label: "Installing…".into(),
                accessible_name: format!("Installing {}", model.display_name),
                enabled: false,
                disabled_reason: Some("Scribe is preparing the model and cannot cancel this step."),
                compact_size: None,
                tone: ModelLifecycleTone::Standard,
            }
        }
        ModelCard::Local(model) if model.installed => {
            if model.primary_action_installs_upgrade || model.primary_action_repairs_runtime {
                let upgrade = model.primary_action_installs_upgrade;
                ModelLifecyclePresentation {
                    action: local_model_primary_action(model),
                    icon: if upgrade {
                        Icon::Download
                    } else {
                        Icon::Refresh
                    },
                    label: if upgrade { "Upgrade" } else { "Repair" }.into(),
                    accessible_name: format!(
                        "{} {}",
                        if upgrade { "Upgrade" } else { "Repair" },
                        model.display_name
                    ),
                    enabled: model.primary_action_enabled,
                    disabled_reason: model.primary_action_disabled_reason.as_deref(),
                    compact_size: None,
                    tone: ModelLifecycleTone::Standard,
                }
            } else {
                let reason = (!model.removal_supported)
                    .then_some(
                        "This model is not an app-managed download and cannot be removed here.",
                    )
                    .or_else(|| {
                        (model.selected && !model.legacy_cleanup_pending && !can_replace_active)
                            .then_some(
                                "Install another ready model before removing the selected model.",
                            )
                    });
                ModelLifecyclePresentation {
                    action: ScreenAction::RequestModelRemoval(model.id.clone()),
                    icon: Icon::Trash,
                    label: "Delete".into(),
                    accessible_name: format!("Delete {}", model.display_name),
                    enabled: reason.is_none(),
                    disabled_reason: reason,
                    compact_size: None,
                    tone: ModelLifecycleTone::DestructiveOutline,
                }
            }
        }
        ModelCard::Local(model) => {
            let (action, label) = if model.primary_action_installs_upgrade {
                (ScreenAction::UpgradeModel(model.id.clone()), "Upgrade")
            } else {
                (
                    ScreenAction::InstallModel(model.id.clone()),
                    match model.download_state {
                        ModelDownloadState::Failed => "Retry",
                        ModelDownloadState::Cancelled => "Resume",
                        _ => "Install",
                    },
                )
            };
            ModelLifecyclePresentation {
                action,
                icon: Icon::Download,
                label: label.into(),
                accessible_name: format!("{label} {}", model.display_name),
                enabled: if model.primary_action_installs_upgrade {
                    model.primary_action_enabled
                } else {
                    model.install_action_enabled
                },
                disabled_reason: model.primary_action_disabled_reason.as_deref().or_else(|| {
                    (!model.install_supported)
                        .then_some("This model has no supported managed download in this build.")
                }),
                compact_size: model
                    .total_bytes
                    .map(format_compact_artifact_size)
                    .filter(|_| label == "Install"),
                tone: if matches!(label, "Install" | "Retry" | "Resume") {
                    ModelLifecycleTone::InverseFilled
                } else {
                    ModelLifecycleTone::Standard
                },
            }
        }
        ModelCard::Remote(entry, variant) => {
            let remote = variant
                .actions
                .iter()
                .find(|action| matches!(action.kind, RemoteCatalogActionKind::Cancel { .. }))
                .or_else(|| remote_primary_action(variant));
            let label = remote.map_or("Install", |action| action.label.as_str());
            let label = if label == "Remove" { "Delete" } else { label };
            ModelLifecyclePresentation {
                action: remote.map_or(ScreenAction::None, |action| {
                    screen_action_for_remote_catalog_action(&action.kind)
                }),
                icon: if label == "Delete" || label == "Remove" {
                    Icon::Trash
                } else {
                    Icon::Download
                },
                label: label.into(),
                accessible_name: format!("{label} {}", entry.display_name),
                enabled: remote.is_some_and(|action| action.enabled),
                disabled_reason: remote.and_then(|action| action.disabled_reason.as_deref()),
                compact_size: (label == "Install")
                    .then(|| format_compact_artifact_size(variant.size_bytes)),
                tone: match label {
                    "Install" | "Retry" | "Resume" => ModelLifecycleTone::InverseFilled,
                    "Delete" => ModelLifecycleTone::DestructiveOutline,
                    _ => ModelLifecycleTone::Standard,
                },
            }
        }
    }
}

fn format_compact_artifact_size(size_bytes: u64) -> String {
    const GB: u64 = 1_000_000_000;
    if size_bytes >= GB {
        format!("{:.1} GB", size_bytes as f64 / GB as f64)
    } else {
        format!("{} MB", (size_bytes / 1_000_000).max(1))
    }
}

fn model_summary_features(card: ModelCard<'_>) -> (Vec<(Icon, &'static str)>, bool) {
    let capabilities = match card {
        ModelCard::Local(model) => model.capabilities,
        ModelCard::Remote(_, variant) => variant.capabilities,
    };
    if !capabilities.capabilities_known {
        return (Vec::new(), false);
    }
    let features = [
        (
            capabilities.native_streaming,
            Icon::Streaming,
            "Native streaming",
        ),
        (capabilities.translation, Icon::Translation, "Translation"),
        (
            capabilities.timestamps,
            Icon::WordTimestamps,
            "Word timestamps",
        ),
        (
            capabilities.batch_transcription,
            Icon::BatchTranscription,
            "Batch transcription",
        ),
    ]
    .into_iter()
    .filter_map(|(supported, icon, name)| supported.then_some((icon, name)))
    .collect::<Vec<_>>();
    (features, true)
}

fn model_requirement_cells(card: ModelCard<'_>) -> [(&'static str, String); 3] {
    let (ram_bytes, storage_label, storage_bytes, capabilities) = match card {
        ModelCard::Local(model) => (
            model.estimated_ram_bytes,
            if model.installed {
                "ON DISK"
            } else {
                "DOWNLOAD SIZE"
            },
            if model.installed {
                model.disk_bytes.or(model.total_bytes)
            } else {
                model.total_bytes
            },
            model.capabilities,
        ),
        ModelCard::Remote(_, variant) => (
            variant.expected_ram_bytes,
            "DOWNLOAD SIZE",
            None,
            variant.capabilities,
        ),
    };
    let storage = match card {
        ModelCard::Local(_) => storage_bytes.map(format_bytes),
        ModelCard::Remote(_, variant) => (!variant.size_label.is_empty())
            .then(|| variant.size_label.clone())
            .or_else(|| (variant.size_bytes > 0).then(|| format_bytes(variant.size_bytes))),
    }
    .unwrap_or_else(|| "Unknown".into());
    let gpu = if !capabilities.capabilities_known {
        "Unknown"
    } else if capabilities.gpu {
        "Supported"
    } else {
        "Not supported"
    };
    [
        (
            "RAM",
            ram_bytes
                .map(format_bytes)
                .unwrap_or_else(|| "Unknown".into()),
        ),
        (storage_label, storage),
        ("GPU", gpu.into()),
    ]
}

fn render_model_requirement_cells(ui: &mut egui::Ui, cells: [(&str, String); 3]) -> egui::Response {
    let colors = ui_palette(ui);
    let width = ui.available_width();
    let gap = ui.spacing().item_spacing.x;
    let three_columns = width >= 480.0;
    let cell_width = if three_columns {
        ((width - gap * 2.0) / 3.0).max(0.0)
    } else {
        width
    };
    let render_cell = |ui: &mut egui::Ui, label: &str, value: String| {
        ui.vertical(|ui| {
            ui.set_min_width(cell_width);
            ui.label(
                RichText::new(label)
                    .small()
                    .strong()
                    .color(colors.muted_text),
            );
            ui.label(value);
        });
    };
    if three_columns {
        ui.horizontal(|ui| {
            for (label, value) in cells {
                ui.allocate_ui_with_layout(
                    Vec2::new(cell_width, 0.0),
                    Layout::top_down(Align::LEFT),
                    |ui| render_cell(ui, label, value),
                );
            }
        })
        .response
    } else {
        ui.vertical(|ui| {
            for (label, value) in cells {
                render_cell(ui, label, value);
            }
        })
        .response
    }
}

fn model_feature_grid_geometry(feature_count: usize, available_width: f32) -> (usize, usize, Vec2) {
    const ICON_WIDTH: f32 = 28.0;
    const ICON_GAP: f32 = 8.0;
    const ICON_TARGET: f32 = 32.0;
    let max_columns =
        (((available_width + ICON_GAP) / (ICON_WIDTH + ICON_GAP)).floor() as usize).clamp(1, 4);
    let columns = feature_count.min(max_columns).max(1);
    let rows = feature_count.div_ceil(columns).max(1);
    (
        columns,
        rows,
        Vec2::new(
            columns as f32 * ICON_WIDTH + columns.saturating_sub(1) as f32 * ICON_GAP,
            rows as f32 * ICON_TARGET + rows.saturating_sub(1) as f32 * ICON_GAP,
        ),
    )
}

fn render_model_features(
    ui: &mut egui::Ui,
    card: ModelCard<'_>,
    available_width: f32,
) -> egui::Response {
    let colors = ui_palette(ui);
    let (features, known) = model_summary_features(card);
    let name = if !known {
        "Features unknown".to_owned()
    } else if features.is_empty() {
        "No supported features".to_owned()
    } else {
        format!(
            "Features: {}",
            features
                .iter()
                .map(|(_, name)| *name)
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    const ICON_GAP: f32 = 8.0;
    const ICON_TARGET: f32 = 32.0;
    const ICON_WIDTH: f32 = 28.0;
    let (columns, _, size) = model_feature_grid_geometry(features.len(), available_width);
    let (rect, response) = ui.allocate_exact_size(size, Sense::hover());
    if known {
        for (index, (icon, feature_name)) in features.iter().enumerate() {
            let column = index % columns;
            let row = index / columns;
            let icon_rect = egui::Rect::from_center_size(
                egui::pos2(
                    rect.left() + ICON_WIDTH / 2.0 + column as f32 * (ICON_WIDTH + ICON_GAP),
                    rect.top() + ICON_TARGET / 2.0 + row as f32 * (ICON_TARGET + ICON_GAP),
                ),
                Vec2::new(ICON_WIDTH, ICON_TARGET),
            );
            ui.painter().text(
                icon_rect.center(),
                Align2::CENTER_CENTER,
                icon_glyph(*icon),
                egui::FontId::proportional(16.0),
                colors.muted_text,
            );
            ui.interact(
                icon_rect,
                response.id.with(("feature-tooltip", index)),
                Sense::hover(),
            )
            .on_hover_text(*feature_name);
        }
    } else {
        ui.painter().text(
            rect.center(),
            Align2::CENTER_CENTER,
            "—",
            egui::FontId::proportional(16.0),
            colors.muted_text,
        );
    }
    ui.ctx().accesskit_node_builder(response.id, |builder| {
        builder.set_role(egui::accesskit::Role::Group);
        builder.set_name(name.as_str());
        builder.set_bounds(accesskit_rect(rect));
    });
    if !known {
        return response.on_hover_text(name);
    }
    response
}

fn render_expanded_model_features(
    ui: &mut egui::Ui,
    card: ModelCard<'_>,
    model_name: &str,
) -> egui::Response {
    let colors = ui_palette(ui);
    let (features, known) = model_summary_features(card);
    if !known {
        return ui.label("Feature support is unknown");
    }
    if features.is_empty() {
        return ui.label("No supported features");
    }
    let columns = if ui.available_width() >= 480.0 { 2 } else { 1 };
    let gap = ui.spacing().item_spacing.x;
    let cell_width =
        ((ui.available_width() - gap * (columns - 1) as f32) / columns as f32).max(0.0);
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        for (row_index, row) in features.chunks(columns).enumerate() {
            if row_index > 0 {
                render_model_layout_gap(
                    ui,
                    model_name,
                    &format!("expanded feature row gap {row_index}"),
                    4.0,
                );
            }
            let _row_response = ui
                .allocate_ui_with_layout(
                    Vec2::new(ui.available_width(), 32.0),
                    Layout::left_to_right(Align::Center),
                    |ui| {
                        for (icon, name) in row {
                            ui.allocate_ui_with_layout(
                                Vec2::new(cell_width, 32.0),
                                Layout::left_to_right(Align::Center),
                                |ui| {
                                    paint_decorative_icon(ui, *icon, colors.muted_text);
                                    ui.label(*name);
                                },
                            );
                        }
                    },
                )
                .response;
            #[cfg(test)]
            register_model_layout_rect(
                ui,
                model_name,
                &format!("expanded feature row {row_index}"),
                _row_response.rect,
            );
        }
    })
    .response
}

fn compact_model_icon_action(
    ui: &mut egui::Ui,
    icon: Icon,
    accessible_name: &str,
    enabled: bool,
    disabled_reason: Option<&str>,
    progress: Option<f32>,
) -> egui::Response {
    let colors = ui_palette(ui);
    let enabled = enabled && ui.is_enabled();
    let (target, response) = ui.allocate_exact_size(Vec2::splat(44.0), Sense::click());
    let hovered = response.hovered() && enabled;
    if hovered || response.has_focus() {
        ui.painter()
            .rect_filled(target, Rounding::same(7.0), colors.active_card_bg);
    }
    if let Some(progress) = progress {
        ui.painter()
            .circle_stroke(target.center(), 12.0, Stroke::new(2.0, colors.primary));
        ui.painter().text(
            target.center(),
            Align2::CENTER_CENTER,
            format!("{:.0}", (progress * 100.0).clamp(0.0, 100.0)),
            egui::FontId::proportional(10.0),
            colors.text,
        );
    } else {
        ui.painter().text(
            target.center(),
            Align2::CENTER_CENTER,
            icon_glyph(icon),
            egui::FontId::proportional(18.0),
            colors.muted_text,
        );
    }
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, accessible_name));
    ui.ctx().accesskit_node_builder(response.id, |builder| {
        builder.set_role(egui::accesskit::Role::Button);
        builder.set_name(accessible_name);
        builder.set_bounds(accesskit_rect(target));
        if !enabled {
            builder.set_disabled();
        }
        if let Some(reason) = disabled_reason {
            builder.set_description(reason);
        }
    });
    if let Some(reason) = disabled_reason {
        focus_tooltip(ui, &response, reason);
        response.clone().on_hover_text(reason);
    } else {
        focus_tooltip(ui, &response, accessible_name);
        response.clone().on_hover_text(accessible_name);
    }
    paint_focus_ring(ui, &response, Rounding::same(7.0));
    response
}

fn model_lifecycle_button(
    ui: &mut egui::Ui,
    label: &str,
    accessible_name: &str,
    enabled: bool,
    disabled_reason: Option<&str>,
    tone: ModelLifecycleTone,
) -> egui::Response {
    let enabled = enabled && ui.is_enabled();
    let response = match tone {
        ModelLifecycleTone::Standard => ui.add_enabled(
            enabled,
            egui::Button::new(label).min_size(Vec2::new(44.0, 44.0)),
        ),
        ModelLifecycleTone::InverseFilled => {
            let colors = ui_palette(ui);
            let color = if enabled {
                colors.inverse_neutral_text
            } else {
                colors.muted_text
            };
            let galley = ui.painter().layout_no_wrap(
                label.to_owned(),
                egui::TextStyle::Button.resolve(ui.style()),
                color,
            );
            let visual_size = Vec2::new(galley.size().x + 24.0, 32.0);
            let (target, response) =
                ui.allocate_exact_size(Vec2::new(visual_size.x.max(44.0), 44.0), Sense::click());
            let visual = egui::Rect::from_center_size(target.center(), visual_size);
            let fill = if enabled {
                colors.inverse_neutral_bg
            } else {
                colors.disabled_bg
            };
            ui.painter().rect_filled(visual, Rounding::same(7.0), fill);
            ui.painter()
                .galley(visual.center() - galley.size() * 0.5, galley, color);
            response
        }
        ModelLifecycleTone::DestructiveOutline => {
            let colors = ui_palette(ui);
            let text = format!("{}  {label}", icon_glyph(Icon::Trash));
            let galley = ui.painter().layout_no_wrap(
                text,
                egui::TextStyle::Button.resolve(ui.style()),
                colors.error_text,
            );
            let visual_size = Vec2::new(galley.size().x + 24.0, 32.0);
            let (target, response) =
                ui.allocate_exact_size(Vec2::new(visual_size.x.max(44.0), 44.0), Sense::click());
            let visual = egui::Rect::from_center_size(target.center(), visual_size);
            let color = if enabled {
                colors.error_text
            } else {
                colors.muted_text
            };
            let fill = if response.hovered() && enabled {
                colors.error_pale
            } else {
                colors.card_bg
            };
            ui.painter()
                .rect(visual, Rounding::same(7.0), fill, Stroke::new(1.0, color));
            ui.painter()
                .galley(visual.center() - galley.size() * 0.5, galley, color);
            response
        }
    };
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, accessible_name));
    ui.ctx().accesskit_node_builder(response.id, |builder| {
        builder.set_role(egui::accesskit::Role::Button);
        builder.set_name(accessible_name);
        builder.set_bounds(accesskit_rect(response.rect));
        if !enabled {
            builder.set_disabled();
        }
        if let Some(reason) = disabled_reason {
            builder.set_description(reason);
        }
    });
    if let Some(reason) = disabled_reason {
        response.clone().on_hover_text(reason);
    } else {
        response.clone().on_hover_text(accessible_name);
    }
    paint_focus_ring(ui, &response, Rounding::same(7.0));
    response
}

fn paint_decorative_icon(ui: &mut egui::Ui, icon: Icon, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(16.0, 18.0), Sense::hover());
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        icon_glyph(icon),
        egui::FontId::proportional(14.0),
        color,
    );
}

struct ModelIdentityResponse {
    has_focus: bool,
}

fn render_model_identity(
    ui: &mut egui::Ui,
    name: &str,
    active: bool,
    selectable: bool,
    width: f32,
) -> ModelIdentityResponse {
    let colors = ui_palette(ui);
    let badge_size = if active {
        installed_model_badge_size(ui, "Active", colors.success_text)
    } else {
        Vec2::ZERO
    };
    let icon_width = 20.0;
    let content_gap = 6.0;
    let badge_gap = if active { 8.0 } else { 0.0 };
    let title_width = (width - icon_width - content_gap - badge_size.x - badge_gap).max(44.0);
    let mut job = egui::text::LayoutJob::default();
    job.append(
        name,
        0.0,
        egui::TextFormat {
            font_id: egui::TextStyle::Button.resolve(ui.style()),
            color: colors.text,
            ..Default::default()
        },
    );
    job.wrap.max_width = title_width;
    job.wrap.max_rows = 2;
    let galley = ui.fonts(|fonts| fonts.layout_job(job));
    let height = galley.size().y.max(44.0);
    let (rect, response) = ui.allocate_exact_size(
        Vec2::new(width, height),
        if selectable {
            Sense::click()
        } else {
            Sense::hover()
        },
    );
    if selectable && (response.hovered() || response.has_focus()) {
        ui.painter()
            .rect_filled(rect, Rounding::same(5.0), colors.active_card_bg);
    }
    let title_pos = egui::pos2(
        rect.left() + icon_width + content_gap,
        rect.center().y - galley.size().y / 2.0,
    );
    let first_line_center_y = title_pos.y
        + galley
            .rows
            .first()
            .map_or(galley.size().y / 2.0, |row| row.rect.center().y);
    ui.painter().text(
        egui::pos2(rect.left() + icon_width / 2.0, first_line_center_y),
        Align2::CENTER_CENTER,
        icon_glyph(if active {
            Icon::CheckCircle
        } else {
            Icon::Waveform
        }),
        egui::FontId::proportional(14.0),
        if active {
            colors.success
        } else {
            colors.muted_text
        },
    );
    ui.painter().galley(title_pos, galley.clone(), colors.text);
    if active {
        let badge_rect = active_badge_rect(
            rect,
            title_pos,
            galley.rows.first().map_or(0.0, |row| row.rect.width()),
            first_line_center_y,
            badge_size,
        );
        paint_installed_model_badge(
            ui,
            "Active",
            badge_rect,
            colors.success,
            colors.success_text,
        );
    }
    if selectable {
        let accessible_name = format!("Use {name} for future transcriptions");
        response.widget_info(|| {
            egui::WidgetInfo::labeled(egui::WidgetType::Button, accessible_name.clone())
        });
        ui.ctx().accesskit_node_builder(response.id, |builder| {
            builder.set_role(egui::accesskit::Role::Button);
            builder.set_name(accessible_name.as_str());
            builder.set_bounds(accesskit_rect(rect));
        });
        focus_tooltip(ui, &response, &accessible_name);
        paint_focus_ring(ui, &response, Rounding::same(5.0));
    } else {
        ui.ctx().accesskit_node_builder(response.id, |builder| {
            builder.set_role(egui::accesskit::Role::StaticText);
            builder.set_name(name);
            builder.set_bounds(accesskit_rect(egui::Rect::from_min_size(
                title_pos,
                galley.size(),
            )));
        });
    }
    ModelIdentityResponse {
        has_focus: response.has_focus(),
    }
}

fn active_badge_rect(
    identity_rect: egui::Rect,
    title_pos: egui::Pos2,
    first_line_width: f32,
    first_line_center_y: f32,
    badge_size: Vec2,
) -> egui::Rect {
    let left = (title_pos.x + first_line_width + 8.0)
        .clamp(identity_rect.left(), identity_rect.right() - badge_size.x);
    egui::Rect::from_min_size(
        egui::pos2(left, first_line_center_y - badge_size.y / 2.0),
        badge_size,
    )
}

fn render_model_metadata(
    ui: &mut egui::Ui,
    _model_name: &str,
    languages: &[String],
    include_ratings: bool,
) {
    let colors = ui_palette(ui);
    ui.horizontal_wrapped(|ui| {
        let (language_name, language_summary) = model_language_summary(languages);
        let language_description = if language_name == "Languages unavailable" {
            language_name.to_owned()
        } else {
            normalized_languages(languages).join(", ")
        };
        if !include_ratings {
            // Match the title text inset (20 px icon slot + 6 px gap).
            ui.add_space(26.0);
        }
        // Keep the decorative globe and its language value in one layout unit so
        // a wrapped row cannot leave the icon orphaned on the following line.
        let _language_row = ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            paint_decorative_icon(ui, Icon::Globe, colors.muted_text);
            let language = ui.label(
                RichText::new(&language_summary)
                    .small()
                    .color(colors.muted_text),
            );
            ui.ctx().accesskit_node_builder(language.id, |builder| {
                builder.set_name(if language_name == "Languages unavailable" {
                    language_name.to_owned()
                } else {
                    format!("{language_name}: {language_summary}")
                });
                builder.set_description(language_description.as_str());
            });
            language.on_hover_text(language_description);
        });
        #[cfg(test)]
        register_model_layout_rect(ui, _model_name, "language row", _language_row.response.rect);
        debug_assert!(!include_ratings, "ratings are rendered by the card layout");
    });
}

fn render_unified_model_card(
    ui: &mut egui::Ui,
    card: ModelCard<'_>,
    expanded: bool,
    can_replace_active: bool,
    restore_remove_focus: bool,
) -> ModelCardRenderResult {
    let colors = ui_palette(ui);
    let card_key = card.key();
    let (name, languages, active, description) = match card {
        ModelCard::Local(model) => (
            &model.display_name,
            &model.languages,
            model.active,
            model_row_description(card),
        ),
        ModelCard::Remote(entry, _) => (
            &entry.display_name,
            &entry.languages,
            false,
            model_row_description(card),
        ),
    };
    let lifecycle = model_lifecycle_presentation(card, can_replace_active);
    let title_selects_model =
        matches!(card, ModelCard::Local(model) if model.installed && model.ready && !model.active);
    let activation_id = ui.make_persistent_id(("select-model-card", card_key.clone()));
    let activation_press_id = activation_id.with("primary-press");
    let activation = title_selects_model.then(|| {
        ui.interact(
            egui::Rect::from_min_size(ui.cursor().min, Vec2::ZERO),
            activation_id,
            Sense::focusable_noninteractive(),
        )
    });
    let mut action = ScreenAction::None;
    let mut restored_remove_focus = false;
    let mut focus_within = false;
    let mut description_fade_rect = None;
    let mut activation_exclusions = Vec::new();
    let (idle_fill, idle_stroke, idle_shadow) =
        model_card_visual_style(colors, ModelCardVisualState::Idle);
    let mut prepared = Frame::none()
        .fill(idle_fill)
        .stroke(idle_stroke)
        .rounding(Rounding::same(9.0))
        .outer_margin(Margin::same(MODEL_CARD_SHADOW_GUTTER))
        .shadow(idle_shadow)
        .inner_margin(Margin::symmetric(
            MODEL_CARD_HORIZONTAL_INSET,
            MODEL_CARD_VERTICAL_INSET,
        ))
        .begin(ui);
    {
        let ui = &mut prepared.content_ui;
        let card_content_width = ui.available_width();
        ui.set_min_width(card_content_width);
        let compact = card_content_width < MODEL_CARD_COMPACT_BREAKPOINT;
        let details_name = format!(
            "{} details for {name}",
            if expanded { "Collapse" } else { "Expand" }
        );
        let render_details =
            |ui: &mut egui::Ui, action: &mut ScreenAction, focus_within: &mut bool| {
                let details = compact_model_icon_action(
                    ui,
                    if expanded {
                        Icon::ChevronUp
                    } else {
                        Icon::ChevronDown
                    },
                    &details_name,
                    true,
                    None,
                    None,
                );
                ui.ctx()
                    .accesskit_node_builder(details.id, |builder| builder.set_expanded(expanded));
                *focus_within |= details.has_focus();
                if details.clicked() {
                    *action = ScreenAction::ToggleModelCardDetails(card_key.clone());
                }
                details
            };
        let render_lifecycle = |ui: &mut egui::Ui,
                                action: &mut ScreenAction,
                                restored_remove_focus: &mut bool,
                                focus_within: &mut bool| {
            let label = if lifecycle.tone == ModelLifecycleTone::DestructiveOutline {
                lifecycle.label.clone()
            } else {
                lifecycle.compact_size.as_ref().map_or_else(
                    || format!("{}  {}", icon_glyph(lifecycle.icon), lifecycle.label),
                    |size| format!("{}  {size}", icon_glyph(lifecycle.icon)),
                )
            };
            let lifecycle_response = model_lifecycle_button(
                ui,
                &label,
                &lifecycle.accessible_name,
                lifecycle.enabled,
                lifecycle.disabled_reason,
                lifecycle.tone,
            );
            *focus_within |= lifecycle_response.has_focus();
            if restore_remove_focus
                && matches!(lifecycle.action, ScreenAction::RequestModelRemoval(_))
            {
                lifecycle_response.request_focus();
                *restored_remove_focus = true;
            }
            if lifecycle_response.clicked() && lifecycle.enabled {
                *action = lifecycle.action.clone();
            }
            lifecycle_response
        };
        if compact {
            let identity_width = card_content_width;
            let identity = render_model_identity(ui, name, active, false, identity_width);
            focus_within |= identity.has_focus;
            let description_width = card_content_width;
            description_fade_rect =
                render_model_description(ui, &description, description_width, 26.0, expanded);
            ui.horizontal(|ui| {
                rating_meter(
                    ui,
                    "Speed",
                    match card {
                        ModelCard::Local(model) => speed_rating(model.speed_tier),
                        ModelCard::Remote(_, variant) => speed_rating(variant.speed_tier),
                    },
                    true,
                );
                rating_meter(
                    ui,
                    "Accuracy",
                    match card {
                        ModelCard::Local(model) => accuracy_rating(&model.accuracy_guidance),
                        ModelCard::Remote(_, variant) => {
                            accuracy_rating(&variant.accuracy_guidance)
                        }
                    },
                    true,
                );
            });
            render_model_metadata(ui, name, languages, false);
            ui.horizontal(|ui| {
                let feature_width = ui.available_width();
                activation_exclusions.push(render_model_features(ui, card, feature_width).rect);
                activation_exclusions.push(
                    render_lifecycle(
                        ui,
                        &mut action,
                        &mut restored_remove_focus,
                        &mut focus_within,
                    )
                    .rect,
                );
                activation_exclusions.push(render_details(ui, &mut action, &mut focus_within).rect);
            });
        } else {
            let identity_track = card_content_width * 0.60;
            let right_track = card_content_width - identity_track;
            let details_width = 44.0;
            ui.scope(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                ui.horizontal_top(|ui| {
                    ui.allocate_ui_with_layout(
                        Vec2::new(identity_track, 0.0),
                        Layout::top_down(Align::Min),
                        |ui| {
                            ui.set_width(identity_track);
                            ui.spacing_mut().item_spacing.y = 2.0;
                            let identity =
                                render_model_identity(ui, name, active, false, identity_track);
                            focus_within |= identity.has_focus;
                            description_fade_rect = render_model_description(
                                ui,
                                &description,
                                identity_track,
                                26.0,
                                expanded,
                            );
                            ui.add_space(4.0);
                            render_model_metadata(ui, name, languages, false);
                        },
                    );
                    ui.allocate_ui_with_layout(
                        Vec2::new(right_track, 0.0),
                        Layout::left_to_right(Align::TOP),
                        |ui| {
                            ui.set_width(right_track);
                            ui.allocate_ui_with_layout(
                                Vec2::new((right_track - details_width).max(0.0), 0.0),
                                Layout::top_down(Align::Min),
                                |ui| {
                                    ui.set_width((right_track - details_width).max(0.0));
                                    let usable_width = ui.available_width();
                                    let region_width = (usable_width / 2.0).max(0.0);
                                    ui.spacing_mut().item_spacing.x = 0.0;
                                    ui.horizontal(|ui| {
                                        for (metric_name, rating) in [
                                            (
                                                "Speed",
                                                match card {
                                                    ModelCard::Local(model) => {
                                                        speed_rating(model.speed_tier)
                                                    }
                                                    ModelCard::Remote(_, variant) => {
                                                        speed_rating(variant.speed_tier)
                                                    }
                                                },
                                            ),
                                            (
                                                "Accuracy",
                                                match card {
                                                    ModelCard::Local(model) => {
                                                        accuracy_rating(&model.accuracy_guidance)
                                                    }
                                                    ModelCard::Remote(_, variant) => {
                                                        accuracy_rating(&variant.accuracy_guidance)
                                                    }
                                                },
                                            ),
                                        ]
                                        .into_iter()
                                        {
                                            let _region = ui.allocate_ui_with_layout(
                                                Vec2::new(region_width, 28.0),
                                                Layout::top_down(Align::Min),
                                                |ui| {
                                                    ui.set_width(region_width);
                                                    rating_meter(ui, metric_name, rating, true)
                                                },
                                            );
                                            #[cfg(test)]
                                            ui.ctx().accesskit_node_builder(
                                                ui.make_persistent_id((
                                                    "model-layout",
                                                    name,
                                                    "metric",
                                                    metric_name,
                                                )),
                                                |builder| {
                                                    builder.set_role(
                                                        egui::accesskit::Role::StaticText,
                                                    );
                                                    builder.set_name(format!(
                                                        "{name} metric region {metric_name}"
                                                    ));
                                                    builder.set_bounds(accesskit_rect(
                                                        _region.response.rect,
                                                    ));
                                                },
                                            );
                                        }
                                    });
                                    let feature_count = model_summary_features(card).0.len();
                                    let bottom_row_height =
                                        model_feature_grid_geometry(feature_count, region_width)
                                            .2
                                            .y
                                            .max(44.0);
                                    ui.horizontal(|ui| {
                                        let _feature_region = ui.allocate_ui_with_layout(
                                            Vec2::new(region_width, bottom_row_height),
                                            Layout::left_to_right(Align::Center),
                                            |ui| {
                                                ui.set_width(region_width);
                                                activation_exclusions.push(
                                                    render_model_features(ui, card, region_width)
                                                        .rect,
                                                )
                                            },
                                        );
                                        #[cfg(test)]
                                        ui.ctx().accesskit_node_builder(
                                            ui.make_persistent_id((
                                                "model-layout",
                                                name,
                                                "features",
                                            )),
                                            |builder| {
                                                builder.set_role(egui::accesskit::Role::StaticText);
                                                builder.set_name(format!("{name} feature region"));
                                                builder.set_bounds(accesskit_rect(
                                                    _feature_region.response.rect,
                                                ));
                                            },
                                        );
                                        let _lifecycle_region = ui.allocate_ui_with_layout(
                                            Vec2::new(region_width, bottom_row_height),
                                            Layout::right_to_left(Align::Center),
                                            |ui| {
                                                ui.set_width(region_width);
                                                activation_exclusions.push(
                                                    render_lifecycle(
                                                        ui,
                                                        &mut action,
                                                        &mut restored_remove_focus,
                                                        &mut focus_within,
                                                    )
                                                    .rect,
                                                )
                                            },
                                        );
                                        #[cfg(test)]
                                        ui.ctx().accesskit_node_builder(
                                            ui.make_persistent_id((
                                                "model-layout",
                                                name,
                                                "lifecycle",
                                            )),
                                            |builder| {
                                                builder.set_role(egui::accesskit::Role::StaticText);
                                                builder
                                                    .set_name(format!("{name} lifecycle region"));
                                                builder.set_bounds(accesskit_rect(
                                                    _lifecycle_region.response.rect,
                                                ));
                                            },
                                        );
                                    });
                                },
                            );
                            ui.allocate_ui_with_layout(
                                Vec2::new(details_width, 0.0),
                                Layout::left_to_right(Align::Center),
                                |ui| {
                                    activation_exclusions.push(
                                        render_details(ui, &mut action, &mut focus_within).rect,
                                    )
                                },
                            );
                        },
                    );
                });
            });
        }
        // The summary establishes the card width. Keep the divider and inline
        // details inside that measured width instead of letting expansion
        // consume the route's remaining horizontal space.
        ui.shrink_width_to_current();
        if expanded {
            ui.scope(|ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                render_model_layout_gap(ui, name, "gap before divider", 6.0);
                let _divider = ui.separator();
                #[cfg(test)]
                register_model_layout_rect(ui, name, "divider", _divider.rect);
                render_model_layout_gap(ui, name, "gap after divider", 6.0);
                restored_remove_focus |= render_inline_model_details(
                    ui,
                    card,
                    can_replace_active,
                    restore_remove_focus,
                    &mut focus_within,
                    &mut action,
                    &mut activation_exclusions,
                );
            });
        }
    }
    let frame = {
        let response = prepared.allocate_space(ui);
        if let Some(activation) = &activation {
            let activation_has_focus = activation.has_focus();
            focus_within |= activation_has_focus;
            ui.ctx().accesskit_node_builder(activation.id, |builder| {
                builder.set_role(egui::accesskit::Role::Button);
                builder.set_name(format!("Use {name} for future transcriptions"));
                builder.set_bounds(accesskit_rect(response.rect));
                builder.set_default_action_verb(egui::accesskit::DefaultActionVerb::Click);
                builder.add_action(egui::accesskit::Action::Default);
            });

            let point_activates_card = |point: egui::Pos2| {
                response.rect.contains(point)
                    && !activation_exclusions
                        .iter()
                        .any(|rect| rect.contains(point))
            };
            let (
                primary_pressed,
                primary_released,
                primary_clicked,
                press_origin,
                release_position,
                keyboard_activation,
                accesskit_activation,
            ) = ui.input(|input| {
                (
                    input.pointer.primary_pressed(),
                    input.pointer.primary_released(),
                    input.pointer.button_clicked(egui::PointerButton::Primary),
                    input.pointer.press_origin(),
                    input.pointer.interact_pos(),
                    activation_has_focus
                        && (input.key_pressed(egui::Key::Enter)
                            || input.key_pressed(egui::Key::Space)),
                    input.has_accesskit_action_request(
                        activation.id,
                        egui::accesskit::Action::Default,
                    ),
                )
            });
            if primary_pressed {
                let started_on_card = press_origin.is_some_and(point_activates_card);
                ui.data_mut(|data| {
                    if started_on_card {
                        data.insert_temp(activation_press_id, true);
                    } else {
                        data.remove::<bool>(activation_press_id);
                    }
                });
            }
            let pointer_activation = if primary_released {
                let started_on_card = ui
                    .data_mut(|data| data.remove_temp::<bool>(activation_press_id))
                    .unwrap_or(false);
                started_on_card
                    && primary_clicked
                    && release_position.is_some_and(point_activates_card)
            } else {
                false
            };
            if (pointer_activation || keyboard_activation || accesskit_activation)
                && action == ScreenAction::None
            {
                activation.request_focus();
                focus_within = true;
                action = ScreenAction::SelectModel(match card {
                    ModelCard::Local(model) => model.id.clone(),
                    ModelCard::Remote(_, _) => unreachable!("only local cards select"),
                });
            }
        }
        let hovered =
            response.hovered() || activation.as_ref().is_some_and(egui::Response::hovered);
        let state = if hovered || focus_within {
            ModelCardVisualState::Active
        } else {
            ModelCardVisualState::Idle
        };
        let (fill, stroke, shadow) = model_card_visual_style(colors, state);
        prepared.frame.fill = fill;
        prepared.frame.stroke = stroke;
        prepared.frame.shadow = shadow;
        prepared.paint(ui);
        if let Some(rect) = description_fade_rect {
            paint_description_fade(ui, rect, fill);
        }
        response
    };
    ui.ctx().accesskit_node_builder(frame.id, |builder| {
        builder.set_role(egui::accesskit::Role::Group);
        builder.set_name(format!("{name} model"));
        if let Some(progress) = model_download_progress_presentation(card) {
            builder.set_description(progress.accessible_text);
        }
        builder.set_bounds(accesskit_rect(frame.rect));
    });
    ModelCardRenderResult {
        action,
        restored_remove_focus,
    }
}

fn render_inline_model_details(
    ui: &mut egui::Ui,
    card: ModelCard<'_>,
    can_replace_active: bool,
    restore_remove_focus: bool,
    focus_within: &mut bool,
    action: &mut ScreenAction,
    activation_exclusions: &mut Vec<egui::Rect>,
) -> bool {
    let mut restored_remove_focus = false;
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        let colors = ui_palette(ui);
        let model_name = match card {
            ModelCard::Local(model) => model.display_name.as_str(),
            ModelCard::Remote(entry, _) => entry.display_name.as_str(),
        };
        let _features_heading = detail_heading(ui, "FEATURES", colors);
        #[cfg(test)]
        register_model_layout_rect(ui, model_name, "features heading", _features_heading.rect);
        render_model_layout_gap(ui, model_name, "features heading content gap", 6.0);
        let _features = render_expanded_model_features(ui, card, model_name);
        #[cfg(test)]
        register_model_layout_rect(ui, model_name, "features content", _features.rect);
        render_model_layout_gap(ui, model_name, "features requirements gap", 12.0);
        let _requirements_heading = detail_heading(ui, "REQUIREMENTS", colors);
        #[cfg(test)]
        register_model_layout_rect(
            ui,
            model_name,
            "requirements heading",
            _requirements_heading.rect,
        );
        render_model_layout_gap(ui, model_name, "requirements heading content gap", 6.0);
        let _requirements = render_model_requirement_cells(ui, model_requirement_cells(card));
        #[cfg(test)]
        register_model_layout_rect(ui, model_name, "requirements content", _requirements.rect);
        match card {
            ModelCard::Local(model) => {
                let maintenance = model.runtime_action_label.is_some()
                    || model.partial_cleanup_available
                    || model.legacy_cleanup_pending;
                if maintenance {
                    render_model_layout_gap(ui, model_name, "requirements maintenance gap", 12.0);
                    let _maintenance_heading = detail_heading(ui, "MAINTENANCE", colors);
                    #[cfg(test)]
                    register_model_layout_rect(
                        ui,
                        model_name,
                        "maintenance heading",
                        _maintenance_heading.rect,
                    );
                    render_model_layout_gap(ui, model_name, "maintenance heading content gap", 6.0);
                }
                if let Some(label) = model.runtime_action_label.as_deref() {
                    let runtime_name = format!("{label} runtime for {}", model.display_name);
                    let response = model_lifecycle_button(
                        ui,
                        label,
                        &runtime_name,
                        model.runtime_action_enabled,
                        model.runtime_action_disabled_reason.as_deref(),
                        ModelLifecycleTone::Standard,
                    );
                    *focus_within |= response.has_focus();
                    activation_exclusions.push(response.rect);
                    if response.clicked() && model.runtime_action_enabled {
                        *action = ScreenAction::MaintainModelRuntime(model.id.clone());
                    }
                }
                if model.partial_cleanup_available {
                    let cleanup_name = format!("Discard partial for {}", model.display_name);
                    let cleanup = model_lifecycle_button(
                        ui,
                        "Discard partial",
                        &cleanup_name,
                        model.partial_cleanup_enabled,
                        model.partial_cleanup_disabled_reason.as_deref(),
                        ModelLifecycleTone::Standard,
                    );
                    *focus_within |= cleanup.has_focus();
                    activation_exclusions.push(cleanup.rect);
                    if cleanup.clicked() && model.partial_cleanup_enabled {
                        *action = ScreenAction::DiscardModelPartial(model.id.clone());
                    }
                }
                if model.legacy_cleanup_pending {
                    let removal_reason = (!model.removal_supported)
                        .then_some(
                            "This model is not an app-managed download and cannot be removed here.",
                        )
                        .or_else(|| {
                            (model.selected && !model.legacy_cleanup_pending && !can_replace_active)
                                .then_some(
                                "Install another ready model before removing the selected model.",
                            )
                        });
                    let removal_name = format!("Delete {}", model.display_name);
                    let removal = model_lifecycle_button(
                        ui,
                        "Delete",
                        &removal_name,
                        removal_reason.is_none(),
                        removal_reason,
                        ModelLifecycleTone::DestructiveOutline,
                    );
                    *focus_within |= removal.has_focus();
                    activation_exclusions.push(removal.rect);
                    if restore_remove_focus {
                        removal.request_focus();
                        restored_remove_focus = true;
                    }
                    if removal.clicked() && removal_reason.is_none() {
                        *action = ScreenAction::RequestModelRemoval(model.id.clone());
                    }
                }
            }
            ModelCard::Remote(entry, variant) => {
                if let Some(cleanup) = variant.actions.iter().find(|candidate| {
                    matches!(
                        candidate.kind,
                        RemoteCatalogActionKind::DiscardPartial { .. }
                    )
                }) {
                    render_model_layout_gap(ui, model_name, "requirements maintenance gap", 12.0);
                    let _maintenance_heading = detail_heading(ui, "MAINTENANCE", colors);
                    #[cfg(test)]
                    register_model_layout_rect(
                        ui,
                        model_name,
                        "maintenance heading",
                        _maintenance_heading.rect,
                    );
                    render_model_layout_gap(ui, model_name, "maintenance heading content gap", 6.0);
                    let cleanup_name = format!("Discard partial for {}", entry.display_name);
                    let response = model_lifecycle_button(
                        ui,
                        "Discard partial",
                        &cleanup_name,
                        cleanup.enabled,
                        cleanup.disabled_reason.as_deref(),
                        ModelLifecycleTone::Standard,
                    );
                    *focus_within |= response.has_focus();
                    activation_exclusions.push(response.rect);
                    if response.clicked() && cleanup.enabled {
                        *action = screen_action_for_remote_catalog_action(&cleanup.kind);
                    }
                }
            }
        }
    });
    restored_remove_focus
}

fn detail_heading(
    ui: &mut egui::Ui,
    text: &str,
    colors: super::theme::ThemePalette,
) -> egui::Response {
    ui.label(
        RichText::new(text)
            .small()
            .strong()
            .color(colors.muted_text),
    )
}

fn render_model_layout_gap(
    ui: &mut egui::Ui,
    _model_name: &str,
    _diagnostic_name: &str,
    height: f32,
) {
    let (_rect, _) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), height), Sense::hover());
    #[cfg(test)]
    register_model_layout_rect(ui, _model_name, _diagnostic_name, _rect);
}

#[cfg(test)]
fn register_model_layout_rect(
    ui: &egui::Ui,
    model_name: &str,
    diagnostic_name: &str,
    rect: egui::Rect,
) {
    ui.ctx().accesskit_node_builder(
        ui.make_persistent_id(("model-layout-diagnostic", model_name, diagnostic_name)),
        |builder| {
            builder.set_role(egui::accesskit::Role::StaticText);
            builder.set_name(format!("{model_name} layout {diagnostic_name}"));
            builder.set_bounds(accesskit_rect(rect));
        },
    );
}

fn merge_model_action(action: &mut ScreenAction, candidate: ScreenAction) {
    if candidate == ScreenAction::None {
        return;
    }
    *action = candidate;
}

fn render_model_section(
    ui: &mut egui::Ui,
    name: &'static str,
    cards: &[ModelCard<'_>],
    expanded: bool,
    toggle_action: Option<ScreenAction>,
    focus: ModelSectionFocus,
    _terminal: bool,
) -> (ScreenAction, bool) {
    let colors = ui_palette(ui);
    let toggle_enabled = toggle_action.is_some();
    let (header_rect, header) = ui.allocate_exact_size(
        Vec2::new(ui.available_width(), 44.0),
        if toggle_enabled {
            Sense::click()
        } else {
            Sense::hover()
        },
    );
    if header.hovered() {
        ui.painter()
            .rect_filled(header_rect, Rounding::same(5.0), colors.active_card_bg);
    }
    ui.allocate_ui_at_rect(header_rect.shrink2(Vec2::new(4.0, 0.0)), |ui| {
        ui.horizontal(|ui| {
            ui.label(RichText::new(format!("{name} ({})", cards.len())).strong());
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(
                    RichText::new(icon_glyph(if expanded {
                        Icon::ChevronUp
                    } else {
                        Icon::ChevronDown
                    }))
                    .color(colors.text),
                );
            });
        });
    });
    ui.ctx().accesskit_node_builder(header.id, |builder| {
        builder.set_role(egui::accesskit::Role::Button);
        if toggle_enabled {
            builder.set_name(format!(
                "{} {name} models",
                if expanded { "Collapse" } else { "Expand" }
            ));
        } else {
            builder.set_name(format!("{name} models expanded for search"));
            builder.set_disabled();
            builder.set_description("Clear the search to restore this section's saved state.");
        }
        builder.set_expanded(expanded);
        builder.set_bounds(accesskit_rect(header_rect));
    });
    paint_focus_ring(ui, &header, Rounding::same(5.0));
    scroll_focused_control_into_view(ui, &header);
    let mut action = if header.clicked() {
        toggle_action.unwrap_or(ScreenAction::None)
    } else {
        ScreenAction::None
    };
    if !expanded || cards.is_empty() {
        return (action, false);
    }

    let mut restored_remove_focus = false;
    ui.add_space(MODEL_CARD_GAP);
    for (index, card) in cards.iter().copied().enumerate() {
        let expanded = focus.expanded.is_some_and(|key| card.matches_key(key));
        let restore_remove_focus = matches!(
            card,
            ModelCard::Local(model)
                if focus.restore_remove_focus.is_some_and(|id| id == model.id)
        );
        let rendered = ui
            .push_id(("model-card", card.key()), |ui| {
                render_unified_model_card(
                    ui,
                    card,
                    expanded,
                    focus.can_replace_active,
                    restore_remove_focus,
                )
            })
            .inner;
        merge_model_action(&mut action, rendered.action);
        restored_remove_focus |= rendered.restored_remove_focus;
        if index + 1 < cards.len() {
            ui.add_space(MODEL_CARD_GAP);
        }
    }
    (action, restored_remove_focus)
}

fn models(
    ui: &mut egui::Ui,
    models: &[ModelViewModel],
    model_catalog: &[ModelViewModel],
    comparison: &ModelComparisonState,
    management: &ModelManagementState,
    language_filter: ModelLanguageFilter,
    remote_catalog: &RemoteCatalogView,
) -> ScreenAction {
    // A native scroll viewport can reserve space that is not reflected in the
    // inherited layout width. Bound this route to the actually paintable area
    // so card frames and trailing controls keep their right inset.
    ui.set_width(current_content_width(ui));
    let colors = ui_palette(ui);
    let mut action = ScreenAction::None;
    let mut import_control = None;
    let mut restored_remove_focus = false;
    let dialog_active = management.dialog.is_some();
    let restore_remove_target_gone = management
        .restore_remove_focus
        .as_deref()
        .is_some_and(|id| !models.iter().any(|model| model.installed && model.id == id));
    #[cfg(test)]
    ui.data_mut(|data| {
        data.remove::<egui::Rect>(egui::Id::new("models-final-card-rect"));
    });
    ui.add_enabled_ui(!dialog_active, |ui| {
    let response = ui.label(RichText::new("Models").size(30.0).strong());
    ui.ctx().accesskit_node_builder(response.id, |builder| {
        builder.set_role(egui::accesskit::Role::Heading);
        builder.set_bounds(accesskit_rect(response.rect));
    });
    ui.label(
        RichText::new("Manage the speech models available on this device.")
            .color(colors.muted_text),
    );
    ui.add_space(18.0);
    let mut query = remote_catalog.query.clone();
    let search = ui.add_sized(
        [ui.available_width(), 44.0],
        egui::TextEdit::singleline(&mut query)
            .id_source("models-search")
            .hint_text("Search models by name, language, or variant"),
    );
    ui.ctx().accesskit_node_builder(search.id, |builder| builder.set_name("Search models"));
    if search.changed() {
        action = ScreenAction::SetRemoteCatalogQuery(query);
    }
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        let mut selected = language_filter;
        let combo = ComboBox::from_id_source("models-language")
            .selected_text(format!("{}  {}", icon_glyph(Icon::Globe), selected.label()))
            .width(156.0)
            .show_ui(ui, |ui| {
                for value in ModelLanguageFilter::ALL {
                    ui.selectable_value(&mut selected, value, value.label());
                }
            });
        ui.ctx().accesskit_node_builder(combo.response.id, |builder| builder.set_name("Filter model languages"));
        if selected != language_filter {
            action = ScreenAction::SetModelLanguageFilter(selected);
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let import = compact_model_icon_action(ui, Icon::Plus, "Import local GGUF", true, None, None);
            if management.restore_add_focus
                || management.restore_after_removal_focus
                || restore_remove_target_gone
            {
                import.request_focus();
            }
            import_control = Some(import.clone());
            if import.clicked() {
                action = ScreenAction::AddModel;
            }
            let refresh = compact_model_icon_action(
                ui,
                Icon::Refresh,
                "Refresh trusted model catalog",
                remote_catalog.refresh_enabled,
                (!remote_catalog.refresh_enabled).then_some("The catalog is already refreshing."),
                None,
            );
            if refresh.clicked() && remote_catalog.refresh_enabled {
                action = ScreenAction::RetryRemoteCatalog;
            }
        });
    });
    ui.add_space(8.0);
    let status = ui.label(RichText::new(&remote_catalog.status.message).color(
        match remote_catalog.status.kind {
            RemoteCatalogStatusKind::Error => colors.error,
            RemoteCatalogStatusKind::Offline => colors.warning,
            _ => colors.muted_text,
        },
    ));
    ui.ctx().accesskit_node_builder(status.id, |builder| {
        builder.set_role(egui::accesskit::Role::Status);
        builder.set_live(egui::accesskit::Live::Polite);
        builder.set_live_atomic();
    });
    let (mut installed_cards, available_cards) = build_model_card_lists(
        models,
        model_catalog,
        remote_catalog,
        language_filter,
    );
    if let Some(restore_id) = management.restore_remove_focus.as_deref()
        && !installed_cards.iter().any(
            |card| matches!(card, ModelCard::Local(model) if model.id == restore_id),
        )
        && let Some(model) = models
            .iter()
            .find(|model| model.installed && model.id == restore_id)
    {
        installed_cards.insert(0, ModelCard::Local(model));
    }
    let can_replace_active = models
        .iter()
        .filter(|model| model.installed && model.ready)
        .count()
        > 1;
    let search_active = !remote_catalog.query.trim().is_empty();
    let comparison_viewport = ui
        .data(|data| data.get_temp::<egui::Rect>(egui::Id::new(("route-viewport", UiRoute::Models))))
        .unwrap_or_else(|| ui.clip_rect());
    let comparison_max_height = if comparison.expanded {
        comparison_viewport.height() * 0.6
    } else {
        MODEL_COMPARISON_COLLAPSED_HEIGHT
    };
    let result_count = ui.label(
        RichText::new(format!(
            "{} model results: {} installed, {} available.",
            installed_cards.len() + available_cards.len(),
            installed_cards.len(),
            available_cards.len()
        ))
        .small()
        .color(colors.muted_text),
    );
    ui.ctx()
        .accesskit_node_builder(result_count.id, |builder| {
            builder.set_role(egui::accesskit::Role::Status);
            builder.set_live(egui::accesskit::Live::Polite);
            builder.set_live_atomic();
        });
    ui.add_space(12.0);
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        let (installed_action, restored_installed_focus) = render_model_section(
            ui,
            "Installed",
            &installed_cards,
            management.installed_expanded
                || search_active
                || management.restore_remove_focus.is_some(),
            (!search_active).then_some(ScreenAction::ToggleInstalledModels),
            ModelSectionFocus {
                expanded: management.expanded_model_card.as_ref(),
                can_replace_active,
                restore_remove_focus: management.restore_remove_focus.as_deref(),
            },
            available_cards.is_empty(),
        );
        merge_model_action(&mut action, installed_action);
        ui.add_space(12.0);
        let (available_action, restored_available_focus) = render_model_section(
            ui,
            "Available",
            &available_cards,
            management.available_expanded || search_active,
            (!search_active).then_some(ScreenAction::ToggleAvailableModels),
            ModelSectionFocus {
                expanded: management.expanded_model_card.as_ref(),
                can_replace_active,
                restore_remove_focus: management.restore_remove_focus.as_deref(),
            },
            true,
        );
        merge_model_action(&mut action, available_action);
        restored_remove_focus = restored_installed_focus || restored_available_focus;
    });
    let comparison_width =
        (comparison_viewport.width() - ROUTE_HORIZONTAL_INSET * 2.0).max(0.0);
    let comparison_surface_id = ui.make_persistent_id("model-comparison-surface");
    let comparison_ctx = ui.ctx().clone();
    comparison_ctx.accesskit_node_builder(comparison_surface_id, |builder| {
        builder.set_role(egui::accesskit::Role::Group);
        builder.set_name("Model comparison surface");
    });
    let mut comparison_surface_rect = None;
    egui::Area::new(ui.make_persistent_id("model-comparison-dock"))
        .order(if dialog_active {
            egui::Order::Middle
        } else {
            egui::Order::Foreground
        })
        .fixed_pos(egui::pos2(
            comparison_viewport.left() + ROUTE_HORIZONTAL_INSET,
            comparison_viewport.bottom()
                - MODEL_COMPARISON_BOTTOM_GAP
                - comparison_max_height,
        ))
        .movable(false)
        .interactable(!dialog_active)
        .show(ui.ctx(), |dock_ui| {
            dock_ui.set_enabled(!dialog_active);
            dock_ui.set_width(comparison_width);
            dock_ui.allocate_ui_with_layout(
                Vec2::new(comparison_width, comparison_max_height),
                Layout::top_down(Align::LEFT),
                |ui| comparison_ctx.with_accessibility_parent(comparison_surface_id, || {
        let comparison_surface = Frame::none()
            .fill(colors.card_bg)
            .stroke(Stroke::new(1.0, colors.border))
            .rounding(Rounding::same(5.0))
            .inner_margin(Margin::same(16.0))
            .show(ui, |ui| {
            ui.set_width((comparison_width - 32.0).max(0.0));
            ui.set_min_height((comparison_max_height - 32.0).max(0.0));
            let toggle_name = if comparison.expanded {
                "Collapse comparison"
            } else {
                "Expand comparison"
            };
            let header_content = ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.label(RichText::new("Compare installed models").strong());
                    ui.label(
                        RichText::new("Comparison measures speed and output on this computer.")
                            .color(colors.muted_text),
                    );
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.label(
                        RichText::new(icon_glyph(if comparison.expanded {
                            Icon::ChevronUp
                        } else {
                            Icon::ChevronDown
                        }))
                        .color(colors.text),
                    );
                });
            });
            let header = ui.interact(
                egui::Rect::from_min_max(
                    header_content.response.rect.min,
                    egui::pos2(ui.max_rect().right(), header_content.response.rect.max.y),
                ),
                ui.make_persistent_id("comparison-header"),
                Sense::click(),
            );
            ui.ctx().accesskit_node_builder(header.id, |builder| {
                builder.set_role(egui::accesskit::Role::Button);
                builder.set_name(toggle_name);
                builder.set_expanded(comparison.expanded);
                if !ui.is_enabled() {
                    builder.set_disabled();
                }
            });
            if comparison.focus_panel {
                header.request_focus();
            }
            focus_tooltip(ui, &header, toggle_name);
            paint_focus_ring(ui, &header, Rounding::same(4.0));
            let header_released = ui.input(|input| {
                input.pointer.any_released()
                    && input
                        .pointer
                        .interact_pos()
                        .is_some_and(|position| header.rect.contains(position))
            });
            if header.clicked() || header_released {
                action = ScreenAction::ToggleComparison;
            }
            if comparison.expanded {
                ui.add_space(12.0);
                let body_height = ui.available_height().max(0.0);
                let _body_scroll = ScrollArea::vertical()
                    .id_source("model-comparison-body")
                    .max_height(body_height)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                let render_selection = |ui: &mut egui::Ui, action: &mut ScreenAction| {
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
                            *action = ScreenAction::ToggleComparisonModel(model.id.clone());
                        }
                    }
                };
                let render_recording_control = |ui: &mut egui::Ui, action: &mut ScreenAction| {
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
                            *action = ScreenAction::StopComparison;
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
                            *action = ScreenAction::StartComparison;
                        }
                    }
                };
                if ui.available_width() >= 720.0 {
                    let control_width = 196.0;
                    let selection_width = (ui.available_width()
                        - control_width
                        - ui.spacing().item_spacing.x)
                        .max(0.0);
                    ui.horizontal_top(|ui| {
                        ui.allocate_ui(Vec2::new(selection_width, 0.0), |ui| {
                            ui.set_min_width(selection_width);
                            ui.horizontal_wrapped(|ui| render_selection(ui, &mut action));
                        });
                        ui.allocate_ui_with_layout(
                            Vec2::new(control_width, 0.0),
                            Layout::right_to_left(Align::Center),
                            |ui| {
                                ui.set_min_width(control_width);
                                render_recording_control(ui, &mut action);
                            },
                        );
                    });
                } else {
                    ui.horizontal_wrapped(|ui| render_selection(ui, &mut action));
                    ui.add_space(8.0);
                    render_recording_control(ui, &mut action);
                }
                if let Some(feedback) = comparison.selection_feedback.as_deref() {
                    ui.label(RichText::new(feedback).small().color(colors.warning));
                }
                if let Some(notice) = comparison.reference_notice.as_deref() {
                    let response = ui.label(RichText::new(notice).small().color(colors.muted_text));
                    ui.ctx().accesskit_node_builder(response.id, |builder| {
                        builder.set_live(egui::accesskit::Live::Polite);
                        builder.set_live_atomic();
                    });
                }
                if comparison.reference_editor_visible {
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
                    if comparison.focus_reference_editor {
                        reference.request_focus();
                    }
                    let can_apply_reference = !reference_draft.trim().is_empty();
                    if reference.changed() {
                        action = ScreenAction::EditComparisonReference(reference_draft);
                    }
                    ui.horizontal(|ui| {
                        let apply_reference = ui.add_enabled(
                            can_apply_reference,
                            egui::Button::new("Apply reference")
                                .min_size(Vec2::new(0.0, 44.0)),
                        );
                        if !can_apply_reference {
                            ui.ctx().accesskit_node_builder(apply_reference.id, |builder| {
                                builder.set_disabled();
                                builder.set_description(
                                    "Enter a reference transcript before applying it.",
                                );
                            });
                        }
                        if apply_reference.clicked() {
                            action = ScreenAction::ApplyComparisonReference;
                        }
                        if ui
                            .add_sized(Vec2::new(0.0, 44.0), egui::Button::new("Clear reference"))
                            .clicked()
                        {
                            action = ScreenAction::ClearComparisonReference;
                        }
                        if button(ui, "Close reference editor", ButtonTone::Text).clicked() {
                            action = ScreenAction::HideComparisonReferenceEditor;
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
                } else if comparison.reference_transcript.is_some() {
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new("Reference applied")
                                .small()
                                .color(colors.muted_text),
                        );
                        let edit_reference = button(ui, "Edit reference", ButtonTone::Text);
                        if comparison.restore_reference_action_focus {
                            edit_reference.request_focus();
                        }
                        if edit_reference.clicked() {
                            action = ScreenAction::ShowComparisonReferenceEditor;
                        }
                    });
                }
                ui.separator();
                let result_action = render_comparison_results(ui, models, comparison);
                if result_action != ScreenAction::None {
                    action = result_action;
                }
                if let Some((id, rect)) = ui.data(|data| {
                    data.get_temp::<(egui::Id, egui::Rect)>(egui::Id::new(
                        COMPARISON_BODY_FOCUSED_CONTROL_SCROLL,
                    ))
                }) && ui.ctx().memory(|memory| memory.focused()) == Some(id)
                {
                    ui.scroll_to_rect(rect, Some(Align::Center));
                }
                    });
                #[cfg(test)]
                ui.data_mut(|data| {
                    data.insert_temp(
                        egui::Id::new(COMPARISON_BODY_SCROLL_DIAGNOSTICS),
                        (
                            _body_scroll.id,
                            _body_scroll.state.offset,
                            _body_scroll.content_size,
                            _body_scroll.inner_rect,
                        ),
                    );
                });
            }
        });
        comparison_surface_rect = Some(comparison_surface.response.rect);
                }),
            );
        });
    let comparison_surface_rect = comparison_surface_rect.expect("comparison surface is rendered");
    ui.ctx()
        .accesskit_node_builder(comparison_surface_id, |builder| {
            builder.set_bounds(accesskit_rect(comparison_surface_rect));
        });
    let terminal_spacer_height = comparison_surface_rect.height()
        + MODEL_LIST_TO_DOCK_GAP
        + MODEL_COMPARISON_BOTTOM_GAP
        - ui.spacing().item_spacing.y;
    #[cfg(test)]
    ui.data_mut(|data| {
        data.insert_temp(
            egui::Id::new("models-layout-diagnostics"),
            (
                comparison_viewport,
                comparison_surface_rect,
                ui.cursor().top(),
                terminal_spacer_height,
            ),
        );
    });
    ui.add_space(terminal_spacer_height);
    #[cfg(test)]
    ui.data_mut(|data| {
        data.insert_temp(
            egui::Id::new("models-comparison-dock-rect"),
            comparison_surface_rect,
        );
    });
    });
    if management.restore_remove_focus.is_some()
        && (restored_remove_focus || restore_remove_target_gone)
        && action == ScreenAction::None
    {
        action = ScreenAction::AcknowledgeModelRemovalFocus;
    }
    // The disabled scope above is the primary boundary for both pointer and
    // assistive actions. Keep this guard as a defense in depth measure for
    // custom controls or future widgets that might still report a response.
    if dialog_active {
        action = ScreenAction::None;
        model_dialog_interaction_shield(ui.ctx());
    }
    let dialog_tab_direction = management
        .dialog
        .as_ref()
        .and_then(|_| consume_model_dialog_tab(ui.ctx()));
    if management.dialog.is_some() && ui.input(|input| input.key_pressed(egui::Key::Escape)) {
        return model_dialog_dismiss_action(management, remote_catalog);
    }
    match &management.dialog {
        Some(ModelDialog::Add) => {
            let mut focusable_controls = Vec::new();
            let mut initial_focus = None;
            let dialog_accessibility_id =
                ui.make_persistent_id(("model-dialog-accessibility", "add"));
            let dialog_ctx = ui.ctx().clone();
            dialog_ctx.accesskit_node_builder(dialog_accessibility_id, |builder| {
                builder.set_role(egui::accesskit::Role::Dialog);
                builder.set_name("Import local GGUF");
                builder.set_modal();
            });
            let mut dialog = None;
            dialog_ctx.with_accessibility_parent(dialog_accessibility_id, || {
                dialog = Some(egui::Area::new(ui.make_persistent_id(("model-dialog", "add")))
                    .order(egui::Order::Foreground)
                    .enabled(true)
                    .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
                    .movable(false)
                    .show(ui.ctx(), |ui| {
                    Frame::window(ui.style()).show(ui, |ui| {
                    ui.set_width(480.0);
                    ui.label(RichText::new("Import local GGUF").heading());
                    ui.add_space(8.0);
                    ui.set_enabled(true);
                    ui.label(
                        RichText::new(
                            "Validate an existing .gguf file in place. Scribe never copies, uploads, or deletes the source file.",
                        )
                        .color(colors.muted_text),
                    );
                    ui.add_space(10.0);
                    let label = ui.label(RichText::new("GGUF file path").strong());
                    let mut path = remote_catalog.local_import.path.clone();
                    let path_input = ui
                        .add_enabled_ui(!remote_catalog.local_import.in_progress, |ui| {
                            ui.add(
                                egui::TextEdit::singleline(&mut path)
                                    .id_source("local-gguf-import-path")
                                    .hint_text("C:\\Models\\model.gguf")
                                    .desired_width(440.0),
                            )
                        })
                        .inner;
                    path_input.clone().labelled_by(label.id);
                    ui.ctx().accesskit_node_builder(path_input.id, |builder| {
                        builder.set_name("GGUF file path");
                    });
                    if !remote_catalog.local_import.in_progress {
                        initial_focus = Some(path_input.id);
                        focusable_controls.push(path_input.id);
                        mark_accesskit_enabled(ui, &path_input);
                    }
                    if path_input.changed() {
                        action = ScreenAction::SetLocalGgufImportPath(path);
                    }
                    if let Some(reason) = remote_catalog.local_import.disabled_reason.as_deref() {
                        ui.label(RichText::new(reason).small().color(colors.warning));
                    }
                    if let Some(message) = remote_catalog.local_import.status_message.as_deref() {
                        let status = ui
                            .push_id("local-gguf-import-status", |ui| {
                                ui.label(
                                    RichText::new(message)
                                        .small()
                                        .color(colors.muted_text),
                                )
                            })
                            .inner;
                        ui.ctx().accesskit_node_builder(status.id, |builder| {
                            builder.set_role(egui::accesskit::Role::Status);
                            builder.set_live(egui::accesskit::Live::Polite);
                            builder.set_live_atomic();
                        });
                    }
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        if remote_catalog.local_import.in_progress {
                            let progress = ui.add(egui::ProgressBar::new(0.0).animate(true).text("Validating local file"));
                            ui.ctx().accesskit_node_builder(progress.id, |builder| {
                                builder.set_role(egui::accesskit::Role::ProgressIndicator);
                                builder.set_name("Local GGUF validation progress");
                            });
                            let cancel = button(ui, "Cancel validation", ButtonTone::Secondary);
                            initial_focus.get_or_insert(cancel.id);
                            focusable_controls.push(cancel.id);
                            mark_accesskit_enabled(ui, &cancel);
                            if cancel.clicked() {
                                action = ScreenAction::CancelLocalGgufImport;
                            }
                        } else {
                            let import = ui
                                .add_enabled_ui(remote_catalog.local_import.import_enabled, |ui| {
                                    button(ui, "Validate and import", ButtonTone::Primary)
                                })
                                .inner;
                            if remote_catalog.local_import.import_enabled {
                                focusable_controls.push(import.id);
                                mark_accesskit_enabled(ui, &import);
                            } else {
                                ui.ctx().accesskit_node_builder(import.id, |builder| {
                                    builder.set_disabled();
                                    if let Some(reason) =
                                        remote_catalog.local_import.disabled_reason.as_deref()
                                    {
                                        builder.set_description(reason);
                                    }
                                });
                            }
                            if import.clicked() {
                                action = ScreenAction::ValidateAndImportLocalGguf;
                            }
                        }
                    });
                    ui.add_space(8.0);
                    let close = button(ui, "Close", ButtonTone::Secondary);
                    initial_focus.get_or_insert(close.id);
                    focusable_controls.push(close.id);
                    mark_accesskit_enabled(ui, &close);
                    if close.clicked() {
                        action = model_dialog_dismiss_action(management, remote_catalog);
                    }
                    });
                    }));
            });
            contain_model_dialog_focus(
                ui.ctx(),
                dialog_tab_direction,
                &focusable_controls,
                initial_focus,
                management.focus_dialog_initial,
            );
            if let Some(dialog) = dialog {
                ui.ctx()
                    .accesskit_node_builder(dialog_accessibility_id, |builder| {
                        builder.set_bounds(accesskit_rect(dialog.response.rect));
                    });
            }
        }
        Some(ModelDialog::Remove(id)) => {
            if let Some(model) = models
                .iter()
                .chain(model_catalog.iter())
                .find(|model| &model.id == id)
            {
                let mut focusable_controls = Vec::new();
                let mut initial_focus = None;
                let dialog_accessibility_id =
                    ui.make_persistent_id(("model-dialog-accessibility", "remove", id));
                let dialog_ctx = ui.ctx().clone();
                dialog_ctx.accesskit_node_builder(dialog_accessibility_id, |builder| {
                    builder.set_role(egui::accesskit::Role::AlertDialog);
                    builder.set_name(format!("Delete {}", model.display_name));
                    builder.set_modal();
                });
                let mut dialog = None;
                dialog_ctx.with_accessibility_parent(dialog_accessibility_id, || {
                    dialog = Some(egui::Area::new(ui.make_persistent_id(("model-dialog", "remove", id)))
                        .order(egui::Order::Foreground)
                        .enabled(true)
                        .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
                        .movable(false)
                        .show(ui.ctx(), |ui| {
                        Frame::window(ui.style()).show(ui, |ui| {
                        ui.set_width(440.0);
                        ui.label(RichText::new("Delete model?").heading());
                        ui.add_space(8.0);
                        ui.set_enabled(true);
                        ui.label(format!("Delete {} from Scribe?", model.display_name));
                        if let Some(replacement_id) = management.removal_replacement.as_deref()
                            && let Some(replacement) = models.iter().find(|candidate| candidate.id == replacement_id)
                        {
                            ui.label(format!(
                                "Scribe will use {} for future transcriptions before removing this model.",
                                replacement.display_name
                            ));
                        }
                        ui.label(RichText::new("Only Scribe-managed artifact files are removed. This cannot be undone.").color(colors.warning));
                        ui.horizontal(|ui| {
                            let cancel = button(ui, "Cancel", ButtonTone::Secondary);
                            initial_focus = Some(cancel.id);
                            focusable_controls.push(cancel.id);
                            mark_accesskit_enabled(ui, &cancel);
                            if cancel.clicked() { action = ScreenAction::CloseModelDialog; }
                            let remove_label = management
                                .removal_replacement
                                .as_deref()
                                .and_then(|replacement_id| models.iter().find(|candidate| candidate.id == replacement_id))
                                .map_or_else(|| "Delete".to_owned(), |replacement| format!("Use {} and delete", replacement.display_name));
                            let remove = button(ui, remove_label, ButtonTone::Danger);
                            focusable_controls.push(remove.id);
                            mark_accesskit_enabled(ui, &remove);
                            if remove.clicked() { action = ScreenAction::ConfirmModelRemoval(model.id.clone()); }
                        });
                        });
                        }));
                });
                contain_model_dialog_focus(
                    ui.ctx(),
                    dialog_tab_direction,
                    &focusable_controls,
                    initial_focus,
                    management.focus_dialog_initial,
                );
                if let Some(dialog) = dialog {
                    ui.ctx()
                        .accesskit_node_builder(dialog_accessibility_id, |builder| {
                            builder.set_bounds(accesskit_rect(dialog.response.rect));
                        });
                }
            }
        }
        None => {}
    }
    action
}

fn model_download_label(model: &ModelViewModel) -> String {
    match model.download_state {
        ModelDownloadState::Downloading => {
            model_download_progress_presentation(ModelCard::Local(model)).map_or_else(
                || "Downloading".to_owned(),
                |progress| progress.display_text,
            )
        }
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

#[derive(Clone, Debug, PartialEq)]
struct ModelDownloadProgressPresentation {
    downloaded_bytes: u64,
    total_bytes: Option<u64>,
    fraction: Option<f32>,
    total_is_unknown: bool,
    display_text: String,
    accessible_text: String,
}

fn model_download_progress_presentation(
    card: ModelCard<'_>,
) -> Option<ModelDownloadProgressPresentation> {
    let (downloaded_bytes, total_bytes) = match card {
        ModelCard::Local(model) if model.download_state == ModelDownloadState::Downloading => {
            (model.downloaded_bytes, model.total_bytes)
        }
        // Remote progress exists only when the live installer supplied it.
        ModelCard::Remote(_, variant) => (variant.downloaded_bytes?, variant.total_bytes),
        _ => return None,
    };
    let total_bytes = total_bytes.filter(|total| *total > 0);
    let fraction =
        total_bytes.map(|total| (downloaded_bytes as f64 / total as f64).clamp(0.0, 1.0) as f32);
    let (display_text, accessible_text) = match total_bytes {
        Some(total) => {
            let percent = fraction.expect("known totals always have a fraction") * 100.0;
            (
                format!(
                    "Downloading {} of {} ({percent:.0}%)",
                    format_download_bytes(downloaded_bytes),
                    format_download_bytes(total)
                ),
                format!(
                    "Downloading {} of {}, {percent:.0}% complete",
                    format_download_bytes(downloaded_bytes),
                    format_download_bytes(total)
                ),
            )
        }
        None => (
            format!("Downloading {}", format_download_bytes(downloaded_bytes)),
            format!(
                "Downloading {}; total download size unknown",
                format_download_bytes(downloaded_bytes)
            ),
        ),
    };
    Some(ModelDownloadProgressPresentation {
        downloaded_bytes,
        total_bytes,
        fraction,
        total_is_unknown: total_bytes.is_none(),
        display_text,
        accessible_text,
    })
}

/// Consume Tab before egui's document-wide navigation sees it, then route it
/// through the current dialog's enabled controls after they have been rendered.
/// egui 0.27 exposes individual focus requests but has no modal focus scope.
fn consume_model_dialog_tab(ctx: &egui::Context) -> Option<bool> {
    ctx.input_mut(|input| {
        if input.consume_key(egui::Modifiers::SHIFT, egui::Key::Tab) {
            Some(true)
        } else if input.consume_key(egui::Modifiers::NONE, egui::Key::Tab) {
            Some(false)
        } else {
            None
        }
    })
}

/// Window contents are rendered after the disabled Models surface, so their
/// AccessKit nodes need an explicit enabled state even though their interaction
/// remains active. Only call this for controls in the dialog's enabled ring.
fn mark_accesskit_enabled(ui: &egui::Ui, response: &egui::Response) {
    ui.ctx().accesskit_node_builder(response.id, |builder| {
        builder.clear_disabled();
    });
}

/// Keep keyboard focus in a model dialog, including wraparound at either end.
/// `controls` is assembled from the controls that are visible and enabled in
/// the current frame, so conditional install, cancel, and maintenance actions
/// cannot receive focus when unavailable.
fn contain_model_dialog_focus(
    ctx: &egui::Context,
    tab_backwards: Option<bool>,
    controls: &[egui::Id],
    initial_focus: Option<egui::Id>,
    request_initial_focus: bool,
) {
    let Some(first) = controls.first().copied() else {
        return;
    };
    let initial_focus = initial_focus.unwrap_or(first);

    if request_initial_focus {
        ctx.memory_mut(|memory| memory.request_focus(initial_focus));
        return;
    }

    let focused = ctx.memory(|memory| memory.focused());
    if focused.is_some_and(|focused| controls.contains(&focused)) {
        return;
    }

    // egui has already advanced focus through controls rendered in this frame.
    // Preserve Tab/Shift+Tab wraparound, but also repair focus that assistive
    // technology moved to the disabled background without a keyboard event.
    let target = match tab_backwards {
        Some(true) => controls.last().copied().unwrap_or(first),
        Some(false) => first,
        None => initial_focus,
    };
    ctx.memory_mut(|memory| memory.request_focus(target));
}

/// Details is primarily a pointer-invoked inspection surface. Do not move
/// focus to a visible button when it opens or closes; the first Tab press
/// starts a normal, contained drawer focus sequence instead.
/// Keep pointer input intended for the Models page below the dialog layer.
/// Keyboard focus is contained separately by `contain_model_dialog_focus`.
/// This mirrors the established
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

fn model_dialog_dismiss_action(
    management: &ModelManagementState,
    remote_catalog: &RemoteCatalogView,
) -> ScreenAction {
    if matches!(management.dialog, Some(ModelDialog::Add))
        && remote_catalog.local_import.in_progress
    {
        ScreenAction::CancelLocalGgufImport
    } else {
        ScreenAction::CloseModelDialog
    }
}

/// Render the drawer header against fixed tracks so the close target remains
/// anchored to the top-right corner even when a model name is long.
fn render_comparison_results(
    ui: &mut egui::Ui,
    models: &[ModelViewModel],
    comparison: &ModelComparisonState,
) -> ScreenAction {
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

    let mut action = ScreenAction::None;
    // Rows follow visible model order, so the first rendered Add action is the stable return target.
    let mut restore_reference_focus = comparison.restore_reference_action_focus;
    if ui.available_width() < COMPARISON_TABLE_MIN_WIDTH {
        for model in selected {
            let result = comparison
                .results
                .iter()
                .find(|(id, _)| id == &model.id)
                .map(|(_, result)| result);
            let group_width = ui.available_width();
            let group_id = ui.make_persistent_id(("compact-comparison-result", &model.id));
            let group_ctx = ui.ctx().clone();
            group_ctx.accesskit_node_builder(group_id, |builder| {
                builder.set_role(egui::accesskit::Role::Group);
                builder.set_name(format!("Comparison result for {}", model.display_name));
            });
            let mut group_rect = None;
            group_ctx.with_accessibility_parent(group_id, || {
                let group = ui.group(|ui| {
                    ui.set_width((group_width - 12.0).max(0.0));
                    ui.spacing_mut().item_spacing.y = 4.0;
                    ui.label(RichText::new(&model.display_name).strong());
                    ui.horizontal_wrapped(|ui| {
                        ui.label(format!("Status: {}", comparison_result_status(result)));
                        ui.label(format!("Duration: {}", comparison_duration(comparison)));
                        ui.label(format!(
                            "Processing time: {}",
                            comparison_processing(result)
                        ));
                    });
                    ui.label(format!("Output: {}", comparison_output_summary(result)));
                    ui.horizontal_wrapped(|ui| {
                        ui.label("Accuracy:");
                        let accuracy_action = comparison_accuracy_cell(
                            ui,
                            model,
                            comparison,
                            result,
                            &mut restore_reference_focus,
                        );
                        if accuracy_action != ScreenAction::None {
                            action = accuracy_action;
                        }
                    });
                    if let Some(rtf) = result.and_then(|result| result.realtime_factor) {
                        ui.label(RichText::new(format!("Real-time factor: {rtf:.2}x")).small());
                    }
                });
                group_rect = Some(group.response.rect);
            });
            if let Some(rect) = group_rect {
                ui.ctx().accesskit_node_builder(group_id, |builder| {
                    builder.set_bounds(accesskit_rect(rect));
                });
            }
        }
        return action;
    }

    let table_id = ui.make_persistent_id("model-comparison-results-table");
    let ctx = ui.ctx().clone();
    ctx.accesskit_node_builder(table_id, |builder| {
        builder.set_role(egui::accesskit::Role::Table);
        builder.set_name("Model comparison results");
    });
    let mut table_rect = None;
    ctx.with_accessibility_parent(table_id, || {
        let table = Frame::none()
            .fill(colors.card_bg)
            .stroke(Stroke::new(1.0, colors.border))
            .rounding(Rounding::same(3.0))
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                let widths = comparison_table_column_widths(ui.available_width());
                let header_id = ui.make_persistent_id("model-comparison-results-header");
                ui.ctx().accesskit_node_builder(header_id, |builder| {
                    builder.set_role(egui::accesskit::Role::Row);
                    builder.set_name("Comparison result columns");
                });
                let header_ctx = ui.ctx().clone();
                let mut header_rect = None;
                header_ctx.with_accessibility_parent(header_id, || {
                    let header = Frame::none()
                        .fill(colors.disabled_bg)
                        .inner_margin(Margin::symmetric(10.0, 4.0))
                        .show(ui, |ui| {
                            ui.spacing_mut().interact_size.y = 0.0;
                            ui.spacing_mut().item_spacing.x = 0.0;
                            ui.horizontal(|ui| {
                                for (width, heading) in widths.into_iter().zip([
                                    "Model",
                                    "Duration",
                                    "Processing time",
                                    "Output",
                                    "Accuracy",
                                ]) {
                                    comparison_table_cell(
                                        ui,
                                        ("comparison-header-cell", heading),
                                        width,
                                        20.0,
                                        egui::accesskit::Role::ColumnHeader,
                                        Some(heading),
                                        |ui| {
                                            ui.label(RichText::new(heading).strong().small());
                                        },
                                    );
                                }
                            });
                        });
                    header_rect = Some(header.response.rect);
                });
                if let Some(rect) = header_rect {
                    ui.ctx().accesskit_node_builder(header_id, |builder| {
                        builder.set_bounds(accesskit_rect(rect));
                    });
                }

                for model in selected {
                    comparison_table_separator(ui, colors.border);
                    let result = comparison
                        .results
                        .iter()
                        .find(|(id, _)| id == &model.id)
                        .map(|(_, result)| result);
                    let row_id = ui.make_persistent_id(("comparison-result-row", &model.id));
                    ui.ctx().accesskit_node_builder(row_id, |builder| {
                        builder.set_role(egui::accesskit::Role::Row);
                        builder.set_name(format!("Comparison result for {}", model.display_name));
                        if let Some(rtf) = result.and_then(|result| result.realtime_factor) {
                            builder.set_description(format!("Real-time factor: {rtf:.2}x"));
                        }
                    });
                    let row_ctx = ui.ctx().clone();
                    let mut row_rect = None;
                    row_ctx.with_accessibility_parent(row_id, || {
                        let row = Frame::none()
                            .inner_margin(Margin::symmetric(10.0, 0.0))
                            .show(ui, |ui| {
                                ui.spacing_mut().interact_size.y = 0.0;
                                ui.spacing_mut().item_spacing.x = 0.0;
                                ui.horizontal(|ui| {
                                    for (index, (width, content)) in widths
                                        .into_iter()
                                        .zip([
                                            comparison_model_summary(model, result),
                                            comparison_duration(comparison),
                                            comparison_processing(result),
                                        ])
                                        .enumerate()
                                    {
                                        comparison_table_cell(
                                            ui,
                                            ("comparison-result-cell", &model.id, index),
                                            width,
                                            44.0,
                                            egui::accesskit::Role::Cell,
                                            None,
                                            |ui| {
                                                let response = ui.add(
                                                    egui::Label::new(content.as_str())
                                                        .truncate(true),
                                                );
                                                if index == 0 {
                                                    response.on_hover_text(content.as_str());
                                                }
                                            },
                                        );
                                    }
                                    let output = comparison_output_summary(result);
                                    let output_cell_name =
                                        format!("Output for {}: {output}", model.display_name);
                                    comparison_table_cell(
                                        ui,
                                        ("comparison-result-cell", &model.id, 3),
                                        widths[3],
                                        44.0,
                                        egui::accesskit::Role::Cell,
                                        Some(&output_cell_name),
                                        |ui| {
                                            let response = ui.add(
                                                egui::Label::new(if output == "No data" {
                                                    RichText::new(&output).italics()
                                                } else {
                                                    RichText::new(&output)
                                                })
                                                .truncate(true),
                                            );
                                            if output != "No data" {
                                                response.on_hover_text(format!(
                                                    "Full output: {output}"
                                                ));
                                            }
                                        },
                                    );
                                    let accuracy_cell_name =
                                        format!("Accuracy for {}", model.display_name);
                                    comparison_table_cell(
                                        ui,
                                        ("comparison-result-cell", &model.id, 4),
                                        widths[4],
                                        44.0,
                                        egui::accesskit::Role::Cell,
                                        Some(&accuracy_cell_name),
                                        |ui| {
                                            let accuracy_action = comparison_accuracy_cell(
                                                ui,
                                                model,
                                                comparison,
                                                result,
                                                &mut restore_reference_focus,
                                            );
                                            if accuracy_action != ScreenAction::None {
                                                action = accuracy_action;
                                            }
                                        },
                                    );
                                });
                            });
                        row_rect = Some(row.response.rect);
                    });
                    if let Some(rect) = row_rect {
                        ui.ctx().accesskit_node_builder(row_id, |builder| {
                            builder.set_bounds(accesskit_rect(rect));
                        });
                    }
                }
            });
        table_rect = Some(table.response.rect);
    });
    if let Some(rect) = table_rect {
        ui.ctx().accesskit_node_builder(table_id, |builder| {
            builder.set_bounds(accesskit_rect(rect));
        });
    }
    action
}

fn comparison_table_column_widths(available_width: f32) -> [f32; 5] {
    let content_width = (available_width - 20.0).max(0.0);
    [
        content_width * 0.15,
        content_width * 0.11,
        content_width * 0.19,
        content_width * 0.13,
        content_width * 0.42,
    ]
}

fn comparison_table_separator(ui: &mut egui::Ui, color: egui::Color32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 1.0), Sense::hover());
    ui.painter()
        .hline(rect.x_range(), rect.center().y, Stroke::new(1.0, color));
}

fn comparison_table_cell(
    ui: &mut egui::Ui,
    id_salt: impl std::hash::Hash,
    width: f32,
    height: f32,
    role: egui::accesskit::Role,
    name: Option<&str>,
    add_contents: impl FnOnce(&mut egui::Ui),
) {
    let cell_id = ui.make_persistent_id(id_salt);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, height), Sense::hover());
    ui.ctx().accesskit_node_builder(cell_id, |builder| {
        builder.set_role(role);
        if let Some(name) = name {
            builder.set_name(name);
        }
        builder.set_bounds(accesskit_rect(rect));
    });
    let clip_rect = rect.intersect(ui.clip_rect());
    let ctx = ui.ctx().clone();
    ctx.with_accessibility_parent(cell_id, || {
        let mut cell_ui = ui.child_ui(rect, Layout::left_to_right(Align::Center));
        cell_ui.set_clip_rect(clip_rect);
        add_contents(&mut cell_ui);
    });
}

fn accesskit_rect(rect: egui::Rect) -> egui::accesskit::Rect {
    egui::accesskit::Rect {
        x0: rect.min.x.into(),
        y0: rect.min.y.into(),
        x1: rect.max.x.into(),
        y1: rect.max.y.into(),
    }
}

fn comparison_status(comparison: &ModelComparisonState) -> Option<String> {
    match comparison.phase {
        ComparisonPhase::Recording => Some("Comparison recording in progress.".into()),
        ComparisonPhase::Processing => Some("Comparison processing in progress.".into()),
        ComparisonPhase::Complete => Some("Comparison results are ready.".into()),
        ComparisonPhase::Error => Some("Comparison finished with an error.".into()),
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

fn comparison_model_summary(
    model: &ModelViewModel,
    result: Option<&super::state::ComparisonResult>,
) -> String {
    result.map_or_else(
        || model.variant_label.clone(),
        |result| {
            format!(
                "{}\n{}",
                model.variant_label,
                comparison_result_status(Some(result))
            )
        },
    )
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

fn comparison_accuracy_cell(
    ui: &mut egui::Ui,
    model: &ModelViewModel,
    comparison: &ModelComparisonState,
    result: Option<&super::state::ComparisonResult>,
    restore_reference_focus: &mut bool,
) -> ScreenAction {
    match (
        comparison.reference_transcript.as_deref(),
        result.and_then(|result| result.word_error_rate),
    ) {
        (Some(reference), Some(rate)) if !reference.trim().is_empty() => {
            ui.label(format!(
                "{:.0}% accuracy",
                ((1.0 - rate).clamp(0.0, 1.0)) * 100.0
            ));
            ScreenAction::None
        }
        (Some(reference), _) if !reference.trim().is_empty() => {
            ui.label("Run comparison to measure");
            ScreenAction::None
        }
        _ => {
            let add_reference = button(
                ui,
                "Add a reference transcript to measure",
                ButtonTone::Text,
            );
            ui.ctx()
                .accesskit_node_builder(add_reference.id, |builder| {
                    builder.set_role(egui::accesskit::Role::Button);
                    builder.set_name("Add a reference transcript to measure");
                    builder.set_description(format!(
                        "Add a reference transcript to measure accuracy for {}.",
                        model.display_name
                    ));
                });
            if *restore_reference_focus {
                add_reference.request_focus();
                *restore_reference_focus = false;
            }
            scroll_focused_comparison_body_control(ui, &add_reference);
            if add_reference.clicked() {
                ScreenAction::ShowComparisonReferenceEditor
            } else {
                ScreenAction::None
            }
        }
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

const SETTINGS_TAB_AUTO_ID_STRIDE: usize = 10_000;

fn settings_tab_auto_id_offset(tab: SettingsTab) -> usize {
    match tab {
        SettingsTab::General | SettingsTab::Output => 0,
        SettingsTab::Recording => SETTINGS_TAB_AUTO_ID_STRIDE,
        SettingsTab::Advanced => SETTINGS_TAB_AUTO_ID_STRIDE * 2,
        SettingsTab::About => SETTINGS_TAB_AUTO_ID_STRIDE * 3,
    }
}

fn settings(
    ui: &mut egui::Ui,
    active_tab: SettingsTab,
    state: &TranscriptionState,
    settings: &RecordingSettingsView,
) -> ScreenAction {
    let active_tab = if active_tab == SettingsTab::Output {
        SettingsTab::General
    } else {
        active_tab
    };
    let colors = ui_palette(ui);
    let mut action = ScreenAction::None;
    ui.horizontal(|ui| {
        let heading = ui.label(RichText::new("Settings").size(30.0).strong());
        ui.ctx().accesskit_node_builder(heading.id, |builder| {
            builder.set_role(egui::accesskit::Role::Heading);
            builder.set_bounds(accesskit_rect(heading.rect));
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
                (SettingsTab::Advanced, "Advanced"),
                (SettingsTab::About, "About"),
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
    if let Some(focused_tab) = tab_responses
        .iter()
        .find_map(|(tab, response)| response.has_focus().then_some(*tab))
    {
        if ui.input(|input| input.key_pressed(egui::Key::ArrowRight)) {
            focus_tab = Some(next_tab(focused_tab));
        } else if ui.input(|input| input.key_pressed(egui::Key::ArrowLeft)) {
            focus_tab = Some(previous_tab(focused_tab));
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
    let selected_tab_id = tab_ids
        .iter()
        .copied()
        .find_map(|(tab, id)| (tab == active_tab).then_some(id))
        .expect("selected settings tab is rendered");
    let panel_id = ui.make_persistent_id(("settings-tab-panel", active_tab));
    ui.ctx().accesskit_node_builder(panel_id, |builder| {
        builder.set_role(egui::accesskit::Role::TabPanel);
        builder.set_name(match active_tab {
            SettingsTab::General => "General settings",
            SettingsTab::Recording => "Recording settings",
            SettingsTab::Output => "General settings",
            SettingsTab::Advanced => "Advanced settings",
            SettingsTab::About => "About Scribe",
        });
        builder.push_labelled_by(selected_tab_id.value().into());
    });
    let ctx = ui.ctx().clone();
    ctx.with_accessibility_parent(panel_id, || {
        // egui 0.27 `push_id` does not salt automatically allocated widget IDs.
        // Reserve a disjoint range so switching tabs cannot update nodes that the
        // AccessKit consumer is simultaneously removing with the old panel.
        ui.skip_ahead_auto_ids(settings_tab_auto_id_offset(active_tab));
        ui.vertical(|ui| {
            ui.set_width(ui.available_width());
            match active_tab {
                SettingsTab::Recording => {
                    recording_settings_panel(ui, state, settings, &mut action)
                }
                SettingsTab::General | SettingsTab::Output => {
                    general_settings_panel(ui, settings, &mut action)
                }
                SettingsTab::Advanced => advanced_settings_panel(ui, state, settings, &mut action),
                SettingsTab::About => about_settings_panel(ui, settings),
            }
        });
    });
    for (tab, tab_id) in tab_ids {
        if tab == active_tab {
            ui.ctx().accesskit_node_builder(tab_id, |builder| {
                builder.push_controlled(panel_id.value().into());
            });
        }
    }
    action
}

fn tab_id(_: &egui::Ui, tab: SettingsTab) -> egui::Id {
    egui::Id::new(("settings-tab", tab))
}

fn tab_control(ui: &mut egui::Ui, tab: SettingsTab, label: &str, selected: bool) -> egui::Response {
    let colors = ui_palette(ui);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(96.0, 44.0), egui::Sense::hover());
    let response = ui.interact(rect, tab_id(ui, tab), egui::Sense::click());
    let fill = if !selected && response.hovered() {
        colors.panel_bg
    } else {
        egui::Color32::TRANSPARENT
    };
    ui.painter().rect_filled(rect, Rounding::same(4.0), fill);
    if selected {
        let underline = egui::Rect::from_min_max(
            egui::pos2(rect.left() + 8.0, rect.bottom() - 3.0),
            egui::pos2(rect.right() - 8.0, rect.bottom()),
        );
        ui.painter()
            .rect_filled(underline, Rounding::same(1.5), colors.accent);
    }
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        label,
        egui::FontId::proportional(14.0),
        if selected {
            colors.text
        } else {
            colors.muted_text
        },
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
    description: &str,
    checked: bool,
    width: f32,
    selected_text: egui::Color32,
) -> egui::Response {
    let colors = ui_palette(ui);
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, 44.0), egui::Sense::hover());
    let response = ui.interact(rect, recording_mode_id(mode), egui::Sense::click());
    let visual = rect.shrink2(Vec2::new(4.0, 6.0));
    if checked {
        ui.painter()
            .rect_filled(visual, Rounding::same(18.0), colors.card_bg);
    } else if response.hovered() {
        ui.painter().rect_stroke(
            visual,
            Rounding::same(18.0),
            Stroke::new(1.0, colors.border_strong),
        );
    }
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        label,
        egui::FontId::proportional(14.0),
        if checked {
            selected_text
        } else {
            colors.primary_button_text
        },
    );
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, label));
    ui.ctx().accesskit_node_builder(response.id, |builder| {
        builder.set_role(egui::accesskit::Role::RadioButton);
        builder.set_name(label);
        builder.set_description(description);
        builder.set_checked(if checked {
            egui::accesskit::Checked::True
        } else {
            egui::accesskit::Checked::False
        });
    });
    paint_focus_ring(ui, &response, Rounding::same(18.0));
    focus_tooltip(ui, &response, description);
    response.on_hover_text(description)
}

fn recording_mode_id(mode: RecordingMode) -> egui::Id {
    egui::Id::new(("recording-mode", mode))
}

fn recording_mode_description(mode: RecordingMode) -> &'static str {
    match mode {
        RecordingMode::PressOnce => {
            "Press the recording hotkey once to start, then press it again to stop."
        }
        RecordingMode::Hold => "Hold the recording hotkey to record, then release it to stop.",
    }
}

fn recording_mode_toggle(
    ui: &mut egui::Ui,
    selected: RecordingMode,
) -> Vec<(RecordingMode, egui::Response)> {
    const TOGGLE_WIDTH: f32 = 232.0;
    const TOGGLE_HEIGHT: f32 = 44.0;
    const TOGGLE_VISUAL_HEIGHT: f32 = 36.0;

    let (track_rect, _) =
        ui.allocate_exact_size(Vec2::new(TOGGLE_WIDTH, TOGGLE_HEIGHT), egui::Sense::hover());
    let track_visual_rect = egui::Rect::from_center_size(
        track_rect.center(),
        Vec2::new(TOGGLE_WIDTH, TOGGLE_VISUAL_HEIGHT),
    );
    let colors = ui_palette(ui);
    ui.painter().rect_filled(
        track_visual_rect,
        Rounding::same(18.0),
        colors.segmented_control_bg,
    );
    let mut toggle_ui = ui.child_ui(track_rect, Layout::left_to_right(Align::Center));
    toggle_ui.spacing_mut().item_spacing.x = 0.0;
    [
        (RecordingMode::PressOnce, "Press once"),
        (RecordingMode::Hold, "Hold"),
    ]
    .into_iter()
    .map(|(mode, label)| {
        (
            mode,
            radio_control(
                &mut toggle_ui,
                mode,
                label,
                recording_mode_description(mode),
                selected == mode,
                TOGGLE_WIDTH / 2.0,
                colors.segmented_control_selected_text,
            ),
        )
    })
    .collect()
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
    settings_section(ui, "Recording behavior", |ui| {
        ui.add_space(6.0);
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
            let focused_mode = [RecordingMode::PressOnce, RecordingMode::Hold]
                .into_iter()
                .find(|mode| ui.memory(|memory| memory.has_focus(recording_mode_id(*mode))));
            let mode_arrow_pressed = ui.input(|input| {
                input.key_pressed(egui::Key::ArrowRight) || input.key_pressed(egui::Key::ArrowLeft)
            });
            let ctx = ui.ctx().clone();
            ctx.accesskit_node_builder(radio_group_id, |builder| {
                builder.set_role(egui::accesskit::Role::RadioGroup);
                builder.set_name("Recording mode");
            });
            ctx.with_accessibility_parent(radio_group_id, || {
                compact_setting_row(ui, "Mode", true, |ui, _| {
                    for (mode, response) in recording_mode_toggle(ui, state.recording_mode) {
                        radio_ids.push(response.id);
                        if response.clicked() {
                            *action = ScreenAction::SetRecordingMode(mode);
                        }
                        if focused_mode == Some(mode) && mode_arrow_pressed {
                            let next = if mode == RecordingMode::PressOnce {
                                RecordingMode::Hold
                            } else {
                                RecordingMode::PressOnce
                            };
                            ui.memory_mut(|memory| memory.request_focus(recording_mode_id(next)));
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
            compact_setting_row(ui, "Duration limit", false, |ui, label_id| {
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
        });
    });
    ui.add_space(16.0);
    settings_section(ui, "Recording input", |ui| {
        ui.add_enabled_ui(!recording_locked, |ui| {
            compact_setting_row(ui, "Global record hotkey", true, |ui, _| {
                ui.vertical(|ui| {
                    ui.horizontal(|ui| {
                        for (index, key) in state
                            .hotkey
                            .split('+')
                            .map(str::trim)
                            .filter(|key| !key.is_empty())
                            .enumerate()
                        {
                            if index > 0 {
                                ui.label(RichText::new("+").color(colors.muted_text));
                            }
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
            compact_setting_row(ui, "Device", true, |ui, label_id| {
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
            compact_setting_row(ui, "Input level", false, |ui, label_id| {
                let (icon_rect, _) = ui.allocate_exact_size(Vec2::new(24.0, 40.0), Sense::hover());
                ui.painter().text(
                    icon_rect.center(),
                    Align2::CENTER_CENTER,
                    icon_glyph(Icon::Microphone),
                    egui::FontId::proportional(18.0),
                    colors.muted_text,
                );
                let mut percent = settings.input_sensitivity_percent;
                let sensitivity =
                    input_sensitivity_meter_slider(ui, settings.input_level_percent, &mut percent)
                        .labelled_by(label_id);
                if sensitivity.changed() {
                    *action = ScreenAction::SetInputSensitivity(percent);
                }
            });
        });
        if let Some(error) = settings.microphone_error.as_deref() {
            ui.add_space(8.0);
            let error_action = microphone_error_notice(ui, error);
            if error_action != ScreenAction::None {
                *action = error_action;
            }
        }
    });
    ui.add_space(16.0);
    settings_section(ui, "Transcription", |ui| {
        let _ = SettingsRow::show_with_help(
            ui,
            "Live transcription preview",
            LIVE_TRANSCRIPTION_PREVIEW_SWITCH_ID,
            LIVE_TRANSCRIPTION_PREVIEW_DESCRIPTION,
            true,
            |ui, _| {
                if settings_switch(
                    ui,
                    LIVE_TRANSCRIPTION_PREVIEW_SWITCH_ID,
                    settings.provisional_feedback,
                    "Live transcription preview",
                    LIVE_TRANSCRIPTION_PREVIEW_DESCRIPTION,
                    !recording_locked,
                )
                .clicked()
                {
                    *action = ScreenAction::ToggleProvisionalFeedback;
                }
            },
        );
        ui.add_enabled_ui(!recording_locked, |ui| {
            let mut streaming = settings.streaming_label.clone();
            setting_row_with_separator(ui, "Streaming mode", true, |ui, label_id| {
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
    });
}

fn voice_detection_settings_section(
    ui: &mut egui::Ui,
    state: &TranscriptionState,
    settings: &RecordingSettingsView,
    action: &mut ScreenAction,
) {
    let recording_locked = matches!(
        state.phase,
        TranscriptionPhase::Listening | TranscriptionPhase::Finalizing
    );
    settings_section(ui, "Voice detection", |ui| {
        if recording_locked {
            let notice = ui.label(
                RichText::new(VOICE_DETECTION_LOCKED_DESCRIPTION).color(ui_palette(ui).warning),
            );
            ui.ctx().accesskit_node_builder(notice.id, |builder| {
                builder.set_live(egui::accesskit::Live::Polite);
                builder.set_live_atomic();
            });
        }
        let mut vad_enabled = settings.vad_enabled;
        let _ = SettingsRow::show_with_help(
            ui,
            "Stop after speech ends",
            STOP_AFTER_SPEECH_SWITCH_ID,
            STOP_AFTER_SPEECH_DESCRIPTION,
            false,
            |ui, _| {
                let response = settings_switch(
                    ui,
                    STOP_AFTER_SPEECH_SWITCH_ID,
                    vad_enabled,
                    "Stop after speech ends",
                    STOP_AFTER_SPEECH_DESCRIPTION,
                    !recording_locked,
                );
                if recording_locked {
                    let description = format!(
                        "{STOP_AFTER_SPEECH_DESCRIPTION} {VOICE_DETECTION_LOCKED_DESCRIPTION}"
                    );
                    ui.ctx().accesskit_node_builder(response.id, |builder| {
                        builder.set_disabled();
                        builder.set_description(description);
                    });
                }
                if response.clicked() {
                    vad_enabled = !vad_enabled;
                    *action = ScreenAction::SetVadEnabled(vad_enabled);
                }
            },
        );
        ui.add_enabled_ui(!recording_locked, |ui| {
            if vad_enabled {
                ui.separator();
                for (index, (label, value, action_for)) in [
                    ("Speech confirmation ms", settings.speech_confirmation_ms, 0),
                    ("Internal pause ms", settings.internal_pause_ms, 1),
                    ("End after silence ms", settings.endpoint_silence_ms, 2),
                    ("Pre-roll ms", settings.pre_roll_ms, 3),
                    ("Post-roll ms", settings.post_roll_ms, 4),
                ]
                .into_iter()
                .enumerate()
                {
                    let _ = SettingsRow::show(ui, label, index < 4, |ui, label_id| {
                        let mut edited = value as i64;
                        if ui
                            .add_sized(
                                [96.0, 44.0],
                                egui::DragValue::new(&mut edited).clamp_range(0..=5_000),
                            )
                            .labelled_by(label_id)
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
                }
            }
        });
    });
}

fn general_settings_panel(
    ui: &mut egui::Ui,
    settings: &RecordingSettingsView,
    action: &mut ScreenAction,
) {
    settings_section(ui, "General settings", |ui| {
        let mut close_to_tray = settings.close_to_tray;
        let _ = SettingsRow::show_with_help(
            ui,
            "Close to tray",
            CLOSE_TO_TRAY_SWITCH_ID,
            CLOSE_TO_TRAY_DESCRIPTION,
            true,
            |ui, _| {
                if settings_switch(
                    ui,
                    CLOSE_TO_TRAY_SWITCH_ID,
                    close_to_tray,
                    "Close to tray",
                    CLOSE_TO_TRAY_DESCRIPTION,
                    true,
                )
                .clicked()
                {
                    close_to_tray = !close_to_tray;
                    *action = ScreenAction::SetCloseToTray(close_to_tray);
                }
            },
        );
        let _ = SettingsRow::show(ui, "Active model", false, |ui, _| {
            ui.label(&settings.active_model_label);
            if button(ui, "Manage models", ButtonTone::Secondary).clicked() {
                *action = ScreenAction::OpenModelSettings;
            }
        });
    });
    ui.add_space(16.0);
    settings_section(ui, "Appearance", |ui| {
        let mut theme = settings.theme_label.clone();
        setting_row_with_separator(ui, "Theme", true, |ui, label_id| {
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
        setting_row_with_separator(ui, "Dictation overlay", true, |ui, label_id| {
            ui.vertical(|ui| {
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
                if !settings.overlay_available {
                    ui.label(RichText::new("The overlay is unavailable because focus safety is not verified on this platform.").color(ui_palette(ui).warning));
                }
            });
        });
        if overlay != settings.overlay_label {
            *action = ScreenAction::SetOverlayMode(overlay);
        }
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
    output_settings_panel(ui, settings, action);
}

fn output_settings_panel(
    ui: &mut egui::Ui,
    settings: &RecordingSettingsView,
    action: &mut ScreenAction,
) {
    settings_section(ui, "Output settings", |ui| {
        let mut auto_insert = settings.auto_insert_transcript;
        let (output_label, output_description) =
            transcript_delivery_copy(settings.show_restore_clipboard);
        let _ = SettingsRow::show_with_help(
            ui,
            output_label,
            AUTO_INSERT_TRANSCRIPT_SWITCH_ID,
            output_description,
            false,
            |ui, _| {
                if settings_switch(
                    ui,
                    AUTO_INSERT_TRANSCRIPT_SWITCH_ID,
                    auto_insert,
                    output_label,
                    output_description,
                    true,
                )
                .clicked()
                {
                    auto_insert = !auto_insert;
                    *action = ScreenAction::SetAutoInsertTranscript(auto_insert);
                }
            },
        );
        if auto_insert {
            if settings.show_restore_clipboard || settings.output_notice.is_some() {
                ui.separator();
            }
            if settings.show_restore_clipboard {
                let mut restore = settings.restore_clipboard_after_insert;
                let _ = SettingsRow::show_with_help(
                    ui,
                    "Restore clipboard after insert",
                    RESTORE_CLIPBOARD_SWITCH_ID,
                    RESTORE_CLIPBOARD_DESCRIPTION,
                    true,
                    |ui, _| {
                        if settings_switch(
                            ui,
                            RESTORE_CLIPBOARD_SWITCH_ID,
                            restore,
                            "Restore clipboard after insert",
                            RESTORE_CLIPBOARD_DESCRIPTION,
                            true,
                        )
                        .clicked()
                        {
                            restore = !restore;
                            *action = ScreenAction::SetRestoreClipboardAfterInsert(restore);
                        }
                    },
                );
                let _ = SettingsRow::show(ui, "Paste delay ms", false, |ui, label_id| {
                    let mut delay = settings.paste_delay_ms as i64;
                    if ui
                        .add_sized(
                            [96.0, 44.0],
                            egui::DragValue::new(&mut delay).clamp_range(1..=1_000),
                        )
                        .labelled_by(label_id)
                        .changed()
                    {
                        *action = ScreenAction::SetPasteDelayMs(delay as u64);
                    }
                });
            } else if let Some(notice) = &settings.output_notice {
                let _ = SettingsRow::show(ui, "Platform behavior", false, |ui, _| {
                    ui.label(RichText::new(notice).color(ui_palette(ui).muted_text));
                });
            }
        }
    });
}

fn about_settings_panel(ui: &mut egui::Ui, settings: &RecordingSettingsView) {
    settings_section(ui, "Application", |ui| {
        about_page(
            ui,
            Path::new(&settings.about_model_directory),
            settings.about_settings_path.as_deref().map(Path::new),
        );
    });
}

fn advanced_settings_panel(
    ui: &mut egui::Ui,
    state: &TranscriptionState,
    settings: &RecordingSettingsView,
    action: &mut ScreenAction,
) {
    voice_detection_settings_section(ui, state, settings, action);
    ui.add_space(16.0);
    settings_section(ui, "History and privacy", |ui| {
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
        {
            let mut mode = settings.history_mode_label.clone();
            let _ = SettingsRow::show(ui, "History storage", false, |ui, label_id| {
                let response = ui
                    .add_enabled_ui(!settings.history_locked, |ui| {
                        ComboBox::from_id_source("history-storage-mode")
                            .selected_text(&mode)
                            .show_ui(ui, |ui| {
                                for value in ["Off", "Transcript only", "Transcript and audio"] {
                                    ui.selectable_value(&mut mode, value.to_owned(), value);
                                }
                            })
                            .response
                            .labelled_by(label_id)
                    })
                    .inner;
                describe_history_lock(ui, &response, settings.history_locked, None);
            });
            if mode != settings.history_mode_label {
                *action = ScreenAction::SetHistoryMode(mode.clone());
            }
            if mode != "Off" {
                ui.separator();
                let mut maximum = settings.max_history_entries as i64;
                let _ = SettingsRow::show(ui, "Maximum unpinned entries", true, |ui, label_id| {
                    let response = ui
                        .add_enabled_ui(!settings.history_locked, |ui| {
                            ui.add_sized(
                                [96.0, 44.0],
                                egui::DragValue::new(&mut maximum).clamp_range(1..=1_000),
                            )
                            .labelled_by(label_id)
                        })
                        .inner;
                    describe_history_lock(ui, &response, settings.history_locked, None);
                    if response.changed() {
                        *action = ScreenAction::SetMaxHistoryEntries(maximum as u32);
                    }
                });
                optional_retention_control(
                    ui,
                    OptionalRetentionSetting {
                        label: "Limit transcript age",
                        unlimited_label: "Keep transcripts until deleted",
                        configured_days: settings.transcript_retention_days,
                        switch_id: LIMIT_TRANSCRIPT_AGE_SWITCH_ID,
                        description: LIMIT_TRANSCRIPT_AGE_DESCRIPTION,
                    },
                    settings.history_locked,
                    action,
                    ScreenAction::SetTranscriptRetentionDays,
                );
                if mode == "Transcript and audio" {
                    optional_retention_control(
                        ui,
                        OptionalRetentionSetting {
                            label: "Limit audio age",
                            unlimited_label: "Keep retained audio until its entry is deleted",
                            configured_days: settings.audio_retention_days,
                            switch_id: LIMIT_AUDIO_AGE_SWITCH_ID,
                            description: LIMIT_AUDIO_AGE_DESCRIPTION,
                        },
                        settings.history_locked,
                        action,
                        ScreenAction::SetAudioRetentionDays,
                    );
                }
                let mut identity = settings.store_application_identity;
                let _ = SettingsRow::show_with_help(
                    ui,
                    "Store application identity",
                    STORE_APPLICATION_IDENTITY_SWITCH_ID,
                    STORE_APPLICATION_IDENTITY_DESCRIPTION,
                    false,
                    |ui, _| {
                        let identity_control = settings_switch(
                            ui,
                            STORE_APPLICATION_IDENTITY_SWITCH_ID,
                            identity,
                            "Store application identity",
                            STORE_APPLICATION_IDENTITY_DESCRIPTION,
                            !settings.history_locked,
                        );
                        describe_history_lock(
                            ui,
                            &identity_control,
                            settings.history_locked,
                            Some(STORE_APPLICATION_IDENTITY_DESCRIPTION),
                        );
                        if identity_control.clicked() {
                            identity = !identity;
                            *action = ScreenAction::SetStoreApplicationIdentity(identity);
                        }
                    },
                );
            }
        }
    });
    ui.add_space(16.0);
    settings_section(ui, "Developer and diagnostics", |ui| {
        let mut enabled = settings.debug_mode;
        let mut playground = None;
        let _ = SettingsRow::show_with_help(
            ui,
            "Enable model Playground",
            ENABLE_MODEL_PLAYGROUND_SWITCH_ID,
            ENABLE_MODEL_PLAYGROUND_DESCRIPTION,
            false,
            |ui, _| {
                playground = Some(settings_switch(
                    ui,
                    ENABLE_MODEL_PLAYGROUND_SWITCH_ID,
                    enabled,
                    "Enable model Playground",
                    ENABLE_MODEL_PLAYGROUND_DESCRIPTION,
                    true,
                ));
            },
        );
        let playground = playground.expect("developer row always renders its switch");
        scroll_focused_control_into_view(ui, &playground);
        if playground.clicked() {
            enabled = !enabled;
            *action = ScreenAction::SetDebugMode(enabled);
        }
        ui.separator();
        if enabled {
            let mut open = None;
            let _ = SettingsRow::show(ui, "Playground", true, |ui, _| {
                open =
                    Some(ui.add_sized([176.0, 44.0], egui::Button::new("Open model Playground")));
            });
            let open = open.expect("enabled developer row always renders its action");
            if settings.focus_playground_open {
                open.request_focus();
            }
            scroll_focused_control_into_view(ui, &open);
            if open.clicked() {
                *action = ScreenAction::OpenDeveloperPlayground;
            }
        }
        let _ = SettingsRow::show(ui, "Diagnostics", false, |ui, _| {
            ui.vertical(|ui| {
                ui.label(format!(
                    "{} recent session snapshot(s) are held in memory. Exports exclude transcript and audio content, secrets, filesystem paths, and raw errors.",
                    settings.diagnostic_session_count
                ));
            for line in &settings.diagnostics {
                ui.label(line);
            }
                let export = ui.add_enabled(
                    settings.can_export_diagnostics,
                    egui::Button::new("Export redacted diagnostics")
                        .min_size(Vec2::new(220.0, 44.0)),
                );
                if !settings.can_export_diagnostics {
                    ui.ctx().accesskit_node_builder(export.id, |builder| {
                        builder.set_disabled();
                        builder.set_description(
                            "Unavailable because the platform settings directory cannot provide a private export location.",
                        );
                    });
                    ui.label(RichText::new("The platform settings directory is unavailable, so Scribe cannot choose a private export location.").color(ui_palette(ui).muted_text));
                }
                if export.clicked() {
                    *action = ScreenAction::ExportRedactedDiagnostics;
                }
            });
        });
    });
}

struct OptionalRetentionSetting<'a> {
    label: &'a str,
    unlimited_label: &'a str,
    configured_days: Option<u32>,
    switch_id: &'a str,
    description: &'a str,
}

fn optional_retention_control(
    ui: &mut egui::Ui,
    setting: OptionalRetentionSetting<'_>,
    history_locked: bool,
    action: &mut ScreenAction,
    update: impl FnOnce(Option<u32>) -> ScreenAction + Copy,
) {
    let mut limited = setting.configured_days.is_some();
    let _ = SettingsRow::show_with_help(
        ui,
        setting.label,
        setting.switch_id,
        setting.description,
        false,
        |ui, _| {
            ui.vertical(|ui| {
                let limit = settings_switch(
                    ui,
                    setting.switch_id,
                    limited,
                    setting.label,
                    setting.description,
                    !history_locked,
                );
                describe_history_lock(ui, &limit, history_locked, Some(setting.description));
                if limit.clicked() {
                    limited = !limited;
                    *action = update(limited.then_some(setting.configured_days.unwrap_or(30)));
                }
                if !limited {
                    ui.label(
                        RichText::new(setting.unlimited_label).color(ui_palette(ui).muted_text),
                    );
                }
            });
        },
    );
    ui.separator();
    if limited {
        let mut days = setting.configured_days.unwrap_or(30) as i64;
        let _ = SettingsRow::show(ui, "Days", false, |ui, label_id| {
            let response = ui
                .add_enabled_ui(!history_locked, |ui| {
                    ui.add_sized(
                        [96.0, 44.0],
                        egui::DragValue::new(&mut days).clamp_range(1..=3_650),
                    )
                    .labelled_by(label_id)
                })
                .inner;
            describe_history_lock(ui, &response, history_locked, None);
            if response.changed() {
                *action = update(Some(days as u32));
            }
        });
        ui.separator();
    }
}

fn describe_history_lock(
    ui: &egui::Ui,
    response: &egui::Response,
    locked: bool,
    description: Option<&str>,
) {
    if locked {
        ui.ctx().accesskit_node_builder(response.id, |builder| {
            let unavailable = "Unavailable while a retained-audio retry owns its history row.";
            builder.set_description(match description {
                Some(description) => format!("{description} {unavailable}"),
                None => unavailable.to_owned(),
            });
        });
    }
}

fn input_sensitivity_meter_slider(
    ui: &mut egui::Ui,
    live_level_percent: u8,
    threshold_percent: &mut u8,
) -> egui::Response {
    use egui::accesskit::{Action, ActionData};

    let desired = Vec2::new(320.0, 44.0);
    let (rect, mut response) = ui.allocate_exact_size(desired, Sense::click_and_drag());
    let previous = *threshold_percent;
    let mut value = f32::from(*threshold_percent).clamp(0.0, 100.0);

    if response.enabled() && response.clicked() {
        response.request_focus();
    }
    if response.enabled()
        && (response.clicked() || response.dragged())
        && let Some(pointer) = response.interact_pointer_pos()
    {
        value = (100.0 * (pointer.x - rect.left()) / rect.width()).clamp(0.0, 100.0);
    }

    let mut decrement = 0usize;
    let mut increment = 0usize;
    if response.enabled() && response.has_focus() {
        ui.ctx().memory_mut(|memory| {
            memory.set_focus_lock_filter(
                response.id,
                egui::EventFilter {
                    horizontal_arrows: true,
                    ..Default::default()
                },
            );
        });
        ui.input(|input| {
            decrement += input.num_presses(egui::Key::ArrowLeft);
            increment += input.num_presses(egui::Key::ArrowRight);
            if input.key_pressed(egui::Key::Home) {
                value = 0.0;
            }
            if input.key_pressed(egui::Key::End) {
                value = 100.0;
            }
        });
    }
    if response.enabled() {
        ui.input(|input| {
            decrement += input.num_accesskit_action_requests(response.id, Action::Decrement);
            increment += input.num_accesskit_action_requests(response.id, Action::Increment);
            for request in input.accesskit_action_requests(response.id, Action::SetValue) {
                if let Some(ActionData::NumericValue(requested)) = request.data {
                    value = requested as f32;
                }
            }
        });
    }
    value = (value + increment as f32 - decrement as f32).clamp(0.0, 100.0);
    *threshold_percent = value.round() as u8;
    if *threshold_percent != previous {
        response.mark_changed();
    }

    let live_state = if live_level_percent == 0 {
        "No input detected."
    } else if live_level_percent >= *threshold_percent {
        "Input detected above sensitivity."
    } else {
        "Input below sensitivity."
    };
    let description = format!(
        "{live_state} Minimum microphone level treated as speech. The colored fill shows the current input level without changing focus or announcing each update. Use Left and Right arrow keys to adjust."
    );
    response.widget_info(|| {
        egui::WidgetInfo::slider(f64::from(*threshold_percent), "Input level sensitivity")
    });
    ui.ctx().accesskit_node_builder(response.id, |builder| {
        builder.set_name("Input level sensitivity");
        builder.set_description(description);
        builder.set_min_numeric_value(0.0);
        builder.set_max_numeric_value(100.0);
        builder.set_numeric_value_step(1.0);
        builder.add_action(Action::SetValue);
        if *threshold_percent < 100 {
            builder.add_action(Action::Increment);
        }
        if *threshold_percent > 0 {
            builder.add_action(Action::Decrement);
        }
    });

    let colors = ui_palette(ui);
    let track = egui::Rect::from_center_size(rect.center(), Vec2::new(rect.width(), 10.0));
    let rounding = Rounding::same(5.0);
    ui.painter()
        .rect_filled(track, rounding, colors.slider_remainder_fill);
    let threshold_position = f32::from(*threshold_percent) / 100.0;
    let threshold_x = track.left() + track.width() * threshold_position;
    let threshold_region = egui::Rect::from_min_max(
        track.min,
        egui::pos2(
            threshold_x.clamp(track.left(), track.right()),
            track.bottom(),
        ),
    );
    if threshold_region.width() > 0.0 {
        ui.painter()
            .rect_filled(threshold_region, rounding, colors.slider_threshold_fill);
    }
    ui.painter().rect_stroke(
        track,
        rounding,
        Stroke::new(1.0, colors.slider_track_border),
    );
    let live_position = f32::from(live_level_percent.min(100)) / 100.0;
    let live_width = track.width() * live_position;
    if live_width > 0.0 {
        let live_rect = egui::Rect::from_min_size(
            egui::pos2(track.left(), track.center().y - 3.0),
            Vec2::new(live_width, 6.0),
        );
        let fill = if live_position >= threshold_position {
            colors.slider_live_above
        } else {
            colors.slider_live_below
        };
        ui.painter()
            .rect_filled(live_rect, Rounding::same(3.0), fill);
    }

    let thumb_center = egui::pos2(threshold_x, track.center().y);
    let thumb_radius = if response.dragged() { 9.0 } else { 8.0 };
    ui.painter()
        .circle_filled(thumb_center, thumb_radius, colors.card_bg);
    ui.painter().circle_stroke(
        thumb_center,
        thumb_radius,
        Stroke::new(if response.has_focus() { 3.0 } else { 2.0 }, colors.primary),
    );
    paint_focus_ring(ui, &response, Rounding::same(5.0));
    response
}

struct SettingsSection;

impl SettingsSection {
    fn show(ui: &mut egui::Ui, title: &str, contents: impl FnOnce(&mut egui::Ui)) {
        card(ui, |ui| {
            ui.label(RichText::new(title).strong());
            ui.add_space(6.0);
            contents(ui);
        });
    }
}

struct SettingsRow;

impl SettingsRow {
    fn show(
        ui: &mut egui::Ui,
        label: &str,
        separator_after: bool,
        contents: impl FnOnce(&mut egui::Ui, egui::Id),
    ) -> egui::Response {
        Self::show_with_optional_help(ui, label, None, separator_after, contents)
    }

    fn show_with_help(
        ui: &mut egui::Ui,
        label: &str,
        id_source: &str,
        description: &str,
        separator_after: bool,
        contents: impl FnOnce(&mut egui::Ui, egui::Id),
    ) -> egui::Response {
        Self::show_with_optional_help(
            ui,
            label,
            Some((id_source, description)),
            separator_after,
            contents,
        )
    }

    fn show_with_optional_help(
        ui: &mut egui::Ui,
        label: &str,
        help: Option<(&str, &str)>,
        separator_after: bool,
        contents: impl FnOnce(&mut egui::Ui, egui::Id),
    ) -> egui::Response {
        let compact = current_content_width(ui) < SETTINGS_COMPACT_BREAKPOINT;
        let row = ui.scope(|ui| {
            let interaction_height = ui.spacing().interact_size.y.max(44.0);
            ui.spacing_mut().interact_size.y = interaction_height;
            ui.set_min_height(interaction_height);
            if compact {
                ui.vertical(|ui| {
                    let label = settings_row_label(ui, label, help);
                    contents(ui, label.id);
                });
            } else {
                ui.horizontal(|ui| {
                    let (label_rect, _) = ui.allocate_exact_size(
                        Vec2::new(SETTINGS_LABEL_COLUMN_WIDTH, 44.0),
                        Sense::hover(),
                    );
                    let mut label_ui = ui.child_ui(
                        label_rect,
                        Layout::left_to_right(Align::Center).with_main_align(Align::LEFT),
                    );
                    let label = settings_row_label(&mut label_ui, label, help);
                    ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                        contents(ui, label.id);
                    });
                });
            }
        });
        if separator_after {
            ui.separator();
        }
        row.response
    }
}

fn settings_row_label(
    ui: &mut egui::Ui,
    label: &str,
    help: Option<(&str, &str)>,
) -> egui::Response {
    ui.horizontal(|ui| {
        let label_response = ui.label(RichText::new(label).color(ui_palette(ui).muted_text));
        if let Some((id_source, description)) = help {
            settings_help_affordance(ui, id_source, label, description);
        }
        label_response
    })
    .inner
}

fn settings_switch(
    ui: &mut egui::Ui,
    id_source: &str,
    checked: bool,
    accessible_name: &str,
    description: &str,
    enabled: bool,
) -> egui::Response {
    const SWITCH_SIZE: Vec2 = Vec2::new(52.0, 44.0);
    const SWITCH_VISUAL_SIZE: Vec2 = Vec2::new(44.0, 24.0);
    const SWITCH_KNOB_DIAMETER: f32 = 18.0;

    ui.add_enabled_ui(enabled, |ui| {
        let (rect, _) = ui.allocate_exact_size(SWITCH_SIZE, Sense::hover());
        let response = ui.interact(rect, egui::Id::new(id_source), Sense::click());
        let effective_checked = checked ^ response.clicked();
        let visual = egui::Rect::from_center_size(rect.center(), SWITCH_VISUAL_SIZE);
        let colors = ui_palette(ui);
        let track_fill = if effective_checked {
            colors.accent
        } else {
            colors.inactive_toggle_track
        };
        let track_stroke = if effective_checked {
            Stroke::new(1.0, colors.accent)
        } else {
            Stroke::new(1.0, colors.inactive_toggle_track)
        };
        ui.painter()
            .rect(visual, Rounding::same(12.0), track_fill, track_stroke);
        let knob_center = egui::pos2(
            if effective_checked {
                visual.right() - SWITCH_KNOB_DIAMETER / 2.0 - 3.0
            } else {
                visual.left() + SWITCH_KNOB_DIAMETER / 2.0 + 3.0
            },
            visual.center().y,
        );
        ui.painter()
            .circle_filled(knob_center, SWITCH_KNOB_DIAMETER / 2.0, colors.card_bg);
        response
            .widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Checkbox, accessible_name));
        ui.ctx().accesskit_node_builder(response.id, |builder| {
            builder.set_role(egui::accesskit::Role::Switch);
            builder.set_name(accessible_name);
            builder.set_description(description);
            builder.set_checked(if effective_checked {
                egui::accesskit::Checked::True
            } else {
                egui::accesskit::Checked::False
            });
            builder.set_bounds(accesskit_rect(rect));
            if !response.enabled() {
                builder.set_disabled();
            }
        });
        paint_focus_ring(ui, &response, Rounding::same(12.0));
        response
    })
    .inner
}

fn settings_help_affordance(
    ui: &mut egui::Ui,
    id_source: &str,
    accessible_name: &str,
    description: &str,
) {
    const HOVER_DELAY_SECONDS: f64 = 0.3;
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(44.0), Sense::hover());
    let response = ui.interact(
        rect,
        egui::Id::new(("settings-help-affordance", id_source)),
        Sense::click(),
    );
    let popup_id = egui::Id::new(("settings-help-popup", id_source));
    let state_id = egui::Id::new("settings-help-state");
    let mut state = ui.data(|data| {
        data.get_temp::<SettingsHelpState>(state_id)
            .unwrap_or_default()
    });
    let now = ui.input(|input| input.time);
    let pointer_over_last_popup = state.dismissed == Some(response.id)
        && ui
            .input(|input| input.pointer.hover_pos())
            .zip(state.popup_rect)
            .is_some_and(|(pointer, popup)| popup.contains(pointer));
    if state.dismissed == Some(response.id)
        && !response.hovered()
        && !response.has_focus()
        && !pointer_over_last_popup
    {
        state.dismissed = None;
        state.popup_rect = None;
    }
    let keyboard_activated = response.has_focus()
        && ui.input(|input| {
            input.key_pressed(egui::Key::Enter) || input.key_pressed(egui::Key::Space)
        });
    let activated = response.clicked() || keyboard_activated;
    if activated {
        if state.active == Some(response.id) && state.pinned {
            state.active = None;
            state.pinned = false;
            state.dismissed = Some(response.id);
        } else {
            state.active = Some(response.id);
            state.pinned = true;
            state.dismissed = None;
        }
        state.hover_candidate = None;
    } else if state.dismissed != Some(response.id) && !state.pinned {
        if response.has_focus() && state.active != Some(response.id) {
            state.active = Some(response.id);
            state.pinned = false;
        } else if response.hovered() {
            let hover_started_at = if state.hover_candidate == Some(response.id) {
                state.hover_started_at
            } else {
                state.hover_candidate = Some(response.id);
                state.hover_started_at = now;
                now
            };
            let hover_elapsed = now - hover_started_at;
            if hover_elapsed >= HOVER_DELAY_SECONDS && state.active != Some(response.id) {
                state.active = Some(response.id);
                state.pinned = false;
            } else if hover_elapsed < HOVER_DELAY_SECONDS {
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_secs_f64(
                        HOVER_DELAY_SECONDS - hover_elapsed,
                    ));
            }
        } else if state.hover_candidate == Some(response.id) {
            state.hover_candidate = None;
        }
    }
    if state.active == Some(response.id) && !activated {
        let pointer_over_popup = ui
            .input(|input| input.pointer.hover_pos())
            .zip(state.popup_rect)
            .is_some_and(|(pointer, popup)| popup.contains(pointer));
        let outside_click = state.pinned
            && ui.input(|input| input.pointer.any_click())
            && !response.hovered()
            && !pointer_over_popup;
        let left_transient_help =
            !state.pinned && !response.hovered() && !response.has_focus() && !pointer_over_popup;
        if outside_click || left_transient_help {
            state.active = None;
            state.pinned = false;
            state.dismissed = outside_click.then_some(response.id);
            state.hover_candidate = None;
        }
    }
    let escape_pressed = ui.input(|input| input.key_pressed(egui::Key::Escape));
    if state.active == Some(response.id) && escape_pressed {
        state.active = None;
        state.pinned = false;
        state.dismissed = Some(response.id);
        state.hover_candidate = None;
    }
    let expanded = state.active == Some(response.id);
    let colors = ui_palette(ui);
    let visual = egui::Rect::from_center_size(rect.center(), Vec2::splat(20.0));
    ui.painter()
        .circle_filled(visual.center(), 10.0, colors.panel_bg);
    ui.painter().circle_stroke(
        visual.center(),
        10.0,
        Stroke::new(1.0, colors.border_strong),
    );
    ui.painter().text(
        visual.center(),
        Align2::CENTER_CENTER,
        "?",
        egui::FontId::proportional(14.0),
        colors.muted_text,
    );
    let help_name = format!("{accessible_name} information");
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, &help_name));
    ui.ctx().accesskit_node_builder(response.id, |builder| {
        builder.set_role(egui::accesskit::Role::Button);
        builder.set_name(help_name);
        builder.set_description(description);
        builder.set_expanded(expanded);
        builder.set_bounds(accesskit_rect(rect));
        builder.set_default_action_verb(egui::accesskit::DefaultActionVerb::Click);
        builder.add_action(egui::accesskit::Action::Default);
        if !response.enabled() {
            builder.set_disabled();
        }
    });
    paint_focus_ring(ui, &response, Rounding::same(10.0));
    let popup_response = expanded.then(|| {
        egui::Area::new(popup_id)
            .order(egui::Order::Foreground)
            .constrain(true)
            .fixed_pos(response.rect.left_bottom())
            .show(ui.ctx(), |ui| {
                egui::Frame::popup(ui.style())
                    .show(ui, |ui| {
                        ui.set_min_width(280.0);
                        ui.set_max_width(320.0);
                        ui.label(description);
                    })
                    .response
            })
            .inner
    });
    if let Some(popup) = popup_response {
        state.popup_rect = Some(popup.rect);
        let pointer_over_popup = ui
            .input(|input| input.pointer.hover_pos())
            .is_some_and(|pointer| popup.rect.contains(pointer));
        let clicked_outside = !activated
            && (popup.clicked_elsewhere()
                || (ui.input(|input| input.pointer.any_click())
                    && !response.hovered()
                    && !pointer_over_popup));
        if (state.pinned && clicked_outside)
            || (!state.pinned
                && !response.hovered()
                && !response.has_focus()
                && !pointer_over_popup)
        {
            state.active = None;
            state.pinned = false;
            state.dismissed = clicked_outside.then_some(response.id);
            state.hover_candidate = None;
        }
    }
    ui.data_mut(|data| data.insert_temp(state_id, state));
}

#[derive(Clone, Copy, Default)]
struct SettingsHelpState {
    active: Option<egui::Id>,
    pinned: bool,
    hover_candidate: Option<egui::Id>,
    hover_started_at: f64,
    dismissed: Option<egui::Id>,
    popup_rect: Option<egui::Rect>,
}

fn transcript_delivery_copy(direct_insertion_available: bool) -> (&'static str, &'static str) {
    if direct_insertion_available {
        (
            "Insert final transcript",
            "Insert the final transcript into the captured app automatically. If insertion is unavailable, the transcript remains on the clipboard.",
        )
    } else {
        (
            "Copy final transcript automatically",
            "Copy the final transcript to the clipboard automatically.",
        )
    }
}

fn settings_section(ui: &mut egui::Ui, title: &str, contents: impl FnOnce(&mut egui::Ui)) {
    SettingsSection::show(ui, title, contents);
}

fn setting_row(ui: &mut egui::Ui, label: &str, contents: impl FnOnce(&mut egui::Ui, egui::Id)) {
    setting_row_with_separator(ui, label, false, contents);
}

fn setting_row_with_separator(
    ui: &mut egui::Ui,
    label: &str,
    separator_after: bool,
    contents: impl FnOnce(&mut egui::Ui, egui::Id),
) {
    let _ = SettingsRow::show(ui, label, separator_after, contents);
}

fn compact_setting_row(
    ui: &mut egui::Ui,
    label: &str,
    separator_after: bool,
    contents: impl FnOnce(&mut egui::Ui, egui::Id),
) {
    let _ = SettingsRow::show(ui, label, separator_after, contents);
}

fn next_tab(tab: SettingsTab) -> SettingsTab {
    match tab {
        SettingsTab::General => SettingsTab::Recording,
        SettingsTab::Recording | SettingsTab::Output => SettingsTab::Advanced,
        SettingsTab::Advanced => SettingsTab::About,
        SettingsTab::About => SettingsTab::General,
    }
}
fn previous_tab(tab: SettingsTab) -> SettingsTab {
    match tab {
        SettingsTab::General => SettingsTab::About,
        SettingsTab::Recording => SettingsTab::General,
        SettingsTab::Output => SettingsTab::Recording,
        SettingsTab::Advanced => SettingsTab::Recording,
        SettingsTab::About => SettingsTab::Advanced,
    }
}
fn format_bytes(bytes: u64) -> String {
    if bytes >= 1_000_000_000 {
        format!("{:.1}GB", bytes as f64 / 1_000_000_000.0)
    } else {
        format!("{}MB", bytes / 1_000_000)
    }
}

fn format_download_bytes(bytes: u64) -> String {
    const MB: u64 = 1_000_000;
    const GB: u64 = 1_000_000_000;
    if bytes >= GB {
        format!("{:.1}GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.1}MB", bytes as f64 / MB as f64)
    } else {
        format!("{bytes}B")
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
#[allow(dead_code)]
fn speed_label(tier: ModelSpeedTier) -> &'static str {
    match tier {
        ModelSpeedTier::VeryFast => "Very Fast",
        ModelSpeedTier::Fast => "Fast",
        ModelSpeedTier::Balanced => "Balanced Speed",
        ModelSpeedTier::AccurateSlow => "Accurate, slower",
        ModelSpeedTier::Unknown => "Speed unknown",
    }
}
#[allow(dead_code)]
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
    fn active_badge_follows_first_title_row_and_stays_bounded() {
        let identity = egui::Rect::from_min_size(egui::pos2(10.0, 20.0), Vec2::new(300.0, 44.0));
        let badge = Vec2::new(60.0, 22.0);
        let short = active_badge_rect(identity, egui::pos2(36.0, 30.0), 80.0, 40.0, badge);
        assert_eq!(short.left(), 124.0);
        assert_eq!(short.center().y, 40.0);
        assert!(identity.contains_rect(short));

        let wrapped = active_badge_rect(identity, egui::pos2(36.0, 30.0), 260.0, 40.0, badge);
        assert_eq!(wrapped.right(), identity.right());
        assert_eq!(wrapped.center().y, 40.0);
        assert!(identity.contains_rect(wrapped));
    }

    #[test]
    fn stable_delete_uses_destructive_outline_without_changing_action() {
        let stable = ModelViewModel {
            id: "stable".into(),
            installed: true,
            download_state: ModelDownloadState::Installed,
            removal_supported: true,
            ..Default::default()
        };
        let stable = model_lifecycle_presentation(ModelCard::Local(&stable), true);
        assert!(matches!(
            stable.action,
            ScreenAction::RequestModelRemoval(_)
        ));
        assert_eq!(stable.tone, ModelLifecycleTone::DestructiveOutline);

        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        crate::ui::controls::configure_accessible_style(&ctx);
        let mut expected_error = Color32::PLACEHOLDER;
        let output = ctx.run(Default::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                expected_error = ui_palette(ui).error_text;
                model_lifecycle_button(
                    ui,
                    "Delete",
                    "Delete stable",
                    true,
                    None,
                    ModelLifecycleTone::DestructiveOutline,
                );
            });
        });
        let trash = icon_glyph(Icon::Trash);
        let text = output
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::epaint::Shape::Text(text) if text.galley.text().contains("Delete") => {
                    Some(text)
                }
                _ => None,
            })
            .expect("stable Delete text");
        assert_eq!(text.galley.text().matches(trash).count(), 1);
        assert!(text.galley.text().contains("Delete"));
        let outline = output
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::epaint::Shape::Rect(rect)
                    if rect.stroke == Stroke::new(1.0, expected_error)
                        && rect.rect.contains_rect(text.visual_bounding_rect()) =>
                {
                    Some(rect.rect)
                }
                _ => None,
            })
            .expect("stable Delete error outline");
        assert!(outline.contains_rect(text.visual_bounding_rect()));
        let delete = output
            .platform_output
            .accesskit_update
            .expect("Delete accessibility update")
            .nodes
            .into_iter()
            .find_map(|(_, node)| (node.name() == Some("Delete stable")).then_some(node))
            .expect("Delete accessibility node");
        let bounds = delete.bounds().expect("Delete target bounds");
        assert!(bounds.width() >= 44.0 && bounds.height() >= 44.0);

        for model in [
            ModelViewModel {
                installed: true,
                download_state: ModelDownloadState::Installed,
                primary_action_installs_upgrade: true,
                ..Default::default()
            },
            ModelViewModel {
                installed: true,
                download_state: ModelDownloadState::Installed,
                primary_action_repairs_runtime: true,
                ..Default::default()
            },
            ModelViewModel {
                download_state: ModelDownloadState::Downloading,
                ..Default::default()
            },
            ModelViewModel {
                download_state: ModelDownloadState::Downloading,
                ..Default::default()
            },
        ] {
            assert_eq!(
                model_lifecycle_presentation(ModelCard::Local(&model), true).tone,
                ModelLifecycleTone::Standard
            );
        }

        for state in [ModelDownloadState::Failed, ModelDownloadState::Cancelled] {
            let model = ModelViewModel {
                download_state: state,
                ..Default::default()
            };
            let presentation = model_lifecycle_presentation(ModelCard::Local(&model), true);
            assert_eq!(presentation.tone, ModelLifecycleTone::InverseFilled);
        }
    }

    #[test]
    fn model_download_progress_is_local_truthful_and_clamped() {
        let downloading = ModelViewModel {
            download_state: ModelDownloadState::Downloading,
            downloaded_bytes: 120,
            total_bytes: Some(100),
            ..Default::default()
        };
        assert_eq!(
            model_download_progress_presentation(ModelCard::Local(&downloading)),
            Some(ModelDownloadProgressPresentation {
                downloaded_bytes: 120,
                total_bytes: Some(100),
                fraction: Some(1.0),
                total_is_unknown: false,
                display_text: "Downloading 120B of 100B (100%)".into(),
                accessible_text: "Downloading 120B of 100B, 100% complete".into(),
            })
        );

        let unknown_total = ModelViewModel {
            download_state: ModelDownloadState::Downloading,
            downloaded_bytes: 42,
            total_bytes: None,
            ..Default::default()
        };
        assert_eq!(
            model_download_progress_presentation(ModelCard::Local(&unknown_total)),
            Some(ModelDownloadProgressPresentation {
                downloaded_bytes: 42,
                total_bytes: None,
                fraction: None,
                total_is_unknown: true,
                display_text: "Downloading 42B".into(),
                accessible_text: "Downloading 42B; total download size unknown".into(),
            })
        );
        assert_eq!(model_download_label(&unknown_total), "Downloading 42B");
        let not_downloading = ModelViewModel::default();
        assert_eq!(
            model_download_progress_presentation(ModelCard::Local(&not_downloading)),
            None
        );
    }

    #[test]
    fn stable_install_uses_inverse_fill_without_changing_action_or_accessibility() {
        let install = ModelViewModel {
            id: "stable-install".into(),
            display_name: "Stable install".into(),
            install_supported: true,
            install_action_enabled: true,
            total_bytes: Some(1_500_000_000),
            ..Default::default()
        };
        let install = model_lifecycle_presentation(ModelCard::Local(&install), true);
        assert_eq!(
            install.action,
            ScreenAction::InstallModel("stable-install".into())
        );
        assert_eq!(install.tone, ModelLifecycleTone::InverseFilled);
        assert_eq!(install.accessible_name, "Install Stable install");
        assert_eq!(install.compact_size.as_deref(), Some("1.5 GB"));

        let remote_entry = RemoteCatalogEntryView {
            id: "trusted/stable".into(),
            display_name: "Remote stable".into(),
            ..Default::default()
        };
        let remote_variant = RemoteCatalogVariantView {
            id: "stable-q5".into(),
            size_bytes: 82_000_000,
            actions: vec![RemoteCatalogActionView {
                label: "Install".into(),
                kind: RemoteCatalogActionKind::Install {
                    remote_model_id: remote_entry.id.clone(),
                    variant_id: "stable-q5".into(),
                },
                enabled: true,
                disabled_reason: None,
            }],
            ..Default::default()
        };
        let remote =
            model_lifecycle_presentation(ModelCard::Remote(&remote_entry, &remote_variant), true);
        assert_eq!(remote.tone, ModelLifecycleTone::InverseFilled);
        assert_eq!(remote.accessible_name, "Install Remote stable");
        assert_eq!(remote.compact_size.as_deref(), Some("82 MB"));

        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        crate::ui::controls::configure_accessible_style(&ctx);
        let mut expected_fill = Color32::PLACEHOLDER;
        let output = ctx.run(Default::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                expected_fill = ui_palette(ui).inverse_neutral_bg;
                model_lifecycle_button(
                    ui,
                    &format!("{}  1.5 GB", icon_glyph(Icon::Download)),
                    &install.accessible_name,
                    install.enabled,
                    install.disabled_reason,
                    install.tone,
                );
            });
        });
        let install_text = output
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::epaint::Shape::Text(text)
                    if text.galley.text().contains(icon_glyph(Icon::Download))
                        && text.galley.text().contains("1.5 GB") =>
                {
                    Some(text)
                }
                _ => None,
            })
            .expect("Install glyph and compact size");
        assert!(output.shapes.iter().any(|shape| {
            matches!(
                &shape.shape,
                egui::epaint::Shape::Rect(rect)
                    if rect.fill == expected_fill
                        && rect.rect.contains_rect(install_text.visual_bounding_rect())
            )
        }));
        let node = output
            .platform_output
            .accesskit_update
            .expect("Install accessibility update")
            .nodes
            .into_iter()
            .find_map(|(_, node)| (node.name() == Some("Install Stable install")).then_some(node))
            .expect("Install accessibility node");
        let bounds = node.bounds().expect("Install target bounds");
        assert!(bounds.width() >= 44.0 && bounds.height() >= 44.0);

        let disabled_ctx = egui::Context::default();
        disabled_ctx.enable_accesskit();
        crate::ui::controls::configure_accessible_style(&disabled_ctx);
        let disabled = disabled_ctx.run(Default::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                model_lifecycle_button(
                    ui,
                    &format!("{}  1.5 GB", icon_glyph(Icon::Download)),
                    "Install unavailable",
                    false,
                    Some("The download is unavailable."),
                    ModelLifecycleTone::InverseFilled,
                );
            });
        });
        let disabled_node = disabled
            .platform_output
            .accesskit_update
            .expect("disabled Install accessibility update")
            .nodes
            .into_iter()
            .find_map(|(_, node)| (node.name() == Some("Install unavailable")).then_some(node))
            .expect("disabled Install accessibility node");
        assert!(disabled_node.is_disabled());
        assert_eq!(
            disabled_node.description(),
            Some("The download is unavailable.")
        );
    }

    #[test]
    fn model_card_visual_states_keep_geometry_and_use_approved_tokens() {
        let colors = crate::ui::theme::ThemePalette::light();
        let (idle_fill, idle_stroke, idle_shadow) =
            model_card_visual_style(colors, ModelCardVisualState::Idle);
        let (active_fill, active_stroke, active_shadow) =
            model_card_visual_style(colors, ModelCardVisualState::Active);

        assert_eq!(idle_fill, colors.card_bg);
        assert_eq!(idle_stroke, Stroke::new(1.0, colors.border));
        assert_eq!(idle_shadow.offset, Vec2::new(0.0, 1.0));
        assert_eq!(idle_shadow.blur, 6.0);
        assert_eq!(idle_shadow.spread, 0.0);
        assert_eq!(active_fill, colors.panel_bg);
        assert_eq!(active_stroke, Stroke::new(2.0, colors.accent));
        assert_eq!(active_shadow.offset, Vec2::new(0.0, 6.0));
        assert_eq!(active_shadow.blur, 18.0);
        assert_eq!(active_shadow.spread, 1.0);
        assert_eq!(MODEL_CARD_SHADOW_GUTTER, 6.0);
    }

    #[test]
    fn description_fade_tracks_the_resolved_card_surface_in_both_themes() {
        for colors in [
            crate::ui::theme::ThemePalette::light(),
            crate::ui::theme::ThemePalette::dark(),
        ] {
            let (idle_fill, _, _) = model_card_visual_style(colors, ModelCardVisualState::Idle);
            let (active_fill, _, _) = model_card_visual_style(colors, ModelCardVisualState::Active);
            assert_ne!(idle_fill, active_fill);
            for (surface, state) in [
                (idle_fill, ModelCardVisualState::Idle),
                (active_fill, ModelCardVisualState::Active),
            ] {
                for step in 0..MODEL_DESCRIPTION_FADE_STEPS {
                    let fade = description_fade_color(surface, step);
                    assert_eq!(
                        (fade.r(), fade.g(), fade.b()),
                        (surface.r(), surface.g(), surface.b())
                    );
                    assert_eq!(fade.a(), description_fade_alpha(step));
                }
                assert_eq!(
                    model_card_visual_style(colors, state).0,
                    surface,
                    "fade must use the resolved {state:?} card surface",
                );
            }
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
                        model_language_filter: ModelLanguageFilter::default(),
                        remote_catalog: &Default::default(),
                        recording_settings: &settings,
                    },
                )
            });
        })
    }

    fn render_output_settings_with_input(
        ctx: &egui::Context,
        settings: &RecordingSettingsView,
        events: Vec<egui::Event>,
    ) -> (egui::FullOutput, ScreenAction) {
        let mut action = ScreenAction::None;
        let output = ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    Vec2::new(900.0, 600.0),
                )),
                events,
                focused: true,
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    output_settings_panel(ui, settings, &mut action);
                });
            },
        );
        (output, action)
    }

    fn render_voice_detection_with_input(
        ctx: &egui::Context,
        settings: &RecordingSettingsView,
        events: Vec<egui::Event>,
    ) -> (egui::FullOutput, ScreenAction) {
        let mut action = ScreenAction::None;
        let output = ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    Vec2::new(900.0, 600.0),
                )),
                events,
                focused: true,
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    voice_detection_settings_section(
                        ui,
                        &TranscriptionState::default(),
                        settings,
                        &mut action,
                    );
                });
            },
        );
        (output, action)
    }

    fn render_recording_settings_with_input(
        ctx: &egui::Context,
        settings_view: &RecordingSettingsView,
        events: Vec<egui::Event>,
    ) -> (egui::FullOutput, ScreenAction) {
        render_settings_with_input(
            ctx,
            SettingsTab::Recording,
            &TranscriptionState::default(),
            settings_view,
            events,
        )
    }

    fn render_settings_with_input(
        ctx: &egui::Context,
        tab: SettingsTab,
        state: &TranscriptionState,
        settings_view: &RecordingSettingsView,
        events: Vec<egui::Event>,
    ) -> (egui::FullOutput, ScreenAction) {
        render_settings_with_input_at(ctx, tab, state, settings_view, events, None, 900.0)
    }

    fn render_settings_with_input_at(
        ctx: &egui::Context,
        tab: SettingsTab,
        state: &TranscriptionState,
        settings_view: &RecordingSettingsView,
        events: Vec<egui::Event>,
        time: Option<f64>,
        width: f32,
    ) -> (egui::FullOutput, ScreenAction) {
        let mut action = ScreenAction::None;
        let output = ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    Vec2::new(width, 3_000.0),
                )),
                events,
                time,
                focused: true,
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    action = settings(ui, tab, state, settings_view);
                });
            },
        );
        (output, action)
    }

    fn click_settings_switch(
        tab: SettingsTab,
        settings_view: &RecordingSettingsView,
        switch_name: &str,
    ) -> (egui::FullOutput, ScreenAction) {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let state = TranscriptionState::default();
        let (initial, action) =
            render_settings_with_input(&ctx, tab, &state, settings_view, Vec::new());
        assert_eq!(action, ScreenAction::None);
        let (bounds, initially_checked) = initial
            .platform_output
            .accesskit_update
            .as_ref()
            .expect("settings should expose AccessKit")
            .nodes
            .iter()
            .find_map(|(_, node)| {
                (node.role() == egui::accesskit::Role::Switch && node.name() == Some(switch_name))
                    .then(|| node.bounds().zip(node.checked()))
                    .flatten()
            })
            .unwrap_or_else(|| panic!("missing {switch_name} switch"));
        let point = egui::pos2(
            ((bounds.x0 + bounds.x1) / 2.0) as f32,
            ((bounds.y0 + bounds.y1) / 2.0) as f32,
        );
        let (_, press_action) = render_settings_with_input(
            &ctx,
            tab,
            &state,
            settings_view,
            vec![
                egui::Event::PointerMoved(point),
                egui::Event::PointerButton {
                    pos: point,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
        assert_eq!(press_action, ScreenAction::None);
        let result = render_settings_with_input(
            &ctx,
            tab,
            &state,
            settings_view,
            vec![
                egui::Event::PointerMoved(point),
                egui::Event::PointerButton {
                    pos: point,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
        let updated_switch = result
            .0
            .platform_output
            .accesskit_update
            .as_ref()
            .expect("updated settings should expose AccessKit")
            .nodes
            .iter()
            .find_map(|(_, node)| {
                (node.role() == egui::accesskit::Role::Switch && node.name() == Some(switch_name))
                    .then_some(node)
            })
            .unwrap_or_else(|| panic!("missing updated {switch_name} switch"));
        assert_eq!(
            updated_switch.checked(),
            Some(match initially_checked {
                egui::accesskit::Checked::True => egui::accesskit::Checked::False,
                egui::accesskit::Checked::False => egui::accesskit::Checked::True,
                other => panic!("unexpected checked state for {switch_name}: {other:?}"),
            })
        );
        result
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
                        model_language_filter: ModelLanguageFilter::default(),
                        remote_catalog: &Default::default(),
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
    fn relative_capture_time_uses_the_compact_reference_labels() {
        assert_eq!(format_relative_capture_time(0), "JUST NOW");
        assert_eq!(format_relative_capture_time(60_000), "1 MIN AGO");
        assert_eq!(format_relative_capture_time(120_000), "2 MINS AGO");
    }

    #[test]
    fn transcribe_metadata_and_hotkey_preserve_visible_and_semantic_labels() {
        let state = TranscriptionState {
            phase: TranscriptionPhase::Ready,
            selected_model_id: Some("base.en".into()),
            last_successful_capture_ms: Some(120_000),
            hotkey: "Ctrl + Space".into(),
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
        for name in ["+", "2 MINS AGO", "BASE.EN"] {
            assert!(
                nodes.iter().any(|(_, node)| node.name() == Some(name)),
                "missing visible Transcribe label {name}"
            );
        }
        assert!(nodes.iter().any(|(_, node)| {
            node.role() == egui::accesskit::Role::Group && node.name() == Some("Recording hotkey")
        }));
    }

    #[test]
    fn transcript_panel_keeps_its_semantic_name_without_a_visible_heading() {
        let state = TranscriptionState {
            phase: TranscriptionPhase::Ready,
            selected_model_id: Some("base.en".into()),
            ..Default::default()
        };
        let output = render_transcribe(&state, &[]);
        let nodes = &output.platform_output.accesskit_update.unwrap().nodes;
        assert!(nodes.iter().any(|(_, node)| {
            node.role() == egui::accesskit::Role::Group && node.name() == Some("Transcript panel")
        }));
        assert!(!nodes.iter().any(|(_, node)| {
            node.role() == egui::accesskit::Role::Heading && node.name() == Some("Transcript")
        }));
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

        let repeated = TranscriptionState {
            phase: TranscriptionPhase::MicrophoneError,
            selected_model_id: Some("base.en".into()),
            notice: Some("Scribe couldn’t access your microphone".into()),
            ..Default::default()
        };
        let output = render_transcribe(&repeated, &[]);
        let canonical_count = output
            .platform_output
            .accesskit_update
            .unwrap()
            .nodes
            .iter()
            .filter(|(_, node)| node.name() == Some(MICROPHONE_ACCESS_ERROR))
            .count();
        assert_eq!(
            canonical_count, 1,
            "canonical microphone message was repeated"
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
    fn dense_model_rows_keep_variant_details_out_of_the_summary() {
        let model = ModelViewModel {
            display_name: "whisper.cpp base.en".into(),
            variant_label: "base.en".into(),
            ..Default::default()
        };
        assert_eq!(model.display_name, "whisper.cpp base.en");
        assert_eq!(model.variant_label, "base.en");
    }

    #[test]
    fn compact_language_codes_are_unique_and_bounded() {
        assert_eq!(
            formatted_language_summary(&["en".into(), "English".into(), "es".into(), "ja".into()]),
            "EN,ES,JA"
        );
        assert_eq!(
            formatted_language_summary(&["en".into(), "es".into(), "ja".into(), "ko".into()]),
            "Multilingual"
        );
        assert_eq!(
            formatted_language_summary(&["it".into(), "ru".into(), "ar".into()]),
            "IT,RU,AR"
        );
        assert_eq!(formatted_language_summary(&["unknown".into()]), "—");
    }

    #[test]
    fn compact_rating_meters_map_only_catalog_authored_values() {
        assert_eq!(
            speed_rating(ModelSpeedTier::VeryFast),
            Some((5, "Very fast"))
        );
        assert_eq!(speed_rating(ModelSpeedTier::Fast), Some((4, "Fast")));
        assert_eq!(
            speed_rating(ModelSpeedTier::Balanced),
            Some((3, "Balanced"))
        );
        assert_eq!(
            speed_rating(ModelSpeedTier::AccurateSlow),
            Some((2, "Slow"))
        );
        assert_eq!(speed_rating(ModelSpeedTier::Unknown), None);

        assert_eq!(accuracy_rating("Basic"), Some((1, "Basic")));
        assert_eq!(accuracy_rating("Fair"), Some((2, "Fair")));
        assert_eq!(accuracy_rating("Good"), Some((3, "Good")));
        assert_eq!(accuracy_rating("Better"), Some((4, "High")));
        assert_eq!(accuracy_rating("High"), Some((4, "High")));
        assert_eq!(accuracy_rating("Highest"), Some((5, "Highest")));
        assert_eq!(accuracy_rating("marketing copy"), None);
    }

    #[test]
    fn compact_artifact_size_uses_whole_mb_below_one_gb_and_one_decimal_above_it() {
        assert_eq!(format_compact_artifact_size(0), "1 MB");
        assert_eq!(format_compact_artifact_size(999_999_999), "999 MB");
        assert_eq!(format_compact_artifact_size(1_000_000_000), "1.0 GB");
        assert_eq!(format_compact_artifact_size(1_550_000_000), "1.6 GB");
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
                        model_language_filter: ModelLanguageFilter::default(),
                        remote_catalog: &Default::default(),
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
                        model_language_filter: ModelLanguageFilter::default(),
                        remote_catalog: &Default::default(),
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
            !update
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
    fn import_dialog_is_modal_labelled_and_excludes_catalog_models() {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let catalog_model = ModelViewModel {
            id: "base.en".into(),
            display_name: "Catalog model must stay outside import dialog".into(),
            variant_label: "base.en".into(),
            ..Default::default()
        };
        let remote_catalog = RemoteCatalogView {
            local_import: super::super::state::LocalGgufImportView {
                path: "C:\\Models\\candidate.gguf".into(),
                import_enabled: false,
                disabled_reason: Some("Choose a readable .gguf file.".into()),
                ..Default::default()
            },
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
                        model_catalog: &[catalog_model],
                        comparison: &Default::default(),
                        model_management: &ModelManagementState {
                            dialog: Some(ModelDialog::Add),
                            focus_dialog_initial: true,
                            ..Default::default()
                        },
                        model_language_filter: ModelLanguageFilter::default(),
                        remote_catalog: &remote_catalog,
                        recording_settings: &Default::default(),
                    },
                )
            });
        });
        let update = output.platform_output.accesskit_update.unwrap();
        let dialog = update
            .nodes
            .iter()
            .find(|(_, node)| {
                node.role() == egui::accesskit::Role::Dialog
                    && node.name() == Some("Import local GGUF")
            })
            .expect("import dialog");
        assert!(dialog.1.is_modal());
        let path_label_id = update
            .nodes
            .iter()
            .find_map(|(id, node)| {
                (node.role() == egui::accesskit::Role::StaticText
                    && node.name() == Some("GGUF file path"))
                .then_some(id)
            })
            .expect("path label");
        let path_input_id = update
            .nodes
            .iter()
            .find_map(|(id, node)| {
                (node.role() == egui::accesskit::Role::TextInput
                    && node.labelled_by().contains(path_label_id))
                .then_some(id)
            })
            .expect("labelled path input");
        assert_eq!(update.focus, *path_input_id);
        for name in ["Validate and import", "Close"] {
            assert!(update.nodes.iter().any(|(_, node)| {
                node.role() == egui::accesskit::Role::Button && node.name() == Some(name)
            }));
        }
        assert!(update.nodes.iter().any(|(_, node)| {
            node.role() == egui::accesskit::Role::Button
                && node.name() == Some("Validate and import")
                && node.is_disabled()
                && node.description() == Some("Choose a readable .gguf file.")
        }));
        assert!(update.nodes.iter().any(|(_, node)| {
            node.role() == egui::accesskit::Role::Status
                && node.name() == Some("1 model results: 0 installed, 1 available.")
                && node.live() == Some(egui::accesskit::Live::Polite)
                && node.is_live_atomic()
        }));
        let (catalog_id, _) = update
            .nodes
            .iter()
            .find(|(_, node)| {
                node.role() == egui::accesskit::Role::Group
                    && node.name() == Some("Catalog model must stay outside import dialog model")
            })
            .expect("catalog row group behind modal");
        assert!(!dialog.1.children().contains(catalog_id));
    }

    #[test]
    fn model_card_languages_and_description_contracts_are_truthful() {
        assert_eq!(
            formatted_language_summary(&["en".into(), "ES".into(), "ja".into()]),
            "EN,ES,JA"
        );
        assert_eq!(
            formatted_language_summary(&["en".into(), "es".into(), "ja".into(), "ko".into()]),
            "Multilingual"
        );
        assert_eq!(
            model_language_summary(&["it".into(), "ru".into(), "ar".into()]),
            ("Languages", "IT,RU,AR".into())
        );
        assert_eq!(
            model_language_summary(&["klingon".into()]),
            ("Languages unavailable", "\u{2014}".into())
        );
        assert!(!MODEL_DESCRIPTION_FADE_WIDTH.is_sign_negative());
        let alphas = (0..MODEL_DESCRIPTION_FADE_STEPS)
            .map(description_fade_alpha)
            .collect::<Vec<_>>();
        assert!(alphas.windows(2).all(|pair| pair[0] < pair[1]));
        assert_eq!(alphas.last(), Some(&u8::MAX));
    }

    #[test]
    fn decorative_model_card_icons_do_not_create_accessibility_nodes() {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let models = [
            ModelViewModel {
                id: "active".into(),
                display_name: "Active model".into(),
                installed: true,
                active: true,
                ready: true,
                removal_supported: true,
                languages: vec!["en".into()],
                ..Default::default()
            },
            ModelViewModel {
                id: "inactive".into(),
                display_name: "Inactive model".into(),
                installed: true,
                ready: true,
                removal_supported: true,
                languages: vec!["en".into()],
                ..Default::default()
            },
        ];
        let output = ctx.run(Default::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                for model in &models {
                    let _ =
                        render_unified_model_card(ui, ModelCard::Local(model), false, true, false);
                }
            });
        });
        let names = output
            .platform_output
            .accesskit_update
            .unwrap()
            .nodes
            .into_iter()
            .filter_map(|(_, node)| node.name().map(str::to_owned))
            .collect::<Vec<_>>();
        for icon in [Icon::CheckCircle, Icon::Waveform, Icon::Globe] {
            assert!(!names.iter().any(|name| name == icon_glyph(icon)));
        }
    }

    #[test]
    fn legacy_model_upgrade_primary_dispatches_upgrade_action() {
        let model = ModelViewModel {
            id: "whisper_cpp_small_en".into(),
            display_name: "Whisper Small — English".into(),
            installed: false,
            legacy_cleanup_pending: true,
            download_state: ModelDownloadState::NotInstalled,
            primary_action_label: "Upgrade model".into(),
            primary_action_enabled: true,
            primary_action_installs_upgrade: true,
            removal_supported: true,
            ..Default::default()
        };
        let current = ModelViewModel {
            id: "whisper_cpp_tiny_en".into(),
            installed: true,
            ready: true,
            ..Default::default()
        };
        let catalog = vec![current, model.clone()];
        let remote_catalog = RemoteCatalogView::default();
        let (installed, available) = build_model_card_lists(
            &catalog,
            &catalog,
            &remote_catalog,
            ModelLanguageFilter::All,
        );

        assert_eq!(installed.len(), 1);
        assert_eq!(available.len(), 1);
        assert_eq!(
            installed[0].key(),
            ModelCardKey::Local("whisper_cpp_tiny_en".into())
        );
        assert_eq!(
            available[0].key(),
            ModelCardKey::Local("whisper_cpp_small_en".into())
        );
        assert_eq!(
            local_model_primary_action(&model),
            ScreenAction::UpgradeModel("whisper_cpp_small_en".into())
        );
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
                        model_language_filter: ModelLanguageFilter::default(),
                        remote_catalog: &Default::default(),
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
            (
                SettingsTab::Output,
                "General settings",
                "Recording behavior",
            ),
            (
                SettingsTab::Advanced,
                "Voice detection",
                "Recording behavior",
            ),
            (SettingsTab::About, "Application", "Recording behavior"),
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
                                SettingsTab::Output => "General settings",
                                SettingsTab::Advanced => "Advanced settings",
                                SettingsTab::About => "About Scribe",
                            }))
            );
            assert!(nodes.iter().any(|(_, node)| node.name() == Some(expected)));
            assert!(!nodes.iter().any(|(_, node)| node.name() == Some(absent)));
        }
    }

    #[test]
    fn settings_tabs_own_exact_sections_and_controls() {
        let settings_view = RecordingSettingsView {
            auto_insert_transcript: true,
            show_restore_clipboard: true,
            debug_mode: true,
            history_mode_label: "Transcript and audio".into(),
            transcript_retention_days: Some(30),
            audio_retention_days: Some(14),
            ..Default::default()
        };
        let output_switch_name = transcript_delivery_copy(settings_view.show_restore_clipboard).0;
        let rendered = [
            SettingsTab::General,
            SettingsTab::Recording,
            SettingsTab::Advanced,
            SettingsTab::About,
        ]
        .map(|tab| {
            let ctx = egui::Context::default();
            ctx.enable_accesskit();
            let output = ctx.run(Default::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let _ = settings(ui, tab, &TranscriptionState::default(), &settings_view);
                });
            });
            let names = output
                .platform_output
                .accesskit_update
                .expect("settings should expose AccessKit")
                .nodes
                .into_iter()
                .filter_map(|(_, node)| node.name().map(str::to_owned))
                .collect::<Vec<_>>();
            (tab, names)
        });

        for (owner, owned_names) in [
            (
                SettingsTab::General,
                &[
                    "General settings",
                    "Appearance",
                    "Output settings",
                    "Close to tray",
                    "Manage models",
                    "Theme",
                    "Dictation overlay",
                    "Overlay position",
                    output_switch_name,
                ][..],
            ),
            (
                SettingsTab::Recording,
                &[
                    "Recording behavior",
                    "Recording input",
                    "Transcription",
                    "Recording mode",
                    "Global record hotkey",
                    "Change shortcut",
                    "Input level",
                    "Live transcription preview",
                    "Streaming mode",
                    "Transcription device",
                ][..],
            ),
            (
                SettingsTab::Advanced,
                &[
                    "Voice detection",
                    "History and privacy",
                    "Developer and diagnostics",
                    "Stop after speech ends",
                    "Speech confirmation ms",
                    "History storage",
                    "Enable model Playground",
                    "Open model Playground",
                    "Export redacted diagnostics",
                ][..],
            ),
            (
                SettingsTab::About,
                &[
                    "Application",
                    "Scribe",
                    "Local-first privacy",
                    "Local paths",
                ][..],
            ),
        ] {
            for name in owned_names {
                for (tab, rendered_names) in &rendered {
                    assert_eq!(
                        rendered_names.iter().any(|rendered| rendered == name),
                        *tab == owner,
                        "{name} must render only on {owner:?}, not {tab:?}"
                    );
                }
            }
        }

        for removed_duplicate in ["Capture hotkey", "Apply"] {
            assert!(
                rendered
                    .iter()
                    .all(|(_, names)| !names.iter().any(|name| name == removed_duplicate))
            );
        }
    }

    #[test]
    fn recording_to_advanced_accesskit_update_keeps_updated_nodes_attached() {
        type AccessKitNodes =
            std::collections::HashMap<egui::accesskit::NodeId, egui::accesskit::Node>;
        type IncrementalUpdateResult = (
            AccessKitNodes,
            egui::accesskit::NodeId,
            Vec<(egui::accesskit::NodeId, Option<String>)>,
        );

        fn apply_incremental_update(
            initial: &egui::accesskit::TreeUpdate,
            update: &egui::accesskit::TreeUpdate,
        ) -> IncrementalUpdateResult {
            let mut nodes = initial
                .nodes
                .iter()
                .cloned()
                .collect::<std::collections::HashMap<_, _>>();
            let mut orphans = std::collections::HashSet::new();
            let mut updated = std::collections::HashSet::new();
            let old_root = initial
                .tree
                .as_ref()
                .expect("initial AccessKit update should define the tree")
                .root;
            if update
                .tree
                .as_ref()
                .is_some_and(|tree| tree.root != old_root)
            {
                orphans.insert(old_root);
            }
            for (id, data) in &update.nodes {
                orphans.remove(id);
                for child in data.children() {
                    orphans.remove(child);
                }
                if let Some(old) = nodes.insert(*id, data.clone()) {
                    updated.insert(*id);
                    for child in old.children() {
                        if !data.children().contains(child) {
                            orphans.insert(*child);
                        }
                    }
                }
            }

            let mut removed = std::collections::HashSet::new();
            let mut pending = orphans.into_iter().collect::<Vec<_>>();
            while let Some(id) = pending.pop() {
                if removed.insert(id)
                    && let Some(node) = nodes.get(&id)
                {
                    pending.extend(node.children());
                }
            }
            let orphaned_updated = updated
                .intersection(&removed)
                .map(|id| {
                    (
                        *id,
                        nodes
                            .get(id)
                            .and_then(|node| node.name())
                            .map(str::to_owned),
                    )
                })
                .collect();
            for id in removed {
                nodes.remove(&id);
            }
            let root = update.tree.as_ref().map_or(old_root, |tree| tree.root);
            (nodes, root, orphaned_updated)
        }

        fn is_descendant(
            nodes: &AccessKitNodes,
            ancestor: egui::accesskit::NodeId,
            target: egui::accesskit::NodeId,
        ) -> bool {
            nodes.get(&ancestor).is_some_and(|node| {
                node.children()
                    .iter()
                    .any(|child| *child == target || is_descendant(nodes, *child, target))
            })
        }

        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let state = TranscriptionState::default();
        let settings_view = RecordingSettingsView {
            debug_mode: true,
            history_mode_label: "Transcript and audio".into(),
            transcript_retention_days: Some(30),
            audio_retention_days: Some(14),
            ..Default::default()
        };
        let render = |tab| {
            ctx.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        Vec2::new(1180.0, 815.0),
                    )),
                    focused: true,
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        let _ = settings(ui, tab, &state, &settings_view);
                    });
                },
            )
            .platform_output
            .accesskit_update
            .expect("settings should expose AccessKit")
        };

        let recording = render(SettingsTab::Recording);
        let advanced = render(SettingsTab::Advanced);
        let (nodes, root, orphaned_updated) = apply_incremental_update(&recording, &advanced);
        assert_eq!(orphaned_updated, Vec::new());
        let panel = nodes
            .iter()
            .find_map(|(id, node)| {
                (node.role() == egui::accesskit::Role::TabPanel
                    && node.name() == Some("Advanced settings"))
                .then_some(*id)
            })
            .expect("Advanced should expose one TabPanel");
        let automatic_stop = nodes
            .iter()
            .find_map(|(id, node)| (node.name() == Some("Stop after speech ends")).then_some(*id))
            .expect("Advanced should expose the Stop after speech ends row");
        assert!(is_descendant(&nodes, panel, automatic_stop));
        assert!(
            !nodes
                .get(&root)
                .expect("AccessKit root should remain attached")
                .children()
                .contains(&automatic_stop)
        );
    }

    #[test]
    fn settings_tab_auto_id_ranges_are_disjoint_and_have_headroom() {
        fn collect_descendants(
            update: &egui::accesskit::TreeUpdate,
            parent: egui::accesskit::NodeId,
            descendants: &mut std::collections::HashSet<egui::accesskit::NodeId>,
        ) {
            let Some(node) = update
                .nodes
                .iter()
                .find_map(|(id, node)| (*id == parent).then_some(node))
            else {
                return;
            };
            for child in node.children() {
                if descendants.insert(*child) {
                    collect_descendants(update, *child, descendants);
                }
            }
        }

        let settings_view = RecordingSettingsView {
            auto_insert_transcript: true,
            show_restore_clipboard: true,
            debug_mode: true,
            history_mode_label: "Transcript and audio".into(),
            transcript_retention_days: Some(30),
            audio_retention_days: Some(14),
            ..Default::default()
        };
        let rendered = [
            SettingsTab::General,
            SettingsTab::Recording,
            SettingsTab::Advanced,
            SettingsTab::About,
        ]
        .map(|tab| {
            let ctx = egui::Context::default();
            ctx.enable_accesskit();
            let output = ctx.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        Vec2::new(1180.0, 3_000.0),
                    )),
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        let _ = settings(ui, tab, &TranscriptionState::default(), &settings_view);
                    });
                },
            );
            let update = output
                .platform_output
                .accesskit_update
                .expect("settings should expose AccessKit");
            let panel = update
                .nodes
                .iter()
                .find_map(|(id, node)| {
                    (node.role() == egui::accesskit::Role::TabPanel).then_some(*id)
                })
                .expect("settings should expose one TabPanel");
            let mut descendants = std::collections::HashSet::new();
            collect_descendants(&update, panel, &mut descendants);
            assert!(
                descendants.len() < SETTINGS_TAB_AUTO_ID_STRIDE / 10,
                "{tab:?} rendered {} panel nodes, exhausting its reserved auto-ID range",
                descendants.len()
            );
            (tab, descendants)
        });

        for (index, (tab, ids)) in rendered.iter().enumerate() {
            for (other_tab, other_ids) in rendered.iter().skip(index + 1) {
                assert!(
                    ids.is_disjoint(other_ids),
                    "{tab:?} and {other_tab:?} Settings panel IDs must be disjoint"
                );
            }
        }
    }

    #[test]
    fn about_settings_uses_one_nonredundant_heading_hierarchy() {
        let output = render_route(UiRoute::Settings(SettingsTab::About));
        let nodes = &output
            .platform_output
            .accesskit_update
            .expect("About settings should expose AccessKit")
            .nodes;
        let headings = nodes
            .iter()
            .filter_map(|(_, node)| {
                (node.role() == egui::accesskit::Role::Heading)
                    .then(|| node.name())
                    .flatten()
            })
            .collect::<Vec<_>>();
        assert_eq!(
            headings.iter().filter(|name| **name == "Settings").count(),
            1
        );
        assert_eq!(headings.iter().filter(|name| **name == "Scribe").count(), 1);
        assert!(!headings.contains(&"About Scribe"));
        for label in ["Application", "Local-first privacy", "Local paths"] {
            assert!(nodes.iter().any(|(_, node)| node.name() == Some(label)));
        }
        for moved_control in ["Diagnostics", "Export redacted diagnostics"] {
            assert!(
                !nodes
                    .iter()
                    .any(|(_, node)| node.name() == Some(moved_control))
            );
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
        let nodes = &output
            .platform_output
            .accesskit_update
            .expect("Recording settings should expose AccessKit")
            .nodes;
        assert!(nodes.iter().any(|(_, node)| {
            node.name() == Some("Finish recording before changing recording settings.")
        }));
        assert!(nodes.iter().any(|(_, node)| {
            node.role() == egui::accesskit::Role::Switch
                && node.name() == Some("Live transcription preview")
                && node.is_disabled()
        }));
    }

    #[test]
    fn voice_detection_explains_why_changes_are_disabled_while_busy() {
        let state = TranscriptionState {
            phase: TranscriptionPhase::Listening,
            ..Default::default()
        };
        let settings_view = RecordingSettingsView::default();
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let (output, action) = render_settings_with_input(
            &ctx,
            SettingsTab::Advanced,
            &state,
            &settings_view,
            Vec::new(),
        );
        assert_eq!(action, ScreenAction::None);
        let nodes = &output
            .platform_output
            .accesskit_update
            .expect("Advanced settings should expose AccessKit")
            .nodes;
        assert!(nodes.iter().any(|(_, node)| {
            node.name() == Some(VOICE_DETECTION_LOCKED_DESCRIPTION)
                && node.live() == Some(egui::accesskit::Live::Polite)
        }));
        let switch = nodes
            .iter()
            .find_map(|(_, node)| {
                (node.role() == egui::accesskit::Role::Switch
                    && node.name() == Some("Stop after speech ends"))
                .then_some(node)
            })
            .expect("voice detection switch should remain exposed while locked");
        assert!(switch.is_disabled());
        assert_eq!(
            switch.description(),
            Some(
                format!("{STOP_AFTER_SPEECH_DESCRIPTION} {VOICE_DETECTION_LOCKED_DESCRIPTION}")
                    .as_str()
            )
        );
        assert!(nodes.iter().any(|(_, node)| {
            node.role() == egui::accesskit::Role::Button
                && node.name() == Some("Stop after speech ends information")
                && !node.is_disabled()
        }));
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
            "Voice detection",
            "History and privacy",
            "History storage",
            "Maximum unpinned entries",
            "Limit transcript age",
            "Limit audio age",
            "Store application identity",
            "Enable model Playground",
            "Diagnostics",
            "Export redacted diagnostics",
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
        assert!(
            nodes
                .iter()
                .any(|(_, node)| node.name() == Some("Copy final transcript automatically"))
        );
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
        for (name, description) in [
            (
                "Press once",
                "Press the recording hotkey once to start, then press it again to stop.",
            ),
            (
                "Hold",
                "Hold the recording hotkey to record, then release it to stop.",
            ),
        ] {
            assert!(nodes.iter().any(|(_, node)| {
                node.role() == egui::accesskit::Role::RadioButton
                    && node.name() == Some(name)
                    && node.description() == Some(description)
            }));
        }
    }

    #[test]
    fn all_settings_switches_expose_distinct_accessible_states_and_help() {
        let settings_view = RecordingSettingsView {
            close_to_tray: true,
            auto_insert_transcript: true,
            show_restore_clipboard: true,
            restore_clipboard_after_insert: false,
            provisional_feedback: true,
            vad_enabled: false,
            history_mode_label: "Transcript and audio".into(),
            transcript_retention_days: Some(30),
            audio_retention_days: None,
            store_application_identity: true,
            debug_mode: false,
            ..Default::default()
        };
        let expected = [
            (
                SettingsTab::General,
                &[
                    ("Close to tray", CLOSE_TO_TRAY_DESCRIPTION, true),
                    (
                        "Insert final transcript",
                        "Insert the final transcript into the captured app automatically. If insertion is unavailable, the transcript remains on the clipboard.",
                        true,
                    ),
                    (
                        "Restore clipboard after insert",
                        RESTORE_CLIPBOARD_DESCRIPTION,
                        false,
                    ),
                ][..],
            ),
            (
                SettingsTab::Recording,
                &[(
                    "Live transcription preview",
                    LIVE_TRANSCRIPTION_PREVIEW_DESCRIPTION,
                    true,
                )][..],
            ),
            (
                SettingsTab::Advanced,
                &[
                    (
                        "Stop after speech ends",
                        STOP_AFTER_SPEECH_DESCRIPTION,
                        false,
                    ),
                    (
                        "Limit transcript age",
                        LIMIT_TRANSCRIPT_AGE_DESCRIPTION,
                        true,
                    ),
                    ("Limit audio age", LIMIT_AUDIO_AGE_DESCRIPTION, false),
                    (
                        "Store application identity",
                        STORE_APPLICATION_IDENTITY_DESCRIPTION,
                        true,
                    ),
                    (
                        "Enable model Playground",
                        ENABLE_MODEL_PLAYGROUND_DESCRIPTION,
                        false,
                    ),
                ][..],
            ),
        ];
        let mut switch_ids = HashSet::new();

        for (tab, switches) in expected {
            let ctx = egui::Context::default();
            ctx.enable_accesskit();
            let output = ctx.run(Default::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let _ = settings(ui, tab, &TranscriptionState::default(), &settings_view);
                });
            });
            let nodes = &output
                .platform_output
                .accesskit_update
                .expect("settings should expose AccessKit")
                .nodes;
            for (name, description, checked) in switches {
                let (id, switch) = nodes
                    .iter()
                    .find(|(_, node)| {
                        node.role() == egui::accesskit::Role::Switch && node.name() == Some(name)
                    })
                    .unwrap_or_else(|| panic!("missing {name} switch"));
                assert!(switch_ids.insert(*id), "switch IDs must be unique: {name}");
                assert_eq!(
                    switch.checked(),
                    Some(if *checked {
                        egui::accesskit::Checked::True
                    } else {
                        egui::accesskit::Checked::False
                    }),
                    "incorrect checked state for {name}",
                );
                assert_eq!(switch.description(), Some(*description));
                assert!(!switch.is_disabled());
                let bounds = switch.bounds().expect("switch bounds");
                assert!(bounds.x1 - bounds.x0 >= 52.0 && bounds.y1 - bounds.y0 >= 44.0);

                let help_name = format!("{name} information");
                let help = nodes
                    .iter()
                    .find_map(|(_, node)| {
                        (node.role() == egui::accesskit::Role::Button
                            && node.name() == Some(help_name.as_str()))
                        .then_some(node)
                    })
                    .unwrap_or_else(|| panic!("missing {name} help affordance"));
                assert_eq!(help.description(), Some(*description));
                assert_eq!(help.checked(), None);
                assert_eq!(help.is_expanded(), Some(false));
                assert!(help.supports_action(egui::accesskit::Action::Default));
                let help_bounds = help.bounds().expect("help bounds");
                assert!(
                    help_bounds.x1 - help_bounds.x0 >= 44.0
                        && help_bounds.y1 - help_bounds.y0 >= 44.0
                );
                assert!(
                    help_bounds.x1 <= bounds.x0,
                    "{name} help must stay in the label column before its switch"
                );
            }
        }
    }

    #[test]
    fn settings_help_opens_a_persistent_disclosure_and_escape_closes_it() {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let settings_view = RecordingSettingsView::default();
        let state = TranscriptionState::default();
        let (initial, action) = render_settings_with_input(
            &ctx,
            SettingsTab::Recording,
            &state,
            &settings_view,
            Vec::new(),
        );
        assert_eq!(action, ScreenAction::None);
        let bounds = initial
            .platform_output
            .accesskit_update
            .as_ref()
            .expect("Recording settings should expose AccessKit")
            .nodes
            .iter()
            .find_map(|(_, node)| {
                (node.role() == egui::accesskit::Role::Button
                    && node.name() == Some("Live transcription preview information"))
                .then(|| node.bounds())
                .flatten()
            })
            .expect("help button should have bounds");
        let point = egui::pos2(
            ((bounds.x0 + bounds.x1) / 2.0) as f32,
            ((bounds.y0 + bounds.y1) / 2.0) as f32,
        );
        let _ = render_settings_with_input(
            &ctx,
            SettingsTab::Recording,
            &state,
            &settings_view,
            vec![
                egui::Event::PointerMoved(point),
                egui::Event::PointerButton {
                    pos: point,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
        let (opened, action) = render_settings_with_input(
            &ctx,
            SettingsTab::Recording,
            &state,
            &settings_view,
            vec![
                egui::Event::PointerMoved(point),
                egui::Event::PointerButton {
                    pos: point,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
        assert_eq!(action, ScreenAction::None);
        let opened_nodes = &opened
            .platform_output
            .accesskit_update
            .expect("open help should expose AccessKit")
            .nodes;
        assert!(opened_nodes.iter().any(|(_, node)| {
            node.role() == egui::accesskit::Role::Button
                && node.name() == Some("Live transcription preview information")
                && node.is_expanded() == Some(true)
        }));
        assert!(
            opened_nodes
                .iter()
                .any(|(_, node)| { node.name() == Some(LIVE_TRANSCRIPTION_PREVIEW_DESCRIPTION) })
        );

        let _ = render_settings_with_input(
            &ctx,
            SettingsTab::Recording,
            &state,
            &settings_view,
            vec![egui::Event::Key {
                key: egui::Key::Escape,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
        );
        let (closed, action) = render_settings_with_input(
            &ctx,
            SettingsTab::Recording,
            &state,
            &settings_view,
            Vec::new(),
        );
        assert_eq!(action, ScreenAction::None);
        assert!(
            closed
                .platform_output
                .accesskit_update
                .expect("closed help should expose AccessKit")
                .nodes
                .iter()
                .any(|(_, node)| {
                    node.role() == egui::accesskit::Role::Button
                        && node.name() == Some("Live transcription preview information")
                        && node.is_expanded() == Some(false)
                })
        );
    }

    #[test]
    fn settings_help_keyboard_activation_pins_toggles_and_escape_stays_closed() {
        let help_id = egui::Id::new((
            "settings-help-affordance",
            LIVE_TRANSCRIPTION_PREVIEW_SWITCH_ID,
        ));
        for key in [egui::Key::Enter, egui::Key::Space] {
            let ctx = egui::Context::default();
            ctx.enable_accesskit();
            let settings_view = RecordingSettingsView::default();
            let state = TranscriptionState::default();
            let _ = ctx.run(
                egui::RawInput {
                    focused: true,
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        let _ = settings(ui, SettingsTab::Recording, &state, &settings_view);
                        ui.memory_mut(|memory| memory.request_focus(help_id));
                    });
                },
            );
            let (focused, _) = render_settings_with_input(
                &ctx,
                SettingsTab::Recording,
                &state,
                &settings_view,
                Vec::new(),
            );
            assert!(help_expanded(
                &focused,
                "Live transcription preview information"
            ));

            let _ = render_settings_with_input(
                &ctx,
                SettingsTab::Recording,
                &state,
                &settings_view,
                vec![egui::Event::Key {
                    key,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                }],
            );
            ctx.memory_mut(|memory| memory.surrender_focus(help_id));
            let (pinned, _) = render_settings_with_input(
                &ctx,
                SettingsTab::Recording,
                &state,
                &settings_view,
                Vec::new(),
            );
            assert!(help_expanded(
                &pinned,
                "Live transcription preview information"
            ));

            let _ = render_settings_with_input(
                &ctx,
                SettingsTab::Recording,
                &state,
                &settings_view,
                Vec::new(),
            );
            ctx.memory_mut(|memory| memory.request_focus(help_id));
            let _ = render_settings_with_input(
                &ctx,
                SettingsTab::Recording,
                &state,
                &settings_view,
                vec![egui::Event::Key {
                    key,
                    physical_key: None,
                    pressed: false,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                }],
            );
            let _ = render_settings_with_input(
                &ctx,
                SettingsTab::Recording,
                &state,
                &settings_view,
                vec![egui::Event::Key {
                    key,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                }],
            );
            let (closed, _) = render_settings_with_input(
                &ctx,
                SettingsTab::Recording,
                &state,
                &settings_view,
                Vec::new(),
            );
            assert!(!help_expanded(
                &closed,
                "Live transcription preview information"
            ));
        }

        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let settings_view = RecordingSettingsView::default();
        let state = TranscriptionState::default();
        let _ = ctx.run(
            egui::RawInput {
                focused: true,
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let _ = settings(ui, SettingsTab::Recording, &state, &settings_view);
                    ui.memory_mut(|memory| memory.request_focus(help_id));
                });
            },
        );
        let _ = render_settings_with_input(
            &ctx,
            SettingsTab::Recording,
            &state,
            &settings_view,
            vec![egui::Event::Key {
                key: egui::Key::Escape,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
        );
        let (closed, _) = render_settings_with_input(
            &ctx,
            SettingsTab::Recording,
            &state,
            &settings_view,
            Vec::new(),
        );
        assert!(!help_expanded(
            &closed,
            "Live transcription preview information"
        ));
    }

    #[test]
    fn settings_help_mouse_toggles_and_outside_click_dismisses_pin() {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let settings_view = RecordingSettingsView::default();
        let state = TranscriptionState::default();
        let (initial, _) = render_settings_with_input(
            &ctx,
            SettingsTab::General,
            &state,
            &settings_view,
            Vec::new(),
        );
        let bounds = help_bounds(&initial, "Close to tray information");
        let point = accesskit_rect_center(bounds);
        let opened = click_settings_help(&ctx, SettingsTab::General, &state, &settings_view, point);
        assert!(help_expanded(&opened, "Close to tray information"));

        let closed = click_settings_help(&ctx, SettingsTab::General, &state, &settings_view, point);
        assert!(!help_expanded(&closed, "Close to tray information"));

        let _ = render_settings_with_input(
            &ctx,
            SettingsTab::General,
            &state,
            &settings_view,
            vec![egui::Event::PointerMoved(egui::pos2(850.0, 2_900.0))],
        );
        let opened = click_settings_help(&ctx, SettingsTab::General, &state, &settings_view, point);
        assert!(help_expanded(&opened, "Close to tray information"));
        let outside = egui::pos2(850.0, 2_900.0);
        let _ = render_settings_with_input(
            &ctx,
            SettingsTab::General,
            &state,
            &settings_view,
            vec![
                egui::Event::PointerMoved(outside),
                egui::Event::PointerButton {
                    pos: outside,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
        let (outside_closed, _) = render_settings_with_input(
            &ctx,
            SettingsTab::General,
            &state,
            &settings_view,
            vec![
                egui::Event::PointerMoved(outside),
                egui::Event::PointerButton {
                    pos: outside,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
        assert!(!help_expanded(&outside_closed, "Close to tray information"));
    }

    #[test]
    fn settings_help_keeps_exactly_one_active_row() {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let settings_view = RecordingSettingsView {
            auto_insert_transcript: true,
            show_restore_clipboard: true,
            ..Default::default()
        };
        let state = TranscriptionState::default();
        let (initial, _) = render_settings_with_input(
            &ctx,
            SettingsTab::General,
            &state,
            &settings_view,
            Vec::new(),
        );
        let later = accesskit_rect_center(help_bounds(
            &initial,
            "Restore clipboard after insert information",
        ));
        let earlier = accesskit_rect_center(help_bounds(&initial, "Close to tray information"));
        let later_open =
            click_settings_help(&ctx, SettingsTab::General, &state, &settings_view, later);
        assert_eq!(expanded_settings_help_count(&later_open), 1);
        assert!(help_expanded(
            &later_open,
            "Restore clipboard after insert information"
        ));

        let earlier_id = egui::Id::new(("settings-help-affordance", CLOSE_TO_TRAY_SWITCH_ID));
        ctx.memory_mut(|memory| memory.request_focus(earlier_id));
        let _ = render_settings_with_input_at(
            &ctx,
            SettingsTab::General,
            &state,
            &settings_view,
            vec![egui::Event::PointerMoved(earlier)],
            Some(1.0),
            900.0,
        );
        let (still_later, _) = render_settings_with_input_at(
            &ctx,
            SettingsTab::General,
            &state,
            &settings_view,
            Vec::new(),
            Some(1.5),
            900.0,
        );
        assert_eq!(expanded_settings_help_count(&still_later), 1);
        assert!(help_expanded(
            &still_later,
            "Restore clipboard after insert information"
        ));

        let earlier_open =
            click_settings_help(&ctx, SettingsTab::General, &state, &settings_view, earlier);
        assert_eq!(expanded_settings_help_count(&earlier_open), 1);
        assert!(help_expanded(&earlier_open, "Close to tray information"));
        assert!(!help_expanded(
            &earlier_open,
            "Restore clipboard after insert information"
        ));
    }

    #[test]
    fn settings_help_hover_is_delayed_transfers_to_popup_and_escape_rearms_after_leave() {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let settings_view = RecordingSettingsView::default();
        let state = TranscriptionState::default();
        let (initial, _) = render_settings_with_input_at(
            &ctx,
            SettingsTab::Recording,
            &state,
            &settings_view,
            Vec::new(),
            Some(0.0),
            900.0,
        );
        let help_point = accesskit_rect_center(help_bounds(
            &initial,
            "Live transcription preview information",
        ));
        let (before_delay, _) = render_settings_with_input_at(
            &ctx,
            SettingsTab::Recording,
            &state,
            &settings_view,
            vec![egui::Event::PointerMoved(help_point)],
            Some(1.0),
            900.0,
        );
        assert!(!help_expanded(
            &before_delay,
            "Live transcription preview information"
        ));
        let (still_before_delay, _) = render_settings_with_input_at(
            &ctx,
            SettingsTab::Recording,
            &state,
            &settings_view,
            Vec::new(),
            Some(1.29),
            900.0,
        );
        assert!(!help_expanded(
            &still_before_delay,
            "Live transcription preview information"
        ));
        let (opened, _) = render_settings_with_input_at(
            &ctx,
            SettingsTab::Recording,
            &state,
            &settings_view,
            Vec::new(),
            Some(1.31),
            900.0,
        );
        assert!(help_expanded(
            &opened,
            "Live transcription preview information"
        ));
        let popup_point =
            accesskit_rect_center(help_bounds(&opened, LIVE_TRANSCRIPTION_PREVIEW_DESCRIPTION));
        let popup_rect = ctx.data(|data| {
            data.get_temp::<SettingsHelpState>(egui::Id::new("settings-help-state"))
                .and_then(|state| state.popup_rect)
                .expect("open help should retain popup geometry")
        });
        assert!(
            popup_rect.contains(popup_point),
            "popup {popup_rect:?} should contain description point {popup_point:?}"
        );
        let (over_popup, _) = render_settings_with_input_at(
            &ctx,
            SettingsTab::Recording,
            &state,
            &settings_view,
            vec![egui::Event::PointerMoved(popup_point)],
            Some(1.32),
            900.0,
        );
        assert!(help_expanded(
            &over_popup,
            "Live transcription preview information"
        ));

        let (escaped, _) = render_settings_with_input_at(
            &ctx,
            SettingsTab::Recording,
            &state,
            &settings_view,
            vec![egui::Event::Key {
                key: egui::Key::Escape,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
            Some(1.33),
            900.0,
        );
        assert!(!help_expanded(
            &escaped,
            "Live transcription preview information"
        ));
        let (still_dismissed, _) = render_settings_with_input_at(
            &ctx,
            SettingsTab::Recording,
            &state,
            &settings_view,
            Vec::new(),
            Some(1.7),
            900.0,
        );
        assert!(!help_expanded(
            &still_dismissed,
            "Live transcription preview information"
        ));
        let outside = egui::pos2(850.0, 2_900.0);
        let _ = render_settings_with_input_at(
            &ctx,
            SettingsTab::Recording,
            &state,
            &settings_view,
            vec![egui::Event::PointerMoved(outside)],
            Some(1.71),
            900.0,
        );
        let _ = render_settings_with_input_at(
            &ctx,
            SettingsTab::Recording,
            &state,
            &settings_view,
            vec![egui::Event::PointerMoved(help_point)],
            Some(1.72),
            900.0,
        );
        let (reopened, _) = render_settings_with_input_at(
            &ctx,
            SettingsTab::Recording,
            &state,
            &settings_view,
            Vec::new(),
            Some(2.03),
            900.0,
        );
        assert!(help_expanded(
            &reopened,
            "Live transcription preview information"
        ));
        let (left_both, _) = render_settings_with_input_at(
            &ctx,
            SettingsTab::Recording,
            &state,
            &settings_view,
            vec![egui::Event::PointerMoved(outside)],
            Some(2.04),
            900.0,
        );
        assert!(!help_expanded(
            &left_both,
            "Live transcription preview information"
        ));
    }

    #[test]
    fn settings_help_uses_label_geometry_and_stays_enabled_for_disabled_switches() {
        let settings_view = RecordingSettingsView::default();
        let state = TranscriptionState::default();
        for (width, compact) in [(900.0, false), (480.0, true)] {
            let ctx = egui::Context::default();
            ctx.enable_accesskit();
            let (output, _) = render_settings_with_input_at(
                &ctx,
                SettingsTab::General,
                &state,
                &settings_view,
                Vec::new(),
                None,
                width,
            );
            let label =
                named_role_bounds(&output, "Close to tray", egui::accesskit::Role::StaticText);
            let help = help_bounds(&output, "Close to tray information");
            let switch = named_role_bounds(&output, "Close to tray", egui::accesskit::Role::Switch);
            if compact {
                assert!(help.x0 >= label.x1);
                assert!(switch.y0 >= help.y1);
            } else {
                assert!(help.x0 >= label.x0);
                assert!(help.x1 <= label.x0 + f64::from(SETTINGS_LABEL_COLUMN_WIDTH));
                assert!(switch.x0 >= label.x0 + f64::from(SETTINGS_LABEL_COLUMN_WIDTH));
            }
        }

        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let locked_state = TranscriptionState {
            phase: TranscriptionPhase::Listening,
            ..Default::default()
        };
        let (locked, _) = render_settings_with_input(
            &ctx,
            SettingsTab::Recording,
            &locked_state,
            &settings_view,
            Vec::new(),
        );
        let nodes = &locked
            .platform_output
            .accesskit_update
            .expect("locked recording settings should expose AccessKit")
            .nodes;
        assert!(nodes.iter().any(|(_, node)| {
            node.role() == egui::accesskit::Role::Switch
                && node.name() == Some("Live transcription preview")
                && node.is_disabled()
        }));
        assert!(nodes.iter().any(|(_, node)| {
            node.role() == egui::accesskit::Role::Button
                && node.name() == Some("Live transcription preview information")
                && !node.is_disabled()
        }));
    }

    fn help_bounds(output: &egui::FullOutput, name: &str) -> egui::accesskit::Rect {
        output
            .platform_output
            .accesskit_update
            .as_ref()
            .and_then(|update| {
                update.nodes.iter().find_map(|(_, node)| {
                    (node.name() == Some(name)).then(|| node.bounds()).flatten()
                })
            })
            .unwrap_or_else(|| panic!("missing {name} help bounds"))
    }

    fn help_expanded(output: &egui::FullOutput, name: &str) -> bool {
        output
            .platform_output
            .accesskit_update
            .as_ref()
            .is_some_and(|update| {
                update
                    .nodes
                    .iter()
                    .any(|(_, node)| node.name() == Some(name) && node.is_expanded() == Some(true))
            })
    }

    fn expanded_settings_help_count(output: &egui::FullOutput) -> usize {
        output
            .platform_output
            .accesskit_update
            .as_ref()
            .map_or(0, |update| {
                update
                    .nodes
                    .iter()
                    .filter(|(_, node)| {
                        node.role() == egui::accesskit::Role::Button
                            && node
                                .name()
                                .is_some_and(|name| name.ends_with(" information"))
                            && node.is_expanded() == Some(true)
                    })
                    .count()
            })
    }

    fn named_role_bounds(
        output: &egui::FullOutput,
        name: &str,
        role: egui::accesskit::Role,
    ) -> egui::accesskit::Rect {
        output
            .platform_output
            .accesskit_update
            .as_ref()
            .and_then(|update| {
                update.nodes.iter().find_map(|(_, node)| {
                    (node.name() == Some(name) && node.role() == role)
                        .then(|| node.bounds())
                        .flatten()
                })
            })
            .unwrap_or_else(|| panic!("missing {name} {role:?} bounds"))
    }

    fn accesskit_rect_center(rect: egui::accesskit::Rect) -> egui::Pos2 {
        egui::pos2(
            ((rect.x0 + rect.x1) / 2.0) as f32,
            ((rect.y0 + rect.y1) / 2.0) as f32,
        )
    }

    fn click_settings_help(
        ctx: &egui::Context,
        tab: SettingsTab,
        state: &TranscriptionState,
        settings_view: &RecordingSettingsView,
        point: egui::Pos2,
    ) -> egui::FullOutput {
        let (_, press_action) = render_settings_with_input(
            ctx,
            tab,
            state,
            settings_view,
            vec![
                egui::Event::PointerMoved(point),
                egui::Event::PointerButton {
                    pos: point,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
        assert_eq!(press_action, ScreenAction::None);
        let (output, release_action) = render_settings_with_input(
            ctx,
            tab,
            state,
            settings_view,
            vec![
                egui::Event::PointerMoved(point),
                egui::Event::PointerButton {
                    pos: point,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
        assert_eq!(release_action, ScreenAction::None);
        output
    }

    #[test]
    fn live_transcription_preview_is_a_named_switch_with_pointer_and_keyboard_activation() {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let settings_view = RecordingSettingsView {
            provisional_feedback: true,
            ..Default::default()
        };

        let (initial, action) =
            render_recording_settings_with_input(&ctx, &settings_view, Vec::new());
        assert_eq!(action, ScreenAction::None);
        let nodes = &initial
            .platform_output
            .accesskit_update
            .expect("Recording settings should expose AccessKit")
            .nodes;
        let switch = nodes
            .iter()
            .find_map(|(_, node)| {
                (node.role() == egui::accesskit::Role::Switch
                    && node.name() == Some("Live transcription preview"))
                .then_some(node)
            })
            .expect("live transcription preview should be exposed as a switch");
        assert_eq!(switch.checked(), Some(egui::accesskit::Checked::True));
        assert_eq!(
            switch.description(),
            Some(LIVE_TRANSCRIPTION_PREVIEW_DESCRIPTION)
        );
        let bounds = switch.bounds().expect("switch should expose its bounds");
        assert!(bounds.x1 - bounds.x0 >= 44.0 && bounds.y1 - bounds.y0 >= 44.0);
        assert!(nodes.iter().any(|(_, node)| {
            node.role() == egui::accesskit::Role::Button
                && node.name() == Some("Live transcription preview information")
                && node.description() == Some(LIVE_TRANSCRIPTION_PREVIEW_DESCRIPTION)
        }));

        let point = egui::pos2(
            ((bounds.x0 + bounds.x1) / 2.0) as f32,
            ((bounds.y0 + bounds.y1) / 2.0) as f32,
        );
        let (_, press_action) = render_recording_settings_with_input(
            &ctx,
            &settings_view,
            vec![
                egui::Event::PointerMoved(point),
                egui::Event::PointerButton {
                    pos: point,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
        assert_eq!(press_action, ScreenAction::None);
        let (_, release_action) = render_recording_settings_with_input(
            &ctx,
            &settings_view,
            vec![
                egui::Event::PointerMoved(point),
                egui::Event::PointerButton {
                    pos: point,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
        assert_eq!(release_action, ScreenAction::ToggleProvisionalFeedback);

        let _ = ctx.run(
            egui::RawInput {
                focused: true,
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let _ = settings(
                        ui,
                        SettingsTab::Recording,
                        &TranscriptionState::default(),
                        &settings_view,
                    );
                    ui.memory_mut(|memory| {
                        memory.request_focus(egui::Id::new(LIVE_TRANSCRIPTION_PREVIEW_SWITCH_ID))
                    });
                });
            },
        );
        let _ = render_recording_settings_with_input(&ctx, &settings_view, Vec::new());
        let (_, keyboard_action) = render_recording_settings_with_input(
            &ctx,
            &settings_view,
            vec![egui::Event::Key {
                key: egui::Key::Space,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
        );
        assert_eq!(keyboard_action, ScreenAction::ToggleProvisionalFeedback);
    }

    #[test]
    fn recording_mode_arrow_navigation_keeps_radio_behavior() {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let settings_view = RecordingSettingsView::default();
        let _ = ctx.run(
            egui::RawInput {
                focused: true,
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let _ = settings(
                        ui,
                        SettingsTab::Recording,
                        &TranscriptionState::default(),
                        &settings_view,
                    );
                    ui.memory_mut(|memory| {
                        memory.request_focus(recording_mode_id(RecordingMode::PressOnce))
                    });
                });
            },
        );
        let _ = render_recording_settings_with_input(&ctx, &settings_view, Vec::new());
        let (_, action) = render_recording_settings_with_input(
            &ctx,
            &settings_view,
            vec![egui::Event::Key {
                key: egui::Key::ArrowRight,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
        );
        assert_eq!(action, ScreenAction::SetRecordingMode(RecordingMode::Hold));
    }

    #[test]
    fn settings_inputs_are_labelled_by_their_visible_rows() {
        for tab in [
            SettingsTab::General,
            SettingsTab::Recording,
            SettingsTab::Advanced,
            SettingsTab::About,
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
    fn recording_settings_keeps_live_level_paint_inside_one_sensitivity_slider() {
        use egui::accesskit::Role;

        let settings_view = RecordingSettingsView {
            input_sensitivity_percent: 42,
            input_level_percent: 72,
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
                node.role() == Role::Slider && node.name() == Some("Input level sensitivity")
            })
            .collect::<Vec<_>>();
        assert_eq!(sliders.len(), 1);
        let slider = &sliders[0].1;
        assert_eq!(slider.min_numeric_value(), Some(0.0));
        assert_eq!(slider.max_numeric_value(), Some(100.0));
        assert_eq!(slider.numeric_value(), Some(42.0));
        assert!(
            slider
                .description()
                .is_some_and(|description| description.contains("colored fill"))
        );
        assert!(
            slider
                .description()
                .is_some_and(|description| description.contains("Input detected"))
        );
        assert!(
            nodes
                .iter()
                .any(|(_, node)| node.name() == Some("Recording input"))
        );
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
        for tab in [SettingsTab::Recording, SettingsTab::Advanced] {
            let ctx = egui::Context::default();
            ctx.enable_accesskit();
            let output = ctx.run(Default::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let _ = settings(ui, tab, &state, &settings_view);
                });
            });
            let nodes = &output.platform_output.accesskit_update.unwrap().nodes;
            if tab == SettingsTab::Recording {
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
        for expected in [SettingsTab::Recording, SettingsTab::Advanced] {
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
                                    SettingsTab::Output => "General",
                                    SettingsTab::Advanced => "Advanced",
                                    SettingsTab::About => "About",
                                })
                    })
            );
        }
    }

    #[test]
    fn settings_tab_arrow_uses_the_focused_tab_instead_of_the_active_tab() {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let state = TranscriptionState::default();
        let settings_view = RecordingSettingsView::default();
        let active = SettingsTab::General;
        let _ = ctx.run(
            egui::RawInput {
                focused: true,
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let _ = settings(ui, active, &state, &settings_view);
                    ui.memory_mut(|memory| memory.request_focus(tab_id(ui, SettingsTab::Advanced)));
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

        let mut action = ScreenAction::None;
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
                    action = settings(ui, active, &state, &settings_view);
                });
            },
        );

        assert_eq!(action, ScreenAction::SetSettingsTab(SettingsTab::About));
    }

    #[test]
    fn disabled_diagnostics_export_describes_the_missing_private_path() {
        let output = render_route(UiRoute::Settings(SettingsTab::Advanced));
        let export = output
            .platform_output
            .accesskit_update
            .expect("Advanced settings should expose AccessKit")
            .nodes
            .into_iter()
            .find_map(|(_, node)| {
                (node.name() == Some("Export redacted diagnostics")).then_some(node)
            })
            .expect("Advanced settings should expose diagnostics export");
        assert!(export.is_disabled());
        assert_eq!(
            export.description(),
            Some(
                "Unavailable because the platform settings directory cannot provide a private export location."
            )
        );
    }

    #[test]
    fn dense_settings_rows_stack_compactly_and_only_paint_requested_separators() {
        for (width, compact) in [(900.0, false), (480.0, true)] {
            let ctx = egui::Context::default();
            let mut row_rect = egui::Rect::NOTHING;
            let mut control_rect = egui::Rect::NOTHING;
            let output = ctx.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        Vec2::new(width, 300.0),
                    )),
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        row_rect = SettingsRow::show(ui, "Dense row", true, |ui, label_id| {
                            control_rect = ui
                                .add_sized([120.0, 44.0], egui::Button::new("Control"))
                                .labelled_by(label_id)
                                .rect;
                        })
                        .rect;
                        let _ = SettingsRow::show(ui, "Second row", false, |ui, label_id| {
                            ui.add_sized([120.0, 44.0], egui::Button::new("Second control"))
                                .labelled_by(label_id);
                        });
                    });
                },
            );
            assert!(row_rect.height() >= 44.0);
            assert!(
                output
                    .shapes
                    .iter()
                    .filter(|shape| matches!(shape.shape, egui::epaint::Shape::LineSegment { .. }))
                    .count()
                    == 1,
                "two rendered rows must paint exactly one separator"
            );
            assert!(control_rect.width() >= 44.0 && control_rect.height() >= 44.0);
            if compact {
                assert!(control_rect.center().y > row_rect.center().y);
            } else {
                assert!((control_rect.center().y - row_rect.center().y).abs() < 8.0);
            }
        }
    }

    #[test]
    fn desktop_settings_rows_use_fixed_left_aligned_columns() {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let output = ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    Vec2::new(900.0, 300.0),
                )),
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let _ = SettingsRow::show(ui, "Desktop column label", false, |ui, label_id| {
                        ui.add_sized([120.0, 44.0], egui::Button::new("Desktop column control"))
                            .labelled_by(label_id);
                    });
                });
            },
        );
        let nodes = &output
            .platform_output
            .accesskit_update
            .expect("settings row should expose AccessKit")
            .nodes;
        let label = nodes
            .iter()
            .find_map(|(_, node)| {
                (node.name() == Some("Desktop column label"))
                    .then(|| node.bounds())
                    .flatten()
            })
            .expect("desktop label should expose bounds");
        let control = nodes
            .iter()
            .find_map(|(_, node)| {
                (node.name() == Some("Desktop column control"))
                    .then(|| node.bounds())
                    .flatten()
            })
            .expect("desktop control should expose bounds");
        let label_text_x = output
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::epaint::Shape::Text(text) if text.galley.text() == "Desktop column label" => {
                    Some(text.pos.x)
                }
                _ => None,
            })
            .expect("desktop label should be painted");
        assert!((label_text_x - label.x0 as f32).abs() < 1.0);
        assert!(
            control.x0 - label.x0 >= f64::from(SETTINGS_LABEL_COLUMN_WIDTH),
            "control starts at {}, label starts at {}",
            control.x0,
            label.x0
        );
        assert!(
            control.x0 - label.x0 < f64::from(SETTINGS_LABEL_COLUMN_WIDTH + 24.0),
            "control starts at {}, label starts at {}",
            control.x0,
            label.x0
        );
    }

    #[test]
    fn actual_settings_actions_have_full_targets_and_compact_rows_stack() {
        let settings_view = RecordingSettingsView {
            auto_insert_transcript: true,
            show_restore_clipboard: true,
            debug_mode: true,
            history_mode_label: "Transcript and audio".into(),
            transcript_retention_days: Some(30),
            audio_retention_days: Some(14),
            ..Default::default()
        };
        let output_switch_name = transcript_delivery_copy(settings_view.show_restore_clipboard).0;
        for (width, compact) in [(900.0, false), (480.0, true)] {
            for (tab, controls) in [
                (
                    SettingsTab::General,
                    &[
                        "Close to tray",
                        output_switch_name,
                        "Restore clipboard after insert",
                        "Manage models",
                    ][..],
                ),
                (
                    SettingsTab::Recording,
                    &[
                        "Press once",
                        "Hold",
                        "Live transcription preview",
                        "Refresh devices",
                        "Change shortcut",
                    ][..],
                ),
                (
                    SettingsTab::Advanced,
                    &[
                        "Stop after speech ends",
                        "Limit transcript age",
                        "Limit audio age",
                        "Store application identity",
                        "Enable model Playground",
                        "Open model Playground",
                        "Export redacted diagnostics",
                    ][..],
                ),
                (SettingsTab::About, &[][..]),
            ] {
                let ctx = egui::Context::default();
                ctx.enable_accesskit();
                let output = ctx.run(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            Vec2::new(width, 3_000.0),
                        )),
                        ..Default::default()
                    },
                    |ctx| {
                        egui::CentralPanel::default().show(ctx, |ui| {
                            let _ =
                                settings(ui, tab, &TranscriptionState::default(), &settings_view);
                        });
                    },
                );
                let nodes = &output.platform_output.accesskit_update.unwrap().nodes;
                let switch_names = [
                    "Close to tray",
                    output_switch_name,
                    "Restore clipboard after insert",
                    "Live transcription preview",
                    "Stop after speech ends",
                    "Limit transcript age",
                    "Limit audio age",
                    "Store application identity",
                    "Enable model Playground",
                ];
                for name in controls {
                    let bounds = nodes
                        .iter()
                        .find_map(|(_, node)| {
                            (node.name() == Some(*name)
                                && (!switch_names.contains(name)
                                    || node.role() == egui::accesskit::Role::Switch))
                                .then(|| node.bounds())
                                .flatten()
                        })
                        .unwrap_or_else(|| panic!("missing {name} at {width}px"));
                    assert!(
                        bounds.x1 - bounds.x0 >= 44.0 && bounds.y1 - bounds.y0 >= 44.0,
                        "{name} target is too small at {width}px: {bounds:?}"
                    );
                }
                let labelled_controls: &[(&str, egui::accesskit::Role)] = match tab {
                    SettingsTab::General => &[
                        ("Theme", egui::accesskit::Role::ComboBox),
                        ("Dictation overlay", egui::accesskit::Role::ComboBox),
                        ("Overlay position", egui::accesskit::Role::ComboBox),
                        ("Paste delay ms", egui::accesskit::Role::SpinButton),
                    ],
                    SettingsTab::Recording => &[
                        ("Duration limit", egui::accesskit::Role::ComboBox),
                        ("Device", egui::accesskit::Role::ComboBox),
                        ("Input level", egui::accesskit::Role::Slider),
                        ("Streaming mode", egui::accesskit::Role::ComboBox),
                        ("Transcription device", egui::accesskit::Role::ComboBox),
                    ],
                    SettingsTab::Advanced => &[
                        ("Speech confirmation ms", egui::accesskit::Role::SpinButton),
                        ("History storage", egui::accesskit::Role::ComboBox),
                        (
                            "Maximum unpinned entries",
                            egui::accesskit::Role::SpinButton,
                        ),
                    ],
                    SettingsTab::About | SettingsTab::Output => &[],
                };
                for (label, role) in labelled_controls {
                    let bounds = nodes
                        .iter()
                        .find_map(|(label_id, node)| {
                            (node.name() == Some(*label)).then(|| {
                                nodes.iter().find_map(|(_, control)| {
                                    (control.role() == *role
                                        && control.labelled_by().contains(label_id))
                                    .then(|| control.bounds())
                                    .flatten()
                                })
                            })
                        })
                        .flatten()
                        .unwrap_or_else(|| {
                            panic!("missing {role:?} labelled by {label} at {width}px")
                        });
                    assert!(
                        bounds.x1 - bounds.x0 >= 44.0 && bounds.y1 - bounds.y0 >= 44.0,
                        "{role:?} labelled by {label} is too small at {width}px: {bounds:?}"
                    );
                }
                if tab == SettingsTab::General {
                    let label = nodes
                        .iter()
                        .find_map(|(_, node)| {
                            (node.name() == Some("Close to tray")
                                && node.role() != egui::accesskit::Role::Switch)
                                .then(|| node.bounds())
                                .flatten()
                        })
                        .expect("close-to-tray row label has bounds");
                    let control = nodes
                        .iter()
                        .find_map(|(_, node)| {
                            (node.name() == Some("Close to tray")
                                && node.role() == egui::accesskit::Role::Switch)
                                .then(|| node.bounds())
                                .flatten()
                        })
                        .expect("close control has bounds");
                    if compact {
                        assert!(control.y0 >= label.y1);
                    } else {
                        assert!((control.y0 + control.y1 - label.y0 - label.y1).abs() < 16.0);
                    }
                }
            }
        }
    }

    #[test]
    fn migrated_settings_switches_dispatch_both_state_transitions() {
        for initially_enabled in [false, true] {
            let settings_view = RecordingSettingsView {
                close_to_tray: initially_enabled,
                ..Default::default()
            };
            let (_, action) =
                click_settings_switch(SettingsTab::General, &settings_view, "Close to tray");
            assert_eq!(action, ScreenAction::SetCloseToTray(!initially_enabled));

            let settings_view = RecordingSettingsView {
                auto_insert_transcript: true,
                show_restore_clipboard: true,
                restore_clipboard_after_insert: initially_enabled,
                ..Default::default()
            };
            let (_, action) = click_settings_switch(
                SettingsTab::General,
                &settings_view,
                "Restore clipboard after insert",
            );
            assert_eq!(
                action,
                ScreenAction::SetRestoreClipboardAfterInsert(!initially_enabled)
            );

            let settings_view = RecordingSettingsView {
                history_mode_label: "Transcript only".into(),
                transcript_retention_days: initially_enabled.then_some(30),
                ..Default::default()
            };
            let (output, action) = click_settings_switch(
                SettingsTab::Advanced,
                &settings_view,
                "Limit transcript age",
            );
            assert_eq!(
                action,
                ScreenAction::SetTranscriptRetentionDays((!initially_enabled).then_some(30))
            );
            let nodes = &output
                .platform_output
                .accesskit_update
                .expect("updated retention settings should expose AccessKit")
                .nodes;
            assert!(nodes.iter().any(|(_, node)| {
                node.role() == egui::accesskit::Role::Switch
                    && node.name() == Some("Limit transcript age")
                    && node.checked()
                        == Some(if initially_enabled {
                            egui::accesskit::Checked::False
                        } else {
                            egui::accesskit::Checked::True
                        })
            }));
            assert_eq!(
                nodes.iter().any(|(_, node)| node.name() == Some("Days")),
                !initially_enabled
            );

            let settings_view = RecordingSettingsView {
                history_mode_label: "Transcript and audio".into(),
                transcript_retention_days: None,
                audio_retention_days: initially_enabled.then_some(14),
                ..Default::default()
            };
            let (output, action) =
                click_settings_switch(SettingsTab::Advanced, &settings_view, "Limit audio age");
            assert_eq!(
                action,
                ScreenAction::SetAudioRetentionDays((!initially_enabled).then_some(30))
            );
            let nodes = output
                .platform_output
                .accesskit_update
                .expect("updated audio retention settings should expose AccessKit")
                .nodes;
            let days_count = nodes
                .iter()
                .filter(|(_, node)| node.name() == Some("Days"))
                .count();
            assert_eq!(days_count, usize::from(!initially_enabled));
            assert!(nodes.iter().any(|(_, node)| {
                node.role() == egui::accesskit::Role::Switch
                    && node.name() == Some("Limit audio age")
                    && node.checked()
                        == Some(if initially_enabled {
                            egui::accesskit::Checked::False
                        } else {
                            egui::accesskit::Checked::True
                        })
            }));

            let settings_view = RecordingSettingsView {
                history_mode_label: "Transcript only".into(),
                store_application_identity: initially_enabled,
                ..Default::default()
            };
            let (_, action) = click_settings_switch(
                SettingsTab::Advanced,
                &settings_view,
                "Store application identity",
            );
            assert_eq!(
                action,
                ScreenAction::SetStoreApplicationIdentity(!initially_enabled)
            );

            let settings_view = RecordingSettingsView {
                debug_mode: initially_enabled,
                ..Default::default()
            };
            let (output, action) = click_settings_switch(
                SettingsTab::Advanced,
                &settings_view,
                "Enable model Playground",
            );
            assert_eq!(action, ScreenAction::SetDebugMode(!initially_enabled));
            assert_eq!(
                output
                    .platform_output
                    .accesskit_update
                    .expect("updated developer settings should expose AccessKit")
                    .nodes
                    .iter()
                    .any(|(_, node)| node.name() == Some("Open model Playground")),
                !initially_enabled
            );
        }
    }

    #[test]
    fn output_separator_follows_auto_insert_post_click_state() {
        for (initially_enabled, expected_action, expected_separators) in [
            (false, ScreenAction::SetAutoInsertTranscript(true), 1),
            (true, ScreenAction::SetAutoInsertTranscript(false), 0),
        ] {
            let ctx = egui::Context::default();
            ctx.enable_accesskit();
            let settings = RecordingSettingsView {
                auto_insert_transcript: initially_enabled,
                show_restore_clipboard: false,
                output_notice: Some("Clipboard fallback remains available.".into()),
                ..Default::default()
            };
            let (initial, action) = render_output_settings_with_input(&ctx, &settings, Vec::new());
            assert_eq!(action, ScreenAction::None);
            let bounds = initial
                .platform_output
                .accesskit_update
                .as_ref()
                .expect("output settings should expose AccessKit")
                .nodes
                .iter()
                .find_map(|(_, node)| {
                    (node.name()
                        == Some(transcript_delivery_copy(settings.show_restore_clipboard).0)
                        && node.role() == egui::accesskit::Role::Switch)
                        .then(|| node.bounds())
                        .flatten()
                })
                .expect("auto-insert switch should have bounds");
            let point = egui::pos2(
                ((bounds.x0 + bounds.x1) / 2.0) as f32,
                ((bounds.y0 + bounds.y1) / 2.0) as f32,
            );
            let (_, press_action) = render_output_settings_with_input(
                &ctx,
                &settings,
                vec![
                    egui::Event::PointerMoved(point),
                    egui::Event::PointerButton {
                        pos: point,
                        button: egui::PointerButton::Primary,
                        pressed: true,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
            );
            assert_eq!(press_action, ScreenAction::None);
            let (released, action) = render_output_settings_with_input(
                &ctx,
                &settings,
                vec![
                    egui::Event::PointerMoved(point),
                    egui::Event::PointerButton {
                        pos: point,
                        button: egui::PointerButton::Primary,
                        pressed: false,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
            );
            assert_eq!(action, expected_action);
            let separator_count = released
                .shapes
                .iter()
                .filter(|shape| matches!(shape.shape, egui::epaint::Shape::LineSegment { .. }))
                .count();
            assert_eq!(separator_count, expected_separators);
        }
    }

    #[test]
    fn voice_detection_rows_and_separators_follow_post_click_state() {
        for (initially_enabled, expected_action, expected_separators) in [
            (false, ScreenAction::SetVadEnabled(true), 5),
            (true, ScreenAction::SetVadEnabled(false), 0),
        ] {
            let ctx = egui::Context::default();
            ctx.enable_accesskit();
            let settings = RecordingSettingsView {
                vad_enabled: initially_enabled,
                ..Default::default()
            };
            let (initial, action) = render_voice_detection_with_input(&ctx, &settings, Vec::new());
            assert_eq!(action, ScreenAction::None);
            let bounds = initial
                .platform_output
                .accesskit_update
                .as_ref()
                .expect("voice detection settings should expose AccessKit")
                .nodes
                .iter()
                .find_map(|(_, node)| {
                    (node.name() == Some("Stop after speech ends")
                        && node.role() == egui::accesskit::Role::Switch)
                        .then(|| node.bounds())
                        .flatten()
                })
                .expect("voice detection switch should have bounds");
            let point = egui::pos2(
                ((bounds.x0 + bounds.x1) / 2.0) as f32,
                ((bounds.y0 + bounds.y1) / 2.0) as f32,
            );
            let (_, press_action) = render_voice_detection_with_input(
                &ctx,
                &settings,
                vec![
                    egui::Event::PointerMoved(point),
                    egui::Event::PointerButton {
                        pos: point,
                        button: egui::PointerButton::Primary,
                        pressed: true,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
            );
            assert_eq!(press_action, ScreenAction::None);
            let (released, action) = render_voice_detection_with_input(
                &ctx,
                &settings,
                vec![
                    egui::Event::PointerMoved(point),
                    egui::Event::PointerButton {
                        pos: point,
                        button: egui::PointerButton::Primary,
                        pressed: false,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
            );
            assert_eq!(action, expected_action);
            let nodes = &released
                .platform_output
                .accesskit_update
                .expect("updated voice detection settings should expose AccessKit")
                .nodes;
            let timing_rows_rendered = nodes
                .iter()
                .any(|(_, node)| node.name() == Some("Speech confirmation ms"));
            assert_eq!(timing_rows_rendered, !initially_enabled);
            let separator_count = released
                .shapes
                .iter()
                .filter(|shape| matches!(shape.shape, egui::epaint::Shape::LineSegment { .. }))
                .count();
            assert_eq!(separator_count, expected_separators);
        }
    }
}
