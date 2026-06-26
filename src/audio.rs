use std::fs;
use std::path::PathBuf;
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
        let result = record_to_wav(
            worker_path.clone(),
            stop_rx,
            max_duration_seconds,
            input_device_name,
            started_tx,
        );
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
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    Ok(dir.join(format!("recording-{millis}.wav")))
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
    let wav_spec = hound::WavSpec {
        channels,
        sample_rate: stream_config.sample_rate.0,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let writer = hound::WavWriter::create(&path, wav_spec)
        .with_context(|| format!("failed to create {}", path.display()))?;
    let writer = std::sync::Arc::new(std::sync::Mutex::new(Some(writer)));
    let writer_for_callback = writer.clone();

    let err_fn = |err| eprintln!("audio input stream error: {err}");
    let stream = match sample_format {
        cpal::SampleFormat::F32 => device.build_input_stream(
            &stream_config,
            move |data: &[f32], _| write_f32(data, &writer_for_callback),
            err_fn,
            None,
        )?,
        cpal::SampleFormat::I16 => device.build_input_stream(
            &stream_config,
            move |data: &[i16], _| write_i16(data, &writer_for_callback),
            err_fn,
            None,
        )?,
        cpal::SampleFormat::U16 => device.build_input_stream(
            &stream_config,
            move |data: &[u16], _| write_u16(data, &writer_for_callback),
            err_fn,
            None,
        )?,
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
    if let Some(target_name) = input_device_name.filter(|name| !name.trim().is_empty()) {
        if let Ok(devices) = host.input_devices() {
            for device in devices {
                if device.name().ok().as_deref() == Some(target_name) {
                    return Ok(device);
                }
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

fn write_f32(input: &[f32], writer: &SharedWavWriter) {
    if let Ok(mut guard) = writer.lock() {
        if let Some(writer) = guard.as_mut() {
            for sample in input {
                let sample = (*sample).clamp(-1.0, 1.0);
                let sample = (sample * i16::MAX as f32) as i16;
                let _ = writer.write_sample(sample);
            }
        }
    }
}

fn write_i16(input: &[i16], writer: &SharedWavWriter) {
    if let Ok(mut guard) = writer.lock() {
        if let Some(writer) = guard.as_mut() {
            for sample in input {
                let _ = writer.write_sample(*sample);
            }
        }
    }
}

fn write_u16(input: &[u16], writer: &SharedWavWriter) {
    if let Ok(mut guard) = writer.lock() {
        if let Some(writer) = guard.as_mut() {
            for sample in input {
                let centered = *sample as i32 - 32768;
                let _ =
                    writer.write_sample(centered.clamp(i16::MIN as i32, i16::MAX as i32) as i16);
            }
        }
    }
}
