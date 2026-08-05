//! Thin mappings between the live application runtime and backend-neutral screens.

use crate::models::TranscriptionStatus;

use super::state::{
    MicrophonePermission, RecordingMode, SettingsSaveState, TranscriptionPhase, TranscriptionState,
};

pub(crate) fn transcription_state(
    status: TranscriptionStatus,
    selected_model_id: Option<String>,
    requesting_microphone: bool,
    no_speech: bool,
    elapsed_ms: u64,
    transcript: String,
    provisional_transcript: String,
    notice: Option<String>,
    hotkey: String,
    recording_mode: RecordingMode,
    microphone_permission: MicrophonePermission,
) -> TranscriptionState {
    let has_selected_model = selected_model_id.is_some();
    let phase = match (status, has_selected_model, requesting_microphone, no_speech) {
        (_, false, false, _) => TranscriptionPhase::NoModel,
        (_, _, true, _) => TranscriptionPhase::RequestingMicrophone,
        (_, _, false, true) => TranscriptionPhase::NoSpeech,
        (TranscriptionStatus::Listening, _, _, _) => TranscriptionPhase::Listening,
        (TranscriptionStatus::Transcribing, _, _, _) => TranscriptionPhase::Finalizing,
        (TranscriptionStatus::Error, _, _, _)
            if microphone_permission == MicrophonePermission::Denied =>
        {
            TranscriptionPhase::MicrophoneError
        }
        (TranscriptionStatus::Error, _, _, _) => TranscriptionPhase::ModelError,
        _ => TranscriptionPhase::Ready,
    };

    TranscriptionState {
        phase,
        selected_model_id,
        committed_transcript: transcript,
        provisional_transcript,
        elapsed_ms,
        notice,
        microphone_permission,
        recording_mode,
        hotkey,
        ..Default::default()
    }
}

pub(crate) fn recording_mode(hold_to_talk: bool) -> RecordingMode {
    if hold_to_talk {
        RecordingMode::Hold
    } else {
        RecordingMode::PressOnce
    }
}

pub(crate) fn settings_save_state(
    persistence_pending: bool,
    last_error: bool,
) -> SettingsSaveState {
    if last_error {
        SettingsSaveState::Failed
    } else if persistence_pending {
        SettingsSaveState::Saving
    } else {
        SettingsSaveState::Clean
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_live_capture_and_failure_phases_without_losing_transcript() {
        let listening = transcription_state(
            TranscriptionStatus::Listening,
            Some("base.en".into()),
            false,
            false,
            1_000,
            "Saved text".into(),
            "Partial".into(),
            None,
            "Ctrl+Shift+Space".into(),
            RecordingMode::PressOnce,
            MicrophonePermission::Granted,
        );
        assert_eq!(listening.phase, TranscriptionPhase::Listening);
        assert_eq!(listening.committed_transcript, "Saved text");

        let denied = transcription_state(
            TranscriptionStatus::Error,
            Some("base.en".into()),
            false,
            false,
            0,
            "Saved text".into(),
            String::new(),
            Some("Scribe couldn\u{2019}t access your microphone".into()),
            "Ctrl+Shift+Space".into(),
            RecordingMode::PressOnce,
            MicrophonePermission::Denied,
        );
        assert_eq!(denied.phase, TranscriptionPhase::MicrophoneError);
        assert_eq!(denied.committed_transcript, "Saved text");
    }

    #[test]
    fn maps_persistence_and_recording_preferences() {
        assert_eq!(recording_mode(false), RecordingMode::PressOnce);
        assert_eq!(recording_mode(true), RecordingMode::Hold);
        assert_eq!(settings_save_state(true, false), SettingsSaveState::Saving);
        assert_eq!(settings_save_state(false, true), SettingsSaveState::Failed);
    }
}
