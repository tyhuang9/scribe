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
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::model_catalog::{RuntimeRequirement, RuntimeVersion, runtime_model_manifest};
use crate::prepared_audio::{PREPARED_SAMPLE_RATE, PreparedAudio};
use crate::transcription::{
    AccelerationPreference, ComputeDevice, ModelId, ResolvedAcceleration, RuntimeCapabilities,
    SpeechEngine, Transcript, TranscriptSegment, TranscriptionOptions,
};

pub(crate) const WARM_MODEL_TTL: Duration = Duration::from_secs(5 * 60);
const WHISPER_DLL_SHA256: &str = "b31690c12461517fe9774e61318ab63a69972b948151feed98b913be35f708b6";
const WHISPER_CLI_SHA256: &str = "58245314fb73b30fbd0cf0542c5c172e23f02b6eb7cad7b51e792439cf5e1755";
#[cfg(test)]
const WHISPER_ARCHIVE_SHA256: &str =
    "7d8be46ecd31828e1eb7a2ecdd0d6b314feafd82163038ab6092594b0a063539";
const COMMON_GGML_DEPENDENCIES: [(&str, &str); 11] = [
    (
        "ggml.dll",
        "db753141098018ab482796052a61e727ee0106cbc280f28397f6a111b5e667d7",
    ),
    (
        "ggml-base.dll",
        "8be6f3e06388b3a9aac75d29bec86363e2e2f5b0cee86ce6438866bcac0bcf86",
    ),
    (
        "ggml-cpu-alderlake.dll",
        "323408503da53ccc67248b26d711f16d73d2d6239f7703a00a6a18b60ed5b8b8",
    ),
    (
        "ggml-cpu-cannonlake.dll",
        "0f659d98b823bb871c7845787bba7485facd220099cf58aa773652b9b842ab2e",
    ),
    (
        "ggml-cpu-cascadelake.dll",
        "8116b0e516134139de29400c536ecf06fe708ce1a078a96d30b562b30d524fbe",
    ),
    (
        "ggml-cpu-haswell.dll",
        "e5925923a47672392f9e9c8c92e4b9b65ea473948bf4f568a0300a3a42485135",
    ),
    (
        "ggml-cpu-icelake.dll",
        "b726d528bee0c811c6b2ad8775357379d651cabb487bbf800331697fe73da187",
    ),
    (
        "ggml-cpu-sandybridge.dll",
        "1c49c64817233b2447ca305b41c66afa4bed31b058bc190a98af2a30cc703542",
    ),
    (
        "ggml-cpu-skylakex.dll",
        "06082dc62a09a82fbba4aab49b2c049b96db84c5fc561a446a8ddbfb9b20bf86",
    ),
    (
        "ggml-cpu-sse42.dll",
        "9a8f55ff1dfad231aa6250ac52c330c5bfa5c4c37691c8b591a68b52090ce40c",
    ),
    (
        "ggml-cpu-x64.dll",
        "45ff644d301b8a1fffc7c5e3864205047360eb197814c7311f366d106bb5b19f",
    ),
];

/// A model selected for the sole native whisper.cpp runtime.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RuntimeModel {
    pub id: ModelId,
    pub path: PathBuf,
    pub package_root: PathBuf,
    pub expected_size_bytes: u64,
    pub expected_sha256: &'static str,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct NativeRuntimeDiagnostics {
    pub resolved_acceleration: ResolvedAcceleration,
    pub native_library_path: PathBuf,
    pub warm_reused: bool,
    pub model_load_duration_ms: u128,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RuntimeExecution {
    pub transcript: Transcript,
    pub diagnostics: NativeRuntimeDiagnostics,
    pub processing_duration_ms: u128,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RuntimeLoadExecution {
    pub diagnostics: NativeRuntimeDiagnostics,
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
    #[error("verified model integrity check failed for {path}: {message}")]
    ModelIntegrity { path: PathBuf, message: String },
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
    #[error("native speech engine failed: {0}")]
    Engine(String),
    #[error("native whisper.cpp runtime lock was poisoned")]
    Poisoned,
    #[error("the model is not handled by the consolidated primary runtime: {0}")]
    UnsupportedModel(ModelId),
    #[error("dedicated native runtime worker is unavailable: {0}")]
    WorkerUnavailable(String),
}

/// Deliberately private: the only Phase 2 selection has one variant.
#[derive(Clone, Copy, Debug)]
enum RuntimeKind {
    TranscribeCpp,
}

const TRANSCRIBE_CPP_RUNTIME_VERSION: RuntimeVersion = RuntimeVersion {
    major: 1,
    minor: 9,
    patch: 1,
};

fn runtime_kind_for_model(model_id: &ModelId) -> Option<RuntimeKind> {
    if !cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        return None;
    }
    let manifest = runtime_model_manifest(model_id)?;
    match manifest.runtime {
        RuntimeRequirement::PrimaryNative
            if TRANSCRIBE_CPP_RUNTIME_VERSION >= manifest.minimum_runtime_version =>
        {
            Some(RuntimeKind::TranscribeCpp)
        }
        RuntimeRequirement::PrimaryNative => None,
    }
}

/// The sole application-level runtime router. Clones share one serialized
/// engine state, which both retains a warm model and enforces the upstream
/// same-context non-concurrency rule.
#[derive(Clone)]
pub(crate) struct RuntimeRouter {
    inner: Arc<Mutex<RouterState>>,
    cancel_generation: Arc<AtomicU64>,
}

impl RuntimeRouter {
    pub(crate) fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(RouterState::default())),
            cancel_generation: Arc::new(AtomicU64::new(0)),
        }
    }

    pub(crate) fn handles_model(&self, model_id: &ModelId) -> bool {
        runtime_kind_for_model(model_id).is_some()
    }

    pub(crate) fn capabilities(&self, model_id: &ModelId) -> Option<RuntimeCapabilities> {
        runtime_kind_for_model(model_id).map(|kind| match kind {
            RuntimeKind::TranscribeCpp => TranscribeCppRuntime::runtime_capabilities(),
        })
    }

    pub(crate) fn transcribe(
        &self,
        model: RuntimeModel,
        preference: AccelerationPreference,
        audio: &PreparedAudio,
        options: &TranscriptionOptions,
        cancellation_snapshot: u64,
    ) -> Result<RuntimeExecution, RuntimeError> {
        if audio.sample_rate != PREPARED_SAMPLE_RATE
            || audio.samples.is_empty()
            || audio
                .samples
                .iter()
                .any(|sample| !sample.is_finite() || !(-1.0..=1.0).contains(sample))
        {
            return Err(RuntimeError::InvalidAudio {
                sample_rate_hz: audio.sample_rate,
                channels: 1,
            });
        }

        let kind = runtime_kind_for_model(&model.id)
            .ok_or_else(|| RuntimeError::UnsupportedModel(model.id.clone()))?;
        let mut state = self.inner.lock().map_err(|_| RuntimeError::Poisoned)?;
        match kind {
            RuntimeKind::TranscribeCpp => state.transcribe_cpp(
                model,
                preference,
                audio,
                options,
                Arc::clone(&self.cancel_generation),
                cancellation_snapshot,
            ),
        }
    }

    pub(crate) fn load(
        &self,
        model: RuntimeModel,
        preference: AccelerationPreference,
    ) -> Result<RuntimeLoadExecution, RuntimeError> {
        let kind = runtime_kind_for_model(&model.id)
            .ok_or_else(|| RuntimeError::UnsupportedModel(model.id.clone()))?;
        let mut state = self.inner.lock().map_err(|_| RuntimeError::Poisoned)?;
        match kind {
            RuntimeKind::TranscribeCpp => {
                state.load_transcribe_cpp(model, preference, Arc::clone(&self.cancel_generation))
            }
        }
    }

    pub(crate) fn health_check(
        &self,
        model: RuntimeModel,
        preference: AccelerationPreference,
    ) -> Result<(), RuntimeError> {
        let kind = runtime_kind_for_model(&model.id)
            .ok_or_else(|| RuntimeError::UnsupportedModel(model.id.clone()))?;
        let mut state = self.inner.lock().map_err(|_| RuntimeError::Poisoned)?;
        match kind {
            RuntimeKind::TranscribeCpp => {
                let runtime = state.transcribe_cpp_runtime(
                    model,
                    preference,
                    Arc::clone(&self.cancel_generation),
                )?;
                SpeechEngine::health_check(runtime)
                    .map_err(|error| RuntimeError::Engine(format!("{error:#}")))
            }
        }
    }

    /// Cancellation is lock-free so it can interrupt inference while the
    /// dedicated native worker owns the serialized engine lock.
    pub(crate) fn cancel_active(&self) {
        self.cancel_generation.fetch_add(1, Ordering::AcqRel);
    }

    pub(crate) fn cancellation_snapshot(&self) -> u64 {
        self.cancel_generation.load(Ordering::Acquire)
    }

    pub(crate) fn unload_all(&self) -> Result<(), RuntimeError> {
        let mut state = self.inner.lock().map_err(|_| RuntimeError::Poisoned)?;
        if let Some(runtime) = state.transcribe_cpp.as_mut() {
            SpeechEngine::unload(runtime)
                .map_err(|error| RuntimeError::Engine(format!("{error:#}")))?;
        }
        state.transcribe_cpp = None;
        Ok(())
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
    fn transcribe_cpp_runtime(
        &mut self,
        model: RuntimeModel,
        preference: AccelerationPreference,
        cancel_generation: Arc<AtomicU64>,
    ) -> Result<&mut TranscribeCppRuntime, RuntimeError> {
        if !cfg!(all(target_os = "windows", target_arch = "x86_64")) {
            return Err(NativeBootstrapFailure::UnsupportedPlatform.into());
        }
        let acceleration = resolve_acceleration(preference)?;
        let reusable = self
            .transcribe_cpp
            .as_ref()
            .is_some_and(|runtime| runtime.model == model && runtime.acceleration == acceleration);
        if !reusable {
            self.transcribe_cpp = Some(TranscribeCppRuntime::new(
                model,
                acceleration,
                cancel_generation,
            ));
        }
        Ok(self
            .transcribe_cpp
            .as_mut()
            .expect("the selected runtime was initialized"))
    }

    fn load_transcribe_cpp(
        &mut self,
        model: RuntimeModel,
        preference: AccelerationPreference,
        cancel_generation: Arc<AtomicU64>,
    ) -> Result<RuntimeLoadExecution, RuntimeError> {
        let runtime = self.transcribe_cpp_runtime(model, preference, cancel_generation)?;
        let load_started = Instant::now();
        let warm_reused = runtime.ensure_loaded()?;
        let model_load_duration_ms = if warm_reused {
            0
        } else {
            load_started.elapsed().as_millis()
        };
        runtime.last_used_at = Some(Instant::now());
        Ok(RuntimeLoadExecution {
            diagnostics: NativeRuntimeDiagnostics {
                resolved_acceleration: runtime.acceleration.clone(),
                native_library_path: runtime.package.native_library_path(),
                warm_reused,
                model_load_duration_ms,
            },
        })
    }

    fn transcribe_cpp(
        &mut self,
        model: RuntimeModel,
        preference: AccelerationPreference,
        audio: &PreparedAudio,
        options: &TranscriptionOptions,
        cancel_generation: Arc<AtomicU64>,
        cancellation_snapshot: u64,
    ) -> Result<RuntimeExecution, RuntimeError> {
        let runtime = self.transcribe_cpp_runtime(model, preference, cancel_generation)?;
        let load_started = Instant::now();
        let warm_reused = runtime.ensure_loaded()?;
        let model_load_duration_ms = if warm_reused {
            0
        } else {
            load_started.elapsed().as_millis()
        };
        let processing_started = Instant::now();
        // Invoke the selected concrete handler through the common engine
        // contract. RuntimeRouter remains the only code that selects it.
        runtime.request_cancel_snapshot = Some(cancellation_snapshot);
        let transcript = match SpeechEngine::transcribe(runtime, audio, options) {
            Ok(transcript) => transcript,
            Err(error) => {
                // A failed native decode may leave upstream context state
                // ambiguous. Discard it so the next request performs a clean
                // load and cannot be misreported as a warm reuse.
                let _ = SpeechEngine::unload(runtime);
                return Err(RuntimeError::Engine(format!("{error:#}")));
            }
        };
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
    cancel_generation: Arc<AtomicU64>,
    request_cancel_snapshot: Option<u64>,
}

impl TranscribeCppRuntime {
    fn new(
        model: RuntimeModel,
        acceleration: ResolvedAcceleration,
        cancel_generation: Arc<AtomicU64>,
    ) -> Self {
        let package = NativePackage::from_root(model.package_root.clone());
        Self {
            model,
            package,
            acceleration,
            loaded: None,
            last_used_at: None,
            cancel_generation,
            request_cancel_snapshot: None,
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

        // Verify immediately before every open, including retries and TTL
        // reloads. Verification is not cached across a dropped handle.
        self.package.verify_native()?;
        verify_runtime_model(&self.model)?;
        let mut handle = NativeWhisperHandle::open(&self.package)?;
        let use_gpu = matches!(self.acceleration.resolved, ComputeDevice::Gpu { .. });
        // Auto resolves to CPU for this verified CPU-only package, so native
        // initialization explicitly receives `use_gpu = false` in both Auto
        // and explicit CPU cases.
        handle.load_model(&self.model.path, use_gpu, 0)?;
        self.loaded = Some(handle);
        Ok(false)
    }

    fn runtime_capabilities() -> RuntimeCapabilities {
        RuntimeCapabilities {
            cancellation: true,
            timestamps: true,
            supported_languages: vec!["en".to_owned()],
            ..RuntimeCapabilities::default()
        }
    }

    fn decode_samples(&mut self, samples: &[f32]) -> Result<Transcript, RuntimeError> {
        let handle = self
            .loaded
            .as_mut()
            .expect("ensure_loaded must retain a native context");
        let cancellation_snapshot = self
            .request_cancel_snapshot
            .take()
            .unwrap_or_else(|| self.cancel_generation.load(Ordering::Acquire));
        handle.transcribe(samples, &self.cancel_generation, cancellation_snapshot)
    }
}

impl SpeechEngine for TranscribeCppRuntime {
    fn load(&mut self) -> anyhow::Result<()> {
        self.ensure_loaded()?;
        Ok(())
    }

    fn transcribe(
        &mut self,
        audio: &PreparedAudio,
        options: &TranscriptionOptions,
    ) -> anyhow::Result<Transcript> {
        if *options != TranscriptionOptions::default() {
            return Err(anyhow::anyhow!(
                "the verified native whisper.cpp adapter currently accepts only default transcription options"
            ));
        }
        if audio.sample_rate != PREPARED_SAMPLE_RATE
            || audio.samples.is_empty()
            || audio
                .samples
                .iter()
                .any(|sample| !sample.is_finite() || !(-1.0..=1.0).contains(sample))
        {
            return Err(anyhow::anyhow!(
                "native whisper.cpp requires non-empty canonical mono 16 kHz finite f32 audio"
            ));
        }
        if self.loaded.is_none() {
            self.load()?;
        }
        self.decode_samples(&audio.samples).map_err(Into::into)
    }

    fn capabilities(&self) -> RuntimeCapabilities {
        Self::runtime_capabilities()
    }

    fn health_check(&mut self) -> anyhow::Result<()> {
        self.package.verify_native()?;
        verify_runtime_model(&self.model)?;
        Ok(())
    }

    fn cancel(&mut self) -> anyhow::Result<()> {
        self.cancel_generation.fetch_add(1, Ordering::AcqRel);
        Ok(())
    }

    fn unload(&mut self) -> anyhow::Result<()> {
        self.loaded.take();
        self.last_used_at = None;
        Ok(())
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

    fn bin_dir(&self) -> PathBuf {
        let nested = self.root.join("bin");
        if nested.is_dir() {
            nested
        } else {
            self.root.clone()
        }
    }

    fn native_library_path(&self) -> PathBuf {
        self.bin_dir().join(if cfg!(windows) {
            "whisper.dll"
        } else {
            "libwhisper.so"
        })
    }

    fn compatibility_cli_path(&self) -> PathBuf {
        Self::compatibility_cli_path_for_root(&self.root)
    }

    fn compatibility_cli_path_for_root(root: &Path) -> PathBuf {
        let bin_dir = if root.join("bin").is_dir() {
            root.join("bin")
        } else {
            root.to_path_buf()
        };
        bin_dir.join(if cfg!(windows) {
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
        for (file_name, hash) in COMMON_GGML_DEPENDENCIES {
            let path = self.bin_dir().join(file_name);
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
    let package = NativePackage::from_root(package_root);
    for (file_name, hash) in COMMON_GGML_DEPENDENCIES {
        verify_sha256(&package.bin_dir().join(file_name), hash)?;
    }
    Ok(())
}

fn native_package_root(path: &Path) -> Option<PathBuf> {
    let parent = path.parent()?;
    if parent
        .file_name()
        .is_some_and(|name| name.eq_ignore_ascii_case("bin"))
    {
        parent.parent().map(Path::to_path_buf)
    } else {
        Some(parent.to_path_buf())
    }
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

fn verify_runtime_model(model: &RuntimeModel) -> Result<(), NativeBootstrapFailure> {
    let metadata =
        std::fs::metadata(&model.path).map_err(|error| NativeBootstrapFailure::ModelIntegrity {
            path: model.path.clone(),
            message: error.to_string(),
        })?;
    if metadata.len() != model.expected_size_bytes {
        return Err(NativeBootstrapFailure::ModelIntegrity {
            path: model.path.clone(),
            message: format!(
                "expected {} bytes, got {}",
                model.expected_size_bytes,
                metadata.len()
            ),
        });
    }
    let actual =
        sha256_file(&model.path).map_err(|error| NativeBootstrapFailure::ModelIntegrity {
            path: model.path.clone(),
            message: error.to_string(),
        })?;
    if !actual.eq_ignore_ascii_case(model.expected_sha256) {
        return Err(NativeBootstrapFailure::ModelIntegrity {
            path: model.path.clone(),
            message: format!("expected SHA-256 {}, got {actual}", model.expected_sha256),
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
type AbortCallback = unsafe extern "C" fn(*mut c_void) -> i32;

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
        abort_callback: Option<AbortCallback>,
        abort_user_data: *mut c_void,
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

    fn transcribe(
        &mut self,
        samples: &[f32],
        cancel_generation: &AtomicU64,
        cancellation_snapshot: u64,
    ) -> Result<Transcript, RuntimeError> {
        let mut callback_state = CallbackState::default();
        let mut abort_state = AbortState {
            generation: cancel_generation,
            started_at: cancellation_snapshot,
        };
        let mut error = std::ptr::null_mut();
        let result = unsafe {
            scribe_whisper_runtime_transcribe(
                self.0.as_ptr(),
                samples.as_ptr(),
                samples.len(),
                Some(collect_segment),
                (&mut callback_state as *mut CallbackState).cast(),
                Some(check_cancelled),
                (&mut abort_state as *mut AbortState<'_>).cast(),
                &mut error,
            )
        };
        if result != 0 {
            return Err(RuntimeError::Inference(
                take_native_string(error)
                    .unwrap_or_else(|| "native shim returned no error".to_owned()),
            ));
        }
        if let Some(error) = callback_state.error {
            return Err(RuntimeError::Callback(error));
        }
        callback_state
            .segments
            .retain(|segment| !is_blank_audio_segment(&segment.text));
        let text = assemble_segment_text(&callback_state.segments);
        Ok(Transcript {
            text,
            segments: callback_state.segments,
            detected_language: None,
            duration_ms: Some((samples.len() as u128 * 1000) / 16_000),
        })
    }
}

impl Drop for NativeWhisperHandle {
    fn drop(&mut self) {
        unsafe { scribe_whisper_runtime_destroy(self.0.as_ptr()) };
    }
}

fn assemble_segment_text(segments: &[TranscriptSegment]) -> String {
    segments
        .iter()
        .map(|segment| segment.text.as_str())
        .collect::<String>()
        .trim()
        .to_owned()
}

fn is_blank_audio_segment(text: &str) -> bool {
    text.trim().eq_ignore_ascii_case("[BLANK_AUDIO]")
}

struct AbortState<'a> {
    generation: &'a AtomicU64,
    started_at: u64,
}

unsafe extern "C" fn check_cancelled(user_data: *mut c_void) -> i32 {
    if user_data.is_null() {
        return 1;
    }
    let state = unsafe { &*user_data.cast::<AbortState<'_>>() };
    i32::from(state.generation.load(Ordering::Acquire) != state.started_at)
}

#[derive(Default)]
struct CallbackState {
    segments: Vec<TranscriptSegment>,
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
        Ok(TranscriptSegment {
            text,
            start_ms: Some(start_ms),
            end_ms: Some(end_ms),
            confidence: None,
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
    let path = path
        .to_str()
        .ok_or_else(|| "native Whisper path is not valid Unicode".to_owned())?;
    CString::new(path.as_bytes())
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
    fn native_handler_implements_the_common_speech_engine_contract() {
        fn assert_engine<T: SpeechEngine>() {}
        assert_engine::<TranscribeCppRuntime>();
    }

    #[test]
    fn runtime_selection_requires_a_catalog_manifest_not_an_id_prefix() {
        assert!(runtime_kind_for_model(&ModelId::new("whisper_cpp_unknown")).is_none());
        if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
            assert!(matches!(
                runtime_kind_for_model(&ModelId::new("whisper_cpp_base_en")),
                Some(RuntimeKind::TranscribeCpp)
            ));
        }
    }

    #[test]
    fn runtime_version_must_meet_the_model_minimum() {
        assert!(
            TRANSCRIBE_CPP_RUNTIME_VERSION
                >= RuntimeVersion {
                    major: 1,
                    minor: 9,
                    patch: 1,
                }
        );
        assert!(
            TRANSCRIBE_CPP_RUNTIME_VERSION
                < RuntimeVersion {
                    major: 1,
                    minor: 10,
                    patch: 0,
                }
        );

        let package_manifest: serde_json::Value = serde_json::from_str(include_str!(
            "../runtime-manifests/whisper-cpp-v1.9.1-windows-x64.json"
        ))
        .unwrap();
        assert_eq!(package_manifest["upstream"]["tag"], "v1.9.1");
    }

    #[test]
    fn manifest_symbols_exactly_match_the_native_shim_contract() {
        let manifest: serde_json::Value = serde_json::from_str(include_str!(
            "../runtime-manifests/whisper-cpp-v1.9.1-windows-x64.json"
        ))
        .unwrap();
        let symbols = manifest["entrypoints"]["required_symbols"]
            .as_array()
            .unwrap()
            .iter()
            .map(|value| value.as_str().unwrap())
            .collect::<Vec<_>>();
        let expected = [
            "whisper_context_default_params_by_ref",
            "whisper_full_default_params_by_ref",
            "whisper_free_context_params",
            "whisper_free_params",
            "whisper_init_from_file_with_params",
            "whisper_full",
            "whisper_full_n_segments",
            "whisper_full_get_segment_text",
            "whisper_full_get_segment_t0",
            "whisper_full_get_segment_t1",
            "whisper_free",
        ];
        assert_eq!(symbols, expected);
        assert_eq!(
            manifest["entrypoints"]["backend_required_symbols"],
            serde_json::json!(["ggml_backend_load"])
        );
        assert_eq!(
            manifest["entrypoints"]["cpu_backend_required_symbols"],
            serde_json::json!(["ggml_backend_score", "ggml_backend_init"])
        );

        let shim = include_str!("../native/whisper_shim.c");
        for symbol in expected {
            assert!(shim.contains(&format!("\"{symbol}\"")));
        }
        assert!(shim.contains("\"ggml_backend_load\""));
        assert!(shim.contains("\"ggml_backend_score\""));
        assert!(!shim.contains("ggml_backend_load_all"));
    }

    #[test]
    fn cancellation_generation_is_observed_without_the_router_lock() {
        let generation = AtomicU64::new(9);
        let mut state = AbortState {
            generation: &generation,
            started_at: 9,
        };
        let state_ptr = (&mut state as *mut AbortState<'_>).cast();

        assert_eq!(unsafe { check_cancelled(state_ptr) }, 0);
        generation.fetch_add(1, Ordering::AcqRel);
        assert_eq!(unsafe { check_cancelled(state_ptr) }, 1);
        assert_eq!(unsafe { check_cancelled(std::ptr::null_mut()) }, 1);
    }

    #[test]
    fn segment_assembly_removes_only_outer_whitespace() {
        let segments = vec![
            TranscriptSegment {
                text: "  Hello".to_owned(),
                start_ms: Some(0),
                end_ms: Some(100),
                confidence: None,
            },
            TranscriptSegment {
                text: " world.  ".to_owned(),
                start_ms: Some(100),
                end_ms: Some(200),
                confidence: None,
            },
        ];

        assert_eq!(assemble_segment_text(&segments), "Hello world.");
    }

    #[test]
    fn private_whisper_adapter_recognizes_blank_audio_sentinel() {
        assert!(is_blank_audio_segment(" [BLANK_AUDIO] "));
        assert!(is_blank_audio_segment("[blank_audio]"));
        assert!(!is_blank_audio_segment("blank audio"));
        assert!(!is_blank_audio_segment("[music]"));
    }

    #[test]
    fn native_package_supports_nested_and_flat_upstream_layouts() {
        let root = std::env::temp_dir().join(format!(
            "scribe-native-layout-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(root.join("bin")).unwrap();
        let nested = NativePackage::from_root(root.clone());
        assert_eq!(nested.native_library_path(), root.join("bin/whisper.dll"));
        std::fs::remove_dir_all(root.join("bin")).unwrap();
        let flat = NativePackage::from_root(root.clone());
        assert_eq!(flat.native_library_path(), root.join("whisper.dll"));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn non_unicode_windows_paths_are_rejected_without_lossy_substitution() {
        use std::ffi::OsString;
        use std::os::windows::ffi::OsStringExt;

        let path = PathBuf::from(OsString::from_wide(&[0xD800]));
        assert!(path_to_cstring(&path).is_err());
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
        let app = app
            .split("\n#[cfg(test)]\nmod layout_tests")
            .next()
            .unwrap();
        for forbidden in [
            "use crate::stt",
            "runtime_catalog::",
            "provider_for_backend",
            ".backend",
            "backend_label",
            "RuntimeRouter",
            "transcribe_with_config",
            "whisper_cpp_",
        ] {
            assert!(
                !app.contains(forbidden),
                "production app escaped the neutral service boundary through {forbidden}"
            );
        }

        for (path, source) in &sources {
            let production_source = if path.file_name().is_some_and(|name| name == "app.rs") {
                source
                    .split("\n#[cfg(test)]\nmod layout_tests")
                    .next()
                    .unwrap()
            } else {
                source
            };
            if production_source.contains("provider_for_backend") {
                let allowed = path.ends_with("stt/mod.rs")
                    || path.file_name().is_some_and(|name| {
                        name == "compatibility_bridge.rs" || name == "runtime_router.rs"
                    });
                assert!(
                    allowed,
                    "legacy provider selection escaped its private bridge into {}",
                    path.display()
                );
            }
        }

        for (path, source) in &sources {
            let production_source = if path.file_name().is_some_and(|name| name == "app.rs") {
                source
                    .split("\n#[cfg(test)]\nmod layout_tests")
                    .next()
                    .unwrap()
            } else {
                source
            };
            for concrete_adapter in [
                "stt::whisper_cpp",
                "stt::faster_whisper",
                "stt::vosk",
                "stt::sherpa_onnx",
            ] {
                let allowed = path
                    .components()
                    .any(|component| component.as_os_str() == "stt")
                    || path.file_name().is_some_and(|name| {
                        name == "runtime_router.rs" || name == "compatibility_bridge.rs"
                    });
                assert!(
                    allowed || !production_source.contains(concrete_adapter),
                    "concrete compatibility adapter escaped its private bridge into {}",
                    path.display()
                );
            }
        }
    }
}
