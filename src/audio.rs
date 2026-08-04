use std::fs::{self, OpenOptions};
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use crossbeam_channel::{Receiver, Sender, bounded};

use crate::config;

type SharedWavWriter =
    std::sync::Arc<std::sync::Mutex<Option<hound::WavWriter<std::io::BufWriter<std::fs::File>>>>>;

pub struct RecordingSession {
    pub audio_path: PathBuf,
    stop_tx: Sender<()>,
    finished_rx: Receiver<Result<PathBuf, String>>,
    level_bits: Arc<AtomicU32>,
}

impl RecordingSession {
    pub fn stop(&self) {
        let _ = self.stop_tx.try_send(());
    }

    pub fn try_finish(&self) -> Option<Result<PathBuf, String>> {
        self.finished_rx.try_recv().ok()
    }

    /// Returns the latest native input peak in the inclusive `0.0..=1.0` range.
    /// Only this aggregate value crosses into UI state; microphone PCM remains in Rust.
    pub fn latest_level(&self) -> f32 {
        f32::from_bits(self.level_bits.load(Ordering::Relaxed)).clamp(0.0, 1.0)
    }

    pub fn stop_and_discard(self, timeout: Duration) -> Result<()> {
        self.stop();
        let path = match self.finished_rx.recv_timeout(timeout) {
            Ok(Ok(path)) => path,
            Ok(Err(message)) => {
                let _ = fs::remove_file(&self.audio_path);
                return Err(anyhow!(message));
            }
            Err(err) => {
                let _ = fs::remove_file(&self.audio_path);
                return Err(anyhow!(
                    "audio recorder did not stop within {timeout:?}: {err}"
                ));
            }
        };
        if path.exists() {
            fs::remove_file(&path)
                .with_context(|| format!("failed to delete captured audio {}", path.display()))?;
        }
        Ok(())
    }
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
    let level_bits = Arc::new(AtomicU32::new(0.0_f32.to_bits()));
    let worker_level_bits = level_bits.clone();

    thread::spawn(move || {
        let result = record_to_wav(
            worker_path.clone(),
            stop_rx,
            max_duration_seconds,
            input_device_name,
            started_tx,
            worker_level_bits,
        );
        let _ = finished_tx.send(result.map(|_| worker_path).map_err(|err| err.to_string()));
    });

    let started = match started_rx.recv_timeout(Duration::from_secs(3)) {
        Ok(started) => started,
        Err(err) => {
            let _ = stop_tx.try_send(());
            let _ = finished_rx.recv_timeout(Duration::from_secs(1));
            let _ = fs::remove_file(&audio_path);
            return Err(anyhow!("audio recorder did not start: {err}"));
        }
    };
    match started {
        Ok(()) => Ok(RecordingSession {
            audio_path,
            stop_tx,
            finished_rx,
            level_bits,
        }),
        Err(message) => {
            let _ = finished_rx.recv_timeout(Duration::from_secs(1));
            let _ = fs::remove_file(&audio_path);
            Err(anyhow!(message))
        }
    }
}

fn temp_wav_path() -> Result<PathBuf> {
    let dir = recording_dir()?;
    fs::create_dir_all(&dir)
        .with_context(|| format!("failed to create recording directory {}", dir.display()))?;
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    Ok(dir.join(format!("recording-{}-{millis}.wav", std::process::id())))
}

fn recording_dir() -> Result<PathBuf> {
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

fn record_to_wav(
    path: PathBuf,
    stop_rx: Receiver<()>,
    max_duration_seconds: u32,
    input_device_name: Option<String>,
    started_tx: Sender<Result<(), String>>,
    level_bits: Arc<AtomicU32>,
) -> Result<()> {
    let host = cpal::default_host();
    let device = select_input_device(&host, input_device_name.as_deref())?;
    let supported_config = device
        .default_input_config()
        .context("failed to read the microphone input config")?;
    let sample_format = supported_config.sample_format();
    let stream_config: cpal::StreamConfig = supported_config.into();
    let channels = stream_config.channels;
    let wav_spec = hound::WavSpec {
        channels,
        sample_rate: stream_config.sample_rate.0,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options
        .open(&path)
        .with_context(|| format!("failed to create private recording {}", path.display()))?;
    let writer = hound::WavWriter::new(BufWriter::new(file), wav_spec)
        .with_context(|| format!("failed to initialize {}", path.display()))?;
    #[cfg(unix)]
    debug_assert_eq!(
        fs::metadata(&path)
            .map(|metadata| {
                use std::os::unix::fs::PermissionsExt;
                metadata.permissions().mode() & 0o777
            })
            .unwrap_or_default(),
        0o600
    );
    let writer = std::sync::Arc::new(std::sync::Mutex::new(Some(writer)));
    let writer_for_callback = writer.clone();

    let err_fn = |err| eprintln!("audio input stream error: {err}");
    let stream = match sample_format {
        cpal::SampleFormat::F32 => device.build_input_stream(
            &stream_config,
            move |data: &[f32], _| write_f32(data, &writer_for_callback, &level_bits),
            err_fn,
            None,
        )?,
        cpal::SampleFormat::I16 => {
            let level_bits = level_bits.clone();
            device.build_input_stream(
                &stream_config,
                move |data: &[i16], _| write_i16(data, &writer_for_callback, &level_bits),
                err_fn,
                None,
            )?
        }
        cpal::SampleFormat::U16 => {
            let level_bits = level_bits.clone();
            device.build_input_stream(
                &stream_config,
                move |data: &[u16], _| write_u16(data, &writer_for_callback, &level_bits),
                err_fn,
                None,
            )?
        }
        other => {
            let message = format!("unsupported microphone sample format: {other:?}");
            let _ = started_tx.send(Err(message.clone()));
            return Err(anyhow!(message));
        }
    };

    if let Err(err) = stream.play() {
        let message = format!("failed to start microphone stream: {err}");
        let _ = started_tx.send(Err(message.clone()));
        return Err(anyhow!(message));
    }
    let _ = started_tx.send(Ok(()));

    let max_duration = Duration::from_secs(max_duration_seconds.max(1) as u64);
    let started_at = std::time::Instant::now();
    while started_at.elapsed() < max_duration {
        if stop_rx.try_recv().is_ok() {
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }

    drop(stream);
    if let Some(writer) = writer.lock().ok().and_then(|mut guard| guard.take()) {
        writer.finalize().context("failed to finalize WAV file")?;
    }

    Ok(())
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

fn write_f32(input: &[f32], writer: &SharedWavWriter, level_bits: &AtomicU32) {
    publish_level(level_bits, peak_f32(input));
    if let Ok(mut guard) = writer.lock()
        && let Some(writer) = guard.as_mut()
    {
        for sample in input {
            let sample = (*sample).clamp(-1.0, 1.0);
            let sample = (sample * i16::MAX as f32) as i16;
            let _ = writer.write_sample(sample);
        }
    }
}

fn write_i16(input: &[i16], writer: &SharedWavWriter, level_bits: &AtomicU32) {
    publish_level(level_bits, peak_i16(input));
    if let Ok(mut guard) = writer.lock()
        && let Some(writer) = guard.as_mut()
    {
        for sample in input {
            let _ = writer.write_sample(*sample);
        }
    }
}

fn write_u16(input: &[u16], writer: &SharedWavWriter, level_bits: &AtomicU32) {
    publish_level(level_bits, peak_u16(input));
    if let Ok(mut guard) = writer.lock()
        && let Some(writer) = guard.as_mut()
    {
        for sample in input {
            let centered = *sample as i32 - 32768;
            let _ = writer.write_sample(centered.clamp(i16::MIN as i32, i16::MAX as i32) as i16);
        }
    }
}

fn publish_level(level_bits: &AtomicU32, level: f32) {
    level_bits.store(level.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
}

fn peak_f32(input: &[f32]) -> f32 {
    input
        .iter()
        .copied()
        .filter(|sample| sample.is_finite())
        .map(f32::abs)
        .fold(0.0, f32::max)
        .clamp(0.0, 1.0)
}

fn peak_i16(input: &[i16]) -> f32 {
    input
        .iter()
        .copied()
        .map(|sample| (sample as i32).unsigned_abs() as f32 / 32768.0)
        .fold(0.0, f32::max)
        .clamp(0.0, 1.0)
}

fn peak_u16(input: &[u16]) -> f32 {
    input
        .iter()
        .copied()
        .map(|sample| (sample as i32 - 32768).unsigned_abs() as f32 / 32768.0)
        .fold(0.0, f32::max)
        .clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stop_and_discard_waits_for_shutdown_and_removes_pcm() {
        let path = std::env::temp_dir().join(format!(
            "scribe-recording-discard-{}-{}.wav",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        fs::write(&path, b"pcm").unwrap();
        let (stop_tx, stop_rx) = bounded(1);
        let (finished_tx, finished_rx) = bounded(1);
        let worker_path = path.clone();
        let worker = thread::spawn(move || {
            stop_rx
                .recv_timeout(Duration::from_secs(1))
                .expect("stop signal");
            finished_tx.send(Ok(worker_path)).unwrap();
        });
        let session = RecordingSession {
            audio_path: path.clone(),
            stop_tx,
            finished_rx,
            level_bits: Arc::new(AtomicU32::new(0.0_f32.to_bits())),
        };

        session.stop_and_discard(Duration::from_secs(1)).unwrap();

        worker.join().unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn stop_and_discard_attempts_cleanup_when_worker_reports_failure() {
        let path = std::env::temp_dir().join(format!(
            "scribe-recording-failed-{}-{}.wav",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        fs::write(&path, b"pcm").unwrap();
        let (stop_tx, _stop_rx) = bounded(1);
        let (finished_tx, finished_rx) = bounded(1);
        finished_tx.send(Err("recorder failed".to_owned())).unwrap();
        let session = RecordingSession {
            audio_path: path.clone(),
            stop_tx,
            finished_rx,
            level_bits: Arc::new(AtomicU32::new(0.0_f32.to_bits())),
        };

        let error = session
            .stop_and_discard(Duration::from_secs(1))
            .unwrap_err();

        assert!(error.to_string().contains("recorder failed"));
        assert!(!path.exists());
    }

    #[test]
    fn native_level_conversion_is_clamped_and_format_neutral() {
        assert_eq!(peak_f32(&[]), 0.0);
        assert_eq!(peak_f32(&[f32::NAN, -0.5, 2.0]), 1.0);
        assert_eq!(peak_i16(&[0, -16384]), 0.5);
        assert_eq!(peak_i16(&[i16::MIN]), 1.0);
        assert_eq!(peak_u16(&[32768]), 0.0);
        assert_eq!(peak_u16(&[0]), 1.0);
        assert!(peak_u16(&[u16::MAX]) < 1.0);
    }
}
