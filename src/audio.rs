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
use crate::config::settings::{
    DEFAULT_MANUAL_ACTIVATION_RMS, MAX_MANUAL_ACTIVATION_RMS, MIN_MANUAL_ACTIVATION_RMS,
};
use crate::prepared_audio::PreparedAudio;
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
const MAX_CAPTURE_PREPARED_FRAMES: usize = 16_000 * (config::MAX_RECORDING_SECONDS as usize + 2);
pub(super) const MIN_INPUT_SAMPLE_RATE: u32 = 8_000;
pub(super) const MAX_INPUT_SAMPLE_RATE: u32 = 384_000;
pub(super) const MAX_INPUT_CHANNELS: u16 = 32;
pub(crate) const MIN_SPEECH_ACTIVATION_RMS: f32 = 0.012;
const FAULT_NONE: u8 = 0;
const FAULT_OVERFLOW: u8 = 1;
const FAULT_STREAM: u8 = 2;
const STARTUP_PENDING: u8 = 0;
const STARTUP_PLAY_COMMITTED: u8 = 1;
const STARTUP_CANCELLED: u8 = 2;

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
    pub vad: VadOptions,
    pub sensitivity: Sensitivity,
    pub intent: CaptureIntent,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CaptureIntent {
    Dictation,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Sensitivity {
    Automatic,
    Manual { activation_rms: f32 },
}

impl Sensitivity {
    fn validate(self) -> Result<(), CaptureError> {
        if matches!(self, Self::Manual { activation_rms } if !activation_rms.is_finite() || !(MIN_MANUAL_ACTIVATION_RMS..=MAX_MANUAL_ACTIVATION_RMS).contains(&activation_rms))
        {
            return Err(CaptureError::InvalidOptions(
                "manual voice activation threshold must be between 0.000251 and 1.0 RMS",
            ));
        }
        Ok(())
    }
}

impl CaptureOptions {
    pub fn new(vad: VadOptions) -> Self {
        Self {
            vad_enabled: true,
            endpointing_enabled: true,
            vad,
            sensitivity: Sensitivity::Automatic,
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
        self.stop_requested.store(true, Ordering::Release);
        let _ = self.startup_state.compare_exchange(
            STARTUP_PENDING,
            STARTUP_CANCELLED,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
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
    /// Maximum native RMS observed across 10 ms VAD signal frames.
    pub maximum_input_rms: f32,
    /// Maximum native sample peak observed by the 30 ms meter windows.
    pub maximum_input_peak: f32,
    pub dropped_samples: usize,
    pub stream_restarts: u32,
}

#[derive(Clone, Debug)]
pub struct CaptureCompletion {
    pub audio: Option<Arc<PreparedAudio>>,
    pub stop_reason: CaptureStopReason,
    pub metrics: CaptureMetrics,
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
    #[error("audio recorder did not stop within {0:?}")]
    StopTimeout(Duration),
    #[error("audio recorder worker disconnected unexpectedly")]
    WorkerDisconnected,
    #[error("failed to spawn audio recorder worker: {0}")]
    WorkerSpawn(String),
}

pub struct RecordingSession {
    stop_requested: Arc<AtomicBool>,
    finished_rx: Receiver<Result<CaptureCompletion, CaptureError>>,
    rms_bits: Arc<AtomicU32>,
    peak_bits: Arc<AtomicU32>,
    level_observed: Arc<AtomicBool>,
    manual_activation_threshold_bits: Arc<AtomicU32>,
    worker: Mutex<Option<thread::JoinHandle<()>>>,
}

impl RecordingSession {
    pub fn stop(&self) {
        self.stop_requested.store(true, Ordering::Release);
    }

    pub fn try_finish(&self) -> Option<Result<CaptureCompletion, CaptureError>> {
        let result = match self.finished_rx.try_recv() {
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
            rms: f32::from_bits(self.rms_bits.load(Ordering::Relaxed)).clamp(0.0, 1.0),
            peak: f32::from_bits(self.peak_bits.load(Ordering::Relaxed)).clamp(0.0, 1.0),
        }
    }

    pub fn has_level_update(&self) -> bool {
        self.level_observed.load(Ordering::Acquire)
    }

    pub fn set_manual_activation_threshold(&self, activation_rms: f32) {
        if activation_rms.is_finite() {
            self.manual_activation_threshold_bits.store(
                activation_rms
                    .clamp(MIN_MANUAL_ACTIVATION_RMS, MAX_MANUAL_ACTIVATION_RMS)
                    .to_bits(),
                Ordering::Release,
            );
        }
    }

    #[cfg(test)]
    pub(crate) fn manual_activation_threshold(&self) -> f32 {
        f32::from_bits(
            self.manual_activation_threshold_bits
                .load(Ordering::Acquire),
        )
    }

    pub fn stop_and_discard(self, timeout: Duration) -> Result<(), CaptureError> {
        self.stop();
        let result = match self.finished_rx.recv_timeout(timeout) {
            Ok(Ok(_completion)) => Ok(()),
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
        self.worker
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
        let stop_requested = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop_requested);
        let (finished_tx, finished_rx) = bounded(1);
        let worker = thread::spawn(move || {
            let started = Instant::now();
            while !worker_stop.load(Ordering::Acquire) && started.elapsed() < Duration::from_secs(2)
            {
                thread::sleep(Duration::from_millis(1));
            }
            if worker_stop.load(Ordering::Acquire) {
                thread::sleep(stop_delay);
            }
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
                    dropped_samples: 0,
                    stream_restarts: 0,
                },
            }));
        });
        Self {
            stop_requested,
            finished_rx,
            rms_bits: Arc::new(AtomicU32::new(0.0_f32.to_bits())),
            peak_bits: Arc::new(AtomicU32::new(0.0_f32.to_bits())),
            level_observed: Arc::new(AtomicBool::new(false)),
            manual_activation_threshold_bits: Arc::new(AtomicU32::new(
                DEFAULT_MANUAL_ACTIVATION_RMS.to_bits(),
            )),
            worker: Mutex::new(Some(worker)),
        }
    }
}

impl Drop for RecordingSession {
    fn drop(&mut self) {
        self.stop_requested.store(true, Ordering::Release);
        self.reap_worker();
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
    max_duration_seconds: u32,
    input_device_name: Option<String>,
    options: CaptureOptions,
    preview_publisher: Option<PreviewAudioPublisher>,
    cancellation: CaptureCancellation,
) -> Result<RecordingSession, CaptureError> {
    options.vad.validate()?;
    options.sensitivity.validate()?;
    let stop_requested = Arc::clone(&cancellation.stop_requested);
    let rms_bits = Arc::new(AtomicU32::new(0.0_f32.to_bits()));
    let peak_bits = Arc::new(AtomicU32::new(0.0_f32.to_bits()));
    let level_observed = Arc::new(AtomicBool::new(false));
    let level_revision = Arc::new(AtomicU64::new(0));
    let vad_threshold_bits = Arc::new(AtomicU32::new(MIN_SPEECH_ACTIVATION_RMS.to_bits()));
    let manual_activation_threshold_bits = Arc::new(AtomicU32::new(
        match options.sensitivity {
            Sensitivity::Automatic => DEFAULT_MANUAL_ACTIVATION_RMS,
            Sensitivity::Manual { activation_rms } => activation_rms,
        }
        .to_bits(),
    ));
    let speech_detected = Arc::new(AtomicBool::new(false));
    let (started_tx, started_rx) = bounded(1);
    let (finished_tx, finished_rx) = bounded(1);

    let worker_stop = Arc::clone(&stop_requested);
    let worker_rms = Arc::clone(&rms_bits);
    let worker_peak = Arc::clone(&peak_bits);
    let worker_observed = Arc::clone(&level_observed);
    let worker_level_revision = Arc::clone(&level_revision);
    let worker_vad_threshold = Arc::clone(&vad_threshold_bits);
    let worker_manual_activation_threshold = Arc::clone(&manual_activation_threshold_bits);
    let worker_speech_detected = Arc::clone(&speech_detected);
    let worker_cancellation = cancellation.clone();
    let worker = thread::Builder::new()
        .name("scribe-audio-capture".to_owned())
        .spawn(move || {
            let result = capture_worker(
                max_duration_seconds,
                input_device_name,
                options,
                preview_publisher,
                worker_stop,
                worker_rms,
                worker_peak,
                worker_observed,
                worker_level_revision,
                worker_vad_threshold,
                worker_manual_activation_threshold,
                worker_speech_detected,
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
            stop_requested,
            finished_rx,
            rms_bits,
            peak_bits,
            level_observed,
            manual_activation_threshold_bits,
            worker: Mutex::new(Some(worker)),
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
    max_duration_seconds: u32,
    input_device_name: Option<String>,
    options: CaptureOptions,
    preview_publisher: Option<PreviewAudioPublisher>,
    stop_requested: Arc<AtomicBool>,
    rms_bits: Arc<AtomicU32>,
    peak_bits: Arc<AtomicU32>,
    level_observed: Arc<AtomicBool>,
    level_revision: Arc<AtomicU64>,
    vad_threshold_bits: Arc<AtomicU32>,
    manual_activation_threshold_bits: Arc<AtomicU32>,
    speech_detected: Arc<AtomicBool>,
    cancellation: CaptureCancellation,
    started_tx: &Sender<Result<(), CaptureError>>,
) -> Result<CaptureCompletion, CaptureError> {
    cancellation.ensure_startup_active()?;
    let host = cpal::default_host();
    cancellation.ensure_startup_active()?;
    let device = select_input_device(&host, input_device_name.as_deref())?;
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
    let mut stream = Some(build_stream(
        &device,
        &config,
        format.sample_format,
        producer,
        Arc::clone(&fault),
        Arc::clone(&dropped_samples),
    )?);
    cancellation.ensure_startup_active()?;
    cancellation.commit_play()?;
    stream
        .as_ref()
        .expect("stream was just built")
        .play()
        .map_err(|error| CaptureError::PlayStream(error.to_string()))?;
    let _ = started_tx.send(Ok(()));

    let mut pipeline = Pipeline::new(
        format.sample_rate,
        format.channels,
        options,
        rms_bits,
        peak_bits,
        level_observed,
        level_revision,
    )?
    .with_vad_telemetry(
        vad_threshold_bits,
        manual_activation_threshold_bits,
        speech_detected,
    )
    .with_preview_publisher(preview_publisher);
    let capture_started = Instant::now();
    let maximum_duration = Duration::from_secs(
        max_duration_seconds
            .clamp(1, config::MAX_RECORDING_SECONDS)
            .into(),
    );
    let mut explicit_stop: Option<(Instant, usize, Duration)> = None;
    let mut restart_policy = RestartPolicy::new(MAX_STREAM_RESTARTS);

    let (stop_reason, stop_trigger_elapsed) = loop {
        drain_ring_bounded(&mut consumer, &mut pipeline, MAX_DRAIN_SAMPLES_PER_TICK);
        pipeline.publish_due_previews();
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
                    retry_stream_start(&mut restart_policy, STREAM_RESTART_BACKOFF, || {
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
                        )?;
                        restarted
                            .play()
                            .map_err(|error| CaptureError::PlayStream(error.to_string()))?;
                        Ok(restarted)
                    })?;
                stream = Some(restarted);
            }
            _ => {}
        }

        let elapsed = capture_started.elapsed();
        if explicit_stop.is_none() && stop_requested.load(Ordering::Acquire) {
            explicit_stop = Some((Instant::now(), pipeline.source_frames(), elapsed));
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

    drop(stream.take());
    drain_ring_all(&mut consumer, &mut pipeline);
    pipeline.publish_due_previews();
    if fault.load(Ordering::Acquire) == FAULT_OVERFLOW {
        return Err(CaptureError::BufferOverflow {
            dropped_samples: dropped_samples.load(Ordering::Relaxed).max(1),
        });
    }

    let source_frames = pipeline.source_frames();
    let speech_trigger_elapsed = pipeline.speech_trigger_elapsed();
    let audio = pipeline.finish(stop_reason)?.map(Arc::new);
    let maximum_levels = pipeline.maximum_levels();
    let prepared_frames = audio.as_ref().map_or(0, |audio| audio.samples.len());
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
            dropped_samples: dropped_samples.load(Ordering::Relaxed),
            stream_restarts: restart_policy.attempts,
        },
    })
}

fn drain_ring_bounded(consumer: &mut Consumer, pipeline: &mut Pipeline, maximum: usize) -> usize {
    let mut drained = 0;
    while drained < maximum
        && let Some(sample) = consumer.pop()
    {
        pipeline.push_interleaved(sample);
        drained += 1;
    }
    drained
}

fn drain_ring_all(consumer: &mut Consumer, pipeline: &mut Pipeline) {
    while drain_ring_bounded(consumer, pipeline, MAX_DRAIN_SAMPLES_PER_TICK) != 0 {}
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
) -> Result<cpal::Stream, CaptureError> {
    let error_fault = Arc::clone(&fault);
    let error_callback = move |_error| {
        mark_stream_fault(&error_fault);
    };
    let result = match sample_format {
        cpal::SampleFormat::F32 => device.build_input_stream(
            config,
            move |data: &[f32], _| {
                enqueue_samples(data, &mut producer, &fault, &dropped_samples, normalize_f32)
            },
            error_callback,
            None,
        ),
        cpal::SampleFormat::I16 => device.build_input_stream(
            config,
            move |data: &[i16], _| {
                enqueue_samples(data, &mut producer, &fault, &dropped_samples, normalize_i16)
            },
            error_callback,
            None,
        ),
        cpal::SampleFormat::U16 => device.build_input_stream(
            config,
            move |data: &[u16], _| {
                enqueue_samples(data, &mut producer, &fault, &dropped_samples, normalize_u16)
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
    normalize: fn(T) -> f32,
) {
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
    use super::*;

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
            normalize_i16,
        );

        assert_eq!(fault.load(Ordering::Acquire), FAULT_OVERFLOW);
        assert_eq!(dropped.load(Ordering::Relaxed), 2);
        assert_eq!(consumer.pop(), Some(normalize_i16(1)));
        assert_eq!(consumer.pop(), Some(normalize_i16(2)));
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
    fn session_manual_threshold_updates_are_bounded_and_ignore_non_finite_values() {
        let session = RecordingSession::simulated(None, CaptureStopReason::Explicit);
        session.set_manual_activation_threshold(f32::INFINITY);
        assert_eq!(
            f32::from_bits(
                session
                    .manual_activation_threshold_bits
                    .load(Ordering::Acquire)
            ),
            DEFAULT_MANUAL_ACTIVATION_RMS
        );

        session.set_manual_activation_threshold(MAX_MANUAL_ACTIVATION_RMS * 2.0);
        assert_eq!(
            f32::from_bits(
                session
                    .manual_activation_threshold_bits
                    .load(Ordering::Acquire)
            ),
            MAX_MANUAL_ACTIVATION_RMS
        );
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
