use std::io;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

static CANCELLATION_GENERATION: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct CancellationSnapshot(u64);

#[derive(Default)]
struct ActiveRequestState {
    requests: usize,
}

#[derive(Debug)]
pub(crate) struct RegisteredRequest;

impl Drop for RegisteredRequest {
    fn drop(&mut self) {
        let (state, changed) = active_request_state();
        let mut state = state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.requests = state.requests.saturating_sub(1);
        changed.notify_all();
    }
}

fn active_request_state() -> &'static (Mutex<ActiveRequestState>, Condvar) {
    static STATE: OnceLock<(Mutex<ActiveRequestState>, Condvar)> = OnceLock::new();
    STATE.get_or_init(|| (Mutex::new(ActiveRequestState::default()), Condvar::new()))
}

#[cfg(test)]
pub(crate) fn cancellation_test_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

pub(crate) fn cancellation_snapshot() -> CancellationSnapshot {
    CancellationSnapshot(CANCELLATION_GENERATION.load(Ordering::Acquire))
}

fn cancellation_error() -> io::Error {
    io::Error::new(
        io::ErrorKind::Interrupted,
        "transcription request was cancelled",
    )
}

fn is_cancelled(snapshot: CancellationSnapshot) -> bool {
    CANCELLATION_GENERATION.load(Ordering::Acquire) != snapshot.0
}

pub(crate) fn register_cancellable_request(
    snapshot: CancellationSnapshot,
) -> io::Result<RegisteredRequest> {
    let (state, _) = active_request_state();
    let mut state = state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if is_cancelled(snapshot) {
        return Err(cancellation_error());
    }
    state.requests = state
        .requests
        .checked_add(1)
        .ok_or_else(|| io::Error::other("active transcription request count overflow"))?;
    Ok(RegisteredRequest)
}

pub(crate) fn cancel_active_requests() {
    CANCELLATION_GENERATION.fetch_add(1, Ordering::AcqRel);
}

pub(crate) fn cancel_active_requests_and_wait(timeout: Duration) -> bool {
    cancel_active_requests();
    let deadline = Instant::now() + timeout;
    let (state, changed) = active_request_state();
    let mut state = state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    while state.requests != 0 {
        let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
            return false;
        };
        let (next, wait) = changed
            .wait_timeout(state, remaining)
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state = next;
        if wait.timed_out() && state.requests != 0 {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancellation_before_registration_rejects_stale_request() {
        let _guard = cancellation_test_lock();
        let snapshot = cancellation_snapshot();
        cancel_active_requests();
        let error = register_cancellable_request(snapshot).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Interrupted);
    }

    #[test]
    fn cancellation_waits_for_registered_request() {
        let _guard = cancellation_test_lock();
        let snapshot = cancellation_snapshot();
        let registration = register_cancellable_request(snapshot).unwrap();
        let waiter = std::thread::spawn(|| cancel_active_requests_and_wait(Duration::from_secs(1)));
        std::thread::sleep(Duration::from_millis(10));
        drop(registration);
        assert!(waiter.join().unwrap());
    }
}
