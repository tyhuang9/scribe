use std::time::Duration;

use crate::transcription::SessionId;

/// User-facing overlay density. This deliberately does not depend on settings
/// schema types so the native overlay can be driven by any application shell.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum OverlayMode {
    #[default]
    Live,
    Minimal,
    Off,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum OverlayPhase {
    #[default]
    Hidden,
    Preparing,
    Listening,
    Finalizing,
    Processing,
    Pasting,
    Success,
    Error,
}

impl OverlayPhase {
    pub fn label(self) -> &'static str {
        match self {
            Self::Hidden => "Hidden",
            Self::Preparing => "Preparing",
            Self::Listening => "Listening",
            Self::Finalizing => "Finalizing",
            Self::Processing => "Processing",
            Self::Pasting => "Pasting",
            Self::Success => "Done",
            Self::Error => "Error",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct OverlayAudioLevel {
    pub rms: f32,
    pub peak: f32,
}

impl OverlayAudioLevel {
    pub fn new(rms: f32, peak: f32) -> Self {
        Self {
            rms: normalized_level(rms),
            peak: normalized_level(peak),
        }
    }
}

fn normalized_level(level: f32) -> f32 {
    if level.is_finite() {
        level.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct OverlayTranscript {
    pub committed: String,
    pub tentative: String,
    pub revision: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OverlayError {
    pub message: String,
    pub recoverable: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct OverlayViewState {
    pub session_id: Option<SessionId>,
    pub mode: OverlayMode,
    pub phase: OverlayPhase,
    pub audio_level: OverlayAudioLevel,
    pub transcript: OverlayTranscript,
    pub error: Option<OverlayError>,
    pub elapsed: Option<Duration>,
    pub reduced_motion: bool,
}

impl Default for OverlayViewState {
    fn default() -> Self {
        Self {
            session_id: None,
            mode: OverlayMode::Live,
            phase: OverlayPhase::Hidden,
            audio_level: OverlayAudioLevel::default(),
            transcript: OverlayTranscript::default(),
            error: None,
            elapsed: None,
            reduced_motion: false,
        }
    }
}

impl OverlayViewState {
    pub fn is_visible(&self) -> bool {
        self.mode != OverlayMode::Off && self.phase != OverlayPhase::Hidden
    }
}

/// Owns the display-only state for the overlay. It accepts already-produced
/// transcript and level data; it never manufactures partial transcripts or
/// calls a concrete speech runtime.
#[derive(Debug, Default)]
pub struct OverlayController {
    state: OverlayViewState,
    last_transcript_revision: Option<u64>,
}

impl OverlayController {
    pub fn new(reduced_motion: bool) -> Self {
        Self {
            state: OverlayViewState {
                reduced_motion,
                ..OverlayViewState::default()
            },
            last_transcript_revision: None,
        }
    }

    pub fn state(&self) -> &OverlayViewState {
        &self.state
    }

    pub fn begin_session(&mut self, session_id: SessionId, mode: OverlayMode) {
        let reduced_motion = self.state.reduced_motion;
        self.state = OverlayViewState {
            session_id: Some(session_id),
            mode,
            phase: OverlayPhase::Preparing,
            reduced_motion,
            ..OverlayViewState::default()
        };
        self.last_transcript_revision = None;
    }

    pub fn set_mode(&mut self, mode: OverlayMode) {
        self.state.mode = mode;
    }

    pub fn set_phase(&mut self, session_id: SessionId, phase: OverlayPhase) -> bool {
        if !self.is_current(session_id) {
            return false;
        }
        self.state.phase = phase;
        if phase != OverlayPhase::Error {
            self.state.error = None;
        }
        true
    }

    pub fn update_audio_level(&mut self, session_id: SessionId, rms: f32, peak: f32) -> bool {
        if !self.is_current(session_id) {
            return false;
        }
        self.state.audio_level = OverlayAudioLevel::new(rms, peak);
        true
    }

    pub fn update_transcript(
        &mut self,
        session_id: SessionId,
        committed: impl Into<String>,
        tentative: impl Into<String>,
        revision: u64,
    ) -> bool {
        if !self.is_current(session_id)
            || self
                .last_transcript_revision
                .is_some_and(|previous| revision <= previous)
        {
            return false;
        }

        self.state.transcript = OverlayTranscript {
            committed: committed.into(),
            tentative: tentative.into(),
            revision,
        };
        self.last_transcript_revision = Some(revision);
        true
    }

    /// Replaces every preview hypothesis with the authoritative full-pass
    /// result. Revision ownership stays inside the controller so the final
    /// update always supersedes any accepted partial sequence.
    pub fn replace_with_final(
        &mut self,
        session_id: SessionId,
        committed: impl Into<String>,
    ) -> bool {
        if !self.is_current(session_id) {
            return false;
        }
        let revision = self
            .last_transcript_revision
            .and_then(|previous| previous.checked_add(1))
            .unwrap_or(1);
        self.state.transcript = OverlayTranscript {
            committed: committed.into(),
            tentative: String::new(),
            revision,
        };
        self.last_transcript_revision = Some(revision);
        true
    }

    pub fn update_elapsed(&mut self, session_id: SessionId, elapsed: Duration) -> bool {
        if !self.is_current(session_id) {
            return false;
        }
        self.state.elapsed = Some(elapsed);
        true
    }

    pub fn show_error(
        &mut self,
        session_id: SessionId,
        message: impl Into<String>,
        recoverable: bool,
    ) -> bool {
        if !self.is_current(session_id) {
            return false;
        }
        self.state.phase = OverlayPhase::Error;
        self.state.error = Some(OverlayError {
            message: message.into(),
            recoverable,
        });
        true
    }

    pub fn hide(&mut self, session_id: SessionId) -> bool {
        if !self.is_current(session_id) {
            return false;
        }
        let mode = self.state.mode;
        let reduced_motion = self.state.reduced_motion;
        self.state = OverlayViewState {
            mode,
            reduced_motion,
            ..OverlayViewState::default()
        };
        self.last_transcript_revision = None;
        true
    }

    fn is_current(&self, session_id: SessionId) -> bool {
        self.state.session_id == Some(session_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn begin_session_has_no_fabricated_transcript() {
        let mut controller = OverlayController::new(false);

        controller.begin_session(SessionId(7), OverlayMode::Live);

        assert_eq!(controller.state().phase, OverlayPhase::Preparing);
        assert!(controller.state().transcript.committed.is_empty());
        assert!(controller.state().transcript.tentative.is_empty());
    }

    #[test]
    fn stale_session_and_revision_updates_are_ignored() {
        let mut controller = OverlayController::new(false);
        controller.begin_session(SessionId(7), OverlayMode::Live);

        assert!(!controller.update_transcript(SessionId(6), "old", "", 1));
        assert!(controller.update_transcript(SessionId(7), "hello", " wor", 2));
        assert!(!controller.update_transcript(SessionId(7), "regressed", "", 2));
        assert_eq!(controller.state().transcript.committed, "hello");
    }

    #[test]
    fn final_transcript_supersedes_any_preview_revision_and_clears_tentative() {
        let mut controller = OverlayController::new(false);
        controller.begin_session(SessionId(7), OverlayMode::Live);
        assert!(controller.update_transcript(SessionId(7), "hello", " wor", 41));

        assert!(controller.replace_with_final(SessionId(7), "Hello world."));
        assert_eq!(controller.state().transcript.committed, "Hello world.");
        assert!(controller.state().transcript.tentative.is_empty());
        assert_eq!(controller.state().transcript.revision, 42);
        assert!(!controller.replace_with_final(SessionId(6), "stale"));
    }

    #[test]
    fn audio_levels_are_finite_and_bounded() {
        let mut controller = OverlayController::new(false);
        controller.begin_session(SessionId(1), OverlayMode::Minimal);

        assert!(controller.update_audio_level(SessionId(1), f32::NAN, 4.0));

        assert_eq!(controller.state().audio_level.rms, 0.0);
        assert_eq!(controller.state().audio_level.peak, 1.0);
    }

    #[test]
    fn off_mode_never_becomes_visible() {
        let mut controller = OverlayController::new(false);
        controller.begin_session(SessionId(1), OverlayMode::Off);

        assert!(!controller.state().is_visible());
    }
}
