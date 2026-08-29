//! Narrow safe wrapper around sherpa-onnx's stateful Silero probability model.

use std::ffi::{CStr, CString, c_char, c_void};
use std::path::Path;
use std::ptr;
use std::rc::Rc;

use anyhow::{Result, anyhow, bail};

pub(crate) const WINDOW_SAMPLES: usize = 512;
pub(crate) const MIN_THRESHOLD: f32 = 0.2;
pub(crate) const MAX_THRESHOLD: f32 = 0.8;
const ERROR_CAPACITY: usize = 512;

unsafe extern "C" {
    fn scribe_silero_vad_create(
        model_path: *const c_char,
        num_threads: i32,
        out_handle: *mut *mut c_void,
        error: *mut c_char,
        error_capacity: usize,
    ) -> i32;
    fn scribe_silero_vad_compute_exact_512(
        handle: *mut c_void,
        samples: *const f32,
        sample_count: usize,
        out_probability: *mut f32,
        error: *mut c_char,
        error_capacity: usize,
    ) -> i32;
    fn scribe_silero_vad_reset(
        handle: *mut c_void,
        error: *mut c_char,
        error_capacity: usize,
    ) -> i32;
    fn scribe_silero_vad_destroy(handle: *mut c_void);
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct VadThreshold(f32);

impl VadThreshold {
    pub(crate) fn new(value: f32) -> Result<Self> {
        if !value.is_finite() || !(MIN_THRESHOLD..=MAX_THRESHOLD).contains(&value) {
            bail!("Silero VAD threshold must be finite and within [0.2, 0.8]");
        }
        Ok(Self(value))
    }

    pub(crate) fn detects(self, probability: f32) -> Result<bool> {
        validate_probability(probability)?;
        Ok(probability > self.0)
    }

    pub(crate) fn value(self) -> f32 {
        self.0
    }
}

pub(crate) struct SileroVadModel {
    handle: *mut c_void,
    _not_send_or_sync: std::marker::PhantomData<Rc<()>>,
}

impl SileroVadModel {
    pub(crate) fn load_bundled(num_threads: i32) -> Result<Self> {
        let asset = crate::support_assets::materialize_bundled_support_assets()?;
        let model = Self::load_verified(asset.path(), num_threads);
        drop(asset);
        model
    }

    fn load_verified(model_path: &Path, num_threads: i32) -> Result<Self> {
        if !(1..=64).contains(&num_threads) {
            bail!("Silero VAD thread count must be within [1, 64]");
        }
        // The C++ VAD shim uses the same reviewed static Sherpa archive as the
        // Rust binding. Referencing its neutral version API keeps that native
        // archive linked without compiling any ASR recognizer/server code into
        // the desktop binary.
        let _ = sherpa_onnx::version();
        let model_path = model_path
            .to_str()
            .ok_or_else(|| anyhow!("Silero VAD asset path is not valid Unicode"))?;
        let model_path = CString::new(model_path)
            .map_err(|_| anyhow!("Silero VAD asset path contains a null byte"))?;
        let mut error = [0 as c_char; ERROR_CAPACITY];
        let mut handle = ptr::null_mut();
        let status = unsafe {
            scribe_silero_vad_create(
                model_path.as_ptr(),
                num_threads,
                &mut handle,
                error.as_mut_ptr(),
                error.len(),
            )
        };
        ffi_result(status, &error, "could not create Silero VAD")?;
        if handle.is_null() {
            bail!("native Silero VAD creation returned a null handle");
        }
        Ok(Self {
            handle,
            _not_send_or_sync: std::marker::PhantomData,
        })
    }

    pub(crate) fn compute(&mut self, samples: &[f32]) -> Result<f32> {
        validate_samples(samples)?;
        let mut error = [0 as c_char; ERROR_CAPACITY];
        let mut probability = 0.0;
        let status = unsafe {
            scribe_silero_vad_compute_exact_512(
                self.handle,
                samples.as_ptr(),
                samples.len(),
                &mut probability,
                error.as_mut_ptr(),
                error.len(),
            )
        };
        ffi_result(status, &error, "could not compute Silero VAD probability")?;
        validate_probability(probability)?;
        Ok(probability)
    }

    pub(crate) fn reset(&mut self) -> Result<()> {
        let mut error = [0 as c_char; ERROR_CAPACITY];
        let status =
            unsafe { scribe_silero_vad_reset(self.handle, error.as_mut_ptr(), error.len()) };
        ffi_result(status, &error, "could not reset Silero VAD")
    }
}

impl Drop for SileroVadModel {
    fn drop(&mut self) {
        unsafe { scribe_silero_vad_destroy(self.handle) };
        self.handle = ptr::null_mut();
    }
}

fn validate_samples(samples: &[f32]) -> Result<()> {
    if samples.len() != WINDOW_SAMPLES {
        bail!("Silero VAD input must contain exactly {WINDOW_SAMPLES} samples");
    }
    if samples
        .iter()
        .any(|sample| !sample.is_finite() || !(-1.0..=1.0).contains(sample))
    {
        bail!("Silero VAD samples must be finite and within [-1, 1]");
    }
    Ok(())
}

fn validate_probability(probability: f32) -> Result<()> {
    if !probability.is_finite() || !(0.0..=1.0).contains(&probability) {
        bail!("Silero VAD probability must be finite and within [0, 1]");
    }
    Ok(())
}

fn ffi_result(status: i32, error: &[c_char], context: &str) -> Result<()> {
    if status == 0 {
        return Ok(());
    }
    let message = unsafe { CStr::from_ptr(error.as_ptr()) }
        .to_string_lossy()
        .into_owned();
    if message.is_empty() {
        bail!("{context}: native status {status}");
    }
    bail!("{context}: {message} (native status {status})")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn real_asset_links_loads_computes_exact_windows_and_resets_state() {
        let mut model = SileroVadModel::load_bundled(1).unwrap();
        let samples = (0..WINDOW_SAMPLES)
            .map(|index| ((index as f32 * 0.071).sin() * 0.25).clamp(-1.0, 1.0))
            .collect::<Vec<_>>();
        let first = model.compute(&samples).unwrap();
        let second = model.compute(&samples).unwrap();
        model.reset().unwrap();
        let after_reset = model.compute(&samples).unwrap();
        println!(
            "first_probability={first:.9} second_probability={second:.9} after_reset_probability={after_reset:.9}"
        );
        assert_eq!(after_reset, first);
        assert!((0.0..=1.0).contains(&first));
    }

    #[test]
    fn rust_boundary_rejects_invalid_windows_and_thresholds() {
        let mut model = SileroVadModel::load_bundled(1).unwrap();
        assert!(model.compute(&[0.0; WINDOW_SAMPLES - 1]).is_err());
        let mut invalid = [0.0; WINDOW_SAMPLES];
        invalid[17] = f32::NAN;
        assert!(model.compute(&invalid).is_err());
        assert!(VadThreshold::new(0.19).is_err());
        assert!(VadThreshold::new(0.81).is_err());
        let threshold = VadThreshold::new(0.5).unwrap();
        assert!(!threshold.detects(0.5).unwrap());
        assert!(threshold.detects(0.500_001).unwrap());
    }

    #[test]
    fn native_boundary_independently_rejects_invalid_pcm() {
        let model = SileroVadModel::load_bundled(1).unwrap();
        let samples = [0.0; WINDOW_SAMPLES];
        let mut probability = 0.0;
        let mut error = [0 as c_char; ERROR_CAPACITY];
        let short_status = unsafe {
            scribe_silero_vad_compute_exact_512(
                model.handle,
                samples.as_ptr(),
                WINDOW_SAMPLES - 1,
                &mut probability,
                error.as_mut_ptr(),
                error.len(),
            )
        };
        assert_ne!(short_status, 0);

        let mut invalid = samples;
        invalid[3] = 2.0;
        error.fill(0);
        let range_status = unsafe {
            scribe_silero_vad_compute_exact_512(
                model.handle,
                invalid.as_ptr(),
                invalid.len(),
                &mut probability,
                error.as_mut_ptr(),
                error.len(),
            )
        };
        assert_ne!(range_status, 0);
    }
}
