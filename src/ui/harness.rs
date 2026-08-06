//! Development-only deterministic fixtures. Actions update only local fixture state.

use eframe::egui::{self, CentralPanel, Frame, ScrollArea};

use super::{
    configure_accessible_style,
    screens::{RecordingSettingsView, ScreenAction, ScreenView, render_screen},
    shell::{AppPage, show_navigation},
    state::{
        ModelComparisonState, ModelDownloadState, ModelSizeTier, ModelSpeedTier, ModelViewModel,
        SettingsTab, TranscriptionPhase, TranscriptionState, UiRoute,
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
            input_level: 0.45,
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
        recommended,
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
        let action = show_harness(ctx, &self.data, &mut self.page);
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
        comparison: &data.comparison,
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
        ScreenAction::None => {}
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
        ScreenAction::SetSettingsTab(tab) => {
            data.route = UiRoute::Settings(tab);
            *page = AppPage::General;
        }
        ScreenAction::SetRecordingMode(mode) => data.transcription.recording_mode = mode,
        ScreenAction::ToggleProvisionalFeedback => {
            data.settings.provisional_feedback = !data.settings.provisional_feedback
        }
        ScreenAction::RefreshDevices | ScreenAction::ChangeShortcut => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
    fn comparison_fixture_matches_the_pre_run_reference_state() {
        let data = Fixture::ModelsCompareExpanded.data();
        assert!(data.comparison.expanded);
        assert_eq!(data.comparison.selected_model_ids.len(), 2);
        assert_eq!(data.comparison.audio_duration_ms, None);
        assert_eq!(data.comparison.reference_transcript, None);
        assert!(data.comparison.results.is_empty());
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
