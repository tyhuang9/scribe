#![allow(dead_code)]

use crate::models::{RecordingStatus, TranscriptResult, TranscriptionStatus};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InsertionStatus {
    NotRequested,
    Inserted,
    CopiedOnly,
    Failed(String),
}

#[derive(Clone, Debug)]
pub struct CoreState {
    pub recording_status: RecordingStatus,
    pub transcription_status: TranscriptionStatus,
    pub transcript: Option<TranscriptResult>,
    pub insertion_status: InsertionStatus,
    pub last_error: Option<String>,
}

impl Default for CoreState {
    fn default() -> Self {
        Self {
            recording_status: RecordingStatus::Idle,
            transcription_status: TranscriptionStatus::Idle,
            transcript: None,
            insertion_status: InsertionStatus::NotRequested,
            last_error: None,
        }
    }
}

pub enum CoreEvent {
    RecordingStarted,
    RecordingFailed(String),
    RecordingFinished,
    TranscriptionStarted,
    TranscriptionSucceeded(TranscriptResult),
    TranscriptionFailed(String),
    InsertionSucceeded,
    InsertionCopiedOnly,
    InsertionFailed(String),
    ClearTranscript,
}

pub fn reduce(state: &mut CoreState, event: CoreEvent) {
    match event {
        CoreEvent::RecordingStarted => {
            state.recording_status = RecordingStatus::Recording;
            state.transcription_status = TranscriptionStatus::Listening;
            state.last_error = None;
        }
        CoreEvent::RecordingFailed(message) => {
            state.recording_status = RecordingStatus::Error;
            state.transcription_status = TranscriptionStatus::Error;
            state.last_error = Some(message);
        }
        CoreEvent::RecordingFinished => {
            state.recording_status = RecordingStatus::Finalizing;
            state.transcription_status = TranscriptionStatus::Finalizing;
        }
        CoreEvent::TranscriptionStarted => {
            state.recording_status = RecordingStatus::Idle;
            state.transcription_status = TranscriptionStatus::Transcribing;
            state.last_error = None;
        }
        CoreEvent::TranscriptionSucceeded(result) => {
            state.recording_status = RecordingStatus::Idle;
            state.transcription_status = TranscriptionStatus::Idle;
            state.transcript = Some(result);
            state.last_error = None;
        }
        CoreEvent::TranscriptionFailed(message) => {
            state.recording_status = RecordingStatus::Idle;
            state.transcription_status = TranscriptionStatus::Error;
            state.last_error = Some(message);
        }
        CoreEvent::InsertionSucceeded => {
            state.insertion_status = InsertionStatus::Inserted;
        }
        CoreEvent::InsertionCopiedOnly => {
            state.insertion_status = InsertionStatus::CopiedOnly;
        }
        CoreEvent::InsertionFailed(message) => {
            state.insertion_status = InsertionStatus::Failed(message);
        }
        CoreEvent::ClearTranscript => {
            state.transcript = None;
            state.insertion_status = InsertionStatus::NotRequested;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::TranscriptSegment;

    #[test]
    fn reducer_tracks_successful_recording_transcription_and_insertion() {
        let mut state = CoreState::default();
        reduce(&mut state, CoreEvent::RecordingStarted);
        assert_eq!(state.recording_status, RecordingStatus::Recording);
        assert_eq!(state.transcription_status, TranscriptionStatus::Listening);

        reduce(&mut state, CoreEvent::RecordingFinished);
        assert_eq!(state.recording_status, RecordingStatus::Finalizing);
        assert_eq!(state.transcription_status, TranscriptionStatus::Finalizing);

        reduce(
            &mut state,
            CoreEvent::TranscriptionSucceeded(fake_transcript("hello")),
        );
        reduce(&mut state, CoreEvent::InsertionSucceeded);

        assert_eq!(state.recording_status, RecordingStatus::Idle);
        assert_eq!(state.transcription_status, TranscriptionStatus::Idle);
        assert_eq!(state.transcript.as_ref().unwrap().text, "hello");
        assert_eq!(state.insertion_status, InsertionStatus::Inserted);
    }

    #[test]
    fn reducer_preserves_transcript_when_insertion_fails() {
        let mut state = CoreState::default();
        reduce(
            &mut state,
            CoreEvent::TranscriptionSucceeded(fake_transcript("visible text")),
        );
        reduce(
            &mut state,
            CoreEvent::InsertionFailed("paste automation failed".to_owned()),
        );

        assert_eq!(state.transcript.as_ref().unwrap().text, "visible text");
        assert_eq!(
            state.insertion_status,
            InsertionStatus::Failed("paste automation failed".to_owned())
        );
    }

    fn fake_transcript(text: &str) -> TranscriptResult {
        TranscriptResult {
            model_id: "whisper_cpp_base_en".to_owned(),
            model_name: "whisper.cpp base.en".to_owned(),
            backend: "whisper.cpp".to_owned(),
            text: text.to_owned(),
            segments: vec![TranscriptSegment {
                start_ms: None,
                end_ms: None,
                text: text.to_owned(),
            }],
            duration_ms: Some(12),
            stdout: text.to_owned(),
            stderr: String::new(),
        }
    }
}
