//! Development-only fixture selection; shared screens live in `ui::screens`.

use std::collections::BTreeSet;

use eframe::egui::{self, CentralPanel, Frame, ScrollArea};

use super::{
    configure_accessible_style,
    screens::{RecordingSettingsView, ScreenView, show_screen},
    shell::{AppPage, show_navigation},
    state::{
        ComparisonPhase, ModelComparisonState, ModelDownloadState, ModelSizeTier, ModelSpeedTier,
        ModelViewModel, SettingsTab, TranscriptionPhase, TranscriptionState, UiRoute,
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
            model("whisper.cpp base.en", "base.en", true, true, 400),
            model("whisper.cpp tiny.en", "tiny.en", false, false, 75),
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
                    " …we discussed the importance of privacy and keeping all…".into();
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
                comparison.phase = ComparisonPhase::Complete;
                comparison.selected_model_ids = models
                    .iter()
                    .map(|model| model.id.clone())
                    .collect::<BTreeSet<_>>();
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
    fixture: Fixture,
    page: AppPage,
}
impl UiHarnessApp {
    pub(crate) fn new(cc: &eframe::CreationContext<'_>, fixture: Fixture) -> Self {
        configure_accessible_style(&cc.egui_ctx);
        Self {
            fixture,
            page: fixture.page(),
        }
    }
}
impl eframe::App for UiHarnessApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        show_harness(ctx, self.fixture, &mut self.page);
        ctx.request_repaint_after(std::time::Duration::from_secs(60));
    }
}
pub(crate) fn show_harness(ctx: &egui::Context, fixture: Fixture, page: &mut AppPage) {
    show_navigation(ctx, page, false);
    let data = fixture.data();
    let route = match *page {
        AppPage::Transcribe => UiRoute::Transcribe,
        AppPage::Models => UiRoute::Models,
        AppPage::General | AppPage::Advanced => UiRoute::Settings(SettingsTab::Recording),
        AppPage::History => UiRoute::History,
        AppPage::About => UiRoute::About,
        AppPage::Debug => UiRoute::Debug,
    };
    let view = ScreenView {
        route: if route == data.route {
            data.route
        } else {
            route
        },
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
                .show(ui, |ui| show_screen(ui, &view));
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    fn render(fixture: Fixture, width: f32) -> egui::FullOutput {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        configure_accessible_style(&ctx);
        let mut page = fixture.page();
        ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::Vec2::new(width, 680.0),
                )),
                ..Default::default()
            },
            |ctx| show_harness(ctx, fixture, &mut page),
        )
    }
    #[test]
    fn every_fixture_renders_wide_and_compact_without_painting_outside_the_viewport() {
        for fixture in Fixture::ALL {
            for width in [1180.0, 960.0] {
                let output = render(fixture, width);
                assert!(output.shapes.iter().all(|shape| shape.clip_rect.max.x <= width && shape.clip_rect.min.x >= 0.0));
            }
        }
    }
    #[test]
    fn icon_only_fixture_controls_have_accesskit_names() {
        let output = render(Fixture::SettingsRecording, 960.0);
        let update = output.platform_output.accesskit_update.unwrap();
        assert!(
            update
                .nodes
                .iter()
                .any(|(_, node)| node.role() == egui::accesskit::Role::Button
                    && node.name() == Some("Refresh devices"))
        );
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
