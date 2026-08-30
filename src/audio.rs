pub(crate) mod control;
mod pipeline;
mod ring_buffer;

use std::fs;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, TryRecvError, bounded};
use thiserror::Error;

use crate::config;
use crate::onnx_worker::{SileroVadDecision, SileroVadWorkerSupervisor};
use crate::prepared_audio::PreparedAudio;
use crate::silero_vad_native::{VadThreshold, WINDOW_SAMPLES};
use crate::streaming::PreviewAudioPublisher;

use self::pipeline::Pipeline;
use self::ring_buffer::{Consumer, Producer, ring_buffer};

const START_TIMEOUT: Duration = Duration::from_secs(3);
const DEFAULT_RING_SECONDS: usize = 2;
const MIN_RING_SAMPLES: usize = 65_536;
const MAX_RING_SAMPLES: usize = 2_000_000;
const MAX_STREAM_RESTARTS: u32 = 2;
const MAX_DRAIN_SAMPLES_PER_TICK: usize = 4_096;
const STREAM_RESTART_BACKOFF: Duration = Duration::from_millis(50);
#[cfg(test)]
pub(crate) const ABORT_STREAM_DROP_BUDGET: Duration = Duration::from_millis(250);
const MAX_CAPTURE_PREPARED_FRAMES: usize = 16_000
    * (config::MAX_RECORDING_SECONDS as usize
        + config::RECORDING_CAPTURE_SAFETY_ALLOWANCE_SECONDS as usize);
pub(super) const MIN_INPUT_SAMPLE_RATE: u32 = 8_000;
pub(super) const MAX_INPUT_SAMPLE_RATE: u32 = 384_000;
pub(super) const MAX_INPUT_CHANNELS: u16 = 32;
pub(crate) const LOW_INPUT_DIAGNOSTIC_RMS: f32 = 0.012;
const FAULT_NONE: u8 = 0;
const FAULT_OVERFLOW: u8 = 1;
const FAULT_STREAM: u8 = 2;
const STARTUP_PENDING: u8 = 0;
const STARTUP_PLAY_COMMITTED: u8 = 1;
const STARTUP_FIRST_SAMPLE: u8 = 2;
const STARTUP_CANCELLED: u8 = 3;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VadOptions {
    pub speech_confirmation: Duration,
    pub pause: Duration,
    pub endpoint: Duration,
    pub pre_roll: Duration,
    pub post_roll: Duration,
}

impl VadOptions {
    pub fn new(
        speech_confirmation: Duration,
        pause: Duration,
        endpoint: Duration,
        pre_roll: Duration,
        post_roll: Duration,
    ) -> Self {
        Self {
            speech_confirmation,
            pause,
            endpoint,
            pre_roll,
            post_roll,
        }
    }

    fn validate(self) -> Result<(), CaptureError> {
        if self.speech_confirmation.is_zero() {
            return Err(CaptureError::InvalidOptions(
                "speech confirmation must be greater than zero",
            ));
        }
        if self.pause.is_zero() || self.pause > self.endpoint {
            return Err(CaptureError::InvalidOptions(
                "pause must be greater than zero and no longer than endpoint",
            ));
        }
        if self.endpoint.is_zero() {
            return Err(CaptureError::InvalidOptions(
                "endpoint must be greater than zero",
            ));
        }
        if self.speech_confirmation > Duration::from_secs(1)
            || self.pause > Duration::from_secs(3)
            || self.endpoint > Duration::from_secs(5)
            || self.pre_roll > Duration::from_secs(2)
            || self.post_roll > Duration::from_secs(2)
        {
            return Err(CaptureError::InvalidOptions(
                "VAD timings exceed the supported capture bounds",
            ));
        }
        Ok(())
    }
}

impl Default for VadOptions {
    fn default() -> Self {
        Self {
            speech_confirmation: Duration::from_millis(150),
            pause: Duration::from_millis(450),
            endpoint: Duration::from_millis(900),
            pre_roll: Duration::from_millis(250),
            post_roll: Duration::from_millis(200),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CaptureOptions {
    pub vad_enabled: bool,
    pub endpointing_enabled: bool,
    /// Allows a conservative, release-only rescue for hold-to-talk clips that
    /// are shorter than the normal speech confirmation interval.
    pub short_speech_rescue: bool,
    pub vad: VadOptions,
    pub detection_mode: SpeechDetectionMode,
    pub intent: CaptureIntent,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SpeechDetectionMode {
    /// Silero's bundled default probability cutoff (0.5).
    Ai,
    /// A literal RMS cutoff applied to complete 30 ms prepared-audio windows.
    ManualThreshold { threshold_rms: f32 },
}

impl SpeechDetectionMode {
    fn validate(self) -> Result<(), CaptureError> {
        match self {
            Self::Ai => VadThreshold::new(0.5)
                .map(|_| ())
                .map_err(WorkerSpeechDetector::vad_error),
            Self::ManualThreshold { threshold_rms }
                if threshold_rms.is_finite() && (0.0..=1.0).contains(&threshold_rms) =>
            {
                Ok(())
            }
            Self::ManualThreshold { .. } => Err(CaptureError::InvalidOptions(
                "manual input threshold must be finite and within [0, 1]",
            )),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CaptureIntent {
    Dictation,
    MeterOnly,
}

impl CaptureOptions {
    pub fn new(vad: VadOptions) -> Self {
        Self {
            vad_enabled: true,
            endpointing_enabled: true,
            short_speech_rescue: false,
            vad,
            detection_mode: SpeechDetectionMode::Ai,
            intent: CaptureIntent::Dictation,
        }
    }
}

impl Default for CaptureOptions {
    fn default() -> Self {
        Self::new(VadOptions::default())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CaptureStopReason {
    Explicit,
    Endpoint,
    MaximumDuration,
}

#[derive(Clone, Default)]
pub struct CaptureCancellation {
    stop_requested: Arc<AtomicBool>,
    startup_state: Arc<AtomicU8>,
}

impl CaptureCancellation {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.cancel_startup();
        self.stop_requested.store(true, Ordering::Release);
    }

    /// Linearizes cancellation against the audio callback's first-sample
    /// transition. A successful transition guarantees the callback cannot
    /// subsequently activate this capture.
    fn cancel_startup(&self) -> bool {
        let mut state = self.startup_state.load(Ordering::Acquire);
        while matches!(state, STARTUP_PENDING | STARTUP_PLAY_COMMITTED) {
            match self.startup_state.compare_exchange_weak(
                state,
                STARTUP_CANCELLED,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return true,
                Err(current) => state = current,
            }
        }
        state == STARTUP_CANCELLED
    }

    pub fn is_cancelled(&self) -> bool {
        self.stop_requested.load(Ordering::Acquire)
    }

    fn ensure_startup_active(&self) -> Result<(), CaptureError> {
        if self.is_cancelled() || self.startup_state.load(Ordering::Acquire) == STARTUP_CANCELLED {
            Err(CaptureError::StartupCancelled)
        } else {
            Ok(())
        }
    }

    fn commit_play(&self) -> Result<(), CaptureError> {
        if self.is_cancelled() {
            return Err(CaptureError::StartupCancelled);
        }
        self.startup_state
            .compare_exchange(
                STARTUP_PENDING,
                STARTUP_PLAY_COMMITTED,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .map(|_| ())
            .map_err(|_| CaptureError::StartupCancelled)
    }

    fn observe_first_sample(&self) -> bool {
        match self.startup_state.compare_exchange(
            STARTUP_PLAY_COMMITTED,
            STARTUP_FIRST_SAMPLE,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) | Err(STARTUP_FIRST_SAMPLE) => true,
            Err(_) => false,
        }
    }

    fn startup_cancelled_before_first_sample(&self) -> bool {
        self.startup_state.load(Ordering::Acquire) == STARTUP_CANCELLED
    }

    fn first_sample_observed(&self) -> bool {
        self.startup_state.load(Ordering::Acquire) == STARTUP_FIRST_SAMPLE
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LevelSnapshot {
    pub rms: f32,
    pub peak: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CaptureMetrics {
    pub duration: Duration,
    pub stop_trigger_elapsed: Duration,
    pub speech_trigger_elapsed: Option<Duration>,
    pub source_sample_rate: u32,
    pub source_channels: u16,
    pub source_frames: usize,
    pub prepared_frames: usize,
    /// Maximum native RMS observed across 10 ms diagnostic windows.
    /// This value never classifies speech.
    pub maximum_input_rms: f32,
    /// Maximum native sample peak observed by the 30 ms meter windows.
    pub maximum_input_peak: f32,
    /// Whether any raw manual-threshold gate window met or exceeded its RMS cutoff.
    pub manual_threshold_crossed: bool,
    pub dropped_samples: usize,
    pub stream_restarts: u32,
    pub timing: CaptureTimingMetrics,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CaptureTimingMetrics {
    pub hotkey_to_worker: Duration,
    pub device_lookup: Duration,
    pub stream_build: Duration,
    pub stream_play: Duration,
    pub first_sample: Duration,
    pub release: Option<Duration>,
    pub finalization: Duration,
}

#[derive(Clone, Debug)]
pub struct CaptureCompletion {
    pub audio: Option<Arc<PreparedAudio>>,
    pub stop_reason: CaptureStopReason,
    pub metrics: CaptureMetrics,
}

/// A one-shot late-binding slot lets urgent microphone startup run before
/// optional rolling-preview/model setup. Closing the slot invalidates a
/// publisher that loses the race with capture finalization.
#[derive(Clone, Default)]
pub(crate) struct PreviewPublisherSlot {
    publisher: Arc<Mutex<Option<PreviewAudioPublisher>>>,
    closed: Arc<AtomicBool>,
}

impl PreviewPublisherSlot {
    pub(crate) fn install(&self, publisher: PreviewAudioPublisher) -> bool {
        if self.closed.load(Ordering::Acquire) {
            publisher.invalidate();
            return false;
        }
        let mut slot = self
            .publisher
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if self.closed.load(Ordering::Acquire) || slot.is_some() {
            publisher.invalidate();
            return false;
        }
        *slot = Some(publisher);
        true
    }

    fn take(&self) -> Option<PreviewAudioPublisher> {
        self.publisher
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
    }

    fn close(&self) {
        self.closed.store(true, Ordering::Release);
        if let Some(publisher) = self.take() {
            publisher.invalidate();
        }
    }
}

#[derive(Clone, Debug, Error, PartialEq)]
pub enum CaptureError {
    #[error("invalid audio capture options: {0}")]
    InvalidOptions(&'static str),
    #[error("unsupported microphone input format: {sample_rate} Hz, {channels} channels")]
    InvalidInputFormat { sample_rate: u32, channels: u16 },
    #[error("failed to enumerate microphone devices: {0}")]
    DeviceEnumeration(String),
    #[error("requested microphone {requested:?} is unavailable")]
    NoInputDevice { requested: Option<String> },
    #[error("failed to read microphone input configuration: {0}")]
    InputConfiguration(String),
    #[error("unsupported microphone sample format: {0}")]
    UnsupportedSampleFormat(String),
    #[error("failed to build microphone input stream: {0}")]
    BuildStream(String),
    #[error("failed to start microphone input stream: {0}")]
    PlayStream(String),
    #[error("microphone format changed while recovering the input stream")]
    InputFormatChanged,
    #[error("microphone input stream failed after {restarts} restart attempts: {last_error}")]
    InputStreamFailed { restarts: u32, last_error: String },
    #[error("audio capture buffer overflowed and dropped {dropped_samples} samples")]
    BufferOverflow { dropped_samples: usize },
    #[error("failed to prepare captured audio: {0}")]
    Preparation(String),
    #[error("prepared audio exceeded the in-memory limit of {maximum_frames} frames")]
    PreparedAudioLimit { maximum_frames: usize },
    #[error("audio recorder did not start within {0:?}")]
    StartTimeout(Duration),
    #[error("microphone startup was cancelled")]
    StartupCancelled,
    #[error("audio capture was discarded")]
    Discarded,
    #[error("audio recorder did not stop within {0:?}")]
    StopTimeout(Duration),
    #[error("audio recorder worker disconnected unexpectedly")]
    WorkerDisconnected,
    #[error("failed to spawn audio recorder worker: {0}")]
    WorkerSpawn(String),
    #[error("audio recorder worker panicked: {0}")]
    WorkerPanic(String),
    #[error(
        "Silero speech detection is unavailable; repair the bundled support asset or restart Scribe: {0}"
    )]
    SpeechDetection(String),
}

trait SpeechDetector: Send {
    fn compute(
        &mut self,
        samples: &[f32; WINDOW_SAMPLES],
    ) -> Result<SileroVadDecision, CaptureError>;
    fn finish(&mut self) -> Result<(), CaptureError>;
    fn cancel(&mut self) -> Result<(), CaptureError>;
}

trait SpeechDetectorFactory: Send + Sync {
    fn acquire(
        &self,
        threshold: VadThreshold,
        startup_cancelled: &AtomicBool,
        discard_requested: Arc<AtomicBool>,
    ) -> Result<Box<dyn SpeechDetector>, CaptureError>;
}

struct WorkerSpeechDetectorFactory;

struct WorkerSpeechDetector {
    supervisor: SileroVadWorkerSupervisor,
    session_id: u64,
    next_request_id: u64,
    active: bool,
    discard_requested: Arc<AtomicBool>,
}

static NEXT_VAD_SESSION_ID: AtomicU64 = AtomicU64::new(1);
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct CaptureId(pub(crate) u64);

#[derive(Clone, Copy, Debug)]
pub(crate) struct CaptureStartContext {
    pub capture_id: CaptureId,
    pub observed_at: Instant,
}

impl CaptureStartContext {
    pub(crate) fn new(capture_id: CaptureId, observed_at: Instant) -> Self {
        Self {
            capture_id,
            observed_at,
        }
    }

    #[cfg(test)]
    pub(crate) fn observed_at(observed_at: Instant) -> Self {
        static NEXT_TEST_CAPTURE_ID: AtomicU64 = AtomicU64::new(1);
        Self::new(
            CaptureId(NEXT_TEST_CAPTURE_ID.fetch_add(1, Ordering::Relaxed).max(1)),
            observed_at,
        )
    }
}

impl WorkerSpeechDetector {
    fn request_id(&mut self) -> u64 {
        let request_id = self.next_request_id;
        self.next_request_id = self.next_request_id.wrapping_add(1).max(1);
        request_id
    }

    fn vad_error(error: impl std::fmt::Display) -> CaptureError {
        CaptureError::SpeechDetection(error.to_string())
    }
}

impl SpeechDetectorFactory for WorkerSpeechDetectorFactory {
    fn acquire(
        &self,
        threshold: VadThreshold,
        startup_cancelled: &AtomicBool,
        discard_requested: Arc<AtomicBool>,
    ) -> Result<Box<dyn SpeechDetector>, CaptureError> {
        let session_id = NEXT_VAD_SESSION_ID.fetch_add(1, Ordering::Relaxed).max(1);
        let (supervisor, next_request_id) = SileroVadWorkerSupervisor::acquire_session(
            session_id,
            1,
            1,
            threshold,
            startup_cancelled,
        )
        .map_err(WorkerSpeechDetector::vad_error)?;
        let detector = WorkerSpeechDetector {
            supervisor,
            session_id,
            next_request_id,
            active: true,
            discard_requested,
        };
        Ok(Box::new(detector))
    }
}

impl SpeechDetector for WorkerSpeechDetector {
    fn compute(
        &mut self,
        samples: &[f32; WINDOW_SAMPLES],
    ) -> Result<SileroVadDecision, CaptureError> {
        let request_id = self.request_id();
        self.supervisor
            .compute_with_cancellation(
                self.session_id,
                request_id,
                samples,
                Some(self.discard_requested.as_ref()),
            )
            .map_err(Self::vad_error)
    }

    fn finish(&mut self) -> Result<(), CaptureError> {
        if !self.active {
            return Ok(());
        }
        let request_id = self.request_id();
        let result = self
            .supervisor
            .end_session(self.session_id, request_id)
            .map_err(Self::vad_error);
        self.active = false;
        result
    }

    fn cancel(&mut self) -> Result<(), CaptureError> {
        if !self.active {
            return Ok(());
        }
        let request_id = self.request_id();
        let result = self
            .supervisor
            .cancel_session(self.session_id, request_id)
            .map_err(Self::vad_error);
        self.active = false;
        result
    }
}

impl Drop for WorkerSpeechDetector {
    fn drop(&mut self) {
        if self.active {
            self.supervisor.abandon_session(self.session_id);
            self.active = false;
        }
    }
}

#[derive(Clone)]
pub struct RecordingSession {
    inner: Arc<RecordingSessionInner>,
}

#[cfg(test)]
#[derive(Clone, Default)]
pub(crate) struct SimulatedCaptureProbe {
    pub(crate) stream_dropped: Arc<AtomicBool>,
    pub(crate) finish_called: Arc<AtomicBool>,
    pub(crate) terminal_preview_called: Arc<AtomicBool>,
    pub(crate) preview_invalidated: Arc<AtomicBool>,
    pub(crate) post_roll_entered: Arc<AtomicBool>,
}

struct RecordingSessionInner {
    stop_requested: Arc<AtomicBool>,
    discard_requested: Arc<AtomicBool>,
    abort_action: Arc<dyn Fn() + Send + Sync>,
    finished_rx: Receiver<Result<CaptureCompletion, CaptureError>>,
    rms_bits: Arc<AtomicU32>,
    peak_bits: Arc<AtomicU32>,
    level_observed: Arc<AtomicBool>,
    level_revision: Arc<AtomicU64>,
    worker: Mutex<Option<thread::JoinHandle<()>>>,
}

impl RecordingSession {
    pub fn stop(&self) {
        self.inner.stop_requested.store(true, Ordering::Release);
    }

    pub(crate) fn abort(&self) {
        self.inner.discard_requested.store(true, Ordering::Release);
        (self.inner.abort_action)();
        self.stop();
    }

    pub fn try_finish(&self) -> Option<Result<CaptureCompletion, CaptureError>> {
        let result = match self.inner.finished_rx.try_recv() {
            Ok(result) => Some(result),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(Err(CaptureError::WorkerDisconnected)),
        };
        if result.is_some() {
            self.join_worker();
        }
        result
    }

    pub fn latest_levels(&self) -> LevelSnapshot {
        LevelSnapshot {
            rms: f32::from_bits(self.inner.rms_bits.load(Ordering::Relaxed)).clamp(0.0, 1.0),
            peak: f32::from_bits(self.inner.peak_bits.load(Ordering::Relaxed)).clamp(0.0, 1.0),
        }
    }

    pub fn has_level_update(&self) -> bool {
        self.inner.level_observed.load(Ordering::Acquire)
    }

    pub fn latest_level_revision(&self) -> u64 {
        self.inner.level_revision.load(Ordering::Acquire)
    }

    pub fn stop_and_discard(self, timeout: Duration) -> Result<(), CaptureError> {
        self.abort();
        let result = match self.inner.finished_rx.recv_timeout(timeout) {
            Ok(Ok(_completion)) => Ok(()),
            Ok(Err(CaptureError::Discarded)) => Ok(()),
            Ok(Err(error)) => Err(error),
            Err(_) => Err(CaptureError::StopTimeout(timeout)),
        };
        if matches!(result, Err(CaptureError::StopTimeout(_))) {
            self.reap_worker();
        } else {
            self.join_worker();
        }
        result
    }

    fn take_worker(&self) -> Option<thread::JoinHandle<()>> {
        self.inner
            .worker
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
    }

    fn join_worker(&self) {
        if let Some(worker) = self.take_worker() {
            let _ = worker.join();
        }
    }

    fn reap_worker(&self) {
        if let Some(worker) = self.take_worker() {
            spawn_worker_reaper(worker);
        }
    }

    #[cfg(test)]
    pub(crate) fn simulated(
        audio: Option<Arc<PreparedAudio>>,
        stop_reason: CaptureStopReason,
    ) -> Self {
        Self::simulated_with_stop_delay(audio, stop_reason, Duration::ZERO)
    }

    #[cfg(test)]
    pub(crate) fn simulated_with_stop_delay(
        audio: Option<Arc<PreparedAudio>>,
        stop_reason: CaptureStopReason,
        stop_delay: Duration,
    ) -> Self {
        Self::simulated_with_abort_probe(audio, stop_reason, stop_delay).0
    }

    #[cfg(test)]
    pub(crate) fn simulated_with_abort_probe(
        audio: Option<Arc<PreparedAudio>>,
        stop_reason: CaptureStopReason,
        stop_delay: Duration,
    ) -> (Self, SimulatedCaptureProbe) {
        let stop_requested = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop_requested);
        let discard_requested = Arc::new(AtomicBool::new(false));
        let worker_discard = Arc::clone(&discard_requested);
        let probe = SimulatedCaptureProbe::default();
        let worker_probe = probe.clone();
        let (finished_tx, finished_rx) = bounded(1);
        let worker = thread::spawn(move || {
            let started = Instant::now();
            while !worker_stop.load(Ordering::Acquire)
                && !worker_discard.load(Ordering::Acquire)
                && started.elapsed() < Duration::from_secs(2)
            {
                thread::sleep(Duration::from_millis(1));
            }
            if worker_discard.load(Ordering::Acquire) {
                worker_probe.stream_dropped.store(true, Ordering::Release);
                let _ = finished_tx.send(Err(CaptureError::Discarded));
                return;
            }
            if worker_stop.load(Ordering::Acquire) && !stop_delay.is_zero() {
                worker_probe
                    .post_roll_entered
                    .store(true, Ordering::Release);
                let deadline = Instant::now() + stop_delay;
                while Instant::now() < deadline {
                    if worker_discard.load(Ordering::Acquire) {
                        worker_probe.stream_dropped.store(true, Ordering::Release);
                        let _ = finished_tx.send(Err(CaptureError::Discarded));
                        return;
                    }
                    thread::sleep(Duration::from_millis(1));
                }
            }
            worker_probe.stream_dropped.store(true, Ordering::Release);
            worker_probe.finish_called.store(true, Ordering::Release);
            worker_probe
                .terminal_preview_called
                .store(true, Ordering::Release);
            let prepared_frames = audio.as_ref().map_or(0, |prepared| prepared.samples.len());
            let source_sample_rate = audio
                .as_ref()
                .map_or(crate::prepared_audio::PREPARED_SAMPLE_RATE, |prepared| {
                    prepared.source_sample_rate
                });
            let source_channels = audio
                .as_ref()
                .map_or(1, |prepared| prepared.source_channels);
            let source_frames = audio.as_ref().map_or(0, |prepared| prepared.source_frames);
            let elapsed = started.elapsed();
            let _ = finished_tx.send(Ok(CaptureCompletion {
                audio,
                stop_reason,
                metrics: CaptureMetrics {
                    duration: elapsed,
                    stop_trigger_elapsed: elapsed,
                    speech_trigger_elapsed: None,
                    source_sample_rate,
                    source_channels,
                    source_frames,
                    prepared_frames,
                    maximum_input_rms: 0.0,
                    maximum_input_peak: 0.0,
                    manual_threshold_crossed: false,
                    dropped_samples: 0,
                    stream_restarts: 0,
                    timing: CaptureTimingMetrics::default(),
                },
            }));
        });
        let abort_probe = probe.clone();
        let session = Self {
            inner: Arc::new(RecordingSessionInner {
                stop_requested,
                discard_requested,
                abort_action: Arc::new(move || {
                    abort_probe
                        .preview_invalidated
                        .store(true, Ordering::Release);
                }),
                finished_rx,
                rms_bits: Arc::new(AtomicU32::new(0.0_f32.to_bits())),
                peak_bits: Arc::new(AtomicU32::new(0.0_f32.to_bits())),
                level_observed: Arc::new(AtomicBool::new(false)),
                level_revision: Arc::new(AtomicU64::new(0)),
                worker: Mutex::new(Some(worker)),
            }),
        };
        (session, probe)
    }

    #[cfg(test)]
    pub(crate) fn set_simulated_telemetry(&self, levels: LevelSnapshot) {
        self.inner
            .rms_bits
            .store(levels.rms.to_bits(), Ordering::Relaxed);
        self.inner
            .peak_bits
            .store(levels.peak.to_bits(), Ordering::Relaxed);
        self.inner.level_observed.store(true, Ordering::Release);
        self.inner.level_revision.fetch_add(1, Ordering::Release);
    }
}

impl Drop for RecordingSessionInner {
    fn drop(&mut self) {
        self.discard_requested.store(true, Ordering::Release);
        (self.abort_action)();
        self.stop_requested.store(true, Ordering::Release);
        if let Some(worker) = self
            .worker
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
        {
            spawn_worker_reaper(worker);
        }
    }
}

fn spawn_worker_reaper(worker: thread::JoinHandle<()>) {
    let _ = thread::Builder::new()
        .name("scribe-audio-reaper".to_owned())
        .spawn(move || {
            let _ = worker.join();
        });
}

pub fn start_recording(
    context: CaptureStartContext,
    max_duration_seconds: u32,
    input_device_name: Option<String>,
    options: CaptureOptions,
    preview_publisher: PreviewPublisherSlot,
    cancellation: CaptureCancellation,
) -> Result<RecordingSession, CaptureError> {
    options.vad.validate()?;
    options.detection_mode.validate()?;
    let stop_requested = Arc::clone(&cancellation.stop_requested);
    let discard_requested = Arc::new(AtomicBool::new(false));
    let abort_preview = preview_publisher.clone();
    let abort_action: Arc<dyn Fn() + Send + Sync> = Arc::new(move || abort_preview.close());
    let rms_bits = Arc::new(AtomicU32::new(0.0_f32.to_bits()));
    let peak_bits = Arc::new(AtomicU32::new(0.0_f32.to_bits()));
    let level_observed = Arc::new(AtomicBool::new(false));
    let level_revision = Arc::new(AtomicU64::new(0));
    let (started_tx, started_rx) = bounded(1);
    let (finished_tx, finished_rx) = bounded(1);

    let worker_stop = Arc::clone(&stop_requested);
    let worker_discard = Arc::clone(&discard_requested);
    let worker_rms = Arc::clone(&rms_bits);
    let worker_peak = Arc::clone(&peak_bits);
    let worker_observed = Arc::clone(&level_observed);
    let worker_level_revision = Arc::clone(&level_revision);
    let worker_cancellation = cancellation.clone();
    let worker = thread::Builder::new()
        .name("scribe-audio-capture".to_owned())
        .spawn(move || {
            let result = capture_worker(
                context,
                max_duration_seconds,
                input_device_name,
                options,
                preview_publisher,
                worker_stop,
                worker_discard,
                worker_rms,
                worker_peak,
                worker_observed,
                worker_level_revision,
                worker_cancellation,
                &started_tx,
            );
            if let Err(error) = &result {
                let _ = started_tx.try_send(Err(error.clone()));
            }
            let _ = finished_tx.send(result);
        })
        .map_err(|error| CaptureError::WorkerSpawn(error.to_string()))?;

    match await_capture_start(&started_rx, &cancellation, worker, START_TIMEOUT) {
        Ok(worker) => Ok(RecordingSession {
            inner: Arc::new(RecordingSessionInner {
                stop_requested,
                discard_requested,
                abort_action,
                finished_rx,
                rms_bits,
                peak_bits,
                level_observed,
                level_revision,
                worker: Mutex::new(Some(worker)),
            }),
        }),
        Err(error) => Err(error),
    }
}

fn await_capture_start(
    started_rx: &Receiver<Result<(), CaptureError>>,
    cancellation: &CaptureCancellation,
    worker: thread::JoinHandle<()>,
    timeout: Duration,
) -> Result<thread::JoinHandle<()>, CaptureError> {
    match started_rx.recv_timeout(timeout) {
        Ok(Ok(())) => Ok(worker),
        Ok(Err(error)) => {
            let _ = worker.join();
            Err(error)
        }
        Err(RecvTimeoutError::Timeout) => {
            cancellation.cancel();
            spawn_worker_reaper(worker);
            Err(CaptureError::StartTimeout(timeout))
        }
        Err(RecvTimeoutError::Disconnected) => {
            cancellation.cancel();
            spawn_worker_reaper(worker);
            Err(CaptureError::WorkerDisconnected)
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn capture_worker(
    context: CaptureStartContext,
    max_duration_seconds: u32,
    input_device_name: Option<String>,
    options: CaptureOptions,
    preview_publisher: PreviewPublisherSlot,
    stop_requested: Arc<AtomicBool>,
    discard_requested: Arc<AtomicBool>,
    rms_bits: Arc<AtomicU32>,
    peak_bits: Arc<AtomicU32>,
    level_observed: Arc<AtomicBool>,
    level_revision: Arc<AtomicU64>,
    cancellation: CaptureCancellation,
    started_tx: &Sender<Result<(), CaptureError>>,
) -> Result<CaptureCompletion, CaptureError> {
    let worker_started = Instant::now();
    let hotkey_to_worker = worker_started.saturating_duration_since(context.observed_at);
    cancellation.ensure_startup_active()?;
    let host = cpal::default_host();
    cancellation.ensure_startup_active()?;
    let device_lookup_started = Instant::now();
    let device = select_input_device(&host, input_device_name.as_deref())?;
    let device_lookup = device_lookup_started.elapsed();
    cancellation.ensure_startup_active()?;
    let supported = device
        .default_input_config()
        .map_err(|error| CaptureError::InputConfiguration(error.to_string()))?;
    cancellation.ensure_startup_active()?;
    let format = InputFormat::from_supported(&supported)?;
    let config: cpal::StreamConfig = supported.into();
    if !input_format_is_credible(format.sample_rate, format.channels) {
        return Err(CaptureError::InvalidInputFormat {
            sample_rate: format.sample_rate,
            channels: format.channels,
        });
    }

    let ring_capacity = usize::try_from(format.sample_rate)
        .unwrap_or(usize::MAX)
        .saturating_mul(format.channels as usize)
        .saturating_mul(DEFAULT_RING_SECONDS)
        .clamp(MIN_RING_SAMPLES, MAX_RING_SAMPLES);
    let (producer, mut consumer) = ring_buffer(ring_capacity);
    let fault = Arc::new(AtomicU8::new(FAULT_NONE));
    let dropped_samples = Arc::new(AtomicUsize::new(0));
    cancellation.ensure_startup_active()?;
    let stream_build_started = Instant::now();
    let mut stream = Some(build_stream(
        &device,
        &config,
        format.sample_format,
        producer,
        Arc::clone(&fault),
        Arc::clone(&dropped_samples),
        cancellation.clone(),
    )?);
    let stream_build = stream_build_started.elapsed();
    let detector = acquire_speech_detector(
        &options,
        &WorkerSpeechDetectorFactory,
        &cancellation.stop_requested,
        Arc::clone(&discard_requested),
    )?;
    let mut pipeline = Pipeline::new(
        format.sample_rate,
        format.channels,
        options,
        detector,
        rms_bits,
        peak_bits,
        level_observed,
        level_revision,
    )?
    .with_preview_slot(preview_publisher);
    if let Err(error) = cancellation.ensure_startup_active() {
        pipeline.cancel_speech_detector()?;
        return Err(error);
    }
    if let Err(error) = cancellation.commit_play() {
        pipeline.cancel_speech_detector()?;
        return Err(error);
    }
    let stream_play_started = Instant::now();
    stream
        .as_ref()
        .expect("stream was just built")
        .play()
        .map_err(|error| CaptureError::PlayStream(error.to_string()))
        .or_else(|error| {
            pipeline.cancel_speech_detector()?;
            Err(error)
        })?;
    let stream_play = stream_play_started.elapsed();
    let capture_started = Instant::now();
    let maximum_duration = Duration::from_secs(
        max_duration_seconds
            .clamp(1, config::MAX_RECORDING_SECONDS)
            .into(),
    );
    let mut explicit_stop: Option<(Instant, usize, Duration)> = None;
    let mut restart_policy = RestartPolicy::new(MAX_STREAM_RESTARTS);
    let mut start_notified = false;
    let mut first_sample = None;

    let (stop_reason, stop_trigger_elapsed) = loop {
        if discard_requested.load(Ordering::Acquire) {
            return discard_capture(&mut stream, &mut consumer, &mut pipeline);
        }
        if let Err(error) = drain_ring_bounded(
            &mut consumer,
            &mut pipeline,
            MAX_DRAIN_SAMPLES_PER_TICK,
            &discard_requested,
        ) {
            if error == CaptureError::Discarded || discard_requested.load(Ordering::Acquire) {
                return discard_capture(&mut stream, &mut consumer, &mut pipeline);
            }
            return Err(error);
        }
        if discard_requested.load(Ordering::Acquire) {
            return discard_capture(&mut stream, &mut consumer, &mut pipeline);
        }
        let elapsed = capture_started.elapsed();
        if cancellation.startup_cancelled_before_first_sample() {
            log_capture_drop(
                context.capture_id,
                hotkey_to_worker,
                device_lookup,
                stream_build,
                stream_play,
                elapsed,
            );
            pipeline.cancel_speech_detector()?;
            return Err(CaptureError::StartupCancelled);
        }
        if !start_notified && cancellation.first_sample_observed() {
            first_sample = Some(elapsed);
            let _ = started_tx.send(Ok(()));
            start_notified = true;
        }
        if explicit_stop.is_none() && stop_requested.load(Ordering::Acquire) {
            explicit_stop = Some((Instant::now(), pipeline.source_frames(), elapsed));
        }
        if explicit_stop.is_none() {
            pipeline.publish_due_previews();
        }
        if pipeline.limit_exceeded() {
            return Err(CaptureError::PreparedAudioLimit {
                maximum_frames: MAX_CAPTURE_PREPARED_FRAMES,
            });
        }

        match fault.load(Ordering::Acquire) {
            FAULT_OVERFLOW => {
                return Err(CaptureError::BufferOverflow {
                    dropped_samples: dropped_samples.load(Ordering::Relaxed).max(1),
                });
            }
            FAULT_STREAM => {
                drop(stream.take());
                let restarted =
                    match retry_stream_start(&mut restart_policy, STREAM_RESTART_BACKOFF, || {
                        if discard_requested.load(Ordering::Acquire) {
                            return Err(CaptureError::Discarded);
                        }
                        let restarted_device =
                            select_input_device(&host, input_device_name.as_deref())?;
                        let current = restarted_device
                            .default_input_config()
                            .map_err(|error| CaptureError::InputConfiguration(error.to_string()))?;
                        ensure_restart_format(format, InputFormat::from_supported(&current)?)?;
                        let producer = consumer.producer_for_restart().ok_or_else(|| {
                            CaptureError::BuildStream(
                                "the previous microphone callback did not quiesce".to_owned(),
                            )
                        })?;
                        fault.store(FAULT_NONE, Ordering::Release);
                        let restarted = build_stream(
                            &restarted_device,
                            &config,
                            format.sample_format,
                            producer,
                            Arc::clone(&fault),
                            Arc::clone(&dropped_samples),
                            cancellation.clone(),
                        )?;
                        if discard_requested.load(Ordering::Acquire) {
                            return Err(CaptureError::Discarded);
                        }
                        restarted
                            .play()
                            .map_err(|error| CaptureError::PlayStream(error.to_string()))?;
                        Ok(restarted)
                    }) {
                        Ok(restarted) => restarted,
                        Err(CaptureError::Discarded) => {
                            return discard_capture(&mut stream, &mut consumer, &mut pipeline);
                        }
                        Err(error) => return Err(error),
                    };
                stream = Some(restarted);
            }
            _ => {}
        }

        if discard_requested.load(Ordering::Acquire) {
            return discard_capture(&mut stream, &mut consumer, &mut pipeline);
        }
        let explicit_post_roll_complete =
            explicit_stop.is_some_and(|(stop_seen, source_frame, _)| {
                let post_roll_frames =
                    duration_to_source_frames(options.vad.post_roll, format.sample_rate);
                pipeline.source_frames() >= source_frame.saturating_add(post_roll_frames)
                    || stop_seen.elapsed() >= options.vad.post_roll
            });
        if let Some(reason) = select_stop_reason(
            explicit_stop.is_some(),
            explicit_post_roll_complete,
            pipeline.endpoint_triggered(),
            elapsed >= maximum_duration,
        ) {
            let trigger_elapsed = explicit_stop.map_or(elapsed, |(_, _, trigger)| trigger);
            break (reason, trigger_elapsed);
        }

        thread::sleep(Duration::from_millis(2));
    };

    let finalization_started = Instant::now();
    drop(stream.take());
    if discard_requested.load(Ordering::Acquire) {
        return discard_capture(&mut stream, &mut consumer, &mut pipeline);
    }
    if let Err(error) = drain_ring_all(&mut consumer, &mut pipeline, &discard_requested) {
        if error == CaptureError::Discarded || discard_requested.load(Ordering::Acquire) {
            return discard_capture(&mut stream, &mut consumer, &mut pipeline);
        }
        return Err(error);
    }
    if discard_requested.load(Ordering::Acquire) {
        return discard_capture(&mut stream, &mut consumer, &mut pipeline);
    }
    if fault.load(Ordering::Acquire) == FAULT_OVERFLOW {
        return Err(CaptureError::BufferOverflow {
            dropped_samples: dropped_samples.load(Ordering::Relaxed).max(1),
        });
    }

    let source_frames = pipeline.source_frames();
    let speech_trigger_elapsed = pipeline.speech_trigger_elapsed();
    let audio = pipeline.finish(stop_reason)?.map(Arc::new);
    let maximum_levels = pipeline.maximum_levels();
    let manual_threshold_crossed = pipeline.manual_threshold_crossed();
    let prepared_frames = audio.as_ref().map_or(0, |audio| audio.samples.len());
    let timing = CaptureTimingMetrics {
        hotkey_to_worker,
        device_lookup,
        stream_build,
        stream_play,
        first_sample: first_sample.unwrap_or_default(),
        release: explicit_stop.map(|(_, _, release)| release),
        finalization: finalization_started.elapsed(),
    };
    log_capture_completion(context.capture_id, timing, prepared_frames == 0);
    Ok(CaptureCompletion {
        audio,
        stop_reason,
        metrics: CaptureMetrics {
            duration: capture_started.elapsed(),
            stop_trigger_elapsed,
            speech_trigger_elapsed,
            source_sample_rate: format.sample_rate,
            source_channels: format.channels,
            source_frames,
            prepared_frames,
            maximum_input_rms: maximum_levels.rms,
            maximum_input_peak: maximum_levels.peak,
            manual_threshold_crossed,
            dropped_samples: dropped_samples.load(Ordering::Relaxed),
            stream_restarts: restart_policy.attempts,
            timing,
        },
    })
}

fn log_capture_drop(
    capture_id: CaptureId,
    hotkey_to_worker: Duration,
    device_lookup: Duration,
    stream_build: Duration,
    stream_play: Duration,
    drop_elapsed: Duration,
) {
    eprintln!(
        "scribe_capture_timing capture_id={} outcome=drop_before_first_sample hotkey_to_worker_us={} device_lookup_us={} stream_build_us={} stream_play_us={} drop_us={}",
        capture_id.0,
        hotkey_to_worker.as_micros(),
        device_lookup.as_micros(),
        stream_build.as_micros(),
        stream_play.as_micros(),
        drop_elapsed.as_micros(),
    );
}

fn log_capture_completion(
    capture_id: CaptureId,
    timing: CaptureTimingMetrics,
    dropped_without_audio: bool,
) {
    eprintln!(
        "scribe_capture_timing capture_id={} outcome={} hotkey_to_worker_us={} device_lookup_us={} stream_build_us={} stream_play_us={} first_sample_us={} release_us={} finalization_us={}",
        capture_id.0,
        if dropped_without_audio {
            "drop_no_audio"
        } else {
            "complete"
        },
        timing.hotkey_to_worker.as_micros(),
        timing.device_lookup.as_micros(),
        timing.stream_build.as_micros(),
        timing.stream_play.as_micros(),
        timing.first_sample.as_micros(),
        timing.release.map_or(0, |release| release.as_micros()),
        timing.finalization.as_micros(),
    );
}

fn drain_ring_bounded(
    consumer: &mut Consumer,
    pipeline: &mut Pipeline,
    maximum: usize,
    discard_requested: &AtomicBool,
) -> Result<usize, CaptureError> {
    let mut drained = 0;
    while drained < maximum
        && let Some(sample) = consumer.pop()
    {
        if discard_requested.load(Ordering::Acquire) {
            return Err(CaptureError::Discarded);
        }
        pipeline.push_interleaved(sample)?;
        drained += 1;
    }
    Ok(drained)
}

fn drain_ring_all(
    consumer: &mut Consumer,
    pipeline: &mut Pipeline,
    discard_requested: &AtomicBool,
) -> Result<(), CaptureError> {
    while drain_ring_bounded(
        consumer,
        pipeline,
        MAX_DRAIN_SAMPLES_PER_TICK,
        discard_requested,
    )? != 0
    {}
    Ok(())
}

fn discard_capture(
    stream: &mut Option<cpal::Stream>,
    consumer: &mut Consumer,
    pipeline: &mut Pipeline,
) -> Result<CaptureCompletion, CaptureError> {
    drop(stream.take());
    consumer.clear();
    pipeline.discard();
    Err(CaptureError::Discarded)
}

fn acquire_speech_detector(
    options: &CaptureOptions,
    factory: &dyn SpeechDetectorFactory,
    startup_cancelled: &AtomicBool,
    discard_requested: Arc<AtomicBool>,
) -> Result<Option<Box<dyn SpeechDetector>>, CaptureError> {
    if options.intent == CaptureIntent::MeterOnly
        || !options.vad_enabled
        || !matches!(options.detection_mode, SpeechDetectionMode::Ai)
    {
        return Ok(None);
    }
    let threshold = VadThreshold::new(0.5).map_err(WorkerSpeechDetector::vad_error)?;
    factory
        .acquire(threshold, startup_cancelled, discard_requested)
        .map(Some)
}

fn duration_to_source_frames(duration: Duration, sample_rate: u32) -> usize {
    let scaled = duration.as_nanos() * u128::from(sample_rate);
    usize::try_from(scaled.div_ceil(1_000_000_000)).unwrap_or(usize::MAX)
}

fn select_stop_reason(
    explicit_pending: bool,
    explicit_post_roll_complete: bool,
    endpoint: bool,
    maximum_duration: bool,
) -> Option<CaptureStopReason> {
    if explicit_pending {
        explicit_post_roll_complete.then_some(CaptureStopReason::Explicit)
    } else if endpoint {
        Some(CaptureStopReason::Endpoint)
    } else if maximum_duration {
        Some(CaptureStopReason::MaximumDuration)
    } else {
        None
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct InputFormat {
    sample_rate: u32,
    channels: u16,
    sample_format: cpal::SampleFormat,
}

impl InputFormat {
    fn from_supported(config: &cpal::SupportedStreamConfig) -> Result<Self, CaptureError> {
        let sample_format = config.sample_format();
        if !matches!(
            sample_format,
            cpal::SampleFormat::F32 | cpal::SampleFormat::I16 | cpal::SampleFormat::U16
        ) {
            return Err(CaptureError::UnsupportedSampleFormat(format!(
                "{sample_format:?}"
            )));
        }
        Ok(Self {
            sample_rate: config.sample_rate().0,
            channels: config.channels(),
            sample_format,
        })
    }
}

fn input_format_is_credible(sample_rate: u32, channels: u16) -> bool {
    (MIN_INPUT_SAMPLE_RATE..=MAX_INPUT_SAMPLE_RATE).contains(&sample_rate)
        && (1..=MAX_INPUT_CHANNELS).contains(&channels)
}

fn ensure_restart_format(expected: InputFormat, current: InputFormat) -> Result<(), CaptureError> {
    if current == expected {
        Ok(())
    } else {
        Err(CaptureError::InputFormatChanged)
    }
}

struct RestartPolicy {
    maximum: u32,
    attempts: u32,
}

impl RestartPolicy {
    fn new(maximum: u32) -> Self {
        Self {
            maximum,
            attempts: 0,
        }
    }

    fn try_restart(&mut self) -> bool {
        if self.attempts >= self.maximum {
            return false;
        }
        self.attempts += 1;
        true
    }
}

fn retry_stream_start<T>(
    policy: &mut RestartPolicy,
    backoff: Duration,
    mut attempt: impl FnMut() -> Result<T, CaptureError>,
) -> Result<T, CaptureError> {
    let mut last_error = "no restart attempt was available".to_owned();
    while policy.try_restart() {
        if policy.attempts > 1 && !backoff.is_zero() {
            thread::sleep(backoff);
        }
        match attempt() {
            Ok(value) => return Ok(value),
            Err(error @ CaptureError::InputFormatChanged) => return Err(error),
            Err(error) => last_error = error.to_string(),
        }
    }
    Err(CaptureError::InputStreamFailed {
        restarts: policy.attempts,
        last_error,
    })
}

fn build_stream(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    sample_format: cpal::SampleFormat,
    mut producer: Producer,
    fault: Arc<AtomicU8>,
    dropped_samples: Arc<AtomicUsize>,
    cancellation: CaptureCancellation,
) -> Result<cpal::Stream, CaptureError> {
    let error_fault = Arc::clone(&fault);
    let error_callback = move |_error| {
        mark_stream_fault(&error_fault);
    };
    let result = match sample_format {
        cpal::SampleFormat::F32 => device.build_input_stream(
            config,
            move |data: &[f32], _| {
                enqueue_samples(
                    data,
                    &mut producer,
                    &fault,
                    &dropped_samples,
                    &cancellation,
                    normalize_f32,
                )
            },
            error_callback,
            None,
        ),
        cpal::SampleFormat::I16 => device.build_input_stream(
            config,
            move |data: &[i16], _| {
                enqueue_samples(
                    data,
                    &mut producer,
                    &fault,
                    &dropped_samples,
                    &cancellation,
                    normalize_i16,
                )
            },
            error_callback,
            None,
        ),
        cpal::SampleFormat::U16 => device.build_input_stream(
            config,
            move |data: &[u16], _| {
                enqueue_samples(
                    data,
                    &mut producer,
                    &fault,
                    &dropped_samples,
                    &cancellation,
                    normalize_u16,
                )
            },
            error_callback,
            None,
        ),
        other => return Err(CaptureError::UnsupportedSampleFormat(format!("{other:?}"))),
    };
    result.map_err(|error| CaptureError::BuildStream(error.to_string()))
}

fn mark_stream_fault(fault: &AtomicU8) {
    let _ = fault.compare_exchange(
        FAULT_NONE,
        FAULT_STREAM,
        Ordering::AcqRel,
        Ordering::Relaxed,
    );
}

fn enqueue_samples<T: Copy>(
    data: &[T],
    producer: &mut Producer,
    fault: &AtomicU8,
    dropped_samples: &AtomicUsize,
    cancellation: &CaptureCancellation,
    normalize: fn(T) -> f32,
) {
    if data.is_empty() || !cancellation.observe_first_sample() {
        return;
    }
    if fault.load(Ordering::Relaxed) != FAULT_NONE {
        dropped_samples.fetch_add(data.len(), Ordering::Relaxed);
        return;
    }
    for (index, sample) in data.iter().copied().enumerate() {
        if producer.push(normalize(sample)).is_err() {
            dropped_samples.fetch_add(data.len() - index, Ordering::Relaxed);
            let _ = fault.compare_exchange(
                FAULT_NONE,
                FAULT_OVERFLOW,
                Ordering::AcqRel,
                Ordering::Relaxed,
            );
            return;
        }
    }
}

fn normalize_f32(sample: f32) -> f32 {
    if sample.is_finite() {
        sample.clamp(-1.0, 1.0)
    } else {
        0.0
    }
}

fn normalize_i16(sample: i16) -> f32 {
    sample as f32 / 32_768.0
}

fn normalize_u16(sample: u16) -> f32 {
    (sample as i32 - 32_768) as f32 / 32_768.0
}

fn select_input_device(
    host: &cpal::Host,
    input_device_name: Option<&str>,
) -> Result<cpal::Device, CaptureError> {
    if let Some(target_name) = input_device_name.filter(|name| !name.trim().is_empty()) {
        let devices = host
            .input_devices()
            .map_err(|error| CaptureError::DeviceEnumeration(error.to_string()))?;
        for device in devices {
            if device.name().ok().as_deref() == Some(target_name) {
                return Ok(device);
            }
        }
        return Err(CaptureError::NoInputDevice {
            requested: Some(target_name.to_owned()),
        });
    }

    host.default_input_device()
        .ok_or_else(|| CaptureError::NoInputDevice {
            requested: input_device_name.map(str::to_owned),
        })
}

pub fn cleanup_abandoned_recordings() -> Result<usize> {
    let dir = recording_dir()?;
    if !dir.exists() {
        return Ok(0);
    }
    let mut removed = 0;
    for entry in fs::read_dir(&dir)
        .with_context(|| format!("failed to inspect recording directory {}", dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        let is_recording = path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("recording-") && name.ends_with(".wav"));
        let stale = entry
            .metadata()
            .ok()
            .and_then(|metadata| metadata.modified().ok())
            .and_then(|modified| modified.elapsed().ok())
            .is_some_and(|age| age >= Duration::from_secs(24 * 60 * 60));
        if is_recording && stale && fs::remove_file(&path).is_ok() {
            removed += 1;
        }
    }
    Ok(removed)
}

pub fn input_device_names() -> Result<Vec<String>> {
    let host = cpal::default_host();
    let mut names = host
        .input_devices()
        .context("failed to enumerate microphone devices")?
        .filter_map(|device| device.name().ok())
        .filter(|name| !name.trim().is_empty())
        .collect::<Vec<_>>();
    names.sort();
    names.dedup();
    Ok(names)
}

fn recording_dir() -> Result<std::path::PathBuf> {
    Ok(config::cache_dir()?.join("recordings"))
}

#[cfg(test)]
mod tests {
    use std::sync::Barrier;

    use super::*;

    struct CountingDetectorFactory {
        calls: AtomicUsize,
        threshold_bits: AtomicU32,
    }

    struct NoopDetector;

    struct CancellationBlockingDetectorFactory {
        entered: Sender<()>,
    }

    impl SpeechDetectorFactory for CountingDetectorFactory {
        fn acquire(
            &self,
            threshold: VadThreshold,
            _cancelled: &AtomicBool,
            _discard_requested: Arc<AtomicBool>,
        ) -> Result<Box<dyn SpeechDetector>, CaptureError> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            self.threshold_bits
                .store(threshold.value().to_bits(), Ordering::Relaxed);
            Ok(Box::new(NoopDetector))
        }
    }

    impl SpeechDetectorFactory for CancellationBlockingDetectorFactory {
        fn acquire(
            &self,
            _threshold: VadThreshold,
            cancelled: &AtomicBool,
            _discard_requested: Arc<AtomicBool>,
        ) -> Result<Box<dyn SpeechDetector>, CaptureError> {
            self.entered.send(()).unwrap();
            while !cancelled.load(Ordering::Acquire) {
                thread::sleep(Duration::from_millis(1));
            }
            Err(CaptureError::SpeechDetection(
                "injected acquisition cancellation".to_owned(),
            ))
        }
    }

    impl SpeechDetector for NoopDetector {
        fn compute(
            &mut self,
            _samples: &[f32; WINDOW_SAMPLES],
        ) -> Result<SileroVadDecision, CaptureError> {
            Ok(SileroVadDecision {
                probability: 0.0,
                speech: false,
            })
        }

        fn finish(&mut self) -> Result<(), CaptureError> {
            Ok(())
        }

        fn cancel(&mut self) -> Result<(), CaptureError> {
            Ok(())
        }
    }

    #[test]
    fn meter_only_capture_makes_zero_speech_detector_factory_calls() {
        let factory = CountingDetectorFactory {
            calls: AtomicUsize::new(0),
            threshold_bits: AtomicU32::new(f32::NAN.to_bits()),
        };
        let options = CaptureOptions {
            intent: CaptureIntent::MeterOnly,
            ..CaptureOptions::default()
        };

        assert!(
            acquire_speech_detector(
                &options,
                &factory,
                &AtomicBool::new(false),
                Arc::new(AtomicBool::new(false)),
            )
            .unwrap()
            .is_none()
        );
        assert_eq!(factory.calls.load(Ordering::Relaxed), 0);

        let detector = acquire_speech_detector(
            &CaptureOptions::default(),
            &factory,
            &AtomicBool::new(false),
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap()
        .unwrap();
        assert_eq!(factory.calls.load(Ordering::Relaxed), 1);
        assert_eq!(
            f32::from_bits(factory.threshold_bits.load(Ordering::Relaxed)),
            0.5
        );
        drop(detector);
    }

    #[test]
    fn capture_ids_are_monotonic_across_all_audio_flows() {
        let observed_at = Instant::now();
        let first = CaptureStartContext::observed_at(observed_at);
        let second = CaptureStartContext::observed_at(observed_at);

        assert!(second.capture_id.0 > first.capture_id.0);
        assert_eq!(first.observed_at, observed_at);
        assert_eq!(second.observed_at, observed_at);
    }

    #[test]
    fn ai_detection_uses_the_fixed_default_threshold_and_manual_detection_skips_silero() {
        let factory = CountingDetectorFactory {
            calls: AtomicUsize::new(0),
            threshold_bits: AtomicU32::new(f32::NAN.to_bits()),
        };
        let options = CaptureOptions {
            detection_mode: SpeechDetectionMode::ManualThreshold { threshold_rms: 0.1 },
            ..CaptureOptions::default()
        };

        assert!(
            acquire_speech_detector(
                &options,
                &factory,
                &AtomicBool::new(false),
                Arc::new(AtomicBool::new(false)),
            )
            .unwrap()
            .is_none()
        );
        assert_eq!(factory.calls.load(Ordering::Relaxed), 0);

        drop(
            acquire_speech_detector(
                &CaptureOptions::default(),
                &factory,
                &AtomicBool::new(false),
                Arc::new(AtomicBool::new(false)),
            )
            .unwrap(),
        );
        assert_eq!(factory.calls.load(Ordering::Relaxed), 1);
        assert_eq!(
            f32::from_bits(factory.threshold_bits.load(Ordering::Relaxed)),
            0.5
        );
    }

    #[test]
    fn cancelled_vad_acquisition_joins_capture_worker_without_output() {
        let cancellation = CaptureCancellation::new();
        let worker_cancellation = cancellation.clone();
        let (entered_tx, entered_rx) = bounded(1);
        let (started_tx, started_rx) = bounded(1);
        let output_delivered = Arc::new(AtomicBool::new(false));
        let worker_output = Arc::clone(&output_delivered);
        let lifetime = Arc::new(());
        let weak_lifetime = Arc::downgrade(&lifetime);
        let worker_lifetime = Arc::clone(&lifetime);
        drop(lifetime);
        let worker = thread::spawn(move || {
            let _lifetime = worker_lifetime;
            let factory = CancellationBlockingDetectorFactory {
                entered: entered_tx,
            };
            let result = acquire_speech_detector(
                &CaptureOptions::default(),
                &factory,
                &worker_cancellation.stop_requested,
                Arc::new(AtomicBool::new(false)),
            );
            if result.is_ok() {
                worker_output.store(true, Ordering::Release);
            }
            let _ = started_tx.send(result.map(|_| ()));
        });

        entered_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        cancellation.cancel();
        let error = await_capture_start(
            &started_rx,
            &cancellation,
            worker,
            Duration::from_millis(250),
        )
        .unwrap_err();

        assert!(matches!(error, CaptureError::SpeechDetection(_)));
        assert!(!output_delivered.load(Ordering::Acquire));
        assert!(weak_lifetime.upgrade().is_none());
    }

    #[test]
    fn sample_formats_convert_to_finite_unit_range() {
        assert_eq!(normalize_f32(f32::NAN), 0.0);
        assert_eq!(normalize_f32(f32::NEG_INFINITY), 0.0);
        assert_eq!(normalize_f32(-2.0), -1.0);
        assert_eq!(normalize_f32(2.0), 1.0);
        assert_eq!(normalize_i16(i16::MIN), -1.0);
        assert_eq!(normalize_i16(16_384), 0.5);
        assert_eq!(normalize_u16(0), -1.0);
        assert_eq!(normalize_u16(32_768), 0.0);
        assert!(normalize_u16(u16::MAX) < 1.0);
    }

    #[test]
    fn callback_overflow_sets_a_structured_fault_and_counts_drops() {
        let (mut producer, mut consumer) = ring_buffer(2);
        let fault = AtomicU8::new(FAULT_NONE);
        let dropped = AtomicUsize::new(0);
        enqueue_samples(
            &[1_i16, 2, 3, 4],
            &mut producer,
            &fault,
            &dropped,
            &CaptureCancellation {
                stop_requested: Arc::new(AtomicBool::new(false)),
                startup_state: Arc::new(AtomicU8::new(STARTUP_FIRST_SAMPLE)),
            },
            normalize_i16,
        );

        assert_eq!(fault.load(Ordering::Acquire), FAULT_OVERFLOW);
        assert_eq!(dropped.load(Ordering::Relaxed), 2);
        assert_eq!(consumer.pop(), Some(normalize_i16(1)));
        assert_eq!(consumer.pop(), Some(normalize_i16(2)));
    }

    #[test]
    fn release_after_play_but_before_first_sample_prevents_callback_activation() {
        let cancellation = CaptureCancellation::new();
        cancellation.commit_play().unwrap();
        cancellation.cancel();
        let (mut producer, mut consumer) = ring_buffer(4);
        let fault = AtomicU8::new(FAULT_NONE);
        let dropped = AtomicUsize::new(0);

        enqueue_samples(
            &[1_i16, 2],
            &mut producer,
            &fault,
            &dropped,
            &cancellation,
            normalize_i16,
        );

        assert!(cancellation.startup_cancelled_before_first_sample());
        assert_eq!(consumer.pop(), None);
    }

    #[test]
    fn concurrent_release_and_first_sample_have_one_atomic_winner() {
        for _ in 0..1_000 {
            let cancellation = CaptureCancellation::new();
            cancellation.commit_play().unwrap();
            let start = Arc::new(Barrier::new(3));

            let cancel_thread = {
                let cancellation = cancellation.clone();
                let start = Arc::clone(&start);
                thread::spawn(move || {
                    start.wait();
                    let cancelled_before_sample = cancellation.cancel_startup();
                    cancellation.stop_requested.store(true, Ordering::Release);
                    cancelled_before_sample
                })
            };
            let callback_thread = {
                let cancellation = cancellation.clone();
                let start = Arc::clone(&start);
                thread::spawn(move || {
                    start.wait();
                    cancellation.observe_first_sample()
                })
            };

            start.wait();
            let cancelled_before_sample = cancel_thread.join().unwrap();
            let first_sample_observed = callback_thread.join().unwrap();
            assert_ne!(
                cancelled_before_sample, first_sample_observed,
                "cancellation and first-sample activation must be mutually exclusive"
            );
        }
    }

    #[test]
    fn release_after_first_sample_preserves_normal_stop_and_post_roll_path() {
        let cancellation = CaptureCancellation::new();
        cancellation.commit_play().unwrap();
        let (mut producer, mut consumer) = ring_buffer(4);
        let fault = AtomicU8::new(FAULT_NONE);
        let dropped = AtomicUsize::new(0);
        enqueue_samples(
            &[1_i16],
            &mut producer,
            &fault,
            &dropped,
            &cancellation,
            normalize_i16,
        );

        cancellation.cancel();

        assert!(cancellation.first_sample_observed());
        assert!(!cancellation.startup_cancelled_before_first_sample());
        assert_eq!(consumer.pop(), Some(normalize_i16(1)));
    }

    #[test]
    fn explicit_stop_has_priority_over_endpoint_and_maximum_duration() {
        assert_eq!(
            select_stop_reason(true, true, true, true),
            Some(CaptureStopReason::Explicit)
        );
        assert_eq!(select_stop_reason(true, false, true, true), None);
        assert_eq!(
            select_stop_reason(false, false, true, true),
            Some(CaptureStopReason::Endpoint)
        );
        assert_eq!(
            select_stop_reason(false, false, false, true),
            Some(CaptureStopReason::MaximumDuration)
        );
    }

    #[test]
    fn restart_policy_is_bounded() {
        let mut policy = RestartPolicy::new(2);
        assert!(policy.try_restart());
        assert!(policy.try_restart());
        assert!(!policy.try_restart());
        assert_eq!(policy.attempts, 2);
    }

    #[test]
    fn complete_stream_rebuild_is_retried_until_it_succeeds() {
        let mut policy = RestartPolicy::new(2);
        let mut attempts = 0;
        let result = retry_stream_start(&mut policy, Duration::ZERO, || {
            attempts += 1;
            if attempts == 1 {
                Err(CaptureError::BuildStream(
                    "injected first failure".to_owned(),
                ))
            } else {
                Ok("restarted")
            }
        });

        assert_eq!(result.unwrap(), "restarted");
        assert_eq!(attempts, 2);
        assert_eq!(policy.attempts, 2);
    }

    #[test]
    fn stream_rebuild_exhaustion_retains_the_last_structured_error() {
        let mut policy = RestartPolicy::new(2);
        let error = retry_stream_start::<()>(&mut policy, Duration::ZERO, || {
            Err(CaptureError::PlayStream(
                "device is still absent".to_owned(),
            ))
        })
        .unwrap_err();

        assert_eq!(
            error,
            CaptureError::InputStreamFailed {
                restarts: 2,
                last_error: "failed to start microphone input stream: device is still absent"
                    .to_owned(),
            }
        );
    }

    #[test]
    fn credible_input_format_bounds_reject_resource_amplification() {
        assert!(input_format_is_credible(8_000, 1));
        assert!(input_format_is_credible(384_000, 32));
        assert!(!input_format_is_credible(1, 1));
        assert!(!input_format_is_credible(48_000, 33));
    }

    #[test]
    fn stream_faults_are_atomic_and_do_not_hide_an_overflow() {
        let stream_fault = AtomicU8::new(FAULT_NONE);
        mark_stream_fault(&stream_fault);
        assert_eq!(stream_fault.load(Ordering::Acquire), FAULT_STREAM);

        let overflow = AtomicU8::new(FAULT_OVERFLOW);
        mark_stream_fault(&overflow);
        assert_eq!(overflow.load(Ordering::Acquire), FAULT_OVERFLOW);
    }

    #[test]
    fn restart_rejects_any_changed_input_format() {
        let expected = InputFormat {
            sample_rate: 48_000,
            channels: 2,
            sample_format: cpal::SampleFormat::F32,
        };
        assert_eq!(ensure_restart_format(expected, expected), Ok(()));
        assert_eq!(
            ensure_restart_format(
                expected,
                InputFormat {
                    sample_rate: 44_100,
                    ..expected
                }
            ),
            Err(CaptureError::InputFormatChanged)
        );
    }

    #[test]
    fn stop_and_discard_drops_in_memory_audio_after_worker_shutdown() {
        let audio = Arc::new(PreparedAudio {
            samples: vec![0.25; 160],
            sample_rate: crate::prepared_audio::PREPARED_SAMPLE_RATE,
            source_sample_rate: crate::prepared_audio::PREPARED_SAMPLE_RATE,
            source_channels: 1,
            source_frames: 160,
        });
        let weak = Arc::downgrade(&audio);
        let session =
            RecordingSession::simulated(Some(Arc::clone(&audio)), CaptureStopReason::Explicit);
        drop(audio);

        session.stop_and_discard(Duration::from_secs(1)).unwrap();
        assert!(weak.upgrade().is_none());
    }

    #[test]
    fn dropping_a_recording_session_requests_stop_and_reaps_its_worker() {
        let audio = Arc::new(PreparedAudio {
            samples: vec![0.25; 160],
            sample_rate: crate::prepared_audio::PREPARED_SAMPLE_RATE,
            source_sample_rate: crate::prepared_audio::PREPARED_SAMPLE_RATE,
            source_channels: 1,
            source_frames: 160,
        });
        let weak = Arc::downgrade(&audio);
        let session =
            RecordingSession::simulated(Some(Arc::clone(&audio)), CaptureStopReason::Explicit);
        drop(audio);

        drop(session);
        let deadline = Instant::now() + Duration::from_secs(1);
        while weak.upgrade().is_some() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(1));
        }

        assert!(weak.upgrade().is_none());
    }

    #[test]
    fn simulated_telemetry_advances_the_latest_meter_revision() {
        let session = RecordingSession::simulated(None, CaptureStopReason::Explicit);
        assert_eq!(session.latest_level_revision(), 0);

        session.set_simulated_telemetry(LevelSnapshot {
            rms: 0.02,
            peak: 0.04,
        });
        assert_eq!(session.latest_level_revision(), 1);
        assert_eq!(session.latest_levels().peak, 0.04);

        session.set_simulated_telemetry(LevelSnapshot::default());
        assert_eq!(session.latest_level_revision(), 2);
        session.stop_and_discard(Duration::from_secs(1)).unwrap();
    }

    #[test]
    fn startup_timeout_returns_before_blocked_worker_is_reaped() {
        let cancellation = CaptureCancellation::new();
        let awaiting_cancellation = cancellation.clone();
        let (started_tx, started_rx) = bounded(1);
        let (worker_blocked_tx, worker_blocked_rx) = bounded(1);
        let (release_tx, release_rx) = bounded(1);
        let (worker_done_tx, worker_done_rx) = bounded(1);
        let worker = thread::spawn(move || {
            let _started_tx = started_tx;
            worker_blocked_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            worker_done_tx.send(()).unwrap();
        });
        worker_blocked_rx.recv().unwrap();

        let (result_tx, result_rx) = bounded(1);
        let waiter = thread::spawn(move || {
            result_tx
                .send(await_capture_start(
                    &started_rx,
                    &awaiting_cancellation,
                    worker,
                    Duration::from_millis(5),
                ))
                .unwrap();
        });
        let result = result_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("startup timeout must not wait for the blocked worker");

        assert!(matches!(
            result,
            Err(CaptureError::StartTimeout(timeout)) if timeout == Duration::from_millis(5)
        ));
        assert!(cancellation.is_cancelled());
        assert!(matches!(
            worker_done_rx.try_recv(),
            Err(TryRecvError::Empty)
        ));

        release_tx.send(()).unwrap();
        worker_done_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("released worker should finish for reaping");
        waiter.join().unwrap();
    }
}
