//! Development-only deterministic fixtures. Actions update only local fixture state.

use eframe::egui::{self, CentralPanel, Frame, ScrollArea};

use super::{
    configure_accessible_style,
    screens::{RecordingSettingsView, ScreenAction, ScreenView, render_screen},
    shell::{AppPage, show_navigation},
    state::{
        ComparisonPhase, ModelComparisonState, ModelDownloadState, ModelManagementState,
        ModelSizeTier, ModelSpeedTier, ModelViewModel, SettingsTab, TranscriptionPhase,
        TranscriptionState, UiRoute,
    },
    theme_palette,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Fixture {
    TranscribeNoModel,
    TranscribeReady,
    TranscribeListening,
    TranscribeFinalizing,
    TranscribeNoSpeech,
    TranscribeMicrophoneError,
    ModelsInstalled,
    ModelsCompareExpanded,
    SettingsRecording,
}

impl Fixture {
    #[cfg(test)]
    pub(crate) const ALL: [Self; 9] = [
        Self::TranscribeNoModel,
        Self::TranscribeReady,
        Self::TranscribeListening,
        Self::TranscribeFinalizing,
        Self::TranscribeNoSpeech,
        Self::TranscribeMicrophoneError,
        Self::ModelsInstalled,
        Self::ModelsCompareExpanded,
        Self::SettingsRecording,
    ];
    pub(crate) fn parse(value: &str) -> Option<Self> {
        Some(match value.trim() {
            "transcribe/no-model" => Self::TranscribeNoModel,
            "transcribe/ready" => Self::TranscribeReady,
            "transcribe/listening" => Self::TranscribeListening,
            "transcribe/finalizing" => Self::TranscribeFinalizing,
            "transcribe/no-speech" => Self::TranscribeNoSpeech,
            "transcribe/microphone-error" => Self::TranscribeMicrophoneError,
            "models/installed" => Self::ModelsInstalled,
            "models/compare-expanded" => Self::ModelsCompareExpanded,
            "settings/recording" => Self::SettingsRecording,
            _ => return None,
        })
    }
    fn page(self) -> AppPage {
        match self {
            Self::ModelsInstalled | Self::ModelsCompareExpanded => AppPage::Models,
            Self::SettingsRecording => AppPage::General,
            _ => AppPage::Transcribe,
        }
    }
    fn data(self) -> FixtureData {
        let mut transcription = TranscriptionState { selected_model_id: Some("base.en".into()), hotkey: "Ctrl + Space".into(), committed_transcript: "Today’s meeting notes regarding the local-first architecture. We discussed the importance of privacy and keeping all model inference on this device.".into(), elapsed_ms: 8_000, ..Default::default() };
        let models = vec![
            model("Base English", "base.en", true, true, 400),
            model("Tiny English", "tiny.en", false, false, 75),
        ];
        let mut comparison = ModelComparisonState::default();
        let settings = RecordingSettingsView {
            duration_label: "30 seconds".into(),
            provisional_feedback: true,
            device_label: "Microphone (fifine Microphone)".into(),
            ..Default::default()
        };
        let route = match self {
            Self::ModelsInstalled | Self::ModelsCompareExpanded => UiRoute::Models,
            Self::SettingsRecording => UiRoute::Settings(SettingsTab::Recording),
            _ => UiRoute::Transcribe,
        };
        match self {
            Self::TranscribeNoModel => {
                transcription.phase = TranscriptionPhase::NoModel;
                transcription.selected_model_id = None;
                transcription.committed_transcript.clear();
            }
            Self::TranscribeReady => transcription.phase = TranscriptionPhase::Ready,
            Self::TranscribeListening => {
                transcription.phase = TranscriptionPhase::Listening;
                transcription.provisional_transcript =
                    "…we discussed the importance of privacy and keeping all…".into();
            }
            Self::TranscribeFinalizing => transcription.phase = TranscriptionPhase::Finalizing,
            Self::TranscribeNoSpeech => {
                transcription.phase = TranscriptionPhase::NoSpeech;
                transcription.notice = Some("No speech detected — nothing was added.".into());
            }
            Self::TranscribeMicrophoneError => {
                transcription.phase = TranscriptionPhase::MicrophoneError;
                transcription.notice = Some("Scribe couldn’t access your microphone".into());
            }
            Self::ModelsInstalled | Self::SettingsRecording => {
                transcription.phase = TranscriptionPhase::Ready
            }
            Self::ModelsCompareExpanded => {
                transcription.phase = TranscriptionPhase::Ready;
                comparison.expanded = true;
                comparison.selected_model_ids =
                    models.iter().map(|model| model.id.clone()).collect();
            }
        }
        FixtureData {
            route,
            transcription,
            models,
            comparison,
            model_management: ModelManagementState::default(),
            settings,
        }
    }
}

fn model(
    display_name: &str,
    variant_label: &str,
    active: bool,
    recommended: bool,
    ram_mb: u64,
) -> ModelViewModel {
    ModelViewModel {
        id: variant_label.into(),
        display_name: display_name.into(),
        variant_label: variant_label.into(),
        installed: true,
        active,
        ready: true,
        recommended,
        primary_action_label: if active { "Active" } else { "Use" }.into(),
        primary_action_enabled: !active,
        runtime_status_label: "Ready".into(),
        download_state: ModelDownloadState::Installed,
        disk_bytes: Some(ram_mb * 1_000_000),
        estimated_ram_bytes: Some(ram_mb * 1_000_000),
        language_summary: "English".into(),
        speed_tier: if active {
            ModelSpeedTier::Balanced
        } else {
            ModelSpeedTier::VeryFast
        },
        size_tier: if active {
            ModelSizeTier::Base
        } else {
            ModelSizeTier::Tiny
        },
        ..Default::default()
    }
}

#[derive(Clone)]
struct FixtureData {
    route: UiRoute,
    transcription: TranscriptionState,
    models: Vec<ModelViewModel>,
    comparison: ModelComparisonState,
    model_management: ModelManagementState,
    settings: RecordingSettingsView,
}
pub(crate) fn fixture_from_env() -> Option<Fixture> {
    std::env::var("SCRIBE_UI_HARNESS")
        .ok()
        .and_then(|value| Fixture::parse(&value))
}

pub(crate) struct UiHarnessApp {
    page: AppPage,
    data: FixtureData,
}
impl UiHarnessApp {
    pub(crate) fn new(cc: &eframe::CreationContext<'_>, fixture: Fixture) -> Self {
        configure_accessible_style(&cc.egui_ctx);
        Self {
            page: fixture.page(),
            data: fixture.data(),
        }
    }
}
impl eframe::App for UiHarnessApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let clear_reference_editor_focus = self.data.comparison.focus_reference_editor;
        let clear_reference_action_focus = self.data.comparison.restore_reference_action_focus;
        let clear_reference_notice = self.data.comparison.reference_notice.is_some();
        let action = show_harness(ctx, &self.data, &mut self.page);
        if clear_reference_editor_focus {
            self.data.comparison.focus_reference_editor = false;
        }
        if clear_reference_action_focus {
            self.data.comparison.restore_reference_action_focus = false;
        }
        if clear_reference_notice {
            self.data.comparison.reference_notice = None;
        }
        apply_action(&mut self.data, &mut self.page, action);
        ctx.request_repaint_after(std::time::Duration::from_secs(60));
    }
}

fn harness_route(page: AppPage, fixture_route: UiRoute) -> UiRoute {
    match page {
        AppPage::Transcribe => UiRoute::Transcribe,
        AppPage::Models => UiRoute::Models,
        AppPage::General | AppPage::Advanced => match fixture_route {
            UiRoute::Settings(tab) => UiRoute::Settings(tab),
            _ => UiRoute::Settings(SettingsTab::Recording),
        },
        AppPage::History => UiRoute::History,
        AppPage::About => UiRoute::About,
        AppPage::Debug => UiRoute::Debug,
    }
}
fn show_harness(ctx: &egui::Context, data: &FixtureData, page: &mut AppPage) -> ScreenAction {
    show_navigation(ctx, page, false);
    let view = ScreenView {
        route: harness_route(*page, data.route),
        transcription: &data.transcription,
        models: &data.models,
        model_catalog: &data.models,
        comparison: &data.comparison,
        model_management: &data.model_management,
        recording_settings: &data.settings,
    };
    CentralPanel::default()
        .frame(
            Frame::none()
                .fill(theme_palette(ctx).content_bg)
                .inner_margin(egui::Margin::same(28.0)),
        )
        .show(ctx, |ui| {
            ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| render_screen(ui, &view))
                .inner
        })
        .inner
}

fn apply_action(data: &mut FixtureData, page: &mut AppPage, action: ScreenAction) {
    match action {
        ScreenAction::None
        | ScreenAction::SelectModel(_)
        | ScreenAction::InstallModel(_)
        | ScreenAction::CancelModelInstall(_)
        | ScreenAction::RepairModelRuntime(_)
        | ScreenAction::MaintainModelRuntime(_)
        | ScreenAction::ShowModelDetails(_)
        | ScreenAction::RequestModelRemoval(_)
        | ScreenAction::ConfirmModelRemoval(_)
        | ScreenAction::CloseModelDialog => {}
        ScreenAction::AddModel | ScreenAction::ChangeModel => {
            data.transcription.selected_model_id = Some("base.en".into());
            data.transcription.phase = TranscriptionPhase::Ready;
        }
        ScreenAction::StartRecording => data.transcription.phase = TranscriptionPhase::Listening,
        ScreenAction::StopRecording => data.transcription.phase = TranscriptionPhase::Finalizing,
        ScreenAction::OpenAudioSettings => *page = AppPage::General,
        ScreenAction::RetryMicrophone => data.transcription.phase = TranscriptionPhase::Listening,
        ScreenAction::ClearTranscript => data.transcription.committed_transcript.clear(),
        ScreenAction::CopyTranscript => {}
        ScreenAction::ToggleComparison => data.comparison.expanded = !data.comparison.expanded,
        ScreenAction::ToggleComparisonModel(id) => {
            if !data.comparison.selected_model_ids.insert(id.clone()) {
                data.comparison.selected_model_ids.remove(&id);
            }
        }
        ScreenAction::StartComparison => {
            let _ = data.comparison.begin();
        }
        ScreenAction::StopComparison => data.comparison.phase = ComparisonPhase::Processing,
        ScreenAction::ShowComparisonReferenceEditor => {
            if let Some(reference) = data.comparison.reference_transcript.as_deref() {
                data.comparison.reference_draft = reference.to_owned();
            }
            data.comparison.reference_editor_visible = true;
            data.comparison.focus_reference_editor = true;
            data.comparison.restore_reference_action_focus = false;
        }
        ScreenAction::HideComparisonReferenceEditor => {
            data.comparison.reference_draft = data
                .comparison
                .reference_transcript
                .clone()
                .unwrap_or_default();
            data.comparison.reference_editor_visible = false;
            data.comparison.focus_reference_editor = false;
            data.comparison.restore_reference_action_focus = true;
        }
        ScreenAction::EditComparisonReference(reference) => {
            data.comparison.reference_draft = reference
        }
        ScreenAction::ApplyComparisonReference => {
            let reference = data.comparison.reference_draft.trim().to_owned();
            data.comparison.reference_draft = reference.clone();
            data.comparison.reference_transcript = (!reference.is_empty()).then_some(reference);
            data.comparison.reference_editor_visible = false;
            data.comparison.focus_reference_editor = false;
            data.comparison.restore_reference_action_focus = true;
            data.comparison.reference_notice = Some("Reference transcript applied.".to_owned());
        }
        ScreenAction::ClearComparisonReference => {
            data.comparison.reference_draft.clear();
            data.comparison.reference_transcript = None;
            data.comparison.reference_editor_visible = false;
            data.comparison.focus_reference_editor = false;
            data.comparison.restore_reference_action_focus = true;
            data.comparison.reference_notice = Some("Reference transcript cleared.".to_owned());
        }
        ScreenAction::SetSettingsTab(tab) => {
            data.route = UiRoute::Settings(tab);
            *page = AppPage::General;
        }
        ScreenAction::SetCloseToTray(value) => data.settings.close_to_tray = value,
        ScreenAction::OpenModelSettings => *page = AppPage::Models,
        ScreenAction::SetHotkeyInput(value) => data.transcription.hotkey = value,
        ScreenAction::ApplyHotkey => {}
        ScreenAction::SetTheme(value) => data.settings.theme_label = value,
        ScreenAction::SetOverlayMode(value) => data.settings.overlay_label = value,
        ScreenAction::SetRecordingMode(mode) => data.transcription.recording_mode = mode,
        ScreenAction::SetDurationSeconds(seconds) => {
            data.settings.duration_seconds = seconds;
            data.settings.duration_label = format!("{seconds} seconds");
        }
        ScreenAction::ToggleProvisionalFeedback => {
            data.settings.provisional_feedback = !data.settings.provisional_feedback
        }
        ScreenAction::SetAudioDevice(device) => {
            data.settings.selected_audio_device = device.clone();
            data.settings.device_label = device.unwrap_or_else(|| "OS default".into());
        }
        ScreenAction::SetInputSensitivity(percent) => {
            data.settings.input_sensitivity_percent = percent;
        }
        ScreenAction::RefreshDevices | ScreenAction::ChangeShortcut => {}
        ScreenAction::SetAutoInsertTranscript(value) => {
            data.settings.auto_insert_transcript = value
        }
        ScreenAction::SetRestoreClipboardAfterInsert(value) => {
            data.settings.restore_clipboard_after_insert = value
        }
        ScreenAction::SetPasteDelayMs(value) => data.settings.paste_delay_ms = value,
        ScreenAction::SetVadEnabled(value) => data.settings.vad_enabled = value,
        ScreenAction::SetSpeechConfirmationMs(value) => {
            data.settings.speech_confirmation_ms = value
        }
        ScreenAction::SetInternalPauseMs(value) => data.settings.internal_pause_ms = value,
        ScreenAction::SetEndpointSilenceMs(value) => data.settings.endpoint_silence_ms = value,
        ScreenAction::SetPreRollMs(value) => data.settings.pre_roll_ms = value,
        ScreenAction::SetPostRollMs(value) => data.settings.post_roll_ms = value,
        ScreenAction::SetStreamingMode(value) => data.settings.streaming_label = value,
        ScreenAction::SetAcceleration(value) => data.settings.acceleration_label = value,
        ScreenAction::SetOverlayPosition(value) => data.settings.overlay_position_label = value,
        ScreenAction::SetDebugMode(value) => data.settings.debug_mode = value,
        ScreenAction::SetHistoryMode(value) => data.settings.history_mode_label = value,
        ScreenAction::SetMaxHistoryEntries(value) => data.settings.max_history_entries = value,
        ScreenAction::SetTranscriptRetentionDays(value) => {
            data.settings.transcript_retention_days = value
        }
        ScreenAction::SetAudioRetentionDays(value) => data.settings.audio_retention_days = value,
        ScreenAction::SetStoreApplicationIdentity(value) => {
            data.settings.store_application_identity = value
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LAYOUT_TOLERANCE: f64 = 1.0;

    fn render(fixture: Fixture, width: f32, height: f32) -> egui::FullOutput {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        configure_accessible_style(&ctx);
        let mut page = fixture.page();
        let data = fixture.data();
        ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::Vec2::new(width, height),
                )),
                ..Default::default()
            },
            |ctx| {
                let _ = show_harness(ctx, &data, &mut page);
            },
        )
    }

    fn render_with_input(
        ctx: &egui::Context,
        data: &mut FixtureData,
        page: &mut AppPage,
        width: f32,
        height: f32,
        events: Vec<egui::Event>,
    ) -> (egui::FullOutput, ScreenAction) {
        let clear_reference_editor_focus = data.comparison.focus_reference_editor;
        let clear_reference_action_focus = data.comparison.restore_reference_action_focus;
        let clear_reference_notice = data.comparison.reference_notice.is_some();
        let mut action = ScreenAction::None;
        let output = ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::Vec2::new(width, height),
                )),
                events,
                ..Default::default()
            },
            |ctx| action = show_harness(ctx, data, page),
        );
        if clear_reference_editor_focus {
            data.comparison.focus_reference_editor = false;
        }
        if clear_reference_action_focus {
            data.comparison.restore_reference_action_focus = false;
        }
        if clear_reference_notice {
            data.comparison.reference_notice = None;
        }
        (output, action)
    }

    fn named_node_bounds(output: &egui::FullOutput, name: &str) -> egui::accesskit::Rect {
        output
            .platform_output
            .accesskit_update
            .as_ref()
            .expect("render should expose an AccessKit update")
            .nodes
            .iter()
            .find_map(|(_, node)| (node.name() == Some(name)).then(|| node.bounds()).flatten())
            .unwrap_or_else(|| panic!("missing AccessKit bounds for {name}"))
    }

    fn named_node_id(output: &egui::FullOutput, name: &str) -> egui::accesskit::NodeId {
        output
            .platform_output
            .accesskit_update
            .as_ref()
            .expect("render should expose an AccessKit update")
            .nodes
            .iter()
            .find_map(|(id, node)| (node.name() == Some(name)).then_some(*id))
            .unwrap_or_else(|| panic!("missing AccessKit node for {name}"))
    }

    fn click_named_control(
        ctx: &egui::Context,
        data: &mut FixtureData,
        page: &mut AppPage,
        width: f32,
        height: f32,
        name: &str,
    ) -> ScreenAction {
        let (initial_output, initial_action) =
            render_with_input(ctx, data, page, width, height, Vec::new());
        assert_eq!(initial_action, ScreenAction::None);
        let bounds = named_node_bounds(&initial_output, name);
        let point = egui::pos2(
            ((bounds.x0 + bounds.x1) / 2.0) as f32,
            ((bounds.y0 + bounds.y1) / 2.0) as f32,
        );
        let (_, press_action) = render_with_input(
            ctx,
            data,
            page,
            width,
            height,
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
        let (_, release_action) = render_with_input(
            ctx,
            data,
            page,
            width,
            height,
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
        release_action
    }

    fn node_matching(
        output: &egui::FullOutput,
        predicate: impl Fn(&egui::accesskit::Node) -> bool,
    ) -> &egui::accesskit::Node {
        output
            .platform_output
            .accesskit_update
            .as_ref()
            .expect("render should expose an AccessKit update")
            .nodes
            .iter()
            .map(|(_, node)| node)
            .find(|node| predicate(node))
            .expect("expected AccessKit node")
    }

    fn focused_node(output: &egui::FullOutput) -> &egui::accesskit::Node {
        let update = output
            .platform_output
            .accesskit_update
            .as_ref()
            .expect("render should expose an AccessKit update");
        update
            .nodes
            .iter()
            .find(|(id, _)| *id == update.focus)
            .map(|(_, node)| node)
            .expect("focused control should remain in the accessibility tree")
    }

    fn assert_polite_atomic_notice(output: &egui::FullOutput, expected: &str) {
        assert!(
            output
                .platform_output
                .accesskit_update
                .as_ref()
                .unwrap()
                .nodes
                .iter()
                .any(|(_, node)| {
                    node.name() == Some(expected)
                        && node.live() == Some(egui::accesskit::Live::Polite)
                        && node.is_live_atomic()
                })
        );
    }

    fn assert_near(actual: f64, expected: f64, label: &str) {
        assert!(
            (actual - expected).abs() <= LAYOUT_TOLERANCE,
            "{label}: expected {expected} ± {LAYOUT_TOLERANCE}, got {actual}"
        );
    }

    fn node_names(output: &egui::FullOutput) -> Vec<String> {
        output
            .platform_output
            .accesskit_update
            .as_ref()
            .unwrap()
            .nodes
            .iter()
            .filter_map(|(_, node)| node.name().map(str::to_owned))
            .collect()
    }
    #[test]
    fn every_fixture_renders_at_native_preferred_and_minimum_dimensions() {
        for fixture in Fixture::ALL {
            for (width, height) in [(1180.0, 815.0), (960.0, 680.0)] {
                let output = render(fixture, width, height);
                assert!(
                    output
                        .shapes
                        .iter()
                        .all(|shape| shape.clip_rect.max.x <= width
                            && shape.clip_rect.min.x >= 0.0
                            && shape.clip_rect.max.y <= height
                            && shape.clip_rect.min.y >= 0.0)
                );
            }
        }
    }
    #[test]
    fn every_fixture_exposes_its_visible_reference_content() {
        for (fixture, expected) in [
            (
                Fixture::TranscribeNoModel,
                "Add a speech model to start transcribing",
            ),
            (Fixture::TranscribeReady, "Start recording"),
            (Fixture::TranscribeListening, "Listening"),
            (Fixture::TranscribeFinalizing, "Finalizing transcript…"),
            (
                Fixture::TranscribeNoSpeech,
                "No speech detected — nothing was added.",
            ),
            (
                Fixture::TranscribeMicrophoneError,
                "Scribe couldn’t access your microphone",
            ),
            (Fixture::ModelsInstalled, "Compare installed models"),
            (Fixture::ModelsCompareExpanded, "No data"),
            (Fixture::SettingsRecording, "Recording behavior"),
        ] {
            assert!(
                node_names(&render(fixture, 1180.0, 815.0))
                    .iter()
                    .any(|name| name.contains(expected)),
                "{fixture:?} missing {expected}"
            );
        }
    }
    #[test]
    fn comparison_panel_stays_near_the_bottom_without_infinite_scroll_spacing() {
        for (fixture, minimum_top) in [
            (Fixture::ModelsInstalled, 500.0),
            (Fixture::ModelsCompareExpanded, 430.0),
        ] {
            let output = render(fixture, 1180.0, 815.0);
            let bounds = output
                .platform_output
                .accesskit_update
                .unwrap()
                .nodes
                .iter()
                .find_map(|(_, node)| {
                    (node.name() == Some("Compare installed models"))
                        .then(|| node.bounds())
                        .flatten()
                })
                .expect("comparison heading should expose finite geometry");
            assert!(
                bounds.y0 >= minimum_top && bounds.y1 <= 815.0,
                "{fixture:?} comparison bounds were {bounds:?}"
            );
        }
    }

    #[test]
    fn model_comparison_surface_matches_main_content_width_at_preferred_and_compact_sizes() {
        for (width, height) in [(1180.0, 815.0), (960.0, 680.0)] {
            for (fixture, toggle_name) in [
                (Fixture::ModelsInstalled, "Expand comparison"),
                (Fixture::ModelsCompareExpanded, "Collapse comparison"),
            ] {
                let output = render(fixture, width, height);
                let surface_node = node_matching(&output, |node| {
                    node.name() == Some("Model comparison surface")
                });
                assert_eq!(surface_node.role(), egui::accesskit::Role::Group);
                let surface = surface_node
                    .bounds()
                    .expect("comparison surface should expose bounds");
                let models = node_matching(&output, |node| {
                    node.role() == egui::accesskit::Role::Heading && node.name() == Some("Models")
                })
                .bounds()
                .expect("Models heading should expose bounds");
                let add_models_node = node_matching(&output, |node| {
                    node.name().is_some_and(|name| name.contains("Add models"))
                });
                assert_eq!(add_models_node.role(), egui::accesskit::Role::Button);
                let add_models = add_models_node
                    .bounds()
                    .expect("Add models should expose bounds");
                let chevron = named_node_bounds(&output, toggle_name);

                assert_near(
                    surface.x0,
                    models.x0,
                    "surface left should align with Models heading",
                );
                assert_near(
                    surface.x1,
                    add_models.x1,
                    "surface right should align with Add models",
                );
                assert_near(
                    chevron.x1,
                    surface.x1 - 16.0,
                    "chevron should align with the surface inner right edge",
                );
            }
        }
    }

    #[test]
    fn comparison_chevron_pointer_and_accesskit_actions_toggle_once() {
        let (width, height) = (1180.0, 815.0);
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        configure_accessible_style(&ctx);
        let mut data = Fixture::ModelsInstalled.data();
        let mut page = Fixture::ModelsInstalled.page();

        let action = click_named_control(
            &ctx,
            &mut data,
            &mut page,
            width,
            height,
            "Expand comparison",
        );
        assert_eq!(action, ScreenAction::ToggleComparison);
        apply_action(&mut data, &mut page, action);
        assert!(data.comparison.expanded);
        assert_eq!(
            render_with_input(&ctx, &mut data, &mut page, width, height, Vec::new()).1,
            ScreenAction::None
        );

        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        configure_accessible_style(&ctx);
        let mut data = Fixture::ModelsInstalled.data();
        let mut page = Fixture::ModelsInstalled.page();
        let (output, action) =
            render_with_input(&ctx, &mut data, &mut page, width, height, Vec::new());
        assert_eq!(action, ScreenAction::None);
        let chevron = named_node_id(&output, "Expand comparison");
        let (_, action) = render_with_input(
            &ctx,
            &mut data,
            &mut page,
            width,
            height,
            vec![egui::Event::AccessKitActionRequest(
                egui::accesskit::ActionRequest {
                    action: egui::accesskit::Action::Default,
                    target: chevron,
                    data: None,
                },
            )],
        );
        assert_eq!(action, ScreenAction::ToggleComparison);
        apply_action(&mut data, &mut page, action);
        assert!(data.comparison.expanded);
        assert_eq!(
            render_with_input(&ctx, &mut data, &mut page, width, height, Vec::new()).1,
            ScreenAction::None
        );
    }

    #[test]
    fn comparison_header_pointer_toggles_without_creating_an_accessible_header_button() {
        let (width, height) = (1180.0, 815.0);
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        configure_accessible_style(&ctx);
        let mut data = Fixture::ModelsInstalled.data();
        let mut page = Fixture::ModelsInstalled.page();

        let (initial_output, initial_action) =
            render_with_input(&ctx, &mut data, &mut page, width, height, Vec::new());
        assert_eq!(initial_action, ScreenAction::None);
        assert!(!data.comparison.expanded);

        let heading = named_node_bounds(&initial_output, "Compare installed models");
        let chevron = named_node_bounds(&initial_output, "Expand comparison");
        let click_point = egui::pos2(
            heading.x0 as f32 + 1.0,
            ((heading.y0 + heading.y1) / 2.0) as f32,
        );
        assert!(
            (click_point.x as f64) >= heading.x0
                && (click_point.x as f64) <= heading.x1
                && (click_point.y as f64) >= heading.y0
                && (click_point.y as f64) <= heading.y1
        );
        assert!(
            (click_point.x as f64) < chevron.x0
                || (click_point.x as f64) > chevron.x1
                || (click_point.y as f64) < chevron.y0
                || (click_point.y as f64) > chevron.y1,
            "header click point must not overlap the chevron"
        );
        let chevron_node = node_matching(&initial_output, |node| {
            node.name() == Some("Expand comparison")
        });
        assert_eq!(chevron_node.role(), egui::accesskit::Role::Button);
        assert_eq!(chevron_node.is_expanded(), Some(false));
        assert!(
            !initial_output
                .platform_output
                .accesskit_update
                .as_ref()
                .unwrap()
                .nodes
                .iter()
                .any(|(_, node)| {
                    node.role() == egui::accesskit::Role::Button
                        && node.name() == Some("Compare installed models")
                }),
            "the pointer-only header must not become a second accessible button"
        );

        let (press_output, press_action) = render_with_input(
            &ctx,
            &mut data,
            &mut page,
            width,
            height,
            vec![
                egui::Event::PointerMoved(click_point),
                egui::Event::PointerButton {
                    pos: click_point,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
        assert_eq!(press_action, ScreenAction::None);
        assert!(!data.comparison.expanded);
        drop(press_output);

        let (release_output, release_action) = render_with_input(
            &ctx,
            &mut data,
            &mut page,
            width,
            height,
            vec![
                egui::Event::PointerMoved(click_point),
                egui::Event::PointerButton {
                    pos: click_point,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
        assert_eq!(release_action, ScreenAction::ToggleComparison);
        apply_action(&mut data, &mut page, release_action);
        assert!(data.comparison.expanded);

        let (expanded_output, expanded_action) =
            render_with_input(&ctx, &mut data, &mut page, width, height, Vec::new());
        assert_eq!(expanded_action, ScreenAction::None);
        let expanded_chevron = node_matching(&expanded_output, |node| {
            node.name() == Some("Collapse comparison")
        });
        assert_eq!(expanded_chevron.role(), egui::accesskit::Role::Button);
        assert_eq!(expanded_chevron.is_expanded(), Some(true));
        drop(release_output);
    }

    #[test]
    fn comparison_fixture_matches_the_pre_run_reference_state() {
        let data = Fixture::ModelsCompareExpanded.data();
        assert!(data.comparison.expanded);
        assert!(!data.comparison.reference_editor_visible);
        assert_eq!(data.comparison.selected_model_ids.len(), 2);
        assert_eq!(data.comparison.audio_duration_ms, None);
        assert_eq!(data.comparison.reference_transcript, None);
        assert!(data.comparison.results.is_empty());
    }

    #[test]
    fn comparison_reference_editor_requires_an_explicit_add_action() {
        let (width, height) = (1180.0, 815.0);
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        configure_accessible_style(&ctx);
        let mut data = Fixture::ModelsCompareExpanded.data();
        let mut page = Fixture::ModelsCompareExpanded.page();

        let (output, action) =
            render_with_input(&ctx, &mut data, &mut page, width, height, Vec::new());
        assert_eq!(action, ScreenAction::None);
        let update = output.platform_output.accesskit_update.as_ref().unwrap();
        assert!(
            !update
                .nodes
                .iter()
                .any(|(_, node)| node.name() == Some("Reference transcript"))
        );
        assert!(update.nodes.iter().any(|(_, node)| {
            node.role() == egui::accesskit::Role::Button
                && node.name() == Some("Add a reference transcript to measure")
        }));

        let action = click_named_control(
            &ctx,
            &mut data,
            &mut page,
            width,
            height,
            "Add a reference transcript to measure",
        );
        assert_eq!(action, ScreenAction::ShowComparisonReferenceEditor);
        apply_action(&mut data, &mut page, action);
        assert!(data.comparison.reference_editor_visible);
        let (output, action) =
            render_with_input(&ctx, &mut data, &mut page, width, height, Vec::new());
        assert_eq!(action, ScreenAction::None);
        assert_eq!(focused_node(&output).name(), Some("Reference transcript"));
        assert!(!data.comparison.focus_reference_editor);
        assert!(
            output
                .platform_output
                .accesskit_update
                .as_ref()
                .unwrap()
                .nodes
                .iter()
                .any(|(_, node)| node.name() == Some("Reference transcript"))
        );
    }

    #[test]
    fn comparison_reference_focus_and_feedback_follow_conditional_controls() {
        let (width, height) = (1180.0, 815.0);
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        configure_accessible_style(&ctx);
        let mut data = Fixture::ModelsCompareExpanded.data();
        let mut page = Fixture::ModelsCompareExpanded.page();

        apply_action(
            &mut data,
            &mut page,
            ScreenAction::ShowComparisonReferenceEditor,
        );
        let (output, action) =
            render_with_input(&ctx, &mut data, &mut page, width, height, Vec::new());
        assert_eq!(action, ScreenAction::None);
        assert_eq!(focused_node(&output).name(), Some("Reference transcript"));
        assert!(!data.comparison.focus_reference_editor);
        assert!(
            output
                .platform_output
                .accesskit_update
                .as_ref()
                .unwrap()
                .nodes
                .iter()
                .any(|(_, node)| {
                    node.name() == Some("Apply reference")
                        && node.is_disabled()
                        && node.description()
                            == Some("Enter a reference transcript before applying it.")
                })
        );

        apply_action(
            &mut data,
            &mut page,
            ScreenAction::EditComparisonReference("spoken words".into()),
        );
        apply_action(&mut data, &mut page, ScreenAction::ApplyComparisonReference);
        assert!(data.comparison.restore_reference_action_focus);
        let (output, action) =
            render_with_input(&ctx, &mut data, &mut page, width, height, Vec::new());
        assert_eq!(action, ScreenAction::None);
        assert_eq!(focused_node(&output).name(), Some("Edit reference"));
        assert_polite_atomic_notice(&output, "Reference transcript applied.");
        assert!(!data.comparison.restore_reference_action_focus);
        assert_eq!(data.comparison.reference_notice, None);

        apply_action(
            &mut data,
            &mut page,
            ScreenAction::ShowComparisonReferenceEditor,
        );
        let _ = render_with_input(&ctx, &mut data, &mut page, width, height, Vec::new());
        apply_action(&mut data, &mut page, ScreenAction::ClearComparisonReference);
        let (output, action) =
            render_with_input(&ctx, &mut data, &mut page, width, height, Vec::new());
        assert_eq!(action, ScreenAction::None);
        assert_eq!(
            focused_node(&output).description(),
            Some("Add a reference transcript to measure accuracy for whisper.cpp base.en.")
        );
        assert_polite_atomic_notice(&output, "Reference transcript cleared.");
        assert!(!data.comparison.restore_reference_action_focus);
        assert_eq!(data.comparison.reference_notice, None);
        apply_action(
            &mut data,
            &mut page,
            ScreenAction::ShowComparisonReferenceEditor,
        );
        let _ = render_with_input(&ctx, &mut data, &mut page, width, height, Vec::new());
        apply_action(
            &mut data,
            &mut page,
            ScreenAction::HideComparisonReferenceEditor,
        );
        let (output, action) =
            render_with_input(&ctx, &mut data, &mut page, width, height, Vec::new());
        assert_eq!(action, ScreenAction::None);
        assert_eq!(
            focused_node(&output).description(),
            Some("Add a reference transcript to measure accuracy for whisper.cpp base.en.")
        );
        assert_eq!(data.comparison.reference_notice, None);

        data.comparison.reference_transcript = Some("spoken words".into());
        data.comparison.reference_draft = "spoken words".into();
        apply_action(
            &mut data,
            &mut page,
            ScreenAction::ShowComparisonReferenceEditor,
        );
        let _ = render_with_input(&ctx, &mut data, &mut page, width, height, Vec::new());
        apply_action(
            &mut data,
            &mut page,
            ScreenAction::EditComparisonReference("unsaved change".into()),
        );
        apply_action(
            &mut data,
            &mut page,
            ScreenAction::HideComparisonReferenceEditor,
        );
        let (output, action) =
            render_with_input(&ctx, &mut data, &mut page, width, height, Vec::new());
        assert_eq!(action, ScreenAction::None);
        assert_eq!(focused_node(&output).name(), Some("Edit reference"));
        assert_eq!(data.comparison.reference_draft, "spoken words");
        assert_eq!(data.comparison.reference_notice, None);
        assert!(!data.comparison.restore_reference_action_focus);
    }

    #[test]
    fn comparison_reference_actions_keep_draft_and_visibility_coherent() {
        let mut data = Fixture::ModelsCompareExpanded.data();
        let mut page = Fixture::ModelsCompareExpanded.page();

        apply_action(
            &mut data,
            &mut page,
            ScreenAction::ShowComparisonReferenceEditor,
        );
        apply_action(
            &mut data,
            &mut page,
            ScreenAction::EditComparisonReference("  spoken words  ".into()),
        );
        apply_action(&mut data, &mut page, ScreenAction::ApplyComparisonReference);
        assert_eq!(
            data.comparison.reference_transcript.as_deref(),
            Some("spoken words")
        );
        assert_eq!(data.comparison.reference_draft, "spoken words");
        assert!(!data.comparison.reference_editor_visible);

        apply_action(
            &mut data,
            &mut page,
            ScreenAction::ShowComparisonReferenceEditor,
        );
        apply_action(
            &mut data,
            &mut page,
            ScreenAction::EditComparisonReference("unsaved change".into()),
        );
        apply_action(
            &mut data,
            &mut page,
            ScreenAction::HideComparisonReferenceEditor,
        );
        assert_eq!(data.comparison.reference_draft, "spoken words");
        assert!(!data.comparison.reference_editor_visible);

        apply_action(
            &mut data,
            &mut page,
            ScreenAction::ShowComparisonReferenceEditor,
        );
        apply_action(&mut data, &mut page, ScreenAction::ClearComparisonReference);
        assert_eq!(data.comparison.reference_transcript, None);
        assert!(data.comparison.reference_draft.is_empty());
        assert!(!data.comparison.reference_editor_visible);
    }

    #[test]
    fn comparison_results_expose_wide_table_and_compact_groups() {
        let wide = render(Fixture::ModelsCompareExpanded, 1180.0, 815.0);
        let wide_nodes = &wide
            .platform_output
            .accesskit_update
            .as_ref()
            .unwrap()
            .nodes;
        for heading in ["Model", "Duration", "Processing time", "Output", "Accuracy"] {
            assert!(wide_nodes.iter().any(|(_, node)| {
                node.role() == egui::accesskit::Role::ColumnHeader && node.name() == Some(heading)
            }));
        }

        let compact = render(Fixture::ModelsCompareExpanded, 680.0, 815.0);
        let compact_nodes = &compact
            .platform_output
            .accesskit_update
            .as_ref()
            .unwrap()
            .nodes;
        for model in ["whisper.cpp base.en", "whisper.cpp tiny.en"] {
            assert!(compact_nodes.iter().any(|(_, node)| {
                node.role() == egui::accesskit::Role::Group
                    && node.name() == Some(format!("Comparison result for {model}").as_str())
            }));
        }
        assert_eq!(
            compact_nodes
                .iter()
                .filter(|(_, node)| {
                    node.role() == egui::accesskit::Role::Button
                        && node.name() == Some("Add a reference transcript to measure")
                })
                .count(),
            2
        );
        assert!(!compact_nodes.iter().any(|(_, node)| {
            node.role() == egui::accesskit::Role::ColumnHeader && node.name() == Some("Model")
        }));
    }

    #[test]
    fn initial_expanded_comparison_table_fits_the_reference_viewport() {
        let output = render(Fixture::ModelsCompareExpanded, 1180.0, 815.0);
        let update = output.platform_output.accesskit_update.as_ref().unwrap();
        let accuracy_actions: Vec<_> = update
            .nodes
            .iter()
            .filter(|(_, node)| {
                node.role() == egui::accesskit::Role::Button
                    && node.name() == Some("Add a reference transcript to measure")
            })
            .collect();
        assert_eq!(accuracy_actions.len(), 2);
        assert!(
            accuracy_actions
                .iter()
                .all(|(_, node)| { node.bounds().is_some_and(|bounds| bounds.y1 <= 815.0) })
        );
        for name in [
            "Model",
            "Duration",
            "Processing time",
            "Output",
            "Accuracy",
            "Add a reference transcript to measure",
        ] {
            let bounds = named_node_bounds(&output, name);
            assert!(
                bounds.y1 <= 815.0,
                "{name} should remain visible at 1180x815, but ended at {}",
                bounds.y1
            );
        }
    }
    #[test]
    fn harness_actions_mutate_only_visible_fixture_state() {
        let mut data = Fixture::TranscribeReady.data();
        let mut page = AppPage::Transcribe;
        apply_action(&mut data, &mut page, ScreenAction::StartRecording);
        assert_eq!(data.transcription.phase, TranscriptionPhase::Listening);
        apply_action(&mut data, &mut page, ScreenAction::StopRecording);
        assert_eq!(data.transcription.phase, TranscriptionPhase::Finalizing);
        apply_action(&mut data, &mut page, ScreenAction::ClearTranscript);
        assert!(data.transcription.committed_transcript.is_empty());
    }
    #[test]
    fn settings_tab_action_persists_the_selected_route() {
        let mut data = Fixture::SettingsRecording.data();
        let mut page = AppPage::General;
        apply_action(
            &mut data,
            &mut page,
            ScreenAction::SetSettingsTab(SettingsTab::Output),
        );
        assert_eq!(data.route, UiRoute::Settings(SettingsTab::Output));
        assert_eq!(
            harness_route(page, data.route),
            UiRoute::Settings(SettingsTab::Output)
        );
    }
    #[test]
    fn comparison_toggle_persists_across_frames() {
        let mut data = Fixture::ModelsInstalled.data();
        let mut page = AppPage::Models;
        apply_action(&mut data, &mut page, ScreenAction::ToggleComparison);
        assert!(data.comparison.expanded);
        apply_action(&mut data, &mut page, ScreenAction::ToggleComparison);
        assert!(!data.comparison.expanded);
    }
    #[test]
    fn icon_only_fixture_controls_have_accesskit_names() {
        let names = node_names(&render(Fixture::SettingsRecording, 960.0, 680.0));
        assert!(names.iter().any(|name| name == "Refresh devices"));
    }
    #[test]
    fn harness_parser_is_exact_and_fail_closed() {
        assert_eq!(
            Fixture::parse("transcribe/ready"),
            Some(Fixture::TranscribeReady)
        );
        assert_eq!(Fixture::parse("debug"), None);
    }
}
