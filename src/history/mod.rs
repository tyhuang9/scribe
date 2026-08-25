//! Durable, runtime-neutral transcription history.

mod audio;
mod database;

use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crossbeam_channel::{Receiver, Sender, bounded};
use thiserror::Error;

use crate::prepared_audio::PreparedAudio;

const DEFAULT_CHANNEL_CAPACITY: usize = 64;
const MAX_PAGE_SIZE: usize = 100;
pub(crate) const MAX_HISTORY_AUDIO_FRAMES: usize = 16_000 * 600;
// Worker startup includes private-directory hardening, SQLite recovery, and
// WAL reconciliation. Hosted Windows runners can delay that work long enough
// to exceed a short scheduler-sensitive deadline without a worker failure.
const HISTORY_STARTUP_TIMEOUT: Duration = Duration::from_secs(10);
const HISTORY_SEND_TIMEOUT: Duration = Duration::from_millis(100);
const HISTORY_REPLY_TIMEOUT: Duration = Duration::from_millis(1_500);
// Keep the process lock held until the worker has had a realistic chance to
// close SQLite and release its handle. This prevents a store dropped by one
// caller from spuriously blocking an immediate, safe reopen by the next one.
const HISTORY_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(3);
static LAST_HISTORY_ID: AtomicI64 = AtomicI64::new(0);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum HistoryStatus {
    Pending,
    Completed,
    Failed,
}

impl HistoryStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Completed => "completed",
            Self::Failed => "failed",
        }
    }

    fn parse(value: &str) -> Result<Self, HistoryError> {
        match value {
            "pending" => Ok(Self::Pending),
            "completed" => Ok(Self::Completed),
            "failed" => Ok(Self::Failed),
            other => Err(HistoryError::Corrupt(format!(
                "unknown history status {other:?}"
            ))),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct HistoryMetrics {
    pub audio_duration_ms: Option<u64>,
    pub processing_duration_ms: Option<u64>,
    pub realtime_factor: Option<f64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct HistoryRetentionPolicy {
    pub max_unpinned_entries: u32,
    pub transcript_retention_days: Option<u32>,
    pub audio_retention_days: Option<u32>,
}

impl Default for HistoryRetentionPolicy {
    fn default() -> Self {
        Self {
            max_unpinned_entries: 20,
            transcript_retention_days: None,
            audio_retention_days: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct HistoryRecord {
    pub id: i64,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub completed_at_ms: Option<i64>,
    pub status: HistoryStatus,
    pub raw_text: String,
    pub final_text: Option<String>,
    pub model_id: String,
    pub metrics: HistoryMetrics,
    pub pinned: bool,
    pub source_app: Option<String>,
    pub audio_path: Option<PathBuf>,
    pub failure: Option<String>,
    pub retry_count: u32,
    pub output_outcome: Option<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct NewHistoryEntry {
    pub raw_text: String,
    pub model_id: String,
    pub source_app: Option<String>,
    pub metrics: HistoryMetrics,
}

#[derive(Clone, Debug)]
pub(crate) struct CompletedHistoryEntry {
    pub raw_text: String,
    pub final_text: String,
    pub metrics: HistoryMetrics,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct HistoryCursor {
    pub created_at_ms: i64,
    pub id: i64,
}

#[derive(Clone, Debug)]
pub(crate) struct HistoryQuery {
    pub text: Option<String>,
    pub status: Option<HistoryStatus>,
    pub pinned: Option<bool>,
    pub before: Option<HistoryCursor>,
    pub limit: usize,
}

impl Default for HistoryQuery {
    fn default() -> Self {
        Self {
            text: None,
            status: None,
            pinned: None,
            before: None,
            limit: 50,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct HistoryPage {
    pub records: Vec<HistoryRecord>,
    pub next: Option<HistoryCursor>,
}

#[derive(Debug)]
pub(crate) struct RetryHistoryEntry {
    pub record: HistoryRecord,
    pub audio: PreparedAudio,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct RetryReleaseAcknowledgement {
    /// Lease removal is complete whenever this acknowledgement is received.
    /// Retention is deliberately reported separately because it runs after
    /// removal and cannot revoke the acknowledgement.
    pub retention_error: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct ReconciliationReport {
    pub interrupted_pending_failed: usize,
    pub deletions_completed: usize,
    pub missing_audio_cleared: usize,
    pub orphan_audio_removed: usize,
    pub temporary_audio_removed: usize,
}

#[derive(Debug, Error)]
pub(crate) enum HistoryError {
    #[error("history worker is unavailable")]
    WorkerUnavailable,
    #[error("history worker stopped unexpectedly")]
    WorkerStopped,
    #[error("history worker did not complete the operation before its deadline")]
    WorkerTimedOut,
    #[error("history storage is already open by another Scribe process")]
    AlreadyOpen,
    #[error("history record {0} was not found")]
    NotFound(i64),
    #[error("invalid history lifecycle transition: {0}")]
    InvalidTransition(String),
    #[error("invalid history input: {0}")]
    InvalidInput(String),
    #[error("unsafe history audio path: {0}")]
    UnsafePath(String),
    #[error("corrupt history data: {0}")]
    Corrupt(String),
    #[error("history database error: {0}")]
    Database(#[from] rusqlite::Error),
    #[error("history filesystem error: {0}")]
    Io(#[from] std::io::Error),
    #[error("history audio error: {0}")]
    Audio(String),
}

type HistoryResult<T> = Result<T, HistoryError>;

enum Command {
    Create {
        id: i64,
        entry: NewHistoryEntry,
        audio: Option<Arc<PreparedAudio>>,
        reply: Sender<HistoryResult<HistoryRecord>>,
    },
    Complete {
        id: i64,
        entry: CompletedHistoryEntry,
        reply: Sender<HistoryResult<HistoryRecord>>,
    },
    CompleteRetry {
        id: i64,
        entry: CompletedHistoryEntry,
        reply: Sender<HistoryResult<HistoryRecord>>,
    },
    Fail {
        id: i64,
        failure: String,
        reply: Sender<HistoryResult<HistoryRecord>>,
    },
    FailRetry {
        id: i64,
        failure: String,
        reply: Sender<HistoryResult<HistoryRecord>>,
    },
    ReleaseRetry {
        id: i64,
        reply: Sender<RetryReleaseAcknowledgement>,
    },
    Retry {
        id: i64,
        reply: Sender<HistoryResult<RetryHistoryEntry>>,
    },
    Search {
        query: HistoryQuery,
        reply: Sender<HistoryResult<HistoryPage>>,
    },
    Pin {
        id: i64,
        pinned: bool,
        reply: Sender<HistoryResult<HistoryRecord>>,
    },
    DeleteAudio {
        id: i64,
        reply: Sender<HistoryResult<HistoryRecord>>,
    },
    Delete {
        id: i64,
        reply: Sender<HistoryResult<()>>,
    },
    AudioPath {
        id: i64,
        reply: Sender<HistoryResult<Option<PathBuf>>>,
    },
    SetRetentionPolicy {
        policy: HistoryRetentionPolicy,
        reply: Sender<HistoryResult<()>>,
    },
    RecordOutputOutcome {
        id: i64,
        outcome: String,
        reply: Sender<HistoryResult<HistoryRecord>>,
    },
    Shutdown,
}

/// Synchronous application handle backed by one bounded, dedicated SQLite worker.
#[derive(Clone)]
pub(crate) struct HistoryStore {
    inner: Arc<HistoryWorker>,
    startup_reconciliation: ReconciliationReport,
}

struct HistoryWorker {
    sender: Sender<Command>,
    worker: Mutex<Option<JoinHandle<()>>>,
}

struct HistoryProcessLock {
    _file: File,
}

impl HistoryStore {
    pub(crate) fn open(
        history_root: impl AsRef<Path>,
        retention_policy: HistoryRetentionPolicy,
    ) -> HistoryResult<Self> {
        Self::open_with_capacity(
            history_root.as_ref().to_path_buf(),
            retention_policy,
            DEFAULT_CHANNEL_CAPACITY,
        )
    }

    fn open_with_capacity(
        history_root: PathBuf,
        retention_policy: HistoryRetentionPolicy,
        channel_capacity: usize,
    ) -> HistoryResult<Self> {
        if channel_capacity == 0 {
            return Err(HistoryError::InvalidInput(
                "history channel capacity must be non-zero".into(),
            ));
        }
        audio::initialize_root(&history_root)?;
        let process_lock = HistoryProcessLock::acquire(&history_root)?;
        let (sender, receiver) = bounded(channel_capacity);
        let (ready_tx, ready_rx) = bounded(1);
        let worker = thread::Builder::new()
            .name("scribe-history".into())
            .spawn(move || {
                // The lock belongs to the worker so even a caller-side startup
                // timeout cannot release it while reconciliation is running.
                let _process_lock = process_lock;
                database::run_worker(history_root, retention_policy, receiver, ready_tx)
            })?;
        let startup_reconciliation = match ready_rx.recv_timeout(HISTORY_STARTUP_TIMEOUT) {
            Ok(Ok(report)) => report,
            Ok(Err(error)) => {
                let _ = worker.join();
                return Err(error);
            }
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                return Err(HistoryError::WorkerTimedOut);
            }
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                let _ = worker.join();
                return Err(HistoryError::WorkerStopped);
            }
        };
        Ok(Self {
            inner: Arc::new(HistoryWorker {
                sender,
                worker: Mutex::new(Some(worker)),
            }),
            startup_reconciliation,
        })
    }

    pub(crate) fn startup_reconciliation(&self) -> ReconciliationReport {
        self.startup_reconciliation
    }

    #[cfg(test)]
    pub(crate) fn create_pending(
        &self,
        entry: NewHistoryEntry,
        audio: Option<&PreparedAudio>,
    ) -> HistoryResult<HistoryRecord> {
        self.create_pending_with_id(Self::reserve_id(), entry, audio)
    }

    /// Reserves a process-unique identifier before persistence starts so a
    /// timed-out caller can still issue ordered terminal cleanup.
    pub(crate) fn reserve_id() -> i64 {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
            .min(i64::MAX as u128) as i64;
        let mut observed = LAST_HISTORY_ID.load(Ordering::Relaxed);
        loop {
            let next = stamp.max(observed.saturating_add(1)).max(1);
            match LAST_HISTORY_ID.compare_exchange_weak(
                observed,
                next,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return next,
                Err(actual) => observed = actual,
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn create_pending_with_id(
        &self,
        id: i64,
        entry: NewHistoryEntry,
        audio: Option<&PreparedAudio>,
    ) -> HistoryResult<HistoryRecord> {
        self.request(|reply| Command::Create {
            id,
            entry,
            audio: audio.cloned().map(Arc::new),
            reply,
        })
    }

    /// Queues creation without waiting for disk or SQLite completion. The
    /// returned receiver is optional evidence for diagnostics; later commands
    /// remain ordered behind this create on the single worker queue.
    pub(crate) fn enqueue_pending(
        &self,
        id: i64,
        entry: NewHistoryEntry,
        audio: Option<Arc<PreparedAudio>>,
    ) -> HistoryResult<Receiver<HistoryResult<HistoryRecord>>> {
        self.enqueue(|reply| Command::Create {
            id,
            entry,
            audio,
            reply,
        })
    }

    #[cfg(test)]
    pub(crate) fn complete(
        &self,
        id: i64,
        entry: CompletedHistoryEntry,
    ) -> HistoryResult<HistoryRecord> {
        self.request(|reply| Command::Complete { id, entry, reply })
    }

    pub(crate) fn enqueue_complete(
        &self,
        id: i64,
        entry: CompletedHistoryEntry,
    ) -> HistoryResult<Receiver<HistoryResult<HistoryRecord>>> {
        self.enqueue(|reply| Command::Complete { id, entry, reply })
    }

    #[cfg(test)]
    pub(crate) fn complete_retry(
        &self,
        id: i64,
        entry: CompletedHistoryEntry,
    ) -> HistoryResult<HistoryRecord> {
        self.request(|reply| Command::CompleteRetry { id, entry, reply })
    }

    pub(crate) fn enqueue_complete_retry(
        &self,
        id: i64,
        entry: CompletedHistoryEntry,
    ) -> HistoryResult<Receiver<HistoryResult<HistoryRecord>>> {
        self.enqueue(|reply| Command::CompleteRetry { id, entry, reply })
    }

    pub(crate) fn fail(&self, id: i64, failure: impl Into<String>) -> HistoryResult<HistoryRecord> {
        self.request(|reply| Command::Fail {
            id,
            failure: failure.into(),
            reply,
        })
    }

    pub(crate) fn fail_retry(
        &self,
        id: i64,
        failure: impl Into<String>,
    ) -> HistoryResult<HistoryRecord> {
        self.request(|reply| Command::FailRetry {
            id,
            failure: failure.into(),
            reply,
        })
    }

    /// Explicitly relinquishes a retry lease when a caller cannot enqueue or
    /// finish the corresponding terminal mutation. This operation is
    /// idempotent because a timed-out terminal request may already have
    /// released the worker-side lease before this command is processed.
    #[cfg(test)]
    pub(crate) fn release_retry(&self, id: i64) -> HistoryResult<RetryReleaseAcknowledgement> {
        let reply = self.enqueue_release_retry(id)?;
        reply
            .recv_timeout(HISTORY_REPLY_TIMEOUT)
            .map_err(|error| match error {
                crossbeam_channel::RecvTimeoutError::Timeout => HistoryError::WorkerTimedOut,
                crossbeam_channel::RecvTimeoutError::Disconnected => HistoryError::WorkerStopped,
            })
    }

    pub(crate) fn enqueue_release_retry(
        &self,
        id: i64,
    ) -> HistoryResult<Receiver<RetryReleaseAcknowledgement>> {
        let (reply, receiver) = bounded(1);
        self.inner
            .sender
            .send_timeout(Command::ReleaseRetry { id, reply }, HISTORY_SEND_TIMEOUT)
            .map_err(|error| match error {
                crossbeam_channel::SendTimeoutError::Timeout(_) => HistoryError::WorkerTimedOut,
                crossbeam_channel::SendTimeoutError::Disconnected(_) => {
                    HistoryError::WorkerUnavailable
                }
            })?;
        Ok(receiver)
    }

    pub(crate) fn retry(&self, id: i64) -> HistoryResult<RetryHistoryEntry> {
        self.request(|reply| Command::Retry { id, reply })
    }

    pub(crate) fn search(&self, mut query: HistoryQuery) -> HistoryResult<HistoryPage> {
        query.limit = query.limit.clamp(1, MAX_PAGE_SIZE);
        self.request(|reply| Command::Search { query, reply })
    }

    pub(crate) fn set_pinned(&self, id: i64, pinned: bool) -> HistoryResult<HistoryRecord> {
        self.request(|reply| Command::Pin { id, pinned, reply })
    }

    pub(crate) fn delete_audio(&self, id: i64) -> HistoryResult<HistoryRecord> {
        self.request(|reply| Command::DeleteAudio { id, reply })
    }

    pub(crate) fn delete(&self, id: i64) -> HistoryResult<()> {
        self.request(|reply| Command::Delete { id, reply })
    }

    /// Returns a validated contained path suitable for a caller-owned player.
    pub(crate) fn validated_audio_path(&self, id: i64) -> HistoryResult<Option<PathBuf>> {
        self.request(|reply| Command::AudioPath { id, reply })
    }

    pub(crate) fn set_retention_policy(&self, policy: HistoryRetentionPolicy) -> HistoryResult<()> {
        self.request(|reply| Command::SetRetentionPolicy { policy, reply })
    }

    /// Persists a caller-reported output result; it never performs output itself.
    pub(crate) fn record_output_outcome(
        &self,
        id: i64,
        outcome: impl Into<String>,
    ) -> HistoryResult<HistoryRecord> {
        self.request(|reply| Command::RecordOutputOutcome {
            id,
            outcome: outcome.into(),
            reply,
        })
    }

    fn request<T>(
        &self,
        command: impl FnOnce(Sender<HistoryResult<T>>) -> Command,
    ) -> HistoryResult<T> {
        let reply_rx = self.enqueue(command)?;
        reply_rx
            .recv_timeout(HISTORY_REPLY_TIMEOUT)
            .map_err(|error| match error {
                crossbeam_channel::RecvTimeoutError::Timeout => HistoryError::WorkerTimedOut,
                crossbeam_channel::RecvTimeoutError::Disconnected => HistoryError::WorkerStopped,
            })?
    }

    fn enqueue<T>(
        &self,
        command: impl FnOnce(Sender<HistoryResult<T>>) -> Command,
    ) -> HistoryResult<Receiver<HistoryResult<T>>> {
        let (reply_tx, reply_rx) = bounded(1);
        self.inner
            .sender
            .send_timeout(command(reply_tx), HISTORY_SEND_TIMEOUT)
            .map_err(|error| match error {
                crossbeam_channel::SendTimeoutError::Timeout(_) => HistoryError::WorkerTimedOut,
                crossbeam_channel::SendTimeoutError::Disconnected(_) => {
                    HistoryError::WorkerUnavailable
                }
            })?;
        Ok(reply_rx)
    }
}

impl HistoryProcessLock {
    fn acquire(root: &Path) -> HistoryResult<Self> {
        let path = root.join("history.lock");
        if audio::validate_regular_file_or_missing(&path)? {
            audio::secure_existing_file(&path)?;
        } else {
            let mut create = OpenOptions::new();
            create.read(true).write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                create.mode(0o600);
            }
            match create.open(&path) {
                Ok(file) => {
                    audio::secure_existing_file(&path)?;
                    drop(file);
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    audio::secure_existing_file(&path)?;
                }
                Err(error) => return Err(error.into()),
            }
        }
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            options.share_mode(0);
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options.open(path).map_err(|error| {
            if error.kind() == std::io::ErrorKind::PermissionDenied
                || error
                    .raw_os_error()
                    .is_some_and(|code| code == 32 || code == 33)
            {
                HistoryError::AlreadyOpen
            } else {
                HistoryError::Io(error)
            }
        })?;
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
                let error = std::io::Error::last_os_error();
                if error
                    .raw_os_error()
                    .is_some_and(|code| code == libc::EWOULDBLOCK || code == libc::EAGAIN)
                {
                    return Err(HistoryError::AlreadyOpen);
                }
                return Err(HistoryError::Io(error));
            }
        }
        Ok(Self { _file: file })
    }
}

pub(crate) fn load_retained_audio_file(path: &Path) -> HistoryResult<PreparedAudio> {
    let file = audio::open_no_follow_regular(path)?;
    audio::decode_prepared_audio(file)
}

#[cfg(unix)]
impl Drop for HistoryProcessLock {
    fn drop(&mut self) {
        use std::os::fd::AsRawFd;
        let _ = unsafe { libc::flock(self._file.as_raw_fd(), libc::LOCK_UN) };
    }
}

impl Drop for HistoryWorker {
    fn drop(&mut self) {
        let shutdown_sent = self
            .sender
            .send_timeout(Command::Shutdown, HISTORY_SEND_TIMEOUT)
            .is_ok();
        if let Ok(worker) = self.worker.get_mut()
            && let Some(worker) = worker.take()
        {
            if shutdown_sent {
                let deadline = std::time::Instant::now() + HISTORY_SHUTDOWN_TIMEOUT;
                while !worker.is_finished() && std::time::Instant::now() < deadline {
                    thread::sleep(Duration::from_millis(10));
                }
            }
            if worker.is_finished() {
                let _ = worker.join();
            }
        }
    }
}

fn recv_command(receiver: &Receiver<Command>) -> Option<Command> {
    receiver.recv().ok()
}

#[cfg(test)]
mod tests;
