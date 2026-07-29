use std::fs::{self, File, OpenOptions};
use std::io::{Seek, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use crossbeam_channel::{Receiver, Sender, bounded};
use fs2::FileExt;

use crate::config;

const WAV_HEADER_BYTES: u64 = 44;
const WAV_BYTES_PER_SAMPLE: u64 = 2;
const RECORDING_STORAGE_RESERVE_BYTES: u64 = 256 * 1024 * 1024;
const STALE_RECORDING_AGE: Duration = Duration::from_secs(24 * 60 * 60);
static RECORDING_NONCE: AtomicU64 = AtomicU64::new(0);
const AUDIO_QUEUE_CAPACITY: usize = 8;

pub struct RecordingSession {
    pub audio_path: PathBuf,
    stop_tx: Sender<()>,
    finished_rx: Receiver<Result<PathBuf, String>>,
}

impl RecordingSession {
    pub fn stop(&self) {
        let _ = self.stop_tx.try_send(());
    }

    pub fn try_finish(&self) -> Option<Result<PathBuf, String>> {
        self.finished_rx.try_recv().ok()
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
) -> Result<RecordingSession> {
    let audio_path = temp_wav_path()?;
    let (stop_tx, stop_rx) = bounded::<()>(1);
    let (finished_tx, finished_rx) = bounded::<Result<PathBuf, String>>(1);
    let (started_tx, started_rx) = bounded::<Result<(), String>>(1);
    let worker_path = audio_path.clone();

    thread::spawn(move || {
        let startup_error_tx = started_tx.clone();
        let result = record_to_wav(
            worker_path.clone(),
            stop_rx,
            max_duration_seconds,
            input_device_name,
            started_tx,
        );
        if let Err(err) = &result {
            let _ = startup_error_tx.try_send(Err(err.to_string()));
        }
        let _ = fs::remove_file(recording_lock_path(&worker_path));
        if result.is_err() {
            let _ = fs::remove_file(&worker_path);
        }
        let _ = finished_tx.send(result.map(|_| worker_path).map_err(|err| err.to_string()));
    });

    match started_rx
        .recv_timeout(Duration::from_secs(3))
        .context("audio recorder did not start")?
    {
        Ok(()) => Ok(RecordingSession {
            audio_path,
            stop_tx,
            finished_rx,
        }),
        Err(message) => Err(anyhow!(message)),
    }
}

fn temp_wav_path() -> Result<PathBuf> {
    let dir = config::cache_dir()?.join("recordings");
    fs::create_dir_all(&dir)
        .with_context(|| format!("failed to create recording directory {}", dir.display()))?;
    if let Err(err) = cleanup_stale_recordings(&dir, SystemTime::now(), STALE_RECORDING_AGE) {
        eprintln!("could not clean stale Scribe recordings: {err}");
    }
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let nonce = RECORDING_NONCE.fetch_add(1, Ordering::Relaxed);
    Ok(dir.join(format!(
        "recording-{millis}-{}-{nonce}.wav",
        std::process::id()
    )))
}

fn record_to_wav(
    path: PathBuf,
    stop_rx: Receiver<()>,
    max_duration_seconds: u32,
    input_device_name: Option<String>,
    started_tx: Sender<Result<(), String>>,
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
    let mut writer = hound::WavWriter::create(&path, wav_spec)
        .with_context(|| format!("failed to create {}", path.display()))?;
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
    let _ = started_tx.send(Ok(()));

    let max_duration = Duration::from_secs(max_duration_seconds.max(1) as u64);
    let started_at = std::time::Instant::now();
    let mut callback_error = None;
    while !recording_limit_reached(started_at.elapsed(), max_duration) {
        if let Ok(message) = callback_error_rx.try_recv() {
            callback_error = Some(message);
            break;
        }
        if stop_rx.try_recv().is_ok() {
            break;
        }
        match sample_rx.recv_timeout(Duration::from_millis(10)) {
            Ok(samples) => {
                if let Err(err) = write_samples(&mut writer, samples) {
                    callback_error = Some(format!("failed to write recording audio: {err}"));
                    break;
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
    if callback_error.is_none() {
        callback_error = callback_error_rx.try_recv().ok();
    }
    while callback_error.is_none() {
        match sample_rx.try_recv() {
            Ok(samples) => {
                if let Err(err) = write_samples(&mut writer, samples) {
                    callback_error = Some(format!("failed to write recording audio: {err}"));
                }
            }
            Err(_) => break,
        }
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

fn ensure_recording_space(
    recording_dir: &Path,
    sample_rate: u32,
    channels: u16,
    duration_seconds: u32,
) -> Result<()> {
    let estimated = estimated_wav_bytes(sample_rate, channels, duration_seconds)
        .ok_or_else(|| anyhow!("recording storage estimate overflowed"))?;
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
    let mut name = recording_path.as_os_str().to_os_string();
    name.push(".lock");
    PathBuf::from(name)
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
    let Some(id) = name
        .strip_prefix("recording-")
        .and_then(|name| name.strip_suffix(".wav"))
    else {
        return false;
    };
    !id.is_empty()
        && id
            .split('-')
            .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
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
        let _ = fs::remove_file(recording_lock_path(&path));
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
        let active = dir.join("recording-222.wav");
        let unrelated = dir.join("notes.wav");
        fs::write(&stale, b"stale").unwrap();
        fs::write(&active, b"active").unwrap();
        fs::write(&unrelated, b"unrelated").unwrap();
        let active_lock = lock_recording_path(&active).unwrap();

        let removed = cleanup_stale_recordings(&dir, SystemTime::now(), Duration::ZERO).unwrap();

        assert_eq!(removed, 1);
        assert!(!stale.exists());
        assert!(active.exists());
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
    fn full_sample_queue_is_reported_instead_of_dropping_audio_silently() {
        let (sample_tx, _sample_rx) = bounded(1);
        let (error_tx, error_rx) = bounded(1);
        sample_tx.try_send(vec![1_i16]).unwrap();

        queue_samples(vec![2_i16], &sample_tx, &error_tx);

        assert!(error_rx.try_recv().unwrap().contains("could not keep up"));
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
}
