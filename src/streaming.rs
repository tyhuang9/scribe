//! Runtime-neutral primitives for bounded incremental transcription.
//!
//! This module contains no application-shell code. It owns the bounded native
//! preview worker plus the data and deterministic rules shared by its callers.

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::prepared_audio::{PREPARED_SAMPLE_RATE, PreparedAudio};
use crate::transcription::{ModelId, RequestId, SessionId, StreamUpdate};

pub const DECODE_INTERVAL_MS: u64 = 250;
pub const ROLLING_WINDOW_MS: u64 = 3_000;
pub const BOUNDARY_OVERLAP_MS: u64 = 650;
pub const STABILITY_PASSES: usize = 2;
pub const STABILITY_HORIZON_MS: u64 = 700;
pub const MAX_COMPARISON_CONTEXT_WORDS: usize = 60;

const FRAMES_PER_MILLISECOND: u64 = PREPARED_SAMPLE_RATE as u64 / 1_000;
const MAX_WINDOW_FRAMES: u64 = ROLLING_WINDOW_MS * FRAMES_PER_MILLISECOND;
const OVERLAP_FRAMES: u64 = BOUNDARY_OVERLAP_MS * FRAMES_PER_MILLISECOND;
const HORIZON_FRAMES: u64 = STABILITY_HORIZON_MS * FRAMES_PER_MILLISECOND;
const DROP_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);

/// Correlation data carried by every preview request and response.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StreamIdentity {
    pub session_id: SessionId,
    pub request_id: RequestId,
    pub model_id: ModelId,
    /// Monotonically increasing within this request. A later result must never
    /// be allowed to overwrite a newer accepted result.
    pub sequence: u64,
}

/// A canonical native-audio snapshot for one rolling-window preview decode.
#[derive(Clone, Debug)]
pub struct PreviewSnapshot {
    pub identity: StreamIdentity,
    pub window_start_frame: u64,
    pub window_end_frame: u64,
    pub audio: Arc<PreparedAudio>,
}

/// Opaque producer handed directly to the native audio worker. The UI owns no
/// method that can read PCM from this type; it only transports the producer
/// from [`crate::transcription::TranscriptionService`] to capture startup.
#[derive(Clone, Debug)]
pub(crate) struct PreviewAudioPublisher {
    identity: StreamIdentity,
    next_sequence: Arc<AtomicU64>,
    mailbox: ReplaceLatestMailbox<PreviewSnapshot>,
}

impl PreviewAudioPublisher {
    fn new(identity: StreamIdentity, mailbox: ReplaceLatestMailbox<PreviewSnapshot>) -> Self {
        Self {
            identity,
            next_sequence: Arc::new(AtomicU64::new(1)),
            mailbox,
        }
    }

    /// Publishes one independently prepared canonical window without waiting
    /// for inference. The replace-latest mailbox retains at most one pending
    /// snapshot while another decode is active.
    pub(crate) fn publish_window(
        &self,
        window_start_frame: u64,
        samples: Vec<f32>,
    ) -> Result<bool, SnapshotError> {
        let window_end_frame = window_start_frame.saturating_add(samples.len() as u64);
        let sequence = self.next_sequence.fetch_add(1, Ordering::Relaxed);
        let mut identity = self.identity.clone();
        identity.sequence = sequence;
        let source_frames = samples.len();
        let audio = Arc::new(
            PreparedAudio::from_captured_mono(samples, PREPARED_SAMPLE_RATE, 1, source_frames)
                .map_err(|_| SnapshotError::InvalidPreparedAudio)?,
        );
        let snapshot = PreviewSnapshot::new(identity, window_start_frame, window_end_frame, audio)?;
        Ok(self.mailbox.publish(snapshot))
    }
}

impl PreviewSnapshot {
    pub fn new(
        identity: StreamIdentity,
        window_start_frame: u64,
        window_end_frame: u64,
        audio: Arc<PreparedAudio>,
    ) -> Result<Self, SnapshotError> {
        let snapshot = Self {
            identity,
            window_start_frame,
            window_end_frame,
            audio,
        };
        snapshot.validate()?;
        Ok(snapshot)
    }

    pub fn validate(&self) -> Result<(), SnapshotError> {
        if self.audio.sample_rate != PREPARED_SAMPLE_RATE {
            return Err(SnapshotError::NonCanonicalSampleRate {
                actual: self.audio.sample_rate,
            });
        }
        if self.window_end_frame <= self.window_start_frame {
            return Err(SnapshotError::EmptyWindow);
        }
        let span = self.window_end_frame - self.window_start_frame;
        if span > MAX_WINDOW_FRAMES {
            return Err(SnapshotError::WindowTooLong { frames: span });
        }
        if self.audio.samples.len() as u64 != span {
            return Err(SnapshotError::AudioLengthMismatch {
                frames: span,
                samples: self.audio.samples.len(),
            });
        }
        if self.audio.samples.iter().any(|sample| !sample.is_finite()) {
            return Err(SnapshotError::NonFiniteAudio);
        }
        if self
            .audio
            .samples
            .iter()
            .any(|sample| !(-1.0..=1.0).contains(sample))
        {
            return Err(SnapshotError::OutOfRangeAudio);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SnapshotError {
    NonCanonicalSampleRate { actual: u32 },
    EmptyWindow,
    WindowTooLong { frames: u64 },
    AudioLengthMismatch { frames: u64, samples: usize },
    NonFiniteAudio,
    OutOfRangeAudio,
    InvalidPreparedAudio,
}

impl fmt::Display for SnapshotError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NonCanonicalSampleRate { actual } => write!(
                f,
                "preview audio must use the canonical {PREPARED_SAMPLE_RATE} Hz rate, got {actual}"
            ),
            Self::EmptyWindow => f.write_str("preview window must contain at least one frame"),
            Self::WindowTooLong { frames } => write!(
                f,
                "preview window has {frames} frames, exceeding the {ROLLING_WINDOW_MS} ms limit"
            ),
            Self::AudioLengthMismatch { frames, samples } => write!(
                f,
                "preview window has {frames} frames but audio contains {samples} samples"
            ),
            Self::NonFiniteAudio => f.write_str("preview audio contains a non-finite sample"),
            Self::OutOfRangeAudio => {
                f.write_str("preview audio contains a sample outside [-1.0, 1.0]")
            }
            Self::InvalidPreparedAudio => {
                f.write_str("preview audio could not be represented as canonical prepared audio")
            }
        }
    }
}

impl std::error::Error for SnapshotError {}

/// A capacity-one, replace-latest mailbox. It has no worker thread: callers
/// own scheduling and may keep one claimed item active while one newer item is
/// pending. Publishing never waits for the consumer.
#[derive(Debug)]
pub struct ReplaceLatestMailbox<T> {
    state: Arc<MailboxState<T>>,
}

impl<T> Clone for ReplaceLatestMailbox<T> {
    fn clone(&self) -> Self {
        Self {
            state: Arc::clone(&self.state),
        }
    }
}

#[derive(Debug)]
struct MailboxState<T> {
    inner: Mutex<MailboxInner<T>>,
    wake: Condvar,
}

#[derive(Debug)]
struct MailboxInner<T> {
    pending: Option<T>,
    active: bool,
    closed: bool,
}

impl<T> Default for ReplaceLatestMailbox<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> ReplaceLatestMailbox<T> {
    pub fn new() -> Self {
        Self {
            state: Arc::new(MailboxState {
                inner: Mutex::new(MailboxInner {
                    pending: None,
                    active: false,
                    closed: false,
                }),
                wake: Condvar::new(),
            }),
        }
    }

    /// Replaces pending work. `false` means the mailbox was closed and the
    /// supplied item was dropped before it could be scheduled.
    pub fn publish(&self, item: T) -> bool {
        let mut inner = self.state.inner.lock().expect("mailbox lock poisoned");
        if inner.closed {
            return false;
        }
        inner.pending = Some(item);
        self.state.wake.notify_one();
        true
    }

    /// Claims the newest pending item only when no other item is active.
    pub fn try_claim(&self) -> Option<ActiveMailboxItem<T>> {
        let mut inner = self.state.inner.lock().expect("mailbox lock poisoned");
        if inner.active {
            return None;
        }
        let item = inner.pending.take()?;
        inner.active = true;
        Some(ActiveMailboxItem {
            state: Arc::clone(&self.state),
            item: Some(item),
        })
    }

    /// Waits until work is claimable or the mailbox closes. This does not spawn
    /// a thread and is intended for exactly one consumer loop.
    pub fn claim(&self) -> Option<ActiveMailboxItem<T>> {
        let mut inner = self.state.inner.lock().expect("mailbox lock poisoned");
        loop {
            if !inner.active
                && let Some(item) = inner.pending.take()
            {
                inner.active = true;
                return Some(ActiveMailboxItem {
                    state: Arc::clone(&self.state),
                    item: Some(item),
                });
            }
            if inner.closed {
                return None;
            }
            inner = self.state.wake.wait(inner).expect("mailbox lock poisoned");
        }
    }

    /// Closes the mailbox and drops pending work. A currently claimed item is
    /// left to its consumer, which can finish or observe its own cancellation.
    pub fn close(&self) {
        let mut inner = self.state.inner.lock().expect("mailbox lock poisoned");
        inner.closed = true;
        inner.pending = None;
        self.state.wake.notify_all();
    }

    #[cfg(test)]
    pub fn is_closed(&self) -> bool {
        self.state
            .inner
            .lock()
            .expect("mailbox lock poisoned")
            .closed
    }

    #[cfg(test)]
    fn counts(&self) -> (usize, usize) {
        let inner = self.state.inner.lock().expect("mailbox lock poisoned");
        (
            usize::from(inner.active),
            usize::from(inner.pending.is_some()),
        )
    }
}

/// A claimed mailbox item. Keeping this value alive records one active decode;
/// dropping it releases the consumer slot for the newest pending item.
#[derive(Debug)]
pub struct ActiveMailboxItem<T> {
    state: Arc<MailboxState<T>>,
    item: Option<T>,
}

impl<T> ActiveMailboxItem<T> {
    #[cfg(test)]
    pub fn item(&self) -> &T {
        self.item.as_ref().expect("active mailbox item consumed")
    }

    pub fn finish(mut self) -> T {
        let item = self.item.take().expect("active mailbox item consumed");
        self.release();
        item
    }

    fn release(&mut self) {
        let mut inner = self.state.inner.lock().expect("mailbox lock poisoned");
        if inner.active {
            inner.active = false;
            self.state.wake.notify_one();
        }
    }
}

impl<T> Drop for ActiveMailboxItem<T> {
    fn drop(&mut self) {
        if self.item.is_some() {
            self.release();
        }
    }
}

/// Text-only result of a preview decode. The application can attach presentation policy
/// at its boundary without the scheduler knowing any concrete decoder type.
#[derive(Debug)]
pub enum PreviewEvent<E> {
    Update {
        identity: StreamIdentity,
        update: StreamUpdate,
    },
    Error {
        identity: StreamIdentity,
        error: E,
    },
}

/// One bounded worker for rolling batch-preview decodes. It consumes exactly
/// one snapshot at a time and keeps only the newest pending snapshot while a
/// synchronous decoder closure is running.
pub struct RollingPreviewSession<E> {
    mailbox: ReplaceLatestMailbox<PreviewSnapshot>,
    updates: ReplaceLatestMailbox<PreviewEvent<E>>,
    worker: Option<JoinHandle<()>>,
    cancel_active: Option<Box<dyn FnOnce() + Send + 'static>>,
}

impl<E: Send + 'static> RollingPreviewSession<E> {
    #[cfg(test)]
    pub fn new<F>(decode: F) -> std::io::Result<Self>
    where
        F: FnMut(PreviewSnapshot) -> Result<StreamUpdate, E> + Send + 'static,
    {
        Self::new_with_cancel(decode, || {})
    }

    pub(crate) fn new_with_cancel<F, C>(mut decode: F, cancel_active: C) -> std::io::Result<Self>
    where
        F: FnMut(PreviewSnapshot) -> Result<StreamUpdate, E> + Send + 'static,
        C: FnOnce() + Send + 'static,
    {
        let mailbox: ReplaceLatestMailbox<PreviewSnapshot> = ReplaceLatestMailbox::new();
        let worker_mailbox = mailbox.clone();
        let updates: ReplaceLatestMailbox<PreviewEvent<E>> = ReplaceLatestMailbox::new();
        let worker_updates = updates.clone();
        let worker = thread::Builder::new()
            .name("scribe-rolling-preview".to_owned())
            .spawn(move || {
                while let Some(active) = worker_mailbox.claim() {
                    let snapshot = active.finish();
                    let identity = snapshot.identity.clone();
                    let event = match decode(snapshot) {
                        Ok(update) => PreviewEvent::Update { identity, update },
                        Err(error) => PreviewEvent::Error { identity, error },
                    };
                    // A slow presentation consumer must not stall decoding or retain an obsolete
                    // partial; the result mailbox replaces it with this result.
                    let _ = worker_updates.publish(event);
                }
            })?;
        Ok(Self {
            mailbox,
            updates,
            worker: Some(worker),
            cancel_active: Some(Box::new(cancel_active)),
        })
    }

    /// Valid snapshots are scheduled without waiting for the decoder.
    #[cfg(test)]
    pub fn try_update(&self, snapshot: PreviewSnapshot) -> bool {
        self.mailbox.publish(snapshot)
    }

    pub(crate) fn audio_publisher(
        &self,
        session_id: SessionId,
        request_id: RequestId,
        model_id: ModelId,
    ) -> PreviewAudioPublisher {
        PreviewAudioPublisher::new(
            StreamIdentity {
                session_id,
                request_id,
                model_id,
                sequence: 0,
            },
            self.mailbox.clone(),
        )
    }

    /// Prevents capture from scheduling another preview and drops the newest
    /// pending snapshot. A currently active decode is allowed to finish.
    pub(crate) fn close(&self) {
        self.mailbox.close();
    }

    /// Returns at most one text-only preview event and never blocks.
    pub fn try_next(&self) -> Option<PreviewEvent<E>> {
        self.updates.try_claim().map(ActiveMailboxItem::finish)
    }

    /// Reports whether the named preview worker has exited without waiting.
    pub fn is_finished(&self) -> bool {
        self.worker.as_ref().is_none_or(JoinHandle::is_finished)
    }

    /// Stops future work and waits no longer than `timeout`. A `false` result
    /// means native work still owns the decoder. A process-exit caller must use
    /// the hard-abort policy before allowing DLL teardown; normal callers may
    /// cancel the owner and call this again while retaining the handle.
    pub fn stop_and_join(&mut self, timeout: Duration) -> bool {
        self.mailbox.close();
        let Some(worker) = self.worker.as_ref() else {
            return true;
        };
        let deadline = Instant::now() + timeout;
        while !worker.is_finished() {
            if Instant::now() >= deadline {
                return false;
            }
            thread::sleep(Duration::from_millis(1));
        }
        self.worker
            .take()
            .expect("worker checked above")
            .join()
            .is_ok()
    }
}

impl<E> Drop for RollingPreviewSession<E> {
    fn drop(&mut self) {
        if self
            .worker
            .as_ref()
            .is_some_and(|worker| !worker.is_finished())
            && let Some(cancel_active) = self.cancel_active.take()
        {
            cancel_active();
        }
        self.mailbox.close();
        let deadline = Instant::now() + DROP_SHUTDOWN_TIMEOUT;
        while self
            .worker
            .as_ref()
            .is_some_and(|worker| !worker.is_finished())
        {
            if Instant::now() >= deadline {
                // Detaching permits DLL teardown to race a live native
                // decoder; joining forever can hang the desktop on exit.
                // Aborting is the only process-safe fallback once the bounded
                // cooperative path is exhausted: it skips Rust/DLL teardown
                // and cannot paste stale text. App-owned shutdown normally
                // consumes this handle first.
                eprintln!(
                    "native rolling-preview worker exceeded the shutdown deadline; aborting safely"
                );
                std::process::abort();
            }
            thread::sleep(Duration::from_millis(1));
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

/// A decoder hypothesis, whose words retain display spelling independently of
/// normalized comparison tokens. Word frames are absolute audio-frame offsets.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TranscriptHypothesis {
    pub identity: StreamIdentity,
    pub window_start_frame: u64,
    pub window_end_frame: u64,
    pub words: Vec<HypothesisWord>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HypothesisWord {
    pub display: String,
    pub start_frame: Option<u64>,
    pub end_frame: Option<u64>,
}

impl HypothesisWord {
    pub fn new(display: impl Into<String>) -> Self {
        Self {
            display: display.into(),
            start_frame: None,
            end_frame: None,
        }
    }

    pub fn at_absolute_frames(mut self, start_frame: u64, end_frame: u64) -> Self {
        self.start_frame = Some(start_frame);
        self.end_frame = Some(end_frame);
        self
    }

    #[cfg(test)]
    pub fn at_relative_frames(
        self,
        window_start_frame: u64,
        start_frame: u64,
        end_frame: u64,
    ) -> Self {
        self.at_absolute_frames(
            window_start_frame.saturating_add(start_frame),
            window_start_frame.saturating_add(end_frame),
        )
    }
}

impl TranscriptHypothesis {
    pub fn from_text(
        identity: StreamIdentity,
        window_start_frame: u64,
        window_end_frame: u64,
        text: impl AsRef<str>,
    ) -> Self {
        Self {
            identity,
            window_start_frame,
            window_end_frame,
            words: text
                .as_ref()
                .split_whitespace()
                .map(HypothesisWord::new)
                .collect(),
        }
    }
}

/// Presentation data with immutable committed text and a revisable tentative
/// suffix. This is intentionally not tied to any application type.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TranscriptState {
    pub committed: String,
    pub tentative: String,
    pub revision: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum UpdateRejection {
    WrongSession,
    WrongRequest,
    WrongModel,
    StaleSequence { last_accepted: u64, received: u64 },
    InvalidWindow,
}

impl fmt::Display for UpdateRejection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WrongSession => f.write_str("stream update belongs to a different session"),
            Self::WrongRequest => f.write_str("stream update belongs to a different request"),
            Self::WrongModel => f.write_str("stream update belongs to a different model"),
            Self::StaleSequence {
                last_accepted,
                received,
            } => write!(
                f,
                "stream update sequence {received} is not newer than {last_accepted}"
            ),
            Self::InvalidWindow => f.write_str("stream update has an invalid audio window"),
        }
    }
}

impl std::error::Error for UpdateRejection {}

/// Stable-prefix transcript reconciliation for rolling or native incremental
/// decoders. It deliberately never mutates `committed` during partial updates.
#[derive(Clone, Debug)]
pub struct TranscriptStabilizer {
    expected: StreamIdentity,
    last_sequence: Option<u64>,
    committed: Vec<HypothesisWord>,
    previous: Option<Vec<HypothesisWord>>,
    state: TranscriptState,
}

impl TranscriptStabilizer {
    pub fn new(session_id: SessionId, request_id: RequestId, model_id: ModelId) -> Self {
        Self {
            expected: StreamIdentity {
                session_id,
                request_id,
                model_id,
                sequence: 0,
            },
            last_sequence: None,
            committed: Vec::new(),
            previous: None,
            state: TranscriptState::default(),
        }
    }

    pub fn push(
        &mut self,
        hypothesis: TranscriptHypothesis,
    ) -> Result<TranscriptState, UpdateRejection> {
        self.validate_identity(&hypothesis.identity)?;
        if hypothesis.window_end_frame <= hypothesis.window_start_frame {
            return Err(UpdateRejection::InvalidWindow);
        }
        self.last_sequence = Some(hypothesis.identity.sequence);

        if hypothesis.words.is_empty() {
            // A decoder dropout must not erase either the immutable prefix or
            // the previous tentative display while the utterance continues.
            return Ok(self.state.clone());
        }

        let window_start_frame = hypothesis.window_start_frame;
        let window_end_frame = hypothesis.window_end_frame;
        let mut current = hypothesis.words;
        let full_committed_prefix = has_full_committed_prefix(&self.committed, &current);
        let committed_overlap = remove_committed_overlap(&self.committed, &mut current);
        if current.is_empty() {
            self.previous = Some(current);
            self.refresh_tentative(&[]);
            return Ok(self.state.clone());
        }

        let stable_count = if STABILITY_PASSES == 2 {
            self.previous
                .as_deref()
                .map(|previous| compatible_prefix(previous, &current))
                .unwrap_or_default()
        } else {
            0
        };
        let cutoff = window_end_frame.saturating_sub(HORIZON_FRAMES);
        let boundary = window_start_frame.saturating_add(OVERLAP_FRAMES);
        let mut commit_count: usize = 0;
        let original_prefix_words = if full_committed_prefix {
            committed_overlap
        } else {
            0
        };
        let current_word_count = current.len().saturating_add(original_prefix_words);
        for word in current.iter().take(stable_count) {
            let (start, end) = effective_frames(
                word,
                window_start_frame,
                window_end_frame,
                commit_count.saturating_add(original_prefix_words),
                current_word_count,
            );
            if (committed_overlap > 0 && start < boundary)
                || end > cutoff
                || !display_is_stable(word, self.previous.as_deref(), commit_count)
            {
                break;
            }
            commit_count += 1;
        }

        if commit_count > 0 {
            self.committed.extend(current.drain(..commit_count));
            self.state.committed = render_words(&self.committed);
        }
        self.previous = Some(current.clone());
        self.refresh_tentative(&current);
        self.state.revision = self.state.revision.saturating_add(1);
        Ok(self.state.clone())
    }

    fn validate_identity(&self, identity: &StreamIdentity) -> Result<(), UpdateRejection> {
        if identity.session_id != self.expected.session_id {
            return Err(UpdateRejection::WrongSession);
        }
        if identity.request_id != self.expected.request_id {
            return Err(UpdateRejection::WrongRequest);
        }
        if identity.model_id != self.expected.model_id {
            return Err(UpdateRejection::WrongModel);
        }
        if let Some(last_accepted) = self.last_sequence
            && identity.sequence <= last_accepted
        {
            return Err(UpdateRejection::StaleSequence {
                last_accepted,
                received: identity.sequence,
            });
        }
        Ok(())
    }

    fn refresh_tentative(&mut self, words: &[HypothesisWord]) {
        self.state.tentative = render_words(words);
    }
}

fn display_is_stable(
    current: &HypothesisWord,
    previous: Option<&[HypothesisWord]>,
    index: usize,
) -> bool {
    previous
        .and_then(|words| words.get(index))
        .is_some_and(|word| word.display == current.display)
}

fn compatible_prefix(previous: &[HypothesisWord], current: &[HypothesisWord]) -> usize {
    // Prefix reconciliation decides which leading tentative words can move to
    // committed. Cap the scan instead of comparing a tail then applying its
    // count to index zero.
    let previous = comparison_prefix(previous);
    let current = comparison_prefix(current);
    let timestamped = timestamps_are_trustworthy(previous) && timestamps_are_trustworthy(current);
    previous
        .iter()
        .zip(current)
        .take_while(|(left, right)| {
            normalized(&left.display) == normalized(&right.display)
                && (!timestamped || timestamps_match(left, right))
        })
        .count()
}

fn comparison_prefix(words: &[HypothesisWord]) -> &[HypothesisWord] {
    &words[..words.len().min(MAX_COMPARISON_CONTEXT_WORDS)]
}

fn timestamps_are_trustworthy(words: &[HypothesisWord]) -> bool {
    let mut last_end = 0;
    !words.is_empty()
        && words
            .iter()
            .all(|word| match (word.start_frame, word.end_frame) {
                (Some(start), Some(end)) if start < end && start >= last_end => {
                    last_end = end;
                    true
                }
                _ => false,
            })
}

fn timestamps_match(left: &HypothesisWord, right: &HypothesisWord) -> bool {
    const DRIFT_FRAMES: u64 = 4_800; // 300 ms at 16 kHz.
    match (
        left.start_frame,
        left.end_frame,
        right.start_frame,
        right.end_frame,
    ) {
        (Some(left_start), Some(left_end), Some(right_start), Some(right_end)) => {
            left_start.abs_diff(right_start) <= DRIFT_FRAMES
                && left_end.abs_diff(right_end) <= DRIFT_FRAMES
        }
        _ => false,
    }
}

fn effective_frames(
    word: &HypothesisWord,
    window_start_frame: u64,
    window_end_frame: u64,
    index: usize,
    word_count: usize,
) -> (u64, u64) {
    if let (Some(start), Some(end)) = (word.start_frame, word.end_frame)
        && start < end
    {
        return (start, end);
    }
    let span = window_end_frame - window_start_frame;
    let count = word_count.max(1) as u64;
    let start = window_start_frame + span * index as u64 / count;
    let end = window_start_frame + span * (index as u64 + 1) / count;
    (start, end)
}

fn remove_committed_overlap(
    committed: &[HypothesisWord],
    current: &mut Vec<HypothesisWord>,
) -> usize {
    let committed_prefix = committed.len();
    if has_full_committed_prefix(committed, current) {
        current.drain(..committed_prefix);
        return committed_prefix;
    }
    let max_overlap = committed
        .len()
        .min(current.len())
        .min(MAX_COMPARISON_CONTEXT_WORDS);
    let overlap = (1..=max_overlap)
        .rev()
        .find(|&count| {
            committed[committed.len() - count..]
                .iter()
                .zip(&current[..count])
                .all(|(left, right)| normalized(&left.display) == normalized(&right.display))
        })
        .unwrap_or_default();
    if overlap > 0 {
        current.drain(..overlap);
    }
    overlap
}

fn has_full_committed_prefix(committed: &[HypothesisWord], current: &[HypothesisWord]) -> bool {
    !committed.is_empty()
        && current.len() >= committed.len()
        && committed
            .iter()
            .zip(&current[..committed.len()])
            .all(|(left, right)| normalized(&left.display) == normalized(&right.display))
}

fn normalized(display: &str) -> String {
    display
        .chars()
        .filter(|character| character.is_alphanumeric() || *character == '\'')
        .flat_map(char::to_lowercase)
        .collect()
}

fn render_words(words: &[HypothesisWord]) -> String {
    let mut text = String::new();
    for word in words {
        if word.display.is_empty() {
            continue;
        }
        if !text.is_empty() && !is_closing_punctuation(&word.display) {
            text.push(' ');
        }
        text.push_str(&word.display);
    }
    text
}

fn is_closing_punctuation(word: &str) -> bool {
    !word.is_empty()
        && word.chars().all(|character| {
            matches!(
                character,
                '.' | ',' | '!' | '?' | ':' | ';' | ')' | ']' | '}'
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    fn identity(sequence: u64) -> StreamIdentity {
        StreamIdentity {
            session_id: SessionId(7),
            request_id: RequestId(11),
            model_id: ModelId::new("preview-model"),
            sequence,
        }
    }

    fn audio(frames: usize) -> Arc<PreparedAudio> {
        Arc::new(PreparedAudio {
            samples: vec![0.0; frames],
            sample_rate: PREPARED_SAMPLE_RATE,
            source_sample_rate: PREPARED_SAMPLE_RATE,
            source_channels: 1,
            source_frames: frames,
        })
    }

    fn hypothesis(sequence: u64, start: u64, end: u64, text: &str) -> TranscriptHypothesis {
        TranscriptHypothesis::from_text(identity(sequence), start, end, text)
    }

    fn snapshot(sequence: u64) -> PreviewSnapshot {
        PreviewSnapshot::new(identity(sequence), 0, 16_000, audio(16_000)).unwrap()
    }

    #[test]
    fn defaults_match_the_phase_seven_contract() {
        assert_eq!(DECODE_INTERVAL_MS, 250);
        assert_eq!(ROLLING_WINDOW_MS, 3_000);
        assert_eq!(BOUNDARY_OVERLAP_MS, 650);
        assert_eq!(STABILITY_PASSES, 2);
        assert_eq!(STABILITY_HORIZON_MS, 700);
        assert_eq!(MAX_COMPARISON_CONTEXT_WORDS, 60);
    }

    #[test]
    fn application_shell_cannot_construct_or_publish_pcm_preview_snapshots() {
        let app_source = include_str!("app.rs");
        let test_module = app_source.find("mod layout_tests").unwrap();
        let test_attribute = app_source[..test_module].rfind("#[cfg(test)]").unwrap();
        let production_source = &app_source[..test_attribute];
        assert!(!production_source.contains("PreviewSnapshot"));
        assert!(!production_source.contains("publish_window"));
        assert!(!production_source.contains("snapshot.audio"));
    }

    #[test]
    fn snapshot_requires_canonical_audio_and_a_bounded_matching_range() {
        let snapshot = PreviewSnapshot::new(identity(1), 10, 48_010, audio(48_000)).unwrap();
        assert_eq!(
            snapshot.window_end_frame - snapshot.window_start_frame,
            48_000
        );

        assert_eq!(
            PreviewSnapshot::new(identity(1), 0, 48_001, audio(48_001)).unwrap_err(),
            SnapshotError::WindowTooLong { frames: 48_001 }
        );
        let mut wrong_rate = (*audio(1)).clone();
        wrong_rate.sample_rate = 48_000;
        assert!(matches!(
            PreviewSnapshot::new(identity(1), 0, 1, Arc::new(wrong_rate)),
            Err(SnapshotError::NonCanonicalSampleRate { .. })
        ));
        let mut out_of_range = (*audio(1)).clone();
        out_of_range.samples[0] = 1.1;
        assert!(matches!(
            PreviewSnapshot::new(identity(1), 0, 1, Arc::new(out_of_range)),
            Err(SnapshotError::OutOfRangeAudio)
        ));
    }

    #[test]
    fn mailbox_keeps_only_the_latest_pending_item_and_close_drops_it() {
        let mailbox = ReplaceLatestMailbox::new();
        assert!(mailbox.publish(1));
        let active = mailbox.try_claim().unwrap();
        assert!(mailbox.publish(2));
        assert!(mailbox.publish(3));
        assert_eq!(mailbox.counts(), (1, 1));
        assert_eq!(*active.item(), 1);
        drop(active);
        assert_eq!(mailbox.try_claim().unwrap().finish(), 3);

        assert!(mailbox.publish(4));
        mailbox.close();
        assert!(mailbox.is_closed());
        assert!(!mailbox.publish(5));
        assert!(mailbox.try_claim().is_none());
    }

    #[test]
    fn slow_consumer_never_allows_more_than_one_active_and_one_pending_item() {
        let mailbox = ReplaceLatestMailbox::new();
        assert!(mailbox.publish("active"));
        let active = mailbox.try_claim().unwrap();
        for value in ["old", "newest", "latest"] {
            assert!(mailbox.publish(value));
            assert_eq!(mailbox.counts(), (1, 1));
        }
        assert!(mailbox.try_claim().is_none());
        drop(active);
        assert_eq!(mailbox.try_claim().unwrap().finish(), "latest");
    }

    #[test]
    fn preview_session_decodes_only_the_newest_pending_snapshot_and_emits_its_latest_event() {
        let (started_sender, started_receiver) = mpsc::channel();
        let release = Arc::new((Mutex::new(false), Condvar::new()));
        let worker_release = Arc::clone(&release);
        let mut session = RollingPreviewSession::<()>::new(move |snapshot| {
            let sequence = snapshot.identity.sequence;
            started_sender.send(sequence).unwrap();
            if sequence == 1 {
                let (lock, wake) = &*worker_release;
                let mut released = lock.lock().unwrap();
                while !*released {
                    released = wake.wait(released).unwrap();
                }
            }
            Ok(StreamUpdate {
                committed: format!("committed {sequence}"),
                tentative: String::new(),
            })
        })
        .unwrap();

        assert!(session.try_update(snapshot(1)));
        assert_eq!(
            started_receiver
                .recv_timeout(Duration::from_secs(1))
                .unwrap(),
            1
        );
        assert!(session.try_update(snapshot(2)));
        assert!(session.try_update(snapshot(3)));
        {
            let (lock, wake) = &*release;
            *lock.lock().unwrap() = true;
            wake.notify_one();
        }
        assert_eq!(
            started_receiver
                .recv_timeout(Duration::from_secs(1))
                .unwrap(),
            3
        );
        assert!(session.stop_and_join(Duration::from_secs(1)));

        match session.try_next().unwrap() {
            PreviewEvent::Update { identity, update } => {
                assert_eq!(identity.sequence, 3);
                assert_eq!(update.committed, "committed 3");
            }
            PreviewEvent::Error { .. } => panic!("test decoder does not fail"),
        }
    }

    #[test]
    fn preview_session_close_drops_pending_work_and_has_a_bounded_join() {
        let (started_sender, started_receiver) = mpsc::channel();
        let release = Arc::new((Mutex::new(false), Condvar::new()));
        let worker_release = Arc::clone(&release);
        let mut session = RollingPreviewSession::<()>::new(move |snapshot| {
            started_sender.send(snapshot.identity.sequence).unwrap();
            let (lock, wake) = &*worker_release;
            let mut released = lock.lock().unwrap();
            while !*released {
                released = wake.wait(released).unwrap();
            }
            Ok(StreamUpdate::default())
        })
        .unwrap();

        assert!(session.try_update(snapshot(1)));
        assert_eq!(
            started_receiver
                .recv_timeout(Duration::from_secs(1))
                .unwrap(),
            1
        );
        assert!(session.try_update(snapshot(2)));
        assert!(!session.stop_and_join(Duration::ZERO));
        assert!(!session.try_update(snapshot(3)));
        {
            let (lock, wake) = &*release;
            *lock.lock().unwrap() = true;
            wake.notify_one();
        }
        assert!(session.stop_and_join(Duration::from_secs(1)));
        assert!(
            started_receiver
                .recv_timeout(Duration::from_millis(20))
                .is_err()
        );
    }

    #[test]
    fn dropping_preview_session_cancels_and_joins_an_active_decoder() {
        let (started_sender, started_receiver) = mpsc::channel();
        let release = Arc::new((Mutex::new(false), Condvar::new()));
        let worker_release = Arc::clone(&release);
        let cancel_release = Arc::clone(&release);
        let cancel_count = Arc::new(AtomicU64::new(0));
        let observed_cancel_count = Arc::clone(&cancel_count);
        let session = RollingPreviewSession::<()>::new_with_cancel(
            move |snapshot| {
                started_sender.send(snapshot.identity.sequence).unwrap();
                let (lock, wake) = &*worker_release;
                let mut released = lock.lock().unwrap();
                while !*released {
                    released = wake.wait(released).unwrap();
                }
                Ok(StreamUpdate::default())
            },
            move || {
                observed_cancel_count.fetch_add(1, Ordering::AcqRel);
                let (lock, wake) = &*cancel_release;
                *lock.lock().unwrap() = true;
                wake.notify_one();
            },
        )
        .unwrap();

        assert!(session.try_update(snapshot(1)));
        assert_eq!(
            started_receiver
                .recv_timeout(Duration::from_secs(1))
                .unwrap(),
            1
        );
        let (dropped_sender, dropped_receiver) = mpsc::channel();
        let dropper = thread::spawn(move || {
            drop(session);
            dropped_sender.send(()).unwrap();
        });
        dropped_receiver
            .recv_timeout(Duration::from_secs(1))
            .unwrap();
        dropper.join().unwrap();
        assert_eq!(cancel_count.load(Ordering::Acquire), 1);
    }

    #[test]
    fn alice_can_be_corrected_to_alex_while_only_the_prefix_commits() {
        let mut stabilizer =
            TranscriptStabilizer::new(SessionId(7), RequestId(11), ModelId::new("preview-model"));
        let start = 0;
        let end = 48_000;
        stabilizer
            .push(hypothesis(1, start, end, "Schedule a meeting with Alice"))
            .unwrap();
        stabilizer
            .push(hypothesis(2, start, end, "Schedule a meeting with Alice"))
            .unwrap();
        let state = stabilizer
            .push(hypothesis(
                3,
                start,
                end,
                "Schedule a meeting with Alex tomorrow",
            ))
            .unwrap();

        assert_eq!(state.committed, "Schedule a meeting with");
        assert_eq!(state.tentative, "Alex tomorrow");
    }

    #[test]
    fn punctuation_and_case_stay_tentative_until_their_display_form_repeats() {
        let mut stabilizer =
            TranscriptStabilizer::new(SessionId(7), RequestId(11), ModelId::new("preview-model"));
        let words = |sequence, ending: &str| TranscriptHypothesis {
            identity: identity(sequence),
            window_start_frame: 0,
            window_end_frame: 48_000,
            words: vec![
                HypothesisWord::new("hello").at_absolute_frames(12_000, 16_000),
                HypothesisWord::new(ending).at_absolute_frames(20_000, 24_000),
            ],
        };
        stabilizer.push(words(1, "world")).unwrap();
        let state = stabilizer.push(words(2, "world.")).unwrap();
        assert_eq!(state.committed, "hello");
        assert_eq!(state.tentative, "world.");
        let state = stabilizer.push(words(3, "world.")).unwrap();
        assert_eq!(state.committed, "hello world.");
        assert!(state.tentative.is_empty());
    }

    #[test]
    fn case_correction_requires_the_corrected_display_form_to_repeat() {
        let mut stabilizer =
            TranscriptStabilizer::new(SessionId(7), RequestId(11), ModelId::new("preview-model"));
        let words = |sequence, ending: &str| TranscriptHypothesis {
            identity: identity(sequence),
            window_start_frame: 0,
            window_end_frame: 48_000,
            words: vec![
                HypothesisWord::new("hello").at_absolute_frames(4_000, 8_000),
                HypothesisWord::new(ending).at_absolute_frames(12_000, 16_000),
            ],
        };

        stabilizer.push(words(1, "World")).unwrap();
        let corrected = stabilizer.push(words(2, "world")).unwrap();
        assert_eq!(corrected.committed, "hello");
        assert_eq!(corrected.tentative, "world");
        let repeated = stabilizer.push(words(3, "world")).unwrap();
        assert_eq!(repeated.committed, "hello world");
        assert!(repeated.tentative.is_empty());
    }

    #[test]
    fn missing_word_can_reappear_without_rewriting_the_committed_prefix() {
        let mut stabilizer =
            TranscriptStabilizer::new(SessionId(7), RequestId(11), ModelId::new("preview-model"));
        stabilizer
            .push(hypothesis(1, 0, 48_000, "we really agree"))
            .unwrap();
        let missing = stabilizer
            .push(hypothesis(2, 0, 48_000, "we agree"))
            .unwrap();
        assert_eq!(missing.committed, "we");
        assert_eq!(missing.tentative, "agree");

        let reappeared = stabilizer
            .push(hypothesis(3, 0, 48_000, "we really agree"))
            .unwrap();
        assert_eq!(reappeared.committed, "we");
        assert_eq!(reappeared.tentative, "really agree");
        let stable = stabilizer
            .push(hypothesis(4, 0, 48_000, "we really agree"))
            .unwrap();
        assert_eq!(stable.committed, "we really");
        assert_eq!(stable.tentative, "agree");
    }

    #[test]
    fn repeated_words_at_a_window_overlap_are_not_duplicated() {
        let mut stabilizer =
            TranscriptStabilizer::new(SessionId(7), RequestId(11), ModelId::new("preview-model"));
        stabilizer.committed = vec![HypothesisWord::new("go"), HypothesisWord::new("go")];
        stabilizer.state.committed = "go go".to_owned();
        stabilizer.previous = Some(vec![HypothesisWord::new("go"), HypothesisWord::new("now")]);
        let state = stabilizer
            .push(hypothesis(1, 0, 48_000, "go now please"))
            .unwrap();

        assert_eq!(state.committed, "go go");
        assert_eq!(state.tentative, "now please");
    }

    #[test]
    fn repeated_words_are_deduplicated_after_normal_consecutive_updates() {
        let mut stabilizer =
            TranscriptStabilizer::new(SessionId(7), RequestId(11), ModelId::new("preview-model"));
        let opening = |sequence| TranscriptHypothesis {
            identity: identity(sequence),
            window_start_frame: 0,
            window_end_frame: 48_000,
            words: vec![
                HypothesisWord::new("go").at_absolute_frames(0, 4_000),
                HypothesisWord::new("go").at_absolute_frames(5_000, 8_000),
            ],
        };
        stabilizer.push(opening(1)).unwrap();
        assert_eq!(stabilizer.push(opening(2)).unwrap().committed, "go go");
        stabilizer
            .push(hypothesis(3, 0, 48_000, "go go now"))
            .unwrap();
        stabilizer
            .push(hypothesis(4, 0, 48_000, "go go now"))
            .unwrap();
        let rolled = stabilizer
            .push(hypothesis(5, 0, 48_000, "go now please"))
            .unwrap();

        assert_eq!(rolled.committed, "go go");
        assert_eq!(rolled.tentative, "now please");
    }

    #[test]
    fn empty_or_dropped_hypotheses_never_erase_committed_text() {
        let mut stabilizer =
            TranscriptStabilizer::new(SessionId(7), RequestId(11), ModelId::new("preview-model"));
        stabilizer.committed = vec![HypothesisWord::new("already")];
        stabilizer.state.committed = "already".to_owned();
        stabilizer.previous = Some(vec![HypothesisWord::new("there")]);
        stabilizer.state.tentative = "there".to_owned();

        let state = stabilizer.push(hypothesis(1, 0, 48_000, "")).unwrap();
        assert_eq!(state.committed, "already");
        assert_eq!(state.tentative, "there");
        let state = stabilizer
            .push(hypothesis(2, 0, 48_000, "already"))
            .unwrap();
        assert_eq!(state.committed, "already");
        assert!(state.tentative.is_empty());
    }

    #[test]
    fn timestamps_are_absolute_and_use_window_offsets_for_horizon_gating() {
        let mut stabilizer =
            TranscriptStabilizer::new(SessionId(7), RequestId(11), ModelId::new("preview-model"));
        let make = |sequence| TranscriptHypothesis {
            identity: identity(sequence),
            window_start_frame: 16_000,
            window_end_frame: 64_000,
            words: vec![
                HypothesisWord::new("mapped").at_relative_frames(16_000, 12_000, 16_000),
                HypothesisWord::new("late").at_relative_frames(16_000, 40_000, 44_000),
            ],
        };
        stabilizer.push(make(1)).unwrap();
        let state = stabilizer.push(make(2)).unwrap();

        assert_eq!(state.committed, "mapped");
        assert_eq!(state.tentative, "late");
    }

    #[test]
    fn initial_window_can_commit_a_word_at_frame_zero_after_two_passes() {
        let mut stabilizer =
            TranscriptStabilizer::new(SessionId(7), RequestId(11), ModelId::new("preview-model"));
        let make = |sequence| TranscriptHypothesis {
            identity: identity(sequence),
            window_start_frame: 0,
            window_end_frame: 48_000,
            words: vec![HypothesisWord::new("opening").at_absolute_frames(0, 8_000)],
        };
        stabilizer.push(make(1)).unwrap();
        let state = stabilizer.push(make(2)).unwrap();

        assert_eq!(state.committed, "opening");
    }

    #[test]
    fn fallback_alignment_deduplicates_without_timestamps() {
        let mut stabilizer =
            TranscriptStabilizer::new(SessionId(7), RequestId(11), ModelId::new("preview-model"));
        stabilizer.committed = vec![HypothesisWord::new("hello"), HypothesisWord::new("world")];
        stabilizer.state.committed = "hello world".to_owned();
        stabilizer.previous = Some(vec![HypothesisWord::new("again")]);
        let state = stabilizer
            .push(hypothesis(1, 0, 48_000, "world again today"))
            .unwrap();

        assert_eq!(state.committed, "hello world");
        assert_eq!(state.tentative, "again today");
    }

    #[test]
    fn short_phrase_matching_an_ancient_prefix_is_not_deduplicated() {
        let committed = vec![
            HypothesisWord::new("ancient"),
            HypothesisWord::new("prefix"),
            HypothesisWord::new("finished"),
        ];
        let mut current = vec![HypothesisWord::new("ancient")];

        assert_eq!(remove_committed_overlap(&committed, &mut current), 0);
        assert_eq!(render_words(&current), "ancient");
    }

    #[test]
    fn two_pass_and_horizon_gating_are_both_required() {
        let mut stabilizer =
            TranscriptStabilizer::new(SessionId(7), RequestId(11), ModelId::new("preview-model"));
        let make = |sequence, window_end| TranscriptHypothesis {
            identity: identity(sequence),
            window_start_frame: 0,
            window_end_frame: window_end,
            words: vec![HypothesisWord::new("stable").at_absolute_frames(12_000, 40_000)],
        };
        let state = stabilizer.push(make(1, 48_000)).unwrap();
        assert!(state.committed.is_empty());
        let state = stabilizer.push(make(2, 48_000)).unwrap();
        assert!(
            state.committed.is_empty(),
            "too recent for the 700 ms horizon"
        );
        let state = stabilizer.push(make(3, 72_000)).unwrap();
        assert_eq!(state.committed, "stable");
    }

    #[test]
    fn horizon_commits_at_exactly_seven_hundred_ms_but_not_six_ninety_nine() {
        let make = |sequence, word_end| TranscriptHypothesis {
            identity: identity(sequence),
            window_start_frame: 0,
            window_end_frame: 48_000,
            words: vec![HypothesisWord::new("boundary").at_absolute_frames(12_000, word_end)],
        };
        let mut exact =
            TranscriptStabilizer::new(SessionId(7), RequestId(11), ModelId::new("preview-model"));
        exact.push(make(1, 36_800)).unwrap();
        assert_eq!(exact.push(make(2, 36_800)).unwrap().committed, "boundary");

        let mut too_recent =
            TranscriptStabilizer::new(SessionId(7), RequestId(11), ModelId::new("preview-model"));
        too_recent.push(make(1, 36_816)).unwrap();
        assert!(
            too_recent
                .push(make(2, 36_816))
                .unwrap()
                .committed
                .is_empty()
        );
    }

    #[test]
    fn overlap_boundary_commits_at_exactly_six_fifty_ms_but_not_one_frame_before() {
        let run = |edge_start| {
            let mut stabilizer = TranscriptStabilizer::new(
                SessionId(7),
                RequestId(11),
                ModelId::new("preview-model"),
            );
            let opening = |sequence| TranscriptHypothesis {
                identity: identity(sequence),
                window_start_frame: 0,
                window_end_frame: 48_000,
                words: vec![HypothesisWord::new("hello").at_absolute_frames(0, 8_000)],
            };
            stabilizer.push(opening(1)).unwrap();
            stabilizer.push(opening(2)).unwrap();
            let rolled = |sequence| TranscriptHypothesis {
                identity: identity(sequence),
                window_start_frame: 0,
                window_end_frame: 48_000,
                words: vec![
                    HypothesisWord::new("hello").at_absolute_frames(0, 8_000),
                    HypothesisWord::new("edge").at_absolute_frames(edge_start, 16_000),
                ],
            };
            stabilizer.push(rolled(3)).unwrap();
            stabilizer.push(rolled(4)).unwrap()
        };

        let one_frame_before = run(10_399);
        assert_eq!(one_frame_before.committed, "hello");
        assert_eq!(one_frame_before.tentative, "edge");
        let exact = run(10_400);
        assert_eq!(exact.committed, "hello edge");
        assert!(exact.tentative.is_empty());
    }

    #[test]
    fn comparison_context_is_bounded_to_sixty_words() {
        let words = (0..65)
            .map(|index| HypothesisWord::new(format!("word{index}")))
            .collect::<Vec<_>>();
        assert_eq!(
            comparison_prefix(&words).len(),
            MAX_COMPARISON_CONTEXT_WORDS
        );
        assert_eq!(
            compatible_prefix(&words, &words),
            MAX_COMPARISON_CONTEXT_WORDS
        );
    }

    #[test]
    fn stale_out_of_order_and_mismatched_updates_are_rejected() {
        let mut stabilizer =
            TranscriptStabilizer::new(SessionId(7), RequestId(11), ModelId::new("preview-model"));
        stabilizer.push(hypothesis(2, 0, 48_000, "new")).unwrap();
        assert!(matches!(
            stabilizer.push(hypothesis(1, 0, 48_000, "old")),
            Err(UpdateRejection::StaleSequence { .. })
        ));

        let mut wrong_session = hypothesis(3, 0, 48_000, "bad");
        wrong_session.identity.session_id = SessionId(8);
        assert_eq!(
            stabilizer.push(wrong_session),
            Err(UpdateRejection::WrongSession)
        );
        let mut wrong_request = hypothesis(3, 0, 48_000, "bad");
        wrong_request.identity.request_id = RequestId(12);
        assert_eq!(
            stabilizer.push(wrong_request),
            Err(UpdateRejection::WrongRequest)
        );
        let mut wrong_model = hypothesis(3, 0, 48_000, "bad");
        wrong_model.identity.model_id = ModelId::new("other-model");
        assert_eq!(
            stabilizer.push(wrong_model),
            Err(UpdateRejection::WrongModel)
        );
    }
}
