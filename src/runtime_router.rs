//! Phase 2 native whisper.cpp runtime boundary.
//!
//! The router owns the only runtime-kind selection. The C shim owns every
//! upstream ABI struct passed by value; Rust communicates only with an opaque
//! handle and primitive callback values.

use std::ffi::{CStr, CString, c_char, c_void};
use std::fs::File;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::ptr::NonNull;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::transcription::{
    AccelerationPreference, ComputeDevice, ModelId, ResolvedAcceleration, RuntimeCapabilities,
};

const WARM_MODEL_TTL: Duration = Duration::from_secs(5 * 60);
const WHISPER_DLL_SHA256: &str = "b31690c12461517fe9774e61318ab63a69972b948151feed98b913be35f708b6";
const WHISPER_CLI_SHA256: &str = "58245314fb73b30fbd0cf0542c5c172e23f02b6eb7cad7b51e792439cf5e1755";
#[cfg(test)]
const WHISPER_ARCHIVE_SHA256: &str =
    "7d8be46ecd31828e1eb7a2ecdd0d6b314feafd82163038ab6092594b0a063539";
const COMMON_GGML_DEPENDENCIES: [(&str, &str); 11] = [
    (
        "bin/ggml.dll",
        "db753141098018ab482796052a61e727ee0106cbc280f28397f6a111b5e667d7",
    ),
    (
        "bin/ggml-base.dll",
        "8be6f3e06388b3a9aac75d29bec86363e2e2f5b0cee86ce6438866bcac0bcf86",
    ),
    (
        "bin/ggml-cpu-alderlake.dll",
        "323408503da53ccc67248b26d711f16d73d2d6239f7703a00a6a18b60ed5b8b8",
    ),
    (
        "bin/ggml-cpu-cannonlake.dll",
        "0f659d98b823bb871c7845787bba7485facd220099cf58aa773652b9b842ab2e",
    ),
    (
        "bin/ggml-cpu-cascadelake.dll",
        "8116b0e516134139de29400c536ecf06fe708ce1a078a96d30b562b30d524fbe",
    ),
    (
        "bin/ggml-cpu-haswell.dll",
        "e5925923a47672392f9e9c8c92e4b9b65ea473948bf4f568a0300a3a42485135",
    ),
    (
        "bin/ggml-cpu-icelake.dll",
        "b726d528bee0c811c6b2ad8775357379d651cabb487bbf800331697fe73da187",
    ),
    (
        "bin/ggml-cpu-sandybridge.dll",
        "1c49c64817233b2447ca305b41c66afa4bed31b058bc190a98af2a30cc703542",
    ),
    (
        "bin/ggml-cpu-skylakex.dll",
        "06082dc62a09a82fbba4aab49b2c049b96db84c5fc561a446a8ddbfb9b20bf86",
    ),
    (
        "bin/ggml-cpu-sse42.dll",
        "9a8f55ff1dfad231aa6250ac52c330c5bfa5c4c37691c8b591a68b52090ce40c",
    ),
    (
        "bin/ggml-cpu-x64.dll",
        "45ff644d301b8a1fffc7c5e3864205047360eb197814c7311f366d106bb5b19f",
    ),
];

/// A model selected for the sole native whisper.cpp runtime.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RuntimeModel {
    pub id: ModelId,
    pub path: PathBuf,
    pub package_root: PathBuf,
}

/// Borrowed, already-prepared mono 16 kHz PCM data.
#[derive(Clone, Copy, Debug)]
pub(crate) struct RuntimeAudio<'a> {
    pub samples: &'a [f32],
    pub sample_rate_hz: u32,
    pub channels: u16,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RuntimeSegment {
    pub text: String,
    pub start_ms: u64,
    pub end_ms: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RuntimeTranscript {
    pub text: String,
    pub segments: Vec<RuntimeSegment>,
    pub detected_language: Option<String>,
    pub duration_ms: Option<u128>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct NativeRuntimeDiagnostics {
    pub resolved_acceleration: ResolvedAcceleration,
    pub native_library_path: PathBuf,
    pub warm_reused: bool,
    pub model_load_duration_ms: u128,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RuntimeExecution {
    pub transcript: RuntimeTranscript,
    pub diagnostics: NativeRuntimeDiagnostics,
    pub processing_duration_ms: u128,
}

#[derive(Debug, Error)]
pub(crate) enum NativeBootstrapFailure {
    #[error("native whisper.cpp is only packaged for Windows x64 in Phase 2")]
    UnsupportedPlatform,
    #[error(
        "GPU was requested, but the verified whisper.cpp package contains CPU backend DLLs only"
    )]
    UnsupportedGpu,
    #[error("native whisper.cpp package is unavailable: {message}")]
    PackageUnavailable { message: String },
    #[error("verified native package file is missing: {path}")]
    PackageFileMissing { path: PathBuf },
    #[error("verified native package hash mismatch for {path}: expected {expected}, got {actual}")]
    PackageHashMismatch {
        path: PathBuf,
        expected: &'static str,
        actual: String,
    },
    #[error("native whisper.cpp library could not load: {message}")]
    NativeLibrary {
        message: String,
        compatibility_cli_path: PathBuf,
    },
    #[error("native whisper.cpp could not load model {path}: {message}")]
    ModelLoad { path: PathBuf, message: String },
    #[error("failed to hash native package file {path}: {source}")]
    PackageIo { path: PathBuf, source: io::Error },
}

impl NativeBootstrapFailure {
    /// Native unavailability may only reach the compatibility CLI after the
    /// CLI and its shared GGML dependencies pass their own hash checks.
    /// Integrity, model, audio, and inference failures are never eligible.
    pub(crate) fn cli_fallback_eligible(&self) -> bool {
        match self {
            Self::NativeLibrary {
                compatibility_cli_path,
                ..
            } => verify_compatibility_cli(compatibility_cli_path).is_ok(),
            Self::PackageFileMissing { path } => native_package_root(path)
                .map(|root| NativePackage::compatibility_cli_path_for_root(&root))
                .is_some_and(|cli| verify_compatibility_cli(&cli).is_ok()),
            _ => false,
        }
    }
}

#[derive(Debug, Error)]
pub(crate) enum RuntimeError {
    #[error(transparent)]
    Bootstrap(#[from] NativeBootstrapFailure),
    #[error(
        "runtime audio must be mono 16 kHz; received {channels} channel(s) at {sample_rate_hz} Hz"
    )]
    InvalidAudio { sample_rate_hz: u32, channels: u16 },
    #[error("native whisper.cpp inference failed: {0}")]
    Inference(String),
    #[error("native whisper.cpp callback failed: {0}")]
    Callback(String),
    #[error("native whisper.cpp runtime lock was poisoned")]
    Poisoned,
    #[error("the model is not handled by the consolidated primary runtime: {0}")]
    UnsupportedModel(ModelId),
}

/// Deliberately private: the only Phase 2 selection has one variant.
#[derive(Clone, Copy, Debug)]
enum RuntimeKind {
    TranscribeCpp,
}

fn runtime_kind_for_model(model_id: &ModelId) -> Option<RuntimeKind> {
    (cfg!(all(target_os = "windows", target_arch = "x86_64"))
        && model_id.as_str().starts_with("whisper_cpp_"))
    .then_some(RuntimeKind::TranscribeCpp)
}

/// The sole application-level runtime router. Clones share one serialized
/// engine state, which both retains a warm model and enforces the upstream
/// same-context non-concurrency rule.
#[derive(Clone)]
pub(crate) struct RuntimeRouter {
    inner: Arc<Mutex<RouterState>>,
}

impl RuntimeRouter {
    pub(crate) fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(RouterState::default())),
        }
    }

    pub(crate) fn handles_model(&self, model_id: &ModelId) -> bool {
        runtime_kind_for_model(model_id).is_some()
    }

    pub(crate) fn capabilities(&self, model: &RuntimeModel) -> Option<RuntimeCapabilities> {
        runtime_kind_for_model(&model.id).map(|kind| match kind {
            RuntimeKind::TranscribeCpp => RuntimeCapabilities {
                timestamps: true,
                supported_languages: vec!["en".to_owned()],
                ..RuntimeCapabilities::default()
            },
        })
    }

    pub(crate) fn transcribe(
        &self,
        model: RuntimeModel,
        preference: AccelerationPreference,
        audio: RuntimeAudio<'_>,
    ) -> Result<RuntimeExecution, RuntimeError> {
        if audio.sample_rate_hz != 16_000
            || audio.channels != 1
            || audio.samples.is_empty()
            || audio
                .samples
                .iter()
                .any(|sample| !sample.is_finite() || !(-1.0..=1.0).contains(sample))
        {
            return Err(RuntimeError::InvalidAudio {
                sample_rate_hz: audio.sample_rate_hz,
                channels: audio.channels,
            });
        }

        let kind = runtime_kind_for_model(&model.id)
            .ok_or_else(|| RuntimeError::UnsupportedModel(model.id.clone()))?;
        let mut state = self.inner.lock().map_err(|_| RuntimeError::Poisoned)?;
        match kind {
            RuntimeKind::TranscribeCpp => state.transcribe_cpp(model, preference, audio.samples),
        }
    }
}

impl std::fmt::Debug for RuntimeRouter {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RuntimeRouter")
            .finish_non_exhaustive()
    }
}

#[derive(Default)]
struct RouterState {
    transcribe_cpp: Option<TranscribeCppRuntime>,
}

impl RouterState {
    fn transcribe_cpp(
        &mut self,
        model: RuntimeModel,
        preference: AccelerationPreference,
        samples: &[f32],
    ) -> Result<RuntimeExecution, RuntimeError> {
        if !cfg!(all(target_os = "windows", target_arch = "x86_64")) {
            return Err(NativeBootstrapFailure::UnsupportedPlatform.into());
        }
        let acceleration = resolve_acceleration(preference)?;
        let reusable = self
            .transcribe_cpp
            .as_ref()
            .is_some_and(|runtime| runtime.model == model && runtime.acceleration == acceleration);
        if !reusable {
            NativePackage::from_root(model.package_root.clone()).verify_native()?;
            self.transcribe_cpp = Some(TranscribeCppRuntime::new(model, acceleration));
        }

        let runtime = self
            .transcribe_cpp
            .as_mut()
            .expect("the selected runtime was initialized");
        let load_started = Instant::now();
        let warm_reused = runtime.ensure_loaded()?;
        let model_load_duration_ms = if warm_reused {
            0
        } else {
            load_started.elapsed().as_millis()
        };
        let processing_started = Instant::now();
        let transcript = runtime.transcribe(samples)?;
        let processing_duration_ms = processing_started.elapsed().as_millis();
        runtime.last_used_at = Some(Instant::now());

        Ok(RuntimeExecution {
            transcript,
            diagnostics: NativeRuntimeDiagnostics {
                resolved_acceleration: runtime.acceleration.clone(),
                native_library_path: runtime.package.native_library_path(),
                warm_reused,
                model_load_duration_ms,
            },
            processing_duration_ms,
        })
    }
}

struct TranscribeCppRuntime {
    model: RuntimeModel,
    package: NativePackage,
    acceleration: ResolvedAcceleration,
    loaded: Option<NativeWhisperHandle>,
    last_used_at: Option<Instant>,
}

impl TranscribeCppRuntime {
    fn new(model: RuntimeModel, acceleration: ResolvedAcceleration) -> Self {
        let package = NativePackage::from_root(model.package_root.clone());
        Self {
            model,
            package,
            acceleration,
            loaded: None,
            last_used_at: None,
        }
    }

    /// Returns whether a retained model context was reused. The mutex held by
    /// `RuntimeRouter` serializes the context and prevents unload during FFI.
    fn ensure_loaded(&mut self) -> Result<bool, NativeBootstrapFailure> {
        let expired = self
            .last_used_at
            .is_some_and(|last_used| last_used.elapsed() >= WARM_MODEL_TTL);
        if expired {
            self.loaded.take();
            self.last_used_at = None;
        }
        if self.loaded.is_some() {
            return Ok(self.last_used_at.is_some());
        }

        let mut handle = NativeWhisperHandle::open(&self.package)?;
        let use_gpu = matches!(self.acceleration.resolved, ComputeDevice::Gpu { .. });
        // Auto resolves to CPU for this verified CPU-only package, so native
        // initialization explicitly receives `use_gpu = false` in both Auto
        // and explicit CPU cases.
        handle.load_model(&self.model.path, use_gpu, 0)?;
        self.loaded = Some(handle);
        Ok(false)
    }

    fn transcribe(&mut self, samples: &[f32]) -> Result<RuntimeTranscript, RuntimeError> {
        let handle = self
            .loaded
            .as_mut()
            .expect("ensure_loaded must retain a native context");
        handle.transcribe(samples)
    }
}

#[derive(Clone, Debug)]
struct NativePackage {
    root: PathBuf,
}

impl NativePackage {
    fn from_root(root: PathBuf) -> Self {
        Self { root }
    }

    fn native_library_path(&self) -> PathBuf {
        self.root.join("bin").join(if cfg!(windows) {
            "whisper.dll"
        } else {
            "libwhisper.so"
        })
    }

    fn compatibility_cli_path(&self) -> PathBuf {
        Self::compatibility_cli_path_for_root(&self.root)
    }

    fn compatibility_cli_path_for_root(root: &Path) -> PathBuf {
        root.join("bin").join(if cfg!(windows) {
            "whisper-cli.exe"
        } else {
            "whisper-cli"
        })
    }

    fn verify_native(&self) -> Result<(), NativeBootstrapFailure> {
        if !self.root.is_dir() {
            return Err(NativeBootstrapFailure::PackageUnavailable {
                message: format!("package root is not a directory: {}", self.root.display()),
            });
        }
        verify_sha256(&self.native_library_path(), WHISPER_DLL_SHA256)?;
        for (relative_path, hash) in COMMON_GGML_DEPENDENCIES {
            let path = self.root.join(relative_path);
            verify_sha256(&path, hash)?;
        }
        Ok(())
    }
}

/// Verifies a compatibility CLI independently before an external caller elects
/// to use its fallback path. Hash failures are never fallback eligible.
pub(crate) fn verify_compatibility_cli(path: &Path) -> Result<(), NativeBootstrapFailure> {
    verify_sha256(path, WHISPER_CLI_SHA256)?;
    let package_root =
        native_package_root(path).ok_or_else(|| NativeBootstrapFailure::PackageUnavailable {
            message: format!(
                "compatibility CLI must be located in a package bin directory: {}",
                path.display()
            ),
        })?;
    for (relative_path, hash) in COMMON_GGML_DEPENDENCIES {
        verify_sha256(&package_root.join(relative_path), hash)?;
    }
    Ok(())
}

fn native_package_root(path: &Path) -> Option<PathBuf> {
    path.parent()?.parent().map(Path::to_path_buf)
}

fn resolve_acceleration(
    requested: AccelerationPreference,
) -> Result<ResolvedAcceleration, NativeBootstrapFailure> {
    match requested {
        AccelerationPreference::Auto => Ok(ResolvedAcceleration {
            requested,
            resolved: ComputeDevice::Cpu,
            diagnostic: Some(
                "Auto selected CPU because the verified whisper.cpp v1.9.1 package has no validated accelerator backend."
                    .to_owned(),
            ),
        }),
        AccelerationPreference::Cpu => Ok(ResolvedAcceleration {
            requested,
            resolved: ComputeDevice::Cpu,
            diagnostic: None,
        }),
        AccelerationPreference::Gpu => Err(NativeBootstrapFailure::UnsupportedGpu),
    }
}

fn verify_sha256(path: &Path, expected: &'static str) -> Result<(), NativeBootstrapFailure> {
    if !path.is_file() {
        return Err(NativeBootstrapFailure::PackageFileMissing {
            path: path.to_path_buf(),
        });
    }
    let actual = sha256_file(path).map_err(|source| NativeBootstrapFailure::PackageIo {
        path: path.to_path_buf(),
        source,
    })?;
    if actual != expected {
        return Err(NativeBootstrapFailure::PackageHashMismatch {
            path: path.to_path_buf(),
            expected,
            actual,
        });
    }
    Ok(())
}

fn sha256_file(path: &Path) -> io::Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 8192];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

#[repr(C)]
struct NativeRuntimeOpaque {
    _private: [u8; 0],
}

type SegmentCallback = unsafe extern "C" fn(*mut c_void, *const c_char, i64, i64);

unsafe extern "C" {
    fn scribe_whisper_runtime_open(
        path: *const c_char,
        out_error: *mut *mut c_char,
    ) -> *mut NativeRuntimeOpaque;
    fn scribe_whisper_runtime_load_model(
        runtime: *mut NativeRuntimeOpaque,
        model_path: *const c_char,
        use_gpu: i32,
        gpu_device: i32,
        out_error: *mut *mut c_char,
    ) -> i32;
    fn scribe_whisper_runtime_transcribe(
        runtime: *mut NativeRuntimeOpaque,
        samples: *const f32,
        sample_count: usize,
        callback: Option<SegmentCallback>,
        user_data: *mut c_void,
        out_language: *mut *mut c_char,
        out_error: *mut *mut c_char,
    ) -> i32;
    fn scribe_whisper_runtime_destroy(runtime: *mut NativeRuntimeOpaque);
    fn scribe_whisper_string_free(value: *mut c_char);
}

struct NativeWhisperHandle(NonNull<NativeRuntimeOpaque>);

// The opaque context is never accessed concurrently: RuntimeRouter keeps this
// handle behind a Mutex, FFI calls borrow it under that lock, and Drop only
// occurs after the lock owner has finished the active call.
unsafe impl Send for NativeWhisperHandle {}

impl NativeWhisperHandle {
    fn open(package: &NativePackage) -> Result<Self, NativeBootstrapFailure> {
        let compatibility_cli_path = package.compatibility_cli_path();
        let library_path = package.native_library_path();
        let library_path = path_to_cstring(&library_path).map_err(|message| {
            NativeBootstrapFailure::NativeLibrary {
                message,
                compatibility_cli_path: compatibility_cli_path.clone(),
            }
        })?;
        let mut error = std::ptr::null_mut();
        let runtime = unsafe { scribe_whisper_runtime_open(library_path.as_ptr(), &mut error) };
        NonNull::new(runtime)
            .map(Self)
            .ok_or_else(|| NativeBootstrapFailure::NativeLibrary {
                message: take_native_string(error)
                    .unwrap_or_else(|| "native shim returned no error".to_owned()),
                compatibility_cli_path,
            })
    }

    fn load_model(
        &mut self,
        model_path: &Path,
        use_gpu: bool,
        gpu_device: i32,
    ) -> Result<(), NativeBootstrapFailure> {
        let original_model_path = model_path.to_path_buf();
        let model_path =
            path_to_cstring(model_path).map_err(|message| NativeBootstrapFailure::ModelLoad {
                path: original_model_path.clone(),
                message,
            })?;
        let mut error = std::ptr::null_mut();
        let result = unsafe {
            scribe_whisper_runtime_load_model(
                self.0.as_ptr(),
                model_path.as_ptr(),
                i32::from(use_gpu),
                gpu_device,
                &mut error,
            )
        };
        if result == 0 {
            Ok(())
        } else {
            Err(NativeBootstrapFailure::ModelLoad {
                path: original_model_path,
                message: take_native_string(error)
                    .unwrap_or_else(|| "native shim returned no error".to_owned()),
            })
        }
    }

    fn transcribe(&mut self, samples: &[f32]) -> Result<RuntimeTranscript, RuntimeError> {
        let mut callback_state = CallbackState::default();
        let mut language = std::ptr::null_mut();
        let mut error = std::ptr::null_mut();
        let result = unsafe {
            scribe_whisper_runtime_transcribe(
                self.0.as_ptr(),
                samples.as_ptr(),
                samples.len(),
                Some(collect_segment),
                (&mut callback_state as *mut CallbackState).cast(),
                &mut language,
                &mut error,
            )
        };
        let detected_language = take_native_string(language);
        if result != 0 {
            return Err(RuntimeError::Inference(
                take_native_string(error)
                    .unwrap_or_else(|| "native shim returned no error".to_owned()),
            ));
        }
        if let Some(error) = callback_state.error {
            return Err(RuntimeError::Callback(error));
        }
        let text = callback_state
            .segments
            .iter()
            .map(|segment| segment.text.as_str())
            .collect::<String>();
        Ok(RuntimeTranscript {
            text,
            segments: callback_state.segments,
            detected_language,
            duration_ms: Some((samples.len() as u128 * 1000) / 16_000),
        })
    }
}

impl Drop for NativeWhisperHandle {
    fn drop(&mut self) {
        unsafe { scribe_whisper_runtime_destroy(self.0.as_ptr()) };
    }
}

#[derive(Default)]
struct CallbackState {
    segments: Vec<RuntimeSegment>,
    error: Option<String>,
}

unsafe extern "C" fn collect_segment(
    user_data: *mut c_void,
    text: *const c_char,
    start_ticks: i64,
    end_ticks: i64,
) {
    if user_data.is_null() {
        return;
    }
    let state = unsafe { &mut *user_data.cast::<CallbackState>() };
    if state.error.is_some() {
        return;
    }
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let text = if text.is_null() {
            return Err("native Whisper returned a NULL segment text pointer".to_owned());
        } else {
            unsafe { CStr::from_ptr(text) }
                .to_string_lossy()
                .into_owned()
        };
        let start_ms = ticks_to_ms(start_ticks)?;
        let end_ms = ticks_to_ms(end_ticks)?;
        Ok(RuntimeSegment {
            text,
            start_ms,
            end_ms,
        })
    }));
    match outcome {
        Ok(Ok(segment)) => state.segments.push(segment),
        Ok(Err(error)) => state.error = Some(error),
        Err(_) => state.error = Some("Rust segment callback panicked".to_owned()),
    }
}

fn ticks_to_ms(ticks: i64) -> Result<u64, String> {
    u64::try_from(ticks)
        .ok()
        .and_then(|value| value.checked_mul(10))
        .ok_or_else(|| "native Whisper returned an invalid segment timestamp".to_owned())
}

fn path_to_cstring(path: &Path) -> Result<CString, String> {
    // Whisper v1.9.1 exposes only narrow `char *` model paths. Windows Unicode
    // path fidelity is therefore not proven for this upstream ABI.
    CString::new(path.to_string_lossy().as_bytes())
        .map_err(|_| "native Whisper path contains an interior NUL byte".to_owned())
}

fn take_native_string(value: *mut c_char) -> Option<String> {
    if value.is_null() {
        return None;
    }
    let string = unsafe { CStr::from_ptr(value) }
        .to_string_lossy()
        .into_owned();
    unsafe { scribe_whisper_string_free(value) };
    Some(string)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collect_rust_sources(root: &Path, output: &mut Vec<(PathBuf, String)>) {
        for entry in std::fs::read_dir(root).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                collect_rust_sources(&path, output);
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                output.push((path.clone(), std::fs::read_to_string(path).unwrap()));
            }
        }
    }

    #[test]
    fn cpu_only_package_resolves_auto_and_cpu_honestly() {
        let auto = resolve_acceleration(AccelerationPreference::Auto).unwrap();
        assert_eq!(auto.resolved, ComputeDevice::Cpu);
        assert!(auto.diagnostic.is_some());

        let cpu = resolve_acceleration(AccelerationPreference::Cpu).unwrap();
        assert_eq!(cpu.resolved, ComputeDevice::Cpu);
        assert_eq!(cpu.diagnostic, None);
        assert!(matches!(
            resolve_acceleration(AccelerationPreference::Gpu),
            Err(NativeBootstrapFailure::UnsupportedGpu)
        ));
    }

    #[test]
    fn fallback_requires_a_separately_verified_compatibility_cli() {
        assert!(
            !NativeBootstrapFailure::NativeLibrary {
                message: "missing symbol".to_owned(),
                compatibility_cli_path: PathBuf::from("missing-whisper-cli.exe"),
            }
            .cli_fallback_eligible()
        );
        assert!(!NativeBootstrapFailure::UnsupportedGpu.cli_fallback_eligible());
        assert!(
            !NativeBootstrapFailure::ModelLoad {
                path: PathBuf::from("model.bin"),
                message: "invalid model".to_owned(),
            }
            .cli_fallback_eligible()
        );
    }

    #[test]
    fn segment_ticks_are_checked_in_ten_millisecond_units() {
        assert_eq!(ticks_to_ms(123), Ok(1230));
        assert!(ticks_to_ms(-1).is_err());
    }

    #[test]
    fn manifest_and_hash_constants_stay_pinned_to_the_audited_package() {
        let manifest = include_str!("../runtime-manifests/whisper-cpp-v1.9.1-windows-x64.json");
        assert!(manifest.contains(WHISPER_DLL_SHA256));
        assert!(manifest.contains(WHISPER_CLI_SHA256));
        assert!(manifest.contains(WHISPER_ARCHIVE_SHA256));
        assert!(manifest.contains("f049fff95a089aa9969deb009cdd4892b3e74916"));
    }

    #[test]
    fn tampered_runtime_file_is_rejected_without_fallback_eligibility() {
        let path = std::env::temp_dir().join(format!(
            "scribe-tampered-runtime-{}-{}.dll",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(&path, b"not the pinned runtime").unwrap();

        let error = verify_sha256(&path, WHISPER_DLL_SHA256).unwrap_err();
        let _ = std::fs::remove_file(path);

        assert!(matches!(
            error,
            NativeBootstrapFailure::PackageHashMismatch { .. }
        ));
        assert!(!error.cli_fallback_eligible());
    }

    #[test]
    fn concrete_runtime_boundary_is_confined_to_the_router() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut sources = Vec::new();
        collect_rust_sources(&root, &mut sources);

        for (path, source) in &sources {
            if path
                .file_name()
                .is_some_and(|name| name == "runtime_router.rs")
            {
                continue;
            }
            assert!(
                !source.contains("RuntimeKind"),
                "RuntimeKind escaped into {}",
                path.display()
            );
            assert!(
                !source.contains("TranscribeCppRuntime"),
                "concrete runtime escaped into {}",
                path.display()
            );
            assert!(
                !source.contains("OnnxSpeechRuntime"),
                "unjustified second handler appeared in {}",
                path.display()
            );
        }

        let app = sources
            .iter()
            .find(|(path, _)| path.file_name().is_some_and(|name| name == "app.rs"))
            .map(|(_, source)| source)
            .unwrap();
        assert!(!app.contains("stt::whisper_cpp"));
        assert!(!app.contains("RuntimeRouter"));
        assert!(!app.contains("transcribe_with_config"));
    }
}
