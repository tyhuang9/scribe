use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::time::Duration;

use crate::onnx_worker::SileroVadDecision;
use crate::prepared_audio::{PREPARED_SAMPLE_RATE, PreparedAudio};
use crate::silero_vad_native::WINDOW_SAMPLES;
use crate::streaming::{DECODE_INTERVAL_MS, PreviewAudioPublisher, ROLLING_WINDOW_MS};

use super::{
    CaptureError, CaptureIntent, CaptureOptions, CaptureStopReason, LevelSnapshot,
    MAX_CAPTURE_PREPARED_FRAMES, SpeechDetector, VadOptions, input_format_is_credible,
};

const LEVEL_WINDOW_SAMPLES: usize = (PREPARED_SAMPLE_RATE as usize) * 30 / 1_000;
const SIGNAL_DIAGNOSTIC_WINDOW_SAMPLES: usize = (PREPARED_SAMPLE_RATE as usize) / 100;
const TARGET_RMS: f32 = 0.1;
const TARGET_PEAK_CEILING: f32 = 0.95;
const MAX_NORMALIZATION_GAIN: f32 = 8.0;
const MIN_NORMALIZABLE_RMS: f32 = 0.000_1;
const PREVIEW_INTERVAL_FRAMES: usize =
    PREPARED_SAMPLE_RATE as usize * DECODE_INTERVAL_MS as usize / 1_000;
const PREVIEW_WINDOW_FRAMES: usize =
    PREPARED_SAMPLE_RATE as usize * ROLLING_WINDOW_MS as usize / 1_000;

pub(super) struct Pipeline {
    source_sample_rate: u32,
    source_channels: u16,
    source_frames: usize,
    channel_samples: usize,
    channel_sum: f64,
    resampler: StreamingLinearResampler,
    prepared: Vec<f32>,
    retain_audio: bool,
    limit_exceeded: bool,
    levels: LevelTracker,
    vad_enabled: bool,
    vad: VadTracker,
    preview_publisher: Option<PreviewAudioPublisher>,
    next_preview_frame: usize,
}

impl Pipeline {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        source_sample_rate: u32,
        source_channels: u16,
        options: CaptureOptions,
        detector: Option<Box<dyn SpeechDetector>>,
        level_bits: Arc<AtomicU32>,
        peak_bits: Arc<AtomicU32>,
        level_observed: Arc<AtomicBool>,
        level_revision: Arc<AtomicU64>,
    ) -> Result<Self, CaptureError> {
        if !input_format_is_credible(source_sample_rate, source_channels) {
            return Err(CaptureError::InvalidInputFormat {
                sample_rate: source_sample_rate,
                channels: source_channels,
            });
        }
        options.vad.validate()?;
        let vad_enabled = options.vad_enabled && options.intent == CaptureIntent::Dictation;
        if vad_enabled != detector.is_some() {
            return Err(CaptureError::InvalidOptions(
                "speech-classifying capture requires exactly one Silero VAD session",
            ));
        }
        Ok(Self {
            source_sample_rate,
            source_channels,
            source_frames: 0,
            channel_samples: 0,
            channel_sum: 0.0,
            resampler: StreamingLinearResampler::new(source_sample_rate, PREPARED_SAMPLE_RATE),
            prepared: Vec::new(),
            retain_audio: options.intent == CaptureIntent::Dictation,
            limit_exceeded: false,
            levels: LevelTracker::new(level_bits, peak_bits, level_observed, level_revision),
            vad_enabled,
            vad: VadTracker::new(options.vad, options.endpointing_enabled, detector),
            preview_publisher: None,
            next_preview_frame: PREVIEW_INTERVAL_FRAMES,
        })
    }

    pub(super) fn with_preview_publisher(
        mut self,
        publisher: Option<PreviewAudioPublisher>,
    ) -> Self {
        self.preview_publisher = publisher;
        self
    }

    pub(super) fn push_interleaved(&mut self, sample: f32) -> Result<(), CaptureError> {
        self.channel_sum += finite_unit(sample) as f64;
        self.channel_samples += 1;
        if self.channel_samples != self.source_channels as usize {
            return Ok(());
        }

        let mono = (self.channel_sum / self.source_channels as f64).clamp(-1.0, 1.0) as f32;
        self.channel_sum = 0.0;
        self.channel_samples = 0;
        self.source_frames += 1;

        let prepared = &mut self.prepared;
        let limit_exceeded = &mut self.limit_exceeded;
        let levels = &mut self.levels;
        let vad = &mut self.vad;
        let vad_enabled = self.vad_enabled;
        let retain_audio = self.retain_audio;
        let mut vad_result = Ok(());
        self.resampler.push(mono, |output| {
            if retain_audio {
                push_bounded(
                    prepared,
                    limit_exceeded,
                    output,
                    MAX_CAPTURE_PREPARED_FRAMES,
                );
            }
            levels.push(output);
            if vad_enabled && vad_result.is_ok() {
                vad_result = vad.push(output);
            }
        });
        vad_result
    }

    pub(super) fn source_frames(&self) -> usize {
        self.source_frames
    }

    /// Clones and normalizes only completed rolling windows. The full capture
    /// buffer remains untouched so enabling preview cannot alter final audio.
    pub(super) fn publish_due_previews(&mut self) {
        let Some(publisher) = self.preview_publisher.as_ref() else {
            return;
        };
        if self.prepared.len() < self.next_preview_frame {
            return;
        }
        // If capture processing catches up after a delay, older due windows
        // are already obsolete under replace-latest scheduling. Publish only
        // the newest complete cadence boundary to avoid burst allocations and
        // stale decodes.
        let missed_intervals =
            (self.prepared.len() - self.next_preview_frame) / PREVIEW_INTERVAL_FRAMES;
        let end = self
            .next_preview_frame
            .saturating_add(missed_intervals.saturating_mul(PREVIEW_INTERVAL_FRAMES));
        self.next_preview_frame = end.saturating_add(PREVIEW_INTERVAL_FRAMES);
        let start = end.saturating_sub(PREVIEW_WINDOW_FRAMES);
        let mut samples = self.prepared[start..end].to_vec();
        normalize_loudness(&mut samples);
        if !matches!(publisher.publish_window(start as u64, samples), Ok(true)) {
            self.preview_publisher = None;
        }
    }

    pub(super) fn limit_exceeded(&self) -> bool {
        self.limit_exceeded
    }

    pub(super) fn endpoint_triggered(&self) -> bool {
        self.vad_enabled && self.vad.endpoint_frame.is_some()
    }

    pub(super) fn speech_trigger_elapsed(&self) -> Option<Duration> {
        self.vad
            .speech_trigger_frame
            .filter(|_| self.vad_enabled)
            .map(prepared_frames_to_duration)
    }

    pub(super) fn finish(
        &mut self,
        stop_reason: CaptureStopReason,
    ) -> Result<Option<PreparedAudio>, CaptureError> {
        let prepared = &mut self.prepared;
        let limit_exceeded = &mut self.limit_exceeded;
        let levels = &mut self.levels;
        let vad = &mut self.vad;
        let vad_enabled = self.vad_enabled;
        let retain_audio = self.retain_audio;
        let mut vad_result = Ok(());
        self.resampler.finish(|output| {
            if retain_audio {
                push_bounded(
                    prepared,
                    limit_exceeded,
                    output,
                    MAX_CAPTURE_PREPARED_FRAMES,
                );
            }
            levels.push(output);
            if vad_enabled && vad_result.is_ok() {
                vad_result = vad.push(output);
            }
        });
        vad_result?;
        self.levels.finish_windows();
        self.vad.finish_detector()?;

        if self.limit_exceeded {
            return Err(CaptureError::PreparedAudioLimit {
                maximum_frames: MAX_CAPTURE_PREPARED_FRAMES,
            });
        }

        if !self.retain_audio {
            return Ok(None);
        }

        if !self.vad_enabled {
            if self.prepared.is_empty() || self.source_frames == 0 {
                return Ok(None);
            }
            normalize_loudness(&mut self.prepared);
            return PreparedAudio::from_captured_mono(
                std::mem::take(&mut self.prepared),
                self.source_sample_rate,
                self.source_channels,
                self.source_frames,
            )
            .map(Some)
            .map_err(|error| CaptureError::Preparation(error.to_string()));
        }

        let Some(speech_start) = self.vad.speech_start_frame else {
            return Ok(None);
        };
        let start =
            speech_start.saturating_sub(duration_to_prepared_frames(self.vad.options.pre_roll));
        let end = match stop_reason {
            CaptureStopReason::Explicit => self.prepared.len(),
            CaptureStopReason::Endpoint | CaptureStopReason::MaximumDuration => self
                .vad
                .last_voice_frame
                .saturating_add(duration_to_prepared_frames(self.vad.options.post_roll))
                .min(self.prepared.len()),
        };
        if start >= end {
            return Ok(None);
        }
        let mut samples = std::mem::take(&mut self.prepared);
        samples.truncate(end);
        if start > 0 {
            samples.drain(..start);
        }
        normalize_loudness(&mut samples);
        let source_frames = prepared_to_source_frames(samples.len(), self.source_sample_rate);
        PreparedAudio::from_captured_mono(
            samples,
            self.source_sample_rate,
            self.source_channels,
            source_frames,
        )
        .map(Some)
        .map_err(|error| CaptureError::Preparation(error.to_string()))
    }

    pub(super) fn maximum_levels(&self) -> LevelSnapshot {
        self.levels.maximum()
    }

    pub(super) fn cancel_speech_detector(&mut self) -> Result<(), CaptureError> {
        self.vad.cancel_detector()
    }
}

fn push_bounded(samples: &mut Vec<f32>, exceeded: &mut bool, sample: f32, maximum: usize) {
    if samples.len() < maximum {
        samples.push(sample);
    } else {
        *exceeded = true;
    }
}

/// Applies deterministic RMS normalization after VAD has made its decisions.
///
/// The gain is bounded so quiet background noise is never amplified without
/// limit, and a peak ceiling prevents the common preparation stage from
/// introducing clipping. Levels and VAD intentionally observe the original
/// signal rather than this presentation/decode gain.
fn normalize_loudness(samples: &mut [f32]) {
    if samples.is_empty() {
        return;
    }
    let (sum_squares, peak) = samples.iter().fold((0.0_f64, 0.0_f32), |state, sample| {
        (
            state.0 + f64::from(*sample) * f64::from(*sample),
            state.1.max(sample.abs()),
        )
    });
    let rms = (sum_squares / samples.len() as f64).sqrt() as f32;
    if rms < MIN_NORMALIZABLE_RMS || peak <= f32::EPSILON {
        return;
    }

    let rms_gain = (TARGET_RMS / rms).min(MAX_NORMALIZATION_GAIN);
    let peak_gain = TARGET_PEAK_CEILING / peak;
    let gain = rms_gain.min(peak_gain);
    for sample in samples {
        *sample = finite_unit(*sample * gain);
    }
}

fn finite_unit(sample: f32) -> f32 {
    if sample.is_finite() {
        sample.clamp(-1.0, 1.0)
    } else {
        0.0
    }
}

fn duration_to_prepared_frames(duration: Duration) -> usize {
    let scaled = duration.as_nanos() * u128::from(PREPARED_SAMPLE_RATE);
    usize::try_from(scaled.div_ceil(1_000_000_000)).unwrap_or(usize::MAX)
}

fn prepared_frames_to_duration(frames: usize) -> Duration {
    Duration::from_secs_f64(frames as f64 / PREPARED_SAMPLE_RATE as f64)
}

fn prepared_to_source_frames(prepared_frames: usize, source_rate: u32) -> usize {
    let scaled = prepared_frames as u128 * u128::from(source_rate);
    usize::try_from(
        (scaled + u128::from(PREPARED_SAMPLE_RATE / 2)) / u128::from(PREPARED_SAMPLE_RATE),
    )
    .unwrap_or(usize::MAX)
    .max(1)
}

struct StreamingLinearResampler {
    source_rate: u32,
    target_rate: u32,
    input_frames: u64,
    output_frames: u64,
    previous: f32,
}

impl StreamingLinearResampler {
    fn new(source_rate: u32, target_rate: u32) -> Self {
        Self {
            source_rate,
            target_rate,
            input_frames: 0,
            output_frames: 0,
            previous: 0.0,
        }
    }

    fn push(&mut self, sample: f32, mut emit: impl FnMut(f32)) {
        let sample = finite_unit(sample);
        if self.input_frames == 0 {
            self.previous = sample;
            self.input_frames = 1;
            emit(sample);
            self.output_frames = 1;
            return;
        }

        let source_index = self.input_frames;
        while u128::from(self.output_frames) * u128::from(self.source_rate)
            <= u128::from(source_index) * u128::from(self.target_rate)
        {
            let source_position =
                self.output_frames as f64 * self.source_rate as f64 / self.target_rate as f64;
            let fraction = (source_position - (source_index - 1) as f64).clamp(0.0, 1.0) as f32;
            emit(finite_unit(
                self.previous + (sample - self.previous) * fraction,
            ));
            self.output_frames += 1;
        }
        self.previous = sample;
        self.input_frames += 1;
    }

    fn finish(&mut self, mut emit: impl FnMut(f32)) {
        if self.input_frames == 0 {
            return;
        }
        let rounded_output_frames = (u128::from(self.input_frames) * u128::from(self.target_rate)
            + u128::from(self.source_rate / 2))
            / u128::from(self.source_rate);
        let rounded_output_frames = u64::try_from(rounded_output_frames.max(1)).unwrap_or(u64::MAX);
        while self.output_frames < rounded_output_frames {
            emit(self.previous);
            self.output_frames += 1;
        }
    }
}

struct LevelTracker {
    sum_squares: f64,
    peak: f32,
    count: usize,
    signal_sum_squares: f64,
    signal_count: usize,
    maximum_rms: f32,
    maximum_peak: f32,
    rms_bits: Arc<AtomicU32>,
    peak_bits: Arc<AtomicU32>,
    observed: Arc<AtomicBool>,
    revision: Arc<AtomicU64>,
}

impl LevelTracker {
    fn new(
        rms_bits: Arc<AtomicU32>,
        peak_bits: Arc<AtomicU32>,
        observed: Arc<AtomicBool>,
        revision: Arc<AtomicU64>,
    ) -> Self {
        Self {
            sum_squares: 0.0,
            peak: 0.0,
            count: 0,
            signal_sum_squares: 0.0,
            signal_count: 0,
            maximum_rms: 0.0,
            maximum_peak: 0.0,
            rms_bits,
            peak_bits,
            observed,
            revision,
        }
    }

    fn push(&mut self, sample: f32) {
        let sample = finite_unit(sample);
        self.sum_squares += f64::from(sample) * f64::from(sample);
        self.peak = self.peak.max(sample.abs());
        self.count += 1;
        self.signal_sum_squares += f64::from(sample) * f64::from(sample);
        self.signal_count += 1;
        if self.signal_count == SIGNAL_DIAGNOSTIC_WINDOW_SAMPLES {
            self.finish_signal_window();
        }
        if self.count == LEVEL_WINDOW_SAMPLES {
            self.publish_meter_window();
        }
    }

    fn finish_windows(&mut self) {
        self.finish_signal_window();
        self.publish_meter_window();
    }

    fn finish_signal_window(&mut self) {
        if self.signal_count == 0 {
            return;
        }
        let rms = (self.signal_sum_squares / self.signal_count as f64).sqrt() as f32;
        self.maximum_rms = self.maximum_rms.max(rms);
        self.signal_sum_squares = 0.0;
        self.signal_count = 0;
    }

    fn publish_meter_window(&mut self) {
        if self.count == 0 {
            return;
        }
        let snapshot = LevelSnapshot {
            rms: (self.sum_squares / self.count as f64).sqrt() as f32,
            peak: self.peak,
        };
        self.maximum_peak = self.maximum_peak.max(snapshot.peak);
        self.rms_bits
            .store(snapshot.rms.to_bits(), Ordering::Relaxed);
        self.peak_bits
            .store(snapshot.peak.to_bits(), Ordering::Relaxed);
        self.revision.fetch_add(1, Ordering::Release);
        self.observed.store(true, Ordering::Release);
        self.sum_squares = 0.0;
        self.peak = 0.0;
        self.count = 0;
    }

    fn maximum(&self) -> LevelSnapshot {
        LevelSnapshot {
            rms: self.maximum_rms,
            peak: self.maximum_peak,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum VadState {
    Waiting,
    Active,
    Paused,
}

struct VadTracker {
    options: VadOptions,
    endpointing_enabled: bool,
    state: VadState,
    window: [f32; WINDOW_SAMPLES],
    window_samples: usize,
    processed_samples: usize,
    candidate_start_frame: Option<usize>,
    candidate_samples: usize,
    speech_start_frame: Option<usize>,
    speech_trigger_frame: Option<usize>,
    last_voice_frame: usize,
    endpoint_frame: Option<usize>,
    detector: Option<Box<dyn SpeechDetector>>,
    last_probability: f32,
}

impl VadTracker {
    fn new(
        options: VadOptions,
        endpointing_enabled: bool,
        detector: Option<Box<dyn SpeechDetector>>,
    ) -> Self {
        Self {
            options,
            endpointing_enabled,
            state: VadState::Waiting,
            window: [0.0; WINDOW_SAMPLES],
            window_samples: 0,
            processed_samples: 0,
            candidate_start_frame: None,
            candidate_samples: 0,
            speech_start_frame: None,
            speech_trigger_frame: None,
            last_voice_frame: 0,
            endpoint_frame: None,
            detector,
            last_probability: 0.0,
        }
    }

    fn push(&mut self, sample: f32) -> Result<(), CaptureError> {
        let sample = finite_unit(sample);
        self.window[self.window_samples] = sample;
        self.window_samples += 1;
        self.processed_samples += 1;
        if self.window_samples == WINDOW_SAMPLES {
            let decision = self
                .detector
                .as_mut()
                .ok_or(CaptureError::InvalidOptions(
                    "Silero VAD session disappeared during capture",
                ))?
                .compute(&self.window)?;
            self.process_window(decision);
            self.window_samples = 0;
        }
        Ok(())
    }

    fn process_window(&mut self, decision: SileroVadDecision) {
        if self.endpoint_frame.is_some() {
            return;
        }
        self.last_probability = decision.probability;
        let frame_end = self.processed_samples;
        let frame_start = frame_end.saturating_sub(WINDOW_SAMPLES);

        match self.state {
            VadState::Waiting => {
                if decision.speech {
                    self.candidate_start_frame.get_or_insert(frame_start);
                    self.candidate_samples += WINDOW_SAMPLES;
                    if self.candidate_samples
                        >= duration_to_prepared_frames(self.options.speech_confirmation)
                    {
                        self.state = VadState::Active;
                        self.speech_start_frame = self.candidate_start_frame;
                        self.speech_trigger_frame = Some(frame_end);
                        self.last_voice_frame = frame_end;
                    }
                } else {
                    self.candidate_start_frame = None;
                    self.candidate_samples = 0;
                }
            }
            VadState::Active => {
                if decision.speech {
                    self.last_voice_frame = frame_end;
                } else if frame_end.saturating_sub(self.last_voice_frame)
                    >= duration_to_prepared_frames(self.options.pause)
                {
                    self.state = VadState::Paused;
                }
            }
            VadState::Paused => {
                if decision.speech {
                    self.state = VadState::Active;
                    self.last_voice_frame = frame_end;
                } else {
                    if self.endpointing_enabled
                        && frame_end.saturating_sub(self.last_voice_frame)
                            >= duration_to_prepared_frames(self.options.endpoint)
                    {
                        self.endpoint_frame = Some(frame_end);
                    }
                }
            }
        }
    }

    fn finish_detector(&mut self) -> Result<(), CaptureError> {
        if let Some(detector) = self.detector.as_mut() {
            detector.finish()?;
        }
        self.detector = None;
        Ok(())
    }

    fn cancel_detector(&mut self) -> Result<(), CaptureError> {
        if let Some(detector) = self.detector.as_mut() {
            detector.cancel()?;
        }
        self.detector = None;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::Mutex;
    use std::sync::mpsc;

    use crate::streaming::RollingPreviewSession;
    use crate::transcription::{ModelId, RequestId, SessionId, StreamUpdate};

    #[derive(Default)]
    struct FakeDetectorState {
        windows: Vec<Vec<f32>>,
        decisions: VecDeque<Result<SileroVadDecision, CaptureError>>,
        finish_calls: usize,
        cancel_calls: usize,
    }

    struct FakeDetector {
        state: Arc<Mutex<FakeDetectorState>>,
    }

    impl SpeechDetector for FakeDetector {
        fn compute(
            &mut self,
            samples: &[f32; WINDOW_SAMPLES],
        ) -> Result<SileroVadDecision, CaptureError> {
            let mut state = self.state.lock().unwrap();
            state.windows.push(samples.to_vec());
            if let Some(decision) = state.decisions.pop_front() {
                return decision;
            }
            let speech = samples.iter().all(|sample| sample.abs() >= 0.1);
            Ok(SileroVadDecision {
                probability: if speech { 0.9 } else { 0.1 },
                speech,
            })
        }

        fn finish(&mut self) -> Result<(), CaptureError> {
            self.state.lock().unwrap().finish_calls += 1;
            Ok(())
        }

        fn cancel(&mut self) -> Result<(), CaptureError> {
            self.state.lock().unwrap().cancel_calls += 1;
            Ok(())
        }
    }

    fn fake_detector() -> (Box<dyn SpeechDetector>, Arc<Mutex<FakeDetectorState>>) {
        let state = Arc::new(Mutex::new(FakeDetectorState::default()));
        (
            Box::new(FakeDetector {
                state: Arc::clone(&state),
            }),
            state,
        )
    }

    fn default_detector() -> Option<Box<dyn SpeechDetector>> {
        Some(fake_detector().0)
    }

    fn level_state() -> (
        Arc<AtomicU32>,
        Arc<AtomicU32>,
        Arc<AtomicBool>,
        Arc<AtomicU64>,
    ) {
        (
            Arc::new(AtomicU32::new(0.0_f32.to_bits())),
            Arc::new(AtomicU32::new(0.0_f32.to_bits())),
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicU64::new(0)),
        )
    }

    fn pipeline(source_rate: u32, channels: u16) -> Pipeline {
        let (rms, peak, observed, revision) = level_state();
        Pipeline::new(
            source_rate,
            channels,
            CaptureOptions::default(),
            default_detector(),
            rms,
            peak,
            observed,
            revision,
        )
        .unwrap()
    }

    fn push_mono_ms(pipeline: &mut Pipeline, milliseconds: usize, value: f32) {
        let frames = pipeline.source_sample_rate as usize * milliseconds / 1_000;
        for _ in 0..frames {
            pipeline.push_interleaved(value).unwrap();
        }
    }

    #[test]
    fn stereo_downmixes_before_resampling() {
        let mut pipeline = pipeline(PREPARED_SAMPLE_RATE, 2);
        for sample in [1.0, -1.0, 0.25, 0.75] {
            pipeline.push_interleaved(sample).unwrap();
        }
        pipeline.vad.speech_start_frame = Some(0);
        pipeline.vad.last_voice_frame = 2;

        let prepared = pipeline
            .finish(CaptureStopReason::Explicit)
            .unwrap()
            .unwrap();
        assert_eq!(prepared.samples[0], 0.0);
        assert!((prepared.samples[1] - 0.141_421_36).abs() < 1e-6);
        assert_eq!(prepared.source_frames, 2);
    }

    #[test]
    fn streaming_resampler_matches_48khz_grid() {
        let mut resampler = StreamingLinearResampler::new(48_000, PREPARED_SAMPLE_RATE);
        let mut output = Vec::new();
        for sample in (0..12).map(|value| value as f32 / 11.0) {
            resampler.push(sample, |sample| output.push(sample));
        }
        resampler.finish(|sample| output.push(sample));

        assert_eq!(output.len(), 4);
        for (actual, expected) in output.iter().zip([0.0, 3.0 / 11.0, 6.0 / 11.0, 9.0 / 11.0]) {
            assert!((actual - expected).abs() < 1e-6);
        }
    }

    #[test]
    fn streaming_resampler_is_deterministic_at_44_1khz() {
        let input = (0..441)
            .map(|value| value as f32 / 440.0)
            .collect::<Vec<_>>();
        let mut one = StreamingLinearResampler::new(44_100, PREPARED_SAMPLE_RATE);
        let mut two = StreamingLinearResampler::new(44_100, PREPARED_SAMPLE_RATE);
        let mut expected = Vec::new();
        let mut actual = Vec::new();
        for sample in &input {
            one.push(*sample, |sample| expected.push(sample));
        }
        for chunk in input.chunks(17) {
            for sample in chunk {
                two.push(*sample, |sample| actual.push(sample));
            }
        }
        one.finish(|sample| expected.push(sample));
        two.finish(|sample| actual.push(sample));

        assert_eq!(actual, expected);
        assert_eq!(actual.len(), 160);
        assert!(actual.iter().all(|sample| sample.is_finite()));
    }

    #[test]
    fn non_finite_samples_are_silenced_and_finite_samples_are_clamped() {
        assert_eq!(finite_unit(f32::NAN), 0.0);
        assert_eq!(finite_unit(f32::INFINITY), 0.0);
        assert_eq!(finite_unit(-2.0), -1.0);
        assert_eq!(finite_unit(2.0), 1.0);
    }

    #[test]
    fn loudness_normalization_is_bounded_and_peak_safe() {
        let mut quiet = vec![0.001; 100];
        normalize_loudness(&mut quiet);
        assert!(quiet.iter().all(|sample| (*sample - 0.008).abs() < 1e-6));

        let mut loud = vec![1.0, -1.0];
        normalize_loudness(&mut loud);
        assert!(
            loud.iter()
                .all(|sample| sample.abs() <= TARGET_PEAK_CEILING)
        );
        assert!((loud[0] - TARGET_RMS).abs() < 1e-6);

        let mut silence = vec![0.0; 100];
        normalize_loudness(&mut silence);
        assert!(silence.iter().all(|sample| *sample == 0.0));
    }

    #[test]
    fn previews_publish_on_exact_cadence_with_bounded_windows_without_mutating_final_audio() {
        let (snapshot_tx, snapshot_rx) = mpsc::channel();
        let mut preview_session = RollingPreviewSession::<()>::new(move |snapshot| {
            snapshot_tx
                .send((
                    snapshot.identity.sequence,
                    snapshot.window_start_frame,
                    snapshot.window_end_frame,
                    snapshot.audio.samples.len(),
                ))
                .unwrap();
            Ok(StreamUpdate::default())
        })
        .unwrap();
        let publisher = preview_session.audio_publisher(
            SessionId(3),
            RequestId(5),
            ModelId::new("preview-model"),
        );
        let disabled_vad = CaptureOptions {
            vad_enabled: false,
            endpointing_enabled: false,
            ..CaptureOptions::default()
        };
        let (preview_rms, preview_peak, preview_observed, preview_revision) = level_state();
        let mut with_preview = Pipeline::new(
            PREPARED_SAMPLE_RATE,
            1,
            disabled_vad,
            None,
            preview_rms,
            preview_peak,
            preview_observed,
            preview_revision,
        )
        .unwrap()
        .with_preview_publisher(Some(publisher));
        let (final_rms, final_peak, final_observed, final_revision) = level_state();
        let mut final_only = Pipeline::new(
            PREPARED_SAMPLE_RATE,
            1,
            disabled_vad,
            None,
            final_rms,
            final_peak,
            final_observed,
            final_revision,
        )
        .unwrap();

        for interval in 1..=13 {
            for frame in 0..PREVIEW_INTERVAL_FRAMES {
                let sample = ((interval * PREVIEW_INTERVAL_FRAMES + frame) % 97) as f32 / 97.0;
                with_preview.push_interleaved(sample).unwrap();
                final_only.push_interleaved(sample).unwrap();
            }
            with_preview.publish_due_previews();
            let (sequence, start, end, samples) = snapshot_rx
                .recv_timeout(Duration::from_secs(1))
                .expect("each exact interval should schedule one snapshot");
            let expected_end = interval * PREVIEW_INTERVAL_FRAMES;
            let expected_start = expected_end.saturating_sub(PREVIEW_WINDOW_FRAMES);
            assert_eq!(sequence, interval as u64);
            assert_eq!(start, expected_start as u64);
            assert_eq!(end, expected_end as u64);
            assert_eq!(samples, expected_end - expected_start);
            assert!(samples <= PREVIEW_WINDOW_FRAMES);
        }

        preview_session.close();
        assert!(preview_session.stop_and_join(Duration::from_secs(1)));
        let preview_final = with_preview
            .finish(CaptureStopReason::Explicit)
            .unwrap()
            .unwrap();
        let final_only = final_only
            .finish(CaptureStopReason::Explicit)
            .unwrap()
            .unwrap();
        assert_eq!(preview_final, final_only);
    }

    #[test]
    fn delayed_preview_publication_skips_obsolete_cadence_windows() {
        let (snapshot_tx, snapshot_rx) = mpsc::channel();
        let mut preview_session = RollingPreviewSession::<()>::new(move |snapshot| {
            snapshot_tx
                .send((
                    snapshot.identity.sequence,
                    snapshot.window_start_frame,
                    snapshot.window_end_frame,
                ))
                .unwrap();
            Ok(StreamUpdate::default())
        })
        .unwrap();
        let publisher = preview_session.audio_publisher(
            SessionId(3),
            RequestId(5),
            ModelId::new("preview-model"),
        );
        let (rms, peak, observed, revision) = level_state();
        let mut pipeline = Pipeline::new(
            PREPARED_SAMPLE_RATE,
            1,
            CaptureOptions {
                vad_enabled: false,
                endpointing_enabled: false,
                ..CaptureOptions::default()
            },
            None,
            rms,
            peak,
            observed,
            revision,
        )
        .unwrap()
        .with_preview_publisher(Some(publisher));

        for _ in 0..(8 * PREVIEW_INTERVAL_FRAMES) {
            pipeline.push_interleaved(0.1).unwrap();
        }
        pipeline.publish_due_previews();

        assert_eq!(
            snapshot_rx.recv_timeout(Duration::from_secs(1)).unwrap(),
            (1, 0, (8 * PREVIEW_INTERVAL_FRAMES) as u64)
        );
        assert!(snapshot_rx.recv_timeout(Duration::from_millis(20)).is_err());
        preview_session.close();
        assert!(preview_session.stop_and_join(Duration::from_secs(1)));
    }

    #[test]
    fn prepared_sample_accumulation_fails_closed_at_its_bound() {
        let mut samples = Vec::new();
        let mut exceeded = false;
        push_bounded(&mut samples, &mut exceeded, 1.0, 2);
        push_bounded(&mut samples, &mut exceeded, 2.0, 2);
        push_bounded(&mut samples, &mut exceeded, 3.0, 2);

        assert_eq!(samples, [1.0, 2.0]);
        assert!(exceeded);
    }

    #[test]
    fn levels_publish_only_on_the_30ms_boundary() {
        let (rms, peak, observed, revision) = level_state();
        let mut pipeline = Pipeline::new(
            PREPARED_SAMPLE_RATE,
            1,
            CaptureOptions::default(),
            default_detector(),
            Arc::clone(&rms),
            Arc::clone(&peak),
            Arc::clone(&observed),
            Arc::clone(&revision),
        )
        .unwrap();
        for _ in 0..LEVEL_WINDOW_SAMPLES - 1 {
            pipeline.push_interleaved(0.5).unwrap();
        }
        assert!(!observed.load(Ordering::Acquire));
        pipeline.push_interleaved(1.0).unwrap();

        assert!(observed.load(Ordering::Acquire));
        assert_eq!(revision.load(Ordering::Acquire), 1);
        assert_eq!(f32::from_bits(peak.load(Ordering::Relaxed)), 1.0);
        let actual_rms = f32::from_bits(rms.load(Ordering::Relaxed));
        let expected_rms = (((LEVEL_WINDOW_SAMPLES - 1) as f64 * 0.25 + 1.0)
            / LEVEL_WINDOW_SAMPLES as f64)
            .sqrt() as f32;
        assert!((actual_rms - expected_rms).abs() < 1e-6);

        for _ in 0..LEVEL_WINDOW_SAMPLES {
            pipeline.push_interleaved(0.25).unwrap();
        }
        assert_eq!(revision.load(Ordering::Acquire), 2);
    }

    #[test]
    fn capture_maximum_levels_include_full_and_partial_signal_and_meter_windows() {
        let mut pipeline = pipeline(PREPARED_SAMPLE_RATE, 1);
        for _ in 0..LEVEL_WINDOW_SAMPLES {
            pipeline.push_interleaved(0.2).unwrap();
        }
        for _ in 0..LEVEL_WINDOW_SAMPLES / 2 {
            pipeline.push_interleaved(0.6).unwrap();
        }

        let _ = pipeline.finish(CaptureStopReason::Explicit).unwrap();

        assert!((pipeline.maximum_levels().rms - 0.6).abs() < 1e-6);
        assert!((pipeline.maximum_levels().peak - 0.6).abs() < 1e-6);
    }

    #[test]
    fn diagnostic_rms_uses_ten_millisecond_frames_while_live_rms_uses_thirty() {
        let (rms_bits, peak_bits, observed, revision) = level_state();
        let mut pipeline = Pipeline::new(
            PREPARED_SAMPLE_RATE,
            1,
            CaptureOptions::default(),
            default_detector(),
            Arc::clone(&rms_bits),
            peak_bits,
            observed,
            revision,
        )
        .unwrap();
        push_mono_ms(&mut pipeline, 20, 0.01);
        push_mono_ms(&mut pipeline, 10, 0.05);

        let live_rms = f32::from_bits(rms_bits.load(Ordering::Relaxed));
        let diagnostic_rms = pipeline.maximum_levels().rms;

        assert!((diagnostic_rms - 0.05).abs() < 1e-6);
        assert!(live_rms < diagnostic_rms);
        assert!(live_rms > 0.01);
    }

    #[test]
    fn one_meter_window_non_speech_burst_reaches_the_low_input_diagnostic_floor() {
        let mut pipeline = pipeline(PREPARED_SAMPLE_RATE, 1);
        push_mono_ms(&mut pipeline, 30, super::super::LOW_INPUT_DIAGNOSTIC_RMS);
        push_mono_ms(&mut pipeline, 100, 0.0);

        assert!(
            pipeline
                .finish(CaptureStopReason::Explicit)
                .unwrap()
                .is_none()
        );
        assert!(pipeline.maximum_levels().rms >= super::super::LOW_INPUT_DIAGNOSTIC_RMS);
    }

    #[test]
    fn silero_receives_only_exact_sequential_post_resample_windows() {
        let (rms, peak, observed, revision) = level_state();
        let (detector, state) = fake_detector();
        let mut pipeline = Pipeline::new(
            PREPARED_SAMPLE_RATE,
            1,
            CaptureOptions::default(),
            Some(detector),
            rms,
            peak,
            observed,
            revision,
        )
        .unwrap();
        let samples = (0..(WINDOW_SAMPLES * 2 + 17))
            .map(|index| index as f32 / (WINDOW_SAMPLES * 4) as f32)
            .collect::<Vec<_>>();
        for sample in &samples {
            pipeline.push_interleaved(*sample).unwrap();
        }
        let _ = pipeline.finish(CaptureStopReason::Explicit).unwrap();

        let state = state.lock().unwrap();
        assert_eq!(
            state.windows.len(),
            2,
            "the final partial window is not padded"
        );
        assert_eq!(state.windows[0], samples[..WINDOW_SAMPLES]);
        assert_eq!(
            state.windows[1],
            samples[WINDOW_SAMPLES..WINDOW_SAMPLES * 2]
        );
        assert_eq!(state.finish_calls, 1);
    }

    #[test]
    fn callback_ring_enqueue_performs_zero_vad_inference_until_worker_drain() {
        let (rms, peak, observed, revision) = level_state();
        let (detector, state) = fake_detector();
        let mut pipeline = Pipeline::new(
            PREPARED_SAMPLE_RATE,
            1,
            CaptureOptions::default(),
            Some(detector),
            rms,
            peak,
            observed,
            revision,
        )
        .unwrap();
        let (mut producer, mut consumer) = super::super::ring_buffer::ring_buffer(WINDOW_SAMPLES);

        for _ in 0..WINDOW_SAMPLES {
            producer.push(0.2).unwrap();
        }
        assert!(state.lock().unwrap().windows.is_empty());

        super::super::drain_ring_bounded(&mut consumer, &mut pipeline, WINDOW_SAMPLES).unwrap();
        assert_eq!(state.lock().unwrap().windows.len(), 1);
    }

    #[test]
    fn equal_silero_decisions_produce_identical_speech_state_at_different_rms_levels() {
        fn run(level: f32) -> (VadState, Option<usize>, Option<usize>, LevelSnapshot) {
            let (rms, peak, observed, revision) = level_state();
            let (detector, state) = fake_detector();
            let decisions = std::iter::repeat_n(true, 5)
                .chain(std::iter::repeat_n(false, 29))
                .map(|speech| {
                    Ok(SileroVadDecision {
                        probability: if speech { 0.9 } else { 0.1 },
                        speech,
                    })
                });
            state.lock().unwrap().decisions.extend(decisions);
            let mut pipeline = Pipeline::new(
                PREPARED_SAMPLE_RATE,
                1,
                CaptureOptions::default(),
                Some(detector),
                rms,
                peak,
                observed,
                revision,
            )
            .unwrap();
            for _ in 0..WINDOW_SAMPLES * 34 {
                pipeline.push_interleaved(level).unwrap();
            }
            (
                pipeline.vad.state,
                pipeline.vad.speech_start_frame,
                pipeline.vad.endpoint_frame,
                pipeline.maximum_levels(),
            )
        }

        let quiet = run(0.001);
        let loud = run(0.9);
        assert_eq!((quiet.0, quiet.1, quiet.2), (loud.0, loud.1, loud.2));
        assert!(quiet.3.rms < loud.3.rms);
    }

    #[test]
    fn silero_compute_failure_stops_the_pipeline_without_an_rms_fallback() {
        let (rms, peak, observed, revision) = level_state();
        let (detector, state) = fake_detector();
        state
            .lock()
            .unwrap()
            .decisions
            .push_back(Err(CaptureError::SpeechDetection(
                "simulated worker crash".to_owned(),
            )));
        let mut pipeline = Pipeline::new(
            PREPARED_SAMPLE_RATE,
            1,
            CaptureOptions::default(),
            Some(detector),
            rms,
            peak,
            observed,
            revision,
        )
        .unwrap();

        for _ in 0..WINDOW_SAMPLES - 1 {
            pipeline.push_interleaved(0.9).unwrap();
        }
        let error = pipeline.push_interleaved(0.9).unwrap_err();
        assert!(matches!(error, CaptureError::SpeechDetection(_)));
        assert!(pipeline.vad.speech_start_frame.is_none());
        assert_eq!(state.lock().unwrap().windows.len(), 1);
    }

    #[test]
    fn cancelling_capture_retires_the_detector_before_more_windows_can_run() {
        let (rms, peak, observed, revision) = level_state();
        let (detector, state) = fake_detector();
        let mut pipeline = Pipeline::new(
            PREPARED_SAMPLE_RATE,
            1,
            CaptureOptions::default(),
            Some(detector),
            rms,
            peak,
            observed,
            revision,
        )
        .unwrap();
        for _ in 0..WINDOW_SAMPLES {
            pipeline.push_interleaved(0.2).unwrap();
        }

        pipeline.cancel_speech_detector().unwrap();
        for _ in 0..WINDOW_SAMPLES - 1 {
            pipeline.push_interleaved(0.2).unwrap();
        }
        assert!(matches!(
            pipeline.push_interleaved(0.2),
            Err(CaptureError::InvalidOptions(_))
        ));

        let state = state.lock().unwrap();
        assert_eq!(state.windows.len(), 1);
        assert_eq!(state.cancel_calls, 1);
        assert_eq!(state.finish_calls, 0);
    }

    #[test]
    fn meter_only_capture_tracks_levels_without_a_detector_or_retained_audio() {
        let (rms_bits, peak_bits, observed, revision) = level_state();
        let mut pipeline = Pipeline::new(
            PREPARED_SAMPLE_RATE,
            1,
            CaptureOptions {
                intent: CaptureIntent::MeterOnly,
                ..CaptureOptions::default()
            },
            None,
            Arc::clone(&rms_bits),
            Arc::clone(&peak_bits),
            Arc::clone(&observed),
            Arc::clone(&revision),
        )
        .unwrap();
        push_mono_ms(&mut pipeline, 150, 0.03);

        assert!(observed.load(Ordering::Acquire));
        assert!(revision.load(Ordering::Acquire) > 0);
        assert!(f32::from_bits(rms_bits.load(Ordering::Relaxed)) > 0.0);
        assert!(f32::from_bits(peak_bits.load(Ordering::Relaxed)) > 0.0);
        assert!(
            pipeline.prepared.is_empty(),
            "meter-only must not retain PCM"
        );

        assert!(
            pipeline
                .finish(CaptureStopReason::Explicit)
                .unwrap()
                .is_none(),
            "meter-only completion has no PreparedAudio"
        );
        assert!(pipeline.prepared.is_empty());
    }

    #[test]
    fn silero_decisions_drive_confirmation_pause_and_endpoint_timers() {
        let (rms, peak, observed, revision) = level_state();
        let mut pipeline = Pipeline::new(
            PREPARED_SAMPLE_RATE,
            1,
            CaptureOptions {
                vad: VadOptions::new(
                    Duration::from_millis(64),
                    Duration::from_millis(64),
                    Duration::from_millis(128),
                    Duration::from_millis(250),
                    Duration::from_millis(200),
                ),
                ..CaptureOptions::default()
            },
            default_detector(),
            rms,
            peak,
            observed,
            revision,
        )
        .unwrap();

        for _ in 0..WINDOW_SAMPLES * 2 {
            pipeline.push_interleaved(0.2).unwrap();
        }
        assert_eq!(pipeline.vad.speech_start_frame, Some(0));
        assert_eq!(pipeline.vad.speech_trigger_frame, Some(WINDOW_SAMPLES * 2));

        for _ in 0..WINDOW_SAMPLES * 2 {
            pipeline.push_interleaved(0.0).unwrap();
        }
        assert_eq!(pipeline.vad.state, VadState::Paused);
        for _ in 0..WINDOW_SAMPLES {
            pipeline.push_interleaved(0.0).unwrap();
        }
        assert!(!pipeline.endpoint_triggered());
        for _ in 0..WINDOW_SAMPLES {
            pipeline.push_interleaved(0.0).unwrap();
        }
        assert!(pipeline.endpoint_triggered());
    }

    #[test]
    fn hold_to_talk_vad_keeps_tracking_speech_after_endpoint_length_silence() {
        let (rms, peak, observed, revision) = level_state();
        let mut pipeline = Pipeline::new(
            PREPARED_SAMPLE_RATE,
            1,
            CaptureOptions {
                endpointing_enabled: false,
                ..CaptureOptions::default()
            },
            default_detector(),
            rms,
            peak,
            observed,
            revision,
        )
        .unwrap();
        push_mono_ms(&mut pipeline, 160, 0.2);
        push_mono_ms(&mut pipeline, 928, 0.0);
        push_mono_ms(&mut pipeline, 224, 0.3);

        assert!(pipeline.vad.speech_start_frame.is_some());
        assert!(pipeline.vad.endpoint_frame.is_none());
        assert!(!pipeline.endpoint_triggered());
        let prepared = pipeline
            .finish(CaptureStopReason::MaximumDuration)
            .unwrap()
            .unwrap();
        assert!(prepared.duration_ms() >= 1_280);
    }

    #[test]
    fn paused_speech_can_resume_before_a_later_endpoint() {
        let mut pipeline = pipeline(PREPARED_SAMPLE_RATE, 1);
        push_mono_ms(&mut pipeline, 160, 0.2);
        push_mono_ms(&mut pipeline, 480, 0.0);
        assert_eq!(pipeline.vad.state, VadState::Paused);

        push_mono_ms(&mut pipeline, 96, 0.2);
        assert_eq!(pipeline.vad.state, VadState::Active);
        assert!(!pipeline.endpoint_triggered());

        push_mono_ms(&mut pipeline, 928, 0.0);
        assert!(pipeline.endpoint_triggered());
    }

    #[test]
    fn repeated_sub_confirmation_bursts_do_not_start_speech() {
        let mut pipeline = pipeline(PREPARED_SAMPLE_RATE, 1);
        for _ in 0..4 {
            push_mono_ms(&mut pipeline, 140, 0.2);
            push_mono_ms(&mut pipeline, 20, 0.0);
        }

        assert_eq!(pipeline.vad.state, VadState::Waiting);
        assert!(pipeline.vad.speech_start_frame.is_none());
        assert!(!pipeline.endpoint_triggered());
    }

    #[test]
    fn sustained_non_speech_decisions_do_not_start_speech() {
        let mut pipeline = pipeline(PREPARED_SAMPLE_RATE, 1);
        push_mono_ms(&mut pipeline, 3_000, 0.01);

        assert_eq!(pipeline.vad.state, VadState::Waiting);
        assert!(pipeline.vad.speech_start_frame.is_none());
    }

    #[test]
    fn endpoint_completion_keeps_pre_roll_and_post_roll() {
        let mut pipeline = pipeline(PREPARED_SAMPLE_RATE, 1);
        push_mono_ms(&mut pipeline, 320, 0.0);
        push_mono_ms(&mut pipeline, 160, 0.2);
        push_mono_ms(&mut pipeline, 928, 0.0);
        assert!(pipeline.endpoint_triggered());

        let expected_start = pipeline
            .vad
            .speech_start_frame
            .unwrap()
            .saturating_sub(duration_to_prepared_frames(pipeline.vad.options.pre_roll));
        let expected_end = pipeline
            .vad
            .last_voice_frame
            .saturating_add(duration_to_prepared_frames(pipeline.vad.options.post_roll));

        let prepared = pipeline
            .finish(CaptureStopReason::Endpoint)
            .unwrap()
            .unwrap();
        assert_eq!(prepared.samples.len(), expected_end - expected_start);
        assert_eq!(prepared.samples[0], 0.0);
        assert!(prepared.samples[duration_to_prepared_frames(pipeline.vad.options.pre_roll)] > 0.1);
        assert_eq!(*prepared.samples.last().unwrap(), 0.0);
        assert_eq!(prepared.source_duration_ms(), prepared.duration_ms());
    }

    #[test]
    fn explicit_completion_keeps_audio_captured_during_post_roll() {
        let mut pipeline = pipeline(PREPARED_SAMPLE_RATE, 1);
        push_mono_ms(&mut pipeline, 320, 0.0);
        push_mono_ms(&mut pipeline, 160, 0.2);
        push_mono_ms(&mut pipeline, 224, 0.0);
        let expected_start = pipeline
            .vad
            .speech_start_frame
            .unwrap()
            .saturating_sub(duration_to_prepared_frames(pipeline.vad.options.pre_roll));
        let expected_len = pipeline.prepared.len() - expected_start;

        let prepared = pipeline
            .finish(CaptureStopReason::Explicit)
            .unwrap()
            .unwrap();
        assert_eq!(prepared.samples.len(), expected_len);
    }

    #[test]
    fn no_speech_returns_no_prepared_audio() {
        let mut pipeline = pipeline(PREPARED_SAMPLE_RATE, 1);
        push_mono_ms(&mut pipeline, 2_000, 0.001);

        assert!(
            pipeline
                .finish(CaptureStopReason::MaximumDuration)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn disabled_vad_returns_silence_without_endpointing_or_trimming() {
        let (rms, peak, observed, revision) = level_state();
        let mut pipeline = Pipeline::new(
            PREPARED_SAMPLE_RATE,
            1,
            CaptureOptions {
                vad_enabled: false,
                endpointing_enabled: false,
                ..CaptureOptions::default()
            },
            None,
            rms,
            peak,
            observed,
            revision,
        )
        .unwrap();
        push_mono_ms(&mut pipeline, 1_000, 0.0);
        assert!(!pipeline.endpoint_triggered());

        let prepared = pipeline
            .finish(CaptureStopReason::MaximumDuration)
            .unwrap()
            .unwrap();
        assert_eq!(prepared.samples.len(), PREPARED_SAMPLE_RATE as usize);
    }
}
