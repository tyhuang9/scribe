//! Development-only deterministic fixtures. Actions update only local fixture state.

use eframe::egui::{self, CentralPanel, Frame};
use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use crate::{
    config::SpeechDetectionMode,
    history::{HistoryMetrics, HistoryRecord, HistoryStatus},
    overlay::{
        self, OverlayAudioLevel, OverlayMode, OverlayPhase, OverlayPosition, OverlayPresentation,
        OverlayTranscript, OverlayViewState,
    },
    transcription::SessionId,
};

use super::{
    HistoryPageAction, HistoryPageState, ThemePalette, configure_accessible_style, history_page,
    model_picker::ReadyModelPickerAction,
    screens::{RecordingSettingsView, ScreenAction, ScreenView, render_screen, show_route_scroll},
    shell::{AppPage, SidebarModelView, show_navigation},
    state::{
        ComparisonPhase, ModelCapabilities, ModelCardKey, ModelComparisonState, ModelDialog,
        ModelDownloadState, ModelLanguageFilter, ModelManagementState, ModelSizeTier,
        ModelSpeedTier, ModelViewModel, RemoteCatalogActionKind, RemoteCatalogActionView,
        RemoteCatalogEntryView, RemoteCatalogStatusKind, RemoteCatalogStatusView,
        RemoteCatalogVariantView, RemoteCatalogView, ResolvedTheme, SettingsTab, TranscribeNotice,
        TranscriptionPhase, TranscriptionState, UiRoute,
    },
    theme_palette,
};

#[cfg(test)]
use super::state::{ComparisonResult, ComparisonResultPhase};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Fixture {
    TranscribeNoModel,
    TranscribeReady,
    TranscribeListening,
    TranscribeFinalizing,
    TranscribeNoSpeech,
    TranscribeMicrophoneError,
    DemoAudio,
    ModelsInstalled,
    ModelsLifecycle,
    ModelsDownloadDownloading,
    ModelsDownloadRetained,
    ModelsDownloadFailedPartial,
    ModelsDownloadFailedAlert,
    ModelsCardIdle,
    ModelsCardFocus,
    ModelsCardExpanded,
    ModelsCompareExpanded,
    History,
    HistoryDetails,
    HistoryConfirmation,
    HistoryDark,
    SettingsRecording,
    OverlayLiveLight,
    OverlayLiveDark,
    OverlayCompactLight,
    OverlayCompactDark,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum HarnessTheme {
    Light,
    Dark,
}

impl HarnessTheme {
    fn parse(value: &str) -> Option<Self> {
        match value.trim() {
            "light" => Some(Self::Light),
            "dark" => Some(Self::Dark),
            _ => None,
        }
    }

    fn dark_mode(self) -> bool {
        self == Self::Dark
    }

    fn label(self) -> &'static str {
        match self {
            Self::Light => "Light",
            Self::Dark => "Dark",
        }
    }

    fn from_settings_label(label: &str) -> Self {
        if label == "Dark" {
            Self::Dark
        } else {
            Self::Light
        }
    }
}

impl Fixture {
    #[cfg(test)]
    pub(crate) const ALL: [Self; 21] = [
        Self::TranscribeNoModel,
        Self::TranscribeReady,
        Self::TranscribeListening,
        Self::TranscribeFinalizing,
        Self::TranscribeNoSpeech,
        Self::TranscribeMicrophoneError,
        Self::ModelsInstalled,
        Self::ModelsLifecycle,
        Self::ModelsDownloadDownloading,
        Self::ModelsDownloadRetained,
        Self::ModelsDownloadFailedPartial,
        Self::ModelsDownloadFailedAlert,
        Self::ModelsCardIdle,
        Self::ModelsCardFocus,
        Self::ModelsCardExpanded,
        Self::ModelsCompareExpanded,
        Self::History,
        Self::HistoryDetails,
        Self::HistoryConfirmation,
        Self::HistoryDark,
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
            "demo/audio" => Self::DemoAudio,
            "models/installed" => Self::ModelsInstalled,
            "models/lifecycle" => Self::ModelsLifecycle,
            "models/download-downloading" => Self::ModelsDownloadDownloading,
            "models/download-retained" => Self::ModelsDownloadRetained,
            "models/download-failed-partial" => Self::ModelsDownloadFailedPartial,
            "models/download-failed-alert" => Self::ModelsDownloadFailedAlert,
            "models/card-idle" => Self::ModelsCardIdle,
            "models/card-focus" => Self::ModelsCardFocus,
            "models/card-expanded" => Self::ModelsCardExpanded,
            "models/compare-expanded" => Self::ModelsCompareExpanded,
            "history" => Self::History,
            "history/details" => Self::HistoryDetails,
            "history/confirmation" => Self::HistoryConfirmation,
            "history/dark" => Self::HistoryDark,
            "settings/recording" => Self::SettingsRecording,
            "overlay/live-light" => Self::OverlayLiveLight,
            "overlay/live-dark" => Self::OverlayLiveDark,
            "overlay/compact-light" => Self::OverlayCompactLight,
            "overlay/compact-dark" => Self::OverlayCompactDark,
            _ => return None,
        })
    }

    fn overlay(self) -> Option<OverlayHarnessFixture> {
        let (mode, dark_mode) = match self {
            Self::OverlayLiveLight => (OverlayMode::Live, false),
            Self::OverlayLiveDark => (OverlayMode::Live, true),
            Self::OverlayCompactLight => (OverlayMode::Minimal, false),
            Self::OverlayCompactDark => (OverlayMode::Minimal, true),
            _ => return None,
        };
        Some(OverlayHarnessFixture {
            dark_mode,
            state: OverlayViewState {
                session_id: Some(SessionId(42)),
                mode,
                phase: OverlayPhase::Listening,
                live_preview_available: mode == OverlayMode::Live,
                audio_level: OverlayAudioLevel::new(0.58, 0.78),
                transcript: OverlayTranscript {
                    committed: "Alright, What is going on? Why is there a line on".to_owned(),
                    tentative: "That's pretty cool. These newest words stay visible.".to_owned(),
                    revision: 1,
                },
                elapsed: Some(Duration::from_secs(10)),
                ..OverlayViewState::default()
            },
        })
    }

    fn page(self) -> AppPage {
        match self {
            Self::ModelsInstalled
            | Self::ModelsLifecycle
            | Self::ModelsDownloadDownloading
            | Self::ModelsDownloadRetained
            | Self::ModelsDownloadFailedPartial
            | Self::ModelsDownloadFailedAlert
            | Self::ModelsCardIdle
            | Self::ModelsCardFocus
            | Self::ModelsCardExpanded
            | Self::ModelsCompareExpanded => AppPage::Models,
            Self::History
            | Self::HistoryDetails
            | Self::HistoryConfirmation
            | Self::HistoryDark => AppPage::History,
            Self::SettingsRecording => AppPage::General,
            _ => AppPage::Transcribe,
        }
    }
    fn data(self) -> FixtureData {
        let mut transcription = TranscriptionState {
            selected_model_id: Some("base.en".into()),
            hotkey: "Ctrl + Space".into(),
            committed_transcript: "Today's meeting notes regarding the local-first architecture. We discussed the importance of privacy and keeping all model inference on the user's machine to ensure zero data leakage. The performance of the small models is acceptable for dictation, but we might need to explore quantized larger models for complex technical jargon.".into(),
            elapsed_ms: 8_000,
            last_successful_capture_ms: Some(120_000),
            ..Default::default()
        };
        let mut models = vec![
            model("whisper.cpp base.en", "base.en", true, true, 400),
            model("whisper.cpp tiny.en", "tiny.en", false, false, 75),
        ];
        let mut model_catalog = Vec::new();
        let mut comparison = ModelComparisonState::default();
        let settings = RecordingSettingsView {
            duration_label: "30 seconds".into(),
            provisional_feedback: true,
            device_label: "Microphone (fifine Microphone)".into(),
            voice_detection_mode: SpeechDetectionMode::ManualThreshold,
            input_threshold_dbfs: -42.0,
            input_level_percent: 68,
            ..Default::default()
        };
        let route = match self {
            Self::ModelsInstalled
            | Self::ModelsLifecycle
            | Self::ModelsDownloadDownloading
            | Self::ModelsDownloadRetained
            | Self::ModelsDownloadFailedPartial
            | Self::ModelsDownloadFailedAlert
            | Self::ModelsCardIdle
            | Self::ModelsCardFocus
            | Self::ModelsCardExpanded
            | Self::ModelsCompareExpanded => UiRoute::Models,
            Self::History
            | Self::HistoryDetails
            | Self::HistoryConfirmation
            | Self::HistoryDark => UiRoute::History,
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
                transcription.notice = Some(TranscribeNotice::information(
                    "No speech detected — nothing was added.",
                ));
            }
            Self::TranscribeMicrophoneError => {
                transcription.phase = TranscriptionPhase::MicrophoneError;
                transcription.notice = Some(TranscribeNotice::information(
                    "Scribe couldn’t access your microphone",
                ));
            }
            Self::DemoAudio => unreachable!("demo/audio is initialized from an audio file"),
            Self::ModelsInstalled
            | Self::SettingsRecording
            | Self::History
            | Self::HistoryDetails
            | Self::HistoryConfirmation
            | Self::HistoryDark
            | Self::OverlayLiveLight
            | Self::OverlayLiveDark
            | Self::OverlayCompactLight
            | Self::OverlayCompactDark => transcription.phase = TranscriptionPhase::Ready,
            Self::ModelsLifecycle
            | Self::ModelsDownloadDownloading
            | Self::ModelsDownloadRetained
            | Self::ModelsDownloadFailedPartial
            | Self::ModelsDownloadFailedAlert
            | Self::ModelsCardIdle
            | Self::ModelsCardFocus
            | Self::ModelsCardExpanded => {
                transcription.phase = TranscriptionPhase::Ready;
                let mut partial = model("Whisper Moonshine", "moonshine.base", false, false, 190);
                partial.installed = false;
                partial.download_state = ModelDownloadState::Cancelled;
                partial.downloaded_bytes = 129_000_000;
                partial.total_bytes = Some(190_000_000);
                partial.partial_cleanup_available = true;
                partial.partial_cleanup_enabled = true;
                partial.description = Some("Resumable local transcription model.".into());
                partial.languages = vec!["en".into()];

                let mut downloading = model("Whisper Parakeet", "parakeet.tdt", false, false, 600);
                downloading.installed = false;
                downloading.download_state = ModelDownloadState::Downloading;
                downloading.downloaded_bytes = 82_000_000;
                downloading.total_bytes = Some(600_000_000);
                downloading.cancel_supported = true;
                downloading.description = Some("Fast local transcription model.".into());
                downloading.languages = vec!["en".into(), "es".into()];

                let mut failed = model("Whisper Medium", "medium.en", false, false, 466);
                failed.installed = false;
                failed.download_state = ModelDownloadState::Failed;
                failed.error_message = Some("Network connection was interrupted.".into());
                failed.description = Some("High-accuracy local transcription model.".into());
                failed.languages = vec!["en".into()];

                let mut failed_with_partial = failed.clone();
                failed_with_partial.id = "medium-retained.en".into();
                failed_with_partial.variant_label = "medium-retained.en".into();
                failed_with_partial.display_name = "Whisper Medium retained".into();
                failed_with_partial.partial_cleanup_available = true;
                failed_with_partial.partial_cleanup_enabled = true;
                failed_with_partial.downloaded_bytes = 241_000_000;
                failed_with_partial.total_bytes = Some(466_000_000);

                let mut available = model("Whisper Large", "large-v3", false, false, 1_550);
                available.installed = false;
                available.download_state = ModelDownloadState::NotInstalled;
                available.description = Some("Highest-accuracy local transcription model.".into());
                available.languages = vec![
                    "en".into(),
                    "es".into(),
                    "ja".into(),
                    "ko".into(),
                    "zh".into(),
                    "fr".into(),
                ];

                model_catalog = match self {
                    Self::ModelsDownloadDownloading => vec![downloading],
                    Self::ModelsDownloadRetained => vec![partial],
                    Self::ModelsDownloadFailedPartial => vec![failed_with_partial],
                    Self::ModelsDownloadFailedAlert => vec![failed],
                    Self::ModelsCardIdle | Self::ModelsCardFocus => Vec::new(),
                    _ => vec![partial, downloading, failed, available],
                };
                if self == Self::ModelsCardFocus {
                    models[1].primary_action_label = "Use this model".into();
                    models[1].primary_action_enabled = true;
                }
                if self == Self::ModelsCardExpanded {
                    let expanded = models
                        .iter_mut()
                        .find(|model| model.id == "tiny.en")
                        .expect("expanded fixture includes tiny.en");
                    expanded.description = Some(
                        "A compact local model for responsive dictation, long recordings, and offline language-aware transcription."
                            .into(),
                    );
                    expanded.languages = vec!["en".into(), "es".into(), "ja".into()];
                    expanded.capabilities = ModelCapabilities {
                        capabilities_known: true,
                        batch_transcription: true,
                        native_streaming: true,
                        cancellation: true,
                        timestamps: true,
                        translation: true,
                        language_detection: true,
                        confidence_scores: true,
                        custom_vocabulary: true,
                        cpu: true,
                        gpu: true,
                    };
                    expanded.runtime_action_label = Some("Repair".into());
                    expanded.runtime_action_enabled = true;
                }
            }
            Self::ModelsCompareExpanded => {
                transcription.phase = TranscriptionPhase::Ready;
                comparison.expanded = true;
                comparison.selected_model_ids =
                    models.iter().map(|model| model.id.clone()).collect();
            }
        }
        let history_records = self
            .is_history()
            .then(history_fixture_records)
            .unwrap_or_default();
        let history_expanded_transcripts =
            if matches!(self, Self::HistoryDetails | Self::HistoryDark) {
                HashSet::from([1])
            } else {
                HashSet::new()
            };
        let history_expanded_details = if matches!(self, Self::HistoryDetails | Self::HistoryDark) {
            HashSet::from([1, 2])
        } else {
            HashSet::new()
        };
        FixtureData {
            route,
            transcription,
            theme_announcement: None,
            models,
            comparison,
            model_management: if self == Self::ModelsCardExpanded {
                ModelManagementState {
                    expanded_model_card: Some(ModelCardKey::Local("tiny.en".into())),
                    ..Default::default()
                }
            } else {
                ModelManagementState::default()
            },
            model_catalog,
            model_language_filter: ModelLanguageFilter::default(),
            remote_catalog: remote_catalog_fixture(),
            settings,
            settings_playground_open: false,
            history_search: String::new(),
            history_records,
            history_confirm_delete: (self == Self::HistoryConfirmation).then_some(2),
            history_playing: matches!(self, Self::HistoryDetails | Self::HistoryDark).then_some(1),
            history_armed_repaste: None,
            history_model_names: HashMap::from([(
                "whisper_cpp_base_en".to_owned(),
                "Whisper Base — English".to_owned(),
            )]),
            history_expanded_transcripts,
            history_expanded_details,
            history_focus_delete_confirmation: self == Self::HistoryConfirmation,
            history_focus_more_action: None,
        }
    }

    fn is_history(self) -> bool {
        matches!(
            self,
            Self::History | Self::HistoryDetails | Self::HistoryConfirmation | Self::HistoryDark
        )
    }

    fn dark_mode(self) -> bool {
        matches!(
            self,
            Self::HistoryDark | Self::OverlayLiveDark | Self::OverlayCompactDark
        )
    }
}

fn history_fixture_records() -> Vec<HistoryRecord> {
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64;
    let long_transcript = "Scribe kept this long meeting transcript entirely on this device. "
        .repeat(8)
        .trim()
        .to_owned();
    vec![
        HistoryRecord {
            id: 1,
            created_at_ms: now_ms - 120_000,
            updated_at_ms: now_ms - 110_000,
            completed_at_ms: Some(now_ms - 110_000),
            status: HistoryStatus::Completed,
            raw_text: format!("Raw capture: {long_transcript}"),
            final_text: Some(long_transcript),
            model_id: "whisper_cpp_base_en".to_owned(),
            metrics: HistoryMetrics {
                audio_duration_ms: Some(42_300),
                processing_duration_ms: Some(2_800),
                realtime_factor: Some(0.07),
            },
            pinned: true,
            source_app: Some("Notes".to_owned()),
            audio_path: Some(PathBuf::from("fixture-completed.wav")),
            failure: None,
            retry_count: 0,
            output_outcome: Some("pasted_safely".to_owned()),
        },
        HistoryRecord {
            id: 2,
            created_at_ms: now_ms - 3_600_000,
            updated_at_ms: now_ms - 3_590_000,
            completed_at_ms: None,
            status: HistoryStatus::Failed,
            raw_text: "A recoverable partial transcript from retained audio.".to_owned(),
            final_text: None,
            model_id: "custom-removed-model".to_owned(),
            metrics: HistoryMetrics {
                audio_duration_ms: Some(18_000),
                processing_duration_ms: None,
                realtime_factor: None,
            },
            pinned: false,
            source_app: Some("Mail".to_owned()),
            audio_path: Some(PathBuf::from("fixture-failed.wav")),
            failure: Some("The local model stopped before transcription completed.".to_owned()),
            retry_count: 1,
            output_outcome: None,
        },
        HistoryRecord {
            id: 3,
            created_at_ms: now_ms - 8_000,
            updated_at_ms: now_ms - 8_000,
            completed_at_ms: None,
            status: HistoryStatus::Pending,
            raw_text: String::new(),
            final_text: None,
            model_id: "whisper_cpp_base_en".to_owned(),
            metrics: HistoryMetrics::default(),
            pinned: false,
            source_app: None,
            audio_path: None,
            failure: None,
            retry_count: 0,
            output_outcome: None,
        },
    ]
}

#[derive(Clone, Debug, PartialEq)]
struct OverlayHarnessFixture {
    dark_mode: bool,
    state: OverlayViewState,
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
        selected: active,
        active,
        ready: true,
        recommended,
        primary_action_label: if active { "Active" } else { "Use this model" }.into(),
        primary_action_enabled: !active,
        primary_action_disabled_reason: active.then(|| "This model is already active.".to_owned()),
        removal_supported: true,
        runtime_status_label: "Ready".into(),
        download_state: ModelDownloadState::Installed,
        description: Some(
            if active {
                "Balanced for everyday dictation."
            } else {
                "More accurate for longer recordings."
            }
            .into(),
        ),
        disk_bytes: Some(ram_mb * 1_000_000),
        total_bytes: Some(ram_mb * 1_000_000),
        estimated_ram_bytes: Some(ram_mb * 1_000_000),
        languages: vec!["en".into()],
        language_summary: "English".into(),
        speed_tier: if active {
            ModelSpeedTier::Balanced
        } else {
            ModelSpeedTier::VeryFast
        },
        accuracy_guidance: if active { "Balanced" } else { "Basic" }.into(),
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
    theme_announcement: Option<String>,
    models: Vec<ModelViewModel>,
    model_catalog: Vec<ModelViewModel>,
    comparison: ModelComparisonState,
    model_management: ModelManagementState,
    model_language_filter: ModelLanguageFilter,
    remote_catalog: RemoteCatalogView,
    settings: RecordingSettingsView,
    settings_playground_open: bool,
    history_search: String,
    history_records: Vec<HistoryRecord>,
    history_confirm_delete: Option<i64>,
    history_playing: Option<i64>,
    history_armed_repaste: Option<i64>,
    history_model_names: HashMap<String, String>,
    history_expanded_transcripts: HashSet<i64>,
    history_expanded_details: HashSet<i64>,
    history_focus_delete_confirmation: bool,
    history_focus_more_action: Option<i64>,
}

fn remote_catalog_fixture() -> RemoteCatalogView {
    RemoteCatalogView {
        local_import: super::state::LocalGgufImportView {
            path: String::new(),
            in_progress: false,
            import_enabled: true,
            disabled_reason: None,
            status_message: None,
        },
        status: RemoteCatalogStatusView {
            kind: RemoteCatalogStatusKind::Available,
            message: "Cached trusted catalog · Showing 1 of 1 trusted catalog models.".into(),
        },
        refresh_enabled: true,
        has_snapshot: true,
        entries: vec![RemoteCatalogEntryView {
            id: "trusted-speech/compact-english".into(),
            display_name: "Compact English".into(),
            description: "A compact English speech recognition candidate.".into(),
            languages: vec!["English".into()],
            trust_label: "Trusted publisher".into(),
            compatibility_detail: "Cross-platform compatibility is still being validated.".into(),
            repository: "trusted-speech/compact-english".into(),
            pinned_revision: "1111111111111111111111111111111111111111".into(),
            variants: vec![RemoteCatalogVariantView {
                id: "compact-english-q5".into(),
                filename: "compact-english-q5.gguf".into(),
                size_label: "82 MB".into(),
                status_label: Some("Pinned GGUF".into()),
                expected_sha256: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                    .into(),
                actions: vec![RemoteCatalogActionView {
                    label: "Install".into(),
                    kind: RemoteCatalogActionKind::Install {
                        remote_model_id: "trusted-speech/compact-english".into(),
                        variant_id: "compact-english-q5".into(),
                    },
                    enabled: true,
                    disabled_reason: None,
                }],
                ..Default::default()
            }],
            ..Default::default()
        }],
        ..Default::default()
    }
}
pub(crate) fn fixture_from_env() -> Option<Fixture> {
    std::env::var("SCRIBE_UI_HARNESS")
        .ok()
        .and_then(|value| Fixture::parse(&value))
}

fn harness_theme_from_env() -> Option<HarnessTheme> {
    std::env::var("SCRIBE_UI_HARNESS_THEME")
        .ok()
        .and_then(|value| HarnessTheme::parse(&value))
}

pub(crate) struct UiHarnessApp {
    page: AppPage,
    data: FixtureData,
    overlay: Option<OverlayHarnessFixture>,
    overlay_presented: bool,
    demo_playback: Option<DemoPlayback>,
}

struct DemoPlayback {
    started_at: Instant,
    transcript: String,
}

fn configure_harness_style(ctx: &egui::Context, dark_mode: bool) {
    ctx.set_visuals(ThemePalette::visuals(dark_mode));
    configure_accessible_style(ctx);
}

impl UiHarnessApp {
    pub(crate) fn new(cc: &eframe::CreationContext<'_>, fixture: Fixture) -> Self {
        let overlay = fixture.overlay();
        let theme =
            if fixture.dark_mode() || overlay.as_ref().is_some_and(|fixture| fixture.dark_mode) {
                HarnessTheme::Dark
            } else if overlay.is_some() {
                HarnessTheme::Light
            } else {
                harness_theme_from_env().unwrap_or(HarnessTheme::Light)
            };
        configure_harness_style(&cc.egui_ctx, theme.dark_mode());
        if overlay.is_some() {
            cc.egui_ctx.send_viewport_cmd(egui::ViewportCommand::Title(
                "Scribe Overlay Fixture Background".to_owned(),
            ));
            cc.egui_ctx
                .send_viewport_cmd(egui::ViewportCommand::Maximized(true));
        }
        let mut data = fixture.data();
        data.settings.theme_label = theme.label().into();
        Self {
            page: fixture.page(),
            data,
            overlay,
            overlay_presented: false,
            demo_playback: None,
        }
    }

    pub(crate) fn new_demo_audio(cc: &eframe::CreationContext<'_>, transcript: String) -> Self {
        let mut app = Self::new(cc, Fixture::TranscribeReady);
        app.data.transcription.committed_transcript.clear();
        app.data.transcription.notice = Some(TranscribeNotice::information(
            "Demo playback — transcript generated locally from a prerecorded audio file.",
        ));
        app.demo_playback = Some(DemoPlayback {
            started_at: Instant::now(),
            transcript,
        });
        app
    }

    fn advance_demo_playback(&mut self) {
        let Some(demo) = self.demo_playback.as_ref() else {
            return;
        };
        let elapsed = demo.started_at.elapsed();
        self.data.transcription.elapsed_ms = elapsed.as_millis() as u64;
        self.data.transcription.phase = if elapsed < Duration::from_millis(1_600) {
            self.data.transcription.provisional_transcript = demo
                .transcript
                .split_whitespace()
                .take(5)
                .collect::<Vec<_>>()
                .join(" ");
            TranscriptionPhase::Listening
        } else if elapsed < Duration::from_millis(2_500) {
            self.data.transcription.provisional_transcript.clear();
            TranscriptionPhase::Finalizing
        } else {
            self.data.transcription.provisional_transcript.clear();
            self.data.transcription.committed_transcript = demo.transcript.clone();
            self.data.transcription.last_successful_capture_ms = Some(0);
            TranscriptionPhase::Ready
        };
    }
}
impl eframe::App for UiHarnessApp {
    fn clear_color(&self, visuals: &egui::Visuals) -> [f32; 4] {
        ThemePalette::from_visuals(visuals)
            .content_bg
            .to_normalized_gamma_f32()
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.advance_demo_playback();
        if let Some(overlay) = &self.overlay {
            show_overlay_fixture_host(ctx, self.overlay_presented);
            let output = overlay::show_overlay_viewport(
                ctx,
                &overlay.state,
                None,
                OverlayPosition::BottomCenter,
                OverlayPresentation {
                    focused: Some(false),
                    minimized: false,
                    hidden_to_tray: false,
                },
            );
            self.overlay_presented = output.presented;
            // Immediate child viewports are driven by the root update loop.
            // Keep the patterned host and real hardened overlay repaintable
            // throughout an extended capture session.
            ctx.request_repaint_after(Duration::from_millis(33));
            return;
        }
        let clear_initial_dialog_focus = self.data.model_management.focus_dialog_initial;
        let clear_add_focus = self.data.model_management.restore_add_focus;
        let clear_reference_editor_focus = self.data.comparison.focus_reference_editor;
        let clear_comparison_focus = self.data.comparison.focus_panel;
        let clear_reference_action_focus = self.data.comparison.restore_reference_action_focus;
        let clear_reference_notice = self.data.comparison.reference_notice.is_some();
        let clear_after_removal_focus = self.data.model_management.restore_after_removal_focus;
        let action = show_harness(ctx, &mut self.data, &mut self.page);
        if clear_reference_editor_focus {
            self.data.comparison.focus_reference_editor = false;
        }
        if clear_comparison_focus {
            self.data.comparison.focus_panel = false;
        }
        if clear_reference_action_focus {
            self.data.comparison.restore_reference_action_focus = false;
        }
        if clear_reference_notice {
            self.data.comparison.reference_notice = None;
        }
        if clear_initial_dialog_focus {
            self.data.model_management.focus_dialog_initial = false;
        }
        if clear_add_focus {
            self.data.model_management.restore_add_focus = false;
        }
        if clear_after_removal_focus {
            self.data.model_management.restore_after_removal_focus = false;
        }
        apply_harness_action(ctx, &mut self.data, &mut self.page, action);
        ctx.request_repaint_after(if self.demo_playback.is_some() {
            Duration::from_millis(33)
        } else {
            Duration::from_secs(60)
        });
    }
}

fn apply_harness_action(
    ctx: &egui::Context,
    data: &mut FixtureData,
    page: &mut AppPage,
    action: ScreenAction,
) {
    let changes_theme = matches!(
        &action,
        ScreenAction::SetTheme(_) | ScreenAction::ToggleResolvedTheme(_)
    );
    apply_action(data, page, action);
    if changes_theme {
        let theme = HarnessTheme::from_settings_label(&data.settings.theme_label);
        configure_harness_style(ctx, theme.dark_mode());
    }
}

fn show_overlay_fixture_host(ctx: &egui::Context, overlay_presented: bool) {
    CentralPanel::default()
        .frame(Frame::none().fill(theme_palette(ctx).content_bg))
        .show(ctx, |ui| {
            let viewport = ui.max_rect();
            let sample_top = (viewport.bottom() - 180.0).max(viewport.top());
            let sample_area = egui::Rect::from_min_max(
                egui::pos2(viewport.left(), sample_top),
                viewport.right_bottom(),
            );
            let panel_width = 150.0;
            let dark_palette = ThemePalette::dark();
            let panels = [
                (
                    crate::branding::ICE_MIST,
                    crate::branding::DEEP_INK,
                    "LIGHT SAMPLE",
                ),
                (dark_palette.content_bg, dark_palette.text, "DARK SAMPLE"),
                (
                    dark_palette.accent,
                    dark_palette.primary_button_text,
                    "SCRIBE TEAL",
                ),
            ];
            let panel_count = (sample_area.width() / panel_width).ceil() as usize;
            for index in 0..panel_count {
                let left = sample_area.left() + index as f32 * panel_width;
                let panel = egui::Rect::from_min_max(
                    egui::pos2(left, sample_area.top()),
                    egui::pos2(
                        (left + panel_width).min(sample_area.right()),
                        sample_area.bottom(),
                    ),
                );
                let (fill, text, label) = panels[index % panels.len()];
                ui.painter().rect_filled(panel, 0.0, fill);
                for row in 0..3 {
                    ui.painter().text(
                        egui::pos2(panel.center().x, panel.top() + 30.0 + row as f32 * 54.0),
                        egui::Align2::CENTER_CENTER,
                        label,
                        egui::FontId::monospace(12.0),
                        text,
                    );
                }
            }
            if !overlay_presented {
                ui.centered_and_justified(|ui| {
                    ui.label("Preparing the hardened overlay capture fixture...");
                });
            }
        });
}

fn harness_route(page: AppPage, fixture_route: UiRoute) -> UiRoute {
    match page {
        AppPage::Transcribe => UiRoute::Transcribe,
        AppPage::Models => UiRoute::Models,
        AppPage::General | AppPage::Advanced | AppPage::About => match fixture_route {
            UiRoute::Settings(SettingsTab::Output) => UiRoute::Settings(SettingsTab::General),
            UiRoute::Settings(tab) => UiRoute::Settings(tab),
            _ => UiRoute::Settings(SettingsTab::Recording),
        },
        AppPage::History => UiRoute::History,
        AppPage::Debug => UiRoute::Debug,
    }
}

fn render_settings_playground_fixture(ui: &mut egui::Ui) -> ScreenAction {
    ui.heading("Settings");
    ui.add_space(12.0);
    ui.heading("Developer Playground");
    ui.label("Compare local model output without leaving Settings.");
    ui.add_space(12.0);
    if ui
        .add_sized([176.0, 44.0], egui::Button::new("Back to Advanced"))
        .clicked()
    {
        ScreenAction::SetSettingsTab(SettingsTab::Advanced)
    } else {
        ScreenAction::None
    }
}

fn show_harness(ctx: &egui::Context, data: &mut FixtureData, page: &mut AppPage) -> ScreenAction {
    let resolved_theme = if ctx.style().visuals.dark_mode {
        ResolvedTheme::Dark
    } else {
        ResolvedTheme::Light
    };
    let (theme_action, model_action) = show_navigation(
        ctx,
        page,
        false,
        resolved_theme,
        SidebarModelView {
            selected_model_id: data.transcription.selected_model_id.as_deref(),
            models: &data.model_catalog,
            disabled_reason: data.transcription.model_change_disabled_reason.as_deref(),
        },
    );
    let navigation_action = model_action.map(|action| match action {
        ReadyModelPickerAction::Select(id) => ScreenAction::SelectQuickModel(id),
        ReadyModelPickerAction::ManageModels => ScreenAction::OpenModelSettings,
    });
    if let Some(message) = data.theme_announcement.take() {
        paint_theme_change_status(ctx, &message);
    }
    if *page != AppPage::General || !matches!(data.route, UiRoute::Settings(_)) {
        data.settings_playground_open = false;
    }
    if *page == AppPage::History {
        let page_action = CentralPanel::default()
            .frame(Frame::none().fill(theme_palette(ctx).content_bg))
            .show(ctx, |ui| {
                show_route_scroll(ui, UiRoute::History, |ui| {
                    let heading = ui.label(
                        egui::RichText::new("History")
                            .font(egui::FontId::proportional(30.0))
                            .color(theme_palette(ui.ctx()).primary)
                            .strong(),
                    );
                    ui.ctx().accesskit_node_builder(heading.id, |builder| {
                        builder.set_role(egui::accesskit::Role::Heading);
                    });
                    ui.add_space(14.0);
                    history_page(
                        ui,
                        HistoryPageState {
                            search: &mut data.history_search,
                            records: &data.history_records,
                            has_more: false,
                            loading: false,
                            error: None,
                            confirm_delete: data.history_confirm_delete,
                            work_active: false,
                            playing: data.history_playing,
                            playback_stopping: false,
                            armed_repaste: data.history_armed_repaste,
                            model_names: &data.history_model_names,
                            expanded_transcripts: &data.history_expanded_transcripts,
                            expanded_details: &data.history_expanded_details,
                            focus_search: false,
                            focus_delete_confirmation: data.history_focus_delete_confirmation,
                            focus_more_action: data.history_focus_more_action,
                        },
                    )
                })
            })
            .inner;
        if let Some(action) = page_action {
            apply_history_fixture_action(data, action);
        }
        return if theme_action != ScreenAction::None {
            theme_action
        } else {
            navigation_action.unwrap_or(ScreenAction::None)
        };
    }
    let view = ScreenView {
        route: harness_route(*page, data.route),
        transcription: &data.transcription,
        models: &data.models,
        model_catalog: &data.model_catalog,
        comparison: &data.comparison,
        model_management: &data.model_management,
        model_language_filter: data.model_language_filter,
        remote_catalog: &data.remote_catalog,
        recording_settings: &data.settings,
    };
    let screen_action = CentralPanel::default()
        .frame(Frame::none().fill(theme_palette(ctx).content_bg))
        .show(ctx, |ui| {
            show_route_scroll(ui, view.route, |ui| {
                if data.settings_playground_open {
                    render_settings_playground_fixture(ui)
                } else {
                    render_screen(ui, &view)
                }
            })
        })
        .inner;
    if theme_action != ScreenAction::None {
        theme_action
    } else {
        navigation_action.unwrap_or(screen_action)
    }
}

fn apply_history_fixture_action(data: &mut FixtureData, action: HistoryPageAction) {
    match action {
        HistoryPageAction::ApplySearch
        | HistoryPageAction::ClearSearch
        | HistoryPageAction::Refresh
        | HistoryPageAction::LoadMore
        | HistoryPageAction::Copy { .. } => {}
        HistoryPageAction::ArmRepaste { id, .. } => data.history_armed_repaste = Some(id),
        HistoryPageAction::TogglePinned { id, pinned } => {
            if let Some(record) = data
                .history_records
                .iter_mut()
                .find(|record| record.id == id)
            {
                record.pinned = pinned;
            }
            data.history_focus_more_action = Some(id);
        }
        HistoryPageAction::Play(id) => data.history_playing = Some(id),
        HistoryPageAction::StopPlayback => data.history_playing = None,
        HistoryPageAction::Retry(id) => data.history_focus_more_action = Some(id),
        HistoryPageAction::DeleteAudio(id) => {
            if let Some(record) = data
                .history_records
                .iter_mut()
                .find(|record| record.id == id)
            {
                record.audio_path = None;
            }
            data.history_focus_more_action = Some(id);
        }
        HistoryPageAction::RequestDelete(id) => {
            data.history_confirm_delete = Some(id);
            data.history_focus_delete_confirmation = true;
        }
        HistoryPageAction::ConfirmDelete(id) => {
            data.history_records.retain(|record| record.id != id);
            data.history_confirm_delete = None;
            data.history_focus_delete_confirmation = false;
        }
        HistoryPageAction::CancelDelete => {
            data.history_focus_more_action = data.history_confirm_delete.take();
            data.history_focus_delete_confirmation = false;
        }
        HistoryPageAction::ToggleTranscript(id) => {
            toggle_fixture_history_state(&mut data.history_expanded_transcripts, id);
        }
        HistoryPageAction::ToggleDetails(id) => {
            toggle_fixture_history_state(&mut data.history_expanded_details, id);
            data.history_focus_more_action = Some(id);
        }
    }
}

fn toggle_fixture_history_state(entries: &mut HashSet<i64>, id: i64) {
    if !entries.remove(&id) {
        entries.insert(id);
    }
}

fn paint_theme_change_status(ctx: &egui::Context, message: &str) {
    let screen_rect = ctx.screen_rect();
    egui::Area::new(egui::Id::new("harness-theme-change-live-status"))
        .order(egui::Order::Foreground)
        .fixed_pos(egui::pos2(screen_rect.max.x + 1.0, screen_rect.max.y + 1.0))
        .show(ctx, |ui| {
            let response = ui.label(message);
            ui.ctx().accesskit_node_builder(response.id, |builder| {
                builder.set_role(egui::accesskit::Role::Status);
                builder.set_name(message);
                builder.set_live(egui::accesskit::Live::Polite);
                builder.set_live_atomic();
            });
        });
}

fn apply_action(data: &mut FixtureData, page: &mut AppPage, action: ScreenAction) {
    match action {
        ScreenAction::None
        | ScreenAction::InstallModel(_)
        | ScreenAction::UpgradeModel(_)
        | ScreenAction::CancelModelInstall(_)
        | ScreenAction::RepairModelRuntime(_)
        | ScreenAction::MaintainModelRuntime(_)
        | ScreenAction::RetryRemoteCatalog => {}
        ScreenAction::DiscardModelPartial(id) => {
            if let Some(model) = data
                .models
                .iter_mut()
                .chain(data.model_catalog.iter_mut())
                .find(|model| model.id == id)
            {
                model.partial_cleanup_available = false;
                model.partial_cleanup_enabled = false;
                model.partial_cleanup_disabled_reason = None;
                model.download_state = ModelDownloadState::NotInstalled;
            }
        }
        ScreenAction::DiscardRemoteCatalogPartial {
            remote_model_id,
            variant_id,
        } => {
            if let Some(variant) = data
                .remote_catalog
                .entries
                .iter_mut()
                .find(|entry| entry.id == remote_model_id)
                .and_then(|entry| {
                    entry
                        .variants
                        .iter_mut()
                        .find(|variant| variant.id == variant_id)
                })
            {
                variant.actions.retain(|action| {
                    !matches!(action.kind, RemoteCatalogActionKind::DiscardPartial { .. })
                });
            }
        }
        ScreenAction::SelectModel(id) => {
            data.transcription.selected_model_id = Some(id.clone());
            for model in &mut data.models {
                model.active = model.id == id;
                model.primary_action_label = if model.active {
                    "Active"
                } else {
                    "Use this model"
                }
                .to_owned();
                model.primary_action_enabled = !model.active;
                model.primary_action_disabled_reason = model
                    .active
                    .then(|| "This model is already active.".to_owned());
            }
            data.model_management.dialog = None;
        }
        ScreenAction::SelectQuickModel(id) => {
            data.transcription.selected_model_id = Some(id);
        }
        ScreenAction::AddModel => {
            data.model_management.dialog = Some(ModelDialog::Add);
            data.model_management.focus_dialog_initial = true;
        }
        ScreenAction::ToggleModelCardDetails(key) => {
            if data.model_management.expanded_model_card.as_ref() == Some(&key) {
                data.model_management.expanded_model_card = None;
            } else {
                data.model_management.expanded_model_card = Some(key);
            }
        }
        ScreenAction::RequestModelRemoval(id) => {
            data.model_management.restore_remove_focus = matches!(
                &data.model_management.expanded_model_card,
                Some(ModelCardKey::Local(current)) if current == &id
            )
            .then(|| id.clone());
            data.model_management.dialog = Some(ModelDialog::Remove(id));
            data.model_management.focus_dialog_initial = true;
        }
        ScreenAction::AcknowledgeModelRemovalFocus => {
            data.model_management.restore_remove_focus = None;
        }
        ScreenAction::CloseModelDialog => match data.model_management.dialog.take() {
            Some(ModelDialog::Add) => data.model_management.restore_add_focus = true,
            Some(ModelDialog::Remove(id)) => data.model_management.restore_remove_focus = Some(id),
            None => {}
        },
        ScreenAction::ConfirmModelRemoval(id) => {
            data.model_management.dialog = None;
            data.model_management.restore_remove_focus = None;
            data.models.retain(|model| model.id != id);
            data.model_management.restore_after_removal_focus = true;
        }
        ScreenAction::StartHotkeyCapture => data.transcription.hotkey_capture_active = true,
        ScreenAction::CancelHotkeyCapture => data.transcription.hotkey_capture_active = false,
        ScreenAction::StartRecording => data.transcription.phase = TranscriptionPhase::Listening,
        ScreenAction::StopRecording => data.transcription.phase = TranscriptionPhase::Finalizing,
        ScreenAction::AbandonRecording => {
            data.transcription.phase = TranscriptionPhase::Ready;
            data.transcription.provisional_transcript.clear();
        }
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
        ScreenAction::SetRemoteCatalogQuery(query) => data.remote_catalog.query = query,
        ScreenAction::SetModelLanguageFilter(filter) => data.model_language_filter = filter,
        ScreenAction::ToggleInstalledModels => {
            data.model_management.installed_expanded = !data.model_management.installed_expanded;
            if !data.model_management.installed_expanded {
                data.model_management.expanded_model_card = None;
            }
        }
        ScreenAction::ToggleAvailableModels => {
            data.model_management.available_expanded = !data.model_management.available_expanded;
            if !data.model_management.available_expanded {
                data.model_management.expanded_model_card = None;
            }
        }
        ScreenAction::SetLocalGgufImportPath(path) => data.remote_catalog.local_import.path = path,
        ScreenAction::ValidateAndImportLocalGguf => {
            data.remote_catalog.local_import.in_progress = true;
            data.remote_catalog.local_import.import_enabled = false;
        }
        ScreenAction::CancelLocalGgufImport => {
            data.remote_catalog.local_import.in_progress = false;
            data.remote_catalog.local_import.import_enabled = true;
        }
        ScreenAction::InstallRemoteCatalogVariant {
            remote_model_id,
            variant_id,
        } => {
            if let Some(variant) = data
                .remote_catalog
                .entries
                .iter_mut()
                .find(|entry| entry.id == remote_model_id)
                .and_then(|entry| {
                    entry
                        .variants
                        .iter_mut()
                        .find(|variant| variant.id == variant_id)
                })
            {
                variant.status_label = Some("Downloading".into());
                variant.actions = vec![RemoteCatalogActionView {
                    label: "Cancel".into(),
                    kind: RemoteCatalogActionKind::Cancel {
                        model_id: "managed-compact-english".into(),
                    },
                    enabled: true,
                    disabled_reason: None,
                }];
            }
        }
        ScreenAction::CancelRemoteCatalogInstall(model_id) => {
            for entry in &mut data.remote_catalog.entries {
                for variant in &mut entry.variants {
                    if variant.actions.iter().any(|action| {
                        matches!(
                            &action.kind,
                            RemoteCatalogActionKind::Cancel {
                                model_id: action_model_id
                            } if action_model_id == &model_id
                        )
                    }) {
                        variant.status_label = Some("Cancelled".into());
                        variant.actions = vec![RemoteCatalogActionView {
                            label: "Resume".into(),
                            kind: RemoteCatalogActionKind::Install {
                                remote_model_id: entry.id.clone(),
                                variant_id: variant.id.clone(),
                            },
                            enabled: true,
                            disabled_reason: None,
                        }];
                    }
                }
            }
        }
        ScreenAction::UseRemoteCatalogModel(model_id) => {
            data.remote_catalog.status.message = format!("Selected catalog model {model_id}.");
        }
        ScreenAction::RemoveRemoteCatalogModel(model_id) => {
            data.remote_catalog.status.message = format!("Removed catalog model {model_id}.");
        }
        ScreenAction::SetSettingsTab(tab) => {
            data.route = UiRoute::Settings(match tab {
                SettingsTab::Output => SettingsTab::General,
                tab => tab,
            });
            data.settings_playground_open = false;
            *page = AppPage::General;
        }
        ScreenAction::SetCloseToTray(value) => data.settings.close_to_tray = value,
        ScreenAction::OpenModelSettings => *page = AppPage::Models,
        ScreenAction::SetTheme(value) => data.settings.theme_label = value,
        ScreenAction::ToggleResolvedTheme(resolved_theme) => {
            let next_theme = match resolved_theme {
                ResolvedTheme::Dark => "Light",
                ResolvedTheme::Light => "Dark",
            };
            data.settings.theme_label = next_theme.into();
            data.theme_announcement = Some(format!("Theme changed to {next_theme}."));
        }
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
        ScreenAction::SetVoiceDetectionMode(mode) => {
            data.settings.voice_detection_mode = mode;
        }
        ScreenAction::SetInputThresholdDbfs(dbfs) => {
            data.settings.input_threshold_dbfs = f32::from(dbfs);
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
        ScreenAction::OpenDeveloperPlayground if data.settings.debug_mode => {
            data.route = UiRoute::Settings(SettingsTab::Advanced);
            data.settings_playground_open = true;
            *page = AppPage::General;
        }
        ScreenAction::OpenDeveloperPlayground => {}
        ScreenAction::ExportRedactedDiagnostics => {}
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

    #[test]
    fn harness_initialization_forces_light_visuals_and_keeps_accessible_style() {
        let ctx = egui::Context::default();
        ctx.set_visuals(egui::Visuals::dark());

        configure_harness_style(&ctx, false);

        let style = ctx.style();
        assert!(!style.visuals.dark_mode);
        assert_eq!(style.visuals, ThemePalette::visuals(false));
        assert_eq!(style.spacing.interact_size, egui::vec2(44.0, 44.0));
        assert_eq!(
            style.text_styles[&egui::TextStyle::Body],
            egui::FontId::new(14.0, egui::FontFamily::Proportional)
        );
    }

    #[test]
    fn overlay_fixture_parser_accepts_only_the_four_capture_variants() {
        assert_eq!(
            Fixture::parse("overlay/live-light"),
            Some(Fixture::OverlayLiveLight)
        );
        assert_eq!(
            Fixture::parse("overlay/live-dark"),
            Some(Fixture::OverlayLiveDark)
        );
        assert_eq!(
            Fixture::parse("overlay/compact-light"),
            Some(Fixture::OverlayCompactLight)
        );
        assert_eq!(
            Fixture::parse("overlay/compact-dark"),
            Some(Fixture::OverlayCompactDark)
        );
        assert_eq!(Fixture::parse("overlay/live"), None);
        assert_eq!(Fixture::parse("overlay/compact-system"), None);
    }

    #[test]
    fn overlay_capture_fixtures_have_fixed_isolated_state() {
        for (fixture, expected_mode, expected_dark) in [
            (Fixture::OverlayLiveLight, OverlayMode::Live, false),
            (Fixture::OverlayLiveDark, OverlayMode::Live, true),
            (Fixture::OverlayCompactLight, OverlayMode::Minimal, false),
            (Fixture::OverlayCompactDark, OverlayMode::Minimal, true),
        ] {
            let overlay = fixture.overlay().expect("overlay fixture should parse");
            assert_eq!(overlay.dark_mode, expected_dark);
            assert_eq!(overlay.state.session_id, Some(SessionId(42)));
            assert_eq!(overlay.state.mode, expected_mode);
            assert_eq!(overlay.state.phase, OverlayPhase::Listening);
            assert_eq!(
                overlay.state.audio_level,
                OverlayAudioLevel::new(0.58, 0.78)
            );
            assert_eq!(overlay.state.elapsed, Some(Duration::from_secs(10)));
            assert_eq!(overlay.state.transcript.revision, 1);
            assert_eq!(
                overlay.state.transcript.committed,
                "Alright, What is going on? Why is there a line on"
            );
            assert_eq!(
                overlay.state.transcript.tentative,
                "That's pretty cool. These newest words stay visible."
            );
            assert!(overlay.state.transcript_announcement.is_none());
            assert!(overlay.state.notice.is_none());
            assert!(overlay.state.error.is_none());
        }
    }

    #[test]
    fn overlay_fixture_style_selects_requested_theme_without_changing_normal_default() {
        let light_ctx = egui::Context::default();
        configure_harness_style(&light_ctx, false);
        assert!(!light_ctx.style().visuals.dark_mode);

        let dark_ctx = egui::Context::default();
        configure_harness_style(&dark_ctx, true);
        assert!(dark_ctx.style().visuals.dark_mode);
    }

    #[test]
    fn harness_theme_parser_accepts_only_documented_deterministic_values() {
        assert_eq!(HarnessTheme::parse("light"), Some(HarnessTheme::Light));
        assert_eq!(HarnessTheme::parse("dark"), Some(HarnessTheme::Dark));
        assert_eq!(HarnessTheme::parse(" light "), Some(HarnessTheme::Light));
        assert_eq!(HarnessTheme::parse("Light"), None);
        assert_eq!(HarnessTheme::parse("system"), None);
        assert_eq!(HarnessTheme::parse(""), None);
    }

    #[test]
    fn harness_theme_toggle_mutates_the_visuals_used_by_the_next_frame() {
        let ctx = egui::Context::default();
        configure_harness_style(&ctx, false);
        let mut data = Fixture::TranscribeReady.data();
        let mut page = Fixture::TranscribeReady.page();

        apply_harness_action(
            &ctx,
            &mut data,
            &mut page,
            ScreenAction::ToggleResolvedTheme(ResolvedTheme::Light),
        );

        assert_eq!(data.settings.theme_label, "Dark");
        assert_eq!(ctx.style().visuals, ThemePalette::visuals(true));
        assert_eq!(
            data.theme_announcement.as_deref(),
            Some("Theme changed to Dark.")
        );
    }

    #[test]
    fn harness_root_clear_color_is_opaque_in_both_themes() {
        let app = UiHarnessApp {
            page: Fixture::TranscribeReady.page(),
            data: Fixture::TranscribeReady.data(),
            overlay: None,
            overlay_presented: false,
            demo_playback: None,
        };

        for visuals in [egui::Visuals::light(), egui::Visuals::dark()] {
            let clear_color = eframe::App::clear_color(&app, &visuals);
            assert_eq!(
                clear_color,
                ThemePalette::from_visuals(&visuals)
                    .content_bg
                    .to_normalized_gamma_f32()
            );
            assert_eq!(clear_color[3], 1.0);
        }
    }

    fn render(fixture: Fixture, width: f32, height: f32) -> egui::FullOutput {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        configure_harness_style(&ctx, fixture.dark_mode());
        let mut page = fixture.page();
        let mut data = fixture.data();
        ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::Vec2::new(width, height),
                )),
                ..Default::default()
            },
            |ctx| {
                let _ = show_harness(ctx, &mut data, &mut page);
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
        render_with_input_at_time(ctx, data, page, width, height, events, None)
    }

    fn render_with_input_at_time(
        ctx: &egui::Context,
        data: &mut FixtureData,
        page: &mut AppPage,
        width: f32,
        height: f32,
        events: Vec<egui::Event>,
        time: Option<f64>,
    ) -> (egui::FullOutput, ScreenAction) {
        let clear_initial_dialog_focus = data.model_management.focus_dialog_initial;
        let clear_add_focus = data.model_management.restore_add_focus;
        let clear_reference_editor_focus = data.comparison.focus_reference_editor;
        let clear_reference_action_focus = data.comparison.restore_reference_action_focus;
        let clear_comparison_panel_focus = data.comparison.focus_panel;
        let clear_reference_notice = data.comparison.reference_notice.is_some();
        let clear_after_removal_focus = data.model_management.restore_after_removal_focus;
        let clear_history_confirmation_focus = data.history_focus_delete_confirmation;
        let clear_history_more_focus = data.history_focus_more_action.is_some();
        let mut action = ScreenAction::None;
        let output = ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::Vec2::new(width, height),
                )),
                events,
                time,
                focused: true,
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
        if clear_comparison_panel_focus {
            data.comparison.focus_panel = false;
        }
        if clear_reference_notice {
            data.comparison.reference_notice = None;
        }
        if clear_initial_dialog_focus {
            data.model_management.focus_dialog_initial = false;
        }
        if clear_add_focus {
            data.model_management.restore_add_focus = false;
        }
        if clear_after_removal_focus {
            data.model_management.restore_after_removal_focus = false;
        }
        if clear_history_confirmation_focus {
            data.history_focus_delete_confirmation = false;
        }
        if clear_history_more_focus {
            data.history_focus_more_action = None;
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

    fn quick_model_card_bounds(output: &egui::FullOutput) -> egui::accesskit::Rect {
        node_matching(output, |node| {
            node.role() == egui::accesskit::Role::Button
                && node.name().is_some_and(|name| {
                    name.starts_with("Choose active model:") || name == "Add a model"
                })
        })
        .bounds()
        .expect("quick model card must expose bounds")
    }

    fn quick_hotkey_card_bounds(output: &egui::FullOutput) -> egui::accesskit::Rect {
        node_matching(output, |node| {
            node.role() == egui::accesskit::Role::Button
                && node.name().is_some_and(|name| {
                    name.starts_with("Change recording shortcut. Current shortcut:")
                        || name.starts_with("Recording shortcut capture.")
                })
        })
        .bounds()
        .expect("quick hotkey card must expose bounds")
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

    fn assert_bounds_within(
        inner: egui::accesskit::Rect,
        outer: egui::accesskit::Rect,
        label: &str,
    ) {
        assert!(
            inner.x0 >= outer.x0 - LAYOUT_TOLERANCE
                && inner.x1 <= outer.x1 + LAYOUT_TOLERANCE
                && inner.y0 >= outer.y0 - LAYOUT_TOLERANCE
                && inner.y1 <= outer.y1 + LAYOUT_TOLERANCE,
            "{label} {inner:?} must remain within {outer:?}"
        );
    }

    fn painted_text_bounds_in(
        output: &egui::FullOutput,
        expected_text: &str,
        cell: egui::accesskit::Rect,
    ) -> egui::Rect {
        fn collect_matching_text_bounds(
            shape: &egui::epaint::Shape,
            expected_text: &str,
            cell: egui::Rect,
            matches: &mut Vec<egui::Rect>,
        ) {
            match shape {
                egui::epaint::Shape::Text(text)
                    if text.galley.text() == expected_text
                        && cell.contains_rect(text.visual_bounding_rect()) =>
                {
                    matches.push(text.visual_bounding_rect());
                }
                egui::epaint::Shape::Vec(shapes) => {
                    for shape in shapes {
                        collect_matching_text_bounds(shape, expected_text, cell, matches);
                    }
                }
                _ => {}
            }
        }

        let cell = egui::Rect::from_min_max(
            egui::pos2(cell.x0 as f32, cell.y0 as f32),
            egui::pos2(cell.x1 as f32, cell.y1 as f32),
        );
        let mut matches = Vec::new();
        for clipped_shape in &output.shapes {
            collect_matching_text_bounds(&clipped_shape.shape, expected_text, cell, &mut matches);
        }
        assert_eq!(
            matches.len(),
            1,
            "expected one painted {expected_text:?} glyph within {cell:?}, found {matches:?}"
        );
        matches
            .pop()
            .expect("exactly one matching painted text shape")
    }

    fn assert_within_tolerance(actual: f64, expected: f64, tolerance: f64, label: &str) {
        assert!(
            (actual - expected).abs() <= tolerance,
            "{label}: expected {expected} ± {tolerance}, got {actual}"
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
    fn transcribe_layout_stays_within_shell_at_reference_widths() {
        for (width, height) in [(1_180.0, 815.0), (1_024.0, 768.0), (960.0, 680.0)] {
            let ctx = egui::Context::default();
            ctx.enable_accesskit();
            configure_accessible_style(&ctx);
            let mut data = Fixture::TranscribeReady.data();
            let mut page = Fixture::TranscribeReady.page();
            let committed = "A deliberately long committed transcript should wrap within the bounded transcript panel without pushing any controls beyond the application content region. ".repeat(16);
            let provisional = "A deliberately long provisional transcript should also wrap within the bounded transcript panel rather than creating horizontal overflow. ".repeat(8);
            data.transcription.committed_transcript = committed.clone();
            data.transcription.provisional_transcript = provisional.clone();

            let (output, action) =
                render_with_input(&ctx, &mut data, &mut page, width, height, Vec::new());
            assert_eq!(action, ScreenAction::None);

            let viewport = egui::accesskit::Rect {
                x0: 0.0,
                y0: 0.0,
                x1: width.into(),
                y1: height.into(),
            };
            let panel = named_node_bounds(&output, "Transcript panel");
            let model = quick_model_card_bounds(&output);
            let hotkey = quick_hotkey_card_bounds(&output);
            for (label, bounds) in [
                ("transcript panel", panel),
                ("selected model card", model),
                ("hotkey card", hotkey),
            ] {
                assert_bounds_within(bounds, viewport, label);
            }
            assert_within_tolerance(model.y0, hotkey.y0, 3.0, "inline control bar");
            assert_within_tolerance(model.y1 - model.y0, 44.0, 3.0, "model target height");
            assert_within_tolerance(hotkey.y1 - hotkey.y0, 44.0, 3.0, "shortcut target height");

            let scroll = node_matching(&output, |node| {
                node.role() == egui::accesskit::Role::ScrollView
                    && node.name() == Some("Scrollable transcript text")
            });
            let scroll_bounds = scroll.bounds().expect("scrollable transcript bounds");
            assert_bounds_within(scroll_bounds, panel, "transcript scroll viewport");
            assert!(
                scroll_bounds.y1 - scroll_bounds.y0 <= 320.0 + LAYOUT_TOLERANCE,
                "transcript viewport exceeded its maximum: {scroll_bounds:?}"
            );
            assert!(scroll.scroll_y_max().unwrap_or_default() > 0.0);

            let committed_node =
                node_matching(&output, |node| node.name() == Some(committed.as_str()));
            assert_eq!(committed_node.live(), Some(egui::accesskit::Live::Polite));
            let estimate_name = format!("Live estimate, may change: {provisional}");
            let estimate =
                node_matching(&output, |node| node.name() == Some(estimate_name.as_str()));
            assert_eq!(estimate.role(), egui::accesskit::Role::StaticText);
            assert!(estimate.live().is_none());

            for name in ["Clear", "Copy"] {
                let bounds = node_matching(&output, |node| node.name() == Some(name))
                    .bounds()
                    .expect("transcript action should expose bounds");
                assert_bounds_within(bounds, panel, name);
                assert!(bounds.y1 - bounds.y0 >= 44.0);
            }
            let names = node_names(&output);
            assert!(!names.iter().any(|name| name.contains("Silence is ignored")));
            assert!(!names.iter().any(|name| name == "BASE.EN"));
        }
    }

    #[test]
    fn active_transcribe_states_fit_reference_viewports_in_both_themes() {
        for dark in [false, true] {
            for fixture in [Fixture::TranscribeListening, Fixture::TranscribeFinalizing] {
                for (width, height) in [(1_180.0, 815.0), (1_024.0, 768.0), (960.0, 680.0)] {
                    let ctx = egui::Context::default();
                    ctx.enable_accesskit();
                    configure_accessible_style(&ctx);
                    ctx.set_visuals(if dark {
                        egui::Visuals::dark()
                    } else {
                        egui::Visuals::light()
                    });
                    let mut data = fixture.data();
                    let mut page = fixture.page();
                    let (output, action) =
                        render_with_input(&ctx, &mut data, &mut page, width, height, Vec::new());
                    assert_eq!(action, ScreenAction::None);

                    let viewport = egui::accesskit::Rect {
                        x0: 0.0,
                        y0: 0.0,
                        x1: width.into(),
                        y1: height.into(),
                    };
                    for (label, bounds) in [
                        ("model control", quick_model_card_bounds(&output)),
                        ("shortcut control", quick_hotkey_card_bounds(&output)),
                        (
                            "transcript panel",
                            named_node_bounds(&output, "Transcript panel"),
                        ),
                    ] {
                        assert_bounds_within(bounds, viewport, label);
                    }
                    let status_name = match fixture {
                        Fixture::TranscribeListening => "Recording",
                        Fixture::TranscribeFinalizing => "Finalizing transcript…",
                        _ => unreachable!(),
                    };
                    assert_bounds_within(
                        named_node_bounds(&output, status_name),
                        viewport,
                        "active status row",
                    );
                    let primary_name = match fixture {
                        Fixture::TranscribeListening => "Stop recording",
                        Fixture::TranscribeFinalizing => "Start recording",
                        _ => unreachable!(),
                    };
                    let primary = named_node_bounds(&output, primary_name);
                    assert_bounds_within(primary, viewport, "active recording action");
                    assert!(primary.y1 - primary.y0 >= 44.0 - LAYOUT_TOLERANCE);
                    if fixture == Fixture::TranscribeListening {
                        assert_bounds_within(
                            named_node_bounds(&output, "Cancel recording and discard it"),
                            viewport,
                            "cancel recording action",
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn selector_wraps_only_after_long_model_and_hotkey_content_no_longer_fit() {
        let long_model_name = "Whisper Large v3 Turbo English — high accuracy dictation";
        for (width, should_stack) in [(1_180.0, false), (600.0, true)] {
            let ctx = egui::Context::default();
            ctx.enable_accesskit();
            configure_accessible_style(&ctx);
            let mut data = Fixture::TranscribeReady.data();
            let mut page = Fixture::TranscribeReady.page();
            data.models[0].display_name = long_model_name.into();
            data.transcription.hotkey = "Ctrl + Shift + Alt + Space".into();

            let (output, action) =
                render_with_input(&ctx, &mut data, &mut page, width, 815.0, Vec::new());
            assert_eq!(action, ScreenAction::None);

            let model = quick_model_card_bounds(&output);
            let hotkey = quick_hotkey_card_bounds(&output);
            let change = node_matching(&output, |node| {
                node.name()
                    .is_some_and(|name| name.starts_with("Choose active model:"))
            })
            .bounds()
            .expect("model card action should remain exposed");
            assert_bounds_within(change, model, "Change action");
            assert_eq!(change, model, "the entire model card must be interactive");
            assert!(change.y1 - change.y0 >= 44.0);
            assert!(model.x1 - model.x0 <= 360.0 + LAYOUT_TOLERANCE);
            assert!(hotkey.x1 - hotkey.x0 <= 300.0 + LAYOUT_TOLERANCE);
            if should_stack {
                assert!(
                    model.y1 <= hotkey.y0 + LAYOUT_TOLERANCE,
                    "selector should stack after content no longer fits: {model:?} {hotkey:?}"
                );
            } else {
                assert!(
                    model.x1 <= hotkey.x0 + LAYOUT_TOLERANCE,
                    "selector should remain inline while its content fits: {model:?} {hotkey:?}"
                );
            }
        }
    }

    #[test]
    fn narrow_selector_wraps_hotkey_contents_inside_the_second_row_card() {
        let long_model_name = "Whisper Large v3 Turbo English — high accuracy dictation";
        for (hotkey_value, model_name) in [
            ("Ctrl + Space", None),
            ("Ctrl + Shift + Alt + Super + Space", Some(long_model_name)),
        ] {
            let ctx = egui::Context::default();
            ctx.enable_accesskit();
            configure_accessible_style(&ctx);
            let mut data = Fixture::TranscribeReady.data();
            let mut page = Fixture::TranscribeReady.page();
            data.transcription.hotkey = hotkey_value.into();
            if let Some(model_name) = model_name {
                data.models[0].display_name = model_name.into();
            }

            let (output, action) =
                render_with_input(&ctx, &mut data, &mut page, 375.0, 815.0, Vec::new());
            assert_eq!(action, ScreenAction::None);

            let viewport = egui::accesskit::Rect {
                x0: 0.0,
                y0: 0.0,
                x1: 375.0,
                y1: 815.0,
            };
            let model = quick_model_card_bounds(&output);
            let hotkey = quick_hotkey_card_bounds(&output);
            assert!(model.y1 <= hotkey.y0 + LAYOUT_TOLERANCE);
            assert_bounds_within(model, viewport, "narrow selected model card");
            assert_bounds_within(hotkey, viewport, "narrow hotkey card");
            let change = node_matching(&output, |node| {
                node.name()
                    .is_some_and(|name| name.starts_with("Choose active model:"))
            })
            .bounds()
            .expect("narrow model card target should expose bounds");
            assert_bounds_within(change, model, "narrow Change target");
            assert_eq!(
                change, model,
                "the narrow model card must be interactive edge to edge"
            );
            assert!(change.y1 - change.y0 >= 44.0);
            assert_within_tolerance(model.y1 - model.y0, 44.0, 1.0, "fixed model card height");
            assert_within_tolerance(hotkey.y1 - hotkey.y0, 44.0, 1.0, "fixed hotkey card height");
        }
    }

    #[test]
    fn no_model_layout_uses_one_compact_recovery_row_and_empty_transcript() {
        for (width, height) in [(1_180.0, 815.0), (1_024.0, 768.0), (960.0, 680.0)] {
            let ctx = egui::Context::default();
            ctx.enable_accesskit();
            configure_accessible_style(&ctx);
            let mut data = Fixture::TranscribeNoModel.data();
            let mut page = Fixture::TranscribeNoModel.page();
            let (output, action) =
                render_with_input(&ctx, &mut data, &mut page, width, height, Vec::new());
            assert_eq!(action, ScreenAction::None);

            let viewport = egui::accesskit::Rect {
                x0: 0.0,
                y0: 0.0,
                x1: width.into(),
                y1: height.into(),
            };
            let panel = named_node_bounds(&output, "Transcript panel");
            let selector = quick_model_card_bounds(&output);
            let hotkey = quick_hotkey_card_bounds(&output);
            let status = named_node_bounds(&output, "Add a speech model to start transcribing.");
            let placeholder = named_node_bounds(&output, "Your transcript will appear here.");
            for (label, bounds) in [
                ("transcript panel", panel),
                ("selected model card", selector),
                ("hotkey card", hotkey),
                ("recovery status", status),
            ] {
                assert_bounds_within(bounds, viewport, label);
            }
            assert_bounds_within(placeholder, panel, "empty transcript placeholder");
            assert_within_tolerance(selector.y0, hotkey.y0, 3.0, "inline control bar");
            for card in [selector, hotkey] {
                assert_within_tolerance(card.y1 - card.y0, 44.0, 3.0, "selector height");
            }
            let names = node_names(&output);
            assert!(names.iter().any(|name| name == "Add model"));
            assert!(!names.iter().any(|name| name == "Clear" || name == "Copy"));
            assert!(!names.iter().any(|name| name.contains("Silence is ignored")));
        }
    }

    #[test]
    fn selector_actions_use_the_entire_card_as_the_accessible_target() {
        let (width, height) = (1180.0, 815.0);
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        configure_accessible_style(&ctx);

        let mut no_model_data = Fixture::TranscribeNoModel.data();
        let mut no_model_page = Fixture::TranscribeNoModel.page();
        let (no_model_output, no_model_action) = render_with_input(
            &ctx,
            &mut no_model_data,
            &mut no_model_page,
            width,
            height,
            Vec::new(),
        );
        assert_eq!(no_model_action, ScreenAction::None);
        let card = quick_model_card_bounds(&no_model_output);
        let select = node_matching(&no_model_output, |node| {
            node.role() == egui::accesskit::Role::Button && node.name() == Some("Add a model")
        });
        let select_bounds = select.bounds().expect("Select should expose bounds");
        assert!(!select.is_disabled());
        assert!(
            card.x1 - card.x0 <= 360.0 + LAYOUT_TOLERANCE,
            "model card should remain bounded instead of filling the route"
        );
        assert_eq!(select_bounds, card);
        assert!(select_bounds.y1 - select_bounds.y0 >= 44.0);
        assert_eq!(
            click_named_control(
                &ctx,
                &mut no_model_data,
                &mut no_model_page,
                width,
                height,
                "Add a model",
            ),
            ScreenAction::AddModel,
        );

        let mut ready_data = Fixture::TranscribeReady.data();
        let mut ready_page = Fixture::TranscribeReady.page();
        assert_eq!(
            click_named_control(
                &ctx,
                &mut ready_data,
                &mut ready_page,
                width,
                height,
                "Choose active model: whisper.cpp base.en",
            ),
            ScreenAction::None,
        );

        let listening = render(Fixture::TranscribeListening, width, height);
        let disabled_change = node_matching(&listening, |node| {
            node.role() == egui::accesskit::Role::Button
                && node.name() == Some("Choose active model: whisper.cpp base.en")
        });
        assert!(disabled_change.is_disabled());
        assert_eq!(
            disabled_change.description(),
            Some("Model selection is unavailable while recording.")
        );
    }

    #[test]
    fn transcribe_fixtures_keep_the_compact_status_and_content_hierarchy() {
        let ready = render(Fixture::TranscribeReady, 1_180.0, 815.0);
        let panel = named_node_bounds(&ready, "Transcript panel");
        let start = node_matching(&ready, |node| {
            node.role() == egui::accesskit::Role::Button && node.name() == Some("Start recording")
        })
        .bounds()
        .expect("Start recording should expose bounds");
        assert!(start.y1 - start.y0 >= 44.0 - LAYOUT_TOLERANCE);
        assert_bounds_within(
            start,
            egui::accesskit::Rect {
                x0: 0.0,
                y0: 0.0,
                x1: 1_180.0,
                y1: 815.0,
            },
            "record action",
        );
        let names = node_names(&ready);
        assert!(
            names
                .iter()
                .any(|name| name == "Choose active model: whisper.cpp base.en")
        );
        assert!(
            names
                .iter()
                .any(|name| { name.starts_with("Change recording shortcut. Current shortcut:") })
        );
        assert!(
            !names
                .iter()
                .any(|name| name == "2 MINS AGO" || name == "BASE.EN")
        );
        assert!(!names.iter().any(|name| name.contains("Silence is ignored")));
        assert!(!names.iter().any(|name| name == "Ready"));
        for name in ["Clear", "Copy"] {
            let bounds = named_node_bounds(&ready, name);
            assert_bounds_within(bounds, panel, name);
        }

        let microphone = render(Fixture::TranscribeMicrophoneError, 1_180.0, 815.0);
        let canonical_count = node_names(&microphone)
            .iter()
            .filter(|name| name.as_str() == "Scribe couldn’t access your microphone.")
            .count();
        assert_eq!(canonical_count, 1);
        assert!(
            node_names(&microphone)
                .iter()
                .any(|name| name == "Try again")
        );
        assert!(
            node_names(&microphone)
                .iter()
                .any(|name| name == "Transcript panel")
        );

        let listening = render(Fixture::TranscribeListening, 1_180.0, 815.0);
        assert!(
            node_names(&listening)
                .iter()
                .any(|name| name == "Recording")
        );
        assert!(
            node_names(&listening)
                .iter()
                .any(|name| name == "Stop recording")
        );
        assert!(
            node_names(&listening)
                .iter()
                .any(|name| name == "Cancel recording and discard it")
        );

        let finalizing = render(Fixture::TranscribeFinalizing, 1_180.0, 815.0);
        assert!(
            node_names(&finalizing)
                .iter()
                .any(|name| name == "Finalizing transcript…")
        );
        assert!(
            node_names(&finalizing)
                .iter()
                .any(|name| name == "Transcript panel")
        );

        let no_model = render(Fixture::TranscribeNoModel, 1_180.0, 815.0);
        assert!(node_names(&no_model).iter().any(|name| name == "Add model"));
    }

    fn page_event(key: egui::Key) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::default(),
        }
    }

    #[test]
    fn escape_during_local_gguf_validation_cancels_instead_of_closing() {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        configure_accessible_style(&ctx);
        let mut data = Fixture::ModelsInstalled.data();
        let mut page = Fixture::ModelsInstalled.page();
        data.model_management.dialog = Some(ModelDialog::Add);
        data.model_management.focus_dialog_initial = true;
        data.remote_catalog.local_import.in_progress = true;
        data.remote_catalog.local_import.import_enabled = false;

        let (_, initial_action) =
            render_with_input(&ctx, &mut data, &mut page, 1180.0, 815.0, Vec::new());
        assert_eq!(initial_action, ScreenAction::None);

        let (_, action) = render_with_input(
            &ctx,
            &mut data,
            &mut page,
            1180.0,
            815.0,
            vec![page_event(egui::Key::Escape)],
        );
        assert_eq!(action, ScreenAction::CancelLocalGgufImport);
    }

    #[test]
    fn harness_model_selection_updates_the_following_transcribe_projection() {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        configure_accessible_style(&ctx);
        let mut data = Fixture::ModelsInstalled.data();
        let mut page = Fixture::ModelsInstalled.page();

        apply_action(
            &mut data,
            &mut page,
            ScreenAction::SelectModel("tiny.en".into()),
        );
        page = AppPage::Transcribe;
        let output = render_with_input(&ctx, &mut data, &mut page, 1180.0, 815.0, Vec::new()).0;

        assert_eq!(
            data.transcription.selected_model_id.as_deref(),
            Some("tiny.en")
        );
        assert!(
            node_names(&output)
                .iter()
                .any(|name| name == "Choose active model: whisper.cpp tiny.en")
        );
    }

    #[test]
    fn sidebar_model_picker_is_available_from_all_four_primary_routes() {
        for starting_page in [
            AppPage::Transcribe,
            AppPage::Models,
            AppPage::History,
            AppPage::General,
        ] {
            let ctx = egui::Context::default();
            ctx.enable_accesskit();
            configure_accessible_style(&ctx);
            let mut data = Fixture::TranscribeReady.data();
            data.model_catalog = vec![
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
            data.transcription.selected_model_id = Some("base.en".into());
            let mut page = starting_page;

            assert_eq!(
                click_named_control(
                    &ctx,
                    &mut data,
                    &mut page,
                    1_180.0,
                    815.0,
                    "Change active model: Whisper Base",
                ),
                ScreenAction::None
            );
            let picker =
                render_with_input(&ctx, &mut data, &mut page, 1_180.0, 815.0, Vec::new()).0;
            let names = node_names(&picker);
            assert!(
                names
                    .iter()
                    .any(|name| name == "Whisper Base, current model")
            );
            assert!(names.iter().any(|name| name == "Select Whisper Tiny"));
            assert!(!names.iter().any(|name| name == "Broken model"));

            let action = click_named_control(
                &ctx,
                &mut data,
                &mut page,
                1_180.0,
                815.0,
                "Select Whisper Tiny",
            );
            assert_eq!(action, ScreenAction::SelectQuickModel("tiny.en".into()));
            apply_action(&mut data, &mut page, action);
            assert_eq!(
                data.transcription.selected_model_id.as_deref(),
                Some("tiny.en")
            );
            assert_eq!(page, starting_page);
        }
    }

    #[test]
    fn sidebar_picker_manage_models_action_keeps_models_as_management() {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        configure_accessible_style(&ctx);
        let mut data = Fixture::TranscribeReady.data();
        data.model_catalog = vec![ModelViewModel {
            id: "base.en".into(),
            display_name: "Whisper Base".into(),
            installed: true,
            ready: true,
            ..Default::default()
        }];
        data.transcription.selected_model_id = Some("base.en".into());
        let mut page = AppPage::History;
        assert_eq!(
            click_named_control(
                &ctx,
                &mut data,
                &mut page,
                1_180.0,
                815.0,
                "Change active model: Whisper Base",
            ),
            ScreenAction::None
        );
        let action =
            click_named_control(&ctx, &mut data, &mut page, 1_180.0, 815.0, "Manage models…");
        assert_eq!(action, ScreenAction::OpenModelSettings);
        apply_action(&mut data, &mut page, action);
        assert_eq!(page, AppPage::Models);
    }

    #[test]
    fn confirmed_model_removal_restores_focus_to_import() {
        let (width, height) = (1180.0, 815.0);
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        configure_accessible_style(&ctx);
        let mut data = Fixture::ModelsInstalled.data();
        let mut page = Fixture::ModelsInstalled.page();
        data.model_management.dialog = Some(ModelDialog::Remove("tiny.en".into()));

        apply_action(
            &mut data,
            &mut page,
            ScreenAction::ConfirmModelRemoval("tiny.en".into()),
        );
        assert!(data.model_management.restore_after_removal_focus);
        assert!(data.model_management.dialog.is_none());
        assert!(data.models.iter().all(|model| model.id != "tiny.en"));

        let (output, action) =
            render_with_input(&ctx, &mut data, &mut page, width, height, Vec::new());
        assert_eq!(action, ScreenAction::None);
        assert!(
            focused_node(&output)
                .name()
                .is_some_and(|name| name.contains("Import"))
        );
        assert!(!data.model_management.restore_after_removal_focus);
        assert!(
            !node_names(&output)
                .iter()
                .any(|name| name.contains("Remove tiny.en")),
            "the deleted model's Remove control must not receive restored focus"
        );
    }

    #[test]
    fn every_fixture_renders_at_native_preferred_and_minimum_dimensions() {
        for fixture in Fixture::ALL {
            for (width, height) in [(1180.0, 815.0), (1024.0, 768.0), (960.0, 680.0)] {
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
    fn history_fixtures_render_production_cards_across_approved_viewports() {
        for fixture in [
            Fixture::History,
            Fixture::HistoryDetails,
            Fixture::HistoryConfirmation,
            Fixture::HistoryDark,
        ] {
            for (width, height) in [(960.0, 680.0), (1024.0, 768.0), (1180.0, 815.0)] {
                let output = render(fixture, width, height);
                let names = node_names(&output);
                for expected in [
                    "Search history",
                    "Completed",
                    "Failed",
                    "Pending",
                    "More actions",
                ] {
                    assert!(
                        names.iter().any(|name| name.contains(expected)),
                        "{fixture:?} at {width}x{height} missing {expected}"
                    );
                }
                assert!(names.iter().all(|name| {
                    !name.contains("Local dictation history remains available in production")
                }));
                assert!(output.shapes.iter().all(|shape| {
                    shape.clip_rect.min.x >= 0.0
                        && shape.clip_rect.min.y >= 0.0
                        && shape.clip_rect.max.x <= width
                        && shape.clip_rect.max.y <= height
                }));
            }
        }
    }

    #[test]
    fn history_harness_opens_the_real_more_actions_menu() {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        configure_harness_style(&ctx, false);
        let mut page = AppPage::History;
        let mut data = Fixture::History.data();
        data.history_records.truncate(1);
        let (initial, initial_action) =
            render_with_input(&ctx, &mut data, &mut page, 1024.0, 768.0, Vec::new());
        assert_eq!(initial_action, ScreenAction::None);
        let more_id = named_node_id(&initial, "More actions");
        let _ = render_with_input(
            &ctx,
            &mut data,
            &mut page,
            1024.0,
            768.0,
            vec![egui::Event::AccessKitActionRequest(
                egui::accesskit::ActionRequest {
                    action: egui::accesskit::Action::Focus,
                    target: more_id,
                    data: None,
                },
            )],
        );
        let _ = render_with_input(&ctx, &mut data, &mut page, 1024.0, 768.0, Vec::new());
        let (_, action) = render_with_input(
            &ctx,
            &mut data,
            &mut page,
            1024.0,
            768.0,
            vec![egui::Event::Key {
                key: egui::Key::Enter,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
        );
        assert_eq!(action, ScreenAction::None);
        let (opened, settled_action) =
            render_with_input(&ctx, &mut data, &mut page, 1024.0, 768.0, Vec::new());
        assert_eq!(settled_action, ScreenAction::None);
        let names = node_names(&opened);
        for expected in [
            "Unpin",
            "Show details",
            "Delete retained audio",
            "Delete entry",
        ] {
            assert!(
                names.iter().any(|name| name == expected),
                "opened History More menu missing {expected}"
            );
        }
        assert_eq!(focused_node(&opened).name(), Some("Unpin"));
        assert!(
            opened
                .platform_output
                .accesskit_update
                .as_ref()
                .expect("open menu should update AccessKit")
                .nodes
                .iter()
                .any(|(_, node)| {
                    node.name() == Some("More actions menu")
                        && node.role() == egui::accesskit::Role::Menu
                })
        );
        for item in [
            "Unpin",
            "Show details",
            "Delete retained audio",
            "Delete entry",
        ] {
            assert!(
                opened
                    .platform_output
                    .accesskit_update
                    .as_ref()
                    .expect("open menu should update AccessKit")
                    .nodes
                    .iter()
                    .any(|(_, node)| {
                        node.name() == Some(item) && node.role() == egui::accesskit::Role::MenuItem
                    })
            );
        }

        let (closed, close_action) = render_with_input(
            &ctx,
            &mut data,
            &mut page,
            1024.0,
            768.0,
            vec![page_event(egui::Key::Escape)],
        );
        assert_eq!(close_action, ScreenAction::None);
        assert_eq!(focused_node(&closed).name(), Some("More actions"));
        assert_eq!(named_node_id(&closed, "More actions"), more_id);
        assert!(node_names(&closed).iter().all(|name| name != "Unpin"));

        let _ = render_with_input(
            &ctx,
            &mut data,
            &mut page,
            1024.0,
            768.0,
            vec![page_event(egui::Key::Space)],
        );
        let (reopened, _) =
            render_with_input(&ctx, &mut data, &mut page, 1024.0, 768.0, Vec::new());
        assert_eq!(focused_node(&reopened).name(), Some("Unpin"));

        let (tab_closed, _) = render_with_input(
            &ctx,
            &mut data,
            &mut page,
            1024.0,
            768.0,
            vec![page_event(egui::Key::Tab)],
        );
        assert_eq!(focused_node(&tab_closed).name(), Some("More actions"));
        let (tab_settled, _) =
            render_with_input(&ctx, &mut data, &mut page, 1024.0, 768.0, Vec::new());
        assert!(node_names(&tab_settled).iter().all(|name| name != "Unpin"));

        let _ = render_with_input(
            &ctx,
            &mut data,
            &mut page,
            1024.0,
            768.0,
            vec![page_event(egui::Key::Space)],
        );
        let _ = render_with_input(&ctx, &mut data, &mut page, 1024.0, 768.0, Vec::new());

        let (details_focused, _) = render_with_input(
            &ctx,
            &mut data,
            &mut page,
            1024.0,
            768.0,
            vec![page_event(egui::Key::ArrowDown)],
        );
        assert_eq!(focused_node(&details_focused).name(), Some("Show details"));
        let _ = render_with_input(
            &ctx,
            &mut data,
            &mut page,
            1024.0,
            768.0,
            vec![page_event(egui::Key::Enter)],
        );
        assert!(data.history_expanded_details.contains(&1));
        let (details_shown, _) =
            render_with_input(&ctx, &mut data, &mut page, 1024.0, 768.0, Vec::new());
        assert_eq!(focused_node(&details_shown).name(), Some("More actions"));
        assert!(
            focused_node(&details_shown)
                .description()
                .is_some_and(|description| description.contains("Details are shown"))
        );
    }

    #[test]
    fn history_delete_confirmation_cancel_restores_the_originating_more_button() {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        configure_harness_style(&ctx, false);
        let mut page = AppPage::History;
        let mut data = Fixture::History.data();
        data.history_records.truncate(1);

        let (initial, _) = render_with_input(&ctx, &mut data, &mut page, 840.0, 500.0, Vec::new());
        let more_id = named_node_id(&initial, "More actions");
        let _ = render_with_input(
            &ctx,
            &mut data,
            &mut page,
            840.0,
            500.0,
            vec![egui::Event::AccessKitActionRequest(
                egui::accesskit::ActionRequest {
                    action: egui::accesskit::Action::Default,
                    target: more_id,
                    data: None,
                },
            )],
        );
        let (menu, _) = render_with_input(&ctx, &mut data, &mut page, 840.0, 500.0, Vec::new());
        let delete_entry_id = named_node_id(&menu, "Delete entry");
        let _ = render_with_input(
            &ctx,
            &mut data,
            &mut page,
            840.0,
            500.0,
            vec![egui::Event::AccessKitActionRequest(
                egui::accesskit::ActionRequest {
                    action: egui::accesskit::Action::Default,
                    target: delete_entry_id,
                    data: None,
                },
            )],
        );
        assert_eq!(data.history_confirm_delete, Some(1));

        let (confirmation, _) =
            render_with_input(&ctx, &mut data, &mut page, 840.0, 500.0, Vec::new());
        assert_eq!(focused_node(&confirmation).name(), Some("Cancel"));
        let cancel_id = named_node_id(&confirmation, "Cancel");
        let _ = render_with_input(
            &ctx,
            &mut data,
            &mut page,
            840.0,
            500.0,
            vec![egui::Event::AccessKitActionRequest(
                egui::accesskit::ActionRequest {
                    action: egui::accesskit::Action::Default,
                    target: cancel_id,
                    data: None,
                },
            )],
        );
        assert_eq!(data.history_confirm_delete, None);
        let (restored, _) = render_with_input(&ctx, &mut data, &mut page, 840.0, 500.0, Vec::new());
        assert_eq!(named_node_id(&restored, "More actions"), more_id);
        assert_eq!(focused_node(&restored).name(), Some("More actions"));
    }

    #[test]
    fn history_more_focus_and_identity_survive_pin_and_audio_mutations() {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        configure_harness_style(&ctx, false);
        let mut page = AppPage::History;
        let mut data = Fixture::History.data();
        data.history_records.truncate(1);

        let (initial, _) = render_with_input(&ctx, &mut data, &mut page, 1024.0, 768.0, Vec::new());
        let more_id = named_node_id(&initial, "More actions");
        let _ = render_with_input(
            &ctx,
            &mut data,
            &mut page,
            1024.0,
            768.0,
            vec![egui::Event::AccessKitActionRequest(
                egui::accesskit::ActionRequest {
                    action: egui::accesskit::Action::Default,
                    target: more_id,
                    data: None,
                },
            )],
        );
        let (menu, _) = render_with_input(&ctx, &mut data, &mut page, 1024.0, 768.0, Vec::new());
        let unpin_id = named_node_id(&menu, "Unpin");
        let _ = render_with_input(
            &ctx,
            &mut data,
            &mut page,
            1024.0,
            768.0,
            vec![egui::Event::AccessKitActionRequest(
                egui::accesskit::ActionRequest {
                    action: egui::accesskit::Action::Default,
                    target: unpin_id,
                    data: None,
                },
            )],
        );
        let (unpinned, _) =
            render_with_input(&ctx, &mut data, &mut page, 1024.0, 768.0, Vec::new());
        assert!(!data.history_records[0].pinned);
        assert_eq!(named_node_id(&unpinned, "More actions"), more_id);
        assert_eq!(focused_node(&unpinned).name(), Some("More actions"));

        let _ = render_with_input(
            &ctx,
            &mut data,
            &mut page,
            1024.0,
            768.0,
            vec![egui::Event::AccessKitActionRequest(
                egui::accesskit::ActionRequest {
                    action: egui::accesskit::Action::Default,
                    target: more_id,
                    data: None,
                },
            )],
        );
        let (menu, _) = render_with_input(&ctx, &mut data, &mut page, 1024.0, 768.0, Vec::new());
        let delete_audio_id = named_node_id(&menu, "Delete retained audio");
        let _ = render_with_input(
            &ctx,
            &mut data,
            &mut page,
            1024.0,
            768.0,
            vec![egui::Event::AccessKitActionRequest(
                egui::accesskit::ActionRequest {
                    action: egui::accesskit::Action::Default,
                    target: delete_audio_id,
                    data: None,
                },
            )],
        );
        let (audio_deleted, _) =
            render_with_input(&ctx, &mut data, &mut page, 1024.0, 768.0, Vec::new());
        assert!(data.history_records[0].audio_path.is_none());
        assert_eq!(named_node_id(&audio_deleted, "More actions"), more_id);
        assert_eq!(focused_node(&audio_deleted).name(), Some("More actions"));
    }

    #[test]
    fn history_more_identity_survives_earlier_record_insertion_and_removal() {
        fn more_id_for_model(output: &egui::FullOutput, model_id: &str) -> egui::accesskit::NodeId {
            action_id_for_model(output, "More actions", model_id)
        }

        fn action_id_for_model(
            output: &egui::FullOutput,
            action_name: &str,
            model_id: &str,
        ) -> egui::accesskit::NodeId {
            output
                .platform_output
                .accesskit_update
                .as_ref()
                .expect("History should update AccessKit")
                .nodes
                .iter()
                .find_map(|(id, node)| {
                    (node.name() == Some(action_name)
                        && node
                            .description()
                            .is_some_and(|description| description.contains(model_id)))
                    .then_some(*id)
                })
                .unwrap_or_else(|| panic!("missing {action_name} for {model_id}"))
        }

        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        configure_harness_style(&ctx, false);
        let mut page = AppPage::History;
        let mut data = Fixture::History.data();
        data.history_focus_more_action = Some(2);
        let (initial, _) = render_with_input(&ctx, &mut data, &mut page, 1024.0, 768.0, Vec::new());
        let target_id = more_id_for_model(&initial, "custom-removed-model");
        let copy_id = action_id_for_model(&initial, "Copy", "custom-removed-model");
        assert_eq!(
            initial
                .platform_output
                .accesskit_update
                .as_ref()
                .unwrap()
                .focus,
            target_id
        );

        let mut inserted = data.history_records[0].clone();
        inserted.id = 99;
        inserted.model_id = "inserted-model".to_owned();
        data.history_records.insert(0, inserted);
        let (after_insertion, _) =
            render_with_input(&ctx, &mut data, &mut page, 1024.0, 768.0, Vec::new());
        assert_eq!(
            more_id_for_model(&after_insertion, "custom-removed-model"),
            target_id
        );
        assert_eq!(
            action_id_for_model(&after_insertion, "Copy", "custom-removed-model"),
            copy_id
        );
        assert_eq!(
            after_insertion
                .platform_output
                .accesskit_update
                .as_ref()
                .unwrap()
                .focus,
            target_id
        );

        data.history_records
            .retain(|record| !matches!(record.id, 99 | 1));
        let (after_removal, _) =
            render_with_input(&ctx, &mut data, &mut page, 1024.0, 768.0, Vec::new());
        assert_eq!(
            more_id_for_model(&after_removal, "custom-removed-model"),
            target_id
        );
        assert_eq!(
            action_id_for_model(&after_removal, "Copy", "custom-removed-model"),
            copy_id
        );
        assert_eq!(
            after_removal
                .platform_output
                .accesskit_update
                .as_ref()
                .unwrap()
                .focus,
            target_id
        );
    }

    #[test]
    fn history_restored_focus_is_visible_in_short_viewports() {
        for (width, height) in [(840.0, 500.0), (960.0, 680.0)] {
            for confirmation in [false, true] {
                let ctx = egui::Context::default();
                ctx.enable_accesskit();
                configure_harness_style(&ctx, false);
                let mut page = AppPage::History;
                let mut data = Fixture::History.data();
                data.history_records.truncate(1);
                data.history_expanded_transcripts.insert(1);
                data.history_expanded_details.insert(1);
                if confirmation {
                    data.history_confirm_delete = Some(1);
                    data.history_focus_delete_confirmation = true;
                } else {
                    data.history_focus_more_action = Some(1);
                }

                let _ = render_with_input(&ctx, &mut data, &mut page, width, height, Vec::new());
                let (focused, _) =
                    render_with_input(&ctx, &mut data, &mut page, width, height, Vec::new());
                let node = focused_node(&focused);
                assert_eq!(
                    node.name(),
                    Some(if confirmation {
                        "Cancel"
                    } else {
                        "More actions"
                    })
                );
                let bounds = node.bounds().expect("focused History action needs bounds");
                assert!(
                    bounds.y0 >= 0.0 && bounds.y1 <= f64::from(height),
                    "focused History action escaped {width}x{height}: {bounds:?}"
                );
            }
        }
    }

    #[test]
    fn every_main_window_route_reflows_inside_the_840_by_500_minimum_viewport() {
        const WIDTH: f32 = 840.0;
        const HEIGHT: f32 = 500.0;

        // This is the production shell path: navigation and the central route
        // scroll area render together. The legacy Output route aliases General,
        // while About is a Settings tab rather than a standalone main window.
        for (label, fixture, page, route, heading, visible_content, scrolls_vertically) in [
            (
                "Transcribe",
                Fixture::TranscribeReady,
                AppPage::Transcribe,
                UiRoute::Transcribe,
                "Transcribe",
                "Start recording",
                false,
            ),
            (
                "Models",
                Fixture::ModelsInstalled,
                AppPage::Models,
                UiRoute::Models,
                "Models",
                "Search models",
                true,
            ),
            (
                "Settings / General",
                Fixture::SettingsRecording,
                AppPage::General,
                UiRoute::Settings(SettingsTab::General),
                "Settings",
                "General settings",
                true,
            ),
            (
                "Settings / Recording",
                Fixture::SettingsRecording,
                AppPage::General,
                UiRoute::Settings(SettingsTab::Recording),
                "Settings",
                "Recording behavior",
                true,
            ),
            (
                "Settings / Advanced",
                Fixture::SettingsRecording,
                AppPage::General,
                UiRoute::Settings(SettingsTab::Advanced),
                "Settings",
                "Voice detection",
                true,
            ),
            (
                "Settings / About",
                Fixture::SettingsRecording,
                AppPage::General,
                UiRoute::Settings(SettingsTab::About),
                "Settings",
                "Application",
                false,
            ),
            (
                "History",
                Fixture::History,
                AppPage::History,
                UiRoute::History,
                "History",
                "Search history",
                true,
            ),
        ] {
            let ctx = egui::Context::default();
            ctx.enable_accesskit();
            configure_accessible_style(&ctx);
            let mut data = fixture.data();
            data.route = route;
            let mut page = page;

            let (output, action) =
                render_with_input(&ctx, &mut data, &mut page, WIDTH, HEIGHT, Vec::new());
            assert_eq!(action, ScreenAction::None, "{label} must render passively");
            assert_eq!(
                harness_route(page, data.route),
                route,
                "{label} must keep its route while it reflows"
            );

            let (_, offset, content_size, viewport) = ctx
                .data(|data| {
                    data.get_temp::<(egui::Id, egui::Vec2, egui::Vec2, egui::Rect)>(egui::Id::new(
                        "route-scroll-diagnostics",
                    ))
                })
                .expect("the central route scroll area must report diagnostics");
            assert!(
                viewport.left() >= -LAYOUT_TOLERANCE as f32
                    && viewport.right() <= WIDTH + LAYOUT_TOLERANCE as f32
                    && viewport.top() >= -LAYOUT_TOLERANCE as f32
                    && viewport.bottom() <= HEIGHT + LAYOUT_TOLERANCE as f32,
                "{label} route viewport escaped the {WIDTH}x{HEIGHT} shell: {viewport:?}"
            );
            assert!(
                content_size.x <= viewport.width() + LAYOUT_TOLERANCE as f32,
                "{label} created horizontal overflow: content={content_size:?}, viewport={viewport:?}"
            );
            assert!(
                offset.x.abs() <= LAYOUT_TOLERANCE as f32,
                "{label} must remain on the vertical-only route scroll axis: offset={offset:?}"
            );
            let has_vertical_overflow =
                content_size.y > viewport.height() + LAYOUT_TOLERANCE as f32;
            assert_eq!(
                has_vertical_overflow,
                scrolls_vertically,
                "{label} must {} at the minimum viewport: content={content_size:?}, viewport={viewport:?}",
                if scrolls_vertically {
                    "use the central vertical scroll area"
                } else {
                    "reflow without unnecessary vertical scrolling"
                }
            );

            assert!(
                output.shapes.iter().all(|shape| {
                    shape.clip_rect.min.x >= -LAYOUT_TOLERANCE as f32
                        && shape.clip_rect.max.x <= WIDTH + LAYOUT_TOLERANCE as f32
                        && shape.clip_rect.min.y >= -LAYOUT_TOLERANCE as f32
                        && shape.clip_rect.max.y <= HEIGHT + LAYOUT_TOLERANCE as f32
                }),
                "{label} emitted paint clipping outside the {WIDTH}x{HEIGHT} shell"
            );

            let heading_bounds = node_matching(&output, |node| {
                node.role() == egui::accesskit::Role::Heading && node.name() == Some(heading)
            })
            .bounds()
            .expect("route heading must expose AccessKit bounds");
            let viewport_bounds = egui::accesskit::Rect {
                x0: viewport.left().into(),
                y0: viewport.top().into(),
                x1: viewport.right().into(),
                y1: viewport.bottom().into(),
            };
            assert_bounds_within(heading_bounds, viewport_bounds, &format!("{label} heading"));
            assert!(
                node_names(&output)
                    .iter()
                    .any(|name| name == visible_content),
                "{label} must expose visible reference content {visible_content:?} through AccessKit"
            );
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
            (Fixture::TranscribeListening, "Recording"),
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
            (
                Fixture::ModelsDownloadDownloading,
                "Pause Whisper Parakeet download",
            ),
            (
                Fixture::ModelsDownloadRetained,
                "Resume Whisper Moonshine download",
            ),
            (
                Fixture::ModelsDownloadFailedPartial,
                "Resume Whisper Medium retained download",
            ),
            (
                Fixture::ModelsDownloadFailedAlert,
                "Show download error for Whisper Medium",
            ),
            (Fixture::ModelsCardIdle, "whisper.cpp base.en"),
            (
                Fixture::ModelsCardFocus,
                "Use whisper.cpp tiny.en for future transcriptions",
            ),
            (Fixture::ModelsCompareExpanded, "No data"),
            (Fixture::History, "Completed"),
            (Fixture::HistoryDetails, "Raw transcript"),
            (Fixture::HistoryConfirmation, "Delete permanently"),
            (Fixture::HistoryDark, "Removed or custom model"),
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
    fn isolated_download_fixtures_expose_truthful_controls_at_both_native_sizes() {
        for (fixture, expected_controls) in [
            (
                Fixture::ModelsDownloadDownloading,
                [
                    "Pause Whisper Parakeet download",
                    "Discard partial for Whisper Parakeet",
                ],
            ),
            (
                Fixture::ModelsDownloadRetained,
                [
                    "Resume Whisper Moonshine download",
                    "Discard partial for Whisper Moonshine",
                ],
            ),
            (
                Fixture::ModelsDownloadFailedPartial,
                [
                    "Resume Whisper Medium retained download",
                    "Discard partial for Whisper Medium retained",
                ],
            ),
            (
                Fixture::ModelsDownloadFailedAlert,
                [
                    "Install Whisper Medium",
                    "Show download error for Whisper Medium",
                ],
            ),
        ] {
            for (width, height) in [(1180.0, 815.0), (960.0, 680.0)] {
                let names = node_names(&render(fixture, width, height));
                for expected in expected_controls {
                    assert!(
                        names.iter().any(|name| name == expected),
                        "{fixture:?} at {width}x{height} missing {expected}"
                    );
                }
            }
        }
    }

    #[test]
    fn remote_cards_use_unknown_ratings_without_extra_state_badges() {
        let names = node_names(&render(Fixture::ModelsInstalled, 1180.0, 815.0));

        assert!(!names.iter().any(|name| name == "Experimental"));
        assert!(names.iter().any(|name| name.contains("Speed: Not rated")));
        assert!(names.iter().any(|name| name == "Accuracy: Not rated"));
        assert!(!names.iter().any(|name| name == "Trusted publisher"));
    }

    #[test]
    fn moonshine_receipt_cards_have_no_legacy_badges_in_all_card_states() {
        fn shape_texts(shape: &egui::epaint::Shape, texts: &mut Vec<String>) {
            match shape {
                egui::epaint::Shape::Text(text) => texts.push(text.galley.text().to_owned()),
                egui::epaint::Shape::Vec(shapes) => {
                    for shape in shapes {
                        shape_texts(shape, texts);
                    }
                }
                _ => {}
            }
        }

        for (width, expanded) in [
            (560.0, false),
            (560.0, true),
            (1180.0, false),
            (1180.0, true),
        ] {
            let ctx = egui::Context::default();
            ctx.enable_accesskit();
            configure_accessible_style(&ctx);
            let mut data = Fixture::ModelsInstalled.data();
            data.models.clear();
            data.model_catalog = vec![ModelViewModel {
                id: "moonshine-tiny-en-int8-onnx".into(),
                display_name: "Moonshine Tiny — English".into(),
                install_supported: true,
                install_action_enabled: true,
                primary_action_label: "Repair model".into(),
                download_state: ModelDownloadState::Failed,
                ..Default::default()
            }];
            if expanded {
                data.model_management.expanded_model_card =
                    Some(ModelCardKey::Local("moonshine-tiny-en-int8-onnx".into()));
            }
            let mut page = AppPage::Models;
            let output = render_with_input(&ctx, &mut data, &mut page, width, 815.0, Vec::new()).0;
            let names = node_names(&output);
            for legacy in ["Experimental", "CPU only", "Final text only"] {
                assert!(
                    !names.iter().any(|name| name.contains(legacy)),
                    "{legacy} must not be exposed in {width}px {expanded:?} card"
                );
            }
            assert!(names.iter().any(|name| name == "Needs repair"));
            let repair = node_matching(&output, |node| {
                node.role() == egui::accesskit::Role::Button
                    && node.name() == Some("Repair Moonshine Tiny — English")
            });
            assert!(!repair.is_disabled());
            let card = node_matching(&output, |node| {
                node.role() == egui::accesskit::Role::Group
                    && node.name() == Some("Moonshine Tiny — English model")
            });
            assert_eq!(card.is_expanded(), None);
            let details = node_matching(&output, |node| {
                node.role() == egui::accesskit::Role::Button
                    && node.name()
                        == Some(
                            format!(
                                "{} details for Moonshine Tiny — English",
                                if expanded { "Collapse" } else { "Expand" }
                            )
                            .as_str(),
                        )
            });
            assert_eq!(details.is_expanded(), Some(expanded));
            let mut texts = Vec::new();
            for shape in &output.shapes {
                shape_texts(&shape.shape, &mut texts);
            }
            for legacy in ["Experimental", "CPU only", "Final text only"] {
                assert!(
                    !texts.iter().any(|text| text.contains(legacy)),
                    "{legacy} must not be painted in {width}px {expanded:?} card"
                );
            }
        }
    }

    #[test]
    fn model_card_ratings_render_all_proportional_bins_and_truthful_unknown_meters() {
        for (guidance, label, value) in [
            ("Basic", "Basic", 1_u8),
            ("Fair", "Fair", 2),
            ("Good", "Good", 3),
            ("High", "High", 4),
            ("Highest", "Highest", 5),
        ] {
            let ctx = egui::Context::default();
            ctx.enable_accesskit();
            configure_accessible_style(&ctx);
            let mut data = Fixture::ModelsInstalled.data();
            let mut model = data.models.remove(0);
            model.accuracy_guidance = guidance.into();
            data.models = vec![model];
            data.model_catalog.clear();
            let mut page = AppPage::Models;
            let output = render_with_input(&ctx, &mut data, &mut page, 1180.0, 815.0, Vec::new()).0;
            let name = format!("Accuracy: {label} ({value} of 5)");
            let meter = node_matching(&output, |node| node.name() == Some(name.as_str()));
            assert_eq!(meter.role(), egui::accesskit::Role::Meter);
            assert_eq!(meter.min_numeric_value(), Some(0.0));
            assert_eq!(meter.max_numeric_value(), Some(5.0));
            assert_eq!(meter.numeric_value(), Some(f64::from(value)));

            let track = named_node_bounds(&output, &format!("{name} layout rating track"));
            let fill = named_node_bounds(&output, &format!("{name} layout rating fill"));
            assert_near(track.height(), 7.0, "rating track height");
            assert_near(fill.x0, track.x0, "rating fill starts at track origin");
            assert_near(
                fill.height(),
                track.height(),
                "rating fill uses full track height",
            );
            assert_near(
                fill.width(),
                track.width() * f64::from(value) / 5.0,
                "rating fill is proportional to the bin",
            );
        }

        let output = render(Fixture::ModelsInstalled, 1180.0, 815.0);
        let unknown = node_matching(&output, |node| node.name() == Some("Accuracy: Not rated"));
        assert_eq!(unknown.role(), egui::accesskit::Role::Meter);
        assert_eq!(unknown.min_numeric_value(), None);
        assert_eq!(unknown.max_numeric_value(), None);
        assert_eq!(unknown.numeric_value(), None);
        let track = named_node_bounds(&output, "Accuracy: Not rated layout rating track");
        assert_near(track.height(), 7.0, "unknown rating keeps an empty track");
        assert!(
            !node_names(&output)
                .iter()
                .any(|name| name == "Accuracy: Not rated layout rating fill"),
            "an unknown rating must not fabricate a filled bin"
        );
    }

    #[test]
    fn install_controls_show_compact_sizes_and_dispatch_local_or_remote_actions() {
        fn shape_texts(shape: &egui::epaint::Shape, texts: &mut Vec<String>) {
            match shape {
                egui::epaint::Shape::Text(text) => texts.push(text.galley.text().to_owned()),
                egui::epaint::Shape::Vec(shapes) => {
                    for shape in shapes {
                        shape_texts(shape, texts);
                    }
                }
                _ => {}
            }
        }

        let (width, height) = (1180.0, 815.0);
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        configure_accessible_style(&ctx);
        let mut local = Fixture::ModelsInstalled.data();
        local.models.clear();
        let model = ModelViewModel {
            id: "local-install".into(),
            display_name: "Local install".into(),
            total_bytes: Some(1_500_000_000),
            install_supported: true,
            install_action_enabled: true,
            languages: vec!["en".into()],
            ..Default::default()
        };
        local.model_catalog = vec![model.clone()];
        let mut page = AppPage::Models;
        let output = render_with_input(&ctx, &mut local, &mut page, width, height, Vec::new()).0;
        let install = node_matching(&output, |node| node.name() == Some("Install Local install"));
        assert!(
            install
                .bounds()
                .is_some_and(|bounds| bounds.width() >= 44.0 && bounds.height() >= 44.0)
        );
        let mut texts = Vec::new();
        for shape in &output.shapes {
            shape_texts(&shape.shape, &mut texts);
        }
        assert!(texts.iter().any(|text| text.contains("1.5 GB")));
        assert_eq!(
            click_named_control(
                &ctx,
                &mut local,
                &mut page,
                width,
                height,
                "Install Local install"
            ),
            ScreenAction::InstallModel(model.id.clone())
        );

        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        configure_accessible_style(&ctx);
        let mut remote = Fixture::ModelsInstalled.data();
        remote.models.clear();
        remote.model_catalog.clear();
        remote.remote_catalog.entries[0].variants[0].size_bytes = 82_000_000;
        let mut page = AppPage::Models;
        let output = render_with_input(&ctx, &mut remote, &mut page, width, height, Vec::new()).0;
        let remote_name = "Compact English (compact-english-q5.gguf)";
        let install = node_matching(&output, |node| {
            node.name() == Some(format!("Install {remote_name}").as_str())
        });
        assert!(
            install
                .bounds()
                .is_some_and(|bounds| bounds.width() >= 44.0 && bounds.height() >= 44.0)
        );
        let mut texts = Vec::new();
        for shape in &output.shapes {
            shape_texts(&shape.shape, &mut texts);
        }
        assert!(texts.iter().any(|text| text.contains("82 MB")));
        assert_eq!(
            click_named_control(
                &ctx,
                &mut remote,
                &mut page,
                width,
                height,
                &format!("Install {remote_name}")
            ),
            ScreenAction::InstallRemoteCatalogVariant {
                remote_model_id: "trusted-speech/compact-english".into(),
                variant_id: "compact-english-q5".into(),
            }
        );
    }

    #[test]
    fn comparison_panel_stays_near_the_bottom_without_infinite_scroll_spacing() {
        for (fixture, expanded) in [
            (Fixture::ModelsInstalled, false),
            (Fixture::ModelsCompareExpanded, true),
        ] {
            let output = render(fixture, 1180.0, 815.0);
            let bounds = named_node_bounds(&output, "Compare installed models");
            let surface = named_node_bounds(&output, "Model comparison surface");
            assert!(
                bounds.y0 >= surface.y0 - LAYOUT_TOLERANCE
                    && bounds.y1 <= surface.y1 + LAYOUT_TOLERANCE,
                "{fixture:?} comparison heading {bounds:?} escaped its dock surface {surface:?}"
            );
            assert_within_tolerance(
                surface.y1,
                815.0 - 24.0,
                LAYOUT_TOLERANCE,
                "comparison surface bottom gap",
            );
            if expanded {
                assert!(
                    surface.y1 - surface.y0 <= 815.0 * 0.6 + LAYOUT_TOLERANCE,
                    "expanded comparison surface exceeded its 60% viewport cap: {surface:?}"
                );
            } else {
                assert_within_tolerance(
                    surface.y1 - surface.y0,
                    82.0,
                    LAYOUT_TOLERANCE,
                    "collapsed comparison surface height",
                );
            }
        }
    }

    #[test]
    fn model_comparison_surface_aligns_to_the_route_content_at_supported_sizes() {
        for (width, height) in [(1180.0, 815.0), (960.0, 680.0)] {
            for (fixture, toggle_name) in [
                (Fixture::ModelsInstalled, "Expand comparison"),
                (Fixture::ModelsCompareExpanded, "Collapse comparison"),
            ] {
                let ctx = egui::Context::default();
                ctx.enable_accesskit();
                configure_accessible_style(&ctx);
                let mut data = fixture.data();
                let mut page = fixture.page();
                let output =
                    render_with_input(&ctx, &mut data, &mut page, width, height, Vec::new()).0;
                let route_viewport = ctx
                    .data(|data| {
                        data.get_temp::<egui::Rect>(egui::Id::new((
                            "route-viewport",
                            UiRoute::Models,
                        )))
                    })
                    .expect("Models route viewport diagnostic");
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
                let chevron = named_node_bounds(&output, toggle_name);

                assert_near(
                    surface.x0,
                    models.x0,
                    "surface left should align with the inset Models content",
                );
                assert_near(
                    surface.x1,
                    f64::from(route_viewport.right() - 28.0),
                    "surface right should align with the inset Models content",
                );
                assert_near(
                    chevron.x1,
                    surface.x1 - 16.0,
                    "chevron should align with the surface inner right edge",
                );
            }
        }
    }

    #[ignore = "native AccessKit tab traversal stress test hangs on Windows; run manually after accessibility runtime changes"]
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
    fn comparison_header_is_one_full_width_accessible_toggle_target() {
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
        let toggle = named_node_bounds(&initial_output, "Expand comparison");
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
            (click_point.x as f64) >= toggle.x0
                && (click_point.x as f64) <= toggle.x1
                && (click_point.y as f64) >= toggle.y0
                && (click_point.y as f64) <= toggle.y1,
            "the accessible toggle must cover the visible header"
        );
        let toggle_node = node_matching(&initial_output, |node| {
            node.name() == Some("Expand comparison")
        });
        assert_eq!(toggle_node.role(), egui::accesskit::Role::Button);
        assert_eq!(toggle_node.is_expanded(), Some(false));
        assert_eq!(
            initial_output
                .platform_output
                .accesskit_update
                .as_ref()
                .unwrap()
                .nodes
                .iter()
                .filter(|(_, node)| {
                    node.role() == egui::accesskit::Role::Button
                        && node.name() == Some("Expand comparison")
                })
                .count(),
            1,
            "the disclosure header must expose one accessible toggle"
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
        let expanded_toggle = node_matching(&expanded_output, |node| {
            node.name() == Some("Collapse comparison")
        });
        assert_eq!(expanded_toggle.role(), egui::accesskit::Role::Button);
        assert_eq!(expanded_toggle.is_expanded(), Some(true));
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
        let wide = render(Fixture::ModelsCompareExpanded, 1476.0, 1018.0);
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
        let header_bounds = ["Model", "Duration", "Processing time", "Output", "Accuracy"]
            .map(|heading| named_node_bounds(&wide, heading));
        for pair in header_bounds.windows(2) {
            assert!(
                pair[0].x1 <= pair[1].x0 + 1.0,
                "comparison headers must occupy non-overlapping columns: {pair:?}"
            );
        }
        assert!(
            !node_names(&wide).iter().any(|name| name == "Not run"),
            "initial desktop rows should not add redundant Not run lines"
        );
        let surface = named_node_bounds(&wide, "Model comparison surface");
        let start = named_node_bounds(&wide, "Start test recording");
        assert!(
            start.x1 >= surface.x1 - 20.0,
            "wide comparison action should align to the right edge"
        );

        for (width, height) in [(1180.0, 815.0), (960.0, 680.0)] {
            let compact = render(Fixture::ModelsCompareExpanded, width, height);
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
                let group = node_matching(&compact, |node| {
                    node.role() == egui::accesskit::Role::Group
                        && node.name() == Some(format!("Comparison result for {model}").as_str())
                })
                .bounds()
                .expect("compact result group should expose bounds");
                let surface = named_node_bounds(&compact, "Model comparison surface");
                assert!(
                    group.x1 - group.x0 >= surface.x1 - surface.x0 - 40.0,
                    "compact result groups should use the comparison content width"
                );
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
    }

    #[test]
    fn four_long_comparison_choices_do_not_overlap_the_recording_action() {
        let (width, height) = (960.0, 680.0);
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        configure_accessible_style(&ctx);
        let mut data = Fixture::ModelsCompareExpanded.data();
        let template = data.models[0].clone();
        data.models = [
            "whisper.cpp exceptionally-long-base-english-quantized",
            "whisper.cpp exceptionally-long-tiny-english-quantized",
            "whisper.cpp exceptionally-long-small-english-quantized",
            "whisper.cpp exceptionally-long-medium-english-quantized",
        ]
        .into_iter()
        .enumerate()
        .map(|(index, name)| {
            let mut model = template.clone();
            model.id = format!("long-{index}");
            model.display_name = name.to_owned();
            model.variant_label = format!("long-{index}");
            model.active = index == 0;
            model.recommended = false;
            model
        })
        .collect();
        data.comparison.selected_model_ids =
            data.models.iter().map(|model| model.id.clone()).collect();
        data.comparison.focus_panel = true;
        let mut page = Fixture::ModelsCompareExpanded.page();

        let output = render_with_input(&ctx, &mut data, &mut page, width, height, Vec::new()).0;
        let start = named_node_bounds(&output, "Start test recording");
        for model in &data.models {
            let choice = node_matching(&output, |node| {
                node.role() == egui::accesskit::Role::CheckBox
                    && node.name() == Some(model.display_name.as_str())
            })
            .bounds()
            .expect("comparison choice should expose bounds");
            let overlaps = choice.x0 < start.x1
                && choice.x1 > start.x0
                && choice.y0 < start.y1
                && choice.y1 > start.y0;
            assert!(
                !overlaps,
                "{} overlaps the recording action",
                model.display_name
            );
        }

        let header = named_node_bounds(&output, "Collapse comparison");
        assert!(header.y0 >= 0.0 && header.y1 <= height.into());
        assert_eq!(focused_node(&output).name(), Some("Collapse comparison"));
    }

    #[test]
    fn focused_final_comparison_action_scrolls_only_the_overfilled_dock_body() {
        for (width, height) in [(1476.0, 1018.0), (1180.0, 815.0), (960.0, 680.0)] {
            let ctx = egui::Context::default();
            ctx.enable_accesskit();
            configure_accessible_style(&ctx);
            let mut data = Fixture::ModelsCompareExpanded.data();
            let template = data.models[0].clone();
            data.models = (0..4)
                .map(|index| {
                    let mut model = template.clone();
                    model.id = format!("overfill-{index}");
                    model.display_name = format!("whisper.cpp comparison model {index}");
                    model.variant_label = format!("overfill-{index}");
                    model.active = index == 0;
                    model.recommended = false;
                    model
                })
                .collect();
            data.comparison.selected_model_ids =
                data.models.iter().map(|model| model.id.clone()).collect();
            data.comparison.results = data
                .models
                .iter()
                .map(|model| {
                    (
                        model.id.clone(),
                        ComparisonResult {
                            phase: ComparisonResultPhase::Complete,
                            output: Some("Comparison output".into()),
                            processing_ms: Some(800),
                            ..Default::default()
                        },
                    )
                })
                .collect();
            data.comparison.selection_feedback =
                Some("Comparison selection details remain available. ".repeat(80));
            let mut page = Fixture::ModelsCompareExpanded.page();

            let initial = render_with_input_at_time(
                &ctx,
                &mut data,
                &mut page,
                width,
                height,
                Vec::new(),
                Some(0.0),
            )
            .0;
            let header_before = named_node_bounds(&initial, "Collapse comparison");
            let target = initial
                .platform_output
                .accesskit_update
                .as_ref()
                .unwrap()
                .nodes
                .iter()
                .filter(|(_, node)| {
                    node.role() == egui::accesskit::Role::Button
                        && node.name() == Some("Add a reference transcript to measure")
                })
                .max_by(|(_, left), (_, right)| {
                    left.bounds()
                        .unwrap()
                        .y1
                        .total_cmp(&right.bounds().unwrap().y1)
                })
                .map(|(id, _)| *id)
                .expect("the final comparison result should expose an accuracy action");
            let _ = render_with_input_at_time(
                &ctx,
                &mut data,
                &mut page,
                width,
                height,
                vec![egui::Event::AccessKitActionRequest(
                    egui::accesskit::ActionRequest {
                        action: egui::accesskit::Action::Focus,
                        target,
                        data: None,
                    },
                )],
                Some(0.1),
            );
            let _ = render_with_input_at_time(
                &ctx,
                &mut data,
                &mut page,
                width,
                height,
                Vec::new(),
                Some(0.2),
            );
            let settled = render_with_input_at_time(
                &ctx,
                &mut data,
                &mut page,
                width,
                height,
                Vec::new(),
                Some(1.0),
            )
            .0;
            let target_bounds = settled
                .platform_output
                .accesskit_update
                .as_ref()
                .unwrap()
                .nodes
                .iter()
                .find(|(id, _)| *id == target)
                .and_then(|(_, node)| node.bounds())
                .expect("the focused final action should remain accessible");
            let (_, body_offset, body_content, body_viewport) = ctx
                .data(|data| {
                    data.get_temp::<(egui::Id, egui::Vec2, egui::Vec2, egui::Rect)>(egui::Id::new(
                        "comparison-body-scroll-diagnostics",
                    ))
                })
                .expect("comparison body should expose its settled test state");
            assert!(
                body_content.y > body_viewport.height() && body_offset.y > 0.0,
                "final comparison focus must advance the overflowing dock body at {width}x{height}"
            );
            let visible_y0 = target_bounds.y0 - f64::from(body_offset.y);
            let visible_y1 = target_bounds.y1 - f64::from(body_offset.y);
            assert!(
                visible_y0 >= f64::from(body_viewport.min.y) - LAYOUT_TOLERANCE
                    && visible_y1 <= f64::from(body_viewport.max.y) + LAYOUT_TOLERANCE,
                "focused final action must be visible in the comparison body; bounds={target_bounds:?}, offset={body_offset:?}, viewport={body_viewport:?}"
            );
            let header_after = named_node_bounds(&settled, "Collapse comparison");
            assert_near(header_after.y0, header_before.y0, "fixed dock header y0");
            assert_near(header_after.y1, header_before.y1, "fixed dock header y1");

            let (_, route_offset, _, _) = ctx
                .data(|data| {
                    data.get_temp::<(egui::Id, egui::Vec2, egui::Vec2, egui::Rect)>(egui::Id::new(
                        "route-scroll-diagnostics",
                    ))
                })
                .expect("outer route should expose its settled test state");
            assert_eq!(
                route_offset,
                egui::Vec2::ZERO,
                "comparison-body focus must not move the outer Models route"
            );
        }
    }

    #[test]
    fn completed_comparison_keeps_the_full_output_accessible_when_visually_truncated() {
        let mut data = Fixture::ModelsCompareExpanded.data();
        let long_output = "This is a realistic multi-sentence comparison transcript. It stays available in full even when the compact table column uses an ellipsis.";
        data.comparison.results = vec![(
            "base.en".into(),
            ComparisonResult {
                phase: ComparisonResultPhase::Complete,
                output: Some(long_output.into()),
                ..Default::default()
            },
        )];
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        configure_accessible_style(&ctx);
        let mut page = Fixture::ModelsCompareExpanded.page();
        let output = render_with_input(&ctx, &mut data, &mut page, 1476.0, 1018.0, Vec::new()).0;

        assert!(
            output
                .platform_output
                .accesskit_update
                .as_ref()
                .unwrap()
                .nodes
                .iter()
                .any(|(_, node)| {
                    node.role() == egui::accesskit::Role::Cell
                        && node.name()
                            == Some(
                                format!("Output for whisper.cpp base.en: {long_output}").as_str(),
                            )
                })
        );
    }

    #[test]
    fn initial_expanded_compact_results_fit_the_1180_reference_viewport() {
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
            "Start test recording",
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
        assert_eq!(data.route, UiRoute::Settings(SettingsTab::General));
        assert_eq!(
            harness_route(page, data.route),
            UiRoute::Settings(SettingsTab::General)
        );
    }

    #[test]
    fn playground_actions_remain_inside_settings_and_back_returns_to_advanced() {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        configure_accessible_style(&ctx);
        let mut data = Fixture::SettingsRecording.data();
        data.settings.debug_mode = true;
        data.route = UiRoute::Settings(SettingsTab::Advanced);
        let mut page = AppPage::General;
        let (width, height) = (900.0, 3_000.0);

        let open_action = click_named_control(
            &ctx,
            &mut data,
            &mut page,
            width,
            height,
            "Open model Playground",
        );
        assert_eq!(open_action, ScreenAction::OpenDeveloperPlayground);
        apply_action(&mut data, &mut page, open_action);
        assert_eq!(page, AppPage::General);
        assert_eq!(data.route, UiRoute::Settings(SettingsTab::Advanced));
        assert!(data.settings_playground_open);

        let playground = render_with_input(&ctx, &mut data, &mut page, width, height, Vec::new()).0;
        let visible = node_names(&playground);
        assert!(visible.iter().any(|name| name == "Settings"));
        assert!(visible.iter().any(|name| name == "Developer Playground"));
        assert!(visible.iter().any(|name| name == "Back to Advanced"));
        let back_bounds = named_node_bounds(&playground, "Back to Advanced");
        assert!(back_bounds.x1 - back_bounds.x0 >= 44.0 && back_bounds.y1 - back_bounds.y0 >= 44.0);

        let back_action = click_named_control(
            &ctx,
            &mut data,
            &mut page,
            width,
            height,
            "Back to Advanced",
        );
        assert_eq!(
            back_action,
            ScreenAction::SetSettingsTab(SettingsTab::Advanced)
        );
        apply_action(&mut data, &mut page, back_action);
        assert_eq!(page, AppPage::General);
        assert_eq!(data.route, UiRoute::Settings(SettingsTab::Advanced));
        assert!(!data.settings_playground_open);

        let advanced = render_with_input(&ctx, &mut data, &mut page, width, height, Vec::new()).0;
        let visible = node_names(&advanced);
        assert!(visible.iter().any(|name| name == "Voice detection"));
        assert!(visible.iter().any(|name| name == "Open model Playground"));
        assert!(!visible.iter().any(|name| name == "Back to Advanced"));
    }

    #[test]
    fn settings_playground_closes_when_primary_navigation_changes() {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        configure_accessible_style(&ctx);
        let mut data = Fixture::SettingsRecording.data();
        data.settings.debug_mode = true;
        data.route = UiRoute::Settings(SettingsTab::Advanced);
        let mut page = AppPage::General;
        let (width, height) = (900.0, 3_000.0);

        let open_action = click_named_control(
            &ctx,
            &mut data,
            &mut page,
            width,
            height,
            "Open model Playground",
        );
        apply_action(&mut data, &mut page, open_action);
        assert!(data.settings_playground_open);

        let navigation_action =
            click_named_control(&ctx, &mut data, &mut page, width, height, "Models");
        assert_eq!(navigation_action, ScreenAction::None);
        assert_eq!(page, AppPage::Models);
        assert!(!data.settings_playground_open);

        let models = render_with_input(&ctx, &mut data, &mut page, width, height, Vec::new()).0;
        let visible = node_names(&models);
        assert!(visible.iter().any(|name| name == "Models"));
        assert!(!visible.iter().any(|name| name == "Developer Playground"));
        assert!(!visible.iter().any(|name| name == "Back to Advanced"));
    }

    #[test]
    fn settings_final_control_is_reachable_through_the_route_scroll_area() {
        let (width, height) = (960.0, 680.0);
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        configure_accessible_style(&ctx);
        let mut data = Fixture::SettingsRecording.data();
        data.route = UiRoute::Settings(SettingsTab::Advanced);
        let mut page = AppPage::General;
        let initial = render_with_input(&ctx, &mut data, &mut page, width, height, Vec::new()).0;
        let target = named_node_id(&initial, "Enable model Playground");
        let focused = render_with_input_at_time(
            &ctx,
            &mut data,
            &mut page,
            width,
            height,
            vec![egui::Event::AccessKitActionRequest(
                egui::accesskit::ActionRequest {
                    action: egui::accesskit::Action::Focus,
                    target,
                    data: None,
                },
            )],
            Some(0.1),
        )
        .0;
        assert_eq!(
            focused_node(&focused).name(),
            Some("Enable model Playground")
        );
        let _ = render_with_input_at_time(
            &ctx,
            &mut data,
            &mut page,
            width,
            height,
            Vec::new(),
            Some(0.2),
        );
        let settled = render_with_input_at_time(
            &ctx,
            &mut data,
            &mut page,
            width,
            height,
            Vec::new(),
            Some(1.0),
        )
        .0;
        let final_bounds = named_node_bounds(&settled, "Enable model Playground");
        let route_scroll = ctx
            .data(|data| {
                data.get_temp::<(egui::Id, egui::Vec2, egui::Vec2, egui::Rect)>(egui::Id::new(
                    "route-scroll-diagnostics",
                ))
            })
            .expect("route scroll area should report its settled test state");
        let (_, offset, content_size, viewport) = route_scroll;
        assert!(
            offset.y > 0.0 && content_size.y > viewport.height(),
            "focusing the final Settings control must advance the overflowing route scroll area"
        );
        let visible_y0 = final_bounds.y0 - f64::from(offset.y);
        let visible_y1 = final_bounds.y1 - f64::from(offset.y);
        assert!(
            visible_y0 >= f64::from(viewport.min.y) && visible_y1 <= f64::from(viewport.max.y),
            "focusing the final Settings control must scroll it into the compact route viewport; content_bounds={final_bounds:?}, offset={offset:?}, viewport={viewport:?}",
        );
    }

    #[test]
    fn settings_recording_fixture_contains_visible_live_meter_signal() {
        let data = Fixture::SettingsRecording.data();
        assert_eq!(
            data.settings.voice_detection_mode,
            SpeechDetectionMode::ManualThreshold
        );
        assert_eq!(data.settings.input_threshold_dbfs, -42.0);
        assert_eq!(data.settings.input_level_percent, 68);
        assert!(
            data.settings.input_level_percent
                > ((data.settings.input_threshold_dbfs + 72.0) / 72.0 * 100.0) as u8,
            "the deterministic fixture should visibly cross the configured threshold"
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
    fn model_cards_expose_accordion_and_semantic_contracts() {
        let collapsed = render(Fixture::ModelsInstalled, 1180.0, 815.0);
        let collapsed_names = node_names(&collapsed);
        let collapsed_nodes = &collapsed
            .platform_output
            .accesskit_update
            .as_ref()
            .unwrap()
            .nodes;
        let active_count = collapsed_nodes
            .iter()
            .filter(|(_, node)| node.name() == Some("Active"))
            .count();
        assert_eq!(active_count, 1, "Active must be the sole card badge");
        assert!(collapsed_nodes.iter().any(|(_, node)| {
            node.name() == Some("Expand details for whisper.cpp tiny.en")
                && node.is_expanded() == Some(false)
        }));
        for name in ["Speed: Very fast (5 of 5)", "Accuracy: Basic (1 of 5)"] {
            assert!(
                collapsed_nodes
                    .iter()
                    .any(|(_, node)| node.name() == Some(name))
            );
        }
        assert!(
            !collapsed_names
                .iter()
                .any(|name| name == "SPEED" || name == "ACCURACY"),
            "metric labels are painter-only; the Meter remains the single semantic node"
        );
        assert!(
            collapsed_names
                .iter()
                .filter(|name| *name == "Languages: EN")
                .count()
                >= 2,
            "collapsed cards should expose compact language semantics"
        );
        assert!(
            !collapsed_names
                .iter()
                .any(|name| matches!(name.as_str(), "400MB" | "75MB")),
            "model size belongs only in the expanded details"
        );

        let expanded = render(Fixture::ModelsCardExpanded, 1180.0, 815.0);
        let expanded_nodes = &expanded
            .platform_output
            .accesskit_update
            .as_ref()
            .unwrap()
            .nodes;
        assert!(expanded_nodes.iter().any(|(_, node)| {
            node.name() == Some("Collapse details for whisper.cpp tiny.en")
                && node.is_expanded() == Some(true)
        }));
        let description = "A compact local model for responsive dictation, long recordings, and offline language-aware transcription.";
        assert_eq!(
            node_names(&expanded)
                .iter()
                .filter(|name| name.as_str() == description)
                .count(),
            1,
            "expanded cards expose the full description exactly once"
        );
        for detail in [
            "REQUIREMENTS",
            "RAM",
            "75MB",
            "ON DISK",
            "GPU",
            "Supported",
            "FEATURES",
            "MAINTENANCE",
        ] {
            assert!(node_names(&expanded).iter().any(|name| name == detail));
        }
        assert!(
            node_names(&expanded)
                .iter()
                .any(|name| name == "Languages: EN,ES,JA"),
            "expanded identity metadata should keep the compact language summary"
        );
        assert!(
            !node_names(&expanded)
                .iter()
                .any(|name| name == "DESCRIPTION" || name == "LANGUAGES"),
            "description and languages belong in the identity stack, not duplicate details"
        );
        assert_eq!(
            node_names(&expanded)
                .iter()
                .filter(|name| name.as_str() == "Delete whisper.cpp tiny.en")
                .count(),
            1,
            "the summary row owns the single uninstall action"
        );
    }

    #[test]
    fn model_card_languages_stay_compact_with_full_names_in_tooltip_and_a11y_metadata() {
        fn text_shapes(shape: &egui::epaint::Shape, texts: &mut Vec<String>) {
            match shape {
                egui::epaint::Shape::Text(text) => texts.push(text.galley.text().to_owned()),
                egui::epaint::Shape::Vec(shapes) => {
                    for shape in shapes {
                        text_shapes(shape, texts);
                    }
                }
                _ => {}
            }
        }

        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        configure_accessible_style(&ctx);
        let mut data = Fixture::ModelsCardExpanded.data();
        let mut page = Fixture::ModelsCardExpanded.page();
        let initial = render_with_input(&ctx, &mut data, &mut page, 1180.0, 815.0, Vec::new()).0;
        let language_name = "Languages: EN,ES,JA";
        let language = node_matching(&initial, |node| node.name() == Some(language_name));
        assert_eq!(language.description(), Some("English, Spanish, Japanese"));
        let bounds = language.bounds().expect("language metadata bounds");
        render_with_input_at_time(
            &ctx,
            &mut data,
            &mut page,
            1180.0,
            815.0,
            vec![egui::Event::PointerMoved(egui::pos2(1.0, 1.0))],
            Some(0.0),
        );
        render_with_input_at_time(
            &ctx,
            &mut data,
            &mut page,
            1180.0,
            815.0,
            vec![egui::Event::PointerMoved(egui::pos2(
                ((bounds.x0 + bounds.x1) / 2.0) as f32,
                ((bounds.y0 + bounds.y1) / 2.0) as f32,
            ))],
            Some(0.1),
        );
        let hovered = render_with_input_at_time(
            &ctx,
            &mut data,
            &mut page,
            1180.0,
            815.0,
            vec![egui::Event::PointerMoved(egui::pos2(
                ((bounds.x0 + bounds.x1) / 2.0) as f32,
                ((bounds.y0 + bounds.y1) / 2.0) as f32,
            ))],
            Some(1.0),
        )
        .0;
        let mut texts = Vec::new();
        for shape in &hovered.shapes {
            text_shapes(&shape.shape, &mut texts);
        }
        assert!(
            texts
                .iter()
                .any(|text| text == "English, Spanish, Japanese"),
            "full language names should remain available in the language tooltip"
        );

        data.models[0].languages = vec!["klingon".into()];
        let unavailable =
            render_with_input(&ctx, &mut data, &mut page, 1180.0, 815.0, Vec::new()).0;
        let unavailable_name = "Languages unavailable";
        let unavailable_language =
            node_matching(&unavailable, |node| node.name() == Some(unavailable_name));
        assert_eq!(unavailable_language.description(), Some(unavailable_name));
        let unavailable_bounds = unavailable_language.bounds().expect("unavailable bounds");
        render_with_input_at_time(
            &ctx,
            &mut data,
            &mut page,
            1180.0,
            815.0,
            vec![egui::Event::PointerMoved(egui::pos2(1.0, 1.0))],
            Some(2.0),
        );
        render_with_input_at_time(
            &ctx,
            &mut data,
            &mut page,
            1180.0,
            815.0,
            vec![egui::Event::PointerMoved(egui::pos2(
                ((unavailable_bounds.x0 + unavailable_bounds.x1) / 2.0) as f32,
                ((unavailable_bounds.y0 + unavailable_bounds.y1) / 2.0) as f32,
            ))],
            Some(2.1),
        );
        let unavailable_hovered = render_with_input_at_time(
            &ctx,
            &mut data,
            &mut page,
            1180.0,
            815.0,
            vec![egui::Event::PointerMoved(egui::pos2(
                ((unavailable_bounds.x0 + unavailable_bounds.x1) / 2.0) as f32,
                ((unavailable_bounds.y0 + unavailable_bounds.y1) / 2.0) as f32,
            ))],
            Some(3.0),
        )
        .0;
        let mut unavailable_texts = Vec::new();
        for shape in &unavailable_hovered.shapes {
            text_shapes(&shape.shape, &mut unavailable_texts);
        }
        assert!(
            unavailable_texts
                .iter()
                .any(|text| text == unavailable_name),
            "unavailable languages should expose a truthful tooltip"
        );
    }

    #[test]
    fn expanded_fixture_exposes_all_feature_grid_evidence() {
        let output = render(Fixture::ModelsCardExpanded, 1180.0, 815.0);
        let names = node_names(&output);
        assert!(names.iter().any(|name| {
            name == "Features: Native streaming, Translation, Word timestamps, Batch transcription"
        }));
        for hidden in [
            "Cancellation",
            "Automatic language detection",
            "Confidence scores",
            "Custom vocabulary",
        ] {
            assert!(
                !names.iter().any(|name| name == hidden),
                "hidden feature {hidden}"
            );
        }
        assert!(
            names
                .iter()
                .any(|name| name == "Repair runtime for whisper.cpp tiny.en")
        );
    }

    #[test]
    fn expanded_requirements_use_distinct_responsive_cells() {
        let render_expanded = |width, height| {
            let ctx = egui::Context::default();
            ctx.enable_accesskit();
            configure_accessible_style(&ctx);
            let mut data = Fixture::ModelsCardExpanded.data();
            let model = data
                .models
                .iter_mut()
                .find(|model| model.id == "tiny.en")
                .expect("expanded fixture includes tiny.en");
            model.estimated_ram_bytes = Some(150_000_000);
            model.disk_bytes = Some(75_000_000);
            model.capabilities = ModelCapabilities {
                capabilities_known: true,
                ..Default::default()
            };
            let mut page = Fixture::ModelsCardExpanded.page();
            render_with_input(&ctx, &mut data, &mut page, width, height, Vec::new()).0
        };
        for (width, height) in [(1180.0, 815.0), (960.0, 680.0)] {
            let output = render_expanded(width, height);
            let card = named_node_bounds(&output, "whisper.cpp tiny.en model");
            let ram = named_node_bounds(&output, "RAM");
            let ram_value = named_node_bounds(&output, "150MB");
            let disk = named_node_bounds(&output, "ON DISK");
            let disk_value = named_node_bounds(&output, "75MB");
            let gpu = named_node_bounds(&output, "GPU");
            let gpu_value = named_node_bounds(&output, "Not supported");
            for (label, value, name) in [
                (ram, ram_value, "RAM cell"),
                (disk, disk_value, "disk cell"),
                (gpu, gpu_value, "GPU cell"),
            ] {
                assert_bounds_within(label, card, name);
                assert_bounds_within(value, card, name);
                assert!(
                    label.y1 <= value.y0 + LAYOUT_TOLERANCE,
                    "{name} label must sit above its value"
                );
            }
            assert!(ram.x1 <= disk.x0 + LAYOUT_TOLERANCE);
            assert!(disk.x1 <= gpu.x0 + LAYOUT_TOLERANCE);
        }

        let compact = render_expanded(375.0, 680.0);
        let card = named_node_bounds(&compact, "whisper.cpp tiny.en model");
        let ram = named_node_bounds(&compact, "RAM");
        let disk = named_node_bounds(&compact, "ON DISK");
        let gpu = named_node_bounds(&compact, "GPU");
        for bounds in [ram, disk, gpu] {
            assert_bounds_within(bounds, card, "compact requirement cell");
        }
        assert!(ram.y1 <= disk.y0 + LAYOUT_TOLERANCE);
        assert!(disk.y1 <= gpu.y0 + LAYOUT_TOLERANCE);
    }

    #[test]
    fn model_metric_labels_sit_above_their_continuous_meters() {
        for width in [1180.0, 960.0] {
            let output = render(Fixture::ModelsInstalled, width, 815.0);
            let speed_meter_name = "Speed: Very fast (5 of 5)";
            let accuracy_meter_name = "Accuracy: Basic (1 of 5)";
            let speed_label =
                named_node_bounds(&output, &format!("{speed_meter_name} visible label"));
            let accuracy_label =
                named_node_bounds(&output, &format!("{accuracy_meter_name} visible label"));
            let speed_meter = named_node_bounds(&output, speed_meter_name);
            let accuracy_meter = named_node_bounds(&output, accuracy_meter_name);
            assert!(speed_label.y1 <= speed_meter.y0);
            assert!(accuracy_label.y1 <= accuracy_meter.y0);
            assert!(speed_meter.x0 < accuracy_meter.x0);
            assert!(speed_meter.width() <= 62.0 + LAYOUT_TOLERANCE);
            assert!(accuracy_meter.width() <= 62.0 + LAYOUT_TOLERANCE);
        }
    }

    #[test]
    fn model_card_summary_uses_compact_feature_slots_and_globe_text_axis() {
        for width in [1180.0, 960.0] {
            let output = render(Fixture::ModelsCardExpanded, width, 815.0);
            let features = named_node_bounds(
                &output,
                "Features: Native streaming, Translation, Word timestamps, Batch transcription",
            );
            assert_near(
                features.x1 - features.x0,
                64.0,
                "four feature slots use two 28px columns with one 8px gap",
            );
            assert_near(
                features.y1 - features.y0,
                72.0,
                "four feature slots use two 32px rows with one 8px gap",
            );

            let title = named_node_bounds(&output, "whisper.cpp tiny.en");
            let description = named_node_bounds(
                &output,
                "A compact local model for responsive dictation, long recordings, and offline language-aware transcription.",
            );
            let language_row =
                named_node_bounds(&output, "whisper.cpp tiny.en layout language row");
            assert_near(
                language_row.x0,
                title.x0,
                "globe left edge must align to the identity text axis",
            );
            assert_near(
                language_row.x0,
                description.x0,
                "globe left edge must align to the description text axis",
            );
        }

        for (feature_count, expected_name, expected_width, expected_height) in [
            (1, "Features: Batch transcription", 28.0, 32.0),
            (
                2,
                "Features: Word timestamps, Batch transcription",
                64.0,
                32.0,
            ),
            (
                3,
                "Features: Translation, Word timestamps, Batch transcription",
                64.0,
                72.0,
            ),
            (
                4,
                "Features: Native streaming, Translation, Word timestamps, Batch transcription",
                64.0,
                72.0,
            ),
        ] {
            let ctx = egui::Context::default();
            ctx.enable_accesskit();
            configure_accessible_style(&ctx);
            let mut data = Fixture::ModelsCardExpanded.data();
            let tiny = data
                .models
                .iter_mut()
                .find(|model| model.id == "tiny.en")
                .expect("expanded fixture includes tiny.en");
            tiny.capabilities = ModelCapabilities {
                capabilities_known: true,
                batch_transcription: true,
                timestamps: feature_count >= 2,
                translation: feature_count >= 3,
                native_streaming: feature_count >= 4,
                ..Default::default()
            };
            data.models.retain(|model| model.id == "tiny.en");
            data.model_catalog.clear();
            data.remote_catalog.entries.clear();
            let mut page = Fixture::ModelsCardExpanded.page();
            for width in [1180.0, 960.0] {
                let output =
                    render_with_input(&ctx, &mut data, &mut page, width, 815.0, Vec::new()).0;
                let features = named_node_bounds(&output, expected_name);
                assert_near(
                    features.width(),
                    expected_width,
                    &format!("{feature_count}-feature group width"),
                );
                assert_near(
                    features.height(),
                    expected_height,
                    &format!("{feature_count}-feature group height"),
                );
            }
        }
    }

    #[test]
    fn model_card_desktop_uses_exact_three_zone_bounds() {
        for (width, height) in [(1180.0, 815.0), (960.0, 680.0)] {
            let ctx = egui::Context::default();
            ctx.enable_accesskit();
            configure_accessible_style(&ctx);
            let mut data = Fixture::ModelsCardExpanded.data();
            data.models.retain(|model| model.id == "tiny.en");
            data.model_catalog.clear();
            data.remote_catalog.entries.clear();
            let mut page = AppPage::Models;
            let output = render_with_input(&ctx, &mut data, &mut page, width, height, Vec::new()).0;
            let card_name = "whisper.cpp tiny.en";
            let identity = named_node_bounds(&output, &format!("{card_name} layout identity zone"));
            let metrics = named_node_bounds(&output, &format!("{card_name} layout metrics zone"));
            let lifecycle =
                named_node_bounds(&output, &format!("{card_name} layout lifecycle zone"));
            let chevron_zone =
                named_node_bounds(&output, &format!("{card_name} layout chevron zone"));
            let chevron = named_node_bounds(&output, &format!("Collapse details for {card_name}"));
            let summary_width = identity.width() + metrics.width() + lifecycle.width();
            assert_near(
                identity.width(),
                summary_width * 0.50,
                "identity zone is exactly 50%",
            );
            assert_near(
                metrics.width(),
                summary_width * 0.24,
                "metrics zone is exactly 24%",
            );
            assert_near(
                lifecycle.width(),
                summary_width * 0.26,
                "lifecycle zone is exactly 26%",
            );
            assert_near(identity.x1, metrics.x0, "identity meets metrics");
            assert_near(metrics.x1, lifecycle.x0, "metrics meets lifecycle");
            assert_near(chevron_zone.width(), 44.0, "chevron zone width");
            assert_near(chevron_zone.height(), 44.0, "chevron zone height");
            assert_near(
                chevron_zone.x1,
                lifecycle.x1,
                "chevron trails lifecycle zone",
            );
            assert_bounds_within(chevron, chevron_zone, "chevron target");

            let speed = named_node_bounds(&output, "Speed: Very fast (5 of 5)");
            let accuracy = named_node_bounds(&output, "Accuracy: Basic (1 of 5)");
            assert_near(
                speed.width(),
                accuracy.width(),
                "metric meter widths are equal",
            );
            assert_near(
                speed.x0 - metrics.x0,
                metrics.x1 - accuracy.x1,
                &format!(
                    "metric meters are symmetrically inset; metrics={metrics:?}, speed={speed:?}, accuracy={accuracy:?}"
                ),
            );
        }
    }

    #[test]
    fn model_card_desktop_metadata_group_uses_fixed_identity_ratio_and_shared_row_geometry() {
        for (width, height) in [(1180.0, 815.0), (960.0, 680.0)] {
            let output = render(Fixture::ModelsCardExpanded, width, height);
            let card_name = "whisper.cpp tiny.en";
            let rect =
                |name: &str| named_node_bounds(&output, &format!("{card_name} layout {name}"));
            let identity = rect("identity zone");
            let metadata_group = rect("metadata group");
            let language_cell = rect("language cell");
            let feature_cell = rect("feature cell");
            let language_icon = rect("language icon");
            let language_text = named_node_bounds(&output, "Languages: EN,ES,JA");
            let features = named_node_bounds(
                &output,
                "Features: Native streaming, Translation, Word timestamps, Batch transcription",
            );

            assert_near(
                metadata_group.width(),
                identity.width() * 0.60,
                "metadata group is exactly 60% of the identity zone",
            );
            assert_near(
                language_cell.width(),
                identity.width() * 0.30,
                "language cell is exactly 30% of the identity zone",
            );
            assert_near(
                feature_cell.width(),
                identity.width() * 0.30,
                "feature cell is exactly 30% of the identity zone",
            );
            assert_near(
                language_cell.x0,
                metadata_group.x0,
                "language begins the group",
            );
            assert_near(feature_cell.x1, metadata_group.x1, "features end the group");
            assert_near(language_cell.x1, feature_cell.x0, "metadata cells abut");
            assert_near(
                language_cell.y0,
                feature_cell.y0,
                "metadata cells share a row",
            );
            assert_near(
                language_cell.y1,
                feature_cell.y1,
                "metadata cells share a height",
            );
            assert_near(
                (language_icon.y0 + language_icon.y1) / 2.0,
                (language_text.y0 + language_text.y1) / 2.0,
                "language globe and text share the vertical text axis",
            );
            assert_near(
                (language_cell.y0 + language_cell.y1) / 2.0,
                (feature_cell.y0 + feature_cell.y1) / 2.0,
                "language and feature cells share the row baseline",
            );
            assert_bounds_within(language_icon, language_cell, "language globe");
            assert_bounds_within(language_text, language_cell, "language text");
            assert!(
                features.x0 >= feature_cell.x0 - LAYOUT_TOLERANCE
                    && features.x1 <= feature_cell.x1 + LAYOUT_TOLERANCE,
                "feature group stays within its fixed-width cell: features={features:?}, cell={feature_cell:?}"
            );
        }
    }

    #[test]
    fn model_card_desktop_metadata_glyphs_align_with_the_first_feature_row() {
        for (width, height) in [(1180.0, 815.0), (960.0, 680.0)] {
            let output = render(Fixture::ModelsCardExpanded, width, height);
            let card_name = "whisper.cpp tiny.en";
            let rect =
                |name: &str| named_node_bounds(&output, &format!("{card_name} layout {name}"));
            let language_cell = rect("language cell");
            let feature_cell = rect("feature cell");
            let globe = painted_text_bounds_in(
                &output,
                crate::ui::controls::icon_glyph(crate::ui::controls::Icon::Globe),
                language_cell,
            );
            let language = painted_text_bounds_in(&output, "EN,ES,JA", language_cell);
            let native_streaming = painted_text_bounds_in(
                &output,
                crate::ui::controls::icon_glyph(crate::ui::controls::Icon::Streaming),
                feature_cell,
            );
            let globe_center = f64::from(globe.center().y);
            let language_center = f64::from(language.center().y);
            let native_streaming_center = f64::from(native_streaming.center().y);

            assert_near(
                globe_center,
                native_streaming_center,
                &format!(
                    "painted globe aligns with the first feature glyph at {width}x{height}; globe={globe_center}, language={language_center}, native streaming={native_streaming_center}"
                ),
            );
            assert_near(
                language_center,
                native_streaming_center,
                &format!(
                    "painted language text aligns with the first feature glyph at {width}x{height}; globe={globe_center}, language={language_center}, native streaming={native_streaming_center}"
                ),
            );
        }
    }

    #[test]
    fn expanded_model_details_keep_compact_section_density() {
        let output = render(Fixture::ModelsCardExpanded, 1180.0, 815.0);
        let card_name = "whisper.cpp tiny.en";
        let rect = |name: &str| named_node_bounds(&output, &format!("{card_name} layout {name}"));
        for (name, expected_height) in [
            ("gap before divider", 6.0),
            ("gap after divider", 6.0),
            ("features heading content gap", 6.0),
            ("expanded feature row gap 1", 4.0),
            ("features requirements gap", 12.0),
            ("requirements heading content gap", 6.0),
            ("requirements maintenance gap", 12.0),
            ("maintenance heading content gap", 6.0),
        ] {
            assert_near(
                rect(name).height(),
                expected_height,
                &format!("{name} height"),
            );
        }

        let feature_row_0 = rect("expanded feature row 0");
        let feature_row_1 = rect("expanded feature row 1");
        assert_near(feature_row_0.height(), 32.0, "first feature row height");
        assert_near(feature_row_1.height(), 32.0, "second feature row height");
        assert_near(
            rect("expanded feature row gap 1").y0,
            feature_row_0.y1,
            "feature row gap starts immediately after first row",
        );
        assert_near(
            rect("expanded feature row gap 1").y1,
            feature_row_1.y0,
            "second feature row starts immediately after its gap",
        );

        let features_heading = rect("features heading");
        let features_content = rect("features content");
        let requirements_heading = rect("requirements heading");
        let requirements_content = rect("requirements content");
        let maintenance_heading = rect("maintenance heading");
        assert!(features_heading.y1 <= features_content.y0);
        assert!(features_content.y1 <= requirements_heading.y0);
        assert!(requirements_heading.y1 <= requirements_content.y0);
        assert!(requirements_content.y1 <= maintenance_heading.y0);
        assert!(
            requirements_content.height() <= 44.0 + LAYOUT_TOLERANCE,
            "requirement cells should keep their natural compact height"
        );
    }

    #[test]
    fn expanded_features_render_all_capabilities_as_icon_label_grid() {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        configure_accessible_style(&ctx);
        let mut data = Fixture::ModelsInstalled.data();
        let mut model = data.models.remove(0);
        model.id = "full-features".into();
        model.display_name = "Full features".into();
        model.capabilities = ModelCapabilities {
            capabilities_known: true,
            batch_transcription: true,
            native_streaming: true,
            cancellation: true,
            timestamps: true,
            translation: true,
            language_detection: true,
            confidence_scores: true,
            custom_vocabulary: true,
            cpu: true,
            gpu: true,
        };
        data.models = vec![model.clone()];
        data.model_catalog.clear();
        data.model_management.expanded_model_card = Some(ModelCardKey::Local(model.id.clone()));
        let mut page = AppPage::Models;
        let output = render_with_input(&ctx, &mut data, &mut page, 1180.0, 815.0, Vec::new()).0;
        let names = node_names(&output);
        for capability in [
            "Native streaming",
            "Translation",
            "Word timestamps",
            "Batch transcription",
        ] {
            assert!(
                names.iter().any(|name| name == capability),
                "missing {capability}"
            );
        }
        for hidden in [
            "Cancellation",
            "Automatic language detection",
            "Confidence scores",
            "Custom vocabulary",
        ] {
            assert!(
                !names.iter().any(|name| name == hidden),
                "hidden feature {hidden}"
            );
        }
    }

    #[test]
    fn feature_tooltip_hit_regions_wrap_and_never_select_the_card() {
        fn text_shapes(shape: &egui::epaint::Shape, texts: &mut Vec<String>) {
            match shape {
                egui::epaint::Shape::Text(text) => texts.push(text.galley.text().to_owned()),
                egui::epaint::Shape::Vec(shapes) => {
                    for shape in shapes {
                        text_shapes(shape, texts);
                    }
                }
                _ => {}
            }
        }

        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        configure_accessible_style(&ctx);
        let mut data = Fixture::ModelsInstalled.data();
        let mut model = data.models.remove(0);
        model.id = "priority-features".into();
        model.display_name = "Priority features".into();
        model.capabilities = ModelCapabilities {
            capabilities_known: true,
            batch_transcription: true,
            native_streaming: true,
            cancellation: true,
            timestamps: true,
            translation: true,
            language_detection: true,
            confidence_scores: true,
            custom_vocabulary: true,
            cpu: true,
            gpu: true,
        };
        data.models = vec![model];
        data.model_catalog.clear();
        let mut page = AppPage::Models;
        let initial = render_with_input(&ctx, &mut data, &mut page, 1180.0, 815.0, Vec::new()).0;
        let feature_name =
            "Features: Native streaming, Translation, Word timestamps, Batch transcription";
        let feature_group = node_matching(&initial, |node| node.name() == Some(feature_name));
        assert_eq!(feature_group.role(), egui::accesskit::Role::Group);
        let nodes = &initial
            .platform_output
            .accesskit_update
            .as_ref()
            .unwrap()
            .nodes;
        assert_eq!(
            nodes
                .iter()
                .filter(|(_, node)| node.name() == Some(feature_name))
                .count(),
            1
        );
        assert!(!nodes.iter().any(|(_, node)| {
            matches!(
                node.role(),
                egui::accesskit::Role::Button | egui::accesskit::Role::Link
            ) && [
                "Native streaming",
                "Translation",
                "Word timestamps",
                "Batch transcription",
            ]
            .contains(&node.name().unwrap_or_default())
        }));
        let bounds = feature_group.bounds().unwrap();
        assert!(
            bounds.height() >= 72.0 - LAYOUT_TOLERANCE,
            "four summary icons should fit within a two-column, two-row group: {bounds:?}"
        );
        for (index, tooltip) in [
            "Native streaming",
            "Translation",
            "Word timestamps",
            "Batch transcription",
        ]
        .into_iter()
        .enumerate()
        {
            let time = index as f64 * 2.0;
            let (_, move_away_action) = render_with_input_at_time(
                &ctx,
                &mut data,
                &mut page,
                1180.0,
                815.0,
                vec![egui::Event::PointerMoved(egui::pos2(1.0, 1.0))],
                Some(time),
            );
            assert_eq!(move_away_action, ScreenAction::None);
            let pointer = egui::pos2(
                bounds.x0 as f32 + 14.0 + (index % 2) as f32 * 36.0,
                bounds.y0 as f32 + 16.0 + (index / 2) as f32 * 40.0,
            );
            let (_, hover_start_action) = render_with_input_at_time(
                &ctx,
                &mut data,
                &mut page,
                1180.0,
                815.0,
                vec![egui::Event::PointerMoved(pointer)],
                Some(time + 0.1),
            );
            assert_eq!(hover_start_action, ScreenAction::None);
            let (hovered, hover_action) = render_with_input_at_time(
                &ctx,
                &mut data,
                &mut page,
                1180.0,
                815.0,
                vec![egui::Event::PointerMoved(pointer)],
                Some(time + 1.0),
            );
            assert_eq!(
                hover_action,
                ScreenAction::None,
                "tooltip {tooltip} must not select the card"
            );
            let mut texts = Vec::new();
            for shape in &hovered.shapes {
                text_shapes(&shape.shape, &mut texts);
            }
            assert!(
                texts.iter().any(|text| text == tooltip),
                "missing tooltip {tooltip}"
            );
        }
    }

    #[test]
    fn feature_summary_uses_batch_transcription_as_the_known_fallback() {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        configure_accessible_style(&ctx);
        let mut data = Fixture::ModelsInstalled.data();
        let mut model = data.models.remove(0);
        model.capabilities = ModelCapabilities {
            capabilities_known: true,
            batch_transcription: true,
            ..Default::default()
        };
        data.models = vec![model];
        data.model_catalog.clear();
        let mut page = AppPage::Models;
        let output = render_with_input(&ctx, &mut data, &mut page, 1180.0, 815.0, Vec::new()).0;
        let feature_name = "Features: Batch transcription";
        let nodes = &output
            .platform_output
            .accesskit_update
            .as_ref()
            .unwrap()
            .nodes;
        assert_eq!(
            nodes
                .iter()
                .filter(|(_, node)| node.name() == Some(feature_name))
                .count(),
            1
        );
        assert_eq!(
            node_matching(&output, |node| node.name() == Some(feature_name)).role(),
            egui::accesskit::Role::Group
        );
    }

    #[test]
    fn expanded_features_distinguish_known_empty_from_unknown() {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        configure_accessible_style(&ctx);
        let mut data = Fixture::ModelsInstalled.data();
        let mut known_empty = data.models.remove(0);
        known_empty.id = "known-empty".into();
        known_empty.display_name = "Known empty".into();
        known_empty.capabilities = ModelCapabilities {
            capabilities_known: true,
            cancellation: true,
            language_detection: true,
            confidence_scores: true,
            custom_vocabulary: true,
            ..Default::default()
        };
        let mut unknown = known_empty.clone();
        unknown.id = "unknown".into();
        unknown.display_name = "Unknown".into();
        unknown.capabilities = ModelCapabilities::default();
        data.models = vec![known_empty.clone(), unknown.clone()];
        data.model_catalog.clear();
        let mut page = AppPage::Models;
        for (model, expected) in [
            (known_empty, "No supported features"),
            (unknown, "Feature support is unknown"),
        ] {
            data.model_management.expanded_model_card = Some(ModelCardKey::Local(model.id));
            let output = render_with_input(&ctx, &mut data, &mut page, 1180.0, 815.0, Vec::new()).0;
            assert!(node_names(&output).iter().any(|name| name == expected));
        }
    }

    #[test]
    fn model_card_lifecycle_controls_dispatch_matching_actions() {
        let (width, height) = (1180.0, 815.0);
        let cases = [
            (
                "Install",
                ModelDownloadState::NotInstalled,
                false,
                false,
                false,
                false,
                ScreenAction::InstallModel("lifecycle".into()),
            ),
            (
                "Install",
                ModelDownloadState::Failed,
                false,
                false,
                false,
                false,
                ScreenAction::InstallModel("lifecycle".into()),
            ),
            (
                "Resume Lifecycle download",
                ModelDownloadState::Cancelled,
                true,
                false,
                false,
                false,
                ScreenAction::InstallModel("lifecycle".into()),
            ),
            (
                "Pause Lifecycle download",
                ModelDownloadState::Downloading,
                false,
                false,
                false,
                false,
                ScreenAction::CancelModelInstall("lifecycle".into()),
            ),
            (
                "Delete",
                ModelDownloadState::Installed,
                false,
                true,
                false,
                false,
                ScreenAction::RequestModelRemoval("lifecycle".into()),
            ),
            (
                "Upgrade",
                ModelDownloadState::Installed,
                false,
                true,
                true,
                false,
                ScreenAction::UpgradeModel("lifecycle".into()),
            ),
            (
                "Repair",
                ModelDownloadState::Installed,
                false,
                true,
                false,
                true,
                ScreenAction::RepairModelRuntime("lifecycle".into()),
            ),
        ];

        for (label, download_state, partial, installed, upgrade, repair, expected) in cases {
            let ctx = egui::Context::default();
            ctx.enable_accesskit();
            configure_accessible_style(&ctx);
            let mut data = Fixture::ModelsInstalled.data();
            let mut model = ModelViewModel {
                id: "lifecycle".into(),
                display_name: "Lifecycle".into(),
                installed,
                ready: installed && !upgrade && !repair,
                install_supported: true,
                install_action_enabled: true,
                cancel_supported: true,
                removal_supported: true,
                primary_action_enabled: true,
                primary_action_installs_upgrade: upgrade,
                primary_action_repairs_runtime: repair,
                download_state,
                partial_cleanup_available: partial,
                languages: vec!["en".into()],
                ..Default::default()
            };
            if label == "Install" {
                model.download_state = ModelDownloadState::NotInstalled;
            }
            data.models = installed.then_some(model.clone()).into_iter().collect();
            data.model_catalog = (!installed).then_some(model).into_iter().collect();
            let mut page = AppPage::Models;
            let name = if label.ends_with(" download") {
                label.to_owned()
            } else {
                format!("{label} Lifecycle")
            };
            assert_eq!(
                click_named_control(&ctx, &mut data, &mut page, width, height, &name),
                expected,
                "{label}"
            );
        }

        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        configure_accessible_style(&ctx);
        let mut data = Fixture::ModelsInstalled.data();
        data.models.clear();
        data.model_catalog = vec![ModelViewModel {
            id: "lifecycle".into(),
            display_name: "Lifecycle".into(),
            download_state: ModelDownloadState::Verifying,
            install_supported: true,
            languages: vec!["en".into()],
            ..Default::default()
        }];
        let mut page = AppPage::Models;
        let output = render_with_input(&ctx, &mut data, &mut page, width, height, Vec::new()).0;
        let installing = node_matching(&output, |node| node.name() == Some("Installing Lifecycle"));
        assert!(installing.is_disabled());
        assert!(
            installing
                .description()
                .is_some_and(|text| text.contains("cannot cancel"))
        );
    }

    #[test]
    fn queued_and_waiting_local_installs_have_truthful_named_cancel_controls() {
        for (state, status, name) in [
            (
                ModelDownloadState::Queued,
                "Queued",
                "Cancel Lifecycle queued download",
            ),
            (
                ModelDownloadState::WaitingForVerification,
                "Waiting for verification",
                "Cancel Lifecycle waiting verification",
            ),
        ] {
            let ctx = egui::Context::default();
            ctx.enable_accesskit();
            configure_accessible_style(&ctx);
            let mut data = Fixture::ModelsInstalled.data();
            data.models.clear();
            data.model_catalog = vec![ModelViewModel {
                id: "lifecycle".into(),
                display_name: "Lifecycle".into(),
                download_state: state,
                cancel_supported: true,
                install_supported: true,
                languages: vec!["en".into()],
                ..Default::default()
            }];
            let mut page = AppPage::Models;
            let output = render_with_input(&ctx, &mut data, &mut page, 1180.0, 815.0, Vec::new()).0;
            let cancel = node_matching(&output, |node| node.name() == Some(name));
            assert!(!cancel.is_disabled(), "{state:?}");
            assert_eq!(cancel.description(), None, "{state:?}");
            node_matching(&output, |node| node.name() == Some(status));
            assert_eq!(
                click_named_control(&ctx, &mut data, &mut page, 1180.0, 815.0, name),
                ScreenAction::CancelModelInstall("lifecycle".into()),
                "{state:?}"
            );
        }
    }

    #[test]
    fn queued_and_waiting_remote_installs_have_truthful_named_cancel_controls() {
        for status in ["Queued for download", "Waiting for verification"] {
            let ctx = egui::Context::default();
            ctx.enable_accesskit();
            configure_accessible_style(&ctx);
            let mut data = Fixture::ModelsInstalled.data();
            data.models.clear();
            data.model_catalog.clear();
            let variant = &mut data.remote_catalog.entries[0].variants[0];
            variant.status_label = Some(status.into());
            variant.downloaded_bytes = Some(0);
            variant.total_bytes = Some(82_000_000);
            variant.actions = vec![RemoteCatalogActionView {
                label: "Cancel".into(),
                kind: RemoteCatalogActionKind::Cancel {
                    model_id: "managed-compact-english".into(),
                },
                enabled: true,
                disabled_reason: None,
            }];
            let mut page = AppPage::Models;
            let cancel_name = "Cancel Compact English (compact-english-q5.gguf)";
            let output = render_with_input(&ctx, &mut data, &mut page, 1180.0, 815.0, Vec::new()).0;
            let cancel = node_matching(&output, |node| node.name() == Some(cancel_name));
            assert!(!cancel.is_disabled(), "{status}");
            assert_eq!(cancel.description(), None, "{status}");
            node_matching(&output, |node| node.name() == Some(status));
            node_matching(&output, |node| {
                node.role() == egui::accesskit::Role::Group
                    && node.name() == Some("Compact English (compact-english-q5.gguf) model")
            });
            assert!(
                !output
                    .platform_output
                    .accesskit_update
                    .as_ref()
                    .unwrap()
                    .nodes
                    .iter()
                    .any(|(_, node)| {
                        node.role() == egui::accesskit::Role::Meter
                            && node
                                .name()
                                .is_some_and(|name| name.starts_with("Downloading"))
                    }),
                "{status} must not expose a download meter",
            );
            assert_eq!(
                click_named_control(&ctx, &mut data, &mut page, 1180.0, 815.0, cancel_name,),
                ScreenAction::CancelRemoteCatalogInstall("managed-compact-english".into()),
                "{status}"
            );
        }
    }

    #[test]
    fn remote_variants_have_unique_group_and_cancel_names() {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        configure_accessible_style(&ctx);
        let mut data = Fixture::ModelsInstalled.data();
        data.models.clear();
        data.model_catalog.clear();
        let first = &mut data.remote_catalog.entries[0].variants[0];
        first.status_label = Some("Queued for download".into());
        first.actions = vec![RemoteCatalogActionView {
            label: "Cancel".into(),
            kind: RemoteCatalogActionKind::Cancel {
                model_id: "managed-compact-english-q5".into(),
            },
            enabled: true,
            disabled_reason: None,
        }];
        let mut second = first.clone();
        second.id = "compact-english-q4".into();
        second.filename = "compact-english-q4.gguf".into();
        second.actions = vec![RemoteCatalogActionView {
            label: "Cancel".into(),
            kind: RemoteCatalogActionKind::Cancel {
                model_id: "managed-compact-english-q4".into(),
            },
            enabled: true,
            disabled_reason: None,
        }];
        data.remote_catalog.entries[0].variants.push(second);
        let mut page = AppPage::Models;
        let output = render_with_input(&ctx, &mut data, &mut page, 1180.0, 815.0, Vec::new()).0;

        for filename in ["compact-english-q5.gguf", "compact-english-q4.gguf"] {
            let qualified = format!("Compact English ({filename})");
            node_matching(&output, |node| {
                node.role() == egui::accesskit::Role::Group
                    && node.name() == Some(format!("{qualified} model").as_str())
            });
            node_matching(&output, |node| {
                node.role() == egui::accesskit::Role::Button
                    && node.name() == Some(format!("Cancel {qualified}").as_str())
            });
        }
    }

    #[test]
    fn concurrent_install_summary_is_one_atomic_polite_status() {
        let summary = "Installing 3 models: 2 downloading, 1 queued, 0 waiting for verification, 0 verifying.";
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        configure_accessible_style(&ctx);
        let mut data = Fixture::ModelsInstalled.data();
        data.model_management.install_status_summary = Some(summary.into());
        let mut page = AppPage::Models;
        let output = render_with_input(&ctx, &mut data, &mut page, 1180.0, 815.0, Vec::new()).0;
        assert_polite_atomic_notice(&output, summary);
        assert_eq!(
            output
                .platform_output
                .accesskit_update
                .as_ref()
                .unwrap()
                .nodes
                .iter()
                .filter(|(_, node)| node.name() == Some(summary))
                .count(),
            1
        );
    }

    #[test]
    fn active_download_progress_is_truthful_clamped_and_isolated_from_card_selection() {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        configure_accessible_style(&ctx);
        let mut data = Fixture::ModelsInstalled.data();
        data.models = vec![ModelViewModel {
            id: "progress".into(),
            display_name: "Progress".into(),
            installed: true,
            ready: true,
            download_state: ModelDownloadState::Downloading,
            downloaded_bytes: 120,
            total_bytes: Some(100),
            cancel_supported: true,
            languages: vec!["en".into()],
            ..Default::default()
        }];
        data.model_catalog.clear();
        data.remote_catalog.entries.clear();
        let mut page = AppPage::Models;
        let output = render_with_input(&ctx, &mut data, &mut page, 1180.0, 815.0, Vec::new()).0;
        let accessible_progress = "Downloading 120B of 100B, 100% complete";
        let meter = node_matching(&output, |node| {
            node.role() == egui::accesskit::Role::Meter && node.name() == Some(accessible_progress)
        });
        assert_eq!(meter.min_numeric_value(), Some(0.0));
        assert_eq!(meter.max_numeric_value(), Some(1.0));
        assert_eq!(meter.numeric_value(), Some(1.0));
        let track = named_node_bounds(
            &output,
            &format!("{accessible_progress} layout download track"),
        );
        let fill = named_node_bounds(
            &output,
            &format!("{accessible_progress} layout download fill"),
        );
        let lifecycle = named_node_bounds(&output, "Progress layout lifecycle zone");
        let chevron = named_node_bounds(&output, "Progress layout chevron zone");
        assert_near(track.height(), 6.0, "download track height");
        assert_near(fill.height(), 6.0, "download fill height");
        assert_near(fill.x0, track.x0, "download fill starts at track origin");
        assert_near(fill.width(), track.width(), "clamped full download fill");
        assert_bounds_within(track, lifecycle, "download track");
        assert!(track.x1 <= chevron.x0 + LAYOUT_TOLERANCE);

        let pause_name = "Pause Progress download";
        let pause = named_node_bounds(&output, pause_name);
        assert_near(pause.width(), 44.0, "Pause target width");
        assert_near(pause.height(), 44.0, "Pause target height");
        assert_eq!(
            click_named_control(&ctx, &mut data, &mut page, 1180.0, 815.0, pause_name,),
            ScreenAction::CancelModelInstall("progress".into()),
            "Pause must retain the partial and win over the selectable card target",
        );
        assert_eq!(
            click_named_control(
                &ctx,
                &mut data,
                &mut page,
                1180.0,
                815.0,
                "Discard partial for Progress",
            ),
            ScreenAction::DiscardModelPartial("progress".into()),
            "X must request the exact partial cleanup without selecting the card",
        );

        data.models[0].downloaded_bytes = 42;
        data.models[0].total_bytes = None;
        let unknown = render_with_input(&ctx, &mut data, &mut page, 1180.0, 815.0, Vec::new()).0;
        let unknown_text = "Downloading 42B; total download size unknown";
        let unknown_meter = node_matching(&unknown, |node| {
            node.role() == egui::accesskit::Role::Meter && node.name() == Some(unknown_text)
        });
        assert_eq!(unknown_meter.min_numeric_value(), None);
        assert_eq!(unknown_meter.max_numeric_value(), None);
        assert_eq!(unknown_meter.numeric_value(), None);
        let names = node_names(&unknown);
        assert!(names.iter().any(|name| name == "42B / Total unknown"));
        assert!(
            !unknown
                .platform_output
                .accesskit_update
                .as_ref()
                .unwrap()
                .nodes
                .iter()
                .any(|(_, node)| node.role() == egui::accesskit::Role::StaticText
                    && node.name() == Some("Downloading"))
        );
        assert!(
            !names
                .iter()
                .any(|name| name == &format!("{unknown_text} layout download fill")),
            "unknown totals must not fabricate a numeric fill",
        );

        data.models[0].download_state = ModelDownloadState::Failed;
        let settled = render_with_input(&ctx, &mut data, &mut page, 1180.0, 815.0, Vec::new()).0;
        assert!(
            !node_names(&settled)
                .iter()
                .any(|name| name.starts_with("Downloading 42B")),
            "progress must disappear once the model is no longer downloading",
        );
    }

    #[test]
    fn remote_download_progress_requires_live_installer_bytes() {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        configure_accessible_style(&ctx);
        let mut data = Fixture::ModelsInstalled.data();
        data.models.clear();
        data.model_catalog.clear();
        let variant = &mut data.remote_catalog.entries[0].variants[0];
        variant.status_label = Some("Downloading".into());
        variant.downloaded_bytes = None;
        variant.actions = vec![RemoteCatalogActionView {
            label: "Cancel".into(),
            kind: RemoteCatalogActionKind::Cancel {
                model_id: "managed-compact-english".into(),
            },
            enabled: true,
            disabled_reason: None,
        }];
        let mut page = AppPage::Models;
        let output = render_with_input(&ctx, &mut data, &mut page, 1180.0, 815.0, Vec::new()).0;
        assert!(
            !output
                .platform_output
                .accesskit_update
                .as_ref()
                .unwrap()
                .nodes
                .iter()
                .any(|(_, node)| {
                    node.role() == egui::accesskit::Role::Meter
                        && node
                            .name()
                            .is_some_and(|name| name.starts_with("Downloading"))
                }),
            "a status label without live byte progress must not fabricate a progress meter",
        );
    }

    #[test]
    fn remote_download_progress_uses_live_installer_bytes_and_cancels_without_card_selection() {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        configure_accessible_style(&ctx);
        let mut data = Fixture::ModelsInstalled.data();
        data.models.clear();
        data.model_catalog.clear();
        let variant = &mut data.remote_catalog.entries[0].variants[0];
        variant.status_label = Some("Downloading".into());
        variant.downloaded_bytes = Some(40);
        variant.total_bytes = Some(100);
        variant.actions = vec![RemoteCatalogActionView {
            label: "Cancel".into(),
            kind: RemoteCatalogActionKind::Cancel {
                model_id: "managed-compact-english".into(),
            },
            enabled: true,
            disabled_reason: None,
        }];
        let mut page = AppPage::Models;
        let output = render_with_input(&ctx, &mut data, &mut page, 1180.0, 815.0, Vec::new()).0;
        let accessible_progress = "Downloading 40B of 100B, 40% complete";
        let meter = node_matching(&output, |node| {
            node.role() == egui::accesskit::Role::Meter && node.name() == Some(accessible_progress)
        });
        assert_eq!(meter.min_numeric_value(), Some(0.0));
        assert_eq!(meter.max_numeric_value(), Some(1.0));
        assert_eq!(meter.numeric_value(), Some(f64::from(0.4_f32)));
        let track = named_node_bounds(
            &output,
            &format!("{accessible_progress} layout download track"),
        );
        let fill = named_node_bounds(
            &output,
            &format!("{accessible_progress} layout download fill"),
        );
        assert_near(
            fill.width(),
            track.width() * 0.4,
            "remote download fill ratio",
        );

        let pause_name = "Pause Compact English (compact-english-q5.gguf)";
        let pause = named_node_bounds(&output, pause_name);
        assert_near(pause.width(), 44.0, "remote Pause target width");
        assert_near(pause.height(), 44.0, "remote Pause target height");
        let action = click_named_control(&ctx, &mut data, &mut page, 1180.0, 815.0, pause_name);
        assert_eq!(
            action,
            ScreenAction::CancelRemoteCatalogInstall("managed-compact-english".into())
        );
        assert!(!matches!(action, ScreenAction::SelectModel(_)));
        assert_eq!(
            click_named_control(
                &ctx,
                &mut data,
                &mut page,
                1180.0,
                815.0,
                "Discard partial for Compact English (compact-english-q5.gguf)",
            ),
            ScreenAction::DiscardRemoteCatalogPartial {
                remote_model_id: "trusted-speech/compact-english".into(),
                variant_id: "compact-english-q5".into(),
            },
            "remote X must request cleanup for the exact trusted artifact",
        );
    }

    #[test]
    fn full_card_interaction_exists_only_for_ready_inactive_installed_models() {
        let (width, height) = (1180.0, 815.0);
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        configure_accessible_style(&ctx);
        let mut data = Fixture::ModelsInstalled.data();
        let mut page = AppPage::Models;
        let output = render_with_input(&ctx, &mut data, &mut page, width, height, Vec::new()).0;
        let card_target = node_matching(&output, |node| {
            node.name() == Some("Use whisper.cpp tiny.en for future transcriptions")
        });
        assert_eq!(card_target.role(), egui::accesskit::Role::Button);
        assert!(card_target.supports_action(egui::accesskit::Action::Default));
        assert!(
            card_target
                .bounds()
                .is_some_and(|bounds| bounds.height() >= 44.0)
        );
        let card_target_id =
            named_node_id(&output, "Use whisper.cpp tiny.en for future transcriptions");
        assert_eq!(
            render_with_input(
                &ctx,
                &mut data,
                &mut page,
                width,
                height,
                vec![egui::Event::AccessKitActionRequest(
                    egui::accesskit::ActionRequest {
                        action: egui::accesskit::Action::Default,
                        target: card_target_id,
                        data: None,
                    }
                )],
            )
            .1,
            ScreenAction::SelectModel("tiny.en".into())
        );

        let active = node_matching(&output, |node| {
            node.name() == Some("whisper.cpp base.en")
                && node.role() == egui::accesskit::Role::StaticText
        });
        assert!(active.bounds().is_some());
        assert!(
            !node_names(&output)
                .iter()
                .any(|name| name == "Use whisper.cpp base.en for future transcriptions")
        );

        for (display_name, model) in [
            (
                "Available title",
                ModelViewModel {
                    id: "available-title".into(),
                    display_name: "Available title".into(),
                    install_supported: true,
                    install_action_enabled: true,
                    languages: vec!["en".into()],
                    ..Default::default()
                },
            ),
            (
                "Upgrade title",
                ModelViewModel {
                    id: "upgrade-title".into(),
                    display_name: "Upgrade title".into(),
                    installed: true,
                    primary_action_enabled: true,
                    primary_action_installs_upgrade: true,
                    download_state: ModelDownloadState::Installed,
                    languages: vec!["en".into()],
                    ..Default::default()
                },
            ),
            (
                "Repair title",
                ModelViewModel {
                    id: "repair-title".into(),
                    display_name: "Repair title".into(),
                    installed: true,
                    primary_action_enabled: true,
                    primary_action_repairs_runtime: true,
                    download_state: ModelDownloadState::Installed,
                    languages: vec!["en".into()],
                    ..Default::default()
                },
            ),
        ] {
            let mut fixture = Fixture::ModelsInstalled.data();
            if model.installed {
                fixture.models = vec![model];
                fixture.model_catalog.clear();
            } else {
                fixture.models.clear();
                fixture.model_catalog = vec![model];
            }
            let rendered =
                render_with_input(&ctx, &mut fixture, &mut page, width, height, Vec::new()).0;
            assert_eq!(
                node_matching(&rendered, |node| node.name() == Some(display_name)).role(),
                egui::accesskit::Role::StaticText,
            );
            assert!(
                !node_names(&rendered).iter().any(|name| {
                    name == &format!("Use {display_name} for future transcriptions")
                })
            );
        }
    }

    #[test]
    fn full_card_background_pointer_keyboard_and_accesskit_activate_once() {
        let (width, height) = (1180.0, 815.0);
        let activate_at = |label: &str, node_name: Option<&str>| {
            let ctx = egui::Context::default();
            ctx.enable_accesskit();
            configure_accessible_style(&ctx);
            let mut data = Fixture::ModelsInstalled.data();
            let mut page = AppPage::Models;
            let initial =
                render_with_input(&ctx, &mut data, &mut page, width, height, Vec::new()).0;
            let point = if let Some(node_name) = node_name {
                let bounds = named_node_bounds(&initial, node_name);
                egui::pos2(
                    ((bounds.x0 + bounds.x1) / 2.0) as f32,
                    ((bounds.y0 + bounds.y1) / 2.0) as f32,
                )
            } else {
                let card = named_node_bounds(&initial, "whisper.cpp tiny.en model");
                egui::pos2((card.x0 + 8.0) as f32, (card.y1 - 8.0) as f32)
            };
            let (_, press) = render_with_input(
                &ctx,
                &mut data,
                &mut page,
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
            assert_eq!(press, ScreenAction::None, "{label} must wait for release");
            render_with_input(
                &ctx,
                &mut data,
                &mut page,
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
            )
            .1
        };
        for (label, node_name) in [
            ("identity", Some("whisper.cpp tiny.en")),
            ("metrics", Some("Speed: Very fast (5 of 5)")),
            ("expanded whitespace", None),
        ] {
            assert_eq!(
                activate_at(label, node_name),
                ScreenAction::SelectModel("tiny.en".into()),
                "{label}"
            );
        }

        for key in [egui::Key::Enter, egui::Key::Space] {
            let ctx = egui::Context::default();
            ctx.enable_accesskit();
            configure_accessible_style(&ctx);
            let mut data = Fixture::ModelsInstalled.data();
            let mut page = AppPage::Models;
            let initial =
                render_with_input(&ctx, &mut data, &mut page, width, height, Vec::new()).0;
            let target = named_node_id(
                &initial,
                "Use whisper.cpp tiny.en for future transcriptions",
            );
            assert_eq!(
                render_with_input(
                    &ctx,
                    &mut data,
                    &mut page,
                    width,
                    height,
                    vec![egui::Event::AccessKitActionRequest(
                        egui::accesskit::ActionRequest {
                            action: egui::accesskit::Action::Focus,
                            target,
                            data: None,
                        }
                    ),]
                )
                .1,
                ScreenAction::None,
            );
            assert_eq!(
                render_with_input(
                    &ctx,
                    &mut data,
                    &mut page,
                    width,
                    height,
                    vec![page_event(key)]
                )
                .1,
                ScreenAction::SelectModel("tiny.en".into()),
                "{key:?} on the focused card target",
            );
        }
    }

    #[test]
    fn ready_card_has_one_selectable_sibling_of_its_child_buttons() {
        let output = render(Fixture::ModelsInstalled, 1180.0, 815.0);
        let update = output.platform_output.accesskit_update.as_ref().unwrap();
        let select_id = named_node_id(&output, "Use whisper.cpp tiny.en for future transcriptions");
        assert_eq!(
            update
                .nodes
                .iter()
                .filter(|(_, node)| {
                    node.role() == egui::accesskit::Role::Button
                        && node.name() == Some("Use whisper.cpp tiny.en for future transcriptions")
                })
                .count(),
            1,
        );
        for child_name in [
            "Delete whisper.cpp tiny.en",
            "Expand details for whisper.cpp tiny.en",
        ] {
            let child_id = named_node_id(&output, child_name);
            assert_ne!(
                child_id, select_id,
                "{child_name} must not reuse the card target"
            );
            assert!(
                !update.nodes.iter().any(|(parent_id, node)| {
                    *parent_id == select_id && node.children().contains(&child_id)
                }),
                "{child_name} must be a sibling, not a nested child of the selectable card",
            );
        }
    }

    #[test]
    fn removal_focus_restores_to_surviving_uninstall_then_clears() {
        let (width, height) = (1180.0, 815.0);
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        configure_accessible_style(&ctx);
        let mut data = Fixture::ModelsInstalled.data();
        let mut page = AppPage::Models;
        data.model_management.dialog = Some(ModelDialog::Remove("tiny.en".into()));
        apply_action(&mut data, &mut page, ScreenAction::CloseModelDialog);
        assert_eq!(
            data.model_management.restore_remove_focus.as_deref(),
            Some("tiny.en")
        );

        let (output, action) =
            render_with_input(&ctx, &mut data, &mut page, width, height, Vec::new());
        assert_eq!(action, ScreenAction::AcknowledgeModelRemovalFocus);
        assert_eq!(
            focused_node(&output).name(),
            Some("Delete whisper.cpp tiny.en")
        );
        apply_action(&mut data, &mut page, action);
        assert!(data.model_management.restore_remove_focus.is_none());

        data.model_management.restore_remove_focus = Some("missing".into());
        let (output, action) =
            render_with_input(&ctx, &mut data, &mut page, width, height, Vec::new());
        assert_eq!(action, ScreenAction::AcknowledgeModelRemovalFocus);
        assert!(
            focused_node(&output)
                .name()
                .is_some_and(|name| name.contains("Import"))
        );
        apply_action(&mut data, &mut page, action);
        assert!(data.model_management.restore_remove_focus.is_none());
    }

    #[test]
    fn inline_runtime_and_active_uninstall_reasons_are_exposed() {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        configure_accessible_style(&ctx);
        let mut data = Fixture::ModelsInstalled.data();
        let mut active = data.models.remove(0);
        active.runtime_action_label = Some("Repair".into());
        active.runtime_action_enabled = false;
        active.runtime_action_disabled_reason =
            Some("Runtime maintenance is already running.".into());
        data.models = vec![active.clone()];
        data.model_catalog.clear();
        data.model_management.expanded_model_card = Some(ModelCardKey::Local(active.id.clone()));
        let mut page = AppPage::Models;
        let output = render_with_input(&ctx, &mut data, &mut page, 1180.0, 815.0, Vec::new()).0;
        let runtime = node_matching(&output, |node| {
            node.name() == Some(format!("Repair runtime for {}", active.display_name).as_str())
        });
        assert!(runtime.is_disabled());
        assert_eq!(
            runtime.description(),
            Some("Runtime maintenance is already running.")
        );
        assert!(
            output
                .platform_output
                .accesskit_update
                .as_ref()
                .unwrap()
                .nodes
                .iter()
                .any(|(_, node)| {
                    node.name() == Some(format!("Delete {}", active.display_name).as_str())
                        && node.description()
                            == Some(
                                "Install another ready model before removing the selected model.",
                            )
                })
        );
    }

    #[test]
    fn expanded_maintenance_control_participates_in_model_card_focus_within() {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        configure_accessible_style(&ctx);
        let mut data = Fixture::ModelsInstalled.data();
        let mut model = data.models.remove(0);
        model.runtime_action_label = Some("Repair".into());
        model.runtime_action_enabled = true;
        data.models = vec![model.clone()];
        data.model_catalog.clear();
        data.model_management.expanded_model_card = Some(ModelCardKey::Local(model.id.clone()));
        let mut page = AppPage::Models;
        let initial = render_with_input(&ctx, &mut data, &mut page, 1180.0, 815.0, Vec::new()).0;
        let name = format!("Repair runtime for {}", model.display_name);
        let target = named_node_id(&initial, &name);
        let (focused, action) = render_with_input(
            &ctx,
            &mut data,
            &mut page,
            1180.0,
            815.0,
            vec![egui::Event::AccessKitActionRequest(
                egui::accesskit::ActionRequest {
                    action: egui::accesskit::Action::Focus,
                    target,
                    data: None,
                },
            )],
        );
        assert_eq!(action, ScreenAction::None);
        assert_eq!(focused_node(&focused).name(), Some(name.as_str()));
        let card = named_node_bounds(&focused, &format!("{} model", model.display_name));
        assert_bounds_within(
            named_node_bounds(&focused, &name),
            card,
            "expanded maintenance focus target",
        );
    }

    #[test]
    fn model_card_render_all_and_compact_controls_remain_contained() {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        configure_accessible_style(&ctx);
        let mut data = Fixture::ModelsInstalled.data();
        data.model_catalog
            .extend((0..100).map(|index| ModelViewModel {
                id: format!("catalog-{index:03}"),
                display_name: format!("Catalog model {index:03}"),
                variant_label: format!("catalog-{index:03}"),
                install_supported: true,
                install_action_enabled: true,
                languages: vec!["en".into()],
                ..Default::default()
            }));
        let mut page = Fixture::ModelsInstalled.page();
        let (wide, action) =
            render_with_input(&ctx, &mut data, &mut page, 1180.0, 815.0, Vec::new());
        assert_eq!(action, ScreenAction::None);
        assert!(
            node_names(&wide)
                .iter()
                .any(|name| name == "Install Catalog model 099")
        );

        let compact = render(Fixture::ModelsCardExpanded, 375.0, 680.0);
        let card = named_node_bounds(&compact, "whisper.cpp tiny.en model");
        for name in [
            "Collapse details for whisper.cpp tiny.en",
            "Delete whisper.cpp tiny.en",
        ] {
            let bounds = named_node_bounds(&compact, name);
            assert_bounds_within(bounds, card, "compact model control");
            assert!(bounds.x1 - bounds.x0 >= 44.0 && bounds.y1 - bounds.y0 >= 44.0);
        }

        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        configure_accessible_style(&ctx);
        let mut data = Fixture::ModelsInstalled.data();
        let long_name =
            "A deliberately long local speech model name that wraps without covering controls";
        data.models = vec![ModelViewModel {
            id: "long-active".into(),
            display_name: long_name.into(),
            installed: true,
            active: true,
            selected: true,
            ready: true,
            removal_supported: true,
            download_state: ModelDownloadState::Installed,
            languages: vec!["en".into()],
            ..Default::default()
        }];
        data.model_catalog.clear();
        let mut page = AppPage::Models;
        let (output, action) =
            render_with_input(&ctx, &mut data, &mut page, 375.0, 680.0, Vec::new());
        assert_eq!(action, ScreenAction::None);
        let card = named_node_bounds(&output, &format!("{long_name} model"));
        assert_bounds_within(
            named_node_bounds(&output, long_name),
            card,
            "long-name compact model title",
        );
        for name in [
            format!("Delete {long_name}"),
            format!("Expand details for {long_name}"),
        ] {
            let bounds = named_node_bounds(&output, &name);
            assert_bounds_within(bounds, card, "long-name compact model control");
            assert!(bounds.width() >= 44.0 && bounds.height() >= 44.0);
        }
    }

    #[test]
    fn lifecycle_accessibility_and_actions_hold_at_viewport_bounds() {
        for (width, height) in [(1180.0, 815.0), (960.0, 680.0), (375.0, 815.0)] {
            let ctx = egui::Context::default();
            ctx.enable_accesskit();
            configure_accessible_style(&ctx);
            let mut data = Fixture::ModelsInstalled.data();
            data.models.clear();
            data.model_catalog = vec![ModelViewModel {
                id: "viewport-install".into(),
                display_name: "Viewport install".into(),
                install_supported: true,
                install_action_enabled: true,
                languages: vec!["en".into()],
                ..Default::default()
            }];
            let mut page = AppPage::Models;
            let output = render_with_input(&ctx, &mut data, &mut page, width, height, Vec::new()).0;
            let install = node_matching(&output, |node| {
                node.name() == Some("Install Viewport install")
                    && node.role() == egui::accesskit::Role::Button
            });
            let install_bounds = install.bounds().expect("Install bounds");
            assert!(
                install_bounds.width() >= 44.0 && install_bounds.height() >= 44.0,
                "Install must retain a named 44px target at {width}px"
            );
            if width >= 620.0 {
                let lifecycle =
                    named_node_bounds(&output, "Viewport install layout lifecycle zone");
                assert_near(
                    (install_bounds.y0 + install_bounds.y1) / 2.0,
                    (lifecycle.y0 + lifecycle.y1) / 2.0,
                    &format!("Install vertical center at {width}px"),
                );
            }
            assert_eq!(
                click_named_control(
                    &ctx,
                    &mut data,
                    &mut page,
                    width,
                    height,
                    "Install Viewport install",
                ),
                ScreenAction::InstallModel("viewport-install".into()),
                "Install action at {width}px"
            );

            data.model_catalog.clear();
            data.models = vec![ModelViewModel {
                id: "viewport-delete".into(),
                display_name: "Viewport delete".into(),
                installed: true,
                ready: true,
                removal_supported: true,
                download_state: ModelDownloadState::Installed,
                languages: vec!["en".into()],
                ..Default::default()
            }];
            let output = render_with_input(&ctx, &mut data, &mut page, width, height, Vec::new()).0;
            let delete = node_matching(&output, |node| {
                node.name() == Some("Delete Viewport delete")
                    && node.role() == egui::accesskit::Role::Button
            });
            let delete_bounds = delete.bounds().expect("Delete bounds");
            assert!(
                delete_bounds.width() >= 44.0 && delete_bounds.height() >= 44.0,
                "Delete must retain a named 44px target at {width}px"
            );
            if width >= 620.0 {
                let lifecycle = named_node_bounds(&output, "Viewport delete layout lifecycle zone");
                assert_near(
                    (delete_bounds.y0 + delete_bounds.y1) / 2.0,
                    (lifecycle.y0 + lifecycle.y1) / 2.0,
                    &format!("Delete vertical center at {width}px"),
                );
            }
            assert_eq!(
                click_named_control(
                    &ctx,
                    &mut data,
                    &mut page,
                    width,
                    height,
                    "Delete Viewport delete",
                ),
                ScreenAction::RequestModelRemoval("viewport-delete".into()),
                "Delete action at {width}px"
            );
        }
    }

    #[test]
    fn model_cards_remain_inside_supported_route_widths() {
        for (width, height) in [(1180.0, 815.0), (960.0, 680.0)] {
            let ctx = egui::Context::default();
            ctx.enable_accesskit();
            configure_accessible_style(&ctx);
            let mut data = Fixture::ModelsCardExpanded.data();
            let mut page = Fixture::ModelsCardExpanded.page();
            let output = render_with_input(&ctx, &mut data, &mut page, width, height, Vec::new()).0;
            let route_viewport = ctx
                .data(|data| {
                    data.get_temp::<egui::Rect>(egui::Id::new(("route-viewport", UiRoute::Models)))
                })
                .expect("Models route viewport diagnostic");
            let route_left = node_matching(&output, |node| {
                node.role() == egui::accesskit::Role::Heading && node.name() == Some("Models")
            })
            .bounds()
            .expect("Models heading should expose bounds")
            .x0;
            let route_right = f64::from(route_viewport.right() - 28.0);
            for name in ["whisper.cpp base.en model", "whisper.cpp tiny.en model"] {
                let bounds = named_node_bounds(&output, name);
                assert_near(
                    bounds.x0,
                    route_left,
                    "supported-width model card must align to the route's left usable edge",
                );
                assert_near(
                    bounds.x1,
                    route_right,
                    "supported-width model card must align to the route's right usable edge",
                );
            }
        }
    }

    #[test]
    fn model_card_summary_stacks_below_and_uses_three_zones_at_the_620px_breakpoint() {
        let compact = render(Fixture::ModelsInstalled, 785.0, 680.0);
        let compact_card = named_node_bounds(&compact, "whisper.cpp tiny.en model");
        let compact_title = named_node_bounds(&compact, "whisper.cpp tiny.en");
        let compact_lifecycle = named_node_bounds(&compact, "Delete whisper.cpp tiny.en");
        assert_near(
            compact_card.x1 - compact_card.x0 - 44.0,
            619.0,
            "the compact breakpoint's 619px content width",
        );
        assert!(
            compact_title.y1 <= compact_lifecycle.y0 + LAYOUT_TOLERANCE,
            "619px card content must use the stacked summary branch: title={compact_title:?}, lifecycle={compact_lifecycle:?}"
        );

        let desktop = render(Fixture::ModelsInstalled, 786.0, 680.0);
        let desktop_card = named_node_bounds(&desktop, "whisper.cpp tiny.en model");
        let desktop_title = named_node_bounds(&desktop, "whisper.cpp tiny.en");
        let desktop_lifecycle = named_node_bounds(&desktop, "Delete whisper.cpp tiny.en");
        let identity = named_node_bounds(&desktop, "whisper.cpp tiny.en layout identity zone");
        let metrics = named_node_bounds(&desktop, "whisper.cpp tiny.en layout metrics zone");
        let lifecycle_zone =
            named_node_bounds(&desktop, "whisper.cpp tiny.en layout lifecycle zone");
        let desktop_chevron = named_node_bounds(&desktop, "Expand details for whisper.cpp tiny.en");
        assert_near(
            desktop_card.x1 - desktop_card.x0 - 44.0,
            620.0,
            "the desktop breakpoint's 620px content width",
        );
        let summary_width = identity.width() + metrics.width() + lifecycle_zone.width();
        assert_near(
            identity.width(),
            summary_width * 0.50,
            "breakpoint identity zone",
        );
        assert_near(
            metrics.width(),
            summary_width * 0.24,
            "breakpoint metrics zone",
        );
        assert_near(
            lifecycle_zone.width(),
            summary_width * 0.26,
            "breakpoint lifecycle zone",
        );
        assert!(
            desktop_title.x1 <= metrics.x0 + LAYOUT_TOLERANCE
                && desktop_lifecycle.x1 <= desktop_chevron.x0 + LAYOUT_TOLERANCE
                && desktop_chevron.x1 <= lifecycle_zone.x1 + LAYOUT_TOLERANCE,
            "620px card content must keep identity, metrics, lifecycle, and chevron in three-zone order: title={desktop_title:?}, metrics={metrics:?}, lifecycle={desktop_lifecycle:?}, chevron={desktop_chevron:?}"
        );
    }

    #[test]
    fn expanding_model_details_preserves_the_card_width() {
        for (width, height) in [(1180.0, 815.0), (960.0, 680.0), (375.0, 680.0)] {
            let ctx = egui::Context::default();
            ctx.enable_accesskit();
            configure_accessible_style(&ctx);
            let mut data = Fixture::ModelsCardExpanded.data();
            data.model_management.expanded_model_card = None;
            let mut page = Fixture::ModelsCardExpanded.page();
            let collapsed =
                render_with_input(&ctx, &mut data, &mut page, width, height, Vec::new()).0;
            data.model_management.expanded_model_card = Some(ModelCardKey::Local("tiny.en".into()));
            let expanded =
                render_with_input(&ctx, &mut data, &mut page, width, height, Vec::new()).0;
            let collapsed_card = named_node_bounds(&collapsed, "whisper.cpp tiny.en model");
            let expanded_card = named_node_bounds(&expanded, "whisper.cpp tiny.en model");

            assert_near(
                expanded_card.x0,
                collapsed_card.x0,
                "expanding details must preserve the card's left edge",
            );
            assert_near(
                expanded_card.x1,
                collapsed_card.x1,
                "expanding details must preserve the card's right edge",
            );
        }
    }

    #[test]
    fn model_card_expansion_keeps_metadata_in_place_and_compact_stacking() {
        let long_description = "A deliberately long model description that must wrap when details are expanded while preserving the identity stack origin.";
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        configure_accessible_style(&ctx);
        let mut data = Fixture::ModelsInstalled.data();
        let model = data
            .models
            .iter_mut()
            .find(|model| model.id == "tiny.en")
            .expect("tiny model fixture");
        model.description = Some(long_description.into());
        let mut page = AppPage::Models;
        let (collapsed, action) =
            render_with_input(&ctx, &mut data, &mut page, 960.0, 680.0, Vec::new());
        assert_eq!(action, ScreenAction::None);
        let collapsed_card = named_node_bounds(&collapsed, "whisper.cpp tiny.en model");
        let collapsed_description = named_node_bounds(&collapsed, long_description);
        let collapsed_language =
            named_node_bounds(&collapsed, "whisper.cpp tiny.en layout language row");
        let collapsed_metrics =
            named_node_bounds(&collapsed, "whisper.cpp tiny.en layout metrics zone");
        let collapsed_lifecycle =
            named_node_bounds(&collapsed, "whisper.cpp tiny.en layout lifecycle zone");

        data.model_management.expanded_model_card = Some(ModelCardKey::Local("tiny.en".into()));
        let (expanded, action) =
            render_with_input(&ctx, &mut data, &mut page, 960.0, 680.0, Vec::new());
        assert_eq!(action, ScreenAction::None);
        let expanded_card = named_node_bounds(&expanded, "whisper.cpp tiny.en model");
        let expanded_description = named_node_bounds(&expanded, long_description);
        let expanded_language =
            named_node_bounds(&expanded, "whisper.cpp tiny.en layout language row");
        let expanded_metrics =
            named_node_bounds(&expanded, "whisper.cpp tiny.en layout metrics zone");
        let expanded_lifecycle =
            named_node_bounds(&expanded, "whisper.cpp tiny.en layout lifecycle zone");
        assert_near(
            expanded_description.x0,
            collapsed_description.x0,
            "expanded description must keep the collapsed identity x origin",
        );
        assert_near(
            expanded_description.y0,
            collapsed_description.y0,
            "wrapped description must keep the collapsed identity y origin",
        );
        assert_near(
            expanded_language.x0,
            collapsed_language.x0,
            "expanded languages must stay in the identity metadata row",
        );
        for (collapsed_zone, expanded_zone, name) in [
            (collapsed_metrics, expanded_metrics, "metrics"),
            (collapsed_lifecycle, expanded_lifecycle, "lifecycle"),
        ] {
            assert_near(
                expanded_zone.y0,
                collapsed_zone.y0,
                &format!("expanded {name} zone must retain its summary y origin"),
            );
        }
        assert!(
            expanded_card.y1 > collapsed_card.y1,
            "wrapping expanded metadata and details must grow the card naturally"
        );

        let compact = render(Fixture::ModelsInstalled, 375.0, 680.0);
        let title = named_node_bounds(&compact, "whisper.cpp tiny.en");
        let lifecycle = named_node_bounds(&compact, "Delete whisper.cpp tiny.en");
        assert!(
            title.y1 <= lifecycle.y0 + LAYOUT_TOLERANCE,
            "below 620px the summary must stack identity before controls"
        );
    }

    #[test]
    fn one_line_expansion_preserves_desktop_summary_geometry_and_compact_containment() {
        for (width, height) in [(1180.0, 815.0), (960.0, 680.0)] {
            let ctx = egui::Context::default();
            ctx.enable_accesskit();
            configure_accessible_style(&ctx);
            let mut data = Fixture::ModelsInstalled.data();
            let mut page = AppPage::Models;
            let collapsed =
                render_with_input(&ctx, &mut data, &mut page, width, height, Vec::new()).0;
            data.model_management.expanded_model_card = Some(ModelCardKey::Local("tiny.en".into()));
            let expanded =
                render_with_input(&ctx, &mut data, &mut page, width, height, Vec::new()).0;
            for name in [
                "whisper.cpp tiny.en",
                "More accurate for longer recordings.",
                "whisper.cpp tiny.en layout language row",
                "whisper.cpp tiny.en layout metrics zone",
                "whisper.cpp tiny.en layout lifecycle zone",
                "whisper.cpp tiny.en layout chevron zone",
            ] {
                let collapsed_bounds = named_node_bounds(&collapsed, name);
                let expanded_bounds = named_node_bounds(&expanded, name);
                assert_near(
                    expanded_bounds.x0,
                    collapsed_bounds.x0,
                    &format!("{name} x origin"),
                );
                assert_near(
                    expanded_bounds.y0,
                    collapsed_bounds.y0,
                    &format!("{name} y origin"),
                );
                assert_near(
                    expanded_bounds.y1,
                    collapsed_bounds.y1,
                    &format!("{name} height"),
                );
            }
        }

        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        configure_accessible_style(&ctx);
        let mut data = Fixture::ModelsInstalled.data();
        data.model_management.expanded_model_card = Some(ModelCardKey::Local("tiny.en".into()));
        let mut page = AppPage::Models;
        let compact = render_with_input(&ctx, &mut data, &mut page, 375.0, 680.0, Vec::new()).0;
        let card = named_node_bounds(&compact, "whisper.cpp tiny.en model");
        for name in [
            "whisper.cpp tiny.en",
            "More accurate for longer recordings.",
            "whisper.cpp tiny.en layout language row",
            "Delete whisper.cpp tiny.en",
            "Collapse details for whisper.cpp tiny.en",
        ] {
            assert_bounds_within(
                named_node_bounds(&compact, name),
                card,
                &format!("375px {name}"),
            );
        }

        let mut compact_summary_data = Fixture::ModelsCardExpanded.data();
        compact_summary_data
            .models
            .retain(|model| model.id == "tiny.en");
        compact_summary_data.model_catalog.clear();
        compact_summary_data.model_management.expanded_model_card = None;
        let mut compact_summary_page = AppPage::Models;
        let compact_summary = render_with_input(
            &ctx,
            &mut compact_summary_data,
            &mut compact_summary_page,
            375.0,
            680.0,
            Vec::new(),
        )
        .0;
        let compact_summary_card = named_node_bounds(&compact_summary, "whisper.cpp tiny.en model");
        let compact_features = named_node_bounds(
            &compact_summary,
            "Features: Native streaming, Translation, Word timestamps, Batch transcription",
        );
        assert_bounds_within(
            compact_features,
            compact_summary_card,
            "375px compact feature group",
        );
    }

    #[test]
    fn model_card_controls_keep_trailing_chevron_order_without_decorative_nodes() {
        for (width, height) in [(1180.0, 815.0), (375.0, 680.0)] {
            let output = render(Fixture::ModelsInstalled, width, height);
            let title = named_node_bounds(&output, "whisper.cpp tiny.en");
            let lifecycle = named_node_bounds(&output, "Delete whisper.cpp tiny.en");
            let chevron = named_node_bounds(&output, "Expand details for whisper.cpp tiny.en");
            if width > 430.0 {
                let speed = named_node_bounds(&output, "Speed: Very fast (5 of 5)");
                let metrics = named_node_bounds(&output, "whisper.cpp tiny.en layout metrics zone");
                let lifecycle_zone =
                    named_node_bounds(&output, "whisper.cpp tiny.en layout lifecycle zone");
                let description =
                    named_node_bounds(&output, "More accurate for longer recordings.");
                let language = named_node_bounds(&output, "Languages: EN");
                assert!(title.x1 <= metrics.x0 + LAYOUT_TOLERANCE);
                assert!(description.x1 <= metrics.x0 + LAYOUT_TOLERANCE);
                assert!(language.x1 <= metrics.x0 + LAYOUT_TOLERANCE);
                assert_near(
                    description.x0,
                    title.x0,
                    "description should align with identity title text",
                );
                assert_near(
                    language.x0 - 20.0,
                    title.x0,
                    "globe should align with identity title text",
                );
                assert_bounds_within(speed, metrics, "speed meter");
                assert_bounds_within(lifecycle, lifecycle_zone, "lifecycle control");
                assert_bounds_within(chevron, lifecycle_zone, "chevron control");
                assert!(lifecycle.x1 <= chevron.x0 + LAYOUT_TOLERANCE);
            } else {
                assert!(title.y1 <= lifecycle.y0 + LAYOUT_TOLERANCE);
                assert!(lifecycle.x1 <= chevron.x0 + LAYOUT_TOLERANCE);
            }
        }
    }

    #[test]
    fn expanded_remote_card_exposes_fallback_details_inline() {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        configure_accessible_style(&ctx);
        let mut data = Fixture::ModelsInstalled.data();
        let entry = &data.remote_catalog.entries[0];
        let variant = &entry.variants[0];
        data.model_management.expanded_model_card = Some(ModelCardKey::Remote {
            entry_id: entry.id.clone(),
            variant_id: variant.id.clone(),
        });
        let mut page = Fixture::ModelsInstalled.page();
        let (output, action) =
            render_with_input(&ctx, &mut data, &mut page, 1180.0, 815.0, Vec::new());
        assert_eq!(action, ScreenAction::None);
        let names = node_names(&output);
        for detail in [
            "REQUIREMENTS",
            "RAM",
            "DOWNLOAD SIZE",
            "82 MB",
            "GPU",
            "FEATURES",
        ] {
            assert!(names.iter().any(|name| name == detail), "missing {detail}");
        }
        assert!(
            names.iter().any(|name| name == "Languages: EN"),
            "expanded remote identity should expose its compact language summary"
        );
        assert_eq!(
            names
                .iter()
                .filter(|name| name.as_str() == "Unknown")
                .count(),
            2,
            "the remote RAM and GPU requirement cells each expose Unknown"
        );
    }

    #[test]
    fn summary_partial_cleanup_keeps_play_and_dispatches_discard() {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        configure_accessible_style(&ctx);
        let mut data = Fixture::ModelsLifecycle.data();
        let model = data
            .model_catalog
            .iter_mut()
            .find(|model| model.id == "moonshine.base")
            .expect("lifecycle fixture includes Moonshine");
        model.partial_cleanup_available = true;
        model.partial_cleanup_enabled = true;
        data.model_management.expanded_model_card =
            Some(ModelCardKey::Local("moonshine.base".into()));
        let mut page = Fixture::ModelsLifecycle.page();

        let initial = render_with_input(&ctx, &mut data, &mut page, 1180.0, 815.0, Vec::new()).0;
        assert!(
            node_names(&initial)
                .iter()
                .any(|name| name == "Resume Whisper Moonshine download")
        );
        let discard = named_node_id(&initial, "Discard partial for Whisper Moonshine");
        let (_, action) = render_with_input(
            &ctx,
            &mut data,
            &mut page,
            1180.0,
            815.0,
            vec![egui::Event::AccessKitActionRequest(
                egui::accesskit::ActionRequest {
                    action: egui::accesskit::Action::Default,
                    target: discard,
                    data: None,
                },
            )],
        );
        assert_eq!(
            action,
            ScreenAction::DiscardModelPartial("moonshine.base".into())
        );
        apply_action(&mut data, &mut page, action);
        let refreshed = render_with_input(&ctx, &mut data, &mut page, 1180.0, 815.0, Vec::new()).0;
        assert!(
            !node_names(&refreshed)
                .iter()
                .any(|name| name == "Discard partial for Whisper Moonshine")
        );
        assert!(
            node_names(&refreshed)
                .iter()
                .any(|name| name == "Collapse details for Whisper Moonshine")
        );
    }

    #[test]
    fn summary_partial_cleanup_uses_safe_app_barriers_and_preserves_remote_ids() {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        configure_accessible_style(&ctx);
        let mut local = Fixture::ModelsLifecycle.data();
        let model = local
            .model_catalog
            .iter_mut()
            .find(|model| model.id == "moonshine.base")
            .expect("lifecycle fixture includes Moonshine");
        model.partial_cleanup_available = true;
        model.partial_cleanup_enabled = false;
        model.partial_cleanup_disabled_reason =
            Some("Wait for the active installation to finish.".into());
        local.model_management.expanded_model_card =
            Some(ModelCardKey::Local("moonshine.base".into()));
        let mut page = Fixture::ModelsLifecycle.page();
        let output = render_with_input(&ctx, &mut local, &mut page, 1180.0, 815.0, Vec::new()).0;
        let discard = node_matching(&output, |node| {
            node.name() == Some("Discard partial for Whisper Moonshine")
        });
        assert!(
            !discard.is_disabled(),
            "the always-available X delegates safety checks to the app mutation barrier"
        );

        let mut remote = Fixture::ModelsInstalled.data();
        remote.remote_catalog.entries[0].variants[0]
            .actions
            .push(RemoteCatalogActionView {
                label: "Discard partial".into(),
                kind: RemoteCatalogActionKind::DiscardPartial {
                    remote_model_id: "trusted-speech/compact-english".into(),
                    variant_id: "compact-english-q5".into(),
                },
                enabled: true,
                disabled_reason: None,
            });
        remote.remote_catalog.entries[0].variants[0].status_label = Some("Cancelled".into());
        remote.remote_catalog.entries[0].variants[0].downloaded_bytes = Some(41_000_000);
        remote.remote_catalog.entries[0].variants[0].total_bytes = Some(82_000_000);
        remote.model_management.expanded_model_card = Some(ModelCardKey::Remote {
            entry_id: "trusted-speech/compact-english".into(),
            variant_id: "compact-english-q5".into(),
        });
        let mut page = Fixture::ModelsInstalled.page();
        let initial = render_with_input(&ctx, &mut remote, &mut page, 1180.0, 815.0, Vec::new()).0;
        let discard = named_node_id(
            &initial,
            "Discard partial for Compact English (compact-english-q5.gguf)",
        );
        let (_, action) = render_with_input(
            &ctx,
            &mut remote,
            &mut page,
            1180.0,
            815.0,
            vec![egui::Event::AccessKitActionRequest(
                egui::accesskit::ActionRequest {
                    action: egui::accesskit::Action::Default,
                    target: discard,
                    data: None,
                },
            )],
        );
        assert_eq!(
            action,
            ScreenAction::DiscardRemoteCatalogPartial {
                remote_model_id: "trusted-speech/compact-english".into(),
                variant_id: "compact-english-q5".into(),
            }
        );
    }

    #[test]
    fn expanded_legacy_cleanup_uses_upgrade_and_explicit_uninstall() {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        configure_accessible_style(&ctx);
        let mut data = Fixture::ModelsInstalled.data();
        data.models.clear();
        data.model_catalog = vec![ModelViewModel {
            id: "legacy".into(),
            display_name: "Legacy model".into(),
            legacy_cleanup_pending: true,
            selected: true,
            primary_action_installs_upgrade: true,
            primary_action_enabled: true,
            removal_supported: true,
            languages: vec!["en".into()],
            ..Default::default()
        }];
        data.model_management.expanded_model_card = Some(ModelCardKey::Local("legacy".into()));
        let mut page = AppPage::Models;
        let output = render_with_input(&ctx, &mut data, &mut page, 1180.0, 815.0, Vec::new()).0;
        assert!(
            node_names(&output)
                .iter()
                .any(|name| name == "Upgrade Legacy model")
        );
        assert!(
            node_names(&output)
                .iter()
                .any(|name| name == "Delete Legacy model")
        );
        assert!(
            !node_names(&output)
                .iter()
                .any(|name| name == "Install Legacy model")
        );
    }

    #[test]
    fn harness_parser_is_exact_and_fail_closed() {
        assert_eq!(
            Fixture::parse("transcribe/ready"),
            Some(Fixture::TranscribeReady)
        );
        assert_eq!(Fixture::parse("demo/audio"), Some(Fixture::DemoAudio));
        assert_eq!(Fixture::parse("debug"), None);
    }
}
