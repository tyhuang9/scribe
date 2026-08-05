use std::collections::HashMap;

use thiserror::Error;

use crate::transcription::{ModelId, RequestId, SessionId};

/// Runtime-neutral reason a user session exists.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionPurpose {
    Dictation,
    Comparison,
}

/// Authoritative active-session phase. Terminal outcomes are retained
/// separately while the coordinator returns to `Idle` immediately.
#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd)]
pub enum DictationPhase {
    #[default]
    Idle,
    StartingCapture,
    Capturing,
    FinalizingCapture,
    Transcribing,
    Output,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum StopReason {
    Endpoint,
    MaximumDuration,
    Explicit,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TerminalOutcome {
    Completed,
    Cancelled,
    Failed,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TerminalSession {
    pub session_id: SessionId,
    pub purpose: SessionPurpose,
    pub outcome: TerminalOutcome,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ModelLoadState {
    NotStarted,
    Loading { model_id: ModelId },
    Ready { model_id: ModelId },
    Failed { model_id: ModelId },
}

#[derive(Clone, Debug)]
struct RequestState {
    model_id: ModelId,
    completed: bool,
    failed: bool,
}

#[derive(Clone, Debug)]
struct PreviewState {
    request_id: RequestId,
    model_id: ModelId,
    last_sequence: Option<u64>,
}

#[derive(Clone, Debug)]
pub struct ActiveSession {
    session_id: SessionId,
    purpose: SessionPurpose,
    phase: DictationPhase,
    stop_reason: Option<StopReason>,
    model_load: ModelLoadState,
    preview: Option<PreviewState>,
    requests: HashMap<RequestId, RequestState>,
}

impl ActiveSession {
    pub fn id(&self) -> SessionId {
        self.session_id
    }

    pub fn purpose(&self) -> SessionPurpose {
        self.purpose
    }

    pub fn phase(&self) -> DictationPhase {
        self.phase
    }

    pub fn stop_reason(&self) -> Option<StopReason> {
        self.stop_reason
    }

    #[cfg(test)]
    pub fn model_load(&self) -> &ModelLoadState {
        &self.model_load
    }

    #[cfg(test)]
    pub fn pending_request_count(&self) -> usize {
        self.requests
            .values()
            .filter(|request| !request.completed)
            .count()
    }
}

#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum CoordinatorError {
    #[error("another dictation session is already active")]
    Busy,
    #[error("session identifier space is exhausted")]
    SessionIdExhausted,
    #[error("request identifier space is exhausted")]
    RequestIdExhausted,
    #[error("event belongs to stale session {0:?}")]
    StaleSession(SessionId),
    #[error("illegal transition from {from:?} to {to:?}")]
    IllegalTransition {
        from: DictationPhase,
        to: DictationPhase,
    },
    #[error("request {0:?} is unknown for the active session")]
    UnknownRequest(RequestId),
    #[error("request {0:?} has already completed")]
    DuplicateCompletion(RequestId),
    #[error("a rolling preview request is already active")]
    PreviewAlreadyActive,
    #[error("the rolling preview request must finish before capture finalization")]
    PreviewStillActive,
    #[error("request {request_id:?} expected model {expected}, got {actual}")]
    WrongModel {
        request_id: RequestId,
        expected: ModelId,
        actual: ModelId,
    },
    #[error("update sequence {actual} is not newer than {previous}")]
    StaleSequence { previous: u64, actual: u64 },
    #[error("model-load event expected {expected}, got {actual}")]
    WrongPreloadModel { expected: ModelId, actual: ModelId },
    #[error("session still has pending transcription requests")]
    PendingRequests,
    #[error("session contains a failed transcription request")]
    FailedRequests,
}

/// Owns all correlation and legal-transition decisions for the one active
/// user session. Concrete runtimes never appear at this boundary.
#[derive(Debug)]
pub struct SessionCoordinator {
    next_session_id: u64,
    next_request_id: u64,
    active: Option<ActiveSession>,
    last_terminal: Option<TerminalSession>,
}

impl Default for SessionCoordinator {
    fn default() -> Self {
        Self {
            next_session_id: 1,
            next_request_id: 1,
            active: None,
            last_terminal: None,
        }
    }
}

impl SessionCoordinator {
    #[cfg(test)]
    fn with_next_ids(next_session_id: u64, next_request_id: u64) -> Self {
        Self {
            next_session_id,
            next_request_id,
            ..Self::default()
        }
    }

    #[cfg(test)]
    pub(crate) fn seed_active_for_test(
        &mut self,
        session_id: SessionId,
        purpose: SessionPurpose,
        requests: impl IntoIterator<Item = (RequestId, ModelId)>,
    ) {
        let requests = requests
            .into_iter()
            .map(|(request_id, model_id)| {
                (
                    request_id,
                    RequestState {
                        model_id,
                        completed: false,
                        failed: false,
                    },
                )
            })
            .collect::<HashMap<_, _>>();
        self.next_session_id = self.next_session_id.max(session_id.0.saturating_add(1));
        if let Some(maximum_request_id) = requests.keys().map(|request| request.0).max() {
            self.next_request_id = self
                .next_request_id
                .max(maximum_request_id.saturating_add(1));
        }
        self.active = Some(ActiveSession {
            session_id,
            purpose,
            phase: DictationPhase::Transcribing,
            stop_reason: Some(StopReason::Explicit),
            model_load: ModelLoadState::NotStarted,
            preview: None,
            requests,
        });
    }

    pub fn phase(&self) -> DictationPhase {
        self.active
            .as_ref()
            .map_or(DictationPhase::Idle, ActiveSession::phase)
    }

    #[cfg(test)]
    pub fn active(&self) -> Option<&ActiveSession> {
        self.active.as_ref()
    }

    pub fn active_session_id(&self) -> Option<SessionId> {
        self.active.as_ref().map(ActiveSession::id)
    }

    pub fn active_purpose(&self) -> Option<SessionPurpose> {
        self.active.as_ref().map(ActiveSession::purpose)
    }

    pub fn stop_reason(&self) -> Option<StopReason> {
        self.active.as_ref().and_then(ActiveSession::stop_reason)
    }

    pub fn last_terminal(&self) -> Option<&TerminalSession> {
        self.last_terminal.as_ref()
    }

    pub fn begin(&mut self, purpose: SessionPurpose) -> Result<SessionId, CoordinatorError> {
        if self.active.is_some() {
            return Err(CoordinatorError::Busy);
        }
        let session_id = SessionId(self.next_session_id);
        self.next_session_id = self
            .next_session_id
            .checked_add(1)
            .ok_or(CoordinatorError::SessionIdExhausted)?;
        self.active = Some(ActiveSession {
            session_id,
            purpose,
            phase: DictationPhase::StartingCapture,
            stop_reason: None,
            model_load: ModelLoadState::NotStarted,
            preview: None,
            requests: HashMap::new(),
        });
        Ok(session_id)
    }

    pub fn capture_started(&mut self, session_id: SessionId) -> Result<(), CoordinatorError> {
        self.transition(
            session_id,
            DictationPhase::StartingCapture,
            DictationPhase::Capturing,
        )
    }

    pub fn request_stop(
        &mut self,
        session_id: SessionId,
        reason: StopReason,
    ) -> Result<StopReason, CoordinatorError> {
        let active = self.active_mut(session_id)?;
        if !matches!(
            active.phase,
            DictationPhase::StartingCapture
                | DictationPhase::Capturing
                | DictationPhase::FinalizingCapture
        ) {
            return Err(CoordinatorError::IllegalTransition {
                from: active.phase,
                to: DictationPhase::FinalizingCapture,
            });
        }
        let resolved = active
            .stop_reason
            .map_or(reason, |current| current.max(reason));
        active.stop_reason = Some(resolved);
        Ok(resolved)
    }

    pub fn capture_finalized(&mut self, session_id: SessionId) -> Result<(), CoordinatorError> {
        if self.active_ref(session_id)?.preview.is_some() {
            return Err(CoordinatorError::PreviewStillActive);
        }
        self.transition(
            session_id,
            DictationPhase::Capturing,
            DictationPhase::FinalizingCapture,
        )
    }

    pub fn model_load_started(
        &mut self,
        session_id: SessionId,
        model_id: ModelId,
    ) -> Result<(), CoordinatorError> {
        let active = self.active_mut(session_id)?;
        if !matches!(
            active.phase,
            DictationPhase::StartingCapture | DictationPhase::Capturing
        ) {
            return Err(CoordinatorError::IllegalTransition {
                from: active.phase,
                to: active.phase,
            });
        }
        active.model_load = ModelLoadState::Loading { model_id };
        Ok(())
    }

    pub fn model_load_finished(
        &mut self,
        session_id: SessionId,
        model_id: &ModelId,
        succeeded: bool,
    ) -> Result<(), CoordinatorError> {
        let active = self.active_mut(session_id)?;
        let ModelLoadState::Loading { model_id: expected } = &active.model_load else {
            return Err(CoordinatorError::WrongPreloadModel {
                expected: model_id.clone(),
                actual: model_id.clone(),
            });
        };
        if expected != model_id {
            return Err(CoordinatorError::WrongPreloadModel {
                expected: expected.clone(),
                actual: model_id.clone(),
            });
        }
        active.model_load = if succeeded {
            ModelLoadState::Ready {
                model_id: model_id.clone(),
            }
        } else {
            ModelLoadState::Failed {
                model_id: model_id.clone(),
            }
        };
        Ok(())
    }

    pub fn start_request(
        &mut self,
        session_id: SessionId,
        model_id: ModelId,
    ) -> Result<RequestId, CoordinatorError> {
        let phase = self.active_ref(session_id)?.phase;
        if !matches!(
            phase,
            DictationPhase::FinalizingCapture | DictationPhase::Transcribing
        ) {
            return Err(CoordinatorError::IllegalTransition {
                from: phase,
                to: DictationPhase::Transcribing,
            });
        }
        let request_id = RequestId(self.next_request_id);
        self.next_request_id = self
            .next_request_id
            .checked_add(1)
            .ok_or(CoordinatorError::RequestIdExhausted)?;
        let active = self.active_mut(session_id)?;
        active.phase = DictationPhase::Transcribing;
        active.requests.insert(
            request_id,
            RequestState {
                model_id,
                completed: false,
                failed: false,
            },
        );
        Ok(request_id)
    }

    pub fn start_preview(
        &mut self,
        session_id: SessionId,
        model_id: ModelId,
    ) -> Result<RequestId, CoordinatorError> {
        let active = self.active_ref(session_id)?;
        if active.purpose != SessionPurpose::Dictation
            || !matches!(
                active.phase,
                DictationPhase::StartingCapture | DictationPhase::Capturing
            )
        {
            return Err(CoordinatorError::IllegalTransition {
                from: active.phase,
                to: active.phase,
            });
        }
        if active.preview.is_some() {
            return Err(CoordinatorError::PreviewAlreadyActive);
        }
        let request_id = RequestId(self.next_request_id);
        self.next_request_id = self
            .next_request_id
            .checked_add(1)
            .ok_or(CoordinatorError::RequestIdExhausted)?;
        self.active_mut(session_id)?.preview = Some(PreviewState {
            request_id,
            model_id,
            last_sequence: None,
        });
        Ok(request_id)
    }

    pub fn accept_preview_update(
        &mut self,
        session_id: SessionId,
        request_id: RequestId,
        model_id: &ModelId,
        sequence: u64,
    ) -> Result<(), CoordinatorError> {
        let active = self.active_mut(session_id)?;
        if !matches!(
            active.phase,
            DictationPhase::StartingCapture | DictationPhase::Capturing
        ) {
            return Err(CoordinatorError::IllegalTransition {
                from: active.phase,
                to: active.phase,
            });
        }
        let preview = active
            .preview
            .as_mut()
            .filter(|preview| preview.request_id == request_id)
            .ok_or(CoordinatorError::UnknownRequest(request_id))?;
        if &preview.model_id != model_id {
            return Err(CoordinatorError::WrongModel {
                request_id,
                expected: preview.model_id.clone(),
                actual: model_id.clone(),
            });
        }
        if let Some(previous) = preview.last_sequence
            && sequence <= previous
        {
            return Err(CoordinatorError::StaleSequence {
                previous,
                actual: sequence,
            });
        }
        preview.last_sequence = Some(sequence);
        Ok(())
    }

    pub fn finish_preview(
        &mut self,
        session_id: SessionId,
        request_id: RequestId,
        model_id: &ModelId,
    ) -> Result<(), CoordinatorError> {
        let active = self.active_mut(session_id)?;
        let preview = active
            .preview
            .as_ref()
            .filter(|preview| preview.request_id == request_id)
            .ok_or(CoordinatorError::UnknownRequest(request_id))?;
        if &preview.model_id != model_id {
            return Err(CoordinatorError::WrongModel {
                request_id,
                expected: preview.model_id.clone(),
                actual: model_id.clone(),
            });
        }
        active.preview = None;
        Ok(())
    }

    pub fn is_current_preview(
        &self,
        session_id: SessionId,
        request_id: RequestId,
        model_id: &ModelId,
    ) -> bool {
        self.active.as_ref().is_some_and(|active| {
            active.session_id == session_id
                && active.purpose == SessionPurpose::Dictation
                && matches!(
                    active.phase,
                    DictationPhase::StartingCapture | DictationPhase::Capturing
                )
                && active.preview.as_ref().is_some_and(|preview| {
                    preview.request_id == request_id && &preview.model_id == model_id
                })
        })
    }

    pub fn complete_request(
        &mut self,
        session_id: SessionId,
        request_id: RequestId,
        model_id: &ModelId,
    ) -> Result<bool, CoordinatorError> {
        self.finish_request(session_id, request_id, model_id, false)
    }

    pub fn fail_request(
        &mut self,
        session_id: SessionId,
        request_id: RequestId,
        model_id: &ModelId,
    ) -> Result<bool, CoordinatorError> {
        self.finish_request(session_id, request_id, model_id, true)
    }

    fn finish_request(
        &mut self,
        session_id: SessionId,
        request_id: RequestId,
        model_id: &ModelId,
        failed: bool,
    ) -> Result<bool, CoordinatorError> {
        let request = self.request_mut(session_id, request_id, model_id)?;
        if request.completed {
            return Err(CoordinatorError::DuplicateCompletion(request_id));
        }
        request.completed = true;
        request.failed = failed;
        Ok(self
            .active_ref(session_id)?
            .requests
            .values()
            .all(|request| request.completed))
    }

    pub fn is_current_request(
        &self,
        purpose: SessionPurpose,
        session_id: SessionId,
        request_id: RequestId,
    ) -> bool {
        self.active.as_ref().is_some_and(|active| {
            active.session_id == session_id
                && active.purpose == purpose
                && active
                    .requests
                    .get(&request_id)
                    .is_some_and(|request| !request.completed)
        })
    }

    pub fn request_model(&self, session_id: SessionId, request_id: RequestId) -> Option<&ModelId> {
        self.active
            .as_ref()
            .filter(|active| active.session_id == session_id)?
            .requests
            .get(&request_id)
            .map(|request| &request.model_id)
    }

    #[cfg(test)]
    pub fn pending_request_count(&self, session_id: SessionId) -> Option<usize> {
        self.active
            .as_ref()
            .filter(|active| active.session_id == session_id)
            .map(ActiveSession::pending_request_count)
    }

    pub fn has_failed_requests(&self, session_id: SessionId) -> Option<bool> {
        self.active
            .as_ref()
            .filter(|active| active.session_id == session_id)
            .map(|active| active.requests.values().any(|request| request.failed))
    }

    pub fn begin_output(&mut self, session_id: SessionId) -> Result<(), CoordinatorError> {
        let active = self.active_mut(session_id)?;
        if active.phase != DictationPhase::Transcribing {
            return Err(CoordinatorError::IllegalTransition {
                from: active.phase,
                to: DictationPhase::Output,
            });
        }
        if active.requests.values().any(|request| !request.completed) {
            return Err(CoordinatorError::PendingRequests);
        }
        if active.requests.values().any(|request| request.failed) {
            return Err(CoordinatorError::FailedRequests);
        }
        active.phase = DictationPhase::Output;
        Ok(())
    }

    pub fn complete(&mut self, session_id: SessionId) -> Result<(), CoordinatorError> {
        let active = self.active_ref(session_id)?;
        let legal = active.phase == DictationPhase::Output
            || (active.phase == DictationPhase::Transcribing
                && active.requests.values().all(|request| request.completed)
                && active.requests.values().all(|request| !request.failed));
        if !legal {
            return Err(CoordinatorError::IllegalTransition {
                from: active.phase,
                to: DictationPhase::Idle,
            });
        }
        self.retire(session_id, TerminalOutcome::Completed)
    }

    #[cfg(test)]
    pub fn cancel(&mut self, session_id: SessionId) -> Result<(), CoordinatorError> {
        self.retire(session_id, TerminalOutcome::Cancelled)
    }

    pub fn cancel_active(&mut self) -> Option<SessionId> {
        let session_id = self.active_session_id()?;
        self.retire(session_id, TerminalOutcome::Cancelled)
            .expect("active session must be cancellable");
        Some(session_id)
    }

    pub fn fail(&mut self, session_id: SessionId) -> Result<(), CoordinatorError> {
        self.retire(session_id, TerminalOutcome::Failed)
    }

    fn request_mut(
        &mut self,
        session_id: SessionId,
        request_id: RequestId,
        model_id: &ModelId,
    ) -> Result<&mut RequestState, CoordinatorError> {
        let active = self.active_mut(session_id)?;
        let request = active
            .requests
            .get_mut(&request_id)
            .ok_or(CoordinatorError::UnknownRequest(request_id))?;
        if &request.model_id != model_id {
            return Err(CoordinatorError::WrongModel {
                request_id,
                expected: request.model_id.clone(),
                actual: model_id.clone(),
            });
        }
        Ok(request)
    }

    fn transition(
        &mut self,
        session_id: SessionId,
        from: DictationPhase,
        to: DictationPhase,
    ) -> Result<(), CoordinatorError> {
        let active = self.active_mut(session_id)?;
        if active.phase != from {
            return Err(CoordinatorError::IllegalTransition {
                from: active.phase,
                to,
            });
        }
        active.phase = to;
        Ok(())
    }

    fn retire(
        &mut self,
        session_id: SessionId,
        outcome: TerminalOutcome,
    ) -> Result<(), CoordinatorError> {
        let active = self
            .active
            .take()
            .ok_or(CoordinatorError::StaleSession(session_id))?;
        if active.session_id != session_id {
            self.active = Some(active);
            return Err(CoordinatorError::StaleSession(session_id));
        }
        self.last_terminal = Some(TerminalSession {
            session_id,
            purpose: active.purpose,
            outcome,
        });
        Ok(())
    }

    fn active_ref(&self, session_id: SessionId) -> Result<&ActiveSession, CoordinatorError> {
        self.active
            .as_ref()
            .filter(|active| active.session_id == session_id)
            .ok_or(CoordinatorError::StaleSession(session_id))
    }

    fn active_mut(
        &mut self,
        session_id: SessionId,
    ) -> Result<&mut ActiveSession, CoordinatorError> {
        self.active
            .as_mut()
            .filter(|active| active.session_id == session_id)
            .ok_or(CoordinatorError::StaleSession(session_id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(id: &str) -> ModelId {
        ModelId::new(id)
    }

    fn captured(coordinator: &mut SessionCoordinator, purpose: SessionPurpose) -> SessionId {
        let session_id = coordinator.begin(purpose).unwrap();
        coordinator.capture_started(session_id).unwrap();
        session_id
    }

    fn transcribing(
        coordinator: &mut SessionCoordinator,
        purpose: SessionPurpose,
        model_id: &str,
    ) -> (SessionId, RequestId) {
        let session_id = captured(coordinator, purpose);
        coordinator
            .request_stop(session_id, StopReason::Explicit)
            .unwrap();
        coordinator.capture_finalized(session_id).unwrap();
        let request_id = coordinator
            .start_request(session_id, model(model_id))
            .unwrap();
        (session_id, request_id)
    }

    #[test]
    fn legal_dictation_path_returns_to_idle_with_terminal_outcome() {
        let mut coordinator = SessionCoordinator::default();
        let (session_id, request_id) =
            transcribing(&mut coordinator, SessionPurpose::Dictation, "balanced");
        assert!(
            coordinator
                .complete_request(session_id, request_id, &model("balanced"))
                .unwrap()
        );
        coordinator.begin_output(session_id).unwrap();
        coordinator.complete(session_id).unwrap();
        assert_eq!(coordinator.phase(), DictationPhase::Idle);
        assert_eq!(
            coordinator.last_terminal().unwrap().outcome,
            TerminalOutcome::Completed
        );
    }

    #[test]
    fn preview_sequence_is_separate_from_final_request_completion() {
        let mut coordinator = SessionCoordinator::default();
        let session_id = coordinator.begin(SessionPurpose::Dictation).unwrap();
        let preview_id = coordinator
            .start_preview(session_id, model("balanced"))
            .unwrap();
        coordinator.capture_started(session_id).unwrap();
        coordinator
            .accept_preview_update(session_id, preview_id, &model("balanced"), 2)
            .unwrap();
        assert_eq!(
            coordinator.accept_preview_update(session_id, preview_id, &model("balanced"), 1),
            Err(CoordinatorError::StaleSequence {
                previous: 2,
                actual: 1,
            })
        );

        coordinator
            .request_stop(session_id, StopReason::Explicit)
            .unwrap();
        coordinator
            .finish_preview(session_id, preview_id, &model("balanced"))
            .unwrap();
        coordinator.capture_finalized(session_id).unwrap();
        let final_id = coordinator
            .start_request(session_id, model("balanced"))
            .unwrap();
        assert_ne!(preview_id, final_id);
        assert!(
            coordinator
                .complete_request(session_id, final_id, &model("balanced"))
                .unwrap()
        );
        coordinator.begin_output(session_id).unwrap();
    }

    #[test]
    fn capture_cannot_finalize_until_the_preview_scheduler_is_closed() {
        let mut coordinator = SessionCoordinator::default();
        let session_id = coordinator.begin(SessionPurpose::Dictation).unwrap();
        let preview_id = coordinator
            .start_preview(session_id, model("balanced"))
            .unwrap();
        coordinator.capture_started(session_id).unwrap();
        coordinator
            .request_stop(session_id, StopReason::Explicit)
            .unwrap();

        assert_eq!(
            coordinator.capture_finalized(session_id),
            Err(CoordinatorError::PreviewStillActive)
        );
        assert_eq!(coordinator.phase(), DictationPhase::Capturing);
        coordinator
            .finish_preview(session_id, preview_id, &model("balanced"))
            .unwrap();
        coordinator.capture_finalized(session_id).unwrap();
    }

    #[test]
    fn preview_rejects_wrong_model_stale_session_and_updates_after_finish() {
        let mut coordinator = SessionCoordinator::default();
        let session_id = coordinator.begin(SessionPurpose::Dictation).unwrap();
        let preview_id = coordinator
            .start_preview(session_id, model("balanced"))
            .unwrap();

        assert!(matches!(
            coordinator.accept_preview_update(session_id, preview_id, &model("different"), 1),
            Err(CoordinatorError::WrongModel { .. })
        ));
        assert!(matches!(
            coordinator.accept_preview_update(
                SessionId(session_id.0 + 1),
                preview_id,
                &model("balanced"),
                1
            ),
            Err(CoordinatorError::StaleSession(_))
        ));
        coordinator
            .finish_preview(session_id, preview_id, &model("balanced"))
            .unwrap();
        assert!(!coordinator.is_current_preview(session_id, preview_id, &model("balanced")));
        assert!(matches!(
            coordinator.accept_preview_update(session_id, preview_id, &model("balanced"), 1),
            Err(CoordinatorError::UnknownRequest(_))
        ));
    }

    #[test]
    fn illegal_transitions_do_not_mutate_phase() {
        let mut coordinator = SessionCoordinator::default();
        let session_id = coordinator.begin(SessionPurpose::Dictation).unwrap();
        assert!(matches!(
            coordinator.capture_finalized(session_id),
            Err(CoordinatorError::IllegalTransition { .. })
        ));
        assert_eq!(coordinator.phase(), DictationPhase::StartingCapture);
    }

    #[test]
    fn busy_begin_is_rejected_without_consuming_an_identifier() {
        let mut coordinator = SessionCoordinator::default();
        let first = coordinator.begin(SessionPurpose::Dictation).unwrap();
        assert_eq!(
            coordinator.begin(SessionPurpose::Comparison),
            Err(CoordinatorError::Busy)
        );
        coordinator.cancel(first).unwrap();
        assert_eq!(
            coordinator.begin(SessionPurpose::Comparison).unwrap(),
            SessionId(2)
        );
    }

    #[test]
    fn identifier_overflow_fails_closed() {
        let mut sessions = SessionCoordinator::with_next_ids(u64::MAX, 1);
        assert_eq!(
            sessions.begin(SessionPurpose::Dictation),
            Err(CoordinatorError::SessionIdExhausted)
        );
        assert!(sessions.active().is_none());

        let mut requests = SessionCoordinator::with_next_ids(1, u64::MAX);
        let session_id = captured(&mut requests, SessionPurpose::Dictation);
        requests.capture_finalized(session_id).unwrap();
        assert_eq!(
            requests.start_request(session_id, model("balanced")),
            Err(CoordinatorError::RequestIdExhausted)
        );
    }

    #[test]
    fn explicit_stop_outranks_endpoint_and_maximum_duration() {
        let mut coordinator = SessionCoordinator::default();
        let session_id = captured(&mut coordinator, SessionPurpose::Dictation);
        assert_eq!(
            coordinator
                .request_stop(session_id, StopReason::Endpoint)
                .unwrap(),
            StopReason::Endpoint
        );
        assert_eq!(
            coordinator
                .request_stop(session_id, StopReason::MaximumDuration)
                .unwrap(),
            StopReason::MaximumDuration
        );
        assert_eq!(
            coordinator
                .request_stop(session_id, StopReason::Explicit)
                .unwrap(),
            StopReason::Explicit
        );
        assert_eq!(
            coordinator
                .request_stop(session_id, StopReason::Endpoint)
                .unwrap(),
            StopReason::Explicit
        );
    }

    #[test]
    fn stale_cross_purpose_and_wrong_model_events_are_rejected() {
        let mut coordinator = SessionCoordinator::default();
        let (session_id, request_id) =
            transcribing(&mut coordinator, SessionPurpose::Comparison, "first");
        assert!(!coordinator.is_current_request(SessionPurpose::Dictation, session_id, request_id));
        assert!(matches!(
            coordinator.complete_request(session_id, request_id, &model("second")),
            Err(CoordinatorError::WrongModel { .. })
        ));
        assert!(matches!(
            coordinator.complete_request(SessionId(session_id.0 + 1), request_id, &model("first")),
            Err(CoordinatorError::StaleSession(_))
        ));
    }

    #[test]
    fn completion_is_exactly_once() {
        let mut coordinator = SessionCoordinator::default();
        let (session_id, request_id) =
            transcribing(&mut coordinator, SessionPurpose::Dictation, "balanced");
        coordinator
            .complete_request(session_id, request_id, &model("balanced"))
            .unwrap();
        assert_eq!(
            coordinator.complete_request(session_id, request_id, &model("balanced")),
            Err(CoordinatorError::DuplicateCompletion(request_id))
        );
    }

    #[test]
    fn comparison_waits_for_every_registered_request() {
        let mut coordinator = SessionCoordinator::default();
        let session_id = captured(&mut coordinator, SessionPurpose::Comparison);
        coordinator.capture_finalized(session_id).unwrap();
        let first = coordinator
            .start_request(session_id, model("first"))
            .unwrap();
        let second = coordinator
            .start_request(session_id, model("second"))
            .unwrap();
        assert!(
            !coordinator
                .complete_request(session_id, first, &model("first"))
                .unwrap()
        );
        assert_eq!(coordinator.pending_request_count(session_id), Some(1));
        assert!(
            coordinator
                .complete_request(session_id, second, &model("second"))
                .unwrap()
        );
        coordinator.complete(session_id).unwrap();
    }

    #[test]
    fn cancellation_retires_every_active_phase() {
        for phase in [
            DictationPhase::StartingCapture,
            DictationPhase::Capturing,
            DictationPhase::FinalizingCapture,
            DictationPhase::Transcribing,
            DictationPhase::Output,
        ] {
            let mut coordinator = SessionCoordinator::default();
            let session_id = coordinator.begin(SessionPurpose::Dictation).unwrap();
            if phase >= DictationPhase::Capturing {
                coordinator.capture_started(session_id).unwrap();
            }
            if phase >= DictationPhase::FinalizingCapture {
                coordinator.capture_finalized(session_id).unwrap();
            }
            if phase >= DictationPhase::Transcribing {
                let request = coordinator
                    .start_request(session_id, model("balanced"))
                    .unwrap();
                if phase == DictationPhase::Output {
                    coordinator
                        .complete_request(session_id, request, &model("balanced"))
                        .unwrap();
                    coordinator.begin_output(session_id).unwrap();
                }
            }
            coordinator.cancel(session_id).unwrap();
            assert_eq!(coordinator.phase(), DictationPhase::Idle);
            assert_eq!(
                coordinator.last_terminal().unwrap().outcome,
                TerminalOutcome::Cancelled
            );
        }
    }

    #[test]
    fn preload_events_are_correlated_to_session_and_model() {
        let mut coordinator = SessionCoordinator::default();
        let session_id = captured(&mut coordinator, SessionPurpose::Dictation);
        coordinator
            .model_load_started(session_id, model("balanced"))
            .unwrap();
        assert!(matches!(
            coordinator.model_load_finished(session_id, &model("other"), true),
            Err(CoordinatorError::WrongPreloadModel { .. })
        ));
        coordinator
            .model_load_finished(session_id, &model("balanced"), true)
            .unwrap();
        assert!(matches!(
            coordinator.active().unwrap().model_load(),
            ModelLoadState::Ready { .. }
        ));
        assert!(matches!(
            coordinator.model_load_finished(SessionId(99), &model("balanced"), true),
            Err(CoordinatorError::StaleSession(_))
        ));
    }
}
