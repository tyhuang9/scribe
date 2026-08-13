//! Development-only deterministic fixtures. Actions update only local fixture state.

use eframe::egui::{self, CentralPanel, Frame};

use super::{
    configure_accessible_style,
    screens::{RecordingSettingsView, ScreenAction, ScreenView, render_screen, show_route_scroll},
    shell::{AppPage, show_navigation},
    state::{
        ComparisonPhase, ModelComparisonState, ModelDialog, ModelDownloadState,
        ModelLanguageFilter, ModelManagementState, ModelSizeTier, ModelSpeedTier, ModelViewModel,
        RemoteCatalogActionKind, RemoteCatalogActionView, RemoteCatalogEntryView,
        RemoteCatalogStatusKind, RemoteCatalogStatusView, RemoteCatalogVariantView,
        RemoteCatalogView, SettingsTab, TranscriptionPhase, TranscriptionState, UiRoute,
    },
    theme_palette,
};

#[cfg(test)]
use super::state::{ComparisonResult, ComparisonResultPhase, ModelCardKey};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Fixture {
    TranscribeNoModel,
    TranscribeReady,
    TranscribeListening,
    TranscribeFinalizing,
    TranscribeNoSpeech,
    TranscribeMicrophoneError,
    ModelsInstalled,
    ModelsLifecycle,
    ModelsDetailsDrawer,
    ModelsCompareExpanded,
    History,
    SettingsRecording,
}

impl Fixture {
    #[cfg(test)]
    pub(crate) const ALL: [Self; 12] = [
        Self::TranscribeNoModel,
        Self::TranscribeReady,
        Self::TranscribeListening,
        Self::TranscribeFinalizing,
        Self::TranscribeNoSpeech,
        Self::TranscribeMicrophoneError,
        Self::ModelsInstalled,
        Self::ModelsLifecycle,
        Self::ModelsDetailsDrawer,
        Self::ModelsCompareExpanded,
        Self::History,
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
            "models/lifecycle" => Self::ModelsLifecycle,
            "models/details-drawer" => Self::ModelsDetailsDrawer,
            "models/compare-expanded" => Self::ModelsCompareExpanded,
            "history" => Self::History,
            "settings/recording" => Self::SettingsRecording,
            _ => return None,
        })
    }
    fn page(self) -> AppPage {
        match self {
            Self::ModelsInstalled
            | Self::ModelsLifecycle
            | Self::ModelsDetailsDrawer
            | Self::ModelsCompareExpanded => AppPage::Models,
            Self::History => AppPage::History,
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
        let models = vec![
            model("whisper.cpp base.en", "base.en", true, true, 400),
            model("whisper.cpp tiny.en", "tiny.en", false, false, 75),
        ];
        let mut model_catalog = Vec::new();
        let mut comparison = ModelComparisonState::default();
        let settings = RecordingSettingsView {
            duration_label: "30 seconds".into(),
            provisional_feedback: true,
            device_label: "Microphone (fifine Microphone)".into(),
            input_sensitivity_percent: 38,
            input_level_percent: 68,
            ..Default::default()
        };
        let route = match self {
            Self::ModelsInstalled
            | Self::ModelsLifecycle
            | Self::ModelsDetailsDrawer
            | Self::ModelsCompareExpanded => UiRoute::Models,
            Self::History => UiRoute::History,
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
            Self::ModelsInstalled | Self::SettingsRecording | Self::History => {
                transcription.phase = TranscriptionPhase::Ready
            }
            Self::ModelsLifecycle | Self::ModelsDetailsDrawer => {
                transcription.phase = TranscriptionPhase::Ready;
                let mut partial = model("Whisper Moonshine", "moonshine.base", false, false, 190);
                partial.installed = false;
                partial.download_state = ModelDownloadState::Cancelled;
                partial.downloaded_bytes = 129_000_000;
                partial.total_bytes = Some(190_000_000);
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

                model_catalog = vec![partial, downloading, failed, available];
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
            model_management: if self == Self::ModelsDetailsDrawer {
                ModelManagementState {
                    dialog: Some(ModelDialog::Details("tiny.en".into())),
                    focus_dialog_initial: true,
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
    models: Vec<ModelViewModel>,
    model_catalog: Vec<ModelViewModel>,
    comparison: ModelComparisonState,
    model_management: ModelManagementState,
    model_language_filter: ModelLanguageFilter,
    remote_catalog: RemoteCatalogView,
    settings: RecordingSettingsView,
    settings_playground_open: bool,
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

pub(crate) struct UiHarnessApp {
    page: AppPage,
    data: FixtureData,
}

fn configure_harness_style(ctx: &egui::Context) {
    ctx.set_visuals(egui::Visuals::light());
    configure_accessible_style(ctx);
}

impl UiHarnessApp {
    pub(crate) fn new(cc: &eframe::CreationContext<'_>, fixture: Fixture) -> Self {
        configure_harness_style(&cc.egui_ctx);
        Self {
            page: fixture.page(),
            data: fixture.data(),
        }
    }
}
impl eframe::App for UiHarnessApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let clear_initial_dialog_focus = self.data.model_management.focus_dialog_initial;
        let clear_returned_details_remove_focus = clear_initial_dialog_focus
            && matches!(
                &self.data.model_management.dialog,
                Some(ModelDialog::Details(id))
                    if self.data.model_management.restore_remove_focus.as_deref() == Some(id)
            );
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
        if clear_returned_details_remove_focus {
            self.data.model_management.restore_remove_focus = None;
        }
        if clear_add_focus {
            self.data.model_management.restore_add_focus = false;
        }
        if clear_after_removal_focus {
            self.data.model_management.restore_after_removal_focus = false;
        }
        apply_action(&mut self.data, &mut self.page, action);
        ctx.request_repaint_after(std::time::Duration::from_secs(60));
    }
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
    show_navigation(ctx, page, false);
    if *page != AppPage::General || !matches!(data.route, UiRoute::Settings(_)) {
        data.settings_playground_open = false;
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
    CentralPanel::default()
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
        .inner
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
        ScreenAction::AddModel => {
            data.model_management.dialog = Some(ModelDialog::Add);
            data.model_management.focus_dialog_initial = true;
        }
        ScreenAction::ShowModelDetails(id) => {
            data.model_management.dialog = Some(ModelDialog::Details(id));
            data.model_management.focus_dialog_initial = true;
        }
        ScreenAction::ShowRemoteModelDetails {
            entry_id,
            variant_id,
        } => {
            data.model_management.dialog = Some(ModelDialog::RemoteDetails {
                entry_id,
                variant_id,
            });
            data.model_management.focus_dialog_initial = true;
        }
        ScreenAction::RequestModelRemoval(id) => {
            data.model_management.restore_remove_focus = matches!(
                &data.model_management.dialog,
                Some(ModelDialog::Details(current)) if current == &id
            )
            .then(|| id.clone());
            data.model_management.dialog = Some(ModelDialog::Remove(id));
            data.model_management.focus_dialog_initial = true;
        }
        ScreenAction::CloseModelDialog => match data.model_management.dialog.take() {
            Some(ModelDialog::Add) => data.model_management.restore_add_focus = true,
            Some(ModelDialog::Details(_)) | Some(ModelDialog::RemoteDetails { .. }) => {}
            Some(ModelDialog::Remove(id))
                if data.model_management.restore_remove_focus.as_deref() == Some(&id) =>
            {
                data.model_management.dialog = Some(ModelDialog::Details(id));
                data.model_management.focus_dialog_initial = true;
            }
            Some(ModelDialog::Remove(id)) => {
                data.model_management.restore_remove_focus = Some(id);
            }
            None => {}
        },
        ScreenAction::ConfirmModelRemoval(id) => {
            data.model_management.dialog = None;
            data.model_management.restore_remove_focus = None;
            data.models.retain(|model| model.id != id);
            data.model_management.restore_after_removal_focus = true;
        }
        ScreenAction::ChangeModel => {
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
        ScreenAction::SetRemoteCatalogQuery(query) => data.remote_catalog.query = query,
        ScreenAction::SetModelLanguageFilter(filter) => data.model_language_filter = filter,
        ScreenAction::ToggleInstalledModels => {
            data.model_management.installed_expanded = !data.model_management.installed_expanded
        }
        ScreenAction::ToggleAvailableModels => {
            data.model_management.available_expanded = !data.model_management.available_expanded
        }
        ScreenAction::FocusModelCard(key) => data.model_management.focus_model_card = Some(key),
        ScreenAction::AcknowledgeModelCardFocus(key) => {
            if data.model_management.focus_model_card.as_ref() == Some(&key) {
                data.model_management.focus_model_card = None;
            }
        }
        ScreenAction::AcknowledgeModelControlFocus { model_id, control } => data
            .model_management
            .acknowledge_control_focus(&model_id, control),
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

        configure_harness_style(&ctx);

        let style = ctx.style();
        assert!(!style.visuals.dark_mode);
        assert_eq!(style.spacing.interact_size, egui::vec2(44.0, 44.0));
        assert_eq!(
            style.text_styles[&egui::TextStyle::Body],
            egui::FontId::new(14.0, egui::FontFamily::Proportional)
        );
    }

    fn render(fixture: Fixture, width: f32, height: f32) -> egui::FullOutput {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        configure_accessible_style(&ctx);
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
        let clear_returned_details_remove_focus = clear_initial_dialog_focus
            && matches!(
                &data.model_management.dialog,
                Some(ModelDialog::Details(id))
                    if data.model_management.restore_remove_focus.as_deref() == Some(id)
            );
        let clear_add_focus = data.model_management.restore_add_focus;
        let clear_reference_editor_focus = data.comparison.focus_reference_editor;
        let clear_reference_action_focus = data.comparison.restore_reference_action_focus;
        let clear_comparison_panel_focus = data.comparison.focus_panel;
        let clear_reference_notice = data.comparison.reference_notice.is_some();
        let clear_after_removal_focus = data.model_management.restore_after_removal_focus;
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
        if clear_returned_details_remove_focus {
            data.model_management.restore_remove_focus = None;
        }
        if clear_add_focus {
            data.model_management.restore_add_focus = false;
        }
        if clear_after_removal_focus {
            data.model_management.restore_after_removal_focus = false;
        }
        (output, action)
    }

    fn render_with_input_and_apply(
        ctx: &egui::Context,
        data: &mut FixtureData,
        page: &mut AppPage,
        width: f32,
        height: f32,
        events: Vec<egui::Event>,
    ) -> (egui::FullOutput, ScreenAction) {
        let (output, action) = render_with_input(ctx, data, page, width, height, events);
        apply_action(data, page, action.clone());
        (output, action)
    }

    fn render_with_input_and_apply_at_time(
        ctx: &egui::Context,
        data: &mut FixtureData,
        page: &mut AppPage,
        width: f32,
        height: f32,
        events: Vec<egui::Event>,
        time: f64,
    ) -> (egui::FullOutput, ScreenAction) {
        let (output, action) =
            render_with_input_at_time(ctx, data, page, width, height, events, Some(time));
        apply_action(data, page, action.clone());
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

    fn node_id_matching(
        output: &egui::FullOutput,
        predicate: impl Fn(&egui::accesskit::Node) -> bool,
    ) -> egui::accesskit::NodeId {
        output
            .platform_output
            .accesskit_update
            .as_ref()
            .expect("render should expose an AccessKit update")
            .nodes
            .iter()
            .find_map(|(id, node)| predicate(node).then_some(*id))
            .expect("expected AccessKit node")
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

    fn accesskit_descends_from(
        output: &egui::FullOutput,
        ancestor: egui::accesskit::NodeId,
        target: egui::accesskit::NodeId,
    ) -> bool {
        let nodes = &output
            .platform_output
            .accesskit_update
            .as_ref()
            .expect("render should expose an AccessKit update")
            .nodes;
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
        for (width, height) in [(1180.0, 815.0), (960.0, 680.0)] {
            let ctx = egui::Context::default();
            ctx.enable_accesskit();
            configure_accessible_style(&ctx);
            let mut data = Fixture::TranscribeReady.data();
            let mut page = Fixture::TranscribeReady.page();
            let committed = "A deliberately long committed transcript should wrap within the bounded transcript panel without pushing any controls beyond the application content region. ".repeat(8);
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
            let model = named_node_bounds(&output, "Selected model");
            let hotkey = named_node_bounds(&output, "Recording hotkey");
            assert!(
                panel.x0 >= viewport.x0 - LAYOUT_TOLERANCE
                    && panel.x1 <= viewport.x1 + LAYOUT_TOLERANCE,
                "transcript panel must remain within the viewport width: {panel:?}"
            );
            for (label, card) in [("selected model card", model), ("hotkey card", hotkey)] {
                assert!(
                    card.x0 >= panel.x0 - LAYOUT_TOLERANCE
                        && card.x1 <= panel.x1 + LAYOUT_TOLERANCE,
                    "{label} {card:?} must remain within content width {panel:?}"
                );
                assert_bounds_within(card, viewport, label);
                assert_within_tolerance(
                    card.y1 - card.y0,
                    44.0,
                    3.0,
                    "compact selector card height",
                );
            }
            assert_within_tolerance(model.y0, 118.0, 3.0, "selector row start");
            if width <= 960.0 {
                assert!(
                    model.y1 <= hotkey.y0 + LAYOUT_TOLERANCE,
                    "compact selector cards must stack: {model:?} and {hotkey:?}"
                );
                assert_within_tolerance(hotkey.y0, 178.0, 3.0, "stacked hotkey row start");
                assert_within_tolerance(panel.y0, 242.0, 6.0, "compact transcript panel top");
            } else {
                assert!(
                    model.x1 <= hotkey.x0 + LAYOUT_TOLERANCE,
                    "selector cards overlap: {model:?} and {hotkey:?}"
                );
                assert_within_tolerance(hotkey.y0, 118.0, 3.0, "wide hotkey row start");
                assert_within_tolerance(panel.y0, 185.0, 6.0, "wide transcript panel top");
            }

            let inline_transcript = format!("{committed} {provisional}");
            let bounds = node_matching(&output, |node| {
                node.name() == Some(inline_transcript.as_str())
            })
            .bounds()
            .expect("inline transcript label should expose bounds");
            assert_bounds_within(bounds, panel, "wrapped inline transcript text");
            assert!(
                bounds.y1 - bounds.y0 > 32.0,
                "inline transcript label did not wrap: {bounds:?}"
            );
            assert!(
                !node_names(&output).iter().any(|name| name == &provisional),
                "provisional text should be appended to the committed transcript"
            );
            for name in ["Clear", "Copy"] {
                let bounds = node_matching(&output, |node| {
                    node.name()
                        .is_some_and(|actual| actual == name || actual.contains(name))
                })
                .bounds()
                .expect("transcript action should expose bounds");
                assert_bounds_within(bounds, panel, name);
            }
            let normal = render(Fixture::TranscribeReady, width, height);
            let normal_panel = named_node_bounds(&normal, "Transcript panel");
            let clear = node_matching(&normal, |node| node.name() == Some("Clear"))
                .bounds()
                .expect("Clear should expose bounds");
            let copy = node_matching(&normal, |node| {
                node.name()
                    .is_some_and(|name| name == "Copy" || name.contains("Copy"))
            })
            .bounds()
            .expect("Copy should expose bounds");
            let helper = node_matching(&normal, |node| {
                node.name()
                    .is_some_and(|name| name.contains("Silence is ignored"))
            })
            .bounds()
            .expect("Silence helper should expose bounds");
            let (panel_top, panel_height, footer_top) = if width <= 960.0 {
                (242.0, 406.0, 590.0)
            } else {
                (185.0, 565.0, 695.0)
            };
            assert_within_tolerance(
                normal_panel.y0,
                panel_top,
                6.0,
                "reference transcript panel top",
            );
            assert_within_tolerance(
                normal_panel.y1 - normal_panel.y0,
                panel_height,
                8.0,
                "reference transcript panel height",
            );
            for action_bounds in [clear, copy] {
                assert_within_tolerance(action_bounds.y0, footer_top, 7.0, "transcript footer top");
                assert_within_tolerance(
                    normal_panel.y1 - action_bounds.y1,
                    14.0,
                    3.0,
                    "transcript footer bottom inset",
                );
            }
            assert_within_tolerance(normal_panel.x1 - copy.x1, 16.0, 3.0, "Copy right inset");
            if width > 960.0 {
                assert_bounds_within(helper, viewport, "Silence helper");
                assert!(
                    helper.y1 <= viewport.y1 + LAYOUT_TOLERANCE,
                    "Silence helper must remain within the central viewport: {helper:?}"
                );
            }
        }
    }

    #[test]
    fn no_model_layout_keeps_the_bordered_empty_state_and_hides_transcript_controls() {
        for (width, height) in [(1180.0, 815.0), (960.0, 680.0)] {
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
            let selector = named_node_bounds(&output, "Selected model");
            let hotkey = named_node_bounds(&output, "Recording hotkey");
            let empty_state = named_node_bounds(&output, "Model required empty state");
            let select = node_matching(&output, |node| node.name() == Some("Select"))
                .bounds()
                .expect("Select should expose bounds");
            assert_bounds_within(panel, viewport, "transcript panel");
            assert_bounds_within(selector, viewport, "selected model card");
            assert_bounds_within(hotkey, viewport, "hotkey card");
            assert_bounds_within(empty_state, panel, "model-required empty state");
            for card in [selector, hotkey] {
                assert_within_tolerance(card.y1 - card.y0, 44.0, 3.0, "selector card height");
            }
            assert_within_tolerance(selector.y0, 118.0, 3.0, "model row start");
            assert!(
                selector.x0 >= panel.x0 - LAYOUT_TOLERANCE
                    && selector.x1 <= panel.x1 + LAYOUT_TOLERANCE,
                "selected model card {selector:?} must fit transcript panel {panel:?}"
            );
            if width <= 960.0 {
                assert!(selector.y1 <= hotkey.y0 + LAYOUT_TOLERANCE);
                assert_within_tolerance(hotkey.y0, 178.0, 3.0, "stacked hotkey row start");
                assert_within_tolerance(panel.y0, 242.0, 6.0, "compact model-required panel top");
                assert_within_tolerance(
                    panel.y1 - panel.y0,
                    406.0,
                    8.0,
                    "compact model-required panel height",
                );
            } else {
                assert_within_tolerance(hotkey.y0, 118.0, 3.0, "wide hotkey row start");
                assert_within_tolerance(panel.y0, 185.0, 6.0, "wide model-required panel top");
                assert_within_tolerance(
                    panel.y1 - panel.y0,
                    565.0,
                    6.0,
                    "wide model-required panel height",
                );
                let helper = node_matching(&output, |node| {
                    node.name()
                        .is_some_and(|name| name.contains("Silence is ignored"))
                })
                .bounds()
                .expect("Silence helper should expose bounds");
                assert_bounds_within(helper, viewport, "Silence helper");
            }
            assert_within_tolerance(selector.x1 - select.x1, 16.0, 1.0, "Select right inset");

            let panel_midpoint = (panel.y0 + panel.y1) / 2.0;
            let empty_midpoint = (empty_state.y0 + empty_state.y1) / 2.0;
            assert!(
                (empty_midpoint - panel_midpoint).abs() <= (panel.y1 - panel.y0) * 0.04,
                "empty state should remain centered in its panel: panel={panel:?}, empty={empty_state:?}"
            );
            let update = output.platform_output.accesskit_update.as_ref().unwrap();
            assert!(!update.nodes.iter().any(|(_, node)| {
                node.role() == egui::accesskit::Role::Heading && node.name() == Some("Transcript")
            }));
            assert!(!update.nodes.iter().any(|(_, node)| {
                node.name()
                    .is_some_and(|name| name == "Clear" || name.contains("Copy"))
            }));
        }
    }

    #[test]
    fn selector_actions_use_fixed_trailing_accessible_targets() {
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
        let card = named_node_bounds(&no_model_output, "Selected model");
        let select = node_matching(&no_model_output, |node| {
            node.role() == egui::accesskit::Role::Button && node.name() == Some("Select")
        });
        let select_bounds = select.bounds().expect("Select should expose bounds");
        assert!(!select.is_disabled());
        assert_within_tolerance(
            card.x1,
            889.2,
            1.0,
            "Selected model card right edge at the 1180px reference width",
        );
        assert_within_tolerance(
            select_bounds.x1 - select_bounds.x0,
            72.0,
            1.0,
            "Select target width",
        );
        assert_within_tolerance(
            select_bounds.y1 - select_bounds.y0,
            44.0,
            1.0,
            "Select target height",
        );
        assert_within_tolerance(
            card.x1 - select_bounds.x1,
            16.0,
            1.0,
            "Select right inset from visible model card",
        );
        assert_eq!(
            click_named_control(
                &ctx,
                &mut no_model_data,
                &mut no_model_page,
                width,
                height,
                "Select",
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
                "Change",
            ),
            ScreenAction::ChangeModel,
        );

        let listening = render(Fixture::TranscribeListening, width, height);
        let disabled_change = node_matching(&listening, |node| {
            node.role() == egui::accesskit::Role::Button && node.name() == Some("Change")
        });
        assert!(disabled_change.is_disabled());
        assert_eq!(
            disabled_change.description(),
            Some("Model selection is unavailable while recording.")
        );
    }

    #[test]
    fn transcribe_fixtures_keep_the_polished_reference_content_and_insets() {
        let ready = render(Fixture::TranscribeReady, 1180.0, 815.0);
        let panel = named_node_bounds(&ready, "Transcript panel");
        let ready_status = named_node_bounds(&ready, "Recording status");
        let start = node_matching(&ready, |node| {
            node.role() == egui::accesskit::Role::Button && node.name() == Some("Start recording")
        })
        .bounds()
        .expect("Start recording should expose bounds");
        let transcript = node_matching(&ready, |node| {
            node.name()
                == Some(
                    "Today's meeting notes regarding the local-first architecture. We discussed the importance of privacy and keeping all model inference on the user's machine to ensure zero data leakage. The performance of the small models is acceptable for dictation, but we might need to explore quantized larger models for complex technical jargon.",
                )
        })
        .bounds()
        .expect("reference transcript should expose bounds");
        let relative_time = node_matching(&ready, |node| node.name() == Some("2 MINS AGO"))
            .bounds()
            .expect("relative-time chip should expose bounds");
        let model_chip = node_matching(&ready, |node| node.name() == Some("BASE.EN"))
            .bounds()
            .expect("model chip should expose bounds");

        for name in ["whisper.cpp base.en", "+", "2 MINS AGO", "BASE.EN"] {
            assert!(
                node_names(&ready).iter().any(|actual| actual == name),
                "ready fixture missing polished reference content {name}"
            );
        }
        assert!(
            start.y1 - start.y0 >= 44.0 - LAYOUT_TOLERANCE,
            "recording control must retain a 44px target: {start:?}"
        );
        assert_within_tolerance(
            ready_status.y1 - ready_status.y0,
            80.0,
            1.0,
            "ready status strip height",
        );
        assert_within_tolerance(
            start.y0 - ready_status.y0,
            18.0,
            3.0,
            "centered recording control top inset",
        );
        assert_within_tolerance(
            ready_status.y1 - start.y1,
            12.0,
            1.0,
            "centered recording control bottom inset",
        );
        assert_within_tolerance(
            transcript.x0 - panel.x0,
            27.0,
            4.0,
            "transcript body left inset",
        );
        for (name, bounds) in [
            ("relative-time chip", relative_time),
            ("model chip", model_chip),
        ] {
            assert_within_tolerance(bounds.y1 - bounds.y0, 26.0, 1.0, name);
            assert_bounds_within(bounds, panel, name);
        }

        let microphone = render(Fixture::TranscribeMicrophoneError, 1180.0, 815.0);
        let canonical_count = node_names(&microphone)
            .iter()
            .filter(|name| name.as_str() == "Scribe couldn’t access your microphone.")
            .count();
        assert_eq!(
            canonical_count, 1,
            "microphone error should not repeat its headline"
        );

        for fixture in [Fixture::TranscribeListening, Fixture::TranscribeFinalizing] {
            let output = render(fixture, 1180.0, 815.0);
            let status = named_node_bounds(&output, "Recording status");
            assert_within_tolerance(
                status.y1 - status.y0,
                80.0,
                1.0,
                "every transcript-present phase uses the same status strip height",
            );
        }

        let no_model = render(Fixture::TranscribeNoModel, 1180.0, 815.0);
        assert!(node_names(&no_model).iter().any(|name| name == "Add model"));
        assert!(!node_names(&ready).iter().any(|name| name == "Transcript"));
    }

    fn tab_event(backwards: bool) -> egui::Event {
        egui::Event::Key {
            key: egui::Key::Tab,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers {
                shift: backwards,
                ..Default::default()
            },
        }
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
    fn model_dialogs_keep_background_controls_inactive() {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        configure_accessible_style(&ctx);
        let mut page = Fixture::ModelsInstalled.page();

        let mut add = Fixture::ModelsInstalled.data();
        add.model_management.dialog = Some(ModelDialog::Add);
        add.model_management.focus_dialog_initial = true;
        let (output, action) =
            render_with_input(&ctx, &mut add, &mut page, 1180.0, 815.0, Vec::new());
        assert_eq!(action, ScreenAction::None);
        assert_eq!(focused_node(&output).name(), Some("GGUF file path"));

        let mut details = Fixture::ModelsInstalled.data();
        details.model_management.dialog = Some(ModelDialog::Details("base.en".into()));
        details.model_management.focus_dialog_initial = true;
        let (output, action) =
            render_with_input(&ctx, &mut details, &mut page, 1180.0, 815.0, Vec::new());
        assert_eq!(action, ScreenAction::None);
        assert_eq!(focused_node(&output).name(), Some("Close model details"));
        let (output, action) = render_with_input(
            &ctx,
            &mut details,
            &mut page,
            1180.0,
            815.0,
            vec![tab_event(false)],
        );
        assert_eq!(action, ScreenAction::None);
        assert!(
            node_matching(&output, |node| {
                node.role() == egui::accesskit::Role::Button
                    && node.name() == Some("Expand comparison")
            })
            .is_disabled()
        );

        let mut remove = Fixture::ModelsInstalled.data();
        remove.model_management.dialog = Some(ModelDialog::Remove("tiny.en".into()));
        remove.model_management.focus_dialog_initial = true;
        let (output, action) =
            render_with_input(&ctx, &mut remove, &mut page, 1180.0, 815.0, Vec::new());
        assert_eq!(action, ScreenAction::None);
        assert_eq!(focused_node(&output).name(), Some("Cancel"));
    }

    #[test]
    fn details_drawer_cycles_tab_focus_without_reaching_models_controls() {
        let (width, height) = (1180.0, 815.0);
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        configure_accessible_style(&ctx);
        let mut data = Fixture::ModelsInstalled.data();
        let mut page = Fixture::ModelsInstalled.page();
        data.model_management.dialog = Some(ModelDialog::Details("tiny.en".into()));
        data.model_management.focus_dialog_initial = true;

        let (initial, action) =
            render_with_input(&ctx, &mut data, &mut page, width, height, Vec::new());
        assert_eq!(action, ScreenAction::None);
        assert_eq!(focused_node(&initial).name(), Some("Close model details"));

        for _ in 0..8 {
            let (mut output, action) = render_with_input(
                &ctx,
                &mut data,
                &mut page,
                width,
                height,
                vec![tab_event(false)],
            );
            assert_eq!(action, ScreenAction::None);
            let drawer_control = [
                "Advanced model information",
                "Remove model from device",
                "Use this model",
                "Close model details",
            ];
            if !drawer_control.contains(&focused_node(&output).name().unwrap_or_default()) {
                let (settled, action) =
                    render_with_input(&ctx, &mut data, &mut page, width, height, Vec::new());
                assert_eq!(action, ScreenAction::None);
                output = settled;
            }
            assert!(
                drawer_control.contains(&focused_node(&output).name().unwrap_or_default()),
                "Tab focus must stay within a named Details control"
            );
            assert!(
                node_matching(&output, |node| {
                    node.role() == egui::accesskit::Role::Button
                        && node.name() == Some("Import local GGUF")
                })
                .is_disabled(),
                "the Models page must stay inert while the drawer is open"
            );
        }
    }

    #[test]
    fn details_drawer_pins_and_initially_focuses_close_in_the_header_corner() {
        let (width, height) = (960.0, 680.0);
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        configure_accessible_style(&ctx);
        let mut data = Fixture::ModelsInstalled.data();
        let mut page = Fixture::ModelsInstalled.page();
        data.model_management.dialog = Some(ModelDialog::Details("tiny.en".into()));
        data.model_management.focus_dialog_initial = true;

        let (output, action) =
            render_with_input(&ctx, &mut data, &mut page, width, height, Vec::new());
        assert_eq!(action, ScreenAction::None);
        let drawer = named_node_bounds(&output, "Model details for whisper.cpp tiny.en");
        let close = named_node_bounds(&output, "Close model details");
        assert!(
            close.x1 >= drawer.x1 - 20.0 && close.y0 <= drawer.y0 + 20.0,
            "Close must stay in the drawer's top-right header corner: drawer={drawer:?}, close={close:?}"
        );
        assert_eq!(focused_node(&output).name(), Some("Close model details"));
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
    fn details_dialog_escape_leaves_no_button_focused() {
        let (width, height) = (1180.0, 815.0);
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        configure_accessible_style(&ctx);
        let mut data = Fixture::ModelsInstalled.data();
        let mut page = Fixture::ModelsInstalled.page();
        data.model_management.dialog = Some(ModelDialog::Details("tiny.en".into()));
        data.model_management.focus_dialog_initial = true;

        let (_, action) = render_with_input(
            &ctx,
            &mut data,
            &mut page,
            width,
            height,
            vec![page_event(egui::Key::Escape)],
        );
        assert_eq!(action, ScreenAction::CloseModelDialog);
        apply_action(&mut data, &mut page, action);

        let (output, action) =
            render_with_input(&ctx, &mut data, &mut page, width, height, Vec::new());
        // A stable toolbar fallback is used for explicit row-removal cancellation.
        // Exact card restoration here can freeze Windows AccessKit during remount.
        assert_eq!(focused_node(&output).name(), None);
        assert_eq!(action, ScreenAction::None);
    }

    #[test]
    fn remote_details_drawer_escape_leaves_no_button_focused() {
        let (width, height) = (1180.0, 815.0);
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        configure_accessible_style(&ctx);
        let mut data = Fixture::ModelsInstalled.data();
        let mut page = Fixture::ModelsInstalled.page();
        data.model_management.dialog = Some(ModelDialog::RemoteDetails {
            entry_id: "trusted-speech/compact-english".into(),
            variant_id: "compact-english-q5".into(),
        });
        data.model_management.focus_dialog_initial = true;

        let (initial, initial_action) =
            render_with_input(&ctx, &mut data, &mut page, width, height, Vec::new());
        assert_eq!(initial_action, ScreenAction::None);
        let close = node_matching(&initial, |node| {
            node.role() == egui::accesskit::Role::Button
                && node.name() == Some("Close model details")
        });
        assert!(
            !close.is_disabled(),
            "the drawer close control must stay enabled"
        );
        assert_eq!(focused_node(&initial).name(), Some("Close model details"));

        let (_, action) = render_with_input(
            &ctx,
            &mut data,
            &mut page,
            width,
            height,
            vec![page_event(egui::Key::Escape)],
        );
        assert_eq!(action, ScreenAction::CloseModelDialog);
        apply_action(&mut data, &mut page, action);
        let (output, action) =
            render_with_input(&ctx, &mut data, &mut page, width, height, Vec::new());
        assert_eq!(focused_node(&output).name(), None);
        assert_eq!(action, ScreenAction::None);
    }

    #[test]
    fn details_drawer_close_leaves_no_button_focused() {
        let (width, height) = (1180.0, 815.0);
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        configure_accessible_style(&ctx);
        let mut data = Fixture::ModelsInstalled.data();
        let mut page = Fixture::ModelsInstalled.page();
        data.model_management.dialog = Some(ModelDialog::Details("tiny.en".into()));
        data.model_management.focus_dialog_initial = true;

        let output = render_with_input(&ctx, &mut data, &mut page, width, height, Vec::new()).0;
        let close = named_node_id(&output, "Close model details");
        let (_, action) = render_with_input(
            &ctx,
            &mut data,
            &mut page,
            width,
            height,
            vec![egui::Event::AccessKitActionRequest(
                egui::accesskit::ActionRequest {
                    action: egui::accesskit::Action::Default,
                    target: close,
                    data: None,
                },
            )],
        );
        assert_eq!(action, ScreenAction::CloseModelDialog);
        apply_action(&mut data, &mut page, action);

        let (output, action) =
            render_with_input(&ctx, &mut data, &mut page, width, height, Vec::new());
        assert_eq!(focused_node(&output).name(), None);
        assert_eq!(action, ScreenAction::None);
    }

    #[test]
    fn active_badge_is_centered_on_the_model_name() {
        let output = render(Fixture::ModelsInstalled, 1180.0, 815.0);
        let title = node_matching(&output, |node| {
            node.role() == egui::accesskit::Role::StaticText
                && node.name() == Some("whisper.cpp base.en")
        })
        .bounds()
        .expect("model title should expose visual bounds");
        let badge = node_matching(&output, |node| {
            node.role() == egui::accesskit::Role::StaticText && node.name() == Some("Active")
        })
        .bounds()
        .expect("Active badge should expose visual bounds");
        assert_within_tolerance(
            (badge.y0 + badge.y1) / 2.0,
            (title.y0 + title.y1) / 2.0 - 3.0,
            0.5,
            "Active badge optical vertical center",
        );
    }

    #[test]
    fn clicking_an_inactive_installed_card_selects_the_real_model() {
        let (width, height) = (1180.0, 815.0);
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        configure_accessible_style(&ctx);
        let mut data = Fixture::ModelsInstalled.data();
        let mut page = Fixture::ModelsInstalled.page();

        let (initial, action) =
            render_with_input(&ctx, &mut data, &mut page, width, height, Vec::new());
        assert_eq!(action, ScreenAction::None);
        let card = named_node_bounds(&initial, "whisper.cpp tiny.en model");
        let point = egui::pos2(card.x0 as f32 + 96.0, ((card.y0 + card.y1) / 2.0) as f32);
        let (_, press_action) = render_with_input(
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
        assert_eq!(press_action, ScreenAction::None);
        let (_, release_action) = render_with_input(
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
        );
        assert_eq!(release_action, ScreenAction::SelectModel("tiny.en".into()));
    }

    #[test]
    fn card_click_target_does_not_steal_details_or_remove_actions() {
        let (width, height) = (1180.0, 815.0);
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        configure_accessible_style(&ctx);
        let mut data = Fixture::ModelsInstalled.data();
        let mut page = Fixture::ModelsInstalled.page();

        let (initial, action) =
            render_with_input(&ctx, &mut data, &mut page, width, height, Vec::new());
        assert_eq!(action, ScreenAction::None);
        let details = named_node_bounds(&initial, "Details for whisper.cpp base.en");
        let details_point = egui::pos2(
            ((details.x0 + details.x1) / 2.0) as f32,
            ((details.y0 + details.y1) / 2.0) as f32,
        );
        let (_, press_action) = render_with_input(
            &ctx,
            &mut data,
            &mut page,
            width,
            height,
            vec![
                egui::Event::PointerMoved(details_point),
                egui::Event::PointerButton {
                    pos: details_point,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
        assert_eq!(press_action, ScreenAction::None);
        let (_, details_action) = render_with_input(
            &ctx,
            &mut data,
            &mut page,
            width,
            height,
            vec![
                egui::Event::PointerMoved(details_point),
                egui::Event::PointerButton {
                    pos: details_point,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
        assert_eq!(
            details_action,
            ScreenAction::ShowModelDetails("base.en".into())
        );

        let remove = named_node_bounds(&initial, "Remove whisper.cpp tiny.en from device");
        let remove_point = egui::pos2(
            ((remove.x0 + remove.x1) / 2.0) as f32,
            ((remove.y0 + remove.y1) / 2.0) as f32,
        );
        let (_, press_action) = render_with_input(
            &ctx,
            &mut data,
            &mut page,
            width,
            height,
            vec![
                egui::Event::PointerMoved(remove_point),
                egui::Event::PointerButton {
                    pos: remove_point,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
        assert_eq!(press_action, ScreenAction::None);
        let (_, remove_action) = render_with_input(
            &ctx,
            &mut data,
            &mut page,
            width,
            height,
            vec![
                egui::Event::PointerMoved(remove_point),
                egui::Event::PointerButton {
                    pos: remove_point,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
        assert_eq!(
            remove_action,
            ScreenAction::RequestModelRemoval("tiny.en".into())
        );
    }

    #[test]
    fn clicking_a_legacy_available_card_starts_its_real_upgrade() {
        let (width, height) = (1180.0, 815.0);
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        configure_accessible_style(&ctx);
        let mut data = Fixture::ModelsInstalled.data();
        let mut page = Fixture::ModelsInstalled.page();
        let legacy_index = data
            .models
            .iter()
            .position(|model| model.id == "tiny.en")
            .expect("installed tiny model");
        let mut legacy = data.models.remove(legacy_index);
        legacy.installed = false;
        legacy.legacy_cleanup_pending = true;
        legacy.ready = false;
        legacy.selected = true;
        legacy.download_state = ModelDownloadState::NotInstalled;
        legacy.primary_action_label = "Upgrade model".into();
        legacy.primary_action_enabled = true;
        legacy.primary_action_installs_upgrade = true;
        legacy.description = Some(
            "Legacy GGML file retained for cleanup. Upgrade or open Details to remove it.".into(),
        );
        data.model_catalog.push(legacy);

        let (initial, action) =
            render_with_input(&ctx, &mut data, &mut page, width, height, Vec::new());
        assert_eq!(action, ScreenAction::None);
        assert!(
            node_names(&initial)
                .iter()
                .any(|name| name == "3 model results: 1 installed, 2 available.")
        );
        assert!(
            node_names(&initial)
                .iter()
                .any(|name| name == "Upgrade whisper.cpp tiny.en")
        );
        let card = named_node_bounds(&initial, "whisper.cpp tiny.en model");
        let point = egui::pos2(card.x0 as f32 + 96.0, ((card.y0 + card.y1) / 2.0) as f32);
        let (_, press_action) = render_with_input(
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
        assert_eq!(press_action, ScreenAction::None);
        let (_, release_action) = render_with_input(
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
        );
        assert_eq!(release_action, ScreenAction::UpgradeModel("tiny.en".into()));
    }

    #[test]
    fn legacy_details_accesskit_removal_cancel_returns_to_the_drawer_remove_action() {
        let (width, height) = (1180.0, 815.0);
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        configure_accessible_style(&ctx);
        let mut data = Fixture::ModelsInstalled.data();
        let mut page = Fixture::ModelsInstalled.page();
        let legacy_index = data
            .models
            .iter()
            .position(|model| model.id == "tiny.en")
            .expect("installed tiny model");
        let mut legacy = data.models.remove(legacy_index);
        legacy.installed = false;
        legacy.legacy_cleanup_pending = true;
        legacy.ready = false;
        legacy.selected = true;
        legacy.download_state = ModelDownloadState::NotInstalled;
        legacy.primary_action_label = "Upgrade model".into();
        legacy.primary_action_enabled = true;
        legacy.primary_action_installs_upgrade = true;
        data.model_catalog.push(legacy);

        let initial = render_with_input(&ctx, &mut data, &mut page, width, height, Vec::new()).0;
        let details_id = named_node_id(&initial, "Details for whisper.cpp tiny.en");
        let (_, action) = render_with_input(
            &ctx,
            &mut data,
            &mut page,
            width,
            height,
            vec![egui::Event::AccessKitActionRequest(
                egui::accesskit::ActionRequest {
                    action: egui::accesskit::Action::Default,
                    target: details_id,
                    data: None,
                },
            )],
        );
        assert_eq!(action, ScreenAction::ShowModelDetails("tiny.en".into()));
        apply_action(&mut data, &mut page, action);

        let details = render_with_input(&ctx, &mut data, &mut page, width, height, Vec::new()).0;
        assert_eq!(focused_node(&details).name(), Some("Close model details"));
        let remove_id = named_node_id(&details, "Remove model from device");
        let (_, action) = render_with_input(
            &ctx,
            &mut data,
            &mut page,
            width,
            height,
            vec![egui::Event::AccessKitActionRequest(
                egui::accesskit::ActionRequest {
                    action: egui::accesskit::Action::Default,
                    target: remove_id,
                    data: None,
                },
            )],
        );
        assert_eq!(action, ScreenAction::RequestModelRemoval("tiny.en".into()));
        apply_action(&mut data, &mut page, action);

        let confirmation =
            render_with_input(&ctx, &mut data, &mut page, width, height, Vec::new()).0;
        assert_eq!(focused_node(&confirmation).name(), Some("Cancel"));
        let cancel_id = named_node_id(&confirmation, "Cancel");
        let (_, action) = render_with_input(
            &ctx,
            &mut data,
            &mut page,
            width,
            height,
            vec![egui::Event::AccessKitActionRequest(
                egui::accesskit::ActionRequest {
                    action: egui::accesskit::Action::Default,
                    target: cancel_id,
                    data: None,
                },
            )],
        );
        assert_eq!(action, ScreenAction::CloseModelDialog);
        apply_action(&mut data, &mut page, action);
        assert_eq!(
            data.model_management.dialog,
            Some(ModelDialog::Details("tiny.en".into()))
        );

        let returned = render_with_input(&ctx, &mut data, &mut page, width, height, Vec::new()).0;
        assert_eq!(
            focused_node(&returned).name(),
            Some("Remove model from device")
        );
    }

    #[test]
    fn remove_dialog_cancel_moves_focus_to_the_models_toolbar() {
        let (width, height) = (1180.0, 815.0);
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        configure_accessible_style(&ctx);
        let mut data = Fixture::ModelsInstalled.data();
        let mut page = Fixture::ModelsInstalled.page();
        data.model_management.dialog = Some(ModelDialog::Remove("tiny.en".into()));
        data.model_management.focus_dialog_initial = true;

        let initial = render_with_input(&ctx, &mut data, &mut page, width, height, Vec::new()).0;
        let cancel_id = named_node_id(&initial, "Cancel");
        let (_, action) = render_with_input(
            &ctx,
            &mut data,
            &mut page,
            width,
            height,
            vec![egui::Event::AccessKitActionRequest(
                egui::accesskit::ActionRequest {
                    action: egui::accesskit::Action::Default,
                    target: cancel_id,
                    data: None,
                },
            )],
        );
        assert_eq!(action, ScreenAction::CloseModelDialog);
        apply_action(&mut data, &mut page, action);

        let (output, action) =
            render_with_input(&ctx, &mut data, &mut page, width, height, Vec::new());
        assert_eq!(focused_node(&output).name(), Some("Import local GGUF"));
        assert_eq!(
            action,
            ScreenAction::AcknowledgeModelControlFocus {
                model_id: "tiny.en".into(),
                control: super::super::state::ModelCardControl::Remove,
            }
        );
    }

    #[test]
    fn model_dialogs_are_modal_and_reject_background_accesskit_actions() {
        let (width, height) = (1180.0, 815.0);
        for (dialog, dialog_name, expected_focus) in [
            (
                ModelDialog::Add,
                "Import local GGUF",
                Some("GGUF file path"),
            ),
            (
                ModelDialog::Details("base.en".into()),
                "Model details for whisper.cpp base.en",
                Some("Close model details"),
            ),
            (
                ModelDialog::Remove("tiny.en".into()),
                "Remove whisper.cpp tiny.en",
                Some("Cancel"),
            ),
        ] {
            let ctx = egui::Context::default();
            ctx.enable_accesskit();
            configure_accessible_style(&ctx);
            let mut data = Fixture::ModelsInstalled.data();
            let mut page = Fixture::ModelsInstalled.page();
            data.model_management.dialog = Some(dialog);
            data.model_management.focus_dialog_initial = true;

            let (initial, action) =
                render_with_input(&ctx, &mut data, &mut page, width, height, Vec::new());
            assert_eq!(action, ScreenAction::None);
            let dialog_node = node_matching(&initial, |node| {
                node.name() == Some(dialog_name) && node.is_modal()
            });
            assert!(dialog_node.is_modal(), "{dialog_name} must be modal");
            let dock = node_matching(&initial, |node| {
                node.role() == egui::accesskit::Role::Button
                    && node.name() == Some("Expand comparison")
            });
            assert!(
                dock.is_disabled(),
                "comparison dock must be disabled behind {dialog_name}"
            );
            let dock_id = named_node_id(&initial, "Expand comparison");

            assert_eq!(focused_node(&initial).name(), expected_focus);
            let (_, action) = render_with_input(
                &ctx,
                &mut data,
                &mut page,
                width,
                height,
                vec![egui::Event::AccessKitActionRequest(
                    egui::accesskit::ActionRequest {
                        action: egui::accesskit::Action::Default,
                        target: dock_id,
                        data: None,
                    },
                )],
            );
            assert_eq!(
                action,
                ScreenAction::None,
                "comparison dock must not act while {dialog_name} is open"
            );
        }
    }

    #[test]
    fn comparison_dock_layer_stays_above_routes_and_below_active_model_dialogs() {
        let (width, height) = (1180.0, 815.0);
        for (dialog, dialog_name, expected_order) in [
            (
                ModelDialog::Add,
                "Import local GGUF",
                egui::Order::Foreground,
            ),
            (
                ModelDialog::Details("base.en".into()),
                "Model details for whisper.cpp base.en",
                egui::Order::Foreground,
            ),
            (
                ModelDialog::Remove("tiny.en".into()),
                "Remove whisper.cpp tiny.en",
                egui::Order::Foreground,
            ),
        ] {
            let ctx = egui::Context::default();
            ctx.enable_accesskit();
            configure_accessible_style(&ctx);
            let mut data = Fixture::ModelsCompareExpanded.data();
            let mut page = Fixture::ModelsCompareExpanded.page();

            let (without_dialog, action) =
                render_with_input(&ctx, &mut data, &mut page, width, height, Vec::new());
            assert_eq!(action, ScreenAction::None);
            let surface = named_node_bounds(&without_dialog, "Model comparison surface");
            let dock_probe = egui::pos2(
                ((surface.x0 + surface.x1) / 2.0) as f32,
                ((surface.y0 + surface.y1) / 2.0) as f32,
            );
            let foreground_dock_layer = ctx
                .memory(|memory| memory.layer_id_at(dock_probe))
                .expect("expanded comparison dock should own its visible surface");
            assert_eq!(foreground_dock_layer.order, egui::Order::Foreground);

            data.model_management.dialog = Some(dialog);
            data.model_management.focus_dialog_initial = true;
            let _ = render_with_input(&ctx, &mut data, &mut page, width, height, Vec::new());
            let (with_dialog, action) =
                render_with_input(&ctx, &mut data, &mut page, width, height, Vec::new());
            assert_eq!(action, ScreenAction::None);
            let dialog_node = node_matching(&with_dialog, |node| {
                node.name() == Some(dialog_name) && node.is_modal()
            });
            assert!(dialog_node.is_modal(), "{dialog_name} must remain modal");
            let dialog_bounds = dialog_node
                .bounds()
                .expect("model dialog should expose bounds");
            assert!(
                dialog_bounds.x0 < surface.x1
                    && dialog_bounds.x1 > surface.x0
                    && dialog_bounds.y0 < surface.y1
                    && dialog_bounds.y1 > surface.y0,
                "{dialog_name} should overlap the expanded comparison dock in this layer fixture"
            );
            let dialog_probe = egui::pos2(
                ((dialog_bounds.x0 + dialog_bounds.x1) / 2.0) as f32,
                ((dialog_bounds.y0 + dialog_bounds.y1) / 2.0) as f32,
            );
            let dialog_layer = ctx
                .memory(|memory| memory.layer_id_at(dialog_probe))
                .expect("active model dialog should own its surface");
            assert_eq!(dialog_layer.order, expected_order);

            let layers = ctx.memory(|memory| memory.layer_ids().collect::<Vec<_>>());
            let dock_index = layers
                .iter()
                .position(|layer| *layer == foreground_dock_layer)
                .expect("comparison dock should retain its foreground layer");
            let dialog_index = layers
                .iter()
                .position(|layer| *layer == dialog_layer)
                .expect("modal frame should contain the model dialog layer");
            assert!(
                dock_index < dialog_index,
                "{dialog_name} must be rendered above the disabled comparison dock"
            );
            assert!(
                node_matching(&with_dialog, |node| {
                    node.role() == egui::accesskit::Role::Button
                        && node.name() == Some("Collapse comparison")
                })
                .is_disabled(),
                "comparison dock must remain inert behind {dialog_name}"
            );
        }
    }

    #[test]
    fn model_dialog_controls_remain_enabled_and_accesskit_actionable() {
        let (width, height) = (1180.0, 815.0);
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        configure_accessible_style(&ctx);
        let mut data = Fixture::ModelsInstalled.data();
        let mut page = Fixture::ModelsInstalled.page();
        data.model_management.dialog = Some(ModelDialog::Remove("tiny.en".into()));
        data.model_management.focus_dialog_initial = true;

        let (initial, action) =
            render_with_input(&ctx, &mut data, &mut page, width, height, Vec::new());
        assert_eq!(action, ScreenAction::None);
        let dialog_id = node_id_matching(&initial, |node| {
            node.role() == egui::accesskit::Role::AlertDialog
                && node.name() == Some("Remove whisper.cpp tiny.en")
        });
        let update = initial.platform_output.accesskit_update.as_ref().unwrap();
        let (remove_id, remove) = update
            .nodes
            .iter()
            .find(|(id, node)| {
                node.role() == egui::accesskit::Role::Button
                    && node.name() == Some("Remove")
                    && !node.is_disabled()
                    && accesskit_descends_from(&initial, dialog_id, *id)
            })
            .expect("enabled Remove button inside removal dialog");
        assert_eq!(remove.role(), egui::accesskit::Role::Button);
        assert!(
            !remove.is_disabled(),
            "dialog Remove control must remain enabled"
        );
        let (_, action) = render_with_input(
            &ctx,
            &mut data,
            &mut page,
            width,
            height,
            vec![egui::Event::AccessKitActionRequest(
                egui::accesskit::ActionRequest {
                    action: egui::accesskit::Action::Default,
                    target: *remove_id,
                    data: None,
                },
            )],
        );
        assert_eq!(action, ScreenAction::ConfirmModelRemoval("tiny.en".into()));
    }

    #[test]
    fn catalog_only_legacy_cleanup_removal_dialog_is_actionable() {
        let (width, height) = (1180.0, 815.0);
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        configure_accessible_style(&ctx);
        let mut data = Fixture::ModelsInstalled.data();
        let mut page = Fixture::ModelsInstalled.page();
        data.model_catalog.push(ModelViewModel {
            id: "whisper_cpp_base_en".into(),
            display_name: "Whisper Base — English".into(),
            legacy_cleanup_pending: true,
            removal_supported: true,
            ..Default::default()
        });
        data.model_management.dialog = Some(ModelDialog::Remove("whisper_cpp_base_en".into()));

        let (initial, action) =
            render_with_input(&ctx, &mut data, &mut page, width, height, Vec::new());
        assert_eq!(action, ScreenAction::None);
        let dialog_id = node_id_matching(&initial, |node| {
            node.role() == egui::accesskit::Role::AlertDialog
                && node.name() == Some("Remove Whisper Base — English")
        });
        let update = initial.platform_output.accesskit_update.as_ref().unwrap();
        let remove_id = update
            .nodes
            .iter()
            .find(|(id, node)| {
                node.role() == egui::accesskit::Role::Button
                    && node.name() == Some("Remove")
                    && !node.is_disabled()
                    && accesskit_descends_from(&initial, dialog_id, *id)
            })
            .map(|(id, _)| *id)
            .expect("enabled Remove button for catalog-only legacy cleanup");
        let (_, action) = render_with_input(
            &ctx,
            &mut data,
            &mut page,
            width,
            height,
            vec![egui::Event::AccessKitActionRequest(
                egui::accesskit::ActionRequest {
                    action: egui::accesskit::Action::Default,
                    target: remove_id,
                    data: None,
                },
            )],
        );
        assert_eq!(
            action,
            ScreenAction::ConfirmModelRemoval("whisper_cpp_base_en".into())
        );
    }

    #[test]
    fn model_dialog_and_comparison_table_preserve_accessible_hierarchy() {
        let (width, height) = (1180.0, 815.0);
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        configure_accessible_style(&ctx);
        let mut data = Fixture::ModelsInstalled.data();
        let mut page = Fixture::ModelsInstalled.page();
        data.model_management.dialog = Some(ModelDialog::Details("tiny.en".into()));

        let dialog_output =
            render_with_input(&ctx, &mut data, &mut page, width, height, Vec::new()).0;
        let dialog_id = node_id_matching(&dialog_output, |node| {
            node.role() == egui::accesskit::Role::Dialog
                && node.name() == Some("Model details for whisper.cpp tiny.en")
        });
        for control in [
            "Use this model",
            "Remove model from device",
            "Close model details",
        ] {
            let control_id = node_id_matching(&dialog_output, |node| {
                node.role() == egui::accesskit::Role::Button && node.name() == Some(control)
            });
            assert!(
                accesskit_descends_from(&dialog_output, dialog_id, control_id),
                "{control} must descend from the details dialog"
            );
        }
        assert!(
            node_names(&dialog_output)
                .iter()
                .any(|name| name == "Advanced model information"),
            "details must offer the progressive-disclosure metadata control"
        );

        let comparison_output = render(Fixture::ModelsCompareExpanded, 1476.0, 1018.0);
        let table_id = node_id_matching(&comparison_output, |node| {
            node.role() == egui::accesskit::Role::Table
                && node.name() == Some("Model comparison results")
        });
        let surface_id = named_node_id(&comparison_output, "Model comparison surface");
        assert!(
            accesskit_descends_from(&comparison_output, surface_id, table_id),
            "wide result table must descend from the comparison surface"
        );
        for model in ["whisper.cpp base.en", "whisper.cpp tiny.en"] {
            let row_id = node_id_matching(&comparison_output, |node| {
                node.role() == egui::accesskit::Role::Row
                    && node.name() == Some(format!("Comparison result for {model}").as_str())
            });
            assert!(
                accesskit_descends_from(&comparison_output, table_id, row_id),
                "{model} row must descend from the comparison table"
            );
            let accuracy_cell_id = node_id_matching(&comparison_output, |node| {
                node.role() == egui::accesskit::Role::Cell
                    && node.name() == Some(format!("Accuracy for {model}").as_str())
            });
            let accuracy_action_id = node_id_matching(&comparison_output, |node| {
                node.role() == egui::accesskit::Role::Button
                    && node.description().is_some_and(|description| {
                        description
                            == format!(
                                "Add a reference transcript to measure accuracy for {model}."
                            )
                    })
            });
            assert!(
                accesskit_descends_from(&comparison_output, row_id, accuracy_cell_id),
                "{model} accuracy cell must descend from its row"
            );
            assert!(
                accesskit_descends_from(&comparison_output, accuracy_cell_id, accuracy_action_id,),
                "{model} accuracy action must descend from its cell"
            );
        }

        let compact_output = render(Fixture::ModelsCompareExpanded, 960.0, 680.0);
        let compact_surface_id = named_node_id(&compact_output, "Model comparison surface");
        for model in ["whisper.cpp base.en", "whisper.cpp tiny.en"] {
            let group_id = node_id_matching(&compact_output, |node| {
                node.role() == egui::accesskit::Role::Group
                    && node.name() == Some(format!("Comparison result for {model}").as_str())
            });
            let accuracy_action_id = node_id_matching(&compact_output, |node| {
                node.role() == egui::accesskit::Role::Button
                    && node.description().is_some_and(|description| {
                        description
                            == format!(
                                "Add a reference transcript to measure accuracy for {model}."
                            )
                    })
            });
            assert!(
                accesskit_descends_from(&compact_output, compact_surface_id, group_id),
                "{model} compact group must descend from the comparison surface"
            );
            assert!(
                accesskit_descends_from(&compact_output, group_id, accuracy_action_id),
                "{model} compact accuracy action must descend from its result group"
            );
        }
    }

    #[test]
    fn installed_model_cards_are_compact_and_expose_details_without_row_activation() {
        let (width, height) = (1180.0, 815.0);
        let row_name = "whisper.cpp tiny.en model";
        let details_name = "Details for whisper.cpp tiny.en";

        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        configure_accessible_style(&ctx);
        let mut data = Fixture::ModelsInstalled.data();
        let mut page = Fixture::ModelsInstalled.page();
        let output = render_with_input(&ctx, &mut data, &mut page, width, height, Vec::new()).0;
        let row = named_node_bounds(&output, row_name);
        assert!(
            ((row.y1 - row.y0) - 76.0).abs() <= LAYOUT_TOLERANCE,
            "installed card height should be 76 px, got {}",
            row.y1 - row.y0
        );
        assert!(
            !node_names(&output)
                .iter()
                .any(|name| name == "Use this model whisper.cpp tiny.en"),
            "inactive activation must be disclosed only in the Details drawer"
        );
        let details_id = named_node_id(&output, details_name);
        let details = output
            .platform_output
            .accesskit_update
            .as_ref()
            .expect("render should expose an AccessKit update")
            .nodes
            .iter()
            .find_map(|(id, node)| (*id == details_id).then(|| node.bounds()).flatten())
            .expect("tiny model Details control should expose bounds");
        assert_bounds_within(details, row, "tiny model Details control");
    }

    #[test]
    fn installed_model_rows_and_metadata_stay_inside_the_route_inset() {
        let row_name = "whisper.cpp base.en model";
        for (width, height) in [(1476.0, 1018.0), (1180.0, 815.0), (960.0, 680.0)] {
            let output = render(Fixture::ModelsInstalled, width, height);
            for header in ["MODEL", "LANGUAGES", "SPEED", "ACCURACY", "SIZE"] {
                assert!(
                    node_names(&output).iter().any(|name| name == header),
                    "{header} header should render at {width}x{height}"
                );
            }
            let surface = named_node_bounds(&output, "Model comparison surface");
            let row = named_node_bounds(&output, row_name);
            assert!(
                row.x0 >= surface.x0 - LAYOUT_TOLERANCE && row.x1 <= surface.x1 + LAYOUT_TOLERANCE,
                "installed model card {row:?} must remain inside route inset {surface:?}"
            );

            let row_contents: Vec<_> = output
                .platform_output
                .accesskit_update
                .as_ref()
                .expect("render should expose an AccessKit update")
                .nodes
                .iter()
                .filter_map(|(_, node)| {
                    let name = node.name()?;
                    let bounds = node.bounds()?;
                    (name != row_name
                        && bounds.y0 >= row.y0 - LAYOUT_TOLERANCE
                        && bounds.y1 <= row.y1 + LAYOUT_TOLERANCE
                        && bounds.x0 >= surface.x0 - LAYOUT_TOLERANCE
                        && bounds.y1 > bounds.y0)
                        .then_some((name, bounds))
                })
                .collect();
            assert!(
                row_contents
                    .iter()
                    .any(|(name, _)| name.contains("English"))
                    && row_contents
                        .iter()
                        .any(|(name, _)| name.contains("Balanced")),
                "installed card should expose language and speed metadata at {width}x{height}"
            );
            for (name, bounds) in row_contents {
                assert_bounds_within(bounds, row, &format!("installed row content {name:?}"));
            }
        }
    }

    #[test]
    fn narrow_model_rows_keep_actions_and_metadata_inside_the_viewport() {
        let output = render(Fixture::ModelsLifecycle, 375.0, 680.0);
        let nodes = &output
            .platform_output
            .accesskit_update
            .as_ref()
            .expect("render should expose an AccessKit update")
            .nodes;
        let rows = nodes
            .iter()
            .filter_map(|(_, node)| {
                (node.role() == egui::accesskit::Role::Group
                    && node.name().is_some_and(|name| name.ends_with(" model")))
                .then(|| node.bounds())
                .flatten()
            })
            .collect::<Vec<_>>();
        assert!(!rows.is_empty(), "narrow fixture should render model rows");
        for row in rows {
            assert_within_tolerance(
                row.y1 - row.y0,
                124.0,
                LAYOUT_TOLERANCE,
                "narrow model row height",
            );
            assert!(
                row.x0 >= 0.0 && row.x1 <= 375.0,
                "row escaped narrow viewport: {row:?}"
            );
        }
        for (_, node) in nodes.iter().filter(|(_, node)| {
            node.role() == egui::accesskit::Role::Button
                && node.name().is_some_and(|name| {
                    name.starts_with("Details for ")
                        || name.starts_with("Download ")
                        || name.starts_with("Resume ")
                        || name.starts_with("Retry ")
                        || name.starts_with("Cancel ")
                })
        }) {
            let bounds = node.bounds().expect("model action should expose bounds");
            let parent_row = nodes.iter().find_map(|(_, candidate)| {
                (candidate.role() == egui::accesskit::Role::Group
                    && candidate.name() == Some("whisper.cpp base.en model"))
                .then(|| candidate.bounds())
                .flatten()
            });
            assert!(
                bounds.x0 >= 0.0 && bounds.x1 <= 375.0 && bounds.y1 > bounds.y0,
                "narrow model action {:?} escaped viewport: {bounds:?}; base row={parent_row:?}",
                node.name(),
            );
            assert!(
                bounds.x1 - bounds.x0 >= 44.0 - LAYOUT_TOLERANCE
                    && bounds.y1 - bounds.y0 >= 44.0 - LAYOUT_TOLERANCE,
                "model action must retain a 44px target: {bounds:?}"
            );
        }
        assert!(
            nodes
                .iter()
                .any(|(_, node)| { node.name().is_some_and(|name| name.contains("MB")) }),
            "narrow rows must preserve the model size metadata"
        );
    }

    #[test]
    fn model_details_preserve_use_and_remove_actions_with_disabled_reasons() {
        let (width, height) = (1180.0, 815.0);
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        configure_accessible_style(&ctx);
        let mut data = Fixture::ModelsInstalled.data();
        let mut page = Fixture::ModelsInstalled.page();
        data.model_management.dialog = Some(ModelDialog::Details("tiny.en".into()));
        let output = render_with_input(&ctx, &mut data, &mut page, width, height, Vec::new()).0;
        let dialog_id = node_id_matching(&output, |node| {
            node.role() == egui::accesskit::Role::Dialog
                && node.name() == Some("Model details for whisper.cpp tiny.en")
        });
        let use_id = node_id_matching(&output, |node| {
            node.role() == egui::accesskit::Role::Button && node.name() == Some("Use this model")
        });
        assert!(accesskit_descends_from(&output, dialog_id, use_id));
        let (_, action) = render_with_input(
            &ctx,
            &mut data,
            &mut page,
            width,
            height,
            vec![egui::Event::AccessKitActionRequest(
                egui::accesskit::ActionRequest {
                    action: egui::accesskit::Action::Default,
                    target: use_id,
                    data: None,
                },
            )],
        );
        assert_eq!(action, ScreenAction::SelectModel("tiny.en".into()));

        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        configure_accessible_style(&ctx);
        let mut repair = Fixture::ModelsInstalled.data();
        let repair_model = repair
            .models
            .iter_mut()
            .find(|model| model.id == "tiny.en")
            .unwrap();
        repair_model.primary_action_label = "Repair runtime".into();
        repair_model.primary_action_enabled = true;
        repair_model.primary_action_repairs_runtime = true;
        repair_model.primary_action_disabled_reason = None;
        repair.model_management.dialog = Some(ModelDialog::Details("tiny.en".into()));
        let mut repair_page = Fixture::ModelsInstalled.page();
        let initial = render_with_input(
            &ctx,
            &mut repair,
            &mut repair_page,
            width,
            height,
            Vec::new(),
        )
        .0;
        let advanced_id = node_id_matching(&initial, |node| {
            node.role() == egui::accesskit::Role::Button
                && node.name() == Some("Advanced model information")
        });
        let (output, advanced_action) = render_with_input(
            &ctx,
            &mut repair,
            &mut repair_page,
            width,
            height,
            vec![egui::Event::AccessKitActionRequest(
                egui::accesskit::ActionRequest {
                    action: egui::accesskit::Action::Default,
                    target: advanced_id,
                    data: None,
                },
            )],
        );
        assert_eq!(advanced_action, ScreenAction::None);
        let repair_id = node_id_matching(&output, |node| {
            node.role() == egui::accesskit::Role::Button
                && node.name() == Some("Repair runtime")
                && !node.is_disabled()
        });
        let (_, repair_action) = render_with_input(
            &ctx,
            &mut repair,
            &mut repair_page,
            width,
            height,
            vec![egui::Event::AccessKitActionRequest(
                egui::accesskit::ActionRequest {
                    action: egui::accesskit::Action::Default,
                    target: repair_id,
                    data: None,
                },
            )],
        );
        assert_eq!(
            repair_action,
            ScreenAction::RepairModelRuntime("tiny.en".into())
        );

        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        configure_accessible_style(&ctx);
        let mut data = Fixture::ModelsInstalled.data();
        let mut page = Fixture::ModelsInstalled.page();
        data.model_management.dialog = Some(ModelDialog::Details("tiny.en".into()));
        let output = render_with_input(&ctx, &mut data, &mut page, width, height, Vec::new()).0;
        let remove_id = named_node_id(&output, "Remove model from device");
        let (_, action) = render_with_input(
            &ctx,
            &mut data,
            &mut page,
            width,
            height,
            vec![egui::Event::AccessKitActionRequest(
                egui::accesskit::ActionRequest {
                    action: egui::accesskit::Action::Default,
                    target: remove_id,
                    data: None,
                },
            )],
        );
        assert_eq!(action, ScreenAction::RequestModelRemoval("tiny.en".into()));

        let mut active = Fixture::ModelsInstalled.data();
        active.models[0].primary_action_disabled_reason =
            Some("This model is already active.".into());
        active
            .models
            .iter_mut()
            .find(|model| model.id == "tiny.en")
            .expect("fixture includes a second installed model")
            .ready = false;
        active.model_management.dialog = Some(ModelDialog::Details("base.en".into()));
        let output = render_with_input(&ctx, &mut active, &mut page, width, height, Vec::new()).0;
        assert!(
            node_names(&output)
                .iter()
                .any(|name| name == "Active model"),
            "the active drawer must explain why it does not offer activation"
        );
        assert!(
            !node_names(&output)
                .iter()
                .any(|name| name == "Use this model"),
            "only inactive ready models may expose the drawer activation action"
        );
        let has_disabled_remove = output
            .platform_output
            .accesskit_update
            .as_ref()
            .expect("details drawer exposes an AccessKit update")
            .nodes
            .iter()
            .any(|(_, node)| {
                node.name() == Some("Remove model from device")
                    && node.is_disabled()
                    && node.description()
                        == Some("Install another ready model before removing the selected model.")
            });
        assert!(
            has_disabled_remove,
            "missing disabled active-removal action; nodes={:?}",
            node_names(&output)
        );
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
                .any(|name| name == "whisper.cpp tiny.en")
        );
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
    fn remote_cards_use_unknown_ratings_without_extra_state_badges() {
        let names = node_names(&render(Fixture::ModelsInstalled, 1180.0, 815.0));

        assert!(!names.iter().any(|name| name == "Experimental"));
        assert!(names.iter().any(|name| name.contains("Speed: Not rated")));
        assert!(names.iter().any(|name| name == "Accuracy: Not rated"));
        assert!(!names.iter().any(|name| name == "Trusted publisher"));
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

    #[test]
    fn shared_route_shell_keeps_titles_and_docks_inside_all_reference_viewports() {
        for (width, height) in [(1476.0, 1018.0), (1180.0, 815.0), (960.0, 680.0)] {
            let titles = [
                (Fixture::TranscribeReady, "Transcribe"),
                (Fixture::ModelsInstalled, "Models"),
                (Fixture::SettingsRecording, "Settings"),
            ]
            .map(|(fixture, title)| {
                node_matching(&render(fixture, width, height), |node| {
                    node.role() == egui::accesskit::Role::Heading && node.name() == Some(title)
                })
                .bounds()
                .expect("route heading should expose bounds")
            });
            for title in titles {
                assert_within_tolerance(title.y0, 28.0, 6.0, "shared route title top inset");
            }

            for (fixture, expanded) in [
                (Fixture::ModelsInstalled, false),
                (Fixture::ModelsCompareExpanded, true),
            ] {
                let output = render(fixture, width, height);
                let surface = named_node_bounds(&output, "Model comparison surface");
                let header = named_node_bounds(
                    &output,
                    if expanded {
                        "Collapse comparison"
                    } else {
                        "Expand comparison"
                    },
                );
                assert_bounds_within(
                    surface,
                    egui::accesskit::Rect {
                        x0: 0.0,
                        y0: 0.0,
                        x1: width.into(),
                        y1: height.into(),
                    },
                    "comparison dock",
                );
                assert_bounds_within(header, surface, "fixed comparison header");
                assert_within_tolerance(
                    surface.y1,
                    f64::from(height - 24.0),
                    LAYOUT_TOLERANCE,
                    "comparison surface bottom gap",
                );
                for name in [
                    "Import local GGUF",
                    "Refresh trusted model catalog",
                    "Remove whisper.cpp base.en from device",
                    "Details for whisper.cpp base.en",
                ] {
                    let bounds = node_matching(&output, |node| {
                        node.role() == egui::accesskit::Role::Button && node.name() == Some(name)
                    })
                    .bounds()
                    .unwrap_or_else(|| panic!("Models action {name:?} should expose bounds"));
                    assert!(
                        bounds.x0 >= surface.x0 - LAYOUT_TOLERANCE
                            && bounds.x1 <= surface.x1 + LAYOUT_TOLERANCE,
                        "Models control {name:?} must stay within the shared route inset: {bounds:?} vs {surface:?}"
                    );
                }
                if expanded && width >= 1_476.0 {
                    let table = node_matching(&output, |node| {
                        node.role() == egui::accesskit::Role::Table
                            && node.name() == Some("Model comparison results")
                    })
                    .bounds()
                    .expect("wide comparison table should expose bounds");
                    assert_bounds_within(table, surface, "wide comparison table");
                    assert!(
                        surface.y1 - surface.y0 <= f64::from(height) * 0.6 + LAYOUT_TOLERANCE,
                        "expanded dock must remain below the 60% viewport cap"
                    );
                } else if expanded {
                    for model in ["whisper.cpp base.en", "whisper.cpp tiny.en"] {
                        let group = node_matching(&output, |node| {
                            node.role() == egui::accesskit::Role::Group
                                && node.name()
                                    == Some(format!("Comparison result for {model}").as_str())
                        })
                        .bounds()
                        .expect("compact comparison group should expose bounds");
                        assert!(
                            group.x0 >= surface.x0
                                && group.x1 <= surface.x1
                                && group.y0 < surface.y1
                                && group.y1 > surface.y0,
                            "compact comparison result group must remain horizontally contained and vertically intersect its scrollable surface: {group:?} vs {surface:?}"
                        );
                    }
                }
                if expanded {
                    let start = named_node_bounds(&output, "Start test recording");
                    assert_bounds_within(start, surface, "comparison recording action");
                    for (_, accuracy) in output
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
                    {
                        assert_bounds_within(
                            accuracy
                                .bounds()
                                .expect("accuracy action should expose bounds"),
                            surface,
                            "comparison result accuracy action",
                        );
                    }
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
    }

    #[test]
    fn models_max_scroll_keeps_the_final_model_card_clear_of_the_dock() {
        for fixture in [Fixture::ModelsInstalled, Fixture::ModelsCompareExpanded] {
            let (width, height) = (1180.0, 815.0);
            let ctx = egui::Context::default();
            ctx.enable_accesskit();
            configure_accessible_style(&ctx);
            let mut data = fixture.data();
            data.models.extend((0..24).map(|index| ModelViewModel {
                id: format!("available-{index:02}"),
                display_name: format!("Available model {index:02}"),
                variant_label: format!("available-{index:02}"),
                install_supported: true,
                install_action_enabled: true,
                language_summary: "English".into(),
                languages: vec!["English".into()],
                speed_tier: ModelSpeedTier::Balanced,
                size_tier: ModelSizeTier::Base,
                ..Default::default()
            }));
            let mut page = fixture.page();
            let _ = render_with_input_at_time(
                &ctx,
                &mut data,
                &mut page,
                width,
                height,
                Vec::new(),
                Some(0.0),
            );
            let (route_id, _, initial_content_size, initial_viewport) = ctx
                .data(|data| {
                    data.get_temp::<(egui::Id, egui::Vec2, egui::Vec2, egui::Rect)>(egui::Id::new(
                        "route-scroll-diagnostics",
                    ))
                })
                .expect("Models route should expose its scroll state in tests");
            let mut route_state = egui::scroll_area::State::load(&ctx, route_id)
                .expect("Models route scroll state should persist");
            route_state.offset.y = (initial_content_size.y - initial_viewport.height()).max(0.0);
            route_state.store(&ctx, route_id);

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
            let surface = named_node_bounds(&settled, "Model comparison surface");
            let final_entry = ctx
                .data(|data| data.get_temp::<egui::Rect>(egui::Id::new("models-final-card-rect")))
                .expect("model list should expose its final card rect in tests");
            let (_, offset, content_size, viewport) = ctx
                .data(|data| {
                    data.get_temp::<(egui::Id, egui::Vec2, egui::Vec2, egui::Rect)>(egui::Id::new(
                        "route-scroll-diagnostics",
                    ))
                })
                .expect("Models route should expose its settled scroll state");
            assert_within_tolerance(
                f64::from(content_size.y),
                f64::from(initial_content_size.y),
                1.0,
                "Models route content height across culling windows",
            );
            assert_within_tolerance(
                f64::from(viewport.height()),
                f64::from(initial_viewport.height()),
                1.0,
                "Models route viewport height across culling windows",
            );
            assert_within_tolerance(
                f64::from(offset.y),
                (content_size.y - viewport.height()).max(0.0).into(),
                LAYOUT_TOLERANCE,
                "Models maximum route offset",
            );
            let visible_entry_bottom = f64::from(final_entry.bottom());
            let layout = ctx
                .data(|data| {
                    data.get_temp::<(egui::Rect, egui::Rect, f32, f32)>(egui::Id::new(
                        "models-layout-diagnostics",
                    ))
                })
                .expect("Models layout diagnostics");
            let clearance = surface.y0 - visible_entry_bottom;
            assert!(
                clearance >= 24.0 - LAYOUT_TOLERANCE,
                "final model card needs at least 24 points of clearance above comparison dock: got {clearance}; final={final_entry:?}, surface={surface:?}, offset={offset:?}, content={content_size:?}, viewport={viewport:?}, layout={layout:?}",
            );
        }
    }

    #[test]
    #[ignore = "native AccessKit tab traversal stress test hangs on Windows; run manually after accessibility runtime changes"]
    fn model_culling_reaches_the_final_card_through_accessible_focus_and_paging() {
        let (width, height) = (1180.0, 815.0);
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        configure_accessible_style(&ctx);
        let mut data = Fixture::ModelsInstalled.data();
        data.comparison.expanded = true;
        data.remote_catalog.entries.clear();
        data.models.extend((0..36).map(|index| ModelViewModel {
            id: format!("available-{index:02}"),
            display_name: format!("Available model {index:02}"),
            variant_label: format!("available-{index:02}"),
            install_supported: true,
            install_action_enabled: true,
            language_summary: "English".into(),
            languages: vec!["English".into()],
            speed_tier: ModelSpeedTier::Balanced,
            size_tier: ModelSizeTier::Base,
            ..Default::default()
        }));
        let mut page = Fixture::ModelsInstalled.page();
        let first_name = "Install Available model 00";
        let final_name = "Install Available model 35";
        let expected_indices = (0..36).collect::<std::collections::BTreeSet<_>>();
        let collect_visible_indices =
            |output: &egui::FullOutput, visible: &mut std::collections::BTreeSet<usize>| {
                for name in node_names(output) {
                    let Some(rest) = name.strip_prefix("Install Available model ") else {
                        continue;
                    };
                    let Some(index) = rest
                        .split_whitespace()
                        .next()
                        .and_then(|value| value.parse::<usize>().ok())
                    else {
                        continue;
                    };
                    visible.insert(index);
                }
            };
        let model_index = |key: &ModelCardKey| match key {
            ModelCardKey::Local(id) => id
                .strip_prefix("available-")
                .and_then(|value| value.parse::<usize>().ok())
                .unwrap_or_else(|| panic!("unexpected local card key {id}")),
            ModelCardKey::Remote { .. } => panic!("expected a local available-model card key"),
        };
        let assert_focused_card_visible = |output: &egui::FullOutput| {
            let bounds = focused_node(output)
                .bounds()
                .expect("focused model card should expose bounds");
            let (_, _, _, viewport) = ctx
                .data(|data| {
                    data.get_temp::<(egui::Id, egui::Vec2, egui::Vec2, egui::Rect)>(egui::Id::new(
                        "route-scroll-diagnostics",
                    ))
                })
                .expect("Models route should expose its scroll viewport");
            let dock = ctx
                .data(|data| {
                    data.get_temp::<egui::Rect>(egui::Id::new("models-comparison-dock-rect"))
                })
                .expect("Models route should expose the expanded comparison dock");
            let unobscured_bottom = dock.top() - 24.0;
            assert!(
                bounds.x0 >= f64::from(viewport.left())
                    && bounds.x1 <= f64::from(viewport.right())
                    && bounds.y0 >= f64::from(viewport.top())
                    && bounds.y1 <= f64::from(unobscured_bottom),
                "acknowledged focused card must stay above the expanded dock: {bounds:?} vs viewport {viewport:?}, dock {dock:?}"
            );
        };

        let (initial, action) =
            render_with_input(&ctx, &mut data, &mut page, width, height, Vec::new());
        assert_eq!(action, ScreenAction::None);
        assert!(!node_names(&initial).iter().any(|name| name == final_name));
        let mut forward_visible = std::collections::BTreeSet::new();
        collect_visible_indices(&initial, &mut forward_visible);
        let available_header = named_node_id(&initial, "Collapse Available models");
        let (_, action) = render_with_input_and_apply(
            &ctx,
            &mut data,
            &mut page,
            width,
            height,
            vec![egui::Event::AccessKitActionRequest(
                egui::accesskit::ActionRequest {
                    action: egui::accesskit::Action::Focus,
                    target: available_header,
                    data: None,
                },
            )],
        );
        assert_eq!(action, ScreenAction::None);
        let (mut output, _) =
            render_with_input_and_apply(&ctx, &mut data, &mut page, width, height, Vec::new());
        collect_visible_indices(&output, &mut forward_visible);
        let mut forward_targets = Vec::new();
        let mut frame_time = 1.0;

        for _ in 0..64 {
            if focused_node(&output).name() == Some(final_name) {
                break;
            }
            let next = output
                .platform_output
                .accesskit_update
                .as_ref()
                .unwrap()
                .nodes
                .iter()
                .find_map(|(id, node)| {
                    (node.role() == egui::accesskit::Role::Button
                        && node.name() == Some("Show Next Available models")
                        && !node.is_disabled())
                    .then_some(*id)
                });
            if let Some(next) = next {
                let (_, action) = render_with_input_and_apply(
                    &ctx,
                    &mut data,
                    &mut page,
                    width,
                    height,
                    vec![egui::Event::AccessKitActionRequest(
                        egui::accesskit::ActionRequest {
                            action: egui::accesskit::Action::Default,
                            target: next,
                            data: None,
                        },
                    )],
                );
                let ScreenAction::FocusModelCard(target) = action else {
                    panic!("page sentinel should request an exact model card focus");
                };
                let target_index = model_index(&target);
                if let Some(previous) = forward_targets.last() {
                    assert!(
                        target_index > *previous,
                        "forward paging targets must advance monotonically: {forward_targets:?} then {target_index}"
                    );
                }
                forward_targets.push(target_index);
                let mut acknowledged = false;
                for _ in 0..4 {
                    let (focused, action) = render_with_input_and_apply_at_time(
                        &ctx,
                        &mut data,
                        &mut page,
                        width,
                        height,
                        Vec::new(),
                        frame_time,
                    );
                    frame_time += 0.1;
                    collect_visible_indices(&focused, &mut forward_visible);
                    match action {
                        ScreenAction::None => {
                            assert_eq!(
                                data.model_management.focus_model_card.as_ref(),
                                Some(&target),
                                "card focus must remain pending while its rect is clipped or offscreen"
                            );
                            output = focused;
                        }
                        ScreenAction::AcknowledgeModelCardFocus(acknowledged_target) => {
                            assert_eq!(acknowledged_target, target);
                            assert_focused_card_visible(&focused);
                            assert!(
                                data.model_management.focus_model_card.is_none(),
                                "acknowledgement must clear the completed focus request exactly once"
                            );
                            output = focused;
                            acknowledged = true;
                            break;
                        }
                        unexpected => panic!(
                            "pending card focus should emit only None or its acknowledgement, got {unexpected:?}"
                        ),
                    }
                }
                assert!(
                    acknowledged,
                    "card focus should settle within four empty-event frames; pending={:?}, focused={:?}, route={:?}, dock={:?}",
                    data.model_management.focus_model_card,
                    focused_node(&output).bounds(),
                    ctx.data(|data| data
                        .get_temp::<(egui::Id, egui::Vec2, egui::Vec2, egui::Rect)>(
                            egui::Id::new("route-scroll-diagnostics")
                        )),
                    ctx.data(|data| data
                        .get_temp::<egui::Rect>(egui::Id::new("models-comparison-dock-rect"))),
                );
            } else {
                let (focused, action) = render_with_input_and_apply(
                    &ctx,
                    &mut data,
                    &mut page,
                    width,
                    height,
                    vec![tab_event(false)],
                );
                assert_eq!(action, ScreenAction::None);
                output = focused;
                collect_visible_indices(&output, &mut forward_visible);
            }
        }

        assert_eq!(focused_node(&output).name(), Some(final_name));
        assert!(!node_names(&output).iter().any(|name| name == first_name));
        assert_eq!(
            forward_visible, expected_indices,
            "forward accessible paging must expose every available model without gaps"
        );

        let mut reverse_visible = std::collections::BTreeSet::new();
        collect_visible_indices(&output, &mut reverse_visible);
        let mut reverse_targets = Vec::new();
        for _ in 0..64 {
            if focused_node(&output).name() == Some(first_name) {
                break;
            }
            let (_, action) = render_with_input_and_apply(
                &ctx,
                &mut data,
                &mut page,
                width,
                height,
                vec![page_event(egui::Key::PageUp)],
            );
            let ScreenAction::FocusModelCard(target) = action else {
                panic!(
                    "PageUp from a rendered model primary should request exact previous-page focus"
                );
            };
            let target_index = model_index(&target);
            if let Some(previous) = reverse_targets.last() {
                assert!(
                    target_index < *previous,
                    "reverse paging targets must retreat monotonically: {reverse_targets:?} then {target_index}"
                );
            }
            reverse_targets.push(target_index);

            let mut acknowledged = false;
            for _ in 0..4 {
                let (focused, action) = render_with_input_and_apply_at_time(
                    &ctx,
                    &mut data,
                    &mut page,
                    width,
                    height,
                    Vec::new(),
                    frame_time,
                );
                frame_time += 0.1;
                collect_visible_indices(&focused, &mut reverse_visible);
                match action {
                    ScreenAction::None => {
                        assert_eq!(
                            data.model_management.focus_model_card.as_ref(),
                            Some(&target),
                            "reverse card focus must remain pending while its rect is clipped or offscreen"
                        );
                        output = focused;
                    }
                    ScreenAction::AcknowledgeModelCardFocus(acknowledged_target) => {
                        assert_eq!(acknowledged_target, target);
                        assert_focused_card_visible(&focused);
                        assert!(
                            data.model_management.focus_model_card.is_none(),
                            "reverse acknowledgement must clear the completed focus request exactly once"
                        );
                        output = focused;
                        acknowledged = true;
                        break;
                    }
                    unexpected => panic!(
                        "reverse pending card focus should emit only None or its acknowledgement, got {unexpected:?}"
                    ),
                }
            }
            assert!(
                acknowledged,
                "reverse card focus should settle within four empty-event frames; pending={:?}, focused={:?}, route={:?}, dock={:?}",
                data.model_management.focus_model_card,
                focused_node(&output).bounds(),
                ctx.data(
                    |data| data.get_temp::<(egui::Id, egui::Vec2, egui::Vec2, egui::Rect)>(
                        egui::Id::new("route-scroll-diagnostics")
                    )
                ),
                ctx.data(|data| data
                    .get_temp::<egui::Rect>(egui::Id::new("models-comparison-dock-rect"))),
            );
        }
        assert_eq!(focused_node(&output).name(), Some(first_name));
        assert_eq!(
            reverse_visible, expected_indices,
            "reverse accessible paging must expose every available model without gaps"
        );
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
        let target = named_node_id(&initial, "Enable local model Playground");
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
            Some("Enable local model Playground")
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
        let final_bounds = named_node_bounds(&settled, "Enable local model Playground");
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
    fn runtime_not_ready_state_stays_out_of_the_compact_row() {
        let (width, height) = (1180.0, 815.0);
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        configure_accessible_style(&ctx);
        let mut data = Fixture::ModelsInstalled.data();
        data.models[1].ready = false;
        let mut page = Fixture::ModelsInstalled.page();

        let output = render_with_input(&ctx, &mut data, &mut page, width, height, Vec::new()).0;
        assert!(
            !node_names(&output)
                .iter()
                .any(|name| name == "Runtime not ready"),
            "compact rows should reserve state details for the Details drawer"
        );
        assert!(
            node_names(&output)
                .iter()
                .any(|name| name == "Details for whisper.cpp tiny.en"),
            "the model remains inspectable through its Details control"
        );
    }
    #[test]
    fn settings_recording_fixture_contains_visible_live_meter_signal() {
        let data = Fixture::SettingsRecording.data();
        assert_eq!(data.settings.input_sensitivity_percent, 38);
        assert_eq!(data.settings.input_level_percent, 68);
        assert!(
            data.settings.input_level_percent > data.settings.input_sensitivity_percent,
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
    fn harness_parser_is_exact_and_fail_closed() {
        assert_eq!(
            Fixture::parse("transcribe/ready"),
            Some(Fixture::TranscribeReady)
        );
        assert_eq!(Fixture::parse("debug"), None);
    }
}
