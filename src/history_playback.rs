use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use crossbeam_channel::{Receiver, Sender, TrySendError, bounded, unbounded};

use crate::history::load_retained_audio_file;
use crate::prepared_audio::PreparedAudio;

const COMMAND_CAPACITY: usize = 4;
const PLAYBACK_POLL_INTERVAL: Duration = Duration::from_millis(10);
const PLAYBACK_SHUTDOWN_TIMEOUT: Duration = Duration::from_millis(500);
const PLAYBACK_DRAIN_SAFETY_MARGIN: Duration = Duration::from_millis(50);

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PlaybackEvent {
    Started { history_id: i64 },
    Completed { history_id: i64 },
    Stopped { history_id: i64 },
    Failed { history_id: i64, error: String },
}

#[derive(Clone, Copy, Debug, thiserror::Error, PartialEq, Eq)]
pub(crate) enum PlaybackCommandError {
    #[error("playback worker command queue is full")]
    Busy,
    #[error("playback worker is unavailable")]
    Disconnected,
    #[error("history id must be positive")]
    InvalidHistoryId,
}

enum PlaybackCommand {
    Play { history_id: i64, path: PathBuf },
    Stop,
    Shutdown,
}

pub(crate) struct PlaybackService {
    command_tx: Sender<PlaybackCommand>,
    event_rx: Receiver<PlaybackEvent>,
    worker: Option<JoinHandle<()>>,
}

impl PlaybackService {
    pub(crate) fn new() -> std::io::Result<Self> {
        let (command_tx, command_rx) = bounded(COMMAND_CAPACITY);
        // Commands are bounded. Events are state transitions only and are
        // unbounded so terminal playback state cannot be silently dropped.
        let (event_tx, event_rx) = unbounded();
        let worker = thread::Builder::new()
            .name("scribe-history-playback".into())
            .spawn(move || playback_worker(command_rx, event_tx))?;
        Ok(Self {
            command_tx,
            event_rx,
            worker: Some(worker),
        })
    }

    pub(crate) fn play(
        &self,
        history_id: i64,
        validated_wav_path: PathBuf,
    ) -> Result<(), PlaybackCommandError> {
        if history_id <= 0 {
            return Err(PlaybackCommandError::InvalidHistoryId);
        }
        try_send(
            &self.command_tx,
            PlaybackCommand::Play {
                history_id,
                path: validated_wav_path,
            },
        )
    }

    pub(crate) fn stop(&self) -> Result<(), PlaybackCommandError> {
        try_send(&self.command_tx, PlaybackCommand::Stop)
    }

    pub(crate) fn try_next_event(&self) -> Option<PlaybackEvent> {
        self.event_rx.try_recv().ok()
    }
}

impl Drop for PlaybackService {
    fn drop(&mut self) {
        let shutdown_sent = self
            .command_tx
            .send_timeout(PlaybackCommand::Shutdown, PLAYBACK_POLL_INTERVAL)
            .is_ok();
        if let Some(worker) = self.worker.take() {
            if shutdown_sent {
                let deadline = std::time::Instant::now() + PLAYBACK_SHUTDOWN_TIMEOUT;
                while !worker.is_finished() && std::time::Instant::now() < deadline {
                    thread::sleep(PLAYBACK_POLL_INTERVAL);
                }
            }
            if worker.is_finished() {
                let _ = worker.join();
            }
        }
    }
}

fn try_send(
    sender: &Sender<PlaybackCommand>,
    command: PlaybackCommand,
) -> Result<(), PlaybackCommandError> {
    sender.try_send(command).map_err(|error| match error {
        TrySendError::Full(_) => PlaybackCommandError::Busy,
        TrySendError::Disconnected(_) => PlaybackCommandError::Disconnected,
    })
}

struct ActivePlayback<T> {
    history_id: i64,
    _stream: T,
    finished: Arc<AtomicBool>,
    failed: Arc<AtomicBool>,
}

#[derive(Default)]
struct PlaybackDrain {
    deadline: Option<Instant>,
}

impl PlaybackDrain {
    fn arm(
        &mut self,
        now: Instant,
        predicted_queue_delay: Duration,
        callback_buffer_duration: Duration,
    ) -> Result<(), String> {
        if self.deadline.is_some() {
            return Ok(());
        }
        let drain_delay = predicted_queue_delay
            .checked_add(callback_buffer_duration)
            .and_then(|duration| duration.checked_add(PLAYBACK_DRAIN_SAFETY_MARGIN))
            .ok_or_else(|| "audio output drain duration overflowed".to_owned())?;
        self.deadline = now.checked_add(drain_delay);
        if self.deadline.is_none() {
            return Err("audio output drain deadline overflowed".into());
        }
        Ok(())
    }

    fn elapsed(&self, now: Instant) -> bool {
        self.deadline.is_some_and(|deadline| now >= deadline)
    }
}

fn playback_worker(command_rx: Receiver<PlaybackCommand>, event_tx: Sender<PlaybackEvent>) {
    let mut active = None;
    loop {
        let command = if active.is_some() {
            match command_rx.recv_timeout(PLAYBACK_POLL_INTERVAL) {
                Ok(command) => Some(command),
                Err(crossbeam_channel::RecvTimeoutError::Timeout) => None,
                Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
            }
        } else {
            match command_rx.recv() {
                Ok(command) => Some(command),
                Err(_) => break,
            }
        };

        if let Some(command) = command {
            match command {
                PlaybackCommand::Play { history_id, path } => {
                    emit_stopped(&mut active, &event_tx);
                    match start_playback(history_id, path) {
                        Ok(playback) => {
                            emit(&event_tx, PlaybackEvent::Started { history_id });
                            active = Some(playback);
                        }
                        Err(error) => emit(&event_tx, PlaybackEvent::Failed { history_id, error }),
                    }
                }
                PlaybackCommand::Stop => emit_stopped(&mut active, &event_tx),
                PlaybackCommand::Shutdown => {
                    emit_stopped(&mut active, &event_tx);
                    break;
                }
            }
            continue;
        }

        let Some(playback) = active.as_ref() else {
            continue;
        };
        if playback.failed.load(Ordering::Acquire) {
            let history_id = playback.history_id;
            active.take();
            emit(
                &event_tx,
                PlaybackEvent::Failed {
                    history_id,
                    error: "audio output stream failed".into(),
                },
            );
        } else if playback.finished.load(Ordering::Acquire) {
            let history_id = playback.history_id;
            active.take();
            emit(&event_tx, PlaybackEvent::Completed { history_id });
        }
    }
}

fn emit_stopped<T>(active: &mut Option<ActivePlayback<T>>, event_tx: &Sender<PlaybackEvent>) {
    if let Some(playback) = active.take() {
        emit(
            event_tx,
            PlaybackEvent::Stopped {
                history_id: playback.history_id,
            },
        );
    }
}

fn emit(event_tx: &Sender<PlaybackEvent>, event: PlaybackEvent) {
    let _ = event_tx.send(event);
}

fn start_playback(history_id: i64, path: PathBuf) -> Result<ActivePlayback<cpal::Stream>, String> {
    let audio = load_bounded_retained_audio(&path)?;
    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .ok_or_else(|| "no default audio output device is available".to_owned())?;
    let supported = device
        .default_output_config()
        .map_err(|error| format!("failed to read default output configuration: {error}"))?;
    let sample_format = supported.sample_format();
    let config: cpal::StreamConfig = supported.into();
    let finished = Arc::new(AtomicBool::new(false));
    let failed = Arc::new(AtomicBool::new(false));
    let stream = build_output_stream(
        &device,
        &config,
        sample_format,
        audio.samples,
        audio.sample_rate,
        Arc::clone(&finished),
        Arc::clone(&failed),
    )?;
    stream
        .play()
        .map_err(|error| format!("failed to start audio output: {error}"))?;
    Ok(ActivePlayback {
        history_id,
        _stream: stream,
        finished,
        failed,
    })
}

fn load_bounded_retained_audio(path: &std::path::Path) -> Result<PreparedAudio, String> {
    load_retained_audio_file(path)
        .map_err(|error| format!("retained audio path was rejected: {error}"))
}

fn build_output_stream(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    sample_format: cpal::SampleFormat,
    mono: Vec<f32>,
    source_rate: u32,
    finished: Arc<AtomicBool>,
    failed: Arc<AtomicBool>,
) -> Result<cpal::Stream, String> {
    macro_rules! build {
        ($sample:ty) => {{
            build_typed_output_stream::<$sample>(
                device,
                config,
                mono,
                source_rate,
                finished,
                failed,
            )
        }};
    }
    match sample_format {
        cpal::SampleFormat::I8 => build!(i8),
        cpal::SampleFormat::I16 => build!(i16),
        cpal::SampleFormat::I32 => build!(i32),
        cpal::SampleFormat::I64 => build!(i64),
        cpal::SampleFormat::U8 => build!(u8),
        cpal::SampleFormat::U16 => build!(u16),
        cpal::SampleFormat::U32 => build!(u32),
        cpal::SampleFormat::U64 => build!(u64),
        cpal::SampleFormat::F32 => build!(f32),
        cpal::SampleFormat::F64 => build!(f64),
        other => Err(format!("unsupported output sample format: {other}")),
    }
}

fn build_typed_output_stream<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    mono: Vec<f32>,
    source_rate: u32,
    finished: Arc<AtomicBool>,
    failed: Arc<AtomicBool>,
) -> Result<cpal::Stream, String>
where
    T: cpal::SizedSample + cpal::FromSample<f32>,
{
    let output_rate = config.sample_rate.0;
    let channels = usize::from(config.channels);
    let total_output_samples = output_sample_count(mono.len(), source_rate, output_rate, channels)?;
    let mut cursor = 0;
    let mut drain = PlaybackDrain::default();
    let callback_failed = Arc::clone(&failed);
    device
        .build_output_stream(
            config,
            move |output: &mut [T], callback| {
                let final_buffer_submitted = fill_resampled_output(
                    output,
                    &mono,
                    source_rate,
                    output_rate,
                    channels,
                    total_output_samples,
                    &mut cursor,
                );
                let now = Instant::now();
                if final_buffer_submitted {
                    let timestamp = callback.timestamp();
                    let predicted_queue_delay = timestamp
                        .playback
                        .duration_since(&timestamp.callback)
                        .unwrap_or_default();
                    let callback_frames = output.len().div_ceil(channels);
                    let callback_buffer_duration =
                        Duration::from_secs_f64(callback_frames as f64 / f64::from(output_rate));
                    if drain
                        .arm(now, predicted_queue_delay, callback_buffer_duration)
                        .is_err()
                    {
                        callback_failed.store(true, Ordering::Release);
                    }
                }
                if drain.elapsed(now) {
                    finished.store(true, Ordering::Release);
                }
            },
            move |_error| failed.store(true, Ordering::Release),
            None,
        )
        .map_err(|error| format!("failed to build audio output stream: {error}"))
}

fn fill_resampled_output<T>(
    output: &mut [T],
    mono: &[f32],
    source_rate: u32,
    output_rate: u32,
    channels: usize,
    total_output_samples: usize,
    cursor: &mut usize,
) -> bool
where
    T: cpal::Sample + cpal::FromSample<f32>,
{
    let had_audio_before_fill = *cursor < total_output_samples;
    for destination in output {
        let sample = if *cursor >= total_output_samples {
            0.0
        } else {
            let output_frame = *cursor / channels;
            let numerator = output_frame as u64 * u64::from(source_rate);
            let lower = (numerator / u64::from(output_rate)) as usize;
            let fraction = (numerator % u64::from(output_rate)) as f32 / output_rate as f32;
            let left = mono[lower.min(mono.len() - 1)];
            let right = mono[(lower + 1).min(mono.len() - 1)];
            left + (right - left) * fraction
        };
        *destination = T::from_sample(sample);
        *cursor = cursor.saturating_add(1);
    }
    had_audio_before_fill && *cursor >= total_output_samples
}

fn output_sample_count(
    input_frames: usize,
    source_rate: u32,
    output_rate: u32,
    output_channels: usize,
) -> Result<usize, String> {
    if input_frames == 0 || source_rate == 0 || output_rate == 0 || output_channels == 0 {
        return Err("retained audio or output configuration is empty".into());
    }
    input_frames
        .checked_mul(output_rate as usize)
        .and_then(|value| value.checked_add(source_rate as usize - 1))
        .map(|value| value / source_rate as usize)
        .and_then(|frames| frames.checked_mul(output_channels))
        .ok_or_else(|| "retained audio is too large for the output device".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resampling_interpolates_and_preserves_duration() {
        let mut output = [0.0_f32; 4];
        let mut cursor = 0;
        assert!(fill_resampled_output(
            &mut output,
            &[0.0, 1.0],
            2,
            4,
            1,
            4,
            &mut cursor,
        ));
        assert_eq!(output, [0.0, 0.5, 1.0, 1.0]);
    }

    #[test]
    fn output_preparation_fills_every_channel() {
        let mut output = [0.0_f32; 4];
        let mut cursor = 0;
        assert!(fill_resampled_output(
            &mut output,
            &[0.25, -0.5],
            16_000,
            16_000,
            2,
            4,
            &mut cursor,
        ));
        assert_eq!(output, [0.25, 0.25, -0.5, -0.5]);
    }

    #[test]
    fn final_buffer_submission_is_distinct_from_device_drain() {
        let mut cursor = 0;
        let mut first = [0.0_f32; 3];
        assert!(fill_resampled_output(
            &mut first,
            &[0.25, -0.5],
            16_000,
            16_000,
            1,
            2,
            &mut cursor,
        ));
        assert_eq!(first, [0.25, -0.5, 0.0]);
        let mut silence = [1.0_f32; 2];
        assert!(!fill_resampled_output(
            &mut silence,
            &[0.25, -0.5],
            16_000,
            16_000,
            1,
            2,
            &mut cursor,
        ));
        assert_eq!(silence, [0.0, 0.0]);
    }

    #[test]
    fn drain_waits_for_predicted_queue_buffer_and_safety_margin() {
        let start = Instant::now();
        let mut drain = PlaybackDrain::default();
        drain
            .arm(start, Duration::from_millis(20), Duration::from_millis(10))
            .unwrap();

        assert!(!drain.elapsed(start + Duration::from_millis(79)));
        assert!(drain.elapsed(start + Duration::from_millis(80)));
    }

    #[test]
    fn repeated_callbacks_do_not_shorten_an_armed_drain_deadline() {
        let start = Instant::now();
        let mut drain = PlaybackDrain::default();
        drain
            .arm(start, Duration::from_millis(40), Duration::from_millis(20))
            .unwrap();
        drain
            .arm(
                start + Duration::from_millis(10),
                Duration::ZERO,
                Duration::ZERO,
            )
            .unwrap();

        assert!(!drain.elapsed(start + Duration::from_millis(109)));
        assert!(drain.elapsed(start + Duration::from_millis(110)));
    }

    #[test]
    fn stopping_active_playback_preserves_correlation() {
        let (event_tx, event_rx) = bounded(1);
        let mut active = Some(ActivePlayback {
            history_id: 42,
            _stream: (),
            finished: Arc::new(AtomicBool::new(false)),
            failed: Arc::new(AtomicBool::new(false)),
        });
        emit_stopped(&mut active, &event_tx);
        assert!(active.is_none());
        assert_eq!(
            event_rx.try_recv().expect("stopped event"),
            PlaybackEvent::Stopped { history_id: 42 }
        );
    }

    #[test]
    fn invalid_history_id_is_rejected_without_touching_audio_hardware() {
        let service = PlaybackService::new().expect("playback service");
        assert_eq!(
            service.play(0, PathBuf::from("unused.wav")),
            Err(PlaybackCommandError::InvalidHistoryId)
        );
    }
}
