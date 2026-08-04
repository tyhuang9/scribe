mod pipeline;
mod ring_buffer;

use std::fs;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use crossbeam_channel::{Receiver, Sender, TryRecvError, bounded};
use thiserror::Error;

use crate::config;
use crate::prepared_audio::PreparedAudio;

use self::pipeline::Pipeline;
use self::ring_buffer::{Consumer, Producer, ring_buffer};

const START_TIMEOUT: Duration = Duration::from_secs(3);
const DEFAULT_RING_SECONDS: usize = 2;
const MIN_RING_SAMPLES: usize = 65_536;
const MAX_RING_SAMPLES: usize = 2_000_000;
const MAX_STREAM_RESTARTS: u32 = 2;
const FAULT_NONE: u8 = 0;
const FAULT_OVERFLOW: u8 = 1;
const FAULT_STREAM: u8 = 2;

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
    pub vad: VadOptions,
}

impl CaptureOptions {
    pub fn new(vad: VadOptions) -> Self {
        Self {
            vad_enabled: true,
            vad,
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
    #[error("microphone input format has zero channels or sample rate")]
    InvalidInputFormat,
    #[error("failed to enumerate microphone devices: {0}")]
    DeviceEnumeration(String),
    #[error("microphone {requested:?} was not found and no default input is available")]
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
    #[error("microphone input stream failed after {restarts} restart attempts")]
    InputStreamFailed { restarts: u32 },
    #[error("audio capture buffer overflowed and dropped {dropped_samples} samples")]
    BufferOverflow { dropped_samples: usize },
    #[error("failed to prepare captured audio: {0}")]
    Preparation(String),
    #[error("audio recorder did not start within {0:?}")]
    StartTimeout(Duration),
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
}

impl RecordingSession {
    pub fn stop(&self) {
        self.stop_requested.store(true, Ordering::Release);
    }

    pub fn try_finish(&self) -> Option<Result<CaptureCompletion, CaptureError>> {
        match self.finished_rx.try_recv() {
            Ok(result) => Some(result),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(Err(CaptureError::WorkerDisconnected)),
        }
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

    pub fn stop_and_discard(self, timeout: Duration) -> Result<(), CaptureError> {
        self.stop();
        match self.finished_rx.recv_timeout(timeout) {
            Ok(Ok(_completion)) => Ok(()),
            Ok(Err(error)) => Err(error),
            Err(_) => Err(CaptureError::StopTimeout(timeout)),
        }
    }

    #[cfg(test)]
    pub(crate) fn simulated(
        audio: Option<Arc<PreparedAudio>>,
        stop_reason: CaptureStopReason,
    ) -> Self {
        let stop_requested = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop_requested);
        let (finished_tx, finished_rx) = bounded(1);
        thread::spawn(move || {
            let started = Instant::now();
            while !worker_stop.load(Ordering::Acquire) && started.elapsed() < Duration::from_secs(2)
            {
                thread::sleep(Duration::from_millis(1));
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
        }
    }
}

pub fn start_recording(
    max_duration_seconds: u32,
    input_device_name: Option<String>,
    options: CaptureOptions,
) -> Result<RecordingSession, CaptureError> {
    options.vad.validate()?;
    let stop_requested = Arc::new(AtomicBool::new(false));
    let rms_bits = Arc::new(AtomicU32::new(0.0_f32.to_bits()));
    let peak_bits = Arc::new(AtomicU32::new(0.0_f32.to_bits()));
    let level_observed = Arc::new(AtomicBool::new(false));
    let (started_tx, started_rx) = bounded(1);
    let (finished_tx, finished_rx) = bounded(1);

    let worker_stop = Arc::clone(&stop_requested);
    let worker_rms = Arc::clone(&rms_bits);
    let worker_peak = Arc::clone(&peak_bits);
    let worker_observed = Arc::clone(&level_observed);
    thread::Builder::new()
        .name("scribe-audio-capture".to_owned())
        .spawn(move || {
            let result = capture_worker(
                max_duration_seconds,
                input_device_name,
                options,
                worker_stop,
                worker_rms,
                worker_peak,
                worker_observed,
                &started_tx,
            );
            if let Err(error) = &result {
                let _ = started_tx.try_send(Err(error.clone()));
            }
            let _ = finished_tx.send(result);
        })
        .map_err(|error| CaptureError::WorkerSpawn(error.to_string()))?;

    match started_rx.recv_timeout(START_TIMEOUT) {
        Ok(Ok(())) => Ok(RecordingSession {
            stop_requested,
            finished_rx,
            rms_bits,
            peak_bits,
            level_observed,
        }),
        Ok(Err(error)) => Err(error),
        Err(_) => {
            stop_requested.store(true, Ordering::Release);
            Err(CaptureError::StartTimeout(START_TIMEOUT))
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn capture_worker(
    max_duration_seconds: u32,
    input_device_name: Option<String>,
    options: CaptureOptions,
    stop_requested: Arc<AtomicBool>,
    rms_bits: Arc<AtomicU32>,
    peak_bits: Arc<AtomicU32>,
    level_observed: Arc<AtomicBool>,
    started_tx: &Sender<Result<(), CaptureError>>,
) -> Result<CaptureCompletion, CaptureError> {
    let host = cpal::default_host();
    let device = select_input_device(&host, input_device_name.as_deref())?;
    let supported = device
        .default_input_config()
        .map_err(|error| CaptureError::InputConfiguration(error.to_string()))?;
    let format = InputFormat::from_supported(&supported)?;
    let config: cpal::StreamConfig = supported.into();
    if format.sample_rate == 0 || format.channels == 0 {
        return Err(CaptureError::InvalidInputFormat);
    }

    let ring_capacity = usize::try_from(format.sample_rate)
        .unwrap_or(usize::MAX)
        .saturating_mul(format.channels as usize)
        .saturating_mul(DEFAULT_RING_SECONDS)
        .clamp(MIN_RING_SAMPLES, MAX_RING_SAMPLES);
    let (producer, consumer) = ring_buffer(ring_capacity);
    let fault = Arc::new(AtomicU8::new(FAULT_NONE));
    let dropped_samples = Arc::new(AtomicUsize::new(0));
    let mut stream = Some(build_stream(
        &device,
        &config,
        format.sample_format,
        producer,
        Arc::clone(&fault),
        Arc::clone(&dropped_samples),
    )?);
    stream
        .as_ref()
        .expect("stream was just built")
        .play()
        .map_err(|error| CaptureError::PlayStream(error.to_string()))?;
    let _ = started_tx.send(Ok(()));

    let mut pipeline = Pipeline::new(
        format.sample_rate,
        format.channels,
        options.vad_enabled,
        options.vad,
        rms_bits,
        peak_bits,
        level_observed,
    )?;
    let capture_started = Instant::now();
    let maximum_duration = Duration::from_secs(max_duration_seconds.max(1) as u64);
    let mut explicit_stop: Option<(Instant, usize, Duration)> = None;
    let mut restart_policy = RestartPolicy::new(MAX_STREAM_RESTARTS);

    let (stop_reason, stop_trigger_elapsed) = loop {
        drain_ring(&consumer, &mut pipeline);

        match fault.load(Ordering::Acquire) {
            FAULT_OVERFLOW => {
                return Err(CaptureError::BufferOverflow {
                    dropped_samples: dropped_samples.load(Ordering::Relaxed).max(1),
                });
            }
            FAULT_STREAM => {
                drop(stream.take());
                if !restart_policy.try_restart() {
                    return Err(CaptureError::InputStreamFailed {
                        restarts: restart_policy.attempts,
                    });
                }
                let current = device
                    .default_input_config()
                    .map_err(|error| CaptureError::InputConfiguration(error.to_string()))?;
                ensure_restart_format(format, InputFormat::from_supported(&current)?)?;
                fault.store(FAULT_NONE, Ordering::Release);
                let restarted = build_stream(
                    &device,
                    &config,
                    format.sample_format,
                    consumer.producer_for_restart(),
                    Arc::clone(&fault),
                    Arc::clone(&dropped_samples),
                )?;
                restarted
                    .play()
                    .map_err(|error| CaptureError::PlayStream(error.to_string()))?;
                stream = Some(restarted);
            }
            _ => {}
        }

        let elapsed = capture_started.elapsed();
        if explicit_stop.is_none() && stop_requested.load(Ordering::Acquire) {
            explicit_stop = Some((Instant::now(), pipeline.source_frames(), elapsed));
        }

        if let Some((stop_seen, source_frame, trigger_elapsed)) = explicit_stop {
            let post_roll_frames =
                duration_to_source_frames(options.vad.post_roll, format.sample_rate);
            if pipeline.source_frames() >= source_frame.saturating_add(post_roll_frames)
                || stop_seen.elapsed() >= options.vad.post_roll
            {
                break (CaptureStopReason::Explicit, trigger_elapsed);
            }
        } else if let Some(reason) = select_stop_reason(
            false,
            pipeline.endpoint_triggered(),
            elapsed >= maximum_duration,
        ) {
            break (reason, elapsed);
        }

        thread::sleep(Duration::from_millis(2));
    };

    drop(stream.take());
    drain_ring(&consumer, &mut pipeline);
    if fault.load(Ordering::Acquire) == FAULT_OVERFLOW {
        return Err(CaptureError::BufferOverflow {
            dropped_samples: dropped_samples.load(Ordering::Relaxed).max(1),
        });
    }

    let source_frames = pipeline.source_frames();
    let speech_trigger_elapsed = pipeline.speech_trigger_elapsed();
    let audio = pipeline.finish(stop_reason)?.map(Arc::new);
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
            dropped_samples: dropped_samples.load(Ordering::Relaxed),
            stream_restarts: restart_policy.attempts,
        },
    })
}

fn drain_ring(consumer: &Consumer, pipeline: &mut Pipeline) {
    while let Some(sample) = consumer.pop() {
        pipeline.push_interleaved(sample);
    }
}

fn duration_to_source_frames(duration: Duration, sample_rate: u32) -> usize {
    let scaled = duration.as_nanos() * u128::from(sample_rate);
    usize::try_from(scaled.div_ceil(1_000_000_000)).unwrap_or(usize::MAX)
}

fn select_stop_reason(
    explicit: bool,
    endpoint: bool,
    maximum_duration: bool,
) -> Option<CaptureStopReason> {
    if explicit {
        Some(CaptureStopReason::Explicit)
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

fn build_stream(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    sample_format: cpal::SampleFormat,
    producer: Producer,
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
                enqueue_samples(data, &producer, &fault, &dropped_samples, normalize_f32)
            },
            error_callback,
            None,
        ),
        cpal::SampleFormat::I16 => device.build_input_stream(
            config,
            move |data: &[i16], _| {
                enqueue_samples(data, &producer, &fault, &dropped_samples, normalize_i16)
            },
            error_callback,
            None,
        ),
        cpal::SampleFormat::U16 => device.build_input_stream(
            config,
            move |data: &[u16], _| {
                enqueue_samples(data, &producer, &fault, &dropped_samples, normalize_u16)
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
    producer: &Producer,
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
    let dir = config::cache_dir()?.join("recordings");
    fs::create_dir_all(&dir)
        .with_context(|| format!("failed to create recording directory {}", dir.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))
            .with_context(|| format!("failed to secure recording directory {}", dir.display()))?;
    }
    Ok(dir)
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
        let (producer, consumer) = ring_buffer(2);
        let fault = AtomicU8::new(FAULT_NONE);
        let dropped = AtomicUsize::new(0);
        enqueue_samples(
            &[1_i16, 2, 3, 4],
            &producer,
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
            select_stop_reason(true, true, true),
            Some(CaptureStopReason::Explicit)
        );
        assert_eq!(
            select_stop_reason(false, true, true),
            Some(CaptureStopReason::Endpoint)
        );
        assert_eq!(
            select_stop_reason(false, false, true),
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
}
