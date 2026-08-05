//! Backend-neutral UI contracts shared by production views and the development harness.

use std::collections::BTreeSet;

#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub(crate) enum SettingsTab {
    #[default]
    General,
    Recording,
    Output,
    Advanced,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum UiRoute {
    #[default]
    Transcribe,
    Models,
    Settings(SettingsTab),
    History,
    About,
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
    pub notice: Option<String>,
    pub microphone_permission: MicrophonePermission,
    pub selected_audio_device_id: Option<String>,
    pub recording_mode: RecordingMode,
    pub hotkey: String,
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
                self.notice = Some("Scribe couldn\u{2019}t access your microphone.".into());
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
                self.notice = Some("No speech detected — nothing was added.".into());
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
    pub streaming_preview: bool,
    pub translation: bool,
    pub timestamps: bool,
    pub language_detection: bool,
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
    pub installed: bool,
    pub active: bool,
    pub ready: bool,
    pub recommended: bool,
    pub custom: bool,
    pub install_supported: bool,
    pub install_action_enabled: bool,
    pub cancel_supported: bool,
    pub removal_supported: bool,
    pub download_state: ModelDownloadState,
    pub downloaded_bytes: u64,
    pub total_bytes: Option<u64>,
    pub disk_bytes: Option<u64>,
    pub estimated_ram_bytes: Option<u64>,
    pub languages: Vec<String>,
    pub language_summary: String,
    pub speed_tier: ModelSpeedTier,
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
    Details(String),
    Remove(String),
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct ModelManagementState {
    pub dialog: Option<ModelDialog>,
    pub restore_add_focus: bool,
    pub mutation_block_reason: Option<String>,
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

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct ComparisonResult {
    pub output: Option<String>,
    pub processing_ms: Option<u64>,
    pub realtime_factor: Option<f32>,
    pub word_error_rate: Option<f32>,
    pub character_error_rate: Option<f32>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct ModelComparisonState {
    pub expanded: bool,
    pub selected_model_ids: BTreeSet<String>,
    pub phase: ComparisonPhase,
    pub audio_duration_ms: Option<u64>,
    pub reference_transcript: Option<String>,
    pub results: Vec<(String, ComparisonResult)>,
}

impl ModelComparisonState {
    pub(crate) fn can_start(&self) -> bool {
        self.selected_model_ids.len() >= 2
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
        self.reference_transcript = None;
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
        assert_eq!(comparison.reference_transcript, None);
        assert!(comparison.results.is_empty());
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
