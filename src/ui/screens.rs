//! Shared, backend-neutral egui screen renderers.

use std::{collections::HashSet, path::Path};

use eframe::egui::{
    self, Align, Align2, Color32, ComboBox, Frame, Layout, Margin, RichText, Rounding, ScrollArea,
    Sense, Stroke, Vec2,
};

#[cfg(test)]
use crate::model_catalog::BUNDLED_BASE_MODEL_ID;

use super::{
    about_page,
    controls::{
        ButtonTone, Icon, SearchFieldResponse, button, card, focus_tooltip, icon_glyph, keycap,
        paint_focus_ring, search_field,
    },
    model_picker::{
        ReadyModelPickerAction, close_ready_model_picker_and_restore_focus, show_ready_model_picker,
    },
    state::{
        ComparisonPhase, ComparisonResultPhase, ModelCardKey, ModelComparisonState, ModelDialog,
        ModelDownloadState, ModelLanguageFilter, ModelManagementState, ModelSizeTier,
        ModelSpeedTier, ModelViewModel, RecordingMode, RemoteCatalogActionKind,
        RemoteCatalogActionView, RemoteCatalogEntryView, RemoteCatalogStatusKind,
        RemoteCatalogVariantView, RemoteCatalogView, ResolvedTheme, SettingsSaveState, SettingsTab,
        TranscriptionPhase, TranscriptionState, UiRoute,
    },
    ui_palette,
};

const TRANSCRIPT_PANEL_PREFERRED_MIN_HEIGHT: f32 = 565.0;
const TRANSCRIPT_PANEL_MIN_HEIGHT: f32 = 272.0;
const MODEL_REQUIRED_CONTENT_HEIGHT: f32 = 176.0;
const SELECTOR_CONTROL_HEIGHT: f32 = 44.0;
const SELECTOR_MODEL_MIN_WIDTH: f32 = 224.0;
const SELECTOR_MODEL_MAX_WIDTH: f32 = 360.0;
const SELECTOR_HOTKEY_MIN_WIDTH: f32 = 224.0;
const SELECTOR_HOTKEY_MAX_WIDTH: f32 = 300.0;
const SELECTOR_CARD_ROUNDING: f32 = 6.0;
const HOTKEY_CAPTURE_PROMPT: &str = "Press shortcut · Esc cancels";
const TRANSCRIPT_FOOTER_INSET: f32 = 16.0;
const TRANSCRIPT_BODY_PADDING: f32 = 26.0;
const TRANSCRIPT_BODY_VERTICAL_PADDING: f32 = 24.0;
const TRANSCRIPT_STATUS_VERTICAL_PADDING: f32 = 13.0;
const TRANSCRIPT_STATUS_CONTENT_HEIGHT: f32 = 54.0;
const TRANSCRIPT_STATUS_SPINNER_SLOT: f32 = 44.0;
const TRANSCRIPT_STATUS_SPINNER_SIZE: f32 = 26.0;
const TRANSCRIPT_HELPER_VIEWPORT_GUARD: f32 = 14.0;
const MICROPHONE_ACCESS_ERROR: &str = "Scribe couldn’t access your microphone.";

const ROUTE_TOP_INSET: f32 = 28.0;
const ROUTE_HORIZONTAL_INSET: f32 = 28.0;
const ROUTE_BOTTOM_INSET: f32 = 16.0;
// egui 0.27 does not salt automatic widget IDs when a scope is pushed. Keep
// each top-level route in its own range so a route transition cannot reparent
// an automatic ID while AccessKit removes the previous route's subtree.
const ROUTE_AUTO_ID_STRIDE: usize = 100_000;
const SETTINGS_COMPACT_BREAKPOINT: f32 = 620.0;
const SETTINGS_LABEL_COLUMN_WIDTH: f32 = 270.0;
#[derive(Clone, Copy)]
struct SettingsHelp {
    id_source: &'static str,
    description: &'static str,
}

impl SettingsHelp {
    const fn new(id_source: &'static str, description: &'static str) -> Self {
        Self {
            id_source,
            description,
        }
    }
}

fn settings_help_metadata(label: &str) -> Option<SettingsHelp> {
    match label {
        "Transcription device" => Some(TRANSCRIPTION_DEVICE_HELP),
        "Streaming mode" => Some(STREAMING_MODE_HELP),
        "Speech confirmation ms" => Some(SPEECH_CONFIRMATION_HELP),
        "Internal pause ms" => Some(INTERNAL_PAUSE_HELP),
        "End after silence ms" => Some(END_AFTER_SILENCE_HELP),
        "Pre-roll ms" => Some(PRE_ROLL_HELP),
        "Post-roll ms" => Some(POST_ROLL_HELP),
        "Active model" => Some(ACTIVE_MODEL_HELP),
        "Dictation overlay" => Some(DICTATION_OVERLAY_HELP),
        "Paste delay ms" => Some(PASTE_DELAY_HELP),
        "History storage" => Some(HISTORY_STORAGE_HELP),
        "Maximum unpinned entries" => Some(MAX_HISTORY_ENTRIES_HELP),
        "Transcript days" => Some(TRANSCRIPT_RETENTION_DAYS_HELP),
        "Audio days" => Some(AUDIO_RETENTION_DAYS_HELP),
        _ => None,
    }
}

const TRANSCRIPTION_DEVICE_HELP: SettingsHelp = SettingsHelp::new(
    "transcription-device-help",
    "Auto selects available local hardware. GPU may be faster when supported; CPU only avoids GPU acceleration.",
);
const STREAMING_MODE_HELP: SettingsHelp = SettingsHelp::new(
    "streaming-mode-help",
    "For transcription sessions, Auto and Rolling preview show temporary local text while recording; Final text only waits for the final transcription.",
);
const ACTIVE_MODEL_HELP: SettingsHelp = SettingsHelp::new(
    "active-model-help",
    "The selected local model determines transcription accuracy, speed, and disk use. Manage models to change it.",
);
const DICTATION_OVERLAY_HELP: SettingsHelp = SettingsHelp::new(
    "dictation-overlay-help",
    "Show recording feedback above other apps. This is unavailable where Scribe cannot verify that the overlay will not steal focus.",
);
const PASTE_DELAY_HELP: SettingsHelp = SettingsHelp::new(
    "paste-delay-help",
    "Wait this long after copying before Scribe sends the paste shortcut to the captured app.",
);
const HISTORY_STORAGE_HELP: SettingsHelp = SettingsHelp::new(
    "history-storage-help",
    "Choose whether Scribe keeps no history, transcript text only, or transcript text with retained audio on this device.",
);
const MAX_HISTORY_ENTRIES_HELP: SettingsHelp = SettingsHelp::new(
    "maximum-unpinned-entries-help",
    "When the limit is reached, Scribe removes the oldest unpinned entries. Pinned entries are kept.",
);
const TRANSCRIPT_RETENTION_DAYS_HELP: SettingsHelp = SettingsHelp::new(
    "transcript-retention-days-help",
    "Remove the entire unpinned history entry, including any retained audio, after this many days. Pinned entries are kept.",
);
const AUDIO_RETENTION_DAYS_HELP: SettingsHelp = SettingsHelp::new(
    "audio-retention-days-help",
    "Remove only retained audio from unpinned entries after this many days; the transcript entry remains. Pinned entries are kept.",
);
const SPEECH_CONFIRMATION_HELP: SettingsHelp = SettingsHelp::new(
    "speech-confirmation-ms-help",
    "Require speech to continue for this long before Scribe treats it as confirmed speech.",
);
const INTERNAL_PAUSE_HELP: SettingsHelp = SettingsHelp::new(
    "internal-pause-ms-help",
    "Treat a pause shorter than this as part of the same phrase rather than a new speech segment.",
);
const END_AFTER_SILENCE_HELP: SettingsHelp = SettingsHelp::new(
    "end-after-silence-ms-help",
    "In Press once mode, end recording after speech has stopped for this long.",
);
const PRE_ROLL_HELP: SettingsHelp = SettingsHelp::new(
    "pre-roll-ms-help",
    "Keep this much audio from just before speech begins so the first word is less likely to be cut off.",
);
const POST_ROLL_HELP: SettingsHelp = SettingsHelp::new(
    "post-roll-ms-help",
    "Keep this much audio after speech ends so the last word is less likely to be cut off.",
);
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
const SPEECH_DETECTION_SENSITIVITY_HELP_ID: &str = "speech-detection-sensitivity-help";
const INPUT_LEVEL_SENSITIVITY_CONTROL_ID: &str = "input-level-sensitivity-control";
const SPEECH_DETECTION_SENSITIVITY_DESCRIPTION: &str = "More sensitive settings lower the speech-probability threshold, so quieter or less certain speech is more likely to be detected.";
const INPUT_LEVEL_SENSITIVITY_DESCRIPTION: &str = "The teal fill shows microphone volume. The marker adjusts how readily Scribe treats audio as speech; it is not a literal volume cutoff.";
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
const MODELS_ROUTE_HEADING_FOCUS_REQUEST: &str = "models-route-heading-focus-request";
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

pub(crate) fn request_models_route_heading_focus(ctx: &egui::Context) {
    ctx.data_mut(|data| data.insert_temp(egui::Id::new(MODELS_ROUTE_HEADING_FOCUS_REQUEST), true));
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

fn models_content_width(ui: &egui::Ui) -> f32 {
    // egui 0.27 leaves one trailing item-spacing slot in the Models content
    // UI's reported available width. Flexible rows that consume the full
    // value otherwise paint exactly one gap beyond the route's right edge.
    let reported = current_content_width(ui);
    (reported - ui.spacing().item_spacing.x)
        .max(44.0)
        .min(reported)
}

fn selector_text_width(ui: &egui::Ui, text: &str, font: egui::FontId) -> f32 {
    ui.painter()
        .layout_no_wrap(text.to_owned(), font, ui_palette(ui).text)
        .size()
        .x
}

fn selector_card_width(ui: &egui::Ui, value: &str, min_width: f32, max_width: f32) -> f32 {
    // Both quick controls are action cards, not expanding form fields. Reserve
    // enough room for a useful value but cap their footprint on wide screens.
    (76.0 + selector_text_width(ui, value, egui::FontId::proportional(14.0)))
        .clamp(min_width, max_width)
}

fn ellipsized_selector_value(
    ui: &egui::Ui,
    text: &str,
    font: egui::FontId,
    color: Color32,
    max_width: f32,
) -> (String, std::sync::Arc<egui::Galley>) {
    let max_width = max_width.max(0.0);
    let layout = |value: &str| {
        ui.painter()
            .layout_no_wrap(value.to_owned(), font.clone(), color)
    };
    let full = layout(text);
    if full.size().x <= max_width {
        return (text.to_owned(), full);
    }

    const ELLIPSIS: &str = "…";
    if layout(ELLIPSIS).size().x > max_width {
        return (String::new(), layout(""));
    }
    let boundaries = text
        .char_indices()
        .map(|(index, _)| index)
        .chain(std::iter::once(text.len()))
        .collect::<Vec<_>>();
    let mut low = 0;
    let mut high = boundaries.len();
    while low + 1 < high {
        let middle = (low + high) / 2;
        let candidate = format!("{}{}", &text[..boundaries[middle]], ELLIPSIS);
        if layout(&candidate).size().x <= max_width {
            low = middle;
        } else {
            high = middle;
        }
    }
    let displayed = format!("{}{}", &text[..boundaries[low]], ELLIPSIS);
    let galley = layout(&displayed);
    (displayed, galley)
}

fn paint_selector_card(
    ui: &egui::Ui,
    rect: egui::Rect,
    response: &egui::Response,
    icon: Icon,
    heading: &str,
    value: &str,
) {
    let colors = ui_palette(ui);
    let fill = if !response.enabled() {
        colors.disabled_bg
    } else if response.hovered() || response.has_focus() {
        colors.panel_bg
    } else {
        colors.card_bg
    };
    let stroke = Stroke::new(
        1.0,
        if response.enabled() && (response.hovered() || response.has_focus()) {
            colors.border_strong
        } else {
            colors.border
        },
    );
    ui.painter()
        .rect(rect, Rounding::same(SELECTOR_CARD_ROUNDING), fill, stroke);

    let icon_x = rect.min.x + 18.0;
    ui.painter().text(
        egui::pos2(icon_x, rect.center().y),
        Align2::CENTER_CENTER,
        icon_glyph(icon),
        egui::FontId::proportional(19.0),
        if response.enabled() {
            colors.muted_text
        } else {
            colors.text
        },
    );
    let text_rect = egui::Rect::from_min_max(
        egui::pos2(rect.min.x + 36.0, rect.min.y + 5.0),
        egui::pos2(rect.max.x - 10.0, rect.max.y - 4.0),
    );
    let painter = ui.painter().with_clip_rect(text_rect);
    painter.text(
        egui::pos2(text_rect.min.x, rect.min.y + 12.0),
        Align2::LEFT_CENTER,
        heading,
        egui::FontId::proportional(11.0),
        colors.muted_text,
    );
    let value_color = if response.enabled() {
        colors.text
    } else {
        colors.muted_text
    };
    let (_, value_galley) = ellipsized_selector_value(
        ui,
        value,
        egui::FontId::proportional(14.0),
        value_color,
        text_rect.width(),
    );
    let value_position = egui::pos2(
        text_rect.min.x,
        rect.min.y + 28.0 - value_galley.size().y * 0.5,
    );
    painter.galley(value_position, value_galley, value_color);
}

fn transcript_panel_height(ui: &egui::Ui) -> f32 {
    let helper_height = ui.text_style_height(&egui::TextStyle::Body);
    let remaining_height = ui.clip_rect().max.y
        - ui.available_rect_before_wrap().min.y
        - ui.spacing().item_spacing.y
        - helper_height
        - TRANSCRIPT_HELPER_VIEWPORT_GUARD;
    let remaining_height = remaining_height.max(0.0);
    remaining_height.clamp(
        TRANSCRIPT_PANEL_MIN_HEIGHT.min(remaining_height),
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
    pub speech_detection_sensitivity_percent: u8,
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
            speech_detection_sensitivity_percent: 50,
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
    SelectQuickModel(String),
    StartHotkeyCapture,
    CancelHotkeyCapture,
    StartRecording,
    StopRecording,
    AbandonRecording,
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
    ToggleResolvedTheme(ResolvedTheme),
    SetOverlayMode(String),
    SetRecordingMode(RecordingMode),
    SetDurationSeconds(u32),
    ToggleProvisionalFeedback,
    SetAudioDevice(Option<String>),
    SetSpeechDetectionSensitivity(u8),
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
        UiRoute::Transcribe => transcribe(ui, view.transcription, view.models, view.model_catalog),
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
                    ui.skip_ahead_auto_ids(route_auto_id_offset(route));
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

fn route_auto_id_offset(route: UiRoute) -> usize {
    match route {
        UiRoute::Transcribe => 0,
        UiRoute::Models => ROUTE_AUTO_ID_STRIDE,
        // Settings reserves its own 10,000-ID ranges for each tab inside this
        // route-level range.
        UiRoute::Settings(_) => ROUTE_AUTO_ID_STRIDE * 2,
        UiRoute::History => ROUTE_AUTO_ID_STRIDE * 3,
        UiRoute::About => ROUTE_AUTO_ID_STRIDE * 4,
        UiRoute::Debug => ROUTE_AUTO_ID_STRIDE * 5,
    }
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
    quick_models: &[ModelViewModel],
) -> ScreenAction {
    let name = selected_model_name(state, models);
    let no_model = state.phase == TranscriptionPhase::NoModel;
    let model_disabled_reason = state
        .model_change_disabled_reason
        .as_deref()
        .or_else(|| model_selector_disabled_reason(state.phase));
    let hotkey_disabled_reason = state.hotkey_change_disabled_reason.as_deref();
    let mut action = ScreenAction::None;
    let available_width = current_content_width(ui);
    let gap = ui.spacing().item_spacing.x;
    let model_width =
        selector_card_width(ui, name, SELECTOR_MODEL_MIN_WIDTH, SELECTOR_MODEL_MAX_WIDTH);
    let hotkey_value = if state.hotkey_capture_active {
        HOTKEY_CAPTURE_PROMPT
    } else {
        &state.hotkey
    };
    let hotkey_width = selector_card_width(
        ui,
        hotkey_value,
        SELECTOR_HOTKEY_MIN_WIDTH,
        SELECTOR_HOTKEY_MAX_WIDTH,
    );
    let inline = available_width >= model_width + hotkey_width + gap;
    let model_width = if inline {
        model_width
    } else {
        available_width.min(SELECTOR_MODEL_MAX_WIDTH)
    };
    let hotkey_width = if inline {
        hotkey_width
    } else {
        available_width.min(SELECTOR_HOTKEY_MAX_WIDTH)
    };
    ui.allocate_ui_with_layout(
        Vec2::new(available_width, 0.0),
        if inline {
            Layout::left_to_right(Align::TOP)
        } else {
            Layout::top_down(Align::LEFT)
        },
        |ui| {
            let was_enabled = ui.is_enabled();
            ui.set_enabled(was_enabled && model_disabled_reason.is_none());
            let (model_card_rect, _) = ui.allocate_exact_size(
                Vec2::new(model_width, SELECTOR_CONTROL_HEIGHT),
                egui::Sense::hover(),
            );
            let response = ui.interact(
                model_card_rect,
                egui::Id::new("selected-model-action"),
                egui::Sense::click(),
            );
            ui.set_enabled(was_enabled);
            let picker_id = egui::Id::new("quick-model-picker");
            if !response.enabled() {
                close_ready_model_picker_and_restore_focus(ui, picker_id, response.id);
            }
            paint_selector_card(
                ui,
                model_card_rect,
                &response,
                Icon::Cpu,
                "Active model",
                name,
            );
            let action_name = if no_model {
                "Add a model".to_owned()
            } else {
                format!("Choose active model: {name}")
            };
            response
                .widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, &action_name));
            paint_focus_ring(ui, &response, Rounding::same(SELECTOR_CARD_ROUNDING));
            if let Some(reason) = model_disabled_reason {
                ui.ctx().accesskit_node_builder(response.id, |builder| {
                    builder.set_description(reason);
                });
                focus_tooltip(ui, &response, reason);
                response.clone().on_hover_text(reason);
            }
            let model_keyboard_activated = response.has_focus()
                && ui.input(|input| {
                    input.key_pressed(egui::Key::Enter) || input.key_pressed(egui::Key::Space)
                });
            if response.enabled() && (response.clicked() || model_keyboard_activated) {
                action = if no_model {
                    ScreenAction::AddModel
                } else {
                    ScreenAction::None
                };
                if !no_model {
                    ui.memory_mut(|memory| memory.toggle_popup(picker_id));
                }
            }
            let picker_open = ui.memory(|memory| memory.is_popup_open(picker_id));
            ui.ctx().accesskit_node_builder(response.id, |builder| {
                builder.set_role(egui::accesskit::Role::Button);
                builder.set_name(action_name.clone());
                builder.set_description(
                    model_disabled_reason.unwrap_or("Opens the installed ready-model picker."),
                );
                builder.set_expanded(picker_open);
                builder.set_bounds(egui::accesskit::Rect {
                    x0: model_card_rect.min.x.into(),
                    y0: model_card_rect.min.y.into(),
                    x1: model_card_rect.max.x.into(),
                    y1: model_card_rect.max.y.into(),
                });
                if !response.enabled() {
                    builder.set_disabled();
                }
            });
            if response.enabled() && response.hovered() {
                response
                    .clone()
                    .on_hover_cursor(egui::CursorIcon::PointingHand);
            }
            if response.enabled()
                && !no_model
                && let Some(picker_action) = show_ready_model_picker(
                    ui,
                    picker_id,
                    &response,
                    state.selected_model_id.as_deref(),
                    quick_models,
                )
            {
                action = match picker_action {
                    ReadyModelPickerAction::Select(id) => ScreenAction::SelectQuickModel(id),
                    ReadyModelPickerAction::ManageModels => ScreenAction::OpenModelSettings,
                };
            }
            let picker_open = ui.memory(|memory| memory.is_popup_open(picker_id));
            ui.ctx()
                .accesskit_node_builder(response.id, |builder| builder.set_expanded(picker_open));
            if !inline {
                ui.add_space(ui.spacing().item_spacing.y);
            }
            let was_enabled = ui.is_enabled();
            ui.set_enabled(was_enabled && hotkey_disabled_reason.is_none());
            let (hotkey_card_rect, _) = ui.allocate_exact_size(
                Vec2::new(hotkey_width, SELECTOR_CONTROL_HEIGHT),
                egui::Sense::hover(),
            );
            let hotkey_response = ui.interact(
                hotkey_card_rect,
                egui::Id::new("recording-hotkey-action"),
                egui::Sense::click(),
            );
            ui.set_enabled(was_enabled);
            paint_selector_card(
                ui,
                hotkey_card_rect,
                &hotkey_response,
                Icon::Keyboard,
                "Recording shortcut",
                hotkey_value,
            );
            let hotkey_action_name = if state.hotkey_capture_active {
                "Cancel recording shortcut capture"
            } else {
                "Change recording shortcut"
            };
            hotkey_response.widget_info(|| {
                egui::WidgetInfo::labeled(egui::WidgetType::Button, hotkey_action_name)
            });
            ui.ctx()
                .accesskit_node_builder(hotkey_response.id, |builder| {
                    builder.set_role(egui::accesskit::Role::Button);
                    builder.set_name(hotkey_action_name);
                    builder.set_bounds(egui::accesskit::Rect {
                        x0: hotkey_card_rect.min.x.into(),
                        y0: hotkey_card_rect.min.y.into(),
                        x1: hotkey_card_rect.max.x.into(),
                        y1: hotkey_card_rect.max.y.into(),
                    });
                    if !hotkey_response.enabled() {
                        builder.set_disabled();
                    }
                });
            paint_focus_ring(ui, &hotkey_response, Rounding::same(SELECTOR_CARD_ROUNDING));
            if let Some(reason) = hotkey_disabled_reason {
                ui.ctx()
                    .accesskit_node_builder(hotkey_response.id, |builder| {
                        builder.set_description(reason);
                    });
                focus_tooltip(ui, &hotkey_response, reason);
                hotkey_response.clone().on_hover_text(reason);
            } else {
                focus_tooltip(ui, &hotkey_response, hotkey_action_name);
            }
            if hotkey_response.enabled() && hotkey_response.hovered() {
                hotkey_response
                    .clone()
                    .on_hover_cursor(egui::CursorIcon::PointingHand);
            }
            let hotkey_keyboard_activated = hotkey_response.has_focus()
                && ui.input(|input| {
                    input.key_pressed(egui::Key::Enter) || input.key_pressed(egui::Key::Space)
                });
            if hotkey_response.enabled() && (hotkey_response.clicked() || hotkey_keyboard_activated)
            {
                action = if state.hotkey_capture_active {
                    ScreenAction::CancelHotkeyCapture
                } else {
                    ScreenAction::StartHotkeyCapture
                };
            }
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
            let abandon = recording_square_button(
                ui,
                Icon::Close,
                "Cancel recording and discard it",
                colors.error,
                colors.danger_button_text,
                Stroke::NONE,
            );
            if abandon.clicked() {
                action = ScreenAction::AbandonRecording;
            }
            ui.vertical(|ui| {
                ui.spacing_mut().interact_size.y = 0.0;
                ui.horizontal(|ui| {
                    let (dot_rect, _) =
                        ui.allocate_exact_size(Vec2::splat(8.0), egui::Sense::hover());
                    ui.painter()
                        .circle_filled(dot_rect.center(), 4.0, colors.error);
                    let status = ui.label(RichText::new("Recording").strong().color(colors.error));
                    ui.ctx().accesskit_node_builder(status.id, |builder| {
                        if !state.suppress_live_announcements {
                            builder.set_live(egui::accesskit::Live::Polite);
                            builder.set_live_atomic();
                        }
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
                    if !state.suppress_live_announcements {
                        builder.set_live(egui::accesskit::Live::Polite);
                        builder.set_live_atomic();
                    }
                });
                ui.label("This may take a moment.");
            });
        }
        TranscriptionPhase::RequestingMicrophone => {
            let abandon = recording_square_button(
                ui,
                Icon::Close,
                "Cancel recording and discard it",
                colors.error,
                colors.danger_button_text,
                Stroke::NONE,
            );
            if abandon.clicked() {
                action = ScreenAction::AbandonRecording;
            }
            status_spinner(ui, "Requesting microphone access progress");
            ui.vertical(|ui| {
                ui.spacing_mut().interact_size.y = 0.0;
                let status = ui.label(RichText::new("Requesting microphone access…").strong());
                ui.ctx().accesskit_node_builder(status.id, |builder| {
                    if !state.suppress_live_announcements {
                        builder.set_live(egui::accesskit::Live::Polite);
                        builder.set_live_atomic();
                    }
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
                    if !state.suppress_live_announcements {
                        builder.set_live(egui::accesskit::Live::Polite);
                        builder.set_live_atomic();
                    }
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
                    RecordingMode::PressOnce => {
                        format!("Press {} to use the record shortcut", state.hotkey)
                    }
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
                                    if !state.suppress_live_announcements {
                                        builder.set_live(egui::accesskit::Live::Polite);
                                        builder.set_live_atomic();
                                    }
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
                                if !state.suppress_live_announcements {
                                    builder.set_live(egui::accesskit::Live::Assertive);
                                    builder.set_live_atomic();
                                }
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
                                    body_format,
                                );
                                transcript.append(
                                    "  Live estimate: ",
                                    0.0,
                                    egui::TextFormat {
                                        font_id: egui::TextStyle::Body.resolve(ui.style()),
                                        color: colors.tertiary_text,
                                        italics: true,
                                        ..Default::default()
                                    },
                                );
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
                                builder.set_name(state.committed_transcript.as_str());
                                if !state.provisional_transcript.is_empty() {
                                    builder.set_description(
                                        "Italic text is a live estimate and may change until recording ends.",
                                    );
                                }
                                if !state.suppress_live_announcements {
                                    builder.set_live(egui::accesskit::Live::Polite);
                                    builder.set_live_atomic();
                                }
                            });
                            if !state.provisional_transcript.is_empty() {
                                let estimate =
                                    ui.allocate_response(Vec2::ZERO, egui::Sense::hover());
                                ui.ctx().accesskit_node_builder(estimate.id, |builder| {
                                    builder.set_role(egui::accesskit::Role::StaticText);
                                    builder.set_name(format!(
                                        "Live estimate, may change: {}",
                                        state.provisional_transcript
                                    ));
                                });
                            }
                        }
                        if state.committed_transcript.trim().is_empty()
                            && !state.provisional_transcript.is_empty()
                        {
                            ui.add_space(8.0);
                            let response = ui.label(
                                RichText::new(format!(
                                    "Live estimate: {}",
                                    state.provisional_transcript
                                ))
                                .italics()
                                .color(colors.tertiary_text),
                            );
                            ui.ctx().accesskit_node_builder(response.id, |builder| {
                                builder.set_name(format!(
                                    "Live estimate, may change: {}",
                                    state.provisional_transcript
                                ));
                            });
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
    quick_models: &[ModelViewModel],
) -> ScreenAction {
    header(ui, "Transcribe", "Audio stays on this device.");
    let action = selector_row(
        ui,
        state,
        models,
        if quick_models.is_empty() {
            models
        } else {
            quick_models
        },
    );
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
const MODEL_CARD_SUMMARY_HEIGHT: f32 = 100.0;
const MODEL_LANGUAGE_OPTICAL_OFFSET_Y: f32 = -6.0;

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

fn remote_catalog_variant_matches(
    entry: &RemoteCatalogEntryView,
    variant: &RemoteCatalogVariantView,
    query: &str,
    language_filter: ModelLanguageFilter,
) -> bool {
    language_filter.matches(&entry.languages)
        && (query.is_empty()
            || [
                entry.display_name.as_str(),
                entry.description.as_str(),
                entry.language_summary.as_str(),
                entry.repository.as_str(),
                variant.id.as_str(),
                variant.filename.as_str(),
                variant.size_label.as_str(),
                variant.accuracy_guidance.as_str(),
            ]
            .into_iter()
            .any(|field| field.to_ascii_lowercase().contains(query))
            || variant
                .status_label
                .as_deref()
                .is_some_and(|status| status.to_ascii_lowercase().contains(query)))
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
        entry
            .variants
            .iter()
            .filter(|variant| {
                remote_catalog_variant_matches(entry, variant, &query, language_filter)
            })
            .filter_map(|variant| {
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
    if !description_overflows(ui, description, content_width) {
        // A fitting expanded description is the same summary line as its
        // collapsed preview, without any truncation to apply.
        return render_model_description_preview(ui, description, width, left_inset);
    }
    let mut job = egui::text::LayoutJob::default();
    job.append(
        description,
        0.0,
        egui::TextFormat {
            font_id: egui::TextStyle::Small.resolve(ui.style()),
            color: colors.muted_text,
            ..Default::default()
        },
    );
    job.wrap.max_width = content_width;
    let content_height = ui.fonts(|fonts| fonts.layout_job(job).size().y.max(18.0)) + 2.0;
    ui.horizontal_top(|ui| {
        ui.add_space(left_inset);
        ui.allocate_ui_with_layout(
            Vec2::new(content_width, content_height),
            Layout::top_down(Align::LEFT),
            |ui| {
                ui.set_width(content_width);
                // Match the preview's vertically centered first-line
                // baseline, then let wrapped content extend below it.
                ui.add_space(2.0);
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
            #[cfg(test)]
            register_model_layout_rect(ui, &accessible_name, "rating track", track);
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
                #[cfg(test)]
                register_model_layout_rect(ui, &accessible_name, "rating fill", fill);
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
        ModelCard::Local(model) => model
            .description
            .clone()
            .unwrap_or_else(|| "Local speech-to-text model.".to_owned()),
        ModelCard::Remote(entry, _) => entry.description.clone(),
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

fn remote_variant_accessible_name(
    entry: &RemoteCatalogEntryView,
    variant: &RemoteCatalogVariantView,
) -> String {
    format!("{} ({})", entry.display_name, variant.filename)
}

struct ModelLifecyclePresentation<'a> {
    action: ScreenAction,
    icon: Icon,
    label: String,
    accessible_name: String,
    enabled: bool,
    disabled_reason: Option<&'a str>,
    visible_status: Option<String>,
    compact_size: Option<String>,
    tone: ModelLifecycleTone,
}

struct ModelLifecycleControls<'a> {
    /// Settled bundled models have no lifecycle action to expose. Their
    /// non-removability is conveyed by the model card's accessible description
    /// instead of a redundant disabled "Installed" button.
    primary: Option<ModelLifecyclePresentation<'a>>,
    discard: Option<ScreenAction>,
    discard_name: Option<String>,
    error_message: Option<&'a str>,
    error_accessible_name: Option<String>,
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
                icon: Icon::Pause,
                label: "Pause".into(),
                accessible_name: format!("Pause {} download", model.display_name),
                enabled: model.cancel_supported,
                disabled_reason: model.primary_action_disabled_reason.as_deref(),
                visible_status: None,
                compact_size: None,
                tone: ModelLifecycleTone::Standard,
            }
        }
        ModelCard::Local(model)
            if matches!(
                model.download_state,
                ModelDownloadState::Queued | ModelDownloadState::WaitingForVerification
            ) =>
        {
            let waiting = model.download_state == ModelDownloadState::WaitingForVerification;
            ModelLifecyclePresentation {
                action: ScreenAction::CancelModelInstall(model.id.clone()),
                icon: Icon::Close,
                label: "Cancel".into(),
                accessible_name: format!(
                    "Cancel {} {}",
                    model.display_name,
                    if waiting {
                        "waiting verification"
                    } else {
                        "queued download"
                    }
                ),
                enabled: model.cancel_supported,
                disabled_reason: (!model.cancel_supported)
                    .then_some(model.primary_action_disabled_reason.as_deref())
                    .flatten(),
                visible_status: Some(
                    if waiting {
                        "Waiting for verification"
                    } else {
                        "Queued"
                    }
                    .to_owned(),
                ),
                compact_size: None,
                tone: ModelLifecycleTone::Standard,
            }
        }
        ModelCard::Local(model)
            if matches!(
                model.download_state,
                ModelDownloadState::Verifying | ModelDownloadState::Extracting
            ) =>
        {
            ModelLifecyclePresentation {
                action: ScreenAction::None,
                icon: Icon::Spinner,
                label: "Installing…".into(),
                accessible_name: format!("Installing {}", model.display_name),
                enabled: false,
                disabled_reason: Some("Scribe is preparing the model and cannot cancel this step."),
                visible_status: None,
                compact_size: None,
                tone: ModelLifecycleTone::Standard,
            }
        }
        ModelCard::Local(model) if model.included => ModelLifecyclePresentation {
            action: ScreenAction::None,
            icon: Icon::CheckCircle,
            label: "Installed".into(),
            accessible_name: format!("{} is installed with Scribe", model.display_name),
            enabled: false,
            disabled_reason: Some(
                "This verified model is installed with Scribe and cannot be removed.",
            ),
            visible_status: None,
            compact_size: None,
            tone: ModelLifecycleTone::Standard,
        },
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
                    visible_status: None,
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
                    visible_status: None,
                    compact_size: None,
                    tone: ModelLifecycleTone::DestructiveOutline,
                }
            }
        }
        ModelCard::Local(model) => {
            let (action, label) = if model.bundled {
                (ScreenAction::InstallModel(model.id.clone()), "Repair")
            } else if model.primary_action_installs_upgrade {
                (ScreenAction::UpgradeModel(model.id.clone()), "Upgrade")
            } else {
                (
                    ScreenAction::InstallModel(model.id.clone()),
                    match model.download_state {
                        ModelDownloadState::Failed | ModelDownloadState::Cancelled
                            if model.partial_cleanup_available =>
                        {
                            "Resume"
                        }
                        _ => "Install",
                    },
                )
            };
            ModelLifecyclePresentation {
                action,
                icon: if matches!(
                    model.download_state,
                    ModelDownloadState::Cancelled | ModelDownloadState::Failed
                ) && model.partial_cleanup_available
                {
                    Icon::Play
                } else {
                    Icon::Download
                },
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
                visible_status: None,
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
            let variant_name = remote_variant_accessible_name(entry, variant);
            let remote = variant
                .actions
                .iter()
                .find(|action| matches!(action.kind, RemoteCatalogActionKind::Cancel { .. }))
                .or_else(|| remote_primary_action(variant));
            let label = remote.map_or("Install", |action| action.label.as_str());
            let label = if label == "Remove" { "Delete" } else { label };
            let cancel_waiting = matches!(
                variant.status_label.as_deref(),
                Some("Queued for download") | Some("Waiting for verification")
            );
            ModelLifecyclePresentation {
                action: remote.map_or(ScreenAction::None, |action| {
                    screen_action_for_remote_catalog_action(&action.kind)
                }),
                icon: if matches!(
                    remote.map(|action| &action.kind),
                    Some(RemoteCatalogActionKind::Cancel { .. })
                ) {
                    if cancel_waiting {
                        Icon::Close
                    } else {
                        Icon::Pause
                    }
                } else if label == "Delete" || label == "Remove" {
                    Icon::Trash
                } else {
                    Icon::Download
                },
                label: if matches!(
                    remote.map(|action| &action.kind),
                    Some(RemoteCatalogActionKind::Cancel { .. })
                ) {
                    if cancel_waiting { "Cancel" } else { "Pause" }.into()
                } else {
                    label.into()
                },
                accessible_name: format!(
                    "{} {}",
                    if matches!(
                        remote.map(|action| &action.kind),
                        Some(RemoteCatalogActionKind::Cancel { .. })
                    ) {
                        if cancel_waiting { "Cancel" } else { "Pause" }
                    } else {
                        label
                    },
                    variant_name
                ),
                enabled: remote.is_some_and(|action| action.enabled),
                disabled_reason: remote.and_then(|action| action.disabled_reason.as_deref()),
                visible_status: if cancel_waiting {
                    variant.status_label.clone()
                } else {
                    None
                },
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

fn model_lifecycle_controls<'a>(
    card: ModelCard<'a>,
    can_replace_active: bool,
) -> ModelLifecycleControls<'a> {
    let mut primary = Some(model_lifecycle_presentation(card, can_replace_active));
    let discard = match card {
        ModelCard::Local(model)
            if model.download_state == ModelDownloadState::Downloading
                || (matches!(
                    model.download_state,
                    ModelDownloadState::Cancelled | ModelDownloadState::Failed
                ) && model.partial_cleanup_available) =>
        {
            Some(ScreenAction::DiscardModelPartial(model.id.clone()))
        }
        ModelCard::Remote(entry, variant)
            if matches!(
                variant.status_label.as_deref(),
                Some("Downloading") | Some("Cancelled") | Some("Failed")
            ) =>
        {
            variant
                .actions
                .iter()
                .find_map(|action| match &action.kind {
                    RemoteCatalogActionKind::DiscardPartial {
                        remote_model_id,
                        variant_id,
                    } => Some(ScreenAction::DiscardRemoteCatalogPartial {
                        remote_model_id: remote_model_id.clone(),
                        variant_id: variant_id.clone(),
                    }),
                    _ => None,
                })
                .or_else(|| {
                    (variant.status_label.as_deref() == Some("Downloading")).then(|| {
                        ScreenAction::DiscardRemoteCatalogPartial {
                            remote_model_id: entry.id.clone(),
                            variant_id: variant.id.clone(),
                        }
                    })
                })
        }
        _ => None,
    };
    let discard_name = discard.as_ref().map(|_| match card {
        ModelCard::Local(model) => format!("Discard partial for {}", model.display_name),
        ModelCard::Remote(entry, variant) => format!(
            "Discard partial for {}",
            remote_variant_accessible_name(entry, variant)
        ),
    });
    if discard.is_some()
        && primary
            .as_ref()
            .is_some_and(|primary| matches!(primary.label.as_str(), "Install" | "Retry" | "Resume"))
        && !primary.as_ref().is_some_and(|primary| {
            matches!(
                primary.action,
                ScreenAction::CancelModelInstall(_) | ScreenAction::CancelRemoteCatalogInstall(_)
            )
        })
    {
        let primary = primary.as_mut().expect("primary lifecycle presentation");
        primary.icon = Icon::Play;
        primary.label = "Resume".into();
        let model_name = match card {
            ModelCard::Local(model) => model.display_name.clone(),
            ModelCard::Remote(entry, variant) => remote_variant_accessible_name(entry, variant),
        };
        primary.accessible_name = format!("Resume {model_name} download");
    }
    if matches!(
        card,
        ModelCard::Local(model) if model.included && model.installed && model.ready
    ) {
        primary = None;
    }
    ModelLifecycleControls {
        primary,
        discard,
        discard_name,
        error_message: model_download_error(card),
        error_accessible_name: model_download_error(card).map(|_| match card {
            ModelCard::Local(model) => format!("Show download error for {}", model.display_name),
            ModelCard::Remote(entry, variant) => {
                format!(
                    "Show download error for {}",
                    remote_variant_accessible_name(entry, variant)
                )
            }
        }),
    }
}

fn model_download_error(card: ModelCard<'_>) -> Option<&str> {
    match card {
        ModelCard::Local(model)
            if model.download_state == ModelDownloadState::Failed
                && !model.partial_cleanup_available =>
        {
            model.error_message.as_deref()
        }
        ModelCard::Remote(_, variant) if variant.downloaded_bytes.is_none() => {
            variant.error_message.as_deref()
        }
        _ => None,
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
        (((available_width + ICON_GAP) / (ICON_WIDTH + ICON_GAP)).floor() as usize).clamp(1, 2);
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

fn model_language_filter_control(
    ui: &mut egui::Ui,
    selected: &mut ModelLanguageFilter,
    compact: bool,
) -> egui::Response {
    if compact {
        return compact_model_language_filter_control(ui, selected);
    }
    let combo = ComboBox::from_id_source("models-language")
        .selected_text(format!("{}  {}", icon_glyph(Icon::Globe), selected.label()))
        .width(156.0)
        .show_ui(ui, |ui| {
            for value in ModelLanguageFilter::ALL {
                ui.selectable_value(selected, value, value.label());
            }
        });
    ui.ctx()
        .accesskit_node_builder(combo.response.id, |builder| {
            builder.set_name("Filter model languages")
        });
    combo.response
}

fn compact_model_language_filter_control(
    ui: &mut egui::Ui,
    selected: &mut ModelLanguageFilter,
) -> egui::Response {
    #[derive(Clone)]
    struct DeferredFocusRestore {
        trigger: egui::Id,
        popup_focus_ids: Vec<egui::Id>,
    }

    let colors = ui_palette(ui);
    let popup_id = ui.make_persistent_id("models-language-popup");
    let restore_id = popup_id.with("focus-restore");
    if let Some(restore) = ui.data(|data| data.get_temp::<DeferredFocusRestore>(restore_id)) {
        ui.data_mut(|data| data.remove::<DeferredFocusRestore>(restore_id));
        let focused = ui.memory(|memory| memory.focused());
        if focused.is_none() || focused.is_some_and(|id| restore.popup_focus_ids.contains(&id)) {
            ui.memory_mut(|memory| memory.request_focus(restore.trigger));
        }
    }
    let (target, response) = ui.allocate_exact_size(Vec2::splat(44.0), Sense::click());
    let mut expanded = ui.memory(|memory| memory.is_popup_open(popup_id));
    let keyboard_activate = response.has_focus()
        && ui.input(|input| {
            input.key_pressed(egui::Key::Enter) || input.key_pressed(egui::Key::Space)
        });
    if response.clicked() || keyboard_activate {
        ui.memory_mut(|memory| memory.toggle_popup(popup_id));
        expanded = ui.memory(|memory| memory.is_popup_open(popup_id));
    }
    let escape_pressed = expanded && ui.input(|input| input.key_pressed(egui::Key::Escape));
    let mut selected_from_popup = false;
    let mut popup_focus_ids = Vec::new();
    if expanded && !escape_pressed {
        egui::popup::popup_below_widget(ui, popup_id, &response, |ui| {
            for value in ModelLanguageFilter::ALL {
                let option = ui.selectable_value(selected, value, value.label());
                popup_focus_ids.push(option.id);
                let keyboard_select = option.has_focus()
                    && ui.input(|input| {
                        input.key_pressed(egui::Key::Enter) || input.key_pressed(egui::Key::Space)
                    });
                if option.clicked() || keyboard_select {
                    *selected = value;
                    selected_from_popup = true;
                }
            }
        });
    }
    let clicked_elsewhere = expanded && response.clicked_elsewhere();
    if selected_from_popup || escape_pressed {
        ui.memory_mut(|memory| memory.close_popup());
        response.request_focus();
        expanded = false;
    } else if clicked_elsewhere {
        let focused = ui.memory(|memory| memory.focused());
        ui.memory_mut(|memory| memory.close_popup());
        let needs_focus_restore = match focused {
            None => true,
            Some(id) => id == response.id || popup_focus_ids.contains(&id),
        };
        if needs_focus_restore {
            ui.data_mut(|data| {
                data.insert_temp(
                    restore_id,
                    DeferredFocusRestore {
                        trigger: response.id,
                        popup_focus_ids,
                    },
                );
            });
        }
        expanded = false;
    }
    if response.hovered() || response.has_focus() || expanded {
        ui.painter()
            .rect_filled(target, Rounding::same(5.0), colors.active_card_bg);
    }
    ui.painter().text(
        target.center(),
        Align2::CENTER_CENTER,
        icon_glyph(Icon::Globe),
        egui::FontId::proportional(18.0),
        colors.muted_text,
    );
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::ComboBox, "Filter model languages")
    });
    ui.ctx().accesskit_node_builder(response.id, |builder| {
        builder.set_role(egui::accesskit::Role::ComboBox);
        builder.set_name("Filter model languages");
        builder.set_description(format!("Current language filter: {}", selected.label()));
        builder.set_expanded(expanded);
        builder.set_bounds(accesskit_rect(target));
    });
    paint_focus_ring(ui, &response, Rounding::same(5.0));
    focus_tooltip(ui, &response, "Filter model languages");
    response.on_hover_text("Filter model languages")
}

struct ModelCatalogActions {
    action: ScreenAction,
    #[cfg(test)]
    refresh: egui::Response,
    #[cfg(test)]
    import: egui::Response,
}

fn model_catalog_actions(
    ui: &mut egui::Ui,
    management: &ModelManagementState,
    restore_remove_target_gone: bool,
    remote_catalog: &RemoteCatalogView,
) -> ModelCatalogActions {
    let refresh = compact_model_icon_action(
        ui,
        Icon::Refresh,
        "Refresh trusted model catalog",
        remote_catalog.refresh_enabled,
        (!remote_catalog.refresh_enabled).then_some("The catalog is already refreshing."),
        None,
    );
    let import = compact_model_icon_action(ui, Icon::Plus, "Import local GGUF", true, None, None);
    if management.restore_add_focus
        || management.restore_after_removal_focus
        || restore_remove_target_gone
    {
        import.request_focus();
    }
    let action = if refresh.clicked() && remote_catalog.refresh_enabled {
        ScreenAction::RetryRemoteCatalog
    } else if import.clicked() {
        ScreenAction::AddModel
    } else {
        ScreenAction::None
    };
    ModelCatalogActions {
        action,
        #[cfg(test)]
        refresh,
        #[cfg(test)]
        import,
    }
}

struct ModelToolbarResponse {
    search: SearchFieldResponse,
    selected: ModelLanguageFilter,
    action: ScreenAction,
    #[cfg(test)]
    language: egui::Response,
    #[cfg(test)]
    refresh: egui::Response,
    #[cfg(test)]
    import: egui::Response,
}

fn model_toolbar(
    ui: &mut egui::Ui,
    query: &mut String,
    management: &ModelManagementState,
    restore_remove_target_gone: bool,
    language_filter: ModelLanguageFilter,
    remote_catalog: &RemoteCatalogView,
) -> ModelToolbarResponse {
    let inline_toolbar_width = 160.0 + 156.0 + 44.0 * 2.0 + ui.spacing().item_spacing.x * 3.0;
    let toolbar_width = models_content_width(ui);
    let mut language = None;
    let (search, selected, catalog_actions) = if toolbar_width >= inline_toolbar_width {
        let mut search = None;
        let mut selected = language_filter;
        let mut catalog_actions = None;
        ui.horizontal(|ui| {
            let reserved = 156.0 + 44.0 * 2.0 + ui.spacing().item_spacing.x * 3.0;
            search = Some(search_field(
                ui,
                (toolbar_width - reserved).max(160.0),
                "models-search",
                query,
                "Search models",
                "Search models by name, language, or variant",
                "Filters installed and available models as you type.",
            ));
            language = Some(model_language_filter_control(ui, &mut selected, false));
            catalog_actions = Some(model_catalog_actions(
                ui,
                management,
                restore_remove_target_gone,
                remote_catalog,
            ));
        });
        (
            search.expect("the inline models toolbar always renders search"),
            selected,
            catalog_actions.expect("the inline models toolbar always renders actions"),
        )
    } else {
        let search = search_field(
            ui,
            toolbar_width,
            "models-search",
            query,
            "Search models",
            "Search models by name, language, or variant",
            "Filters installed and available models as you type.",
        );
        ui.add_space(8.0);
        let mut selected = language_filter;
        let mut catalog_actions = None;
        let compact_language = toolbar_width < 260.0;
        if toolbar_width >= if compact_language { 148.0 } else { 260.0 } {
            ui.horizontal(|ui| {
                language = Some(model_language_filter_control(
                    ui,
                    &mut selected,
                    compact_language,
                ));
                catalog_actions = Some(model_catalog_actions(
                    ui,
                    management,
                    restore_remove_target_gone,
                    remote_catalog,
                ));
            });
        } else {
            // Preserve visual and keyboard order without forcing a narrow
            // route to scroll horizontally. At 45px the globe-only filter
            // and each action take their own 44px row.
            language = Some(model_language_filter_control(ui, &mut selected, true));
            ui.add_space(8.0);
            if toolbar_width >= 96.0 {
                ui.horizontal(|ui| {
                    catalog_actions = Some(model_catalog_actions(
                        ui,
                        management,
                        restore_remove_target_gone,
                        remote_catalog,
                    ));
                });
            } else {
                catalog_actions = Some(model_catalog_actions(
                    ui,
                    management,
                    restore_remove_target_gone,
                    remote_catalog,
                ));
            }
        }
        (
            search,
            selected,
            catalog_actions.expect("the narrow models toolbar always renders actions"),
        )
    };
    #[cfg(not(test))]
    let _ = language;
    ModelToolbarResponse {
        search,
        selected,
        action: catalog_actions.action,
        #[cfg(test)]
        language: language.expect("the models toolbar always renders the language filter"),
        #[cfg(test)]
        refresh: catalog_actions.refresh,
        #[cfg(test)]
        import: catalog_actions.import,
    }
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

struct ModelDownloadModuleResponse {
    response: egui::Response,
    cancel_clicked: bool,
    cancel_has_focus: bool,
    discard_clicked: bool,
    discard_has_focus: bool,
}

fn render_model_download_module(
    ui: &mut egui::Ui,
    progress: &ModelDownloadProgressPresentation,
    primary_icon: Icon,
    cancel_name: &str,
    cancel_enabled: bool,
    cancel_reason: Option<&str>,
    discard_name: Option<&str>,
) -> ModelDownloadModuleResponse {
    let colors = ui_palette(ui);
    let mut cancel_clicked = false;
    let mut cancel_has_focus = false;
    let mut discard_clicked = false;
    let mut discard_has_focus = false;
    let available_width = ui.available_width();
    let control_count = if discard_name.is_some() { 2.0 } else { 1.0 };
    let controls_width = 44.0 * control_count + ui.spacing().item_spacing.x * (control_count - 1.0);
    const MIN_TRACK_WIDTH: f32 = 44.0;
    let track_and_controls_fit =
        available_width >= controls_width + ui.spacing().item_spacing.x + MIN_TRACK_WIDTH;
    let module_height = if track_and_controls_fit {
        22.0 + ui.spacing().item_spacing.y + 44.0
    } else {
        22.0 + 6.0 + 44.0 + 2.0 * ui.spacing().item_spacing.y
    };
    let response = ui
        .allocate_ui_with_layout(
            // The parent lifecycle zone centers this response as one summary
            // item. Reserving its full height centers the label and its
            // track/control row together rather than overflowing downward
            // from a zero-height allocation.
            Vec2::new(available_width, module_height),
            Layout::top_down(Align::Min),
            |ui| {
                let label_width =
                    download_label_slot_width(ui, progress.total_bytes.unwrap_or(u64::MAX));
                let render_track = |ui: &mut egui::Ui, width: f32| {
                    let (track, meter) =
                        ui.allocate_exact_size(Vec2::new(width, 6.0), Sense::hover());
                    ui.painter()
                        .rect_filled(track, Rounding::same(3.0), colors.meter_track);
                    if let Some(fraction) = progress.fraction {
                        let fill = egui::Rect::from_min_size(
                            track.min,
                            Vec2::new(track.width() * fraction, track.height()),
                        );
                        ui.painter()
                            .rect_filled(fill, Rounding::same(3.0), colors.accent);
                        #[cfg(test)]
                        register_model_layout_rect(
                            ui,
                            &progress.accessible_text,
                            "download fill",
                            fill,
                        );
                    }
                    ui.ctx().accesskit_node_builder(meter.id, |builder| {
                        builder.set_role(egui::accesskit::Role::Meter);
                        builder.set_name(progress.accessible_text.as_str());
                        if let Some(fraction) = progress.fraction {
                            builder.set_min_numeric_value(0.0);
                            builder.set_max_numeric_value(1.0);
                            builder.set_numeric_value(f64::from(fraction));
                        }
                        builder.set_bounds(accesskit_rect(track));
                    });
                    #[cfg(test)]
                    register_model_layout_rect(
                        ui,
                        &progress.accessible_text,
                        "download track",
                        track,
                    );
                };
                let mut render_controls = |ui: &mut egui::Ui| {
                    let cancel = compact_model_icon_action(
                        ui,
                        primary_icon,
                        cancel_name,
                        cancel_enabled,
                        cancel_reason,
                        None,
                    );
                    cancel_clicked = cancel.clicked();
                    cancel_has_focus = cancel.has_focus();
                    if let Some(discard_name) = discard_name {
                        let discard = compact_model_icon_action(
                            ui,
                            Icon::Close,
                            discard_name,
                            true,
                            None,
                            None,
                        );
                        discard_clicked = discard.clicked();
                        discard_has_focus = discard.has_focus();
                    }
                };
                let (label_slot, _) = ui.allocate_exact_size(
                    Vec2::new(label_width.min(ui.available_width()), 22.0),
                    Sense::hover(),
                );
                ui.allocate_ui_at_rect(label_slot, |ui| {
                    ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                        ui.label(
                            RichText::new(&progress.display_text)
                                .small()
                                .color(colors.muted_text),
                        );
                    });
                });
                #[cfg(test)]
                register_model_layout_rect(
                    ui,
                    &progress.accessible_text,
                    "download label",
                    label_slot,
                );
                if track_and_controls_fit {
                    ui.horizontal(|ui| {
                        let track_width =
                            (ui.available_width() - controls_width - ui.spacing().item_spacing.x)
                                .max(MIN_TRACK_WIDTH);
                        render_track(ui, track_width);
                        render_controls(ui);
                    });
                } else {
                    // Only wrap at widths that cannot retain a useful 44px
                    // track beside the 44px accessibility targets. Controls
                    // stay below, never above, the progress track.
                    render_track(ui, ui.available_width());
                    ui.horizontal(|ui| render_controls(ui));
                }
            },
        )
        .response;
    ModelDownloadModuleResponse {
        response,
        cancel_clicked,
        cancel_has_focus,
        discard_clicked,
        discard_has_focus,
    }
}

fn download_label_slot_width(ui: &egui::Ui, total_bytes: u64) -> f32 {
    let maximum_label = format!(
        "{} / {}",
        format_download_bytes(total_bytes),
        format_download_bytes(total_bytes)
    );
    ui.fonts(|fonts| {
        fonts
            .layout_no_wrap(
                maximum_label,
                egui::TextStyle::Small.resolve(ui.style()),
                ui_palette(ui).muted_text,
            )
            .rect
            .width()
            .ceil()
    })
}

fn paint_decorative_icon(ui: &mut egui::Ui, icon: Icon, color: Color32) -> egui::Rect {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(16.0, 18.0), Sense::hover());
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        icon_glyph(icon),
        egui::FontId::proportional(14.0),
        color,
    );
    rect
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
            let _language_icon = paint_decorative_icon(ui, Icon::Globe, colors.muted_text);
            let language = ui.label(
                RichText::new(&language_summary)
                    .small()
                    .color(colors.muted_text),
            );
            #[cfg(test)]
            {
                register_model_layout_rect(ui, _model_name, "language icon", _language_icon);
                register_model_layout_rect(ui, _model_name, "language text", language.rect);
            }
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
    let accessible_card_name = match card {
        ModelCard::Local(model) => model.display_name.clone(),
        ModelCard::Remote(entry, variant) => remote_variant_accessible_name(entry, variant),
    };
    let lifecycle = model_lifecycle_controls(card, can_replace_active);
    let show_collapsed_remote_provenance = !expanded
        && matches!(card, ModelCard::Remote(_, _))
        && lifecycle.primary.as_ref().is_some_and(|primary| {
            primary.enabled && matches!(primary.label.as_str(), "Install" | "Resume")
        });
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
            "{} details for {accessible_card_name}",
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
            let Some(primary) = lifecycle.primary.as_ref() else {
                return ui.allocate_exact_size(Vec2::ZERO, Sense::hover()).1;
            };
            if let Some(status) = primary.visible_status.as_deref() {
                let response = ui.label(RichText::new(status).small().color(colors.muted_text));
                ui.ctx().accesskit_node_builder(response.id, |builder| {
                    builder.set_name(status);
                });
                ui.add_space(6.0);
            }
            if (matches!(
                primary.action,
                ScreenAction::CancelModelInstall(_) | ScreenAction::CancelRemoteCatalogInstall(_)
            ) || (lifecycle.discard.is_some() && matches!(primary.icon, Icon::Play)))
                && let Some(progress) = model_download_progress_presentation(card)
            {
                let download = render_model_download_module(
                    ui,
                    &progress,
                    primary.icon,
                    &primary.accessible_name,
                    primary.enabled,
                    primary.disabled_reason,
                    lifecycle.discard_name.as_deref(),
                );
                *focus_within |= download.cancel_has_focus || download.discard_has_focus;
                if download.cancel_clicked && primary.enabled {
                    *action = primary.action.clone();
                } else if download.discard_clicked
                    && let Some(discard) = lifecycle.discard.as_ref()
                {
                    *action = discard.clone();
                }
                return download.response;
            }
            let label = if primary.tone == ModelLifecycleTone::DestructiveOutline {
                primary.label.clone()
            } else {
                primary.compact_size.as_ref().map_or_else(
                    || format!("{}  {}", icon_glyph(primary.icon), primary.label),
                    |size| format!("{}  {size}", icon_glyph(primary.icon)),
                )
            };
            let mut lifecycle_response = model_lifecycle_button(
                ui,
                &label,
                &primary.accessible_name,
                primary.enabled,
                primary.disabled_reason,
                primary.tone,
            );
            *focus_within |= lifecycle_response.has_focus();
            if restore_remove_focus
                && matches!(primary.action, ScreenAction::RequestModelRemoval(_))
            {
                lifecycle_response.request_focus();
                *restored_remove_focus = true;
            }
            if lifecycle_response.clicked() && primary.enabled {
                *action = primary.action.clone();
            }
            if let (Some(discard), Some(discard_name)) = (
                lifecycle.discard.as_ref(),
                lifecycle.discard_name.as_deref(),
            ) {
                let discard_response =
                    compact_model_icon_action(ui, Icon::Close, discard_name, true, None, None);
                *focus_within |= discard_response.has_focus();
                if discard_response.clicked() {
                    *action = discard.clone();
                }
                lifecycle_response = lifecycle_response.union(discard_response);
            }
            if let (Some(error_name), Some(error_message)) = (
                lifecycle.error_accessible_name.as_deref(),
                lifecycle.error_message,
            ) {
                let alert =
                    model_download_error_affordance(ui, card.key(), error_name, error_message);
                *focus_within |= alert.has_focus();
                lifecycle_response.union(alert)
            } else {
                lifecycle_response
            }
        };
        let render_collapsed_remote_provenance = |ui: &mut egui::Ui| {
            if show_collapsed_remote_provenance && let ModelCard::Remote(entry, variant) = card {
                render_model_layout_gap(ui, name, "install provenance gap", 8.0);
                let heading = detail_heading(ui, "DOWNLOAD PROVENANCE", colors);
                ui.ctx().accesskit_node_builder(heading.id, |builder| {
                    builder.set_name("Download provenance");
                });
                render_model_layout_gap(ui, name, "install provenance content gap", 4.0);
                render_remote_model_provenance_rows(ui, entry, variant);
            }
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
                let feature_width = ui.available_width().min(72.0);
                activation_exclusions.push(render_model_features(ui, card, feature_width).rect);
            });
            ui.horizontal(|ui| {
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
            render_collapsed_remote_provenance(ui);
        } else {
            let identity_width = card_content_width * 0.50;
            let metrics_width = card_content_width * 0.24;
            let lifecycle_width = card_content_width - identity_width - metrics_width;
            let details_width = 44.0;
            ui.scope(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                ui.horizontal_top(|ui| {
                    let _identity_zone = ui.allocate_ui_with_layout(
                        Vec2::new(identity_width, 0.0),
                        Layout::top_down(Align::Min),
                        |ui| {
                            ui.set_width(identity_width);
                            let identity =
                                render_model_identity(ui, name, active, false, identity_width);
                            focus_within |= identity.has_focus;
                            description_fade_rect = render_model_description(
                                ui,
                                &description,
                                identity_width,
                                26.0,
                                expanded,
                            );
                            ui.add_space(4.0);
                            let metadata_group_width = identity_width * 0.60;
                            let metadata_cell_width = identity_width * 0.30;
                            let (summary_features, _) = model_summary_features(card);
                            let (_, _, feature_grid_size) = model_feature_grid_geometry(
                                summary_features.len(),
                                metadata_cell_width,
                            );
                            let (metadata_group_rect, _) = ui.allocate_exact_size(
                                Vec2::new(metadata_group_width, feature_grid_size.y),
                                Sense::hover(),
                            );
                            let language_cell_rect = egui::Rect::from_min_size(
                                metadata_group_rect.min,
                                Vec2::new(metadata_cell_width, metadata_group_rect.height()),
                            );
                            let feature_cell_rect = egui::Rect::from_min_size(
                                language_cell_rect.right_top(),
                                Vec2::new(metadata_cell_width, metadata_group_rect.height()),
                            );
                            let language_content_rect = egui::Rect::from_min_size(
                                language_cell_rect.min,
                                Vec2::new(
                                    language_cell_rect.width(),
                                    feature_grid_size.y.min(32.0),
                                ),
                            )
                            // Align painted small-text and globe centers with the first feature row.
                            .translate(Vec2::new(0.0, MODEL_LANGUAGE_OPTICAL_OFFSET_Y));
                            let features = ui.allocate_ui_at_rect(metadata_group_rect, |ui| {
                                ui.spacing_mut().item_spacing.x = 0.0;
                                ui.allocate_ui_at_rect(language_content_rect, |ui| {
                                    ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                                        render_model_metadata(ui, name, languages, false)
                                    });
                                });
                                ui.allocate_ui_at_rect(feature_cell_rect, |ui| {
                                    ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                                        render_model_features(ui, card, metadata_cell_width)
                                    })
                                })
                                .inner
                            });
                            activation_exclusions.push(features.inner.inner.rect);
                            #[cfg(test)]
                            {
                                register_model_layout_rect(
                                    ui,
                                    name,
                                    "metadata group",
                                    metadata_group_rect,
                                );
                                register_model_layout_rect(
                                    ui,
                                    name,
                                    "language cell",
                                    language_cell_rect,
                                );
                                register_model_layout_rect(
                                    ui,
                                    name,
                                    "feature cell",
                                    feature_cell_rect,
                                );
                            }
                        },
                    );
                    let _metrics_zone = ui.allocate_ui_with_layout(
                        Vec2::new(metrics_width, MODEL_CARD_SUMMARY_HEIGHT),
                        Layout::top_down(Align::Min),
                        |ui| {
                            ui.set_width(metrics_width);
                            ui.add_space((MODEL_CARD_SUMMARY_HEIGHT - 44.0) / 2.0);
                            let (metric_row, _) = ui.allocate_exact_size(
                                Vec2::new(metrics_width, 44.0),
                                Sense::hover(),
                            );
                            let metric_width = metric_row.width() / 2.0;
                            for (index, (metric_name, rating)) in [
                                (
                                    "Speed",
                                    match card {
                                        ModelCard::Local(model) => speed_rating(model.speed_tier),
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
                            .enumerate()
                            {
                                let cell = egui::Rect::from_min_size(
                                    egui::pos2(
                                        metric_row.left() + index as f32 * metric_width,
                                        metric_row.top(),
                                    ),
                                    Vec2::new(metric_width, metric_row.height()),
                                );
                                ui.allocate_ui_at_rect(cell, |ui| {
                                    ui.set_width(cell.width());
                                    ui.with_layout(Layout::top_down(Align::Center), |ui| {
                                        rating_meter(ui, metric_name, rating, true)
                                    });
                                });
                            }
                        },
                    );
                    let (lifecycle_rect, _) = ui.allocate_exact_size(
                        Vec2::new(lifecycle_width, MODEL_CARD_SUMMARY_HEIGHT),
                        Sense::hover(),
                    );
                    let _lifecycle_zone = ui.allocate_ui_at_rect(lifecycle_rect, |ui| {
                        ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                            let body_width = (lifecycle_width - details_width).max(0.0);
                            let body_rect = egui::Rect::from_min_size(
                                lifecycle_rect.min,
                                Vec2::new(body_width, MODEL_CARD_SUMMARY_HEIGHT),
                            );
                            let _body = ui.allocate_ui_at_rect(body_rect, |ui| {
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    activation_exclusions.push(
                                        render_lifecycle(
                                            ui,
                                            &mut action,
                                            &mut restored_remove_focus,
                                            &mut focus_within,
                                        )
                                        .rect,
                                    );
                                });
                            });
                            #[cfg(test)]
                            register_model_layout_rect(ui, name, "lifecycle body", body_rect);
                            let rail_rect = egui::Rect::from_min_size(
                                egui::pos2(body_rect.right(), lifecycle_rect.center().y - 22.0),
                                Vec2::new(details_width, 44.0),
                            );
                            let _rail = ui.allocate_ui_at_rect(rail_rect, |ui| {
                                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                                    activation_exclusions.push(
                                        render_details(ui, &mut action, &mut focus_within).rect,
                                    )
                                });
                            });
                            #[cfg(test)]
                            register_model_layout_rect(ui, name, "chevron zone", rail_rect);
                        });
                    });
                    #[cfg(test)]
                    {
                        register_model_layout_rect(
                            ui,
                            name,
                            "identity zone",
                            _identity_zone.response.rect,
                        );
                        register_model_layout_rect(
                            ui,
                            name,
                            "metrics zone",
                            _metrics_zone.response.rect,
                        );
                        register_model_layout_rect(ui, name, "lifecycle zone", lifecycle_rect);
                    }
                });
            });
            render_collapsed_remote_provenance(ui);
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
    let card_accessible_description = match card {
        ModelCard::Local(model) if model.included && model.installed && model.ready => {
            Some("Installed with Scribe; this model cannot be removed.".to_owned())
        }
        _ => model_download_progress_presentation(card).map(|progress| progress.accessible_text),
    };
    ui.ctx().accesskit_node_builder(frame.id, |builder| {
        builder.set_role(egui::accesskit::Role::Group);
        builder.set_name(format!("{accessible_card_name} model"));
        if let Some(description) = card_accessible_description {
            builder.set_description(description);
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
                let maintenance =
                    model.runtime_action_label.is_some() || model.legacy_cleanup_pending;
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
                render_model_layout_gap(ui, model_name, "requirements provenance gap", 12.0);
                let provenance_heading = detail_heading(ui, "PROVENANCE", colors);
                ui.ctx()
                    .accesskit_node_builder(provenance_heading.id, |builder| {
                        builder.set_name("Model provenance");
                    });
                render_model_layout_gap(ui, model_name, "provenance heading content gap", 6.0);
                render_remote_model_provenance_rows(ui, entry, variant);
            }
        }
    });
    restored_remove_focus
}

fn render_remote_model_provenance_rows(
    ui: &mut egui::Ui,
    entry: &RemoteCatalogEntryView,
    variant: &RemoteCatalogVariantView,
) {
    let trust = ui.label(format!("Trust: {}", entry.trust_label));
    ui.ctx().accesskit_node_builder(trust.id, |builder| {
        builder.set_name(format!("Trust: {}", entry.trust_label));
    });
    let repository = ui.label(format!("Repository: {}", entry.repository));
    ui.ctx().accesskit_node_builder(repository.id, |builder| {
        builder.set_name(format!("Repository: {}", entry.repository));
    });
    let compatibility = ui.label(format!("Compatibility: {}", entry.compatibility_detail));
    ui.ctx()
        .accesskit_node_builder(compatibility.id, |builder| {
            builder.set_name(format!("Compatibility: {}", entry.compatibility_detail));
        });
    let revision = ui.label(format!("Pinned revision: {}", entry.pinned_revision));
    ui.ctx().accesskit_node_builder(revision.id, |builder| {
        builder.set_name(format!("Pinned revision: {}", entry.pinned_revision));
    });
    let artifact = ui.label(format!("Artifact: {}", variant.filename));
    ui.ctx().accesskit_node_builder(artifact.id, |builder| {
        builder.set_name(format!("Artifact: {}", variant.filename));
    });
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
    toggle_action: ScreenAction,
    focus: ModelSectionFocus,
    _terminal: bool,
) -> (ScreenAction, bool) {
    let colors = ui_palette(ui);
    let (header_rect, header) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), 44.0), Sense::click());
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
        builder.set_name(format!(
            "{} {name} models",
            if expanded { "Collapse" } else { "Expand" }
        ));
        builder.set_expanded(expanded);
        builder.set_bounds(accesskit_rect(header_rect));
    });
    paint_focus_ring(ui, &header, Rounding::same(5.0));
    scroll_focused_control_into_view(ui, &header);
    let mut action = if header.clicked() {
        toggle_action
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
    ui.set_width(models_content_width(ui));
    let colors = ui_palette(ui);
    let mut action = ScreenAction::None;
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
    let response = ui.add(
        egui::Label::new(RichText::new("Models").size(30.0).strong())
            .selectable(false)
            .sense(Sense::focusable_noninteractive()),
    );
    ui.ctx().accesskit_node_builder(response.id, |builder| {
        builder.set_role(egui::accesskit::Role::Heading);
        builder.set_name("Models");
        builder.set_bounds(accesskit_rect(response.rect));
    });
    if ui.data_mut(|data| {
        let id = egui::Id::new(MODELS_ROUTE_HEADING_FOCUS_REQUEST);
        let requested = data.get_temp::<bool>(id).unwrap_or(false);
        data.remove::<bool>(id);
        requested
    }) {
        response.request_focus();
    }
    ui.label(
        RichText::new("Manage the speech models available on this device.")
            .color(colors.muted_text),
    );
    ui.add_space(18.0);
    let mut query = remote_catalog.query.clone();
    let toolbar = model_toolbar(
        ui,
        &mut query,
        management,
        restore_remove_target_gone,
        language_filter,
        remote_catalog,
    );
    if toolbar.search.changed {
        action = ScreenAction::SetRemoteCatalogQuery(query);
    }
    if toolbar.search.clear_requested {
        action = ScreenAction::SetRemoteCatalogQuery(String::new());
    }
    if toolbar.selected != language_filter {
        action = ScreenAction::SetModelLanguageFilter(toolbar.selected);
    }
    merge_model_action(&mut action, toolbar.action);
    ui.add_space(8.0);
    if matches!(
        remote_catalog.status.kind,
        RemoteCatalogStatusKind::Loading | RemoteCatalogStatusKind::Error | RemoteCatalogStatusKind::Idle
    ) {
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
    }
    if let Some(summary) = management.install_status_summary.as_deref() {
        ui.add_space(6.0);
        let aggregate_status = ui
            .push_id("aggregate-model-install-status", |ui| {
                ui.label(RichText::new(summary).small().color(colors.muted_text))
            })
            .inner;
        ui.ctx()
            .accesskit_node_builder(aggregate_status.id, |builder| {
                builder.set_role(egui::accesskit::Role::Status);
                builder.set_name(summary);
                builder.set_live(egui::accesskit::Live::Polite);
                builder.set_live_atomic();
            });
    }
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
    let comparison_viewport = ui
        .data(|data| data.get_temp::<egui::Rect>(egui::Id::new(("route-viewport", UiRoute::Models))))
        .unwrap_or_else(|| ui.clip_rect());
    let comparison_max_height = if comparison.expanded {
        comparison_viewport.height() * 0.6
    } else {
        MODEL_COMPARISON_COLLAPSED_HEIGHT
    };
    let result_count = ui.allocate_response(egui::Vec2::ZERO, Sense::hover());
    ui.ctx()
        .accesskit_node_builder(result_count.id, |builder| {
            builder.set_role(egui::accesskit::Role::Status);
            builder.set_name(format!(
                "{} model results: {} installed, {} available.",
                installed_cards.len() + available_cards.len(),
                installed_cards.len(),
                available_cards.len()
            ));
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
            management.installed_expanded || management.restore_remove_focus.is_some(),
            ScreenAction::ToggleInstalledModels,
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
            management.available_expanded,
            ScreenAction::ToggleAvailableModels,
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
        ModelCard::Local(model)
            if model.download_state == ModelDownloadState::Downloading
                || (matches!(
                    model.download_state,
                    ModelDownloadState::Cancelled | ModelDownloadState::Failed
                ) && model.partial_cleanup_available) =>
        {
            (model.downloaded_bytes, model.total_bytes)
        }
        // Retained byte counts can exist while queued or waiting; only an
        // actively transferring variant owns a truthful progress meter.
        ModelCard::Remote(_, variant) if variant.status_label.as_deref() == Some("Downloading") => {
            (variant.downloaded_bytes?, variant.total_bytes)
        }
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
                    "{} / {}",
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
            format!(
                "{} / Total unknown",
                format_download_bytes(downloaded_bytes)
            ),
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
        });
        let _ = SettingsRow::show_with_help(
            ui,
            "Input level",
            SPEECH_DETECTION_SENSITIVITY_HELP_ID,
            INPUT_LEVEL_SENSITIVITY_DESCRIPTION,
            false,
            |ui, label_id| {
                let (icon_rect, _) = ui.allocate_exact_size(Vec2::new(24.0, 40.0), Sense::hover());
                ui.painter().text(
                    icon_rect.center(),
                    Align2::CENTER_CENTER,
                    icon_glyph(Icon::Microphone),
                    egui::FontId::proportional(18.0),
                    colors.muted_text,
                );
                let mut percent = settings.speech_detection_sensitivity_percent;
                let response = input_level_sensitivity_control(
                    ui,
                    settings.input_level_percent,
                    &mut percent,
                    !recording_locked,
                )
                .labelled_by(label_id);
                if response.changed() {
                    *action = ScreenAction::SetSpeechDetectionSensitivity(percent);
                }
            },
        );
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
            let _ = SettingsRow::show(ui, "Streaming mode", true, |ui, label_id| {
                let response = ComboBox::from_id_source("streaming-mode")
                    .selected_text(&streaming)
                    .show_ui(ui, |ui| {
                        for value in ["Auto", "Rolling preview", "Final text only"] {
                            ui.selectable_value(&mut streaming, value.to_owned(), value);
                        }
                    })
                    .response
                    .labelled_by(label_id);
                describe_setting(ui, &response, STREAMING_MODE_HELP);
            });
            if streaming != settings.streaming_label {
                *action = ScreenAction::SetStreamingMode(streaming);
            }
            let mut acceleration = settings.acceleration_label.clone();
            let _ = SettingsRow::show(ui, "Transcription device", false, |ui, label_id| {
                let response = ComboBox::from_id_source("advanced-transcription-device-mode")
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
                describe_setting(ui, &response, TRANSCRIPTION_DEVICE_HELP);
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
                for (index, (label, value, action_for, help)) in [
                    (
                        "Speech confirmation ms",
                        settings.speech_confirmation_ms,
                        0,
                        SPEECH_CONFIRMATION_HELP,
                    ),
                    (
                        "Internal pause ms",
                        settings.internal_pause_ms,
                        1,
                        INTERNAL_PAUSE_HELP,
                    ),
                    (
                        "End after silence ms",
                        settings.endpoint_silence_ms,
                        2,
                        END_AFTER_SILENCE_HELP,
                    ),
                    ("Pre-roll ms", settings.pre_roll_ms, 3, PRE_ROLL_HELP),
                    ("Post-roll ms", settings.post_roll_ms, 4, POST_ROLL_HELP),
                ]
                .into_iter()
                .enumerate()
                {
                    let _ = SettingsRow::show(ui, label, index < 4, |ui, label_id| {
                        let mut edited = value as i64;
                        let response = ui
                            .add_sized(
                                [96.0, 44.0],
                                egui::DragValue::new(&mut edited).clamp_range(0..=5_000),
                            )
                            .labelled_by(label_id);
                        describe_setting(ui, &response, help);
                        if response.changed() {
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
            let manage = button(ui, "Manage models", ButtonTone::Secondary);
            describe_setting(ui, &manage, ACTIVE_MODEL_HELP);
            if manage.clicked() {
                *action = ScreenAction::OpenModelSettings;
            }
        });
    });
    ui.add_space(16.0);
    settings_section(ui, "Appearance", |ui| {
        let mut theme = settings.theme_label.clone();
        compact_setting_row(ui, "Theme", true, |ui, label_id| {
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
        let _ = SettingsRow::show(ui, "Dictation overlay", true, |ui, label_id| {
            ui.vertical(|ui| {
                let response = ui.add_enabled_ui(settings.overlay_available, |ui| {
                    ComboBox::from_id_source("overlay-mode")
                        .selected_text(&overlay)
                        .show_ui(ui, |ui| {
                            for value in ["Live preview", "Compact status", "Off"] {
                                ui.selectable_value(&mut overlay, value.to_owned(), value);
                            }
                        })
                        .response
                        .labelled_by(label_id)
                }).inner;
                describe_setting(ui, &response, DICTATION_OVERLAY_HELP);
                if !settings.overlay_available {
                    ui.label(RichText::new("The overlay is unavailable because focus safety is not verified on this platform.").color(ui_palette(ui).warning));
                }
            });
        });
        if overlay != settings.overlay_label {
            *action = ScreenAction::SetOverlayMode(overlay);
        }
        let mut position = settings.overlay_position_label.clone();
        compact_setting_row(ui, "Overlay position", false, |ui, label_id| {
            ui.add_enabled_ui(
                settings.overlay_available && settings.overlay_label != "Off",
                |ui| {
                    ComboBox::from_id_source("overlay-position")
                        .selected_text(&position)
                        .show_ui(ui, |ui| {
                            for value in ["Top", "Bottom"] {
                                ui.selectable_value(&mut position, value.to_owned(), value);
                            }
                        })
                        .response
                        .labelled_by(label_id)
                },
            );
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
                    let response = ui
                        .add_sized(
                            [96.0, 44.0],
                            egui::DragValue::new(&mut delay).clamp_range(1..=1_000),
                        )
                        .labelled_by(label_id);
                    describe_setting(ui, &response, PASTE_DELAY_HELP);
                    if response.changed() {
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
                describe_setting(ui, &response, HISTORY_STORAGE_HELP);
                describe_history_lock(
                    ui,
                    &response,
                    settings.history_locked,
                    Some(HISTORY_STORAGE_HELP.description),
                );
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
                    describe_setting(ui, &response, MAX_HISTORY_ENTRIES_HELP);
                    describe_history_lock(
                        ui,
                        &response,
                        settings.history_locked,
                        Some(MAX_HISTORY_ENTRIES_HELP.description),
                    );
                    if response.changed() {
                        *action = ScreenAction::SetMaxHistoryEntries(maximum as u32);
                    }
                });
                optional_retention_control(
                    ui,
                    OptionalRetentionSetting {
                        label: "Limit transcript age",
                        days_label: "Transcript days",
                        unlimited_label: "Keep transcripts until deleted",
                        configured_days: settings.transcript_retention_days,
                        help: SettingsHelp::new(
                            LIMIT_TRANSCRIPT_AGE_SWITCH_ID,
                            LIMIT_TRANSCRIPT_AGE_DESCRIPTION,
                        ),
                        days_help: TRANSCRIPT_RETENTION_DAYS_HELP,
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
                            days_label: "Audio days",
                            unlimited_label: "Keep retained audio until its entry is deleted",
                            configured_days: settings.audio_retention_days,
                            help: SettingsHelp::new(
                                LIMIT_AUDIO_AGE_SWITCH_ID,
                                LIMIT_AUDIO_AGE_DESCRIPTION,
                            ),
                            days_help: AUDIO_RETENTION_DAYS_HELP,
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
    days_label: &'a str,
    unlimited_label: &'a str,
    configured_days: Option<u32>,
    help: SettingsHelp,
    days_help: SettingsHelp,
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
        setting.help.id_source,
        setting.help.description,
        false,
        |ui, _| {
            ui.vertical(|ui| {
                let limit = settings_switch(
                    ui,
                    setting.help.id_source,
                    limited,
                    setting.label,
                    setting.help.description,
                    !history_locked,
                );
                describe_history_lock(ui, &limit, history_locked, Some(setting.help.description));
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
        let _ = SettingsRow::show(ui, setting.days_label, false, |ui, label_id| {
            let response = ui
                .add_enabled_ui(!history_locked, |ui| {
                    ui.add_sized(
                        [96.0, 44.0],
                        egui::DragValue::new(&mut days).clamp_range(1..=3_650),
                    )
                    .labelled_by(label_id)
                })
                .inner;
            describe_setting(ui, &response, setting.days_help);
            describe_history_lock(
                ui,
                &response,
                history_locked,
                Some(setting.days_help.description),
            );
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

fn input_level_sensitivity_control(
    ui: &mut egui::Ui,
    live_level_percent: u8,
    sensitivity_percent: &mut u8,
    sensitivity_enabled: bool,
) -> egui::Response {
    use egui::accesskit::{Action, ActionData};

    const TRACK_WIDTH: f32 = 280.0;
    const LABEL_GAP: f32 = 12.0;
    const LABEL_WIDTH: f32 = 108.0;

    let desired = Vec2::new(TRACK_WIDTH + LABEL_GAP + LABEL_WIDTH, 44.0);
    let (rect, _) = ui.allocate_exact_size(desired, Sense::hover());
    let mut response = ui.interact(
        rect,
        egui::Id::new(INPUT_LEVEL_SENSITIVITY_CONTROL_ID),
        if sensitivity_enabled {
            Sense::click_and_drag()
        } else {
            Sense::hover()
        },
    );
    let track = egui::Rect::from_center_size(
        egui::pos2(rect.left() + TRACK_WIDTH * 0.5, rect.center().y),
        Vec2::new(TRACK_WIDTH, 10.0),
    );
    let previous = *sensitivity_percent;
    let mut value = f32::from(*sensitivity_percent).clamp(0.0, 100.0);

    if sensitivity_enabled && response.clicked() {
        response.request_focus();
    }
    if sensitivity_enabled
        && (response.clicked() || response.dragged())
        && let Some(pointer) = response.interact_pointer_pos()
        && pointer.x <= track.right()
    {
        value = (100.0 * (pointer.x - track.left()) / track.width()).clamp(0.0, 100.0);
    }

    let mut decrement = 0usize;
    let mut increment = 0usize;
    if sensitivity_enabled && response.has_focus() {
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
    if sensitivity_enabled {
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
    *sensitivity_percent = value.round() as u8;
    if *sensitivity_percent != previous {
        response.mark_changed();
    }

    let threshold = 0.8 - (f32::from(*sensitivity_percent) / 100.0) * 0.6;
    let mut description = format!(
        "{INPUT_LEVEL_SENSITIVITY_DESCRIPTION} {SPEECH_DETECTION_SENSITIVITY_DESCRIPTION} {} percent sensitivity uses a {threshold:.2} speech probability threshold. Use Left and Right arrow keys to adjust.",
        *sensitivity_percent
    );
    if !sensitivity_enabled {
        description.push(' ');
        description.push_str(VOICE_DETECTION_LOCKED_DESCRIPTION);
    }
    response.widget_info(|| {
        egui::WidgetInfo::slider(
            f64::from(*sensitivity_percent),
            "Speech detection sensitivity",
        )
    });
    ui.ctx().accesskit_node_builder(response.id, |builder| {
        builder.set_name("Speech detection sensitivity");
        builder.set_description(description);
        builder.set_min_numeric_value(0.0);
        builder.set_max_numeric_value(100.0);
        builder.set_numeric_value_step(1.0);
        if sensitivity_enabled {
            builder.add_action(Action::SetValue);
            if *sensitivity_percent < 100 {
                builder.add_action(Action::Increment);
            }
            if *sensitivity_percent > 0 {
                builder.add_action(Action::Decrement);
            }
        } else {
            builder.set_disabled();
        }
    });

    let colors = ui_palette(ui);
    let rounding = Rounding::same(5.0);
    ui.painter()
        .rect_filled(track, rounding, colors.slider_remainder_fill);
    let live_width = track.width() * (f32::from(live_level_percent.min(100)) / 100.0);
    if live_width > 0.0 {
        ui.painter().rect_filled(
            egui::Rect::from_min_size(track.min, Vec2::new(live_width, track.height())),
            rounding,
            colors.slider_live_above,
        );
    }
    ui.painter().rect_stroke(
        track,
        rounding,
        Stroke::new(1.0, colors.slider_track_border),
    );
    let sensitivity_position = f32::from(*sensitivity_percent) / 100.0;
    let sensitivity_x = track.left() + track.width() * sensitivity_position;
    let thumb_center = egui::pos2(sensitivity_x, track.center().y);
    let thumb_radius = if sensitivity_enabled && response.dragged() {
        9.0
    } else {
        8.0
    };
    // The two high-contrast rings keep the sensitivity marker visible over
    // both the idle track and the teal microphone-level fill.
    ui.painter().circle_filled(
        thumb_center,
        thumb_radius + 2.0,
        colors.sensitivity_marker_on_track,
    );
    ui.painter().circle_filled(
        thumb_center,
        thumb_radius,
        colors.sensitivity_marker_on_live,
    );
    ui.painter()
        .circle_filled(thumb_center, thumb_radius - 3.0, colors.card_bg);
    ui.painter().text(
        egui::pos2(track.right() + LABEL_GAP, rect.center().y),
        Align2::LEFT_CENTER,
        format!("Sensitivity {}%", *sensitivity_percent),
        egui::FontId::proportional(14.0),
        if sensitivity_enabled {
            colors.text
        } else {
            colors.muted_text
        },
    );
    if sensitivity_enabled {
        paint_focus_ring(ui, &response, Rounding::same(5.0));
    }
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
        let help = settings_help_metadata(label).map(|help| (help.id_source, help.description));
        Self::show_with_optional_help(ui, label, help, separator_after, contents)
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
    popup_affordance(
        ui,
        egui::Id::new(("settings-help-affordance", id_source)),
        egui::Id::new(("settings-help-popup", id_source)),
        egui::Id::new("settings-help-state"),
        &format!("{accessible_name} information"),
        description,
        "?",
    );
}

fn model_download_error_affordance(
    ui: &mut egui::Ui,
    card_key: ModelCardKey,
    accessible_name: &str,
    description: &str,
) -> egui::Response {
    popup_affordance(
        ui,
        egui::Id::new(("model-download-error-affordance", card_key.clone())),
        egui::Id::new(("model-download-error-popup", card_key)),
        egui::Id::new("model-download-error-state"),
        accessible_name,
        description,
        icon_glyph(Icon::Warning),
    )
}

fn popup_affordance(
    ui: &mut egui::Ui,
    control_id: egui::Id,
    popup_id: egui::Id,
    state_id: egui::Id,
    accessible_name: &str,
    description: &str,
    glyph: &str,
) -> egui::Response {
    const HOVER_DELAY_SECONDS: f64 = 0.3;
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(44.0), Sense::hover());
    let response = ui.interact(rect, control_id, Sense::click());
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
        glyph,
        egui::FontId::proportional(14.0),
        colors.muted_text,
    );
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, accessible_name));
    ui.ctx().accesskit_node_builder(response.id, |builder| {
        builder.set_role(egui::accesskit::Role::Button);
        builder.set_name(accessible_name);
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
    response
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

fn compact_setting_row(
    ui: &mut egui::Ui,
    label: &str,
    separator_after: bool,
    contents: impl FnOnce(&mut egui::Ui, egui::Id),
) {
    let _ = SettingsRow::show(ui, label, separator_after, contents);
}

fn describe_setting(ui: &egui::Ui, response: &egui::Response, help: SettingsHelp) {
    ui.ctx().accesskit_node_builder(response.id, |builder| {
        builder.set_description(help.description);
    });
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
    use crate::{
        history::{HistoryMetrics, HistoryRecord, HistoryStatus},
        ui::{HistoryPageAction, HistoryPageState, history_page, theme::ThemePalette},
    };

    type AccessKitNodes = std::collections::HashMap<egui::accesskit::NodeId, egui::accesskit::Node>;
    type OrphanedNodes = Vec<(egui::accesskit::NodeId, Option<String>)>;
    type IncrementalUpdateResult = (
        AccessKitNodes,
        egui::accesskit::NodeId,
        OrphanedNodes,
        OrphanedNodes,
    );

    #[derive(Default)]
    struct NoopAccessKitChangeHandler;

    impl accesskit_consumer::TreeChangeHandler for NoopAccessKitChangeHandler {
        fn node_added(&mut self, _node: &accesskit_consumer::Node<'_>) {}

        fn node_updated(
            &mut self,
            _old_node: &accesskit_consumer::DetachedNode,
            _new_node: &accesskit_consumer::Node<'_>,
        ) {
        }

        fn focus_moved(
            &mut self,
            _old_node: Option<&accesskit_consumer::DetachedNode>,
            _new_node: Option<&accesskit_consumer::Node<'_>>,
            _current_state: &accesskit_consumer::TreeState,
        ) {
        }

        fn node_removed(
            &mut self,
            _node: &accesskit_consumer::DetachedNode,
            _current_state: &accesskit_consumer::TreeState,
        ) {
        }
    }

    /// Mirrors the orphan-removal pass in AccessKit consumer 0.16.1. This is
    /// intentionally exercised against egui's real consecutive TreeUpdates:
    /// an updated node must never also be removed as an orphan.
    fn apply_accesskit_incremental_update(
        initial: &egui::accesskit::TreeUpdate,
        update: &egui::accesskit::TreeUpdate,
    ) -> IncrementalUpdateResult {
        let mut nodes = initial
            .nodes
            .iter()
            .cloned()
            .collect::<std::collections::HashMap<_, _>>();
        let initial_ids = nodes
            .keys()
            .copied()
            .collect::<std::collections::HashSet<_>>();
        let mut orphans = std::collections::HashSet::new();
        let mut updated = std::collections::HashSet::new();
        let mut added = std::collections::HashSet::new();
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
            let old = nodes.insert(*id, data.clone());
            if initial_ids.contains(id) {
                updated.insert(*id);
            } else {
                added.insert(*id);
            }
            if let Some(old) = old {
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
        let named_orphans = |candidates: &std::collections::HashSet<egui::accesskit::NodeId>| {
            candidates
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
                .collect::<OrphanedNodes>()
        };
        let orphaned_updated = named_orphans(&updated);
        let orphaned_added = named_orphans(&added);
        for id in removed {
            nodes.remove(&id);
        }
        let root = update.tree.as_ref().map_or(old_root, |tree| tree.root);
        (nodes, root, orphaned_updated, orphaned_added)
    }

    fn accesskit_is_descendant(
        nodes: &AccessKitNodes,
        ancestor: egui::accesskit::NodeId,
        target: egui::accesskit::NodeId,
    ) -> bool {
        nodes.get(&ancestor).is_some_and(|node| {
            node.children()
                .iter()
                .any(|child| *child == target || accesskit_is_descendant(nodes, *child, target))
        })
    }

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
    fn models_search_preserves_saved_section_state_and_toggle_actions() {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        crate::ui::controls::configure_accessible_style(&ctx);
        let installed = ModelViewModel {
            id: "matching-installed".into(),
            display_name: "Matching installed model".into(),
            installed: true,
            ready: true,
            languages: vec!["en".into()],
            ..Default::default()
        };
        let available = ModelViewModel {
            id: "matching-available".into(),
            display_name: "Matching available model".into(),
            languages: vec!["en".into()],
            ..Default::default()
        };
        let management = ModelManagementState {
            installed_expanded: false,
            available_expanded: false,
            ..Default::default()
        };
        let remote_catalog = RemoteCatalogView {
            query: "matching".into(),
            refresh_enabled: true,
            ..Default::default()
        };
        let render = |events| {
            let mut action = ScreenAction::None;
            let output = ctx.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        Vec2::new(680.0, 500.0),
                    )),
                    events,
                    focused: true,
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        action = models(
                            ui,
                            std::slice::from_ref(&installed),
                            std::slice::from_ref(&available),
                            &ModelComparisonState::default(),
                            &management,
                            ModelLanguageFilter::All,
                            &remote_catalog,
                        );
                    });
                },
            );
            (output, action)
        };
        let (initial, action) = render(Vec::new());
        assert_eq!(action, ScreenAction::None);
        let nodes = &initial
            .platform_output
            .accesskit_update
            .as_ref()
            .expect("models search should update AccessKit")
            .nodes;
        assert!(nodes.iter().any(|(_, node)| {
            node.role() == egui::accesskit::Role::Status
                && node.name() == Some("2 model results: 1 installed, 1 available.")
        }));
        for name in ["Expand Installed models", "Expand Available models"] {
            let node = nodes
                .iter()
                .find_map(|(_, node)| (node.name() == Some(name)).then_some(node))
                .unwrap_or_else(|| panic!("missing active-search toggle: {name}"));
            assert_eq!(node.role(), egui::accesskit::Role::Button);
            assert!(
                !node.is_disabled(),
                "{name} must remain available during search"
            );
        }
        let installed_bounds = named_role_bounds(
            &initial,
            "Expand Installed models",
            egui::accesskit::Role::Button,
        );
        let click_point = accesskit_rect_center(installed_bounds);
        let _ = render(vec![egui::Event::PointerButton {
            pos: click_point,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::NONE,
        }]);
        let (_, action) = render(vec![egui::Event::PointerButton {
            pos: click_point,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        }]);
        assert_eq!(action, ScreenAction::ToggleInstalledModels);
    }

    struct ModelToolbarBounds {
        content: egui::Rect,
        search: egui::Rect,
        language: egui::Rect,
        refresh: egui::Rect,
        import: egui::Rect,
    }

    fn render_model_toolbar_in_content_region(
        width: f32,
        dark_mode: bool,
        text_scale: f32,
    ) -> ModelToolbarBounds {
        let management = ModelManagementState::default();
        let remote_catalog = RemoteCatalogView {
            refresh_enabled: true,
            ..Default::default()
        };
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        crate::ui::controls::configure_accessible_style(&ctx);
        ctx.set_visuals(if dark_mode {
            egui::Visuals::dark()
        } else {
            egui::Visuals::light()
        });
        if text_scale > 1.0 {
            ctx.style_mut(|style| {
                for font in style.text_styles.values_mut() {
                    font.size *= text_scale;
                }
            });
        }
        let mut bounds = None;
        let _ = ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    Vec2::new(1_000.0, 500.0),
                )),
                focused: true,
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    // This harness supplies the toolbar's exact content
                    // width, avoiding panel-margin artifacts. ui_palette
                    // derives Scribe's ThemePalette from these visuals.
                    assert_eq!(
                        ui_palette(ui).panel_bg,
                        if dark_mode {
                            ThemePalette::dark().panel_bg
                        } else {
                            ThemePalette::light().panel_bg
                        }
                    );
                    let content =
                        egui::Rect::from_min_size(ui.cursor().min, Vec2::new(width, 320.0));
                    ui.allocate_ui_at_rect(content, |ui| {
                        ui.set_width(content.width());
                        let mut query = String::new();
                        let toolbar = model_toolbar(
                            ui,
                            &mut query,
                            &management,
                            false,
                            ModelLanguageFilter::All,
                            &remote_catalog,
                        );
                        bounds = Some(ModelToolbarBounds {
                            content,
                            search: toolbar.search.input.rect,
                            language: toolbar.language.rect,
                            refresh: toolbar.refresh.rect,
                            import: toolbar.import.rect,
                        });
                    });
                });
            },
        );
        bounds.expect("toolbar content harness should render every visible control")
    }

    #[test]
    fn models_toolbar_reflows_inside_exact_content_regions() {
        for (width, dark_mode, text_scale) in [
            (45.0, false, 1.0),
            (120.0, true, 1.0),
            (220.0, false, 1.5),
            (220.0, true, 1.5),
            (680.0, false, 1.0),
        ] {
            let bounds = render_model_toolbar_in_content_region(width, dark_mode, text_scale);
            for (name, control) in [
                ("Search models", bounds.search),
                ("Filter model languages", bounds.language),
                ("Refresh trusted model catalog", bounds.refresh),
                ("Import local GGUF", bounds.import),
            ] {
                assert!(
                    bounds.content.contains_rect(control),
                    "{name} must stay inside the provided {width}px content region: content={:?}, control={control:?}",
                    bounds.content,
                );
                assert!(
                    control.width() >= 44.0 && control.height() >= 44.0,
                    "{name} must retain a 44px target at width {width}: {control:?}"
                );
            }
            assert!(
                (bounds.search.top() - bounds.content.top()).abs() < 1.0,
                "search input must retain first-row priority at width {width}"
            );
            match width as i32 {
                680 => {
                    assert!(
                        (bounds.search.top() - bounds.language.top()).abs() < 1.0
                            && bounds.search.left() <= bounds.language.left()
                            && bounds.language.left() <= bounds.refresh.left()
                            && bounds.refresh.right() <= bounds.import.left(),
                        "wide toolbar must keep search and controls adjacent in visual/tab order"
                    );
                }
                220 => {
                    assert!(
                        bounds.search.bottom() <= bounds.language.top()
                            && (bounds.language.top() - bounds.refresh.top()).abs() < 1.0
                            && bounds.language.left() <= bounds.refresh.left()
                            && bounds.refresh.right() <= bounds.import.left(),
                        "220px toolbar must reflow search before compact controls without changing order"
                    );
                }
                120 => {
                    assert!(
                        bounds.search.bottom() <= bounds.language.top()
                            && bounds.language.bottom() <= bounds.refresh.top()
                            && (bounds.refresh.top() - bounds.import.top()).abs() < 1.0
                            && bounds.refresh.right() <= bounds.import.left(),
                        "120px toolbar must stack search and language before adjacent actions"
                    );
                }
                45 => {
                    assert!(
                        bounds.search.bottom() <= bounds.language.top()
                            && bounds.language.bottom() <= bounds.refresh.top()
                            && bounds.refresh.bottom() <= bounds.import.top(),
                        "45px toolbar must defer auxiliary controls into contained rows"
                    );
                }
                _ => unreachable!("test widths are explicit"),
            }
        }
    }

    #[test]
    fn narrow_toolbar_component_tests_are_below_the_real_app_minimum() {
        assert_eq!(crate::MIN_APP_INNER_SIZE, [960.0, 680.0]);
        assert!(
            [45.0, 120.0, 220.0]
                .into_iter()
                .all(|width| width < crate::MIN_APP_INNER_SIZE[0]),
            "sub-44 component widths exercise fallback behavior; the native app cannot expose them as a viewport"
        );
    }

    #[test]
    fn compact_language_filter_preserves_selection_popup_and_keyboard_contracts() {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        crate::ui::controls::configure_accessible_style(&ctx);
        let mut selected = ModelLanguageFilter::All;
        let render = |selected: &mut ModelLanguageFilter, events| {
            let mut response = None;
            let output = ctx.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        Vec2::new(320.0, 320.0),
                    )),
                    events,
                    focused: true,
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        response = Some(compact_model_language_filter_control(ui, selected));
                    });
                },
            );
            (
                output,
                response.expect("compact language filter should render"),
            )
        };
        let (_, language) = render(&mut selected, Vec::new());
        assert_eq!(language.rect.size(), Vec2::splat(44.0));
        let point = language.rect.center();
        let _ = render(&mut selected, vec![primary_pointer_event(point, true)]);
        let (activated, _) = render(&mut selected, vec![primary_pointer_event(point, false)]);
        assert!(
            compact_language_filter_expanded(&activated),
            "pointer activation must announce the expanded state in the activation frame"
        );
        let (open, _) = render(&mut selected, Vec::new());
        let nodes = &open
            .platform_output
            .accesskit_update
            .as_ref()
            .expect("open compact filter should update AccessKit")
            .nodes;
        assert!(nodes.iter().any(|(_, node)| {
            node.role() == egui::accesskit::Role::ComboBox
                && node.name() == Some("Filter model languages")
                && node.is_expanded() == Some(true)
        }));
        let english = nodes
            .iter()
            .find_map(|(_, node)| {
                (node.name() == Some("English"))
                    .then(|| node.bounds())
                    .flatten()
            })
            .expect("open filter should expose its English option");
        assert!(
            nodes
                .iter()
                .any(|(_, node)| node.name() == Some("All languages"))
        );
        assert!(
            nodes
                .iter()
                .any(|(_, node)| node.name() == Some("Multilingual"))
        );
        let english_point = accesskit_rect_center(english);
        let _ = render(
            &mut selected,
            vec![primary_pointer_event(english_point, true)],
        );
        let (selection_frame, trigger_after_selection) = render(
            &mut selected,
            vec![primary_pointer_event(english_point, false)],
        );
        assert_eq!(selected, ModelLanguageFilter::English);
        assert!(
            !compact_language_filter_expanded(&selection_frame),
            "selecting an option must announce the collapsed state in the selection frame"
        );
        assert_eq!(
            ctx.memory(|memory| memory.focused()),
            Some(trigger_after_selection.id),
            "pointer selection must restore focus to the compact filter trigger"
        );
        let (selected_output, _) = render(&mut selected, Vec::new());
        let selected_node = selected_output
            .platform_output
            .accesskit_update
            .as_ref()
            .and_then(|update| {
                update.nodes.iter().find_map(|(_, node)| {
                    (node.role() == egui::accesskit::Role::ComboBox
                        && node.name() == Some("Filter model languages"))
                    .then_some(node)
                })
            })
            .expect("compact filter should retain its accessible name");
        assert_eq!(
            selected_node.description(),
            Some("Current language filter: English")
        );
        selected = ModelLanguageFilter::All;
        let (_, language) = render(&mut selected, Vec::new());
        ctx.memory_mut(|memory| memory.request_focus(language.id));
        let (keyboard_activation, _) = render(
            &mut selected,
            vec![key_press(egui::Key::Enter, egui::Modifiers::NONE)],
        );
        assert!(
            compact_language_filter_expanded(&keyboard_activation),
            "keyboard activation must announce expanded without waiting for another frame"
        );
        // This follows egui's real focus order from the trigger into the
        // popup and then from the current filter to English.
        for _ in 0..3 {
            let _ = render(
                &mut selected,
                vec![key_press(egui::Key::Tab, egui::Modifiers::NONE)],
            );
        }
        let (keyboard_selection, trigger_after_keyboard_selection) = render(
            &mut selected,
            vec![key_press(egui::Key::Enter, egui::Modifiers::NONE)],
        );
        assert_eq!(selected, ModelLanguageFilter::English);
        assert!(
            !compact_language_filter_expanded(&keyboard_selection),
            "keyboard selection must announce collapsed in the selection frame"
        );
        assert_eq!(
            ctx.memory(|memory| memory.focused()),
            Some(trigger_after_keyboard_selection.id),
            "keyboard selection must restore trigger focus"
        );
        let _ = render(
            &mut selected,
            vec![key_press(egui::Key::Enter, egui::Modifiers::NONE)],
        );
        let (escape_frame, trigger_after_escape) = render(
            &mut selected,
            vec![key_press(egui::Key::Escape, egui::Modifiers::NONE)],
        );
        assert!(
            !compact_language_filter_expanded(&escape_frame),
            "Escape must announce collapsed in the closing frame"
        );
        assert_eq!(
            ctx.memory(|memory| memory.focused()),
            Some(trigger_after_escape.id),
            "Escape must restore focus to the compact filter trigger"
        );
        let (closed, _) = render(&mut selected, Vec::new());
        assert!(
            closed
                .platform_output
                .accesskit_update
                .as_ref()
                .is_some_and(|update| update.nodes.iter().any(|(_, node)| {
                    node.role() == egui::accesskit::Role::ComboBox
                        && node.name() == Some("Filter model languages")
                        && node.is_expanded() == Some(false)
                }))
        );
    }

    #[test]
    fn compact_language_filter_restores_or_preserves_focus_after_popup_close() {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        crate::ui::controls::configure_accessible_style(&ctx);
        let mut selected = ModelLanguageFilter::All;
        let previous_text = std::cell::RefCell::new(String::new());
        let next_text = std::cell::RefCell::new(String::new());
        let render = |selected: &mut ModelLanguageFilter, events| {
            let mut previous = None;
            let mut trigger = None;
            let mut next = None;
            let output = ctx.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        Vec2::new(400.0, 400.0),
                    )),
                    events,
                    focused: true,
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        ui.horizontal(|ui| {
                            previous = Some({
                                let mut text = previous_text.borrow_mut();
                                ui.add(
                                    egui::TextEdit::singleline(&mut *text)
                                        .hint_text("Previous focus target"),
                                )
                            });
                            trigger = Some(compact_model_language_filter_control(ui, selected));
                            next = Some({
                                let mut text = next_text.borrow_mut();
                                ui.add(
                                    egui::TextEdit::singleline(&mut *text)
                                        .hint_text("Next focus target"),
                                )
                            });
                        });
                    });
                },
            );
            (
                output,
                previous.expect("previous focus target should render"),
                trigger.expect("compact filter should render"),
                next.expect("next focus target should render"),
            )
        };
        let (_, previous, trigger, _next) = render(&mut selected, Vec::new());
        let open_popup = |selected: &mut ModelLanguageFilter| {
            let _ = render(
                selected,
                vec![primary_pointer_event(trigger.rect.center(), true)],
            );
            render(
                selected,
                vec![primary_pointer_event(trigger.rect.center(), false)],
            )
        };
        let (opened, _, _trigger, next) = open_popup(&mut selected);
        assert!(compact_language_filter_expanded(&opened));
        let _ = render(
            &mut selected,
            vec![primary_pointer_event(next.rect.center(), true)],
        );
        let (outside_destination, _, _trigger, next) = render(
            &mut selected,
            vec![primary_pointer_event(next.rect.center(), false)],
        );
        assert!(!compact_language_filter_expanded(&outside_destination));
        let _ = render(&mut selected, Vec::new());
        assert_eq!(
            ctx.memory(|memory| memory.focused()),
            Some(next.id),
            "outside click must preserve a real focusable destination"
        );

        let (reopened, _, _trigger, _) = open_popup(&mut selected);
        assert!(compact_language_filter_expanded(&reopened));
        let blank = egui::pos2(360.0, 360.0);
        let _ = render(&mut selected, vec![primary_pointer_event(blank, true)]);
        let (outside_blank, _, trigger, _) =
            render(&mut selected, vec![primary_pointer_event(blank, false)]);
        assert!(!compact_language_filter_expanded(&outside_blank));
        let _ = render(&mut selected, Vec::new());
        assert_eq!(
            ctx.memory(|memory| memory.focused()),
            Some(trigger.id),
            "outside click without a focus destination must restore the trigger"
        );

        let _ = render(
            &mut selected,
            vec![key_press(egui::Key::Tab, egui::Modifiers::NONE)],
        );
        assert_eq!(
            ctx.memory(|memory| memory.focused()),
            Some(next.id),
            "Tab should continue from the restored trigger to the next control"
        );
        ctx.memory_mut(|memory| memory.request_focus(trigger.id));
        let _ = render(
            &mut selected,
            vec![key_press(egui::Key::Tab, egui::Modifiers::SHIFT)],
        );
        // egui schedules backwards tab traversal for the next frame so the
        // destination can receive its gained-focus state.
        let _ = render(&mut selected, Vec::new());
        assert_eq!(
            ctx.memory(|memory| memory.focused()),
            Some(previous.id),
            "Shift+Tab should continue from the trigger to the previous control"
        );
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
        let stable_model = ModelViewModel {
            id: "stable".into(),
            installed: true,
            download_state: ModelDownloadState::Installed,
            removal_supported: true,
            ..Default::default()
        };
        let stable_controls = model_lifecycle_controls(ModelCard::Local(&stable_model), true);
        assert_eq!(
            stable_controls
                .primary
                .as_ref()
                .expect("normal installed models keep a lifecycle control")
                .label,
            "Delete"
        );

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
    fn bundled_model_hides_settled_lifecycle_but_retains_repair_path() {
        let included = ModelViewModel {
            id: BUNDLED_BASE_MODEL_ID.into(),
            display_name: "Whisper Base — English".into(),
            bundled: true,
            included: true,
            installed: true,
            ready: true,
            removal_supported: false,
            download_state: ModelDownloadState::Installed,
            ..Default::default()
        };
        let presentation = model_lifecycle_presentation(ModelCard::Local(&included), true);
        assert_eq!(presentation.action, ScreenAction::None);
        assert_eq!(presentation.label, "Installed");
        assert_eq!(presentation.icon, Icon::CheckCircle);
        assert!(!presentation.enabled);
        assert_eq!(
            presentation.disabled_reason,
            Some("This verified model is installed with Scribe and cannot be removed.")
        );
        assert!(
            model_lifecycle_controls(ModelCard::Local(&included), true)
                .primary
                .is_none(),
            "settled bundled models must not expose a redundant Installed lifecycle control"
        );

        let repair = ModelViewModel {
            id: BUNDLED_BASE_MODEL_ID.into(),
            display_name: "Whisper Base — English".into(),
            bundled: true,
            install_supported: true,
            install_action_enabled: true,
            download_state: ModelDownloadState::Failed,
            primary_action_disabled_reason: Some(
                "Repair downloads the exact pinned model after you choose it.".into(),
            ),
            ..Default::default()
        };
        let presentation = model_lifecycle_presentation(ModelCard::Local(&repair), true);
        assert_eq!(
            presentation.action,
            ScreenAction::InstallModel(BUNDLED_BASE_MODEL_ID.into())
        );
        assert_eq!(presentation.label, "Repair");
        assert!(presentation.enabled);
        assert_eq!(
            model_lifecycle_controls(ModelCard::Local(&repair), true)
                .primary
                .as_ref()
                .expect("corrupt bundled models keep the Repair action")
                .label,
            "Repair"
        );
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
                display_text: "120B / 100B".into(),
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
                display_text: "42B / Total unknown".into(),
                accessible_text: "Downloading 42B; total download size unknown".into(),
            })
        );
        let not_downloading = ModelViewModel::default();
        assert_eq!(
            model_download_progress_presentation(ModelCard::Local(&not_downloading)),
            None
        );
    }

    #[test]
    fn download_label_slot_uses_the_actual_byte_label_font_and_maximum_value() {
        let ctx = egui::Context::default();
        let _ = ctx.run(Default::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let total = 1_500_000_000;
                let expected = format!(
                    "{} / {}",
                    format_download_bytes(total),
                    format_download_bytes(total)
                );
                let expected_width = ui.fonts(|fonts| {
                    fonts
                        .layout_no_wrap(
                            expected,
                            egui::TextStyle::Small.resolve(ui.style()),
                            ui_palette(ui).muted_text,
                        )
                        .rect
                        .width()
                });
                assert_eq!(
                    download_label_slot_width(ui, total),
                    expected_width.ceil(),
                    "the reserved label slot must be measured using the rendered byte-label style"
                );
            });
        });
    }

    #[test]
    fn settled_download_without_partial_keeps_install_primary_and_never_uses_play_or_warning() {
        for download_state in [ModelDownloadState::Failed, ModelDownloadState::Cancelled] {
            let model = ModelViewModel {
                display_name: "Settled local model".into(),
                download_state,
                partial_cleanup_available: false,
                ..Default::default()
            };
            let primary = model_lifecycle_presentation(ModelCard::Local(&model), true);
            assert_eq!(primary.label, "Install");
            assert_eq!(primary.icon, Icon::Download);
            assert_eq!(primary.tone, ModelLifecycleTone::InverseFilled);
        }
    }

    #[test]
    fn download_card_desktop_zones_center_progress_with_track_row_controls() {
        let model = ModelViewModel {
            id: "geometry-download".into(),
            display_name: "Geometry download".into(),
            download_state: ModelDownloadState::Downloading,
            downloaded_bytes: 82_000_000,
            total_bytes: Some(100_000_000),
            cancel_supported: true,
            ..Default::default()
        };
        for (width, height) in [(1180.0, 815.0), (960.0, 680.0)] {
            let output = render_model_card_at(&model, width, height, Vec::new());
            let lifecycle = model_layout_bounds(&output, "Geometry download layout lifecycle zone");
            let body = model_layout_bounds(&output, "Geometry download layout lifecycle body");
            let rail = model_layout_bounds(&output, "Geometry download layout chevron zone");
            let progress_name = "Downloading 82.0MB of 100.0MB, 82% complete";
            let label =
                model_layout_bounds(&output, &format!("{progress_name} layout download label"));
            let track =
                model_layout_bounds(&output, &format!("{progress_name} layout download track"));
            let pause = named_role_bounds(
                &output,
                "Pause Geometry download download",
                egui::accesskit::Role::Button,
            );
            let discard = named_role_bounds(
                &output,
                "Discard partial for Geometry download",
                egui::accesskit::Role::Button,
            );
            assert_eq!(lifecycle.height(), f64::from(MODEL_CARD_SUMMARY_HEIGHT));
            assert_eq!(body.height(), f64::from(MODEL_CARD_SUMMARY_HEIGHT));
            assert_eq!(rail.width(), 44.0);
            assert_eq!(rail.height(), 44.0);
            assert!((rail.y0 + rail.y1 - lifecycle.y0 - lifecycle.y1).abs() < 0.1);
            assert!(
                label.y1 <= track.y0,
                "the stable byte label must stay above the track: label={label:?} track={track:?}"
            );
            assert!(
                label.y1 <= pause.y0 && label.y1 <= discard.y0,
                "the stable byte label must stay above its controls: label={label:?} pause={pause:?} discard={discard:?}"
            );
            assert!(
                track.y0 < pause.y1 && pause.y0 < track.y1,
                "Pause must share the desktop track row: track={track:?} pause={pause:?}"
            );
            assert!(
                track.y0 < discard.y1 && discard.y0 < track.y1,
                "Discard must share the desktop track row: track={track:?} discard={discard:?}"
            );
            let module_bottom = track.y1.max(pause.y1).max(discard.y1);
            let module_center = (label.y0 + module_bottom) / 2.0;
            let lifecycle_center = (lifecycle.y0 + lifecycle.y1) / 2.0;
            assert!(
                (module_center - lifecycle_center).abs() <= 1.0,
                "the complete progress module must be vertically centered: module={module_center} lifecycle={lifecycle_center}"
            );
            assert!((track.x0 - body.x0).abs() < 0.1);
            assert!(
                track.x1 <= pause.x0 + 0.1,
                "track={track:?} pause={pause:?}"
            );
            assert!(
                pause.x1 <= discard.x0 + 0.1,
                "pause={pause:?} discard={discard:?}"
            );
        }
    }

    #[test]
    fn narrow_download_module_wraps_controls_below_the_track_only_when_needed() {
        let progress = ModelDownloadProgressPresentation {
            downloaded_bytes: 42,
            total_bytes: Some(100),
            fraction: Some(0.42),
            total_is_unknown: false,
            display_text: "42B / 100B".into(),
            accessible_text: "Downloading 42B of 100B, 42% complete".into(),
        };
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let output = ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    Vec2::new(180.0, 120.0),
                )),
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    ui.allocate_ui_with_layout(
                        Vec2::new(120.0, 0.0),
                        Layout::top_down(Align::Min),
                        |ui| {
                            ui.set_width(120.0);
                            ui.set_max_width(120.0);
                            render_model_download_module(
                                ui,
                                &progress,
                                Icon::Pause,
                                "Pause Narrow download",
                                true,
                                None,
                                Some("Discard partial for Narrow download"),
                            );
                        },
                    );
                });
            },
        );
        let label = model_layout_bounds(
            &output,
            "Downloading 42B of 100B, 42% complete layout download label",
        );
        let track = model_layout_bounds(
            &output,
            "Downloading 42B of 100B, 42% complete layout download track",
        );
        let pause = named_role_bounds(
            &output,
            "Pause Narrow download",
            egui::accesskit::Role::Button,
        );
        let close = named_role_bounds(
            &output,
            "Discard partial for Narrow download",
            egui::accesskit::Role::Button,
        );
        assert!(label.y1 <= track.y0, "label={label:?} track={track:?}");
        assert!(pause.y0 >= track.y1, "pause={pause:?} track={track:?}");
        assert!(close.y0 >= track.y1, "close={close:?} track={track:?}");
        assert!(track.width() > 0.0);
        assert!(pause.width() >= 44.0 && pause.height() >= 44.0);
        assert!(close.width() >= 44.0 && close.height() >= 44.0);
    }

    #[test]
    fn compact_download_card_keeps_track_and_controls_on_one_row_at_375px() {
        let model = ModelViewModel {
            id: "compact-download".into(),
            display_name: "Compact download".into(),
            download_state: ModelDownloadState::Downloading,
            downloaded_bytes: 42,
            total_bytes: Some(100),
            cancel_supported: true,
            ..Default::default()
        };
        let output = render_model_card_at(&model, 375.0, 680.0, Vec::new());
        let progress_name = "Downloading 42B of 100B, 42% complete";
        let label = model_layout_bounds(&output, &format!("{progress_name} layout download label"));
        let track = model_layout_bounds(&output, &format!("{progress_name} layout download track"));
        let pause = named_role_bounds(
            &output,
            "Pause Compact download download",
            egui::accesskit::Role::Button,
        );
        let discard = named_role_bounds(
            &output,
            "Discard partial for Compact download",
            egui::accesskit::Role::Button,
        );
        assert!(label.y1 <= track.y0, "label={label:?} track={track:?}");
        assert!(
            track.y0 < pause.y1 && pause.y0 < track.y1,
            "pause={pause:?} track={track:?}"
        );
        assert!(
            track.y0 < discard.y1 && discard.y0 < track.y1,
            "discard={discard:?} track={track:?}"
        );
        assert!(
            track.x1 <= pause.x0 + 0.1,
            "track={track:?} pause={pause:?}"
        );
        assert!(
            pause.x1 <= discard.x0 + 0.1,
            "pause={pause:?} discard={discard:?}"
        );
    }

    #[test]
    fn byte_label_slot_keeps_progress_controls_fixed_for_equal_totals() {
        for (width, height) in [(1180.0, 815.0), (960.0, 680.0), (375.0, 680.0)] {
            let base = ModelViewModel {
                id: "stable-byte-slot".into(),
                display_name: "Stable byte slot".into(),
                download_state: ModelDownloadState::Downloading,
                total_bytes: Some(100_000_000),
                cancel_supported: true,
                ..Default::default()
            };
            let early = ModelViewModel {
                downloaded_bytes: 0,
                ..base.clone()
            };
            let late = ModelViewModel {
                downloaded_bytes: 99_000_000,
                ..base
            };
            let early_progress = model_download_progress_presentation(ModelCard::Local(&early))
                .expect("early progress");
            let late_progress = model_download_progress_presentation(ModelCard::Local(&late))
                .expect("late progress");
            let early_output = render_model_card_at(&early, width, height, Vec::new());
            let late_output = render_model_card_at(&late, width, height, Vec::new());
            let layout = |output: &egui::FullOutput,
                          progress: &ModelDownloadProgressPresentation| {
                (
                    model_layout_bounds(
                        output,
                        &format!("{} layout download label", progress.accessible_text),
                    ),
                    model_layout_bounds(
                        output,
                        &format!("{} layout download track", progress.accessible_text),
                    ),
                    named_role_bounds(
                        output,
                        "Pause Stable byte slot download",
                        egui::accesskit::Role::Button,
                    ),
                    named_role_bounds(
                        output,
                        "Discard partial for Stable byte slot",
                        egui::accesskit::Role::Button,
                    ),
                )
            };
            let early_layout = layout(&early_output, &early_progress);
            let late_layout = layout(&late_output, &late_progress);
            assert_eq!(early_layout.0, late_layout.0, "label slot at {width}px");
            assert_eq!(early_layout.1, late_layout.1, "track slot at {width}px");
            assert_eq!(early_layout.2, late_layout.2, "Pause slot at {width}px");
            assert_eq!(early_layout.3, late_layout.3, "Discard slot at {width}px");
        }
    }

    #[test]
    fn failed_download_warning_is_a_named_44px_button_with_the_actual_error() {
        let model = ModelViewModel {
            id: "failed-warning".into(),
            display_name: "Failed warning".into(),
            download_state: ModelDownloadState::Failed,
            error_message: Some("TLS certificate validation failed.".into()),
            ..Default::default()
        };
        let output = render_model_card_at(&model, 960.0, 680.0, Vec::new());
        let warning = output
            .platform_output
            .accesskit_update
            .as_ref()
            .and_then(|update| {
                update.nodes.iter().find_map(|(_, node)| {
                    (node.role() == egui::accesskit::Role::Button
                        && node.name() == Some("Show download error for Failed warning"))
                    .then_some(node)
                })
            })
            .expect("download error warning control");
        let bounds = warning.bounds().expect("warning bounds");
        assert!(bounds.width() >= 44.0 && bounds.height() >= 44.0);
        assert_eq!(
            warning.description(),
            Some("TLS certificate validation failed.")
        );
    }

    #[test]
    fn remote_download_error_is_preserved_for_the_warning_alert() {
        let entry = RemoteCatalogEntryView {
            display_name: "Remote failure".into(),
            ..Default::default()
        };
        let variant = RemoteCatalogVariantView {
            error_message: Some("Pinned artifact could not be fetched.".into()),
            ..Default::default()
        };
        assert_eq!(
            model_download_error(ModelCard::Remote(&entry, &variant)),
            Some("Pinned artifact could not be fetched.")
        );
    }

    #[test]
    fn local_retained_partial_bytes_project_after_failed_and_cancelled_restarts() {
        for state in [ModelDownloadState::Failed, ModelDownloadState::Cancelled] {
            let model = ModelViewModel {
                download_state: state,
                partial_cleanup_available: true,
                downloaded_bytes: 82_000_000,
                total_bytes: Some(100_000_000),
                ..Default::default()
            };
            let progress = model_download_progress_presentation(ModelCard::Local(&model))
                .expect("retained local partial projects after restart");
            assert_eq!(progress.downloaded_bytes, 82_000_000);
            assert_eq!(progress.total_bytes, Some(100_000_000));
            assert_eq!(progress.display_text, "82.0MB / 100.0MB");
        }
    }

    #[test]
    fn remote_retained_partial_bytes_do_not_create_inactive_download_meters() {
        for status_label in ["Failed", "Cancelled"] {
            let entry = RemoteCatalogEntryView {
                display_name: "Remote retained partial".into(),
                ..Default::default()
            };
            let variant = RemoteCatalogVariantView {
                status_label: Some(status_label.into()),
                downloaded_bytes: Some(82_000_000),
                total_bytes: Some(100_000_000),
                ..Default::default()
            };
            assert_eq!(
                model_download_progress_presentation(ModelCard::Remote(&entry, &variant)),
                None,
                "{status_label} is not an active transfer"
            );
        }
    }

    #[test]
    fn download_card_hides_lifecycle_and_percent_text_from_the_visible_surface() {
        let model = ModelViewModel {
            id: "quiet-download".into(),
            display_name: "Quiet download".into(),
            download_state: ModelDownloadState::Downloading,
            downloaded_bytes: 42,
            total_bytes: Some(100),
            cancel_supported: true,
            ..Default::default()
        };
        let output = render_model_card_at(&model, 960.0, 680.0, Vec::new());
        let visible_text = output
            .shapes
            .iter()
            .filter_map(|shape| match &shape.shape {
                egui::epaint::Shape::Text(text) => Some(text.galley.text()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert!(!visible_text.iter().any(|text| text.contains("Downloading")));
        assert!(!visible_text.iter().any(|text| text.contains('%')));
        assert!(visible_text.iter().any(|text| text.contains("42B / 100B")));
    }

    #[test]
    fn download_warning_click_is_excluded_from_card_activation_and_escape_or_outside_click_dismisses()
     {
        let model = failed_warning_model();
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        crate::ui::controls::configure_accessible_style(&ctx);
        let initial = render_model_card_with_context(&ctx, &model, Vec::new());
        let warning = named_role_bounds(
            &initial.0,
            "Show download error for Failed warning",
            egui::accesskit::Role::Button,
        );
        let point = accesskit_rect_center(warning);
        let pressed = render_model_card_with_context(
            &ctx,
            &model,
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
        assert_eq!(pressed.1, ScreenAction::None);
        let opened = render_model_card_with_context(
            &ctx,
            &model,
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
        assert_eq!(opened.1, ScreenAction::None);
        assert!(button_expanded(
            &opened.0,
            "Show download error for Failed warning"
        ));
        let still_pinned = render_model_card_with_context(
            &ctx,
            &model,
            vec![egui::Event::PointerMoved(egui::pos2(900.0, 600.0))],
        );
        assert!(button_expanded(
            &still_pinned.0,
            "Show download error for Failed warning"
        ));
        let dismissed = render_model_card_with_context(
            &ctx,
            &model,
            vec![egui::Event::Key {
                key: egui::Key::Escape,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
        );
        assert!(!button_expanded(
            &dismissed.0,
            "Show download error for Failed warning"
        ));

        let reopened = click_model_warning(&ctx, &model, point);
        assert!(button_expanded(
            &reopened.0,
            "Show download error for Failed warning"
        ));
        let _outside_press = render_model_card_with_context(
            &ctx,
            &model,
            vec![egui::Event::PointerButton {
                pos: egui::pos2(900.0, 600.0),
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            }],
        );
        let outside = render_model_card_with_context(
            &ctx,
            &model,
            vec![egui::Event::PointerButton {
                pos: egui::pos2(900.0, 600.0),
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            }],
        );
        assert!(!button_expanded(
            &outside.0,
            "Show download error for Failed warning"
        ));
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
            filename: "stable-q5.gguf".into(),
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
        assert_eq!(
            remote.accessible_name,
            "Install Remote stable (stable-q5.gguf)"
        );
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
    fn expanded_remote_card_keeps_install_provenance_visible_and_accessible() {
        fn collect_text(shape: &egui::epaint::Shape, text: &mut Vec<String>) {
            match shape {
                egui::epaint::Shape::Text(text_shape) => {
                    text.push(text_shape.galley.text().to_owned())
                }
                egui::epaint::Shape::Vec(shapes) => {
                    for shape in shapes {
                        collect_text(shape, text);
                    }
                }
                _ => {}
            }
        }

        let entry = RemoteCatalogEntryView {
            id: "trusted/compact".into(),
            display_name: "Compact English".into(),
            trust_label: "Trusted publisher".into(),
            repository: "trusted/compact".into(),
            pinned_revision: "1111111111111111111111111111111111111111".into(),
            compatibility_detail: "Validated for this device.".into(),
            ..Default::default()
        };
        let variant = RemoteCatalogVariantView {
            id: "compact-q5".into(),
            filename: "compact-q5.gguf".into(),
            ..Default::default()
        };
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        crate::ui::controls::configure_accessible_style(&ctx);
        let output = ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(960.0, 900.0),
                )),
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    render_unified_model_card(
                        ui,
                        ModelCard::Remote(&entry, &variant),
                        true,
                        false,
                        false,
                    );
                });
            },
        );
        let expected = [
            "Trust: Trusted publisher",
            "Repository: trusted/compact",
            "Compatibility: Validated for this device.",
            "Pinned revision: 1111111111111111111111111111111111111111",
            "Artifact: compact-q5.gguf",
        ];
        let mut painted = Vec::new();
        for shape in &output.shapes {
            collect_text(&shape.shape, &mut painted);
        }
        for value in expected {
            assert!(
                painted.iter().any(|text| text == value),
                "missing visible provenance: {value}; painted={painted:?}"
            );
        }
        let nodes = &output.platform_output.accesskit_update.unwrap().nodes;
        for value in [
            "Trust: Trusted publisher",
            "Repository: trusted/compact",
            "Compatibility: Validated for this device.",
            "Pinned revision: 1111111111111111111111111111111111111111",
            "Artifact: compact-q5.gguf",
        ] {
            assert!(
                nodes.iter().any(|(_, node)| node.name() == Some(value)),
                "missing accessible provenance: {value}"
            );
        }
    }

    #[test]
    fn collapsed_actionable_remote_card_shows_complete_install_provenance() {
        fn collect_text(shape: &egui::epaint::Shape, text: &mut Vec<String>) {
            match shape {
                egui::epaint::Shape::Text(text_shape) => {
                    text.push(text_shape.galley.text().to_owned())
                }
                egui::epaint::Shape::Vec(shapes) => {
                    for shape in shapes {
                        collect_text(shape, text);
                    }
                }
                _ => {}
            }
        }

        let entry = RemoteCatalogEntryView {
            id: "trusted/actionable".into(),
            display_name: "Actionable English".into(),
            trust_label: "Trusted publisher".into(),
            repository: "trusted/actionable".into(),
            pinned_revision: "2222222222222222222222222222222222222222".into(),
            compatibility_detail: "Validated for this device.".into(),
            ..Default::default()
        };
        let variant = RemoteCatalogVariantView {
            id: "actionable-q5".into(),
            filename: "actionable-q5.gguf".into(),
            actions: vec![RemoteCatalogActionView {
                label: "Install".into(),
                kind: RemoteCatalogActionKind::Install {
                    remote_model_id: entry.id.clone(),
                    variant_id: "actionable-q5".into(),
                },
                enabled: true,
                disabled_reason: None,
            }],
            ..Default::default()
        };
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        crate::ui::controls::configure_accessible_style(&ctx);
        let output = ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(960.0, 900.0),
                )),
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    render_unified_model_card(
                        ui,
                        ModelCard::Remote(&entry, &variant),
                        false,
                        false,
                        false,
                    );
                });
            },
        );
        let provenance = [
            "Trust: Trusted publisher",
            "Repository: trusted/actionable",
            "Compatibility: Validated for this device.",
            "Pinned revision: 2222222222222222222222222222222222222222",
            "Artifact: actionable-q5.gguf",
        ];
        let mut painted = Vec::new();
        for shape in &output.shapes {
            collect_text(&shape.shape, &mut painted);
        }
        for value in provenance {
            assert!(
                painted.iter().any(|text| text == value),
                "missing collapsed visible provenance: {value}; painted={painted:?}"
            );
        }
        let nodes = &output.platform_output.accesskit_update.unwrap().nodes;
        assert!(nodes.iter().any(|(_, node)| {
            node.name() == Some("Install Actionable English (actionable-q5.gguf)")
                && !node.is_disabled()
        }));
        for value in provenance {
            assert!(
                nodes.iter().any(|(_, node)| node.name() == Some(value)),
                "missing collapsed accessible provenance: {value}"
            );
        }
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

    fn render_selector_with_key(
        ctx: &egui::Context,
        state: &TranscriptionState,
        models: &[ModelViewModel],
        focus_id: egui::Id,
        key: egui::Key,
        picker_open: bool,
    ) -> (egui::FullOutput, ScreenAction) {
        let mut action = ScreenAction::None;
        let output = ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    Vec2::new(900.0, 300.0),
                )),
                focused: true,
                events: vec![egui::Event::Key {
                    key,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                }],
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    if picker_open {
                        ui.memory_mut(|memory| {
                            memory.open_popup(egui::Id::new("quick-model-picker"));
                        });
                    }
                    ctx.memory_mut(|memory| memory.request_focus(focus_id));
                    action = selector_row(ui, state, models, models);
                });
            },
        );
        (output, action)
    }

    fn render_selector_with_events(
        ctx: &egui::Context,
        state: &TranscriptionState,
        models: &[ModelViewModel],
        width: f32,
        events: Vec<egui::Event>,
    ) -> (egui::FullOutput, ScreenAction) {
        let mut action = ScreenAction::None;
        let output = ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    Vec2::new(width + 16.0, 320.0),
                )),
                focused: true,
                events,
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    ui.allocate_ui_with_layout(
                        Vec2::new(width, 0.0),
                        Layout::top_down(Align::LEFT),
                        |ui| action = selector_row(ui, state, models, models),
                    );
                });
            },
        );
        (output, action)
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
        for name in ["2 MINS AGO", "BASE.EN"] {
            assert!(
                nodes.iter().any(|(_, node)| node.name() == Some(name)),
                "missing visible Transcribe label {name}"
            );
        }
        let hotkey = nodes
            .iter()
            .find_map(|(_, node)| {
                (node.role() == egui::accesskit::Role::Button
                    && node.name() == Some("Change recording shortcut"))
                .then_some(node)
            })
            .expect("recording shortcut button");
        let bounds = hotkey.bounds().expect("recording shortcut bounds");
        assert!(bounds.width() >= 44.0 && bounds.height() >= 44.0);
        assert!(nodes.iter().any(|(_, node)| {
            node.role() == egui::accesskit::Role::Button
                && node.name() == Some("Choose active model: whisper.cpp base.en")
        }));
        assert!(
            !nodes.iter().any(|(_, node)| {
                node.role() == egui::accesskit::Role::Button
                    && matches!(node.name(), Some("Change") | Some("Select"))
            }),
            "the card must be the only model chooser, without a nested Change target"
        );
    }

    #[test]
    fn quick_controls_support_enter_space_current_checkmark_and_ready_models_only() {
        let state = TranscriptionState {
            phase: TranscriptionPhase::Ready,
            selected_model_id: Some("base.en".into()),
            hotkey: "Ctrl+Space".into(),
            ..Default::default()
        };
        let models = vec![
            ModelViewModel {
                id: "base.en".into(),
                display_name: "Whisper Base".into(),
                installed: true,
                ready: true,
                ..Default::default()
            },
            ModelViewModel {
                id: "tiny.en".into(),
                display_name: "Whisper Tiny".into(),
                installed: true,
                ready: true,
                ..Default::default()
            },
            ModelViewModel {
                id: "broken.en".into(),
                display_name: "Broken model".into(),
                installed: true,
                ready: false,
                ..Default::default()
            },
        ];

        for key in [egui::Key::Enter, egui::Key::Space] {
            let ctx = egui::Context::default();
            ctx.enable_accesskit();
            crate::ui::controls::configure_accessible_style(&ctx);
            let _ = render_selector_with_key(
                &ctx,
                &state,
                &models,
                egui::Id::new("recording-hotkey-action"),
                egui::Key::Escape,
                false,
            );
            let (_, action) = render_selector_with_key(
                &ctx,
                &state,
                &models,
                egui::Id::new("recording-hotkey-action"),
                key,
                false,
            );
            assert_eq!(action, ScreenAction::StartHotkeyCapture);

            let capturing = TranscriptionState {
                hotkey_capture_active: true,
                ..state.clone()
            };
            let _ = render_selector_with_key(
                &ctx,
                &capturing,
                &models,
                egui::Id::new("recording-hotkey-action"),
                egui::Key::Escape,
                false,
            );
            let (_, action) = render_selector_with_key(
                &ctx,
                &capturing,
                &models,
                egui::Id::new("recording-hotkey-action"),
                key,
                false,
            );
            assert_eq!(action, ScreenAction::CancelHotkeyCapture);

            let _ = render_selector_with_key(
                &ctx,
                &state,
                &models,
                egui::Id::new("selected-model-action"),
                egui::Key::Escape,
                false,
            );
            let (picker, action) = render_selector_with_key(
                &ctx,
                &state,
                &models,
                egui::Id::new("selected-model-action"),
                key,
                false,
            );
            assert_eq!(action, ScreenAction::None);
            let nodes = picker.platform_output.accesskit_update.unwrap().nodes;
            let change = nodes
                .iter()
                .find_map(|(_, node)| {
                    (node.name() == Some("Choose active model: Whisper Base")).then_some(node)
                })
                .expect("model card action");
            let bounds = change.bounds().expect("Change bounds");
            assert!(bounds.width() >= 44.0 && bounds.height() >= 44.0);
            assert_eq!(change.is_expanded(), Some(true));

            let (closed, action) = render_selector_with_key(
                &ctx,
                &state,
                &models,
                egui::Id::new("selected-model-action"),
                egui::Key::Escape,
                false,
            );
            assert_eq!(action, ScreenAction::None);
            let closed_change = closed
                .platform_output
                .accesskit_update
                .unwrap()
                .nodes
                .into_iter()
                .find_map(|(_, node)| {
                    (node.name() == Some("Choose active model: Whisper Base")).then_some(node)
                })
                .expect("closed model card action");
            assert_eq!(closed_change.is_expanded(), Some(false));
            assert_eq!(
                ctx.memory(|memory| memory.focused()),
                Some(egui::Id::new("selected-model-action"))
            );

            let _ = render_selector_with_key(
                &ctx,
                &state,
                &models,
                egui::Id::new("quick-model-picker").with(("option", "tiny.en")),
                egui::Key::Escape,
                true,
            );
            let (picker, action) = render_selector_with_key(
                &ctx,
                &state,
                &models,
                egui::Id::new("quick-model-picker").with(("option", "tiny.en")),
                key,
                true,
            );
            assert_eq!(action, ScreenAction::SelectQuickModel("tiny.en".into()));
            let nodes = picker.platform_output.accesskit_update.unwrap().nodes;
            assert!(nodes.iter().any(|(_, node)| {
                node.name() == Some("Whisper Base, current model")
                    && node.is_selected() == Some(true)
            }));
            assert!(nodes.iter().any(|(_, node)| {
                node.name() == Some("Select Whisper Tiny") && node.is_selected() == Some(false)
            }));
            assert!(
                !nodes
                    .iter()
                    .any(|(_, node)| node.name() == Some("Broken model"))
            );
        }
    }

    #[test]
    fn disabled_transcribe_model_card_closes_an_open_picker_and_ignores_keyboard_activation() {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        crate::ui::controls::configure_accessible_style(&ctx);
        let reason = "Wait for the current operation before changing models.";
        let state = TranscriptionState {
            phase: TranscriptionPhase::Ready,
            selected_model_id: Some("base.en".into()),
            hotkey: "Ctrl+Space".into(),
            model_change_disabled_reason: Some(reason.into()),
            ..Default::default()
        };
        let models = vec![
            ModelViewModel {
                id: "base.en".into(),
                display_name: "Whisper Base".into(),
                installed: true,
                ready: true,
                ..Default::default()
            },
            ModelViewModel {
                id: "tiny.en".into(),
                display_name: "Whisper Tiny".into(),
                installed: true,
                ready: true,
                ..Default::default()
            },
        ];
        let popup_id = egui::Id::new("quick-model-picker");
        let (output, action) = render_selector_with_key(
            &ctx,
            &state,
            &models,
            popup_id.with(("option", "tiny.en")),
            egui::Key::Enter,
            true,
        );
        assert_eq!(action, ScreenAction::None);
        assert!(!ctx.memory(|memory| memory.is_popup_open(popup_id)));
        assert_eq!(
            ctx.memory(|memory| memory.focused()),
            Some(egui::Id::new("selected-model-action"))
        );
        let update = output.platform_output.accesskit_update.unwrap();
        let change = update
            .nodes
            .iter()
            .find_map(|(_, node)| {
                (node.role() == egui::accesskit::Role::Button
                    && node.name() == Some("Choose active model: Whisper Base"))
                .then_some(node)
            })
            .expect("disabled model card action");
        assert!(change.is_disabled());
        assert_eq!(change.description(), Some(reason));
        assert_eq!(change.is_expanded(), Some(false));
        assert!(
            !update
                .nodes
                .iter()
                .any(|(_, node)| node.name() == Some("Select Whisper Tiny"))
        );
    }

    #[test]
    fn quick_model_picker_manage_models_surrenders_card_focus() {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        crate::ui::controls::configure_accessible_style(&ctx);
        let state = TranscriptionState {
            phase: TranscriptionPhase::Ready,
            selected_model_id: Some("base.en".into()),
            hotkey: "Ctrl+Space".into(),
            ..Default::default()
        };
        let models = vec![ModelViewModel {
            id: "base.en".into(),
            display_name: "Whisper Base".into(),
            installed: true,
            ready: true,
            ..Default::default()
        }];
        let picker_id = egui::Id::new("quick-model-picker");
        let card_id = egui::Id::new("selected-model-action");
        ctx.memory_mut(|memory| {
            memory.open_popup(picker_id);
            memory.request_focus(card_id);
        });
        let (opened, _) = render_selector_with_events(&ctx, &state, &models, 900.0, Vec::new());
        let manage_id = opened
            .platform_output
            .accesskit_update
            .unwrap()
            .nodes
            .into_iter()
            .find_map(|(id, node)| {
                node.name()
                    .is_some_and(|name| name.starts_with("Manage models"))
                    .then_some(id)
            })
            .expect("Manage models action");
        let (_, action) = render_selector_with_events(
            &ctx,
            &state,
            &models,
            900.0,
            vec![egui::Event::AccessKitActionRequest(
                egui::accesskit::ActionRequest {
                    action: egui::accesskit::Action::Default,
                    target: manage_id,
                    data: None,
                },
            )],
        );
        assert_eq!(action, ScreenAction::OpenModelSettings);
        assert!(!ctx.memory(|memory| memory.is_popup_open(picker_id)));
        assert_ne!(ctx.memory(|memory| memory.focused()), Some(card_id));
    }

    #[test]
    fn disabled_focused_hotkey_ignores_enter_and_space_for_start_and_cancel() {
        let models = vec![ModelViewModel {
            id: "base.en".into(),
            display_name: "Whisper Base".into(),
            installed: true,
            ready: true,
            ..Default::default()
        }];
        for capture_active in [false, true] {
            for key in [egui::Key::Enter, egui::Key::Space] {
                let ctx = egui::Context::default();
                ctx.enable_accesskit();
                crate::ui::controls::configure_accessible_style(&ctx);
                let state = TranscriptionState {
                    phase: TranscriptionPhase::Ready,
                    selected_model_id: Some("base.en".into()),
                    hotkey: "Ctrl+Space".into(),
                    hotkey_capture_active: capture_active,
                    hotkey_change_disabled_reason: Some("Hotkey change unavailable.".into()),
                    ..Default::default()
                };
                let (output, action) = render_selector_with_key(
                    &ctx,
                    &state,
                    &models,
                    egui::Id::new("recording-hotkey-action"),
                    key,
                    false,
                );
                assert_eq!(action, ScreenAction::None);
                let expected_name = if capture_active {
                    "Cancel recording shortcut capture"
                } else {
                    "Change recording shortcut"
                };
                let hotkey = output
                    .platform_output
                    .accesskit_update
                    .unwrap()
                    .nodes
                    .into_iter()
                    .find_map(|(_, node)| {
                        (node.role() == egui::accesskit::Role::Button
                            && node.name() == Some(expected_name))
                        .then_some(node)
                    })
                    .expect("disabled hotkey action");
                assert!(hotkey.is_disabled());
            }
        }
    }

    #[test]
    fn hotkey_capture_keeps_the_quick_control_height_stable() {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        crate::ui::controls::configure_accessible_style(&ctx);
        let state = TranscriptionState {
            phase: TranscriptionPhase::Ready,
            selected_model_id: Some("base.en".into()),
            hotkey: "Ctrl+Space".into(),
            hotkey_capture_active: true,
            ..Default::default()
        };
        let models = vec![ModelViewModel {
            id: "base.en".into(),
            display_name: "Whisper Base".into(),
            installed: true,
            ready: true,
            ..Default::default()
        }];
        let (output, action) =
            render_selector_with_events(&ctx, &state, &models, 363.0, Vec::new());
        assert_eq!(action, ScreenAction::None);
        let update = output.platform_output.accesskit_update.unwrap();
        let bounds = |name: &str| {
            update
                .nodes
                .iter()
                .find_map(|(_, node)| (node.name() == Some(name)).then(|| node.bounds()))
                .flatten()
                .unwrap_or_else(|| panic!("missing {name}"))
        };
        let capture = bounds("Cancel recording shortcut capture");
        assert!(capture.width() <= SELECTOR_HOTKEY_MAX_WIDTH as f64);
        assert_eq!(capture.height(), SELECTOR_CONTROL_HEIGHT as f64);

        let idle_state = TranscriptionState {
            hotkey_capture_active: false,
            ..state
        };
        let (idle, _) = render_selector_with_events(&ctx, &idle_state, &models, 363.0, Vec::new());
        let idle_update = idle.platform_output.accesskit_update.unwrap();
        let idle_card = idle_update
            .nodes
            .iter()
            .find_map(|(_, node)| {
                (node.name() == Some("Change recording shortcut")).then(|| node.bounds())
            })
            .flatten()
            .expect("idle hotkey card");
        assert_eq!(capture.height(), idle_card.height());
    }

    #[test]
    fn selector_values_fit_the_minimum_card_without_hard_clipping() {
        let ctx = egui::Context::default();
        crate::ui::controls::configure_accessible_style(&ctx);
        let text_width = SELECTOR_MODEL_MIN_WIDTH - 36.0 - 10.0;
        let long_model =
            "Whisper Large v3 Turbo English with an intentionally very long descriptive name";
        let _ = ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    Vec2::new(500.0, 160.0),
                )),
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let color = ui_palette(ui).text;
                    let font = egui::FontId::proportional(14.0);
                    let (capture_text, capture_galley) = ellipsized_selector_value(
                        ui,
                        HOTKEY_CAPTURE_PROMPT,
                        font.clone(),
                        color,
                        text_width,
                    );
                    assert_eq!(capture_text, HOTKEY_CAPTURE_PROMPT);
                    assert!(capture_galley.size().x <= text_width);

                    let (model_text, model_galley) =
                        ellipsized_selector_value(ui, long_model, font, color, text_width);
                    assert!(model_text.ends_with('…'));
                    assert!(!model_text.contains('�'));
                    assert!(model_galley.size().x <= text_width);
                    let paint_origin = egui::pos2(36.0, 28.0 - model_galley.size().y * 0.5);
                    let paint_bounds = egui::Rect::from_min_size(paint_origin, model_galley.size());
                    let value_bounds = egui::Rect::from_min_max(
                        egui::pos2(36.0, 5.0),
                        egui::pos2(SELECTOR_MODEL_MIN_WIDTH - 10.0, 40.0),
                    );
                    assert!(value_bounds.contains_rect(paint_bounds));
                });
            },
        );

        let render_ctx = egui::Context::default();
        render_ctx.enable_accesskit();
        crate::ui::controls::configure_accessible_style(&render_ctx);
        let state = TranscriptionState {
            phase: TranscriptionPhase::Ready,
            selected_model_id: Some("long-model".into()),
            hotkey: "Ctrl+Space".into(),
            hotkey_capture_active: true,
            ..Default::default()
        };
        let models = vec![ModelViewModel {
            id: "long-model".into(),
            display_name: long_model.into(),
            installed: true,
            ready: true,
            ..Default::default()
        }];
        let (output, action) =
            render_selector_with_events(&render_ctx, &state, &models, 900.0, Vec::new());
        assert_eq!(action, ScreenAction::None);
        let painted_values = output
            .shapes
            .iter()
            .filter_map(|shape| match &shape.shape {
                egui::epaint::Shape::Text(text) => Some(text.galley.text()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert!(painted_values.contains(&HOTKEY_CAPTURE_PROMPT));
        assert!(!painted_values.contains(&long_model));
        assert!(
            painted_values
                .iter()
                .any(|text| text.starts_with("Whisper Large") && text.ends_with('…'))
        );
    }

    #[test]
    fn enabled_selector_cards_use_pointing_hand_across_the_whole_surface_but_disabled_do_not() {
        let models = vec![ModelViewModel {
            id: "base.en".into(),
            display_name: "Whisper Base".into(),
            installed: true,
            ready: true,
            ..Default::default()
        }];
        let state = TranscriptionState {
            phase: TranscriptionPhase::Ready,
            selected_model_id: Some("base.en".into()),
            hotkey: "Ctrl+Space".into(),
            ..Default::default()
        };
        for (name, expected_action) in [
            ("Choose active model: Whisper Base", ScreenAction::None),
            (
                "Change recording shortcut",
                ScreenAction::StartHotkeyCapture,
            ),
        ] {
            let ctx = egui::Context::default();
            ctx.enable_accesskit();
            crate::ui::controls::configure_accessible_style(&ctx);
            let (initial, _) =
                render_selector_with_events(&ctx, &state, &models, 900.0, Vec::new());
            let bounds = initial
                .platform_output
                .accesskit_update
                .unwrap()
                .nodes
                .into_iter()
                .find_map(|(_, node)| (node.name() == Some(name)).then(|| node.bounds()))
                .flatten()
                .expect("selector action bounds");
            let point = egui::pos2(
                (bounds.x0 + 4.0) as f32,
                ((bounds.y0 + bounds.y1) / 2.0) as f32,
            );
            let (hovered, _) = render_selector_with_events(
                &ctx,
                &state,
                &models,
                900.0,
                vec![egui::Event::PointerMoved(point)],
            );
            assert_eq!(
                hovered.platform_output.cursor_icon,
                egui::CursorIcon::PointingHand
            );
            let _ = render_selector_with_events(
                &ctx,
                &state,
                &models,
                900.0,
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
            let (_, action) = render_selector_with_events(
                &ctx,
                &state,
                &models,
                900.0,
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
        }

        let disabled = TranscriptionState {
            model_change_disabled_reason: Some("Model change unavailable.".into()),
            hotkey_change_disabled_reason: Some("Hotkey change unavailable.".into()),
            ..state
        };
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        crate::ui::controls::configure_accessible_style(&ctx);
        let (initial, _) = render_selector_with_events(&ctx, &disabled, &models, 900.0, Vec::new());
        let change = initial
            .platform_output
            .accesskit_update
            .unwrap()
            .nodes
            .into_iter()
            .find_map(|(_, node)| {
                (node.name() == Some("Choose active model: Whisper Base")).then(|| node.bounds())
            })
            .flatten()
            .expect("disabled model card bounds");
        let point = egui::pos2(
            (change.x0 + 4.0) as f32,
            ((change.y0 + change.y1) / 2.0) as f32,
        );
        let (hovered, action) = render_selector_with_events(
            &ctx,
            &disabled,
            &models,
            900.0,
            vec![egui::Event::PointerMoved(point)],
        );
        assert_ne!(
            hovered.platform_output.cursor_icon,
            egui::CursorIcon::PointingHand
        );
        assert_eq!(action, ScreenAction::None);
    }

    #[test]
    fn selector_cards_are_bounded_and_stack_at_their_compact_threshold() {
        let state = TranscriptionState {
            phase: TranscriptionPhase::Ready,
            selected_model_id: Some("large-v3-turbo.en".into()),
            hotkey: "Ctrl + Shift + Alt + Space".into(),
            ..Default::default()
        };
        let models = vec![ModelViewModel {
            id: "large-v3-turbo.en".into(),
            display_name: "Whisper Large v3 Turbo English — high accuracy dictation".into(),
            variant_label: "large-v3-turbo.en".into(),
            ..Default::default()
        }];

        let measurement_ctx = egui::Context::default();
        crate::ui::controls::configure_accessible_style(&measurement_ctx);
        let mut threshold = 0.0;
        let _ = measurement_ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    Vec2::new(1_600.0, 200.0),
                )),
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    threshold = selector_card_width(
                        ui,
                        selected_model_name(&state, &models),
                        SELECTOR_MODEL_MIN_WIDTH,
                        SELECTOR_MODEL_MAX_WIDTH,
                    ) + selector_card_width(
                        ui,
                        &state.hotkey,
                        SELECTOR_HOTKEY_MIN_WIDTH,
                        SELECTOR_HOTKEY_MAX_WIDTH,
                    ) + ui.spacing().item_spacing.x;
                });
            },
        );

        for (width, should_stack) in [
            (threshold + 1.0, false),
            (threshold, false),
            (threshold - 1.0, true),
        ] {
            let ctx = egui::Context::default();
            ctx.enable_accesskit();
            crate::ui::controls::configure_accessible_style(&ctx);
            let output = ctx.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        Vec2::new(width + 16.0, 240.0),
                    )),
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        ui.allocate_ui_with_layout(
                            Vec2::new(width, 0.0),
                            Layout::top_down(Align::LEFT),
                            |ui| {
                                selector_row(ui, &state, &models, &models);
                            },
                        );
                    });
                },
            );
            let update = output.platform_output.accesskit_update.unwrap();
            let bounds = |name: &str| {
                update
                    .nodes
                    .iter()
                    .find_map(|(_, node)| {
                        (node.name() == Some(name)
                            || (name.starts_with("Choose active model:")
                                && node.name().is_some_and(|node_name| {
                                    node_name.starts_with("Choose active model:")
                                })))
                        .then(|| node.bounds())
                    })
                    .flatten()
                    .unwrap_or_else(|| panic!("missing {name} bounds"))
            };
            let model = bounds(
                "Choose active model: Whisper Large v3 Turbo English â€” high accuracy dictation",
            );
            let hotkey = bounds("Change recording shortcut");
            assert!(model.width() <= SELECTOR_MODEL_MAX_WIDTH as f64);
            assert!(hotkey.width() <= SELECTOR_HOTKEY_MAX_WIDTH as f64);
            assert_eq!(model.height(), SELECTOR_CONTROL_HEIGHT as f64);
            assert_eq!(hotkey.height(), SELECTOR_CONTROL_HEIGHT as f64);
            if should_stack {
                assert!(model.y1 <= hotkey.y0);
            } else {
                assert!(model.x1 <= hotkey.x0);
                assert!((model.y0 - hotkey.y0).abs() <= f64::EPSILON);
            }
        }
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
    fn active_capture_exposes_keyboard_operable_discard_action() {
        for phase in [
            TranscriptionPhase::RequestingMicrophone,
            TranscriptionPhase::Listening,
        ] {
            let state = TranscriptionState {
                phase,
                selected_model_id: Some("base.en".into()),
                ..Default::default()
            };
            let output = render_transcribe(&state, &[]);
            let nodes = &output.platform_output.accesskit_update.unwrap().nodes;
            assert!(nodes.iter().any(|(_, node)| {
                node.role() == egui::accesskit::Role::Button
                    && node.name() == Some("Cancel recording and discard it")
            }));
        }
    }

    #[test]
    fn presented_live_overlay_suppresses_root_live_transcript_ownership() {
        let state = TranscriptionState {
            phase: TranscriptionPhase::Listening,
            selected_model_id: Some("base.en".into()),
            committed_transcript: "committed words".into(),
            provisional_transcript: "tentative words".into(),
            suppress_live_announcements: true,
            ..Default::default()
        };
        let output = render_transcribe(&state, &[]);
        let nodes = &output.platform_output.accesskit_update.unwrap().nodes;
        assert!(!nodes.iter().any(|(_, node)| {
            node.live().is_some()
                && node.name().is_some_and(|name| {
                    name.contains("committed words") || name.contains("tentative words")
                })
        }));
        assert!(!nodes.iter().any(|(_, node)| {
            node.live().is_some() && node.name().is_some_and(|name| name.contains("Recording"))
        }));
    }

    #[test]
    fn enabled_root_announces_phase_and_committed_transcript() {
        let state = TranscriptionState {
            phase: TranscriptionPhase::Listening,
            selected_model_id: Some("base.en".into()),
            committed_transcript: "committed words".into(),
            provisional_transcript: "tentative words".into(),
            suppress_live_announcements: false,
            ..Default::default()
        };
        let output = render_transcribe(&state, &[]);
        let nodes = &output.platform_output.accesskit_update.unwrap().nodes;
        let polite_nodes = nodes
            .iter()
            .filter(|(_, node)| node.live() == Some(egui::accesskit::Live::Polite))
            .collect::<Vec<_>>();

        assert_eq!(polite_nodes.len(), 2);
        assert!(
            polite_nodes
                .iter()
                .any(|(_, node)| node.name() == Some("Recording"))
        );
        let committed = nodes
            .iter()
            .find_map(|(_, node)| (node.name() == Some("committed words")).then_some(node))
            .expect("committed transcript node");
        assert_eq!(committed.live(), Some(egui::accesskit::Live::Polite));
        let estimate = nodes
            .iter()
            .find_map(|(_, node)| {
                (node.name() == Some("Live estimate, may change: tentative words")).then_some(node)
            })
            .expect("separate live-estimate node with actual provisional text");
        assert_eq!(estimate.role(), egui::accesskit::Role::StaticText);
        assert!(estimate.live().is_none());
        assert!(polite_nodes.iter().all(|(_, node)| {
            !node
                .name()
                .is_some_and(|name| name.contains("tentative words"))
        }));
        assert!(nodes.iter().any(|(_, node)| {
            node.description()
                == Some("Italic text is a live estimate and may change until recording ends.")
        }));
    }

    #[test]
    fn provisional_only_text_is_visibly_and_accessibly_named_as_a_live_estimate() {
        let state = TranscriptionState {
            phase: TranscriptionPhase::Listening,
            selected_model_id: Some("base.en".into()),
            provisional_transcript: "words may change".into(),
            ..Default::default()
        };
        let output = render_transcribe(&state, &[]);
        let nodes = &output.platform_output.accesskit_update.unwrap().nodes;

        assert!(nodes.iter().any(|(_, node)| {
            node.name() == Some("Live estimate, may change: words may change")
        }));
    }

    #[test]
    fn listening_phase_is_exposed_as_recording() {
        let state = TranscriptionState {
            phase: TranscriptionPhase::Listening,
            selected_model_id: Some("base.en".into()),
            ..Default::default()
        };
        let output = render_transcribe(&state, &[]);
        let nodes = &output.platform_output.accesskit_update.unwrap().nodes;

        assert!(
            nodes
                .iter()
                .any(|(_, node)| node.name() == Some("Recording"))
        );
        assert!(
            !nodes
                .iter()
                .any(|(_, node)| node.name() == Some("Listening"))
        );
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
                node.name() == Some("Choose active model: whisper.cpp base.en")
                    && node.description() == Some(reason)
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
    fn remote_catalog_cards_apply_query_language_and_duplicate_filters() {
        let known_local = ModelViewModel {
            id: "known-local".into(),
            display_name: "Installed duplicate target".into(),
            installed: true,
            ready: true,
            languages: vec!["en".into()],
            ..Default::default()
        };
        let entries = vec![
            RemoteCatalogEntryView {
                id: "acme/english-archive".into(),
                display_name: "English Archive".into(),
                description: "Production transcription for English recordings.".into(),
                languages: vec!["en".into()],
                language_summary: "English only".into(),
                repository: "acme/english-archive".into(),
                variants: vec![
                    RemoteCatalogVariantView {
                        id: "english-q5-balanced".into(),
                        filename: "english-q5.gguf".into(),
                        size_label: "512 MB".into(),
                        status_label: Some("Stable".into()),
                        accuracy_guidance: "Balanced accuracy".into(),
                        ..Default::default()
                    },
                    RemoteCatalogVariantView {
                        id: "english-experimental".into(),
                        filename: "english-fast.gguf".into(),
                        size_label: "410 MB".into(),
                        status_label: Some("Experimental candidate".into()),
                        accuracy_guidance: "Fast transcription".into(),
                        ..Default::default()
                    },
                    RemoteCatalogVariantView {
                        id: "english-normalized-duplicate".into(),
                        filename: "english-duplicate.gguf".into(),
                        accuracy_guidance: "Balanced accuracy".into(),
                        normalized_model_id: Some("known-local".into()),
                        ..Default::default()
                    },
                    RemoteCatalogVariantView {
                        id: "english-managed-duplicate".into(),
                        filename: "english-managed-duplicate.gguf".into(),
                        accuracy_guidance: "Balanced accuracy".into(),
                        managed_model_id: Some("known-local".into()),
                        ..Default::default()
                    },
                ],
                ..Default::default()
            },
            RemoteCatalogEntryView {
                id: "acme/world".into(),
                display_name: "World Speech".into(),
                description: "Multilingual transcription.".into(),
                languages: vec!["en".into(), "es".into()],
                language_summary: "English, Spanish".into(),
                repository: "acme/world".into(),
                variants: vec![RemoteCatalogVariantView {
                    id: "world-q4".into(),
                    filename: "world-q4.gguf".into(),
                    size_label: "1.2 GB".into(),
                    status_label: Some("Stable".into()),
                    accuracy_guidance: "Fast transcription".into(),
                    ..Default::default()
                }],
                ..Default::default()
            },
            RemoteCatalogEntryView {
                id: "acme/japanese".into(),
                display_name: "Japanese Speech".into(),
                description: "Japanese transcription.".into(),
                languages: vec!["ja".into()],
                language_summary: "Japanese".into(),
                repository: "acme/japanese".into(),
                variants: vec![RemoteCatalogVariantView {
                    id: "japanese-q5-balanced".into(),
                    filename: "japanese-q5.gguf".into(),
                    size_label: "512 MB".into(),
                    status_label: Some("Stable".into()),
                    accuracy_guidance: "Balanced accuracy".into(),
                    ..Default::default()
                }],
                ..Default::default()
            },
        ];

        for (query, language_filter, expected_remote_ids) in [
            (
                "english archive",
                ModelLanguageFilter::English,
                &["english-q5-balanced", "english-experimental"][..],
            ),
            (
                "production transcription",
                ModelLanguageFilter::English,
                &["english-q5-balanced", "english-experimental"][..],
            ),
            (
                "english only",
                ModelLanguageFilter::English,
                &["english-q5-balanced", "english-experimental"][..],
            ),
            (
                "acme/english-archive",
                ModelLanguageFilter::English,
                &["english-q5-balanced", "english-experimental"][..],
            ),
            (
                "english-q5-balanced",
                ModelLanguageFilter::English,
                &["english-q5-balanced"][..],
            ),
            (
                "english-q5.gguf",
                ModelLanguageFilter::English,
                &["english-q5-balanced"][..],
            ),
            (
                "balanced accuracy",
                ModelLanguageFilter::English,
                &["english-q5-balanced"][..],
            ),
            (
                "experimental candidate",
                ModelLanguageFilter::English,
                &["english-experimental"][..],
            ),
            (
                "512 mb",
                ModelLanguageFilter::English,
                &["english-q5-balanced"][..],
            ),
            (
                "acme/world",
                ModelLanguageFilter::Multilingual,
                &["world-q4"][..],
            ),
            ("not in the catalog", ModelLanguageFilter::All, &[][..]),
        ] {
            let remote_catalog = RemoteCatalogView {
                query: query.into(),
                entries: entries.clone(),
                ..Default::default()
            };
            let (installed, available) = build_model_card_lists(
                std::slice::from_ref(&known_local),
                &[],
                &remote_catalog,
                language_filter,
            );

            assert!(installed.is_empty(), "query={query}");
            assert_eq!(available.len(), expected_remote_ids.len(), "query={query}");
            let remote_ids = available
                .iter()
                .map(|card| match card {
                    ModelCard::Remote(_, variant) => variant.id.as_str(),
                    ModelCard::Local(_) => {
                        panic!("query={query} unexpectedly included a local card")
                    }
                })
                .collect::<Vec<_>>();
            assert_eq!(remote_ids.as_slice(), expected_remote_ids, "query={query}");
        }
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
        let (nodes, root, orphaned_updated, orphaned_added) =
            apply_accesskit_incremental_update(&recording, &advanced);
        assert_eq!(orphaned_updated, Vec::new());
        assert_eq!(orphaned_added, Vec::new());
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
        assert!(accesskit_is_descendant(&nodes, panel, automatic_stop));
        assert!(
            !nodes
                .get(&root)
                .expect("AccessKit root should remain attached")
                .children()
                .contains(&automatic_stop)
        );
    }

    #[test]
    fn route_auto_id_ranges_are_disjoint_and_leave_room_for_settings_tabs() {
        let settings_origin = route_auto_id_offset(UiRoute::Settings(SettingsTab::General));
        let history_origin = route_auto_id_offset(UiRoute::History);
        let route_headroom = history_origin - settings_origin;
        let final_settings_tab_offset = [
            SettingsTab::General,
            SettingsTab::Recording,
            SettingsTab::Advanced,
            SettingsTab::About,
        ]
        .into_iter()
        .map(settings_tab_auto_id_offset)
        .max()
        .expect("settings exposes at least one tab");
        assert!(
            final_settings_tab_offset < route_headroom,
            "the last Settings tab range must fit before the History route range"
        );

        let routes = [
            UiRoute::Transcribe,
            UiRoute::Models,
            UiRoute::Settings(SettingsTab::General),
            UiRoute::History,
            UiRoute::About,
            UiRoute::Debug,
        ];
        for (index, route) in routes.iter().enumerate() {
            for other_route in routes.iter().skip(index + 1) {
                assert_ne!(
                    route_auto_id_offset(*route),
                    route_auto_id_offset(*other_route),
                    "{route:?} and {other_route:?} must use distinct automatic ID ranges"
                );
            }
        }
        for tab in [
            SettingsTab::General,
            SettingsTab::Recording,
            SettingsTab::Advanced,
            SettingsTab::About,
        ] {
            assert_eq!(
                route_auto_id_offset(UiRoute::Settings(tab)),
                route_auto_id_offset(UiRoute::Settings(SettingsTab::General))
            );
        }
    }

    #[test]
    fn every_route_transition_to_history_keeps_accesskit_nodes_attached() {
        fn history_record() -> HistoryRecord {
            HistoryRecord {
                id: 7,
                created_at_ms: 1,
                updated_at_ms: 1,
                completed_at_ms: Some(1),
                status: HistoryStatus::Completed,
                raw_text: "raw transcription".into(),
                final_text: Some("final transcription".into()),
                model_id: "whisper_cpp_base_en".into(),
                metrics: HistoryMetrics::default(),
                pinned: false,
                source_app: None,
                audio_path: None,
                failure: None,
                retry_count: 0,
                output_outcome: None,
            }
        }

        fn raw_input() -> egui::RawInput {
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    Vec2::new(1180.0, 815.0),
                )),
                focused: true,
                ..Default::default()
            }
        }

        fn is_descendant(
            update: &egui::accesskit::TreeUpdate,
            ancestor: egui::accesskit::NodeId,
            target: egui::accesskit::NodeId,
        ) -> bool {
            let Some((_, ancestor)) = update.nodes.iter().find(|(id, _)| *id == ancestor) else {
                return false;
            };
            let mut pending = ancestor.children().to_vec();
            while let Some(id) = pending.pop() {
                if id == target {
                    return true;
                }
                if let Some((_, node)) = update.nodes.iter().find(|(node_id, _)| *node_id == id) {
                    pending.extend_from_slice(node.children());
                }
            }
            false
        }

        fn assert_history_results_semantics(update: &egui::accesskit::TreeUpdate) {
            let results_id = update
                .nodes
                .iter()
                .find(|(_, node)| {
                    node.role() == egui::accesskit::Role::Group
                        && node.name() == Some("History results")
                })
                .map(|(id, _)| *id)
                .expect("History must expose one stable results group");
            let heading_id = update
                .nodes
                .iter()
                .find(|(_, node)| {
                    node.role() == egui::accesskit::Role::Heading
                        && node
                            .name()
                            .is_some_and(|name| name.starts_with("Completed - "))
                })
                .map(|(id, _)| *id)
                .expect("populated History must expose a record heading");
            let action_id = update
                .nodes
                .iter()
                .find(|(_, node)| {
                    node.role() == egui::accesskit::Role::Button
                        && node.name() == Some("Delete entry")
                })
                .map(|(id, _)| *id)
                .expect("populated History must expose record actions");
            assert!(is_descendant(update, results_id, heading_id));
            assert!(is_descendant(update, results_id, action_id));
        }

        fn assert_multi_record_context(
            update: &egui::accesskit::TreeUpdate,
            model_ids: &[&str],
            armed_model_id: &str,
        ) {
            let results_id = update
                .nodes
                .iter()
                .find(|(_, node)| {
                    node.role() == egui::accesskit::Role::Group
                        && node.name() == Some("History results")
                })
                .map(|(id, _)| *id)
                .expect("History must expose one stable results group");
            for model_id in model_ids {
                let raw_id = update
                    .nodes
                    .iter()
                    .find(|(_, node)| {
                        node.name() == Some("Raw transcript")
                            && node
                                .description()
                                .is_some_and(|description| description.contains(model_id))
                    })
                    .map(|(id, _)| *id)
                    .unwrap_or_else(|| {
                        panic!("Raw transcript disclosure must identify model {model_id}")
                    });
                assert!(is_descendant(update, results_id, raw_id));
            }
            let armed_id = update
                .nodes
                .iter()
                .find(|(_, node)| {
                    node.name() == Some("Paste armed")
                        && node.description().is_some_and(|description| {
                            description.contains(armed_model_id)
                                && description.contains("already armed")
                        })
                })
                .map(|(id, _)| *id)
                .expect("armed paste action must identify its record context");
            assert!(is_descendant(update, results_id, armed_id));
        }

        fn assert_incremental_safe(
            previous: &egui::accesskit::TreeUpdate,
            next: &egui::accesskit::TreeUpdate,
            transition: &str,
        ) {
            let (_, _, orphaned_updated, orphaned_added) =
                apply_accesskit_incremental_update(previous, next);
            assert!(
                orphaned_updated.is_empty(),
                "{transition} orphaned updated nodes: {orphaned_updated:?}"
            );
            assert!(
                orphaned_added.is_empty(),
                "{transition} orphaned added nodes: {orphaned_added:?}"
            );
        }

        fn render_source_route(ctx: &egui::Context, route: UiRoute) -> egui::accesskit::TreeUpdate {
            let state = TranscriptionState {
                phase: TranscriptionPhase::Ready,
                ..Default::default()
            };
            let settings = RecordingSettingsView::default();
            let comparison = ModelComparisonState::default();
            ctx.run(raw_input(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    show_route_scroll(ui, route, |ui| {
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
                });
            })
            .platform_output
            .accesskit_update
            .expect("source route should expose AccessKit")
        }

        fn render_history(
            ctx: &egui::Context,
            records: &[HistoryRecord],
            loading: bool,
            query: &str,
            armed_repaste: Option<i64>,
        ) -> egui::accesskit::TreeUpdate {
            let mut search = query.to_owned();
            ctx.run(raw_input(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    show_route_scroll(ui, UiRoute::History, |ui| {
                        // Match the production History route's page header,
                        // status slot, and body order before rendering cards.
                        ui.allocate_ui_with_layout(
                            egui::Vec2::new(ui.available_width(), 0.0),
                            egui::Layout::top_down(egui::Align::LEFT),
                            |ui| {
                                ui.horizontal_top(|ui| {
                                    let heading =
                                        ui.label(RichText::new("History").size(30.0).strong());
                                    ui.ctx().accesskit_node_builder(heading.id, |builder| {
                                        builder.set_role(egui::accesskit::Role::Heading);
                                    });
                                    ui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::Center),
                                        |ui| {
                                            ui.label("Ready");
                                        },
                                    );
                                });
                                ui.add_space(2.0);
                                let status = ui.label("Ready");
                                ui.ctx().accesskit_node_builder(status.id, |builder| {
                                    builder.set_live(egui::accesskit::Live::Polite);
                                });
                                ui.add_space(14.0);
                                history_page(
                                    ui,
                                    HistoryPageState {
                                        search: &mut search,
                                        records,
                                        has_more: false,
                                        loading,
                                        error: None,
                                        confirm_delete: None,
                                        work_active: false,
                                        playing: None,
                                        playback_stopping: false,
                                        armed_repaste,
                                        focus_search: false,
                                        focus_delete_confirmation: false,
                                    },
                                );
                            },
                        );
                    });
                });
            })
            .platform_output
            .accesskit_update
            .expect("history should expose AccessKit")
        }

        fn render_history_interaction(
            ctx: &egui::Context,
            search: &mut String,
            records: &[HistoryRecord],
            loading: bool,
            focus_search: bool,
            events: Vec<egui::Event>,
        ) -> (egui::accesskit::TreeUpdate, Option<HistoryPageAction>) {
            let mut action = None;
            let output = ctx.run(
                egui::RawInput {
                    events,
                    ..raw_input()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        show_route_scroll(ui, UiRoute::History, |ui| {
                            ui.allocate_ui_with_layout(
                                egui::Vec2::new(ui.available_width(), 0.0),
                                egui::Layout::top_down(egui::Align::LEFT),
                                |ui| {
                                    ui.horizontal_top(|ui| {
                                        let heading =
                                            ui.label(RichText::new("History").size(30.0).strong());
                                        ui.ctx().accesskit_node_builder(heading.id, |builder| {
                                            builder.set_role(egui::accesskit::Role::Heading);
                                        });
                                        ui.with_layout(
                                            egui::Layout::right_to_left(egui::Align::Center),
                                            |ui| {
                                                ui.label("Ready");
                                            },
                                        );
                                    });
                                    ui.add_space(2.0);
                                    let status = ui.label("Ready");
                                    ui.ctx().accesskit_node_builder(status.id, |builder| {
                                        builder.set_live(egui::accesskit::Live::Polite);
                                    });
                                    ui.add_space(14.0);
                                    action = history_page(
                                        ui,
                                        HistoryPageState {
                                            search,
                                            records,
                                            has_more: false,
                                            loading,
                                            error: None,
                                            confirm_delete: None,
                                            work_active: false,
                                            playing: None,
                                            playback_stopping: false,
                                            armed_repaste: None,
                                            focus_search,
                                            focus_delete_confirmation: false,
                                        },
                                    );
                                },
                            );
                        });
                    });
                },
            );
            (
                output
                    .platform_output
                    .accesskit_update
                    .expect("interactive history should expose AccessKit"),
                action,
            )
        }

        let record = history_record();
        let history_states = [
            ("empty", &[][..], false, "No matching history entries"),
            ("loading", &[][..], true, "Loading local history"),
            (
                "populated",
                std::slice::from_ref(&record),
                false,
                "1 history entries loaded",
            ),
        ];
        for source_route in [
            UiRoute::Transcribe,
            UiRoute::Models,
            UiRoute::Settings(SettingsTab::Advanced),
            UiRoute::About,
            UiRoute::Debug,
        ] {
            for (state_name, records, loading, expected_status) in history_states {
                let ctx = egui::Context::default();
                ctx.enable_accesskit();
                let source = render_source_route(&ctx, source_route);
                let history = render_history(&ctx, records, loading, "", None);
                let (nodes, _root, orphaned_updated, orphaned_added) =
                    apply_accesskit_incremental_update(&source, &history);

                assert!(
                    orphaned_updated.is_empty(),
                    "{source_route:?} -> {state_name} History reparented an updated AccessKit node: {orphaned_updated:?}"
                );
                assert!(
                    orphaned_added.is_empty(),
                    "{source_route:?} -> {state_name} History added and removed an AccessKit node in one update: {orphaned_added:?}"
                );
                assert!(
                    nodes
                        .values()
                        .any(|node| node.name() == Some(expected_status)),
                    "{source_route:?} -> {state_name} History should retain its status node"
                );
                if state_name == "populated" {
                    assert!(nodes.values().any(|node| {
                        node.role() == egui::accesskit::Role::Group
                            && node.name() == Some("History results")
                    }));
                    assert_history_results_semantics(&history);
                }
                let mut consumer = accesskit_consumer::Tree::new(source, true);
                consumer.update_and_process_changes(history, &mut NoopAccessKitChangeHandler);
            }
        }

        // Live History search changes both the search-control subtree and the
        // result subtree across consecutive incremental updates. Exercise the
        // same populated -> loading -> filtered sequence used by the app so an
        // updated AccessKit node can never also be removed as an orphan.
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let populated = render_history(&ctx, std::slice::from_ref(&record), false, "", None);
        let loading = render_history(&ctx, &[], true, "meeting", None);
        let mut consumer = accesskit_consumer::Tree::new(populated.clone(), true);
        consumer.update_and_process_changes(loading.clone(), &mut NoopAccessKitChangeHandler);
        let (_, _, orphaned_updated, orphaned_added) =
            apply_accesskit_incremental_update(&populated, &loading);
        assert!(
            orphaned_updated.is_empty(),
            "live search orphaned updated nodes while entering loading: {orphaned_updated:?}"
        );
        assert!(
            orphaned_added.is_empty(),
            "live search orphaned added nodes while entering loading: {orphaned_added:?}"
        );

        let filtered = render_history(&ctx, std::slice::from_ref(&record), false, "meeting", None);
        consumer.update_and_process_changes(filtered.clone(), &mut NoopAccessKitChangeHandler);
        let (_, _, orphaned_updated, orphaned_added) =
            apply_accesskit_incremental_update(&loading, &filtered);
        assert!(
            orphaned_updated.is_empty(),
            "live search orphaned updated nodes while showing results: {orphaned_updated:?}"
        );
        assert!(
            orphaned_added.is_empty(),
            "live search orphaned added nodes while showing results: {orphaned_added:?}"
        );

        let removed = render_history(&ctx, &[], false, "meeting", None);
        let (_, _, orphaned_updated, orphaned_added) =
            apply_accesskit_incremental_update(&filtered, &removed);
        assert!(
            orphaned_updated.is_empty(),
            "record removal orphaned updated nodes: {orphaned_updated:?}"
        );
        assert!(
            orphaned_added.is_empty(),
            "record removal orphaned added nodes: {orphaned_added:?}"
        );
        consumer.update_and_process_changes(removed.clone(), &mut NoopAccessKitChangeHandler);

        let mut replacement_record = record.clone();
        replacement_record.id = 2;
        replacement_record.raw_text = "replacement transcript".into();
        replacement_record.final_text = Some("replacement transcript".into());
        let replacement = render_history(
            &ctx,
            std::slice::from_ref(&replacement_record),
            false,
            "meeting",
            None,
        );
        let mut replacement_consumer = accesskit_consumer::Tree::new(filtered.clone(), true);
        replacement_consumer
            .update_and_process_changes(replacement.clone(), &mut NoopAccessKitChangeHandler);
        let (_, _, orphaned_updated, orphaned_added) =
            apply_accesskit_incremental_update(&filtered, &replacement);
        assert!(
            orphaned_updated.is_empty(),
            "record replacement orphaned updated nodes: {orphaned_updated:?}"
        );
        assert!(
            orphaned_added.is_empty(),
            "record replacement orphaned added nodes: {orphaned_added:?}"
        );

        let mut second_record = record.clone();
        second_record.id = 8;
        second_record.raw_text = "second transcript".into();
        second_record.final_text = Some("clean second transcript".into());
        second_record.model_id = "whisper_cpp_small_en".into();
        let mut third_record = record.clone();
        third_record.id = 9;
        third_record.raw_text = "third transcript".into();
        third_record.final_text = Some("clean third transcript".into());
        third_record.model_id = "whisper_cpp_medium_en".into();

        let three_records = render_history(
            &ctx,
            &[record.clone(), second_record.clone(), third_record.clone()],
            false,
            "",
            Some(second_record.id),
        );
        assert_history_results_semantics(&three_records);
        assert_multi_record_context(
            &three_records,
            &[
                "whisper_cpp_base_en",
                "whisper_cpp_small_en",
                "whisper_cpp_medium_en",
            ],
            "whisper_cpp_small_en",
        );
        let mut multi_record_consumer = accesskit_consumer::Tree::new(three_records.clone(), true);

        let middle_removed = render_history(
            &ctx,
            &[record.clone(), third_record.clone()],
            false,
            "",
            None,
        );
        assert_incremental_safe(&three_records, &middle_removed, "middle record removal");
        multi_record_consumer
            .update_and_process_changes(middle_removed.clone(), &mut NoopAccessKitChangeHandler);
        assert_history_results_semantics(&middle_removed);

        let first_removed =
            render_history(&ctx, std::slice::from_ref(&third_record), false, "", None);
        assert_incremental_safe(&middle_removed, &first_removed, "first record removal");
        multi_record_consumer
            .update_and_process_changes(first_removed.clone(), &mut NoopAccessKitChangeHandler);
        assert_history_results_semantics(&first_removed);

        let replacement_after_removals = render_history(
            &ctx,
            std::slice::from_ref(&replacement_record),
            false,
            "replacement",
            None,
        );
        assert_incremental_safe(
            &first_removed,
            &replacement_after_removals,
            "remaining record replacement",
        );
        multi_record_consumer.update_and_process_changes(
            replacement_after_removals.clone(),
            &mut NoopAccessKitChangeHandler,
        );
        assert_history_results_semantics(&replacement_after_removals);

        let loading_after_records = render_history(&ctx, &[], true, "replacement", None);
        assert_incremental_safe(
            &replacement_after_removals,
            &loading_after_records,
            "populated to loading",
        );
        multi_record_consumer.update_and_process_changes(
            loading_after_records.clone(),
            &mut NoopAccessKitChangeHandler,
        );

        let empty_after_loading = render_history(&ctx, &[], false, "replacement", None);
        assert_incremental_safe(
            &loading_after_records,
            &empty_after_loading,
            "loading to empty",
        );
        multi_record_consumer.update_and_process_changes(
            empty_after_loading.clone(),
            &mut NoopAccessKitChangeHandler,
        );

        let results_after_empty = render_history(
            &ctx,
            std::slice::from_ref(&replacement_record),
            false,
            "replacement",
            None,
        );
        assert_incremental_safe(
            &empty_after_loading,
            &results_after_empty,
            "empty to results",
        );
        multi_record_consumer.update_and_process_changes(
            results_after_empty.clone(),
            &mut NoopAccessKitChangeHandler,
        );
        assert_history_results_semantics(&results_after_empty);

        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let mut search = String::new();
        let (initial, _) = render_history_interaction(
            &ctx,
            &mut search,
            std::slice::from_ref(&record),
            false,
            true,
            Vec::new(),
        );
        let mut consumer = accesskit_consumer::Tree::new(initial, true);
        let (settled, _) = render_history_interaction(
            &ctx,
            &mut search,
            std::slice::from_ref(&record),
            false,
            false,
            Vec::new(),
        );
        consumer.update_and_process_changes(settled, &mut NoopAccessKitChangeHandler);
        let (typed, typed_action) = render_history_interaction(
            &ctx,
            &mut search,
            std::slice::from_ref(&record),
            false,
            false,
            vec![egui::Event::Text("meeting".into())],
        );
        assert_eq!(typed_action, Some(HistoryPageAction::ApplySearch));
        consumer.update_and_process_changes(typed, &mut NoopAccessKitChangeHandler);
        let (loading, _) =
            render_history_interaction(&ctx, &mut search, &[], true, false, Vec::new());
        consumer.update_and_process_changes(loading, &mut NoopAccessKitChangeHandler);
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
        assert!(nodes.iter().any(|(_, node)| {
            node.role() == egui::accesskit::Role::RadioGroup
                && node.name() == Some("Recording mode")
        }));
        assert!(!nodes.iter().any(|(_, node)| {
            node.role() == egui::accesskit::Role::Button
                && node.name() == Some("Global record hotkey information")
        }));
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
    fn non_obvious_settings_expose_contextual_help_with_accessible_descriptions() {
        assert_eq!(
            TRANSCRIPT_RETENTION_DAYS_HELP.description,
            "Remove the entire unpinned history entry, including any retained audio, after this many days. Pinned entries are kept."
        );
        assert_eq!(
            AUDIO_RETENTION_DAYS_HELP.description,
            "Remove only retained audio from unpinned entries after this many days; the transcript entry remains. Pinned entries are kept."
        );
        let settings_view = RecordingSettingsView {
            auto_insert_transcript: true,
            show_restore_clipboard: true,
            history_mode_label: "Transcript and audio".into(),
            transcript_retention_days: Some(30),
            audio_retention_days: Some(30),
            ..Default::default()
        };
        let expected = [
            (
                SettingsTab::General,
                &[
                    ("Active model", ACTIVE_MODEL_HELP),
                    ("Dictation overlay", DICTATION_OVERLAY_HELP),
                    ("Paste delay ms", PASTE_DELAY_HELP),
                ][..],
            ),
            (
                SettingsTab::Recording,
                &[
                    ("Streaming mode", STREAMING_MODE_HELP),
                    ("Transcription device", TRANSCRIPTION_DEVICE_HELP),
                ][..],
            ),
            (
                SettingsTab::Advanced,
                &[
                    ("Speech confirmation ms", SPEECH_CONFIRMATION_HELP),
                    ("Internal pause ms", INTERNAL_PAUSE_HELP),
                    ("End after silence ms", END_AFTER_SILENCE_HELP),
                    ("Pre-roll ms", PRE_ROLL_HELP),
                    ("Post-roll ms", POST_ROLL_HELP),
                    ("History storage", HISTORY_STORAGE_HELP),
                    ("Maximum unpinned entries", MAX_HISTORY_ENTRIES_HELP),
                    ("Transcript days", TRANSCRIPT_RETENTION_DAYS_HELP),
                    ("Audio days", AUDIO_RETENTION_DAYS_HELP),
                ][..],
            ),
        ];
        let expected_absent = [
            (
                SettingsTab::General,
                ["Theme", "Overlay position"].as_slice(),
            ),
            (
                SettingsTab::Recording,
                ["Mode", "Duration limit", "Global record hotkey", "Device"].as_slice(),
            ),
        ];

        for (tab, rows) in expected {
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
            for (label, help) in rows {
                let name = format!("{label} information");
                let help_node = nodes
                    .iter()
                    .find_map(|(_, node)| {
                        (node.role() == egui::accesskit::Role::Button
                            && node.name() == Some(name.as_str()))
                        .then_some(node)
                    })
                    .unwrap_or_else(|| panic!("missing {name}"));
                assert_eq!(help_node.description(), Some(help.description));
                assert_eq!(help_node.is_expanded(), Some(false));
                let bounds = help_node.bounds().expect("help target bounds");
                assert!(bounds.width() >= 44.0 && bounds.height() >= 44.0);
            }
        }
        for (tab, labels) in expected_absent {
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
            for label in labels {
                let name = format!("{label} information");
                assert!(
                    !nodes.iter().any(|(_, node)| {
                        node.role() == egui::accesskit::Role::Button
                            && node.name() == Some(name.as_str())
                    }),
                    "{name} should not expose redundant contextual help"
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

    fn primary_pointer_event(pos: egui::Pos2, pressed: bool) -> egui::Event {
        egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        }
    }

    fn key_press(key: egui::Key, modifiers: egui::Modifiers) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        }
    }

    fn compact_language_filter_expanded(output: &egui::FullOutput) -> bool {
        output
            .platform_output
            .accesskit_update
            .as_ref()
            .is_some_and(|update| {
                update.nodes.iter().any(|(_, node)| {
                    node.role() == egui::accesskit::Role::ComboBox
                        && node.name() == Some("Filter model languages")
                        && node.is_expanded() == Some(true)
                })
            })
    }

    fn model_layout_bounds(output: &egui::FullOutput, name: &str) -> egui::accesskit::Rect {
        named_role_bounds(output, name, egui::accesskit::Role::StaticText)
    }

    fn render_model_card_at(
        model: &ModelViewModel,
        width: f32,
        height: f32,
        events: Vec<egui::Event>,
    ) -> egui::FullOutput {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        crate::ui::controls::configure_accessible_style(&ctx);
        ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    Vec2::new(width, height),
                )),
                events,
                focused: true,
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let _ =
                        render_unified_model_card(ui, ModelCard::Local(model), false, true, false);
                });
            },
        )
    }

    fn failed_warning_model() -> ModelViewModel {
        ModelViewModel {
            id: "failed-warning".into(),
            display_name: "Failed warning".into(),
            download_state: ModelDownloadState::Failed,
            error_message: Some("TLS certificate validation failed.".into()),
            ..Default::default()
        }
    }

    fn render_model_card_with_context(
        ctx: &egui::Context,
        model: &ModelViewModel,
        events: Vec<egui::Event>,
    ) -> (egui::FullOutput, ScreenAction) {
        let mut action = ScreenAction::None;
        let output = ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    Vec2::new(960.0, 680.0),
                )),
                events,
                focused: true,
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    action =
                        render_unified_model_card(ui, ModelCard::Local(model), false, true, false)
                            .action;
                });
            },
        );
        (output, action)
    }

    fn click_model_warning(
        ctx: &egui::Context,
        model: &ModelViewModel,
        point: egui::Pos2,
    ) -> (egui::FullOutput, ScreenAction) {
        let _ = render_model_card_with_context(
            ctx,
            model,
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
        render_model_card_with_context(
            ctx,
            model,
            vec![
                egui::Event::PointerMoved(point),
                egui::Event::PointerButton {
                    pos: point,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        )
    }

    fn button_expanded(output: &egui::FullOutput, name: &str) -> bool {
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
    fn input_level_combines_live_meter_with_one_accessible_sensitivity_slider() {
        use egui::accesskit::{Action, Role};

        let settings_view = RecordingSettingsView {
            speech_detection_sensitivity_percent: 42,
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
                node.role() == Role::Slider && node.name() == Some("Speech detection sensitivity")
            })
            .collect::<Vec<_>>();
        assert_eq!(sliders.len(), 1);
        let slider = &sliders[0].1;
        assert_eq!(slider.min_numeric_value(), Some(0.0));
        assert_eq!(slider.max_numeric_value(), Some(100.0));
        assert_eq!(slider.numeric_value(), Some(42.0));
        assert!(slider.supports_action(Action::SetValue));
        assert!(slider.supports_action(Action::Increment));
        assert!(slider.supports_action(Action::Decrement));
        assert!(
            slider
                .description()
                .is_some_and(|description| description.contains("speech probability threshold"))
        );
        assert!(
            slider
                .description()
                .is_some_and(|description| description.contains("teal fill"))
        );
        assert!(!slider.is_disabled());
        assert!(
            nodes
                .iter()
                .any(|(_, node)| node.name() == Some("Input level"))
        );

        let advanced_ctx = egui::Context::default();
        advanced_ctx.enable_accesskit();
        let advanced = advanced_ctx.run(Default::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let _ = settings(
                    ui,
                    SettingsTab::Advanced,
                    &TranscriptionState::default(),
                    &settings_view,
                );
            });
        });
        let advanced_nodes = &advanced.platform_output.accesskit_update.unwrap().nodes;
        assert!(
            !advanced_nodes
                .iter()
                .any(|(_, node)| node.role() == Role::Slider)
        );

        let locked_ctx = egui::Context::default();
        locked_ctx.enable_accesskit();
        let locked = locked_ctx.run(Default::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let _ = settings(
                    ui,
                    SettingsTab::Recording,
                    &TranscriptionState {
                        phase: TranscriptionPhase::Listening,
                        ..Default::default()
                    },
                    &settings_view,
                );
            });
        });
        let locked_slider = locked
            .platform_output
            .accesskit_update
            .unwrap()
            .nodes
            .into_iter()
            .find_map(|(_, node)| {
                (node.role() == Role::Slider && node.name() == Some("Speech detection sensitivity"))
                    .then_some(node)
            })
            .expect("recording settings should retain the disabled sensitivity slider");
        assert!(locked_slider.is_disabled());
        assert!(!locked_slider.supports_action(Action::SetValue));
        assert!(!locked_slider.supports_action(Action::Increment));
        assert!(!locked_slider.supports_action(Action::Decrement));
        assert!(locked_slider.description().is_some_and(|description| {
            description.contains(VOICE_DETECTION_LOCKED_DESCRIPTION)
        }));
    }

    #[test]
    fn input_level_sensitivity_slider_supports_pointer_and_keyboard_adjustment() {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let settings_view = RecordingSettingsView {
            speech_detection_sensitivity_percent: 42,
            input_level_percent: 72,
            ..Default::default()
        };
        let (initial, action) =
            render_recording_settings_with_input(&ctx, &settings_view, Vec::new());
        assert_eq!(action, ScreenAction::None);
        let bounds = initial
            .platform_output
            .accesskit_update
            .expect("recording settings should expose AccessKit")
            .nodes
            .into_iter()
            .find_map(|(_, node)| {
                (node.role() == egui::accesskit::Role::Slider
                    && node.name() == Some("Speech detection sensitivity"))
                .then(|| node.bounds())
                .flatten()
            })
            .expect("combined slider should expose bounds");
        let point = egui::pos2(
            (bounds.x0 + 98.0) as f32,
            ((bounds.y0 + bounds.y1) * 0.5) as f32,
        );
        let _ = render_recording_settings_with_input(
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
        let (_, pointer_action) = render_recording_settings_with_input(
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
        assert!(matches!(
            pointer_action,
            ScreenAction::SetSpeechDetectionSensitivity(35)
        ));

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
                        memory.request_focus(egui::Id::new(INPUT_LEVEL_SENSITIVITY_CONTROL_ID))
                    });
                });
            },
        );
        let _ = render_recording_settings_with_input(&ctx, &settings_view, Vec::new());
        let (_, keyboard_action) = render_recording_settings_with_input(
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
        assert_eq!(
            keyboard_action,
            ScreenAction::SetSpeechDetectionSensitivity(43)
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
                let history_storage_description = format!(
                    "{} Unavailable while a retained-audio retry owns its history row.",
                    HISTORY_STORAGE_HELP.description
                );
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
                        && node.description() == Some(history_storage_description.as_str())
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
                        (
                            "Speech detection sensitivity",
                            egui::accesskit::Role::Slider,
                        ),
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
                                        && (control.labelled_by().contains(label_id)
                                            || control.name() == Some(*label)))
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
                nodes
                    .iter()
                    .any(|(_, node)| node.name() == Some("Transcript days")),
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
                .filter(|(_, node)| node.name() == Some("Audio days"))
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
