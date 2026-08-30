use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Instant;

use crossbeam_channel::{Receiver, Sender, select, unbounded};

use super::{
    CaptureCancellation, CaptureError, CaptureId, CaptureOptions, CaptureStartContext,
    PreviewPublisherSlot, RecordingSession, VadPrewarmService,
};

pub(crate) type StartCapture = dyn Fn(CaptureRequest, CaptureCancellation) -> Result<RecordingSession, CaptureError>
    + Send
    + Sync;
type ReleaseReaperTask = Box<dyn FnOnce() + Send + 'static>;
type ReleaseReaperSpawner =
    dyn Fn(String, ReleaseReaperTask) -> Result<(), String> + Send + Sync + 'static;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CaptureHotkeyMode {
    HoldToTalk,
    Toggle,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AudioOwnerKind {
    Capture,
    MicrophoneTest,
    Playback,
}

#[derive(Clone)]
pub(crate) struct CaptureRequest {
    pub(crate) capture_id: CaptureId,
    pub(crate) observed_at: Instant,
    pub(crate) max_duration_seconds: u32,
    pub(crate) input_device_name: Option<String>,
    pub(crate) options: CaptureOptions,
    pub(crate) preview_slot: PreviewPublisherSlot,
    pub(crate) owner: AudioOwnerKind,
}

#[derive(Clone, PartialEq)]
struct StartTemplate {
    max_duration_seconds: u32,
    input_device_name: Option<String>,
    options: CaptureOptions,
}

#[derive(Clone, Default, PartialEq)]
struct HotkeySnapshot {
    enabled: bool,
    mode: Option<CaptureHotkeyMode>,
    start: Option<StartTemplate>,
}

#[derive(Clone)]
pub(crate) struct CaptureTicket {
    pub(crate) capture_id: CaptureId,
    pub(crate) preview_slot: PreviewPublisherSlot,
    config_revision: u64,
}

#[derive(Clone)]
pub(crate) struct AudioOwnerLease {
    pub(crate) id: u64,
    pub(crate) owner: AudioOwnerKind,
}

#[derive(Clone)]
pub(crate) enum HotkeyDispatch {
    Start(CaptureTicket),
    Stop { capture_id: CaptureId },
    None,
}

pub(crate) enum CaptureLifecycleEvent {
    Starting {
        capture_id: CaptureId,
        owner: AudioOwnerKind,
    },
    Ready {
        capture_id: CaptureId,
        owner: AudioOwnerKind,
        session: RecordingSession,
    },
    StopRequested {
        capture_id: CaptureId,
    },
    Aborted {
        capture_id: CaptureId,
    },
    Failed {
        capture_id: CaptureId,
        owner: AudioOwnerKind,
        error: CaptureError,
    },
    Released {
        capture_id: CaptureId,
        owner: AudioOwnerKind,
    },
    Reconfigured {
        revision: u64,
    },
    Shutdown,
}

#[derive(Debug, thiserror::Error, Eq, PartialEq)]
pub(crate) enum CaptureControlError {
    #[error("audio is already owned by {0:?}")]
    Owned(AudioOwnerKind),
    #[error("capture command references stale id {0}")]
    Stale(u64),
    #[error("audio controller has shut down")]
    Shutdown,
}

struct OwnerState {
    id: u64,
    kind: AudioOwnerKind,
    observed_at: Instant,
    cancellation: Option<CaptureCancellation>,
    session: Option<RecordingSession>,
    release_requested: bool,
    reaper_started: bool,
    adopted: bool,
}

struct SharedState {
    next_id: u64,
    owner: Option<OwnerState>,
    hotkey: HotkeySnapshot,
    config_revision: u64,
    shutdown: bool,
}

impl Default for SharedState {
    fn default() -> Self {
        Self {
            next_id: 1,
            owner: None,
            hotkey: HotkeySnapshot::default(),
            config_revision: 0,
            shutdown: false,
        }
    }
}

enum ControlCommand {
    Start {
        request: CaptureRequest,
        cancellation: CaptureCancellation,
    },
    Stop {
        capture_id: CaptureId,
    },
    Abort {
        capture_id: CaptureId,
    },
    Reconfigure {
        revision: u64,
    },
    Shutdown,
}

struct StartResult {
    request: CaptureRequest,
    result: Result<RecordingSession, CaptureError>,
}

#[derive(Clone)]
pub(crate) struct CaptureControlHandle {
    state: Arc<Mutex<SharedState>>,
    command_tx: Sender<ControlCommand>,
    lifecycle_tx: Sender<CaptureLifecycleEvent>,
    reaper_spawner: Arc<ReleaseReaperSpawner>,
}

pub(crate) struct CaptureController {
    handle: CaptureControlHandle,
    lifecycle_rx: Receiver<CaptureLifecycleEvent>,
    worker: Option<thread::JoinHandle<()>>,
    vad_service: Option<Arc<VadPrewarmService>>,
}

impl CaptureController {
    pub(crate) fn new() -> Result<Self, CaptureError> {
        super::initialize_capture_timing_logger();
        let vad_service = VadPrewarmService::new();
        #[cfg(not(test))]
        vad_service.prewarm()?;
        let start_vad_service = Arc::clone(&vad_service);
        Self::with_components(
            Arc::new(move |request, cancellation| {
                super::start_recording(
                    CaptureStartContext::new(request.capture_id, request.observed_at),
                    request.max_duration_seconds,
                    request.input_device_name,
                    request.options,
                    request.preview_slot,
                    cancellation,
                    Arc::clone(&start_vad_service),
                )
            }),
            Arc::new(spawn_release_reaper_thread),
            Some(vad_service),
        )
    }

    #[cfg(test)]
    pub(crate) fn with_start_capture_for_test(
        start_capture: Arc<StartCapture>,
    ) -> Result<Self, CaptureError> {
        Self::with_start_capture(start_capture)
    }

    #[cfg(test)]
    pub(crate) fn with_reaper_spawner_for_test(
        start_capture: Arc<StartCapture>,
        reaper_spawner: Arc<ReleaseReaperSpawner>,
    ) -> Result<Self, CaptureError> {
        Self::with_components(start_capture, reaper_spawner, None)
    }

    #[cfg(test)]
    fn with_start_capture(start_capture: Arc<StartCapture>) -> Result<Self, CaptureError> {
        Self::with_components(start_capture, Arc::new(spawn_release_reaper_thread), None)
    }

    fn with_components(
        start_capture: Arc<StartCapture>,
        reaper_spawner: Arc<ReleaseReaperSpawner>,
        vad_service: Option<Arc<VadPrewarmService>>,
    ) -> Result<Self, CaptureError> {
        let state = Arc::new(Mutex::new(SharedState::default()));
        let (command_tx, command_rx) = unbounded();
        let (lifecycle_tx, lifecycle_rx) = unbounded();
        let worker_state = Arc::clone(&state);
        let worker_lifecycle_tx = lifecycle_tx.clone();
        let worker_reaper_spawner = Arc::clone(&reaper_spawner);
        let worker = thread::Builder::new()
            .name("scribe-audio-control".to_owned())
            .spawn(move || {
                control_loop(
                    worker_state,
                    command_rx,
                    worker_lifecycle_tx,
                    start_capture,
                    worker_reaper_spawner,
                )
            })
            .map_err(|error| CaptureError::WorkerSpawn(error.to_string()))?;
        Ok(Self {
            handle: CaptureControlHandle {
                state,
                command_tx,
                lifecycle_tx,
                reaper_spawner,
            },
            lifecycle_rx,
            worker: Some(worker),
            vad_service,
        })
    }

    pub(crate) fn handle(&self) -> CaptureControlHandle {
        self.handle.clone()
    }

    pub(crate) fn poll_events(&self) -> Vec<CaptureLifecycleEvent> {
        let mut events = Vec::new();
        while let Ok(event) = self.lifecycle_rx.try_recv() {
            events.push(event);
        }
        events
    }
}

impl Drop for CaptureController {
    fn drop(&mut self) {
        self.handle.shutdown();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        if let Some(vad_service) = self.vad_service.take() {
            vad_service.shutdown();
        }
    }
}

impl CaptureControlHandle {
    pub(crate) fn reconfigure_hotkey(
        &self,
        enabled: bool,
        mode: CaptureHotkeyMode,
        max_duration_seconds: u32,
        input_device_name: Option<String>,
        options: CaptureOptions,
    ) -> Result<u64, CaptureControlError> {
        let (revision, revoke_capture_id) = {
            let mut state = self.lock_state();
            if state.shutdown {
                return Err(CaptureControlError::Shutdown);
            }
            let replacement = HotkeySnapshot {
                enabled,
                mode: Some(mode),
                start: Some(StartTemplate {
                    max_duration_seconds,
                    input_device_name,
                    options,
                }),
            };
            if state.hotkey == replacement {
                return Ok(state.config_revision);
            }
            state.config_revision = state
                .config_revision
                .checked_add(1)
                .ok_or(CaptureControlError::Shutdown)?;
            state.hotkey = replacement;
            let revoke_capture_id = state.owner.as_ref().and_then(|owner| {
                (owner.kind == AudioOwnerKind::Capture && !owner.adopted)
                    .then_some(CaptureId(owner.id))
            });
            (state.config_revision, revoke_capture_id)
        };
        if let Some(capture_id) = revoke_capture_id {
            self.terminate_capture(capture_id);
        }
        self.command_tx
            .send(ControlCommand::Reconfigure { revision })
            .map_err(|_| CaptureControlError::Shutdown)?;
        Ok(revision)
    }

    pub(crate) fn dispatch_hotkey(&self, pressed: bool, observed_at: Instant) -> HotkeyDispatch {
        let mut state = self.lock_state();
        if state.shutdown {
            return HotkeyDispatch::None;
        }
        let Some(mode) = state.hotkey.mode else {
            return HotkeyDispatch::None;
        };
        let should_stop = matches!(
            (mode, pressed),
            (CaptureHotkeyMode::HoldToTalk, false) | (CaptureHotkeyMode::Toggle, true)
        );
        if should_stop
            && let Some(owner) = state
                .owner
                .as_ref()
                .filter(|owner| owner.kind == AudioOwnerKind::Capture)
        {
            let capture_id = CaptureId(owner.id);
            request_stop(owner, observed_at);
            let _ = self.command_tx.send(ControlCommand::Stop { capture_id });
            return HotkeyDispatch::Stop { capture_id };
        }

        // Eligibility changes can suppress a new capture, but must never strand
        // an existing controller-owned capture without its mode-correct Stop.
        if !state.hotkey.enabled {
            return HotkeyDispatch::None;
        }
        let should_start = matches!(
            (mode, pressed, state.owner.as_ref()),
            (CaptureHotkeyMode::HoldToTalk, true, None) | (CaptureHotkeyMode::Toggle, true, None)
        );
        if should_start {
            let Some(template) = state.hotkey.start.clone() else {
                return HotkeyDispatch::None;
            };
            return self.start_locked(
                &mut state,
                AudioOwnerKind::Capture,
                observed_at,
                template,
                false,
            );
        }

        HotkeyDispatch::None
    }

    pub(crate) fn start_capture(
        &self,
        owner: AudioOwnerKind,
        observed_at: Instant,
        max_duration_seconds: u32,
        input_device_name: Option<String>,
        options: CaptureOptions,
    ) -> Result<CaptureTicket, CaptureControlError> {
        let mut state = self.lock_state();
        if state.shutdown {
            return Err(CaptureControlError::Shutdown);
        }
        if let Some(active) = state.owner.as_ref() {
            return Err(CaptureControlError::Owned(active.kind));
        }
        match self.start_locked(
            &mut state,
            owner,
            observed_at,
            StartTemplate {
                max_duration_seconds,
                input_device_name,
                options,
            },
            true,
        ) {
            HotkeyDispatch::Start(ticket) => Ok(ticket),
            HotkeyDispatch::None | HotkeyDispatch::Stop { .. } => {
                Err(CaptureControlError::Shutdown)
            }
        }
    }

    fn start_locked(
        &self,
        state: &mut SharedState,
        owner: AudioOwnerKind,
        observed_at: Instant,
        template: StartTemplate,
        adopted: bool,
    ) -> HotkeyDispatch {
        let capture_id = CaptureId(state.next_id);
        let Some(next_id) = state.next_id.checked_add(1) else {
            state.shutdown = true;
            return HotkeyDispatch::None;
        };
        state.next_id = next_id;
        let cancellation = CaptureCancellation::new();
        let preview_slot = PreviewPublisherSlot::default();
        let request = CaptureRequest {
            capture_id,
            observed_at,
            max_duration_seconds: template.max_duration_seconds,
            input_device_name: template.input_device_name,
            options: template.options,
            preview_slot: preview_slot.clone(),
            owner,
        };
        state.owner = Some(OwnerState {
            id: capture_id.0,
            kind: owner,
            observed_at,
            cancellation: Some(cancellation.clone()),
            session: None,
            release_requested: false,
            reaper_started: false,
            adopted,
        });
        if self
            .command_tx
            .send(ControlCommand::Start {
                request,
                cancellation,
            })
            .is_err()
        {
            state.owner = None;
            state.shutdown = true;
            return HotkeyDispatch::None;
        }
        HotkeyDispatch::Start(CaptureTicket {
            capture_id,
            preview_slot,
            config_revision: state.config_revision,
        })
    }

    pub(crate) fn adopt_hotkey_capture(
        &self,
        ticket: &CaptureTicket,
    ) -> Result<(), CaptureControlError> {
        let mut state = self.lock_state();
        let eligible = state.hotkey.enabled && state.config_revision == ticket.config_revision;
        let Some(owner) = state.owner.as_mut() else {
            return Err(CaptureControlError::Stale(ticket.capture_id.0));
        };
        if !eligible
            || owner.id != ticket.capture_id.0
            || owner.kind != AudioOwnerKind::Capture
            || owner.adopted
            || owner.release_requested
        {
            return Err(CaptureControlError::Stale(ticket.capture_id.0));
        }
        owner.adopted = true;
        Ok(())
    }

    pub(crate) fn terminate_capture(&self, capture_id: CaptureId) {
        let _ = self.abort(capture_id);
        let _ = self.release(capture_id.0);
    }

    pub(crate) fn stop(&self, capture_id: CaptureId) -> Result<(), CaptureControlError> {
        let state = self.lock_state();
        let owner = matching_capture_owner(&state, capture_id)?;
        request_stop(owner, Instant::now());
        self.command_tx
            .send(ControlCommand::Stop { capture_id })
            .map_err(|_| CaptureControlError::Shutdown)
    }

    pub(crate) fn abort(&self, capture_id: CaptureId) -> Result<(), CaptureControlError> {
        let state = self.lock_state();
        let owner = matching_capture_owner(&state, capture_id)?;
        request_abort(owner);
        self.command_tx
            .send(ControlCommand::Abort { capture_id })
            .map_err(|_| CaptureControlError::Shutdown)
    }

    pub(crate) fn reserve_owner(
        &self,
        owner: AudioOwnerKind,
    ) -> Result<AudioOwnerLease, CaptureControlError> {
        let mut state = self.lock_state();
        if state.shutdown {
            return Err(CaptureControlError::Shutdown);
        }
        if let Some(active) = state.owner.as_ref() {
            return Err(CaptureControlError::Owned(active.kind));
        }
        let id = state.next_id;
        state.next_id = state
            .next_id
            .checked_add(1)
            .ok_or(CaptureControlError::Shutdown)?;
        state.owner = Some(OwnerState {
            id,
            kind: owner,
            observed_at: Instant::now(),
            cancellation: None,
            session: None,
            release_requested: false,
            reaper_started: false,
            adopted: true,
        });
        Ok(AudioOwnerLease { id, owner })
    }

    pub(crate) fn release(&self, id: u64) -> Result<(), CaptureControlError> {
        let (released, reaper) = {
            let mut state = self.lock_state();
            let Some(owner) = state.owner.as_mut() else {
                return Err(CaptureControlError::Stale(id));
            };
            if owner.id != id {
                return Err(CaptureControlError::Stale(id));
            }
            if owner.cancellation.is_none()
                || owner
                    .session
                    .as_ref()
                    .is_some_and(|session| session.try_finish().is_some())
            {
                let kind = owner.kind;
                state.owner = None;
                (Some(kind), None)
            } else {
                owner.release_requested = true;
                if let Some(session) = owner.session.clone()
                    && !owner.reaper_started
                {
                    owner.reaper_started = true;
                    (None, Some((owner.kind, session)))
                } else {
                    (None, None)
                }
            }
        };
        if let Some(kind) = released {
            let _ = self.lifecycle_tx.send(CaptureLifecycleEvent::Released {
                capture_id: CaptureId(id),
                owner: kind,
            });
        }
        if let Some((kind, session)) = reaper {
            spawn_release_reaper(
                Arc::clone(&self.state),
                CaptureId(id),
                kind,
                session,
                self.lifecycle_tx.clone(),
                Arc::clone(&self.reaper_spawner),
            );
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn owner(&self) -> Option<AudioOwnerKind> {
        self.lock_state().owner.as_ref().map(|owner| owner.kind)
    }

    pub(crate) fn owner_id(&self, kind: AudioOwnerKind) -> Option<u64> {
        self.lock_state()
            .owner
            .as_ref()
            .filter(|owner| owner.kind == kind)
            .map(|owner| owner.id)
    }

    fn shutdown(&self) {
        {
            let mut state = self.lock_state();
            state.shutdown = true;
            if let Some(owner) = state.owner.as_ref() {
                request_abort(owner);
            }
        }
        let _ = self.command_tx.send(ControlCommand::Shutdown);
    }

    fn lock_state(&self) -> std::sync::MutexGuard<'_, SharedState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

fn matching_capture_owner(
    state: &SharedState,
    capture_id: CaptureId,
) -> Result<&OwnerState, CaptureControlError> {
    let Some(owner) = state.owner.as_ref() else {
        return Err(CaptureControlError::Stale(capture_id.0));
    };
    if owner.id != capture_id.0 || owner.cancellation.is_none() {
        return Err(CaptureControlError::Stale(capture_id.0));
    }
    Ok(owner)
}

fn request_stop(owner: &OwnerState, observed_at: Instant) {
    if let Some(cancellation) = owner.cancellation.as_ref() {
        cancellation.cancel_at(observed_at.saturating_duration_since(owner.observed_at));
    }
    if let Some(session) = owner.session.as_ref() {
        session.stop();
    }
}

fn request_abort(owner: &OwnerState) {
    if let Some(cancellation) = owner.cancellation.as_ref() {
        cancellation.cancel();
    }
    if let Some(session) = owner.session.as_ref() {
        session.abort();
    }
}

fn spawn_release_reaper_thread(name: String, task: ReleaseReaperTask) -> Result<(), String> {
    thread::Builder::new()
        .name(name)
        .spawn(task)
        .map(|_| ())
        .map_err(|error| error.to_string())
}

fn spawn_release_reaper(
    state: Arc<Mutex<SharedState>>,
    capture_id: CaptureId,
    owner: AudioOwnerKind,
    session: RecordingSession,
    lifecycle_tx: Sender<CaptureLifecycleEvent>,
    reaper_spawner: Arc<ReleaseReaperSpawner>,
) {
    let recovery_session = session.clone();
    let reaper_state = Arc::clone(&state);
    let reaper_lifecycle_tx = lifecycle_tx.clone();
    let task = Box::new(move || {
        while session.try_finish().is_none() {
            thread::sleep(std::time::Duration::from_millis(2));
        }
        let mut state = reaper_state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if state
            .owner
            .as_ref()
            .is_some_and(|active| active.id == capture_id.0)
        {
            state.owner = None;
            drop(state);
            let _ = reaper_lifecycle_tx.send(CaptureLifecycleEvent::Released { capture_id, owner });
        }
    });
    if let Err(error) = reaper_spawner(format!("scribe-audio-release-{}", capture_id.0), task) {
        recovery_session.abort();
        let mut state = state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if state
            .owner
            .as_ref()
            .is_some_and(|active| active.id == capture_id.0)
        {
            state.owner = None;
            state.shutdown = true;
        }
        drop(state);
        let _ = lifecycle_tx.send(CaptureLifecycleEvent::Failed {
            capture_id,
            owner,
            error: CaptureError::WorkerSpawn(format!("release reaper: {error}")),
        });
    }
}

fn control_loop(
    state: Arc<Mutex<SharedState>>,
    command_rx: Receiver<ControlCommand>,
    lifecycle_tx: Sender<CaptureLifecycleEvent>,
    start_capture: Arc<StartCapture>,
    reaper_spawner: Arc<ReleaseReaperSpawner>,
) {
    let (start_result_tx, start_result_rx) = unbounded::<StartResult>();
    loop {
        select! {
            recv(command_rx) -> command => match command {
                Ok(ControlCommand::Start { request, cancellation }) => {
                    let _ = lifecycle_tx.send(CaptureLifecycleEvent::Starting {
                        capture_id: request.capture_id,
                        owner: request.owner,
                    });
                    let result_tx = start_result_tx.clone();
                    let start_capture = Arc::clone(&start_capture);
                    let failed_request = request.clone();
                    if let Err(error) = thread::Builder::new()
                        .name(format!("scribe-audio-start-{}", request.capture_id.0))
                        .spawn(move || {
                            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                start_capture(request.clone(), cancellation)
                            }))
                            .unwrap_or_else(|panic| {
                                Err(CaptureError::WorkerPanic(panic_message(panic)))
                            });
                            let _ = result_tx.send(StartResult { request, result });
                        })
                    {
                        let mut state = state
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner());
                        if state.owner.as_ref().is_some_and(|owner| {
                            owner.id == failed_request.capture_id.0
                                && owner.kind == failed_request.owner
                        }) {
                            state.owner = None;
                        }
                        drop(state);
                        let _ = lifecycle_tx.send(CaptureLifecycleEvent::Failed {
                            capture_id: failed_request.capture_id,
                            owner: failed_request.owner,
                            error: CaptureError::WorkerSpawn(error.to_string()),
                        });
                    }
                }
                Ok(ControlCommand::Stop { capture_id }) => {
                    let _ = lifecycle_tx.send(CaptureLifecycleEvent::StopRequested { capture_id });
                }
                Ok(ControlCommand::Abort { capture_id }) => {
                    let _ = lifecycle_tx.send(CaptureLifecycleEvent::Aborted { capture_id });
                }
                Ok(ControlCommand::Reconfigure { revision }) => {
                    let _ = lifecycle_tx.send(CaptureLifecycleEvent::Reconfigured { revision });
                }
                Ok(ControlCommand::Shutdown) | Err(_) => {
                    let _ = lifecycle_tx.send(CaptureLifecycleEvent::Shutdown);
                    break;
                }
            },
            recv(start_result_rx) -> result => {
                let Ok(StartResult { request, result }) = result else {
                    continue;
                };
                match result {
                    Ok(session) => {
                        let (accepted, reap) = {
                            let mut state = state
                                .lock()
                                .unwrap_or_else(|poisoned| poisoned.into_inner());
                            if let Some(owner) = state.owner.as_mut()
                                && owner.id == request.capture_id.0
                                && owner.kind == request.owner
                            {
                                owner.session = Some(session.clone());
                                if owner.release_requested {
                                    session.abort();
                                    owner.reaper_started = true;
                                    (false, true)
                                } else {
                                    (true, false)
                                }
                            } else {
                                (false, false)
                            }
                        };
                        if accepted {
                            let _ = lifecycle_tx.send(CaptureLifecycleEvent::Ready {
                                capture_id: request.capture_id,
                                owner: request.owner,
                                session,
                            });
                        } else if reap {
                            spawn_release_reaper(
                                Arc::clone(&state),
                                request.capture_id,
                                request.owner,
                                session,
                                lifecycle_tx.clone(),
                                Arc::clone(&reaper_spawner),
                            );
                        } else {
                            session.abort();
                        }
                    }
                    Err(error) => {
                        {
                            let mut state = state
                                .lock()
                                .unwrap_or_else(|poisoned| poisoned.into_inner());
                            if state.owner.as_ref().is_some_and(|owner| {
                                owner.id == request.capture_id.0 && owner.kind == request.owner
                            }) {
                                state.owner = None;
                            }
                        }
                        let _ = lifecycle_tx.send(CaptureLifecycleEvent::Failed {
                            capture_id: request.capture_id,
                            owner: request.owner,
                            error,
                        });
                    }
                }
            }
        }
    }
}

fn panic_message(panic: Box<dyn std::any::Any + Send>) -> String {
    if let Some(message) = panic.downcast_ref::<&str>() {
        (*message).to_owned()
    } else if let Some(message) = panic.downcast_ref::<String>() {
        message.clone()
    } else {
        "non-string panic payload".to_owned()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use super::*;
    use crate::audio::{ABORT_STREAM_DROP_BUDGET, CaptureStopReason};

    fn controller_with_counter(calls: Arc<AtomicUsize>) -> CaptureController {
        CaptureController::with_start_capture(Arc::new(move |_request, cancellation| {
            calls.fetch_add(1, Ordering::Relaxed);
            while !cancellation.is_cancelled() {
                thread::sleep(Duration::from_millis(1));
            }
            Err(CaptureError::StartupCancelled)
        }))
        .unwrap()
    }

    #[test]
    fn idle_controller_constructs_no_capture_resources() {
        let calls = Arc::new(AtomicUsize::new(0));
        let controller = controller_with_counter(Arc::clone(&calls));
        thread::sleep(Duration::from_millis(10));

        assert_eq!(calls.load(Ordering::Relaxed), 0);
        assert!(controller.handle().owner().is_none());
    }

    fn assert_abort_bypasses_normal_finalization(during_post_roll: bool) {
        let (probe_tx, probe_rx) = unbounded();
        let controller = CaptureController::with_start_capture(Arc::new(move |_request, _| {
            let (session, probe) = RecordingSession::simulated_with_abort_probe(
                None,
                CaptureStopReason::Explicit,
                Duration::from_secs(2),
            );
            probe_tx.send(probe).unwrap();
            Ok(session)
        }))
        .unwrap();
        let handle = controller.handle();
        let ticket = handle
            .start_capture(
                AudioOwnerKind::Capture,
                Instant::now(),
                30,
                None,
                CaptureOptions::default(),
            )
            .unwrap();
        let probe = probe_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        let ready = (0..200).any(|_| {
            let ready = controller.poll_events().into_iter().any(|event| {
                matches!(
                    event,
                    CaptureLifecycleEvent::Ready { capture_id, .. }
                        if capture_id == ticket.capture_id
                )
            });
            if !ready {
                thread::sleep(Duration::from_millis(1));
            }
            ready
        });
        assert!(ready);

        if during_post_roll {
            handle.stop(ticket.capture_id).unwrap();
            for _ in 0..200 {
                if probe.post_roll_entered.load(Ordering::Acquire) {
                    break;
                }
                thread::sleep(Duration::from_millis(1));
            }
            assert!(probe.post_roll_entered.load(Ordering::Acquire));
        }

        let aborted_at = Instant::now();
        handle.abort(ticket.capture_id).unwrap();
        assert!(probe.preview_invalidated.load(Ordering::Acquire));
        handle.release(ticket.capture_id.0).unwrap();
        if handle.owner().is_some() {
            assert!(matches!(
                handle.reserve_owner(AudioOwnerKind::Playback),
                Err(CaptureControlError::Owned(AudioOwnerKind::Capture))
            ));
        }
        for _ in 0..250 {
            if handle.owner().is_none() {
                break;
            }
            thread::sleep(Duration::from_millis(1));
        }

        assert!(handle.owner().is_none());
        assert!(aborted_at.elapsed() < ABORT_STREAM_DROP_BUDGET);
        assert!(probe.stream_dropped.load(Ordering::Acquire));
        assert!(!probe.finish_called.load(Ordering::Acquire));
        assert!(!probe.terminal_preview_called.load(Ordering::Acquire));
        let playback = handle.reserve_owner(AudioOwnerKind::Playback).unwrap();
        handle.release(playback.id).unwrap();
    }

    #[test]
    fn abort_during_active_drain_drops_stream_without_normal_finalization() {
        assert_abort_bypasses_normal_finalization(false);
    }

    #[test]
    fn abort_during_two_second_post_roll_drops_stream_without_normal_finalization() {
        assert_abort_bypasses_normal_finalization(true);
    }

    #[test]
    fn direct_hotkey_dispatch_sends_start_before_returning_ticket() {
        let calls = Arc::new(AtomicUsize::new(0));
        let controller = controller_with_counter(Arc::clone(&calls));
        let handle = controller.handle();
        handle
            .reconfigure_hotkey(
                true,
                CaptureHotkeyMode::HoldToTalk,
                30,
                None,
                CaptureOptions::default(),
            )
            .unwrap();
        let HotkeyDispatch::Start(ticket) = handle.dispatch_hotkey(true, Instant::now()) else {
            panic!("press should dispatch start");
        };
        assert_eq!(handle.owner(), Some(AudioOwnerKind::Capture));
        assert!(ticket.capture_id.0 > 0);
        for _ in 0..100 {
            if calls.load(Ordering::Relaxed) == 1 {
                break;
            }
            thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        assert!(matches!(
            handle.dispatch_hotkey(false, Instant::now()),
            HotkeyDispatch::Stop { capture_id } if capture_id == ticket.capture_id
        ));
        let failed = (0..100).find_map(|_| {
            let event = controller.poll_events().into_iter().find(|event| {
                matches!(
                    event,
                    CaptureLifecycleEvent::Failed {
                        capture_id,
                        error: CaptureError::StartupCancelled,
                        ..
                    } if *capture_id == ticket.capture_id
                )
            });
            if event.is_none() {
                thread::sleep(Duration::from_millis(1));
            }
            event
        });
        assert!(failed.is_some(), "release must cancel startup before ready");
    }

    fn disabled_reconfiguration_still_dispatches_stop(mode: CaptureHotkeyMode) {
        let calls = Arc::new(AtomicUsize::new(0));
        let controller = controller_with_counter(Arc::clone(&calls));
        let handle = controller.handle();
        handle
            .reconfigure_hotkey(true, mode, 30, None, CaptureOptions::default())
            .unwrap();
        let HotkeyDispatch::Start(ticket) = handle.dispatch_hotkey(true, Instant::now()) else {
            panic!("initial press should dispatch start");
        };
        handle.adopt_hotkey_capture(&ticket).unwrap();
        for _ in 0..100 {
            if calls.load(Ordering::Acquire) == 1 {
                break;
            }
            thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(calls.load(Ordering::Acquire), 1);

        handle
            .reconfigure_hotkey(false, mode, 30, None, CaptureOptions::default())
            .unwrap();
        let stop_pressed = matches!(mode, CaptureHotkeyMode::Toggle);
        assert!(matches!(
            handle.dispatch_hotkey(stop_pressed, Instant::now()),
            HotkeyDispatch::Stop { capture_id } if capture_id == ticket.capture_id
        ));
        let failed = (0..100).find_map(|_| {
            let event = controller.poll_events().into_iter().find(|event| {
                matches!(
                    event,
                    CaptureLifecycleEvent::Failed {
                        capture_id,
                        error: CaptureError::StartupCancelled,
                        ..
                    } if *capture_id == ticket.capture_id
                )
            });
            if event.is_none() {
                thread::sleep(Duration::from_millis(1));
            }
            event
        });
        assert!(
            failed.is_some(),
            "disabled reconfiguration must not strand capture"
        );
    }

    #[test]
    fn disabled_reconfiguration_keeps_hold_release_stop_eligible() {
        disabled_reconfiguration_still_dispatches_stop(CaptureHotkeyMode::HoldToTalk);
    }

    #[test]
    fn disabled_reconfiguration_keeps_toggle_stop_eligible() {
        disabled_reconfiguration_still_dispatches_stop(CaptureHotkeyMode::Toggle);
    }

    #[test]
    fn stale_unadopted_ticket_is_revoked_before_worker_can_accept_audio() {
        let (entered_tx, entered_rx) = unbounded();
        let (cancelled_tx, cancelled_rx) = unbounded();
        let (retire_tx, retire_rx) = unbounded();
        let accepted_audio = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let worker_accepted_audio = Arc::clone(&accepted_audio);
        let controller =
            CaptureController::with_start_capture(Arc::new(move |_request, cancellation| {
                entered_tx.send(()).unwrap();
                while !cancellation.is_cancelled() {
                    thread::sleep(Duration::from_millis(1));
                }
                cancelled_tx.send(()).unwrap();
                retire_rx.recv().unwrap();
                if !cancellation.is_cancelled() {
                    worker_accepted_audio.store(true, Ordering::Release);
                    return Ok(RecordingSession::simulated(
                        None,
                        CaptureStopReason::Explicit,
                    ));
                }
                Err(CaptureError::StartupCancelled)
            }))
            .unwrap();
        let handle = controller.handle();
        handle
            .reconfigure_hotkey(
                true,
                CaptureHotkeyMode::HoldToTalk,
                30,
                None,
                CaptureOptions::default(),
            )
            .unwrap();
        let HotkeyDispatch::Start(ticket) = handle.dispatch_hotkey(true, Instant::now()) else {
            panic!("press should issue an unadopted ticket");
        };
        entered_rx.recv_timeout(Duration::from_secs(1)).unwrap();

        handle
            .reconfigure_hotkey(
                false,
                CaptureHotkeyMode::HoldToTalk,
                30,
                None,
                CaptureOptions::default(),
            )
            .unwrap();
        cancelled_rx.recv_timeout(Duration::from_secs(1)).unwrap();

        assert!(handle.adopt_hotkey_capture(&ticket).is_err());
        assert_eq!(handle.owner(), Some(AudioOwnerKind::Capture));
        assert!(matches!(
            handle.reserve_owner(AudioOwnerKind::Playback),
            Err(CaptureControlError::Owned(AudioOwnerKind::Capture))
        ));
        assert!(!accepted_audio.load(Ordering::Acquire));

        retire_tx.send(()).unwrap();
        for _ in 0..100 {
            if handle.owner().is_none() {
                break;
            }
            thread::sleep(Duration::from_millis(1));
        }
        assert!(handle.owner().is_none());
        assert!(!accepted_audio.load(Ordering::Acquire));
        let playback = handle.reserve_owner(AudioOwnerKind::Playback).unwrap();
        handle.release(playback.id).unwrap();
        assert!(controller.poll_events().into_iter().all(|event| !matches!(
            event,
            CaptureLifecycleEvent::Ready { capture_id, .. }
                if capture_id == ticket.capture_id
        )));
    }

    #[test]
    fn stale_capture_id_cannot_stop_new_owner() {
        let controller = CaptureController::with_start_capture(Arc::new(|_request, _| {
            Ok(RecordingSession::simulated(
                None,
                CaptureStopReason::Explicit,
            ))
        }))
        .unwrap();
        let handle = controller.handle();
        let first = handle
            .start_capture(
                AudioOwnerKind::Capture,
                Instant::now(),
                30,
                None,
                CaptureOptions::default(),
            )
            .unwrap();
        handle.abort(first.capture_id).unwrap();
        handle.release(first.capture_id.0).unwrap();
        assert!(matches!(
            handle.start_capture(
                AudioOwnerKind::Capture,
                Instant::now(),
                30,
                None,
                CaptureOptions::default(),
            ),
            Err(CaptureControlError::Owned(AudioOwnerKind::Capture))
        ));
        for _ in 0..100 {
            if handle.owner().is_none() {
                break;
            }
            thread::sleep(Duration::from_millis(1));
        }
        assert!(handle.owner().is_none());
        let second = handle
            .start_capture(
                AudioOwnerKind::Capture,
                Instant::now(),
                30,
                None,
                CaptureOptions::default(),
            )
            .unwrap();

        assert_eq!(
            handle.stop(first.capture_id),
            Err(CaptureControlError::Stale(first.capture_id.0))
        );
        assert_eq!(handle.owner(), Some(AudioOwnerKind::Capture));
        handle.abort(second.capture_id).unwrap();
    }

    #[test]
    fn one_owner_blocks_capture_test_and_playback_overlap() {
        let calls = Arc::new(AtomicUsize::new(0));
        let controller = controller_with_counter(calls);
        let handle = controller.handle();
        let capture = handle
            .start_capture(
                AudioOwnerKind::Capture,
                Instant::now(),
                30,
                None,
                CaptureOptions::default(),
            )
            .unwrap();

        assert!(matches!(
            handle.start_capture(
                AudioOwnerKind::MicrophoneTest,
                Instant::now(),
                30,
                None,
                CaptureOptions::default(),
            ),
            Err(CaptureControlError::Owned(AudioOwnerKind::Capture))
        ));
        assert!(matches!(
            handle.reserve_owner(AudioOwnerKind::Playback),
            Err(CaptureControlError::Owned(AudioOwnerKind::Capture))
        ));
        handle.abort(capture.capture_id).unwrap();
    }

    #[test]
    fn release_before_start_result_admission_emits_terminal_release() {
        let (entered_tx, entered_rx) = unbounded();
        let (continue_tx, continue_rx) = unbounded();
        let (probe_tx, probe_rx) = unbounded();
        let controller = CaptureController::with_start_capture(Arc::new(move |_request, _| {
            entered_tx.send(()).unwrap();
            continue_rx.recv().unwrap();
            let (session, probe) = RecordingSession::simulated_with_abort_probe(
                None,
                CaptureStopReason::Explicit,
                Duration::from_secs(2),
            );
            probe_tx.send(probe).unwrap();
            Ok(session)
        }))
        .unwrap();
        let handle = controller.handle();
        let ticket = handle
            .start_capture(
                AudioOwnerKind::Capture,
                Instant::now(),
                30,
                None,
                CaptureOptions::default(),
            )
            .unwrap();
        entered_rx.recv_timeout(Duration::from_secs(1)).unwrap();

        handle.abort(ticket.capture_id).unwrap();
        handle.release(ticket.capture_id.0).unwrap();
        continue_tx.send(()).unwrap();
        let probe = probe_rx.recv_timeout(Duration::from_secs(1)).unwrap();

        let released = (0..200).find_map(|_| {
            let event = controller.poll_events().into_iter().find(|event| {
                matches!(
                    event,
                    CaptureLifecycleEvent::Released { capture_id, .. }
                        if *capture_id == ticket.capture_id
                )
            });
            if event.is_none() {
                thread::sleep(Duration::from_millis(1));
            }
            event
        });
        assert!(released.is_some());
        assert!(handle.owner().is_none());
        assert!(probe.stream_dropped.load(Ordering::Acquire));
        assert!(probe.preview_invalidated.load(Ordering::Acquire));
        assert!(!probe.finish_called.load(Ordering::Acquire));
        assert!(!probe.terminal_preview_called.load(Ordering::Acquire));
        assert!(controller.poll_events().into_iter().all(|event| !matches!(
            event,
            CaptureLifecycleEvent::Ready { capture_id, .. }
                if capture_id == ticket.capture_id
        )));
    }

    #[test]
    fn panicking_start_worker_emits_failure_and_releases_owner() {
        let controller = CaptureController::with_start_capture(Arc::new(|_, _| {
            panic!("injected start panic");
        }))
        .unwrap();
        let handle = controller.handle();
        let ticket = handle
            .start_capture(
                AudioOwnerKind::Capture,
                Instant::now(),
                30,
                None,
                CaptureOptions::default(),
            )
            .unwrap();

        let failed = (0..100).find_map(|_| {
            let event = controller.poll_events().into_iter().find(|event| {
                matches!(
                    event,
                    CaptureLifecycleEvent::Failed {
                        capture_id,
                        error: CaptureError::WorkerPanic(message),
                        ..
                    } if *capture_id == ticket.capture_id && message == "injected start panic"
                )
            });
            if event.is_none() {
                thread::sleep(Duration::from_millis(1));
            }
            event
        });
        assert!(failed.is_some());
        assert!(handle.owner().is_none());
    }

    #[test]
    fn release_reaper_spawn_failure_fails_closed_without_leaking_owner() {
        let controller = CaptureController::with_reaper_spawner_for_test(
            Arc::new(|_, _| {
                Ok(RecordingSession::simulated_with_stop_delay(
                    None,
                    CaptureStopReason::Explicit,
                    Duration::from_millis(25),
                ))
            }),
            Arc::new(|_, _| Err("injected spawn failure".to_owned())),
        )
        .unwrap();
        let handle = controller.handle();
        let ticket = handle
            .start_capture(
                AudioOwnerKind::Capture,
                Instant::now(),
                30,
                None,
                CaptureOptions::default(),
            )
            .unwrap();
        let ready = (0..100).find_map(|_| {
            let event = controller.poll_events().into_iter().find(|event| {
                matches!(
                    event,
                    CaptureLifecycleEvent::Ready { capture_id, .. }
                        if *capture_id == ticket.capture_id
                )
            });
            if event.is_none() {
                thread::sleep(Duration::from_millis(1));
            }
            event
        });
        assert!(ready.is_some());

        handle.abort(ticket.capture_id).unwrap();
        handle.release(ticket.capture_id.0).unwrap();

        assert!(handle.owner().is_none());
        assert!(matches!(
            handle.start_capture(
                AudioOwnerKind::Capture,
                Instant::now(),
                30,
                None,
                CaptureOptions::default(),
            ),
            Err(CaptureControlError::Shutdown)
        ));
        assert!(controller.poll_events().into_iter().any(|event| {
            matches!(
                event,
                CaptureLifecycleEvent::Failed {
                    capture_id,
                    error: CaptureError::WorkerSpawn(message),
                    ..
                } if capture_id == ticket.capture_id && message.contains("injected spawn failure")
            )
        }));
    }
}
