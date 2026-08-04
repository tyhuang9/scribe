//! Runtime-neutral WAV decoding and deterministic speech-audio preparation.

use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;

use anyhow::{Context, Result, anyhow, bail, ensure};

/// The canonical sample rate consumed by Scribe speech engines.
pub const PREPARED_SAMPLE_RATE: u32 = 16_000;

/// Decoded speech audio in Scribe's runtime-neutral input format.
///
/// `samples` always contains finite mono `f32` values in `[-1.0, 1.0]` at
/// [`PREPARED_SAMPLE_RATE`]. Source metadata describes the decoded WAV before
/// downmixing and resampling; it is retained for diagnostics and duration
/// calculations without exposing a model or runtime type.
#[derive(Clone, Debug, PartialEq)]
pub struct PreparedAudio {
    pub samples: Vec<f32>,
    pub sample_rate: u32,
    pub source_sample_rate: u32,
    pub source_channels: u16,
    pub source_frames: usize,
}

impl PreparedAudio {
    /// Builds canonical in-memory audio produced by the native capture worker.
    pub(crate) fn from_captured_mono(
        mut samples: Vec<f32>,
        source_sample_rate: u32,
        source_channels: u16,
        source_frames: usize,
    ) -> Result<Self> {
        ensure!(
            source_sample_rate > 0,
            "source sample rate must be non-zero"
        );
        ensure!(source_channels > 0, "source channel count must be non-zero");
        ensure!(
            source_frames > 0,
            "captured audio contains no source frames"
        );
        ensure!(
            !samples.is_empty(),
            "captured audio contains no prepared samples"
        );
        for sample in &mut samples {
            ensure!(
                sample.is_finite(),
                "captured audio contains a non-finite sample"
            );
            *sample = sample.clamp(-1.0, 1.0);
        }
        Ok(Self {
            samples,
            sample_rate: PREPARED_SAMPLE_RATE,
            source_sample_rate,
            source_channels,
            source_frames,
        })
    }

    /// Decodes and prepares a WAV file from disk.
    #[allow(
        dead_code,
        reason = "the separate Phase 2 integration slice will call this filesystem boundary"
    )]
    pub fn from_wav_path(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let file = File::open(path)
            .map_err(|err| anyhow!("failed to open WAV input {}: {err}", path.display()))?;
        Self::from_wav_reader(BufReader::new(file))
            .map_err(|err| anyhow!("failed to prepare WAV input {}: {err:#}", path.display()))
    }

    /// Decodes and prepares WAV bytes from any reader.
    ///
    /// This entry point keeps decoding testable without filesystem fixtures.
    /// Signed integer PCM at 8, 16, 24, or 32 bits and IEEE float32 are
    /// accepted. Finite float samples outside the conventional range are
    /// clipped; non-finite values are rejected because silently substituting
    /// audio would hide corrupt input.
    pub fn from_wav_reader<R: Read>(reader: R) -> Result<Self> {
        let mut wav =
            hound::WavReader::new(reader).map_err(|err| anyhow!("invalid WAV input: {err}"))?;
        let spec = wav.spec();

        ensure!(spec.channels > 0, "WAV input has zero channels");
        ensure!(spec.sample_rate > 0, "WAV input has zero sample rate");

        let declared_samples = wav.len() as usize;
        ensure!(declared_samples > 0, "WAV input contains no audio samples");

        let mut interleaved = Vec::new();
        interleaved
            .try_reserve_exact(declared_samples)
            .map_err(|err| anyhow!("WAV input is too large to decode: {err}"))?;

        match spec.sample_format {
            hound::SampleFormat::Int => {
                ensure!(
                    matches!(spec.bits_per_sample, 8 | 16 | 24 | 32),
                    "unsupported integer PCM bit depth: {}",
                    spec.bits_per_sample
                );
                let scale = (1_u64 << (spec.bits_per_sample - 1)) as f64;
                for sample in wav.samples::<i32>() {
                    let sample = sample
                        .map_err(|err| anyhow!("truncated or malformed integer PCM data: {err}"))?;
                    let normalized = ((sample as f64) / scale).clamp(-1.0, 1.0) as f32;
                    interleaved.push(normalized);
                }
            }
            hound::SampleFormat::Float => {
                ensure!(
                    spec.bits_per_sample == 32,
                    "unsupported floating-point PCM bit depth: {}",
                    spec.bits_per_sample
                );
                for sample in wav.samples::<f32>() {
                    let sample = sample
                        .map_err(|err| anyhow!("truncated or malformed float32 PCM data: {err}"))?;
                    ensure!(
                        sample.is_finite(),
                        "WAV input contains a non-finite float sample"
                    );
                    interleaved.push(sample.clamp(-1.0, 1.0));
                }
            }
        }

        ensure!(
            interleaved.len() == declared_samples,
            "WAV sample count does not match its header"
        );
        let channels = spec.channels as usize;
        ensure!(
            interleaved.len().is_multiple_of(channels),
            "WAV input does not contain whole channel frames"
        );

        let source_frames = interleaved.len() / channels;
        ensure!(
            source_frames > 0,
            "WAV input contains no complete audio frames"
        );

        let mono = downmix_to_mono(&interleaved, channels);
        let samples = resample_linear(&mono, spec.sample_rate, PREPARED_SAMPLE_RATE)?;

        Ok(Self {
            samples,
            sample_rate: PREPARED_SAMPLE_RATE,
            source_sample_rate: spec.sample_rate,
            source_channels: spec.channels,
            source_frames,
        })
    }

    /// Duration represented by the source frame count, rounded down to
    /// milliseconds.
    #[allow(
        dead_code,
        reason = "duration diagnostics are consumed by a later Phase 2 integration slice"
    )]
    pub fn source_duration_ms(&self) -> u128 {
        self.source_frames as u128 * 1_000 / self.source_sample_rate as u128
    }

    /// Duration represented by the prepared samples, rounded down to
    /// milliseconds.
    #[allow(
        dead_code,
        reason = "duration diagnostics are consumed by a later Phase 2 integration slice"
    )]
    pub fn duration_ms(&self) -> u128 {
        self.samples.len() as u128 * 1_000 / self.sample_rate as u128
    }
}

fn downmix_to_mono(interleaved: &[f32], channels: usize) -> Vec<f32> {
    interleaved
        .chunks_exact(channels)
        .map(|frame| {
            let sum = frame.iter().map(|sample| *sample as f64).sum::<f64>();
            (sum / channels as f64).clamp(-1.0, 1.0) as f32
        })
        .collect()
}

/// Resamples a mono signal on a fixed output time grid using linear
/// interpolation.
///
/// The output length is the source frame-count duration scaled to the target
/// rate and rounded to the nearest frame, with one frame retained for every
/// non-empty input. Output frame `i` samples source time
/// `i * source_rate / target_rate`; when upsampling places a final output time
/// past the last source sample center, the last source value is held. Every
/// output is therefore a convex interpolation (or endpoint copy), so this
/// stage cannot amplify beyond the input range. This bounded Phase 2 resampler
/// deliberately performs no loudness normalization or anti-alias filtering.
fn resample_linear(input: &[f32], source_rate: u32, target_rate: u32) -> Result<Vec<f32>> {
    ensure!(!input.is_empty(), "cannot resample empty audio");
    ensure!(source_rate > 0, "source sample rate must be non-zero");
    ensure!(target_rate > 0, "target sample rate must be non-zero");

    if source_rate == target_rate {
        return Ok(input.to_vec());
    }

    let scaled_len = input.len() as u128 * target_rate as u128;
    let rounded_len = (scaled_len + u128::from(source_rate / 2)) / u128::from(source_rate);
    let output_len = usize::try_from(rounded_len.max(1))
        .context("resampled audio length exceeds this platform's address space")?;
    let mut output = Vec::new();
    output
        .try_reserve_exact(output_len)
        .map_err(|err| anyhow!("resampled audio is too large to allocate: {err}"))?;

    let rate_ratio = source_rate as f64 / target_rate as f64;
    let last_index = input.len() - 1;
    for output_index in 0..output_len {
        let source_position = output_index as f64 * rate_ratio;
        let lower = (source_position.floor() as usize).min(last_index);
        let upper = lower.saturating_add(1).min(last_index);
        let fraction = if lower == last_index {
            0.0
        } else {
            (source_position - lower as f64) as f32
        };
        let value = input[lower] + (input[upper] - input[lower]) * fraction;
        if !value.is_finite() {
            bail!("resampling produced a non-finite sample");
        }
        output.push(value.clamp(-1.0, 1.0));
    }

    Ok(output)
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    fn wav_bytes_i32(spec: hound::WavSpec, samples: &[i32]) -> Vec<u8> {
        let mut cursor = Cursor::new(Vec::new());
        {
            let mut writer = hound::WavWriter::new(&mut cursor, spec).unwrap();
            for sample in samples {
                writer.write_sample(*sample).unwrap();
            }
            writer.finalize().unwrap();
        }
        cursor.into_inner()
    }

    fn wav_bytes_f32(channels: u16, sample_rate: u32, samples: &[f32]) -> Vec<u8> {
        let mut cursor = Cursor::new(Vec::new());
        {
            let spec = hound::WavSpec {
                channels,
                sample_rate,
                bits_per_sample: 32,
                sample_format: hound::SampleFormat::Float,
            };
            let mut writer = hound::WavWriter::new(&mut cursor, spec).unwrap();
            for sample in samples {
                writer.write_sample(*sample).unwrap();
            }
            writer.finalize().unwrap();
        }
        cursor.into_inner()
    }

    fn prepare(bytes: Vec<u8>) -> Result<PreparedAudio> {
        PreparedAudio::from_wav_reader(Cursor::new(bytes))
    }

    fn assert_samples_close(actual: &[f32], expected: &[f32]) {
        assert_eq!(actual.len(), expected.len());
        for (index, (actual, expected)) in actual.iter().zip(expected).enumerate() {
            assert!(
                (actual - expected).abs() <= 1e-6,
                "sample {index}: expected {expected}, got {actual}"
            );
        }
    }

    #[test]
    fn captured_audio_constructor_clamps_finite_samples_and_rejects_non_finite_samples() {
        let prepared =
            PreparedAudio::from_captured_mono(vec![-2.0, -0.25, 2.0], 48_000, 2, 9).unwrap();
        assert_eq!(prepared.samples, [-1.0, -0.25, 1.0]);

        assert!(
            PreparedAudio::from_captured_mono(vec![f32::NAN], 48_000, 1, 3)
                .unwrap_err()
                .to_string()
                .contains("non-finite")
        );
    }

    #[test]
    fn mono_16khz_float_audio_passes_through_without_resampling() {
        let source = [-1.0, -0.25, 0.0, 0.5, 1.0];

        let prepared = prepare(wav_bytes_f32(1, PREPARED_SAMPLE_RATE, &source)).unwrap();

        assert_eq!(prepared.samples, source);
        assert_eq!(prepared.sample_rate, PREPARED_SAMPLE_RATE);
        assert_eq!(prepared.source_sample_rate, PREPARED_SAMPLE_RATE);
        assert_eq!(prepared.source_channels, 1);
        assert_eq!(prepared.source_frames, source.len());
    }

    #[test]
    fn interleaved_stereo_is_downmixed_by_arithmetic_mean() {
        let source = [1.0, -1.0, 0.25, 0.75, -0.5, -0.25];

        let prepared = prepare(wav_bytes_f32(2, PREPARED_SAMPLE_RATE, &source)).unwrap();

        assert_samples_close(&prepared.samples, &[0.0, 0.5, -0.375]);
        assert_eq!(prepared.source_channels, 2);
        assert_eq!(prepared.source_frames, 3);
    }

    #[test]
    fn eight_khz_audio_doubles_length_on_the_16khz_time_grid() {
        let source = [0.0, 1.0, 0.0, -1.0];

        let prepared = prepare(wav_bytes_f32(1, 8_000, &source)).unwrap();

        assert_samples_close(
            &prepared.samples,
            &[0.0, 0.5, 1.0, 0.5, 0.0, -0.5, -1.0, -1.0],
        );
        assert_eq!(prepared.samples.len(), 8);
        assert_eq!(prepared.source_frames, 4);
    }

    #[test]
    fn forty_eight_khz_audio_uses_every_third_source_time() {
        let source = (0..12).map(|value| value as f32 / 11.0).collect::<Vec<_>>();

        let prepared = prepare(wav_bytes_f32(1, 48_000, &source)).unwrap();

        assert_samples_close(
            &prepared.samples,
            &[0.0, 3.0 / 11.0, 6.0 / 11.0, 9.0 / 11.0],
        );
        assert_eq!(prepared.samples.len(), 4);
    }

    #[test]
    fn integer_pcm_bit_depths_map_to_the_finite_unit_range() {
        for (bits, minimum, maximum) in [
            (8, -128, 127),
            (16, -32_768, 32_767),
            (24, -8_388_608, 8_388_607),
            (32, i32::MIN, i32::MAX),
        ] {
            let spec = hound::WavSpec {
                channels: 1,
                sample_rate: PREPARED_SAMPLE_RATE,
                bits_per_sample: bits,
                sample_format: hound::SampleFormat::Int,
            };

            let prepared = prepare(wav_bytes_i32(spec, &[minimum, 0, maximum])).unwrap();
            let scale = (1_u64 << (bits - 1)) as f64;
            let expected_maximum = (maximum as f64 / scale) as f32;

            assert_samples_close(&prepared.samples, &[-1.0, 0.0, expected_maximum]);
            assert!(prepared.samples.iter().all(|sample| sample.is_finite()));
            assert!(
                prepared
                    .samples
                    .iter()
                    .all(|sample| (-1.0..=1.0).contains(sample))
            );
        }
    }

    #[test]
    fn finite_float32_samples_are_clipped_without_normalization() {
        let prepared = prepare(wav_bytes_f32(
            1,
            PREPARED_SAMPLE_RATE,
            &[-2.0, -0.25, 0.5, 2.0],
        ))
        .unwrap();

        assert_samples_close(&prepared.samples, &[-1.0, -0.25, 0.5, 1.0]);
    }

    #[test]
    fn empty_wav_is_rejected() {
        let error = prepare(wav_bytes_f32(1, PREPARED_SAMPLE_RATE, &[])).unwrap_err();

        assert!(error.to_string().contains("no audio samples"));
    }

    #[test]
    fn malformed_and_truncated_wavs_are_rejected() {
        assert!(prepare(b"not a wav".to_vec()).is_err());

        let mut truncated = wav_bytes_i32(
            hound::WavSpec {
                channels: 1,
                sample_rate: PREPARED_SAMPLE_RATE,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            },
            &[1, 2, 3, 4],
        );
        truncated.pop();

        let error = prepare(truncated).unwrap_err();
        assert!(error.to_string().contains("truncated or malformed"));
    }

    #[test]
    fn zero_channels_and_zero_sample_rate_are_rejected() {
        let valid = wav_bytes_i32(
            hound::WavSpec {
                channels: 1,
                sample_rate: PREPARED_SAMPLE_RATE,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            },
            &[1],
        );

        let mut zero_channels = valid.clone();
        zero_channels[22..24].copy_from_slice(&0_u16.to_le_bytes());
        assert!(
            prepare(zero_channels)
                .unwrap_err()
                .to_string()
                .contains("zero channels")
        );

        let mut zero_rate = valid;
        zero_rate[24..28].copy_from_slice(&0_u32.to_le_bytes());
        zero_rate[28..32].copy_from_slice(&0_u32.to_le_bytes());
        assert!(
            prepare(zero_rate)
                .unwrap_err()
                .to_string()
                .contains("zero sample rate")
        );
    }

    #[test]
    fn incomplete_interleaved_frame_is_rejected() {
        let mut cursor = Cursor::new(Vec::new());
        {
            let spec = hound::WavSpec {
                channels: 2,
                sample_rate: PREPARED_SAMPLE_RATE,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            };
            let mut writer = hound::WavWriter::new(&mut cursor, spec).unwrap();
            for sample in [1_i16, 2, 3] {
                writer.write_sample(sample).unwrap();
            }
            assert!(writer.finalize().is_err());
        }

        assert!(prepare(cursor.into_inner()).is_err());
    }

    #[test]
    fn non_finite_float_samples_are_rejected() {
        for sample in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            let error = prepare(wav_bytes_f32(1, PREPARED_SAMPLE_RATE, &[sample])).unwrap_err();
            assert!(error.to_string().contains("non-finite"));
        }
    }

    #[test]
    fn duration_metadata_tracks_source_and_prepared_frame_counts() {
        let samples = vec![0.0; 9_600];

        let prepared = prepare(wav_bytes_f32(2, 48_000, &samples)).unwrap();

        assert_eq!(prepared.source_frames, 4_800);
        assert_eq!(prepared.samples.len(), 1_600);
        assert_eq!(prepared.source_duration_ms(), 100);
        assert_eq!(prepared.duration_ms(), 100);
    }
}
