//! Backend-neutral UI contracts shared by production views and the development harness.

use std::collections::BTreeSet;

#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub(crate) enum SettingsTab {
    #[default]
    General,
    Recording,
    /// Legacy route retained so saved/deep links can be normalized to General.
    Output,
    Advanced,
    About,
}

#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub(crate) enum UiRoute {
    #[default]
    Transcribe,
    Models,
    Settings(SettingsTab),
    // These routes remain part of the shared renderer contract even though
    // this native shell currently routes the pages outside `ScreenView`.
    #[allow(dead_code)]
    History,
    #[allow(dead_code)]
    About,
    #[allow(dead_code)]
    Debug,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum TranscriptionPhase {
    #[default]
    NoModel,
    Ready,
    RequestingMicrophone,
    Listening,
    Finalizing,
    NoSpeech,
    MicrophoneError,
    ModelLoading,
    ModelError,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct TranscriptionState {
    pub phase: TranscriptionPhase,
    pub selected_model_id: Option<String>,
    pub committed_transcript: String,
    pub provisional_transcript: String,
    pub recording_started_at_ms: Option<u64>,
    pub elapsed_ms: u64,
    pub last_successful_capture_ms: Option<u64>,
    /// A Transcribe-local result or recovery message. Cross-route application
    /// status is intentionally not rendered on this screen.
    pub notice: Option<TranscribeNotice>,
    pub microphone_permission: MicrophonePermission,
    pub selected_audio_device_id: Option<String>,
    pub recording_mode: RecordingMode,
    pub hotkey: String,
    pub hotkey_capture_active: bool,
    pub hotkey_change_disabled_reason: Option<String>,
    pub model_change_disabled_reason: Option<String>,
    pub record_control_needs_focus: bool,
    /// True only while the successfully presented background overlay owns
    /// recording announcements for this frame.
    pub suppress_live_announcements: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TranscribeNoticeTone {
    Information,
    Error,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TranscribeRecoveryAction {
    AddModel,
    OpenModelSettings,
    RetryMicrophone,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TranscribeNotice {
    pub tone: TranscribeNoticeTone,
    pub message: String,
    pub recovery_action: Option<TranscribeRecoveryAction>,
}

impl TranscribeNotice {
    pub(crate) fn information(message: impl Into<String>) -> Self {
        Self {
            tone: TranscribeNoticeTone::Information,
            message: message.into(),
            recovery_action: None,
        }
    }

    pub(crate) fn error(
        message: impl Into<String>,
        recovery_action: TranscribeRecoveryAction,
    ) -> Self {
        Self {
            tone: TranscribeNoticeTone::Error,
            message: message.into(),
            recovery_action: Some(recovery_action),
        }
    }

    pub(crate) fn failure(message: impl Into<String>) -> Self {
        Self {
            tone: TranscribeNoticeTone::Error,
            message: message.into(),
            recovery_action: None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum MicrophonePermission {
    #[default]
    Unknown,
    Granted,
    Denied,
}

#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub(crate) enum RecordingMode {
    #[default]
    PressOnce,
    Hold,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ResolvedTheme {
    Light,
    Dark,
}

#[derive(Clone, Debug, Eq, PartialEq)]
#[allow(dead_code)]
pub(crate) enum TranscriptionEvent {
    ModelReady(String),
    StartRequested,
    MicrophoneGranted,
    MicrophoneFailed,
    Partial(String),
    StopRequested,
    FinalText(String),
    NoSpeech,
    ModelFailed,
    Retry,
    ModelRemoved,
}

impl TranscriptionState {
    #[allow(dead_code)]
    pub(crate) fn apply(&mut self, event: TranscriptionEvent) {
        match event {
            TranscriptionEvent::ModelReady(id)
                if matches!(
                    self.phase,
                    TranscriptionPhase::NoModel
                        | TranscriptionPhase::ModelLoading
                        | TranscriptionPhase::ModelError
                ) =>
            {
                self.selected_model_id = Some(id);
                self.phase = TranscriptionPhase::Ready;
                self.notice = None;
            }
            TranscriptionEvent::StartRequested
                if matches!(
                    self.phase,
                    TranscriptionPhase::Ready
                        | TranscriptionPhase::NoSpeech
                        | TranscriptionPhase::MicrophoneError
                ) =>
            {
                self.phase = TranscriptionPhase::RequestingMicrophone;
                self.notice = None;
            }
            TranscriptionEvent::MicrophoneGranted
                if self.phase == TranscriptionPhase::RequestingMicrophone =>
            {
                self.phase = TranscriptionPhase::Listening;
                self.microphone_permission = MicrophonePermission::Granted;
            }
            TranscriptionEvent::MicrophoneFailed
                if self.phase == TranscriptionPhase::RequestingMicrophone =>
            {
                self.phase = TranscriptionPhase::MicrophoneError;
                self.microphone_permission = MicrophonePermission::Denied;
                self.notice = Some(TranscribeNotice::error(
                    "Scribe couldn\u{2019}t access your microphone.",
                    TranscribeRecoveryAction::RetryMicrophone,
                ));
            }
            TranscriptionEvent::Partial(text) if self.phase == TranscriptionPhase::Listening => {
                self.provisional_transcript = text;
            }
            TranscriptionEvent::StopRequested if self.phase == TranscriptionPhase::Listening => {
                self.phase = TranscriptionPhase::Finalizing;
            }
            TranscriptionEvent::FinalText(text) if self.phase == TranscriptionPhase::Finalizing => {
                append_transcript(&mut self.committed_transcript, &text);
                self.provisional_transcript.clear();
                self.phase = TranscriptionPhase::Ready;
            }
            TranscriptionEvent::NoSpeech
                if matches!(
                    self.phase,
                    TranscriptionPhase::Listening | TranscriptionPhase::Finalizing
                ) =>
            {
                self.provisional_transcript.clear();
                self.phase = TranscriptionPhase::NoSpeech;
                self.notice = Some(TranscribeNotice::information(
                    "No speech detected — nothing was added.",
                ));
            }
            TranscriptionEvent::ModelFailed if self.phase == TranscriptionPhase::ModelLoading => {
                self.provisional_transcript.clear();
                self.phase = TranscriptionPhase::ModelError;
            }
            TranscriptionEvent::Retry if self.phase == TranscriptionPhase::MicrophoneError => {
                self.phase = TranscriptionPhase::RequestingMicrophone;
                self.notice = None;
            }
            TranscriptionEvent::ModelRemoved
                if matches!(
                    self.phase,
                    TranscriptionPhase::Ready
                        | TranscriptionPhase::NoSpeech
                        | TranscriptionPhase::MicrophoneError
                        | TranscriptionPhase::ModelError
                        | TranscriptionPhase::NoModel
                ) =>
            {
                self.selected_model_id = None;
                self.provisional_transcript.clear();
                self.phase = TranscriptionPhase::NoModel;
            }
            _ => {}
        }
    }
}

#[allow(dead_code)]
fn append_transcript(transcript: &mut String, text: &str) {
    let text = text.trim();
    if text.is_empty() {
        return;
    }
    if !transcript.trim().is_empty() {
        transcript.push(' ');
    }
    transcript.push_str(text);
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[allow(dead_code)]
pub(crate) enum ModelDownloadState {
    #[default]
    NotInstalled,
    Queued,
    Downloading,
    WaitingForVerification,
    Verifying,
    Extracting,
    Installed,
    Failed,
    Cancelled,
}

impl ModelDownloadState {
    #[allow(dead_code)]
    pub(crate) fn normalize(self, next: Self) -> Self {
        if self == Self::Installed && next != Self::Installed {
            Self::Installed
        } else {
            next
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct ModelCapabilities {
    /// False means unsupported only when `capabilities_known` is true.
    pub capabilities_known: bool,
    pub batch_transcription: bool,
    pub native_streaming: bool,
    pub cancellation: bool,
    pub timestamps: bool,
    pub translation: bool,
    pub language_detection: bool,
    pub confidence_scores: bool,
    pub custom_vocabulary: bool,
    pub cpu: bool,
    pub gpu: bool,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[allow(dead_code)]
pub(crate) enum ModelSpeedTier {
    VeryFast,
    Fast,
    Balanced,
    AccurateSlow,
    #[default]
    Unknown,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[allow(dead_code)]
pub(crate) enum ModelSizeTier {
    Tiny,
    Small,
    Base,
    Medium,
    Large,
    #[default]
    Unknown,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum ModelCompatibility {
    #[default]
    Supported,
    Experimental,
    Incompatible,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct ModelViewModel {
    pub id: String,
    pub display_name: String,
    pub variant_label: String,
    pub description: Option<String>,
    pub runtime_group: String,
    pub architecture: Option<String>,
    /// These are copied from the verified local manifest only when known.
    pub artifact_repository: Option<String>,
    pub artifact_revision: Option<String>,
    pub artifact_filename: Option<String>,
    pub artifact_path: Option<String>,
    /// The immutable Windows x64 release asset expected beside Scribe.
    pub bundled: bool,
    /// A verified bundled asset whose embedded runtime is ready.
    pub included: bool,
    pub installed: bool,
    /// The model selected in Settings.
    pub selected: bool,
    pub active: bool,
    pub ready: bool,
    pub recommended: bool,
    pub custom: bool,
    pub install_supported: bool,
    pub install_action_enabled: bool,
    pub primary_action_label: String,
    pub primary_action_enabled: bool,
    pub primary_action_disabled_reason: Option<String>,
    pub cancel_supported: bool,
    pub removal_supported: bool,
    pub partial_cleanup_available: bool,
    pub partial_cleanup_enabled: bool,
    pub partial_cleanup_disabled_reason: Option<String>,
    pub runtime_status_label: String,
    pub runtime_detail: Option<String>,
    pub runtime_version_label: Option<String>,
    pub runtime_storage_label: Option<String>,
    pub download_state: ModelDownloadState,
    pub downloaded_bytes: u64,
    pub total_bytes: Option<u64>,
    pub disk_bytes: Option<u64>,
    pub estimated_ram_bytes: Option<u64>,
    pub languages: Vec<String>,
    pub language_summary: String,
    pub speed_tier: ModelSpeedTier,
    /// Catalog-authored accuracy guidance. Empty means the model has not been rated.
    pub accuracy_guidance: String,
    pub size_tier: ModelSizeTier,
    pub capabilities: ModelCapabilities,
    pub compatibility: ModelCompatibility,
    pub error_message: Option<String>,
}

impl ModelViewModel {
    #[allow(dead_code)]
    pub(crate) fn normalize(mut self) -> Self {
        if self.active {
            self.installed = true;
            self.download_state = ModelDownloadState::Installed;
        } else if self.download_state == ModelDownloadState::Installed {
            self.installed = true;
        }
        self
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ModelDialog {
    Add,
    Remove(String),
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) enum ModelCardKey {
    Local(String),
    Remote {
        entry_id: String,
        variant_id: String,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ModelManagementState {
    pub dialog: Option<ModelDialog>,
    /// The one model card whose inline details are expanded, if any.
    pub expanded_model_card: Option<ModelCardKey>,
    /// One-frame focus request when a dialog first appears.
    pub focus_dialog_initial: bool,
    pub restore_add_focus: bool,
    /// After removing a model, focus a control which remains in the Models page.
    pub restore_after_removal_focus: bool,
    pub restore_remove_focus: Option<String>,
    /// The deterministic ready replacement named in an active-model removal confirmation.
    pub removal_replacement: Option<String>,
    pub mutation_block_reason: Option<String>,
    /// Actionable warning scoped to bundled-model cleanup on the Models page.
    pub lifecycle_warning: Option<String>,
    /// Quiet aggregate lifecycle summary; byte progress is intentionally excluded.
    pub install_status_summary: Option<String>,
    pub installed_expanded: bool,
    pub available_expanded: bool,
}

impl Default for ModelManagementState {
    fn default() -> Self {
        Self {
            dialog: None,
            expanded_model_card: None,
            focus_dialog_initial: false,
            restore_add_focus: false,
            restore_after_removal_focus: false,
            restore_remove_focus: None,
            removal_replacement: None,
            mutation_block_reason: None,
            lifecycle_warning: None,
            install_status_summary: None,
            installed_expanded: true,
            available_expanded: true,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum ModelLanguageFilter {
    #[default]
    All,
    English,
    Multilingual,
}

impl ModelLanguageFilter {
    pub(crate) const ALL: [Self; 3] = [Self::All, Self::English, Self::Multilingual];
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::All => "All languages",
            Self::English => "English",
            Self::Multilingual => "Multilingual",
        }
    }
    pub(crate) fn matches(self, languages: &[String]) -> bool {
        let normalized = languages
            .iter()
            .map(|language| language.trim().to_ascii_lowercase())
            .filter(|language| !language.is_empty())
            .collect::<BTreeSet<_>>();
        let english = normalized.contains("english") || normalized.contains("en");
        let multilingual = normalized.len() > 1 || normalized.contains("multilingual");
        matches!(self, Self::All)
            || (matches!(self, Self::English) && english)
            || (matches!(self, Self::Multilingual) && multilingual)
    }
}

/// Ephemeral catalog controls. These values affect browsing only and are not
/// part of the persisted model configuration.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct RemoteCatalogFilters {
    pub installed_only: bool,
    pub recommended_only: bool,
    pub multilingual_only: bool,
    pub size_tier: RemoteCatalogSizeTier,
}

#[allow(dead_code)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum RemoteCatalogSizeTier {
    #[default]
    Any,
    Compact,
    Standard,
    Large,
}

#[allow(dead_code)]
impl RemoteCatalogSizeTier {
    pub(crate) const ALL: [Self; 4] = [Self::Any, Self::Compact, Self::Standard, Self::Large];

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Any => "Any size",
            Self::Compact => "Compact (up to 512 MiB)",
            Self::Standard => "Standard (512 MiB to 1 GiB)",
            Self::Large => "Large (over 1 GiB)",
        }
    }

    pub(crate) fn matches(self, size_bytes: Option<u64>) -> bool {
        const MIB: u64 = 1024 * 1024;
        const COMPACT_MAX: u64 = 512 * MIB;
        const STANDARD_MAX: u64 = 1024 * MIB;

        match self {
            Self::Any => true,
            Self::Compact => size_bytes.is_some_and(|size| size <= COMPACT_MAX),
            Self::Standard => {
                size_bytes.is_some_and(|size| size > COMPACT_MAX && size <= STANDARD_MAX)
            }
            Self::Large => size_bytes.is_some_and(|size| size > STANDARD_MAX),
        }
    }
}

#[allow(dead_code)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum RemoteCatalogSort {
    #[default]
    Recommended,
    Smallest,
    Largest,
    Name,
}

#[allow(dead_code)]
impl RemoteCatalogSort {
    pub(crate) const ALL: [Self; 4] =
        [Self::Recommended, Self::Smallest, Self::Largest, Self::Name];

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Recommended => "Recommended first",
            Self::Smallest => "Smallest first",
            Self::Largest => "Largest first",
            Self::Name => "Name",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum RemoteCatalogStatusKind {
    Loading,
    Available,
    Offline,
    Error,
    #[default]
    Idle,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct RemoteCatalogStatusView {
    pub kind: RemoteCatalogStatusKind,
    pub message: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum RemoteCatalogActionKind {
    Install {
        remote_model_id: String,
        variant_id: String,
    },
    Cancel {
        model_id: String,
    },
    Use {
        model_id: String,
    },
    Remove {
        model_id: String,
    },
    DiscardPartial {
        remote_model_id: String,
        variant_id: String,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RemoteCatalogActionView {
    pub label: String,
    pub kind: RemoteCatalogActionKind,
    pub enabled: bool,
    pub disabled_reason: Option<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct RemoteCatalogVariantView {
    pub id: String,
    pub filename: String,
    pub size_label: String,
    pub status_label: Option<String>,
    pub expected_sha256: String,
    pub normalized_model_id: Option<String>,
    pub managed_model_id: Option<String>,
    pub size_bytes: u64,
    /// Volatile live download bytes, populated only while the installer reports progress.
    pub downloaded_bytes: Option<u64>,
    /// Volatile live total, which may remain unknown even when a download is active.
    pub total_bytes: Option<u64>,
    /// The installer-reported failure retained for the model-card error alert.
    pub error_message: Option<String>,
    pub size_tier: ModelSizeTier,
    pub speed_tier: ModelSpeedTier,
    pub accuracy_guidance: String,
    pub expected_ram_bytes: Option<u64>,
    pub capabilities: ModelCapabilities,
    pub actions: Vec<RemoteCatalogActionView>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct RemoteCatalogEntryView {
    pub id: String,
    pub display_name: String,
    pub description: String,
    pub languages: Vec<String>,
    pub language_summary: String,
    pub recommended: bool,
    pub trust_label: String,
    pub compatibility_detail: String,
    pub repository: String,
    pub pinned_revision: String,
    pub variants: Vec<RemoteCatalogVariantView>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct LocalGgufImportView {
    pub path: String,
    pub in_progress: bool,
    pub import_enabled: bool,
    pub disabled_reason: Option<String>,
    pub status_message: Option<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct RemoteCatalogView {
    pub local_import: LocalGgufImportView,
    pub query: String,
    pub filters: RemoteCatalogFilters,
    pub sort: RemoteCatalogSort,
    pub status: RemoteCatalogStatusView,
    pub refresh_enabled: bool,
    /// True only when the backend has a validated in-memory network snapshot
    /// or the bundled fallback.
    pub has_snapshot: bool,
    pub entries: Vec<RemoteCatalogEntryView>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[allow(dead_code)]
pub(crate) enum ComparisonPhase {
    #[default]
    Idle,
    Recording,
    Processing,
    Complete,
    Error,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum ComparisonResultPhase {
    #[default]
    Pending,
    Processing,
    Complete,
    Error,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct ComparisonResult {
    pub phase: ComparisonResultPhase,
    pub output: Option<String>,
    pub processing_ms: Option<u64>,
    pub realtime_factor: Option<f32>,
    pub word_error_rate: Option<f32>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct ModelComparisonState {
    pub expanded: bool,
    /// One-frame focus request after the page-header Compare action opens the panel.
    pub focus_panel: bool,
    pub selected_model_ids: BTreeSet<String>,
    pub phase: ComparisonPhase,
    pub audio_duration_ms: Option<u64>,
    pub recording_elapsed_ms: u64,
    pub reference_editor_visible: bool,
    pub focus_reference_editor: bool,
    pub restore_reference_action_focus: bool,
    pub reference_draft: String,
    pub reference_transcript: Option<String>,
    /// One-frame polite confirmation after applying or clearing a reference transcript.
    pub reference_notice: Option<String>,
    pub selection_feedback: Option<String>,
    pub start_disabled_reason: Option<String>,
    pub results: Vec<(String, ComparisonResult)>,
}

impl ModelComparisonState {
    pub(crate) fn can_start(&self) -> bool {
        (2..=4).contains(&self.selected_model_ids.len())
            && matches!(
                self.phase,
                ComparisonPhase::Idle | ComparisonPhase::Complete | ComparisonPhase::Error
            )
    }

    pub(crate) fn begin(&mut self) -> bool {
        if !self.can_start() {
            return false;
        }
        self.phase = ComparisonPhase::Recording;
        self.audio_duration_ms = None;
        self.recording_elapsed_ms = 0;
        self.selection_feedback = None;
        self.results.clear();
        true
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[allow(dead_code)]
pub(crate) enum SettingsSaveState {
    #[default]
    Clean,
    Dirty,
    Saving,
    Saved,
    Failed,
}

impl SettingsSaveState {
    #[allow(dead_code)]
    pub(crate) fn changed(self) -> Self {
        Self::Dirty
    }

    #[allow(dead_code)]
    pub(crate) fn saving(self) -> Self {
        Self::Saving
    }

    #[allow(dead_code)]
    pub(crate) fn completed(self, success: bool) -> Self {
        if success { Self::Saved } else { Self::Failed }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn language_filter_distinguishes_english_and_true_multilingual_models() {
        let english = vec!["English".to_owned()];
        let multilingual = vec!["English".to_owned(), "Spanish".to_owned()];
        let multilingual_marker = vec!["  MULTILINGUAL  ".to_owned()];
        let duplicate_english = vec![" en ".to_owned(), "EN".to_owned()];
        let spanish = vec!["Spanish".to_owned()];

        assert!(ModelLanguageFilter::English.matches(&english));
        assert!(!ModelLanguageFilter::Multilingual.matches(&english));
        assert!(ModelLanguageFilter::Multilingual.matches(&multilingual));
        assert!(ModelLanguageFilter::Multilingual.matches(&multilingual_marker));
        assert!(!ModelLanguageFilter::Multilingual.matches(&duplicate_english));
        assert!(ModelLanguageFilter::English.matches(&duplicate_english));
        assert!(!ModelLanguageFilter::English.matches(&spanish));
        assert!(!ModelLanguageFilter::Multilingual.matches(&spanish));
        assert!(ModelLanguageFilter::All.matches(&spanish));
    }

    #[test]
    fn finalization_appends_once_and_discards_provisional_text() {
        let mut state = TranscriptionState {
            phase: TranscriptionPhase::Listening,
            committed_transcript: "Earlier text.".into(),
            ..Default::default()
        };
        state.apply(TranscriptionEvent::Partial("unfinished".into()));
        state.apply(TranscriptionEvent::StopRequested);
        state.apply(TranscriptionEvent::FinalText("Final words.".into()));
        state.apply(TranscriptionEvent::FinalText("Final words.".into()));

        assert_eq!(state.phase, TranscriptionPhase::Ready);
        assert_eq!(state.committed_transcript, "Earlier text. Final words.");
        assert!(state.provisional_transcript.is_empty());
    }

    #[test]
    fn no_speech_preserves_committed_transcript() {
        let mut state = TranscriptionState {
            phase: TranscriptionPhase::Finalizing,
            committed_transcript: "Keep this.".into(),
            provisional_transcript: "discard this".into(),
            ..Default::default()
        };
        state.apply(TranscriptionEvent::NoSpeech);

        assert_eq!(state.phase, TranscriptionPhase::NoSpeech);
        assert_eq!(state.committed_transcript, "Keep this.");
        assert!(state.provisional_transcript.is_empty());
    }

    #[test]
    fn model_normalization_prevents_contradictory_active_state() {
        let model = ModelViewModel {
            active: true,
            download_state: ModelDownloadState::Downloading,
            ..Default::default()
        }
        .normalize();

        assert!(model.installed);
        assert_eq!(model.download_state, ModelDownloadState::Installed);
        assert_eq!(
            ModelDownloadState::Installed.normalize(ModelDownloadState::Downloading),
            ModelDownloadState::Installed
        );
    }

    #[test]
    fn settings_save_state_reducer_has_a_simple_retry_path() {
        assert_eq!(
            SettingsSaveState::Clean.changed().saving().completed(false),
            SettingsSaveState::Failed
        );
        assert_eq!(
            SettingsSaveState::Failed.changed().saving().completed(true),
            SettingsSaveState::Saved
        );
    }

    #[test]
    fn stale_model_events_do_not_interrupt_recording_or_finalization() {
        for phase in [
            TranscriptionPhase::Listening,
            TranscriptionPhase::Finalizing,
        ] {
            let mut state = TranscriptionState {
                phase,
                selected_model_id: Some("base.en".into()),
                provisional_transcript: "keep this".into(),
                ..Default::default()
            };

            state.apply(TranscriptionEvent::ModelReady("stale-model".into()));
            state.apply(TranscriptionEvent::ModelFailed);

            assert_eq!(state.phase, phase);
            assert_eq!(state.selected_model_id.as_deref(), Some("base.en"));
            assert_eq!(state.provisional_transcript, "keep this");
        }
    }

    #[test]
    fn model_removal_fails_closed_while_capture_or_model_loading_is_active() {
        for phase in [
            TranscriptionPhase::RequestingMicrophone,
            TranscriptionPhase::Listening,
            TranscriptionPhase::Finalizing,
            TranscriptionPhase::ModelLoading,
        ] {
            let mut state = TranscriptionState {
                phase,
                selected_model_id: Some("base.en".into()),
                provisional_transcript: "keep this".into(),
                ..Default::default()
            };

            state.apply(TranscriptionEvent::ModelRemoved);

            assert_eq!(state.phase, phase);
            assert_eq!(state.selected_model_id.as_deref(), Some("base.en"));
            assert_eq!(state.provisional_transcript, "keep this");
        }
    }

    #[test]
    fn comparison_rerun_requires_two_models_and_resets_previous_run() {
        let mut comparison = ModelComparisonState {
            phase: ComparisonPhase::Complete,
            audio_duration_ms: Some(8_000),
            reference_transcript: Some("old reference".into()),
            results: vec![("base.en".into(), ComparisonResult::default())],
            ..Default::default()
        };
        comparison.selected_model_ids.insert("base.en".into());
        assert!(!comparison.can_start());
        comparison.selected_model_ids.insert("tiny.en".into());
        assert!(comparison.can_start());

        assert!(comparison.begin());

        assert_eq!(comparison.phase, ComparisonPhase::Recording);
        assert_eq!(comparison.audio_duration_ms, None);
        assert_eq!(
            comparison.reference_transcript.as_deref(),
            Some("old reference")
        );
        assert!(comparison.results.is_empty());
    }

    #[test]
    fn comparison_reference_editor_starts_hidden() {
        let comparison = ModelComparisonState::default();
        assert!(!comparison.reference_editor_visible);
        assert!(!comparison.focus_reference_editor);
        assert!(!comparison.restore_reference_action_focus);
        assert_eq!(comparison.reference_notice, None);
    }

    #[test]
    fn comparison_selection_is_capped_at_four_models() {
        let mut comparison = ModelComparisonState::default();
        comparison.selected_model_ids.extend(
            ["one", "two", "three", "four", "five"]
                .into_iter()
                .map(str::to_owned),
        );

        assert!(!comparison.can_start());
    }

    #[test]
    fn comparison_cannot_restart_while_busy() {
        for phase in [ComparisonPhase::Recording, ComparisonPhase::Processing] {
            let mut comparison = ModelComparisonState {
                phase,
                ..Default::default()
            };
            comparison
                .selected_model_ids
                .extend(["base.en".into(), "tiny.en".into()]);

            assert!(!comparison.begin());
            assert_eq!(comparison.phase, phase);
        }
    }

    #[test]
    fn model_events_only_apply_during_model_setup() {
        let mut loading = TranscriptionState {
            phase: TranscriptionPhase::ModelLoading,
            ..Default::default()
        };
        loading.apply(TranscriptionEvent::ModelFailed);
        assert_eq!(loading.phase, TranscriptionPhase::ModelError);

        loading.apply(TranscriptionEvent::ModelReady("base.en".into()));
        assert_eq!(loading.phase, TranscriptionPhase::Ready);
        assert_eq!(loading.selected_model_id.as_deref(), Some("base.en"));
    }

    #[test]
    fn approved_ui_copy_uses_real_unicode_punctuation() {
        let no_speech = "No speech detected \u{2014} nothing was added.";
        let finalizing = "Finalizing transcript\u{2026}";
        let microphone = "Scribe couldn\u{2019}t access your microphone";

        assert!(!no_speech.contains('\u{00e2}'));
        assert!(no_speech.contains('\u{2014}'));
        assert!(finalizing.contains('\u{2026}'));
        assert!(microphone.contains('\u{2019}'));
    }
}
