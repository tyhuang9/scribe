use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Seek, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use crossbeam_channel::{Receiver, Sender, bounded};
use fs2::FileExt;

use crate::config;
use crate::live_preview::{
    MAX_PENDING_PREVIEW_CHUNKS, PREVIEW_CHUNK_DURATION_MS, PREVIEW_CHUNK_OVERLAP_MS,
    PreviewArtifact, PreviewSessionId,
};

const WAV_HEADER_BYTES: u64 = 44;
const WAV_BYTES_PER_SAMPLE: u64 = 2;
const RECORDING_STORAGE_RESERVE_BYTES: u64 = 256 * 1024 * 1024;
const MAX_RIFF_WAV_BYTES: u64 = u32::MAX as u64;
const STALE_RECORDING_AGE: Duration = Duration::from_secs(24 * 60 * 60);
const RECORDING_PATH_ATTEMPTS: usize = 32;
static RECORDING_NONCE: AtomicU64 = AtomicU64::new(0);
// Allow 250 ms of conservative 5 ms callback batches: enough for scheduler jitter,
// but bounded so a stalled writer fails promptly instead of growing memory indefinitely.
const AUDIO_QUEUE_LATENCY_BUDGET_MS: usize = 250;
const CONSERVATIVE_CALLBACK_INTERVAL_MS: usize = 5;
const AUDIO_QUEUE_CAPACITY: usize =
    AUDIO_QUEUE_LATENCY_BUDGET_MS / CONSERVATIVE_CALLBACK_INTERVAL_MS;

pub struct RecordingSession {
    pub audio_path: PathBuf,
    stop_tx: Sender<()>,
    finished_rx: Receiver<Result<PathBuf, String>>,
    preview_rx: Receiver<PreviewArtifact>,
    completion_reported: AtomicBool,
}

impl RecordingSession {
    pub fn stop(&self) {
        let _ = self.stop_tx.try_send(());
    }

    pub fn try_finish(&self) -> Option<Result<PathBuf, String>> {
        if self.completion_reported.load(Ordering::Acquire) {
            return None;
        }
        let completion = match self.finished_rx.try_recv() {
            Ok(result) => Some(result),
            Err(crossbeam_channel::TryRecvError::Empty) => None,
            Err(crossbeam_channel::TryRecvError::Disconnected) => Some(Err(
                "audio recorder stopped unexpectedly before reporting completion".to_owned(),
            )),
        };
        if completion.is_some() {
            self.completion_reported.store(true, Ordering::Release);
        }
        completion
    }

    pub fn try_preview_chunk(&self) -> Option<PreviewArtifact> {
        self.preview_rx.try_recv().ok()
    }
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

pub fn wav_duration_ms(path: &Path) -> Option<u128> {
    let reader = hound::WavReader::open(path).ok()?;
    let spec = reader.spec();
    if spec.sample_rate == 0 || spec.channels == 0 {
        return None;
    }

    let total_channel_samples = reader.duration() as u128;
    let frames = total_channel_samples / spec.channels as u128;
    Some(frames * 1000 / spec.sample_rate as u128)
}

pub fn start_recording(
    max_duration_seconds: u32,
    input_device_name: Option<String>,
    preview_session_id: Option<PreviewSessionId>,
) -> Result<RecordingSession> {
    let (audio_path, audio_file) = temp_wav_file()?;
    let (stop_tx, stop_rx) = bounded::<()>(1);
    let (finished_tx, finished_rx) = bounded::<Result<PathBuf, String>>(1);
    let (started_tx, started_rx) = bounded::<Result<(), String>>(1);
    let (preview_tx, preview_rx) = bounded::<PreviewArtifact>(MAX_PENDING_PREVIEW_CHUNKS);
    let worker_path = audio_path.clone();

    thread::spawn(move || {
        let startup_error_tx = started_tx.clone();
        let result = record_to_wav(
            worker_path.clone(),
            audio_file,
            stop_rx,
            max_duration_seconds,
            input_device_name,
            started_tx,
            preview_session_id.map(|session_id| (session_id, preview_tx)),
        );
        if let Err(err) = &result {
            let _ = startup_error_tx.try_send(Err(err.to_string()));
        }
        remove_file_best_effort(&recording_lock_path(&worker_path), "recording lock");
        if result.is_err() {
            remove_file_best_effort(&worker_path, "incomplete recording");
        }
        send_recording_completion(
            &finished_tx,
            result.map(|_| worker_path).map_err(|err| err.to_string()),
        );
    });

    match started_rx
        .recv_timeout(Duration::from_secs(3))
        .context("audio recorder did not start")?
    {
        Ok(()) => Ok(RecordingSession {
            audio_path,
            stop_tx,
            finished_rx,
            preview_rx,
            completion_reported: AtomicBool::new(false),
        }),
        Err(message) => Err(anyhow!(message)),
    }
}

pub fn cleanup_stale_recording_artifacts() -> Result<usize> {
    let dir = config::cache_dir()?.join("recordings");
    match cleanup_stale_recordings(&dir, SystemTime::now(), STALE_RECORDING_AGE) {
        Err(error)
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound) =>
        {
            Ok(0)
        }
        result => result,
    }
}

fn temp_wav_file() -> Result<(PathBuf, File)> {
    let dir = config::cache_dir()?.join("recordings");
    fs::create_dir_all(&dir)
        .with_context(|| format!("failed to create recording directory {}", dir.display()))?;
    if let Err(err) = cleanup_stale_recording_artifacts() {
        eprintln!("could not clean stale Scribe recordings: {err}");
    }
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    for _ in 0..RECORDING_PATH_ATTEMPTS {
        let nonce = RECORDING_NONCE.fetch_add(1, Ordering::Relaxed);
        let path = dir.join(format!(
            "recording-{millis}-{}-{nonce}.wav",
            std::process::id()
        ));
        match create_recording_file(&path) {
            Ok(file) => return Ok((path, file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("failed to reserve recording file {}", path.display())
                });
            }
        }
    }
    Err(anyhow!(
        "failed to reserve a unique recording file after {RECORDING_PATH_ATTEMPTS} attempts"
    ))
}

fn create_recording_file(path: &Path) -> std::io::Result<File> {
    OpenOptions::new()
        .create_new(true)
        .read(true)
        .write(true)
        .open(path)
}

struct PreviewChunkFile {
    path: PathBuf,
    writer: hound::WavWriter<BufWriter<File>>,
    samples_written: u64,
}

struct PreviewChunkWriter {
    recording_path: PathBuf,
    session_id: PreviewSessionId,
    spec: hound::WavSpec,
    preview_tx: Sender<PreviewArtifact>,
    current: Option<PreviewChunkFile>,
    overlap: Vec<i16>,
    chunk_samples: u64,
    overlap_samples: usize,
    next_sequence: u64,
}

impl PreviewChunkWriter {
    fn new(
        recording_path: PathBuf,
        session_id: PreviewSessionId,
        spec: hound::WavSpec,
        preview_tx: Sender<PreviewArtifact>,
    ) -> Result<Self> {
        preview_chunk_path(&recording_path, 0)?;
        let samples_per_ms = u64::from(spec.sample_rate)
            .checked_mul(u64::from(spec.channels))
            .ok_or_else(|| anyhow!("live preview sample rate overflowed"))?;
        let chunk_samples = samples_per_ms
            .checked_mul(PREVIEW_CHUNK_DURATION_MS)
            .map(|samples| samples / 1_000)
            .filter(|samples| *samples > 0)
            .ok_or_else(|| anyhow!("live preview chunk size is invalid"))?;
        let overlap_samples = samples_per_ms
            .checked_mul(PREVIEW_CHUNK_OVERLAP_MS)
            .map(|samples| samples / 1_000)
            .and_then(|samples| usize::try_from(samples).ok())
            .filter(|samples| *samples > 0 && (*samples as u64) < chunk_samples)
            .ok_or_else(|| anyhow!("live preview overlap size is invalid"))?;

        Ok(Self {
            recording_path,
            session_id,
            spec,
            preview_tx,
            current: None,
            overlap: Vec::with_capacity(overlap_samples),
            chunk_samples,
            overlap_samples,
            next_sequence: 0,
        })
    }

    fn write_samples(&mut self, mut samples: &[i16]) -> Result<()> {
        while !samples.is_empty() {
            self.ensure_current_chunk()?;
            let current = self.current.as_mut().expect("preview chunk should be open");
            let remaining = self.chunk_samples - current.samples_written;
            let take = samples
                .len()
                .min(usize::try_from(remaining).unwrap_or(usize::MAX));
            let (chunk, rest) = samples.split_at(take);
            write_samples(&mut current.writer, chunk.iter().copied())
                .context("failed to write live preview chunk")?;
            current.samples_written += take as u64;
            self.remember_overlap(chunk);
            samples = rest;

            if self
                .current
                .as_ref()
                .is_some_and(|current| current.samples_written == self.chunk_samples)
            {
                self.finalize_current_chunk()?;
            }
        }
        Ok(())
    }

    fn ensure_current_chunk(&mut self) -> Result<()> {
        if self.current.is_some() {
            return Ok(());
        }

        let path = preview_chunk_path(&self.recording_path, self.next_sequence)?;
        let file = create_recording_file(&path)
            .with_context(|| format!("failed to reserve live preview chunk {}", path.display()))?;
        let mut writer = match hound::WavWriter::new(BufWriter::new(file), self.spec) {
            Ok(writer) => writer,
            Err(error) => {
                remove_file_best_effort(&path, "incomplete live preview chunk");
                return Err(error).with_context(|| {
                    format!("failed to create live preview chunk {}", path.display())
                });
            }
        };
        if let Err(error) = write_samples(&mut writer, self.overlap.iter().copied()) {
            drop(writer);
            remove_file_best_effort(&path, "incomplete live preview chunk");
            return Err(error).context("failed to seed live preview overlap");
        }
        self.current = Some(PreviewChunkFile {
            path,
            writer,
            samples_written: self.overlap.len() as u64,
        });
        Ok(())
    }

    fn remember_overlap(&mut self, samples: &[i16]) {
        if samples.len() >= self.overlap_samples {
            self.overlap.clear();
            self.overlap
                .extend_from_slice(&samples[samples.len() - self.overlap_samples..]);
            return;
        }
        let excess = self
            .overlap
            .len()
            .saturating_add(samples.len())
            .saturating_sub(self.overlap_samples);
        if excess > 0 {
            self.overlap.drain(..excess);
        }
        self.overlap.extend_from_slice(samples);
    }

    fn finalize_current_chunk(&mut self) -> Result<()> {
        let current = self
            .current
            .take()
            .expect("completed preview chunk should be open");
        let path = current.path;
        if let Err(error) = current.writer.finalize() {
            remove_file_best_effort(&path, "incomplete live preview chunk");
            return Err(error).context("failed to finalize live preview chunk");
        }

        let artifact = PreviewArtifact::new(self.session_id, self.next_sequence, path);
        self.next_sequence += 1;
        if let Err(error) = self.preview_tx.try_send(artifact) {
            match error {
                crossbeam_channel::TrySendError::Full(artifact)
                | crossbeam_channel::TrySendError::Disconnected(artifact) => drop(artifact),
            }
        }
        Ok(())
    }
}

impl Drop for PreviewChunkWriter {
    fn drop(&mut self) {
        if let Some(current) = self.current.take() {
            drop(current.writer);
            remove_file_best_effort(&current.path, "incomplete live preview chunk");
        }
    }
}

fn preview_chunk_path(recording_path: &Path, sequence: u64) -> Result<PathBuf> {
    let name = recording_path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| anyhow!("recording path has no UTF-8 file name"))?;
    let stem = name
        .strip_prefix("recording-")
        .and_then(|name| name.strip_suffix(".wav"))
        .ok_or_else(|| anyhow!("recording path is not owned by Scribe"))?;
    let parts = stem.split('-').collect::<Vec<_>>();
    if parts.len() != 3
        || parts
            .iter()
            .any(|part| part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_digit()))
    {
        return Err(anyhow!("recording path is not an owned session recording"));
    }
    Ok(recording_path.with_file_name(format!("recording-{stem}-chunk-{sequence}.wav")))
}

fn record_to_wav(
    path: PathBuf,
    audio_file: File,
    stop_rx: Receiver<()>,
    max_duration_seconds: u32,
    input_device_name: Option<String>,
    started_tx: Sender<Result<(), String>>,
    preview: Option<(PreviewSessionId, Sender<PreviewArtifact>)>,
) -> Result<()> {
    let host = cpal::default_host();
    let device = select_input_device(&host, input_device_name.as_deref())?;
    let supported_config = device
        .default_input_config()
        .context("failed to read the microphone input config")?;
    let sample_format = supported_config.sample_format();
    let stream_config: cpal::StreamConfig = supported_config.into();
    let channels = stream_config.channels;
    ensure_recording_space(
        path.parent()
            .ok_or_else(|| anyhow!("recording path has no parent directory"))?,
        stream_config.sample_rate.0,
        channels,
        max_duration_seconds,
    )?;
    let _recording_lock = lock_recording_path(&path)?;
    let wav_spec = hound::WavSpec {
        channels,
        sample_rate: stream_config.sample_rate.0,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::new(BufWriter::new(audio_file), wav_spec)
        .with_context(|| format!("failed to create {}", path.display()))?;
    let mut preview_writer = preview.and_then(|(session_id, preview_tx)| {
        match PreviewChunkWriter::new(path.clone(), session_id, wav_spec, preview_tx) {
            Ok(writer) => Some(writer),
            Err(error) => {
                eprintln!("live preview disabled for this recording: {error}");
                None
            }
        }
    });
    let (sample_tx, sample_rx) = bounded::<Vec<i16>>(AUDIO_QUEUE_CAPACITY);
    let (callback_error_tx, callback_error_rx) = bounded::<String>(1);
    let stream = match sample_format {
        cpal::SampleFormat::F32 => {
            let callback_samples = sample_tx.clone();
            let error_tx = callback_error_tx.clone();
            device.build_input_stream(
                &stream_config,
                move |data: &[f32], _| queue_f32(data, &callback_samples, &error_tx),
                stream_error_handler(callback_error_tx.clone()),
                None,
            )?
        }
        cpal::SampleFormat::I16 => {
            let callback_samples = sample_tx.clone();
            let error_tx = callback_error_tx.clone();
            device.build_input_stream(
                &stream_config,
                move |data: &[i16], _| queue_samples(data.to_vec(), &callback_samples, &error_tx),
                stream_error_handler(callback_error_tx.clone()),
                None,
            )?
        }
        cpal::SampleFormat::U16 => {
            let callback_samples = sample_tx.clone();
            let error_tx = callback_error_tx.clone();
            device.build_input_stream(
                &stream_config,
                move |data: &[u16], _| queue_u16(data, &callback_samples, &error_tx),
                stream_error_handler(callback_error_tx.clone()),
                None,
            )?
        }
        other => {
            let message = format!("unsupported microphone sample format: {other:?}");
            let _ = started_tx.send(Err(message.clone()));
            return Err(anyhow!(message));
        }
    };
    drop(sample_tx);
    drop(callback_error_tx);

    if let Err(err) = stream.play() {
        let message = format!("failed to start microphone stream: {err}");
        let _ = started_tx.send(Err(message.clone()));
        return Err(anyhow!(message));
    }
    confirm_recording_started(&started_tx)?;

    let max_duration = Duration::from_secs(max_duration_seconds.max(1) as u64);
    let started_at = std::time::Instant::now();
    let mut callback_error = None;
    while !recording_limit_reached(started_at.elapsed(), max_duration) {
        if let Ok(message) = callback_error_rx.try_recv() {
            callback_error = Some(message);
            break;
        }
        match recording_stop_signal(&stop_rx) {
            RecordingStopSignal::Continue => {}
            RecordingStopSignal::Requested => break,
            RecordingStopSignal::OwnerDropped => {
                callback_error = Some("recording session was cancelled".to_owned());
                break;
            }
        }
        match sample_rx.recv_timeout(Duration::from_millis(10)) {
            Ok(samples) => {
                if let Err(err) = write_samples(&mut writer, samples.iter().copied()) {
                    callback_error = Some(format!("failed to write recording audio: {err}"));
                    break;
                }
                if let Some(preview) = preview_writer.as_mut()
                    && let Err(error) = preview.write_samples(&samples)
                {
                    eprintln!("live preview disabled for this recording: {error}");
                    preview_writer = None;
                }
            }
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                callback_error = Some("audio input stream stopped unexpectedly".to_owned());
                break;
            }
        }
    }

    drop(stream);
    // Stop/limit completion never publishes a partial or queued final chunk.
    drop(preview_writer);
    if callback_error.is_none() {
        callback_error = callback_error_rx.try_recv().ok();
    }
    if callback_error.is_none()
        && let Err(err) = drain_queued_samples(&sample_rx, &mut writer)
    {
        callback_error = Some(format!("failed to write recording audio: {err}"));
    }
    let finalize_error = writer.finalize().err();
    if let Some(message) = callback_error {
        return Err(anyhow!(message));
    }
    if let Some(err) = finalize_error {
        return Err(err).context("failed to finalize WAV file");
    }

    Ok(())
}

fn recording_limit_reached(elapsed: Duration, maximum: Duration) -> bool {
    elapsed >= maximum
}

fn confirm_recording_started(started_tx: &Sender<Result<(), String>>) -> Result<()> {
    started_tx
        .send(Ok(()))
        .map_err(|_| anyhow!("recording startup was cancelled"))
}

fn remove_file_best_effort(path: &Path, description: &str) {
    if let Err(error) = fs::remove_file(path)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        eprintln!("failed to remove {description} {}: {error}", path.display());
    }
}

fn send_recording_completion(
    finished_tx: &Sender<Result<PathBuf, String>>,
    completion: Result<PathBuf, String>,
) {
    if let Err(crossbeam_channel::SendError(Ok(path))) = finished_tx.send(completion) {
        remove_file_best_effort(&path, "orphaned completed recording");
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RecordingStopSignal {
    Continue,
    Requested,
    OwnerDropped,
}

fn recording_stop_signal(stop_rx: &Receiver<()>) -> RecordingStopSignal {
    match stop_rx.try_recv() {
        Ok(()) => RecordingStopSignal::Requested,
        Err(crossbeam_channel::TryRecvError::Empty) => RecordingStopSignal::Continue,
        Err(crossbeam_channel::TryRecvError::Disconnected) => RecordingStopSignal::OwnerDropped,
    }
}

fn estimated_wav_bytes(sample_rate: u32, channels: u16, duration_seconds: u32) -> Option<u64> {
    let budgeted_seconds = u64::from(duration_seconds.max(1)).checked_add(1)?;
    u64::from(sample_rate)
        .checked_mul(u64::from(channels))?
        .checked_mul(WAV_BYTES_PER_SAMPLE)?
        .checked_mul(budgeted_seconds)?
        .checked_add(WAV_HEADER_BYTES)
}

fn has_recording_space(available_bytes: u64, estimated_bytes: u64, reserve_bytes: u64) -> bool {
    estimated_bytes
        .checked_add(reserve_bytes)
        .is_some_and(|required| available_bytes >= required)
}

fn fits_supported_riff_wav(estimated_bytes: u64) -> bool {
    estimated_bytes <= MAX_RIFF_WAV_BYTES
}

fn ensure_recording_space(
    recording_dir: &Path,
    sample_rate: u32,
    channels: u16,
    duration_seconds: u32,
) -> Result<()> {
    let estimated = estimated_wav_bytes(sample_rate, channels, duration_seconds)
        .ok_or_else(|| anyhow!("recording storage estimate overflowed"))?;
    if !fits_supported_riff_wav(estimated) {
        return Err(anyhow!(
            "this microphone format and duration would exceed the 4 GiB WAV limit; choose a shorter recording duration"
        ));
    }
    let available = fs2::available_space(recording_dir)
        .with_context(|| format!("failed to check free space for {}", recording_dir.display()))?;
    if !has_recording_space(available, estimated, RECORDING_STORAGE_RESERVE_BYTES) {
        let needed_mib = estimated.div_ceil(1024 * 1024);
        let available_mib = available / (1024 * 1024);
        let reserve_mib = RECORDING_STORAGE_RESERVE_BYTES / (1024 * 1024);
        return Err(anyhow!(
            "not enough free space for this recording: about {needed_mib} MiB is needed plus a {reserve_mib} MiB safety reserve, but {available_mib} MiB is available"
        ));
    }
    Ok(())
}

fn recording_lock_path(recording_path: &Path) -> PathBuf {
    let owner_name = recording_path
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(recording_owner_name);
    if let Some(mut owner_name) = owner_name {
        owner_name.push_str(".lock");
        return recording_path.with_file_name(owner_name);
    }

    let mut path = recording_path.as_os_str().to_os_string();
    path.push(".lock");
    PathBuf::from(path)
}

fn lock_recording_path(recording_path: &Path) -> Result<File> {
    let lock_path = recording_lock_path(recording_path);
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)
        .with_context(|| format!("failed to create recording lock {}", lock_path.display()))?;
    file.try_lock_exclusive()
        .with_context(|| format!("failed to lock recording path {}", recording_path.display()))?;
    Ok(file)
}

fn recording_lock_is_contended(error: &anyhow::Error) -> bool {
    error.downcast_ref::<std::io::Error>().is_some_and(|error| {
        error.kind() == std::io::ErrorKind::WouldBlock
            || cfg!(windows) && error.raw_os_error() == Some(33)
    })
}

fn is_owned_recording_name(name: &str) -> bool {
    recording_owner_name(name).is_some()
}

fn recording_owner_name(name: &str) -> Option<String> {
    let id = name
        .strip_prefix("recording-")
        .and_then(|name| name.strip_suffix(".wav"))?;
    let parts = id.split('-').collect::<Vec<_>>();
    let numeric = |part: &str| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit());
    match parts.as_slice() {
        [millis] if numeric(millis) => Some(name.to_owned()),
        [millis, pid, nonce] if [millis, pid, nonce].into_iter().all(|part| numeric(part)) => {
            Some(name.to_owned())
        }
        [millis, pid, nonce, "chunk", sequence]
            if [millis, pid, nonce, sequence]
                .into_iter()
                .all(|part| numeric(part)) =>
        {
            Some(format!("recording-{millis}-{pid}-{nonce}.wav"))
        }
        _ => None,
    }
}

fn is_stale_recording(
    name: &str,
    modified: SystemTime,
    now: SystemTime,
    minimum_age: Duration,
) -> bool {
    is_owned_recording_name(name)
        && now
            .duration_since(modified)
            .is_ok_and(|age| age >= minimum_age)
}

fn cleanup_stale_recordings(
    recording_dir: &Path,
    now: SystemTime,
    minimum_age: Duration,
) -> Result<usize> {
    let mut removed = 0;
    for entry in fs::read_dir(recording_dir)
        .with_context(|| format!("failed to inspect {}", recording_dir.display()))?
    {
        let entry =
            entry.with_context(|| format!("failed to inspect {}", recording_dir.display()))?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !is_owned_recording_name(name) {
            continue;
        }
        if !entry
            .file_type()
            .with_context(|| format!("failed to inspect {}", entry.path().display()))?
            .is_file()
        {
            continue;
        }
        let metadata = entry
            .metadata()
            .with_context(|| format!("failed to inspect {}", entry.path().display()))?;
        let Ok(modified) = metadata.modified() else {
            continue;
        };
        if !is_stale_recording(name, modified, now, minimum_age) {
            continue;
        }

        let path = entry.path();
        let lock = match lock_recording_path(&path) {
            Ok(lock) => lock,
            Err(error) if recording_lock_is_contended(&error) => continue,
            Err(error) => return Err(error),
        };
        fs::remove_file(&path)
            .with_context(|| format!("failed to remove stale recording {}", path.display()))?;
        drop(lock);
        remove_file_best_effort(&recording_lock_path(&path), "stale recording lock");
        removed += 1;
    }
    Ok(removed)
}

fn select_input_device(host: &cpal::Host, input_device_name: Option<&str>) -> Result<cpal::Device> {
    if let Some(target_name) = input_device_name.filter(|name| !name.trim().is_empty())
        && let Ok(devices) = host.input_devices()
    {
        for device in devices {
            if device.name().ok().as_deref() == Some(target_name) {
                return Ok(device);
            }
        }
    }

    host.default_input_device().ok_or_else(|| {
        if let Some(target_name) = input_device_name {
            anyhow!(
                "microphone \"{target_name}\" was not found and no default input microphone is available"
            )
        } else {
            anyhow!("no default input microphone was found")
        }
    })
}

fn stream_error_handler(
    error_tx: Sender<String>,
) -> impl FnMut(cpal::StreamError) + Send + 'static {
    move |err| record_callback_error(&error_tx, format!("audio input stream error: {err}"))
}

fn record_callback_error(error_tx: &Sender<String>, message: String) {
    let _ = error_tx.try_send(message);
}

fn write_samples<W, I>(writer: &mut hound::WavWriter<W>, samples: I) -> hound::Result<()>
where
    W: Write + Seek,
    I: IntoIterator<Item = i16>,
{
    for sample in samples {
        writer.write_sample(sample)?;
    }
    Ok(())
}

fn drain_queued_samples<W>(
    sample_rx: &Receiver<Vec<i16>>,
    writer: &mut hound::WavWriter<W>,
) -> hound::Result<()>
where
    W: Write + Seek,
{
    while let Ok(samples) = sample_rx.try_recv() {
        write_samples(writer, samples)?;
    }
    Ok(())
}

fn queue_samples(samples: Vec<i16>, sample_tx: &Sender<Vec<i16>>, error_tx: &Sender<String>) {
    if let Err(error) = sample_tx.try_send(samples) {
        let message = match error {
            crossbeam_channel::TrySendError::Full(_) => {
                "recording writer could not keep up with the microphone".to_owned()
            }
            crossbeam_channel::TrySendError::Disconnected(_) => {
                "recording writer stopped unexpectedly".to_owned()
            }
        };
        record_callback_error(error_tx, message);
    }
}

fn queue_f32(input: &[f32], sample_tx: &Sender<Vec<i16>>, error_tx: &Sender<String>) {
    queue_samples(
        input
            .iter()
            .map(|sample| {
                let sample = sample.clamp(-1.0, 1.0);
                (sample * i16::MAX as f32) as i16
            })
            .collect(),
        sample_tx,
        error_tx,
    );
}

fn queue_u16(input: &[u16], sample_tx: &Sender<Vec<i16>>, error_tx: &Sender<String>) {
    queue_samples(
        input
            .iter()
            .map(|sample| {
                let centered = *sample as i32 - 32768;
                centered.clamp(i16::MIN as i32, i16::MAX as i32) as i16
            })
            .collect(),
        sample_tx,
        error_tx,
    );
}

#[cfg(test)]
mod tests {
    use std::io::{Cursor, Error, ErrorKind, Result as IoResult};

    use super::*;

    struct FailingWriter {
        inner: Cursor<Vec<u8>>,
        byte_limit: u64,
    }

    impl Write for FailingWriter {
        fn write(&mut self, bytes: &[u8]) -> IoResult<usize> {
            let position = self.inner.position();
            if position >= self.byte_limit {
                return Err(Error::new(ErrorKind::StorageFull, "simulated full disk"));
            }
            let allowed = (self.byte_limit - position).min(bytes.len() as u64) as usize;
            self.inner.write(&bytes[..allowed])
        }

        fn flush(&mut self) -> IoResult<()> {
            Ok(())
        }
    }

    impl Seek for FailingWriter {
        fn seek(&mut self, position: std::io::SeekFrom) -> IoResult<u64> {
            self.inner.seek(position)
        }
    }

    fn test_dir(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "scribe-recording-{name}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    #[test]
    fn recording_storage_estimate_is_checked_and_uses_pcm16_output() {
        assert_eq!(estimated_wav_bytes(48_000, 2, 60), Some(11_712_044));
        assert_eq!(estimated_wav_bytes(48_000, 2, 3_600), Some(691_392_044));
        assert_eq!(estimated_wav_bytes(u32::MAX, u16::MAX, u32::MAX), None);
        let oversized = estimated_wav_bytes(384_000, 32, 7_200).unwrap();
        assert!(!fits_supported_riff_wav(oversized));
        assert!(
            ensure_recording_space(Path::new("unused-for-oversized-wav"), 384_000, 32, 7_200)
                .unwrap_err()
                .to_string()
                .contains("4 GiB WAV limit")
        );
        assert!(fits_supported_riff_wav(
            estimated_wav_bytes(48_000, 2, 7_200).unwrap()
        ));
    }

    #[test]
    fn recording_space_decision_keeps_the_full_reserve() {
        assert!(has_recording_space(1_500, 1_000, 500));
        assert!(!has_recording_space(1_499, 1_000, 500));
        assert!(!has_recording_space(u64::MAX, u64::MAX, 1));
    }

    #[test]
    fn stale_cleanup_only_removes_owned_old_unlocked_files() {
        let dir = test_dir("cleanup");
        fs::create_dir_all(dir.join("recording-333.wav")).unwrap();
        let stale = dir.join("recording-111.wav");
        let active = dir.join("recording-222-3-4.wav");
        let active_chunk = dir.join("recording-222-3-4-chunk-5.wav");
        let stale_chunk = dir.join("recording-555-6-7-chunk-8.wav");
        let malformed = dir.join("recording-1-2.wav");
        let unrelated = dir.join("notes.wav");
        fs::write(&stale, b"stale").unwrap();
        fs::write(&active, b"active").unwrap();
        fs::write(&active_chunk, b"active chunk").unwrap();
        fs::write(&stale_chunk, b"stale chunk").unwrap();
        fs::write(&malformed, b"not owned").unwrap();
        fs::write(&unrelated, b"unrelated").unwrap();
        let active_lock = lock_recording_path(&active).unwrap();

        let removed = cleanup_stale_recordings(&dir, SystemTime::now(), Duration::ZERO).unwrap();

        assert_eq!(removed, 2);
        assert!(!stale.exists());
        assert!(!stale_chunk.exists());
        assert!(active.exists());
        assert!(active_chunk.exists());
        assert!(malformed.exists());
        assert!(unrelated.exists());
        assert!(dir.join("recording-333.wav").is_dir());
        drop(active_lock);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn stale_cleanup_respects_filename_and_age_rules() {
        let now = UNIX_EPOCH + Duration::from_secs(10_000);
        assert!(is_stale_recording(
            "recording-123.wav",
            now - Duration::from_secs(100),
            now,
            Duration::from_secs(100)
        ));
        assert!(!is_stale_recording(
            "recording-123.wav",
            now - Duration::from_secs(99),
            now,
            Duration::from_secs(100)
        ));
        assert!(!is_stale_recording(
            "other-123.wav",
            UNIX_EPOCH,
            now,
            Duration::ZERO
        ));
        assert!(!is_stale_recording(
            "recording-../../secret.wav",
            UNIX_EPOCH,
            now,
            Duration::ZERO
        ));
    }

    #[test]
    fn owned_recording_parser_accepts_only_reserved_shapes() {
        assert_eq!(
            recording_owner_name("recording-1.wav").as_deref(),
            Some("recording-1.wav")
        );
        assert_eq!(
            recording_owner_name("recording-1-2-3.wav").as_deref(),
            Some("recording-1-2-3.wav")
        );
        assert_eq!(
            recording_owner_name("recording-1-2-3-chunk-4.wav").as_deref(),
            Some("recording-1-2-3.wav")
        );
        for malformed in [
            "recording-.wav",
            "recording-1-2.wav",
            "recording-1-2-3-4.wav",
            "recording-1-2-3-chunk.wav",
            "recording-1-2-3-chunk-x.wav",
            "recording-1-2-3-chunk-4-5.wav",
            "recording-x-2-3.wav",
            "other-1-2-3.wav",
        ] {
            assert_eq!(recording_owner_name(malformed), None, "{malformed}");
        }
    }

    #[test]
    fn wav_write_errors_are_returned_to_the_recorder() {
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 16_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let sink = FailingWriter {
            inner: Cursor::new(Vec::new()),
            byte_limit: WAV_HEADER_BYTES + 1,
        };
        let mut writer = hound::WavWriter::new(sink, spec).unwrap();

        assert!(write_samples(&mut writer, [1_i16, 2_i16]).is_err());
    }

    #[test]
    fn callback_error_channel_preserves_the_first_failure_without_blocking() {
        let (error_tx, error_rx) = bounded(1);
        record_callback_error(&error_tx, "first failure".to_owned());
        record_callback_error(&error_tx, "second failure".to_owned());

        assert_eq!(error_rx.try_recv().unwrap(), "first failure");
    }

    #[test]
    fn latency_bounded_sample_queue_reports_overflow_instead_of_dropping_audio() {
        assert_eq!(AUDIO_QUEUE_CAPACITY, 50);
        let (sample_tx, _sample_rx) = bounded(AUDIO_QUEUE_CAPACITY);
        let (error_tx, error_rx) = bounded(1);
        for _ in 0..AUDIO_QUEUE_CAPACITY {
            sample_tx.try_send(vec![1_i16]).unwrap();
        }

        queue_samples(vec![2_i16], &sample_tx, &error_tx);

        assert!(error_rx.try_recv().unwrap().contains("could not keep up"));
    }

    #[test]
    fn queued_sample_batches_drain_in_capture_order() {
        let dir = test_dir("queue-order");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("ordered.wav");
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 16_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(&path, spec).unwrap();
        let (sample_tx, sample_rx) = bounded(2);
        sample_tx.send(vec![1_i16, 2_i16]).unwrap();
        sample_tx.send(vec![3_i16, 4_i16]).unwrap();

        drain_queued_samples(&sample_rx, &mut writer).unwrap();
        writer.finalize().unwrap();

        let samples = hound::WavReader::open(&path)
            .unwrap()
            .into_samples::<i16>()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(samples, [1, 2, 3, 4]);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn recording_file_reservation_never_clobbers_an_existing_path() {
        let dir = test_dir("create-new");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("recording-1-2-3.wav");
        fs::write(&path, b"keep me").unwrap();

        let error = create_recording_file(&path).unwrap_err();

        assert_eq!(error.kind(), ErrorKind::AlreadyExists);
        assert_eq!(fs::read(&path).unwrap(), b"keep me");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn dropped_startup_receiver_rejects_the_handshake() {
        let (started_tx, started_rx) = bounded(1);
        drop(started_rx);

        assert!(confirm_recording_started(&started_tx).is_err());
    }

    #[test]
    fn disconnected_worker_is_reported_once_as_recording_completion() {
        let (stop_tx, _stop_rx) = bounded(1);
        let (finished_tx, finished_rx) = bounded(1);
        drop(finished_tx);
        let session = RecordingSession {
            audio_path: PathBuf::from("recording-1.wav"),
            stop_tx,
            finished_rx,
            preview_rx: bounded(1).1,
            completion_reported: AtomicBool::new(false),
        };

        assert!(session.try_finish().unwrap().is_err());
        assert!(session.try_finish().is_none());
    }

    #[test]
    fn dropped_completion_receiver_deletes_successful_recording() {
        let dir = test_dir("dropped-completion");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("recording-1-2-3.wav");
        fs::write(&path, b"completed wav").unwrap();
        let (finished_tx, finished_rx) = bounded(1);
        drop(finished_rx);

        send_recording_completion(&finished_tx, Ok(path.clone()));

        assert!(!path.exists());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn dropped_startup_session_owner_cancels_the_recorder() {
        let (stop_tx, stop_rx) = bounded(1);
        assert_eq!(
            recording_stop_signal(&stop_rx),
            RecordingStopSignal::Continue
        );

        drop(stop_tx);

        assert_eq!(
            recording_stop_signal(&stop_rx),
            RecordingStopSignal::OwnerDropped
        );
    }

    #[test]
    fn explicit_stop_request_remains_a_successful_recording_stop() {
        let (stop_tx, stop_rx) = bounded(1);
        stop_tx.send(()).unwrap();

        assert_eq!(
            recording_stop_signal(&stop_rx),
            RecordingStopSignal::Requested
        );
    }

    #[test]
    fn recording_limit_still_stops_at_the_configured_boundary() {
        let limit = Duration::from_secs(60);
        assert!(!recording_limit_reached(
            Duration::from_millis(59_999),
            limit
        ));
        assert!(recording_limit_reached(Duration::from_secs(60), limit));
    }

    #[test]
    fn preview_chunks_are_finalized_readable_and_overlap_by_500_ms() {
        let dir = test_dir("preview-readable");
        fs::create_dir_all(&dir).unwrap();
        let recording = dir.join("recording-1-2-3.wav");
        let (preview_tx, preview_rx) = bounded(MAX_PENDING_PREVIEW_CHUNKS);
        let mut chunks = PreviewChunkWriter::new(
            recording,
            PreviewSessionId::next(),
            hound::WavSpec {
                channels: 1,
                sample_rate: 10,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            },
            preview_tx,
        )
        .unwrap();
        let samples = (0_i16..95).collect::<Vec<_>>();

        chunks.write_samples(&samples).unwrap();
        let first = preview_rx.try_recv().unwrap();
        let second = preview_rx.try_recv().unwrap();

        assert_eq!(
            first.path().file_name().unwrap(),
            "recording-1-2-3-chunk-0.wav"
        );
        assert_eq!(
            second.path().file_name().unwrap(),
            "recording-1-2-3-chunk-1.wav"
        );
        let read = |path: &Path| {
            hound::WavReader::open(path)
                .unwrap()
                .into_samples::<i16>()
                .collect::<Result<Vec<_>, _>>()
                .unwrap()
        };
        assert_eq!(read(first.path()), (0_i16..50).collect::<Vec<_>>());
        assert_eq!(read(second.path()), (45_i16..95).collect::<Vec<_>>());
        assert_eq!(chunks.overlap.len(), 5);

        drop(first);
        drop(second);
        drop(chunks);
        assert!(fs::read_dir(&dir).unwrap().next().is_none());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn preview_requires_a_full_five_second_chunk_and_cleans_partial_audio() {
        let dir = test_dir("preview-minimum");
        fs::create_dir_all(&dir).unwrap();
        let recording = dir.join("recording-1-2-3.wav");
        let partial_path = dir.join("recording-1-2-3-chunk-0.wav");
        let (preview_tx, preview_rx) = bounded(MAX_PENDING_PREVIEW_CHUNKS);
        let mut chunks = PreviewChunkWriter::new(
            recording,
            PreviewSessionId::next(),
            hound::WavSpec {
                channels: 1,
                sample_rate: 10,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            },
            preview_tx,
        )
        .unwrap();

        chunks.write_samples(&[1; 49]).unwrap();
        assert!(preview_rx.try_recv().is_err());
        assert!(partial_path.exists());
        drop(chunks);
        assert!(!partial_path.exists());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn preview_publication_is_bounded_and_drops_excess_artifacts() {
        let dir = test_dir("preview-bounded");
        fs::create_dir_all(&dir).unwrap();
        let recording = dir.join("recording-1-2-3.wav");
        let (preview_tx, preview_rx) = bounded(MAX_PENDING_PREVIEW_CHUNKS);
        let mut chunks = PreviewChunkWriter::new(
            recording,
            PreviewSessionId::next(),
            hound::WavSpec {
                channels: 1,
                sample_rate: 10,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            },
            preview_tx,
        )
        .unwrap();

        chunks.write_samples(&[1; 140]).unwrap();

        assert_eq!(preview_rx.len(), MAX_PENDING_PREVIEW_CHUNKS);
        assert!(!dir.join("recording-1-2-3-chunk-2.wav").exists());
        drop(chunks);
        drop(preview_rx);
        assert!(fs::read_dir(&dir).unwrap().next().is_none());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn preview_chunk_paths_accept_only_exact_owned_session_names() {
        assert_eq!(
            preview_chunk_path(Path::new("recording-123-4-5.wav"), 6)
                .unwrap()
                .file_name()
                .unwrap(),
            "recording-123-4-5-chunk-6.wav"
        );
        for path in [
            "recording-123.wav",
            "recording-1-2.wav",
            "recording-1-2-3-chunk-4.wav",
            "other-1-2-3.wav",
            "recording-1-2-../../secret.wav",
        ] {
            assert!(preview_chunk_path(Path::new(path), 0).is_err(), "{path}");
        }
    }
}
