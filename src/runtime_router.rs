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
use transcribe_cpp::CancelToken;

use crate::embedded_runtime::{EmbeddedRuntime, TRANSCRIBE_CPP_VERSION};
use crate::model_catalog::{
    ArtifactFormat, RuntimeRequirement, RuntimeVersion, runtime_model_manifest,
};
use crate::onnx_worker::{
    OnnxModelFamily, OnnxModelSpec, OnnxWorkerSupervisor, resolve_cpu_only_acceleration,
};
use crate::prepared_audio::{PREPARED_SAMPLE_RATE, PreparedAudio};
use crate::transcription::{
    AccelerationPreference, ComputeDevice, ModelId, ResolvedAcceleration, RuntimeCapabilities,
    SpeechEngine, SpeechStream, StreamUpdate, StreamingSpeechEngine, Transcript, TranscriptSegment,
    TranscriptionOptions,
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
    pub format: ArtifactFormat,
    /// Legacy GGML uses a hash-verified package; the safe GGUF route is
    /// statically linked and deliberately has no downloaded runtime package.
    pub package_root: Option<PathBuf>,
    pub expected_size_bytes: u64,
    pub expected_sha256: String,
}

/// Internal, typed runtime input. Public model resolution still produces the
/// existing `RuntimeModel`; conversion at the service boundary keeps concrete
/// handler selection confined to this router.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum RuntimeArtifact {
    Gguf(RuntimeModel),
    LegacyCompatibility(RuntimeModel),
    OnnxBundle(OnnxModelSpec),
}

impl From<RuntimeModel> for RuntimeArtifact {
    fn from(model: RuntimeModel) -> Self {
        if is_gguf_model(&model) {
            Self::Gguf(model)
        } else {
            Self::LegacyCompatibility(model)
        }
    }
}

impl RuntimeArtifact {
    fn model_id(&self) -> ModelId {
        match self {
            Self::Gguf(model) | Self::LegacyCompatibility(model) => model.id.clone(),
            Self::OnnxBundle(model) => ModelId::new(model.id.clone()),
        }
    }
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
    /// Identity reported by the loaded model itself. It is intentionally
    /// carried alongside load diagnostics so install validation can persist
    /// observed facts without rebuilding a catalog assumption.
    pub detected_architecture: String,
    pub capabilities: RuntimeCapabilities,
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
    #[error("isolated ONNX speech runtime is unavailable: {0}")]
    OnnxUnavailable(String),
    #[error(
        "legacy GGML model {model_id} at {path} requires a verified whisper.cpp package root; install or repair the compatibility runtime before loading it"
    )]
    MissingLegacyPackageRoot { model_id: ModelId, path: PathBuf },
}

/// Deliberately private: concrete runtime selection never crosses the router.
#[derive(Clone, Copy, Debug)]
enum RuntimeKind {
    TranscribeCpp,
    OnnxSpeech,
}

const TRANSCRIBE_CPP_RUNTIME_VERSION: RuntimeVersion = RuntimeVersion {
    major: 1,
    minor: 9,
    patch: 1,
};

fn runtime_kind_for_model(model_id: &ModelId) -> Option<RuntimeKind> {
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

fn is_gguf_model(model: &RuntimeModel) -> bool {
    model.format == ArtifactFormat::Gguf
}

/// A remote GGUF is admitted only after `TranscriptionService` has resolved
/// its persisted trusted source and supplied an immutable size/digest. The
/// router deliberately keys the safe embedded route on the concrete GGUF
/// artifact rather than on a display/catalog ID, so dynamic catalog entries
/// use the same in-process engine as the bundled default.
fn runtime_kind_for_runtime_model(model: &RuntimeModel) -> Option<RuntimeKind> {
    is_gguf_model(model)
        .then_some(RuntimeKind::TranscribeCpp)
        .or_else(|| runtime_kind_for_model(&model.id))
}

fn runtime_kind_for_artifact(artifact: &RuntimeArtifact) -> Option<RuntimeKind> {
    match artifact {
        RuntimeArtifact::Gguf(_) => Some(RuntimeKind::TranscribeCpp),
        RuntimeArtifact::LegacyCompatibility(model) => runtime_kind_for_model(&model.id),
        RuntimeArtifact::OnnxBundle(_) => Some(RuntimeKind::OnnxSpeech),
    }
}

fn heavy_owner_for_artifact(artifact: &RuntimeArtifact) -> HeavyRuntimeOwner {
    match artifact {
        RuntimeArtifact::Gguf(_) => HeavyRuntimeOwner::EmbeddedGguf,
        RuntimeArtifact::LegacyCompatibility(_) => HeavyRuntimeOwner::LegacyCompatibility,
        RuntimeArtifact::OnnxBundle(_) => HeavyRuntimeOwner::OnnxSpeech,
    }
}

fn embedded_runtime_location() -> PathBuf {
    PathBuf::from(format!(
        "<statically linked transcribe-cpp {TRANSCRIBE_CPP_VERSION}>"
    ))
}

fn onnx_runtime_location() -> PathBuf {
    PathBuf::from("<isolated sherpa-onnx worker>")
}

pub(crate) trait OnnxSupervisorControl: Send + Sync {
    fn load(&self, session_id: u64, request_id: u64, model: OnnxModelSpec) -> anyhow::Result<bool>;
    fn transcribe(
        &self,
        session_id: u64,
        request_id: u64,
        samples: &[f32],
    ) -> anyhow::Result<String>;
    fn start_stream(&self, session_id: u64, request_id: u64) -> anyhow::Result<()>;
    fn audio_chunk(
        &self,
        session_id: u64,
        request_id: u64,
        samples: &[f32],
    ) -> anyhow::Result<String>;
    fn end_stream(&self, session_id: u64, request_id: u64) -> anyhow::Result<String>;
    fn cancel_stream(&self, session_id: u64, request_id: u64) -> anyhow::Result<()>;
    fn health(&self, session_id: u64, request_id: u64) -> anyhow::Result<()>;
    fn unload(&self) -> anyhow::Result<()>;
    fn cancel_active(&self) -> anyhow::Result<()>;
    fn abandon_stream(&self, session_id: u64);
}

impl OnnxSupervisorControl for OnnxWorkerSupervisor {
    fn load(&self, session_id: u64, request_id: u64, model: OnnxModelSpec) -> anyhow::Result<bool> {
        OnnxWorkerSupervisor::load(self, session_id, request_id, model)
    }

    fn transcribe(
        &self,
        session_id: u64,
        request_id: u64,
        samples: &[f32],
    ) -> anyhow::Result<String> {
        OnnxWorkerSupervisor::transcribe(self, session_id, request_id, samples)
    }

    fn start_stream(&self, session_id: u64, request_id: u64) -> anyhow::Result<()> {
        OnnxWorkerSupervisor::start_stream(self, session_id, request_id)
    }

    fn audio_chunk(
        &self,
        session_id: u64,
        request_id: u64,
        samples: &[f32],
    ) -> anyhow::Result<String> {
        OnnxWorkerSupervisor::audio_chunk(self, session_id, request_id, samples)
    }

    fn end_stream(&self, session_id: u64, request_id: u64) -> anyhow::Result<String> {
        OnnxWorkerSupervisor::end_stream(self, session_id, request_id)
    }

    fn cancel_stream(&self, session_id: u64, request_id: u64) -> anyhow::Result<()> {
        OnnxWorkerSupervisor::cancel_stream(self, session_id, request_id)
    }

    fn health(&self, session_id: u64, request_id: u64) -> anyhow::Result<()> {
        OnnxWorkerSupervisor::health(self, session_id, request_id)
    }

    fn unload(&self) -> anyhow::Result<()> {
        OnnxWorkerSupervisor::unload(self)
    }

    fn cancel_active(&self) -> anyhow::Result<()> {
        OnnxWorkerSupervisor::cancel_active(self)
    }

    fn abandon_stream(&self, session_id: u64) {
        OnnxWorkerSupervisor::abandon_stream(self, session_id)
    }
}

type OnnxSupervisorFactory =
    dyn Fn() -> Result<Arc<dyn OnnxSupervisorControl>, RuntimeError> + Send + Sync;

fn production_onnx_supervisor() -> Result<Arc<dyn OnnxSupervisorControl>, RuntimeError> {
    OnnxWorkerSupervisor::spawn()
        .map(|supervisor| Arc::new(supervisor) as Arc<dyn OnnxSupervisorControl>)
        .map_err(|error| RuntimeError::OnnxUnavailable(error.to_string()))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum HeavyRuntimeOwner {
    EmbeddedGguf,
    LegacyCompatibility,
    OnnxSpeech,
}

#[derive(Default)]
struct HeavyRuntimeOwnership {
    current: Option<HeavyRuntimeOwner>,
}

impl HeavyRuntimeOwnership {
    fn transition(
        &mut self,
        requested: HeavyRuntimeOwner,
        unload: impl FnOnce(HeavyRuntimeOwner) -> Result<(), RuntimeError>,
    ) -> Result<(), RuntimeError> {
        let Some(current) = self.current.filter(|current| *current != requested) else {
            return Ok(());
        };
        unload(current)?;
        self.current = None;
        Ok(())
    }

    fn activate(&mut self, owner: HeavyRuntimeOwner) {
        self.current = Some(owner);
    }

    fn clear(&mut self, owner: HeavyRuntimeOwner) {
        if self.current == Some(owner) {
            self.current = None;
        }
    }
}

#[derive(Clone)]
pub(crate) struct RuntimeActivity {
    inner: Arc<Mutex<RuntimeActivityState>>,
}

struct RuntimeActivityState {
    active_streams: usize,
    active_requests: usize,
    generation: u64,
    idle_since: Instant,
}

impl Default for RuntimeActivity {
    fn default() -> Self {
        Self {
            inner: Arc::new(Mutex::new(RuntimeActivityState {
                active_streams: 0,
                active_requests: 0,
                generation: 0,
                idle_since: Instant::now(),
            })),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum IdleTimeoutAction {
    Unload,
    Defer(Duration),
}

impl RuntimeActivity {
    fn acquire_stream(&self) -> Result<RuntimeActivityLease, RuntimeError> {
        let mut state = self.inner.lock().map_err(|_| RuntimeError::Poisoned)?;
        state.active_streams = state.active_streams.saturating_add(1);
        Ok(RuntimeActivityLease {
            activity: self.clone(),
            generation: state.generation,
            kind: RuntimeActivityKind::Stream,
            refresh_idle_on_release: true,
            released: false,
        })
    }

    pub(crate) fn acquire_request(&self) -> Result<RuntimeActivityLease, RuntimeError> {
        let mut state = self.inner.lock().map_err(|_| RuntimeError::Poisoned)?;
        state.active_requests = state.active_requests.saturating_add(1);
        Ok(RuntimeActivityLease {
            activity: self.clone(),
            generation: state.generation,
            kind: RuntimeActivityKind::Request,
            refresh_idle_on_release: false,
            released: false,
        })
    }

    pub(crate) fn mark_command_complete(&self) {
        self.mark_command_complete_at(Instant::now());
    }

    fn mark_command_complete_at(&self, now: Instant) {
        if let Ok(mut state) = self.inner.lock()
            && state.active_streams == 0
            && state.active_requests == 0
        {
            state.idle_since = now;
        }
    }

    pub(crate) fn timeout_action(&self, ttl: Duration) -> IdleTimeoutAction {
        self.timeout_action_at(Instant::now(), ttl)
    }

    fn timeout_action_at(&self, now: Instant, ttl: Duration) -> IdleTimeoutAction {
        let Ok(state) = self.inner.lock() else {
            return IdleTimeoutAction::Defer(ttl);
        };
        if state.active_streams != 0 || state.active_requests != 0 {
            return IdleTimeoutAction::Defer(ttl);
        }
        let idle_for = now.saturating_duration_since(state.idle_since);
        if idle_for >= ttl {
            IdleTimeoutAction::Unload
        } else {
            IdleTimeoutAction::Defer(ttl - idle_for)
        }
    }

    fn force_release_streams(&self) {
        self.force_release_streams_at(Instant::now());
    }

    fn force_release_streams_at(&self, now: Instant) {
        if let Ok(mut state) = self.inner.lock() {
            state.active_streams = 0;
            state.active_requests = 0;
            state.generation = state.generation.wrapping_add(1);
            state.idle_since = now;
        }
    }

    #[cfg(test)]
    pub(crate) fn active_streams(&self) -> usize {
        self.inner.lock().unwrap().active_streams
    }
}

pub(crate) struct RuntimeActivityLease {
    activity: RuntimeActivity,
    generation: u64,
    kind: RuntimeActivityKind,
    refresh_idle_on_release: bool,
    released: bool,
}

#[derive(Clone, Copy)]
enum RuntimeActivityKind {
    Stream,
    Request,
}

impl RuntimeActivityLease {
    pub(crate) fn complete_successfully(&mut self) {
        self.complete_successfully_at(Instant::now());
    }

    fn complete_successfully_at(&mut self, now: Instant) {
        self.refresh_idle_on_release = true;
        self.release_at(now);
    }

    fn release(&mut self) {
        self.release_at(Instant::now());
    }

    fn release_at(&mut self, now: Instant) {
        if self.released {
            return;
        }
        if let Ok(mut state) = self.activity.inner.lock()
            && state.generation == self.generation
        {
            match self.kind {
                RuntimeActivityKind::Stream => {
                    state.active_streams = state.active_streams.saturating_sub(1);
                }
                RuntimeActivityKind::Request => {
                    state.active_requests = state.active_requests.saturating_sub(1);
                }
            }
            if self.refresh_idle_on_release
                && state.active_streams == 0
                && state.active_requests == 0
            {
                state.idle_since = now;
            }
        }
        self.released = true;
    }
}

impl Drop for RuntimeActivityLease {
    fn drop(&mut self) {
        self.release();
    }
}

/// The sole application-level runtime router. Clones share one serialized
/// engine state, which both retains a warm model and enforces the upstream
/// same-context non-concurrency rule.
#[derive(Clone)]
pub(crate) struct RuntimeRouter {
    inner: Arc<Mutex<RouterState>>,
    cancel_generation: Arc<AtomicU64>,
    embedded_cancellation: Arc<Mutex<Option<CancelToken>>>,
    onnx_cancellation: Arc<Mutex<Option<Arc<dyn OnnxSupervisorControl>>>>,
    onnx_factory: Arc<OnnxSupervisorFactory>,
    next_onnx_correlation: Arc<AtomicU64>,
    runtime_activity: RuntimeActivity,
}

struct EmbeddedCancellationContext {
    token: Arc<Mutex<Option<CancelToken>>>,
    generation: Arc<AtomicU64>,
    snapshot: u64,
}

impl RuntimeRouter {
    pub(crate) fn new() -> Self {
        Self::with_onnx_factory(Arc::new(production_onnx_supervisor))
    }

    fn with_onnx_factory(onnx_factory: Arc<OnnxSupervisorFactory>) -> Self {
        Self {
            inner: Arc::new(Mutex::new(RouterState::default())),
            cancel_generation: Arc::new(AtomicU64::new(0)),
            embedded_cancellation: Arc::new(Mutex::new(None)),
            onnx_cancellation: Arc::new(Mutex::new(None)),
            onnx_factory,
            next_onnx_correlation: Arc::new(AtomicU64::new(0)),
            runtime_activity: RuntimeActivity::default(),
        }
    }

    #[cfg(test)]
    pub(crate) fn with_test_onnx_factory(
        factory: impl Fn() -> Result<Arc<dyn OnnxSupervisorControl>, RuntimeError>
        + Send
        + Sync
        + 'static,
    ) -> Self {
        Self::with_onnx_factory(Arc::new(factory))
    }

    #[cfg(test)]
    pub(crate) fn onnx_state_for_test(&self) -> (bool, bool, bool) {
        let state = self.inner.lock().unwrap();
        (
            state.onnx.is_some(),
            self.onnx_cancellation.lock().unwrap().is_some(),
            state.heavy_ownership.current == Some(HeavyRuntimeOwner::OnnxSpeech),
        )
    }

    pub(crate) fn handles_model(&self, model_id: &ModelId) -> bool {
        runtime_kind_for_model(model_id).is_some()
    }

    pub(crate) fn managed_runtime_id(&self, model_id: &ModelId) -> Option<&'static str> {
        if runtime_model_manifest(model_id)
            .is_some_and(|manifest| manifest.artifact_filename.ends_with(".gguf"))
        {
            return None;
        }
        runtime_kind_for_model(model_id).map(|kind| match kind {
            RuntimeKind::TranscribeCpp => "whisper_cpp",
            RuntimeKind::OnnxSpeech => unreachable!("catalog models never select private ONNX"),
        })
    }

    pub(crate) fn capabilities(&self, model_id: &ModelId) -> Option<RuntimeCapabilities> {
        runtime_kind_for_model(model_id).map(|kind| match kind {
            RuntimeKind::TranscribeCpp => TranscribeCppRuntime::runtime_capabilities(),
            RuntimeKind::OnnxSpeech => unreachable!("catalog models never select private ONNX"),
        })
    }

    pub(crate) fn embedded_capabilities(&self) -> RuntimeCapabilities {
        TranscribeCppRuntime::runtime_capabilities()
    }

    pub(crate) fn transcribe(
        &self,
        artifact: RuntimeArtifact,
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

        let kind = runtime_kind_for_artifact(&artifact)
            .ok_or_else(|| RuntimeError::UnsupportedModel(artifact.model_id()))?;
        let owner = heavy_owner_for_artifact(&artifact);
        if let RuntimeArtifact::OnnxBundle(model) = &artifact {
            model
                .validate()
                .map_err(|error| RuntimeError::OnnxUnavailable(error.to_string()))?;
            resolve_cpu_only_acceleration(preference)
                .map_err(|error| RuntimeError::OnnxUnavailable(error.to_string()))?;
        }
        let mut state = self.inner.lock().map_err(|_| RuntimeError::Poisoned)?;
        state.prepare_heavy_runtime(
            owner,
            &self.embedded_cancellation,
            &self.onnx_cancellation,
            &self.runtime_activity,
        )?;
        match (kind, artifact) {
            (RuntimeKind::TranscribeCpp, RuntimeArtifact::Gguf(model)) => state
                .transcribe_embedded(
                    model,
                    preference,
                    audio,
                    options,
                    EmbeddedCancellationContext {
                        token: Arc::clone(&self.embedded_cancellation),
                        generation: Arc::clone(&self.cancel_generation),
                        snapshot: cancellation_snapshot,
                    },
                ),
            (RuntimeKind::TranscribeCpp, RuntimeArtifact::LegacyCompatibility(model)) => state
                .transcribe_cpp(
                    model,
                    preference,
                    audio,
                    options,
                    Arc::clone(&self.cancel_generation),
                    cancellation_snapshot,
                ),
            (RuntimeKind::OnnxSpeech, RuntimeArtifact::OnnxBundle(model)) => state.transcribe_onnx(
                model,
                preference,
                audio,
                options,
                Arc::clone(&self.onnx_factory),
                Arc::clone(&self.onnx_cancellation),
                Arc::clone(&self.next_onnx_correlation),
                self.runtime_activity.clone(),
            ),
            _ => unreachable!("runtime kind and typed artifact must agree"),
        }
    }

    pub(crate) fn load(
        &self,
        artifact: RuntimeArtifact,
        preference: AccelerationPreference,
    ) -> Result<RuntimeLoadExecution, RuntimeError> {
        let kind = runtime_kind_for_artifact(&artifact)
            .ok_or_else(|| RuntimeError::UnsupportedModel(artifact.model_id()))?;
        let owner = heavy_owner_for_artifact(&artifact);
        if let RuntimeArtifact::OnnxBundle(model) = &artifact {
            model
                .validate()
                .map_err(|error| RuntimeError::OnnxUnavailable(error.to_string()))?;
            resolve_cpu_only_acceleration(preference)
                .map_err(|error| RuntimeError::OnnxUnavailable(error.to_string()))?;
        }
        let mut state = self.inner.lock().map_err(|_| RuntimeError::Poisoned)?;
        state.prepare_heavy_runtime(
            owner,
            &self.embedded_cancellation,
            &self.onnx_cancellation,
            &self.runtime_activity,
        )?;
        match (kind, artifact) {
            (RuntimeKind::TranscribeCpp, RuntimeArtifact::Gguf(model)) => {
                state.load_embedded(model, preference, Arc::clone(&self.embedded_cancellation))
            }
            (RuntimeKind::TranscribeCpp, RuntimeArtifact::LegacyCompatibility(model)) => {
                state.load_transcribe_cpp(model, preference, Arc::clone(&self.cancel_generation))
            }
            (RuntimeKind::OnnxSpeech, RuntimeArtifact::OnnxBundle(model)) => state.load_onnx(
                model,
                preference,
                Arc::clone(&self.onnx_factory),
                Arc::clone(&self.onnx_cancellation),
                Arc::clone(&self.next_onnx_correlation),
                self.runtime_activity.clone(),
            ),
            _ => unreachable!("runtime kind and typed artifact must agree"),
        }
    }

    pub(crate) fn health_check(
        &self,
        artifact: RuntimeArtifact,
        preference: AccelerationPreference,
    ) -> Result<(), RuntimeError> {
        let kind = runtime_kind_for_artifact(&artifact)
            .ok_or_else(|| RuntimeError::UnsupportedModel(artifact.model_id()))?;
        let owner = heavy_owner_for_artifact(&artifact);
        if let RuntimeArtifact::OnnxBundle(model) = &artifact {
            model
                .validate()
                .map_err(|error| RuntimeError::OnnxUnavailable(error.to_string()))?;
            resolve_cpu_only_acceleration(preference)
                .map_err(|error| RuntimeError::OnnxUnavailable(error.to_string()))?;
        }
        let mut state = self.inner.lock().map_err(|_| RuntimeError::Poisoned)?;
        state.prepare_heavy_runtime(
            owner,
            &self.embedded_cancellation,
            &self.onnx_cancellation,
            &self.runtime_activity,
        )?;
        match (kind, artifact) {
            (RuntimeKind::TranscribeCpp, RuntimeArtifact::Gguf(model)) => state
                .health_check_embedded(model, preference, Arc::clone(&self.embedded_cancellation)),
            (RuntimeKind::TranscribeCpp, RuntimeArtifact::LegacyCompatibility(model)) => state
                .health_check_transcribe_cpp(
                    model,
                    preference,
                    Arc::clone(&self.cancel_generation),
                ),
            (RuntimeKind::OnnxSpeech, RuntimeArtifact::OnnxBundle(model)) => state.health_onnx(
                model,
                preference,
                Arc::clone(&self.onnx_factory),
                Arc::clone(&self.onnx_cancellation),
                Arc::clone(&self.next_onnx_correlation),
                self.runtime_activity.clone(),
            ),
            _ => unreachable!("runtime kind and typed artifact must agree"),
        }
    }

    pub(crate) fn start_stream(
        &self,
        artifact: RuntimeArtifact,
        preference: AccelerationPreference,
        options: &TranscriptionOptions,
    ) -> Result<Box<dyn SpeechStream>, RuntimeError> {
        let RuntimeArtifact::OnnxBundle(model) = artifact else {
            return Err(RuntimeError::OnnxUnavailable(
                "incremental streaming is available only for a private ONNX bundle".to_owned(),
            ));
        };
        model
            .validate()
            .map_err(|error| RuntimeError::OnnxUnavailable(error.to_string()))?;
        resolve_cpu_only_acceleration(preference)
            .map_err(|error| RuntimeError::OnnxUnavailable(error.to_string()))?;
        let mut state = self.inner.lock().map_err(|_| RuntimeError::Poisoned)?;
        state.prepare_heavy_runtime(
            HeavyRuntimeOwner::OnnxSpeech,
            &self.embedded_cancellation,
            &self.onnx_cancellation,
            &self.runtime_activity,
        )?;
        let loaded = {
            let runtime = state.onnx_runtime(
                Arc::clone(&self.onnx_factory),
                Arc::clone(&self.onnx_cancellation),
                Arc::clone(&self.next_onnx_correlation),
                self.runtime_activity.clone(),
            )?;
            runtime.load_model(model, preference)
        };
        if let Err(error) = loaded {
            state.discard_onnx_runtime(&self.onnx_cancellation, &self.runtime_activity);
            return Err(error);
        }
        state
            .heavy_ownership
            .activate(HeavyRuntimeOwner::OnnxSpeech);
        let started = StreamingSpeechEngine::start_stream(
            state
                .onnx
                .as_mut()
                .expect("successful ONNX load retains its runtime"),
            options,
        );
        match started {
            Ok(stream) => Ok(stream),
            Err(error) => {
                state.discard_onnx_runtime(&self.onnx_cancellation, &self.runtime_activity);
                Err(RuntimeError::Engine(format!("{error:#}")))
            }
        }
    }

    /// Cancellation never waits for the native worker lock. The legacy path
    /// observes its generation atomically; the safe adapter receives an owned
    /// `CancelToken` that it polls in the native decode callback.
    pub(crate) fn cancel_active(&self) {
        self.cancel_generation.fetch_add(1, Ordering::AcqRel);
        if let Ok(active) = self.embedded_cancellation.lock()
            && let Some(token) = active.as_ref()
        {
            token.cancel();
        }
        if let Ok(active) = self.onnx_cancellation.lock()
            && let Some(supervisor) = active.as_ref()
        {
            let _ = supervisor.cancel_active();
        }
    }

    pub(crate) fn cancellation_snapshot(&self) -> u64 {
        self.cancel_generation.load(Ordering::Acquire)
    }

    pub(crate) fn runtime_activity(&self) -> RuntimeActivity {
        self.runtime_activity.clone()
    }

    pub(crate) fn unload_all(&self) -> Result<(), RuntimeError> {
        let mut state = self.inner.lock().map_err(|_| RuntimeError::Poisoned)?;
        let mut first_error = None;
        if let Some(runtime) = state.transcribe_cpp.as_mut() {
            if let Err(error) = SpeechEngine::unload(runtime) {
                first_error = Some(RuntimeError::Engine(format!("{error:#}")));
            }
        }
        state.transcribe_cpp = None;
        if let Some(runtime) = state.embedded.as_mut() {
            if let Err(error) = SpeechEngine::unload(runtime)
                && first_error.is_none()
            {
                first_error = Some(RuntimeError::Engine(format!("{error:#}")));
            }
        }
        state.embedded = None;
        state.embedded_model = None;
        if let Some(runtime) = state.onnx.as_mut() {
            if let Err(error) = SpeechEngine::unload(runtime)
                && first_error.is_none()
            {
                first_error = Some(RuntimeError::Engine(format!("{error:#}")));
            }
        }
        state.onnx = None;
        match self.embedded_cancellation.lock() {
            Ok(mut cancellation) => *cancellation = None,
            Err(poisoned) => {
                *poisoned.into_inner() = None;
                if first_error.is_none() {
                    first_error = Some(RuntimeError::Poisoned);
                }
            }
        }
        match self.onnx_cancellation.lock() {
            Ok(mut cancellation) => *cancellation = None,
            Err(poisoned) => {
                *poisoned.into_inner() = None;
                if first_error.is_none() {
                    first_error = Some(RuntimeError::Poisoned);
                }
            }
        }
        state.heavy_ownership = HeavyRuntimeOwnership::default();
        self.runtime_activity.force_release_streams();
        first_error.map_or(Ok(()), Err)
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
    heavy_ownership: HeavyRuntimeOwnership,
    transcribe_cpp: Option<TranscribeCppRuntime>,
    embedded: Option<EmbeddedRuntime>,
    embedded_model: Option<RuntimeModel>,
    onnx: Option<OnnxSpeechRuntime>,
}

fn embedded_request_is_warm(
    current_model: Option<&RuntimeModel>,
    current_runtime: Option<(&Path, AccelerationPreference, bool)>,
    requested_model: &RuntimeModel,
    requested_preference: AccelerationPreference,
) -> bool {
    current_model == Some(requested_model)
        && current_runtime.is_some_and(|(path, preference, loaded)| {
            path == requested_model.path && preference == requested_preference && loaded
        })
}

impl RouterState {
    fn discard_onnx_runtime(
        &mut self,
        cancellation: &Arc<Mutex<Option<Arc<dyn OnnxSupervisorControl>>>>,
        runtime_activity: &RuntimeActivity,
    ) {
        self.onnx = None;
        match cancellation.lock() {
            Ok(mut active) => *active = None,
            Err(poisoned) => *poisoned.into_inner() = None,
        }
        self.heavy_ownership.clear(HeavyRuntimeOwner::OnnxSpeech);
        runtime_activity.force_release_streams();
    }

    fn discard_embedded_runtime(&mut self, cancellation: &Arc<Mutex<Option<CancelToken>>>) {
        self.embedded = None;
        self.embedded_model = None;
        match cancellation.lock() {
            Ok(mut active) => *active = None,
            Err(poisoned) => *poisoned.into_inner() = None,
        }
        self.heavy_ownership.clear(HeavyRuntimeOwner::EmbeddedGguf);
    }

    fn prepare_heavy_runtime(
        &mut self,
        requested: HeavyRuntimeOwner,
        embedded_cancellation: &Arc<Mutex<Option<CancelToken>>>,
        onnx_cancellation: &Arc<Mutex<Option<Arc<dyn OnnxSupervisorControl>>>>,
        runtime_activity: &RuntimeActivity,
    ) -> Result<(), RuntimeError> {
        let mut ownership = std::mem::take(&mut self.heavy_ownership);
        let result = ownership.transition(requested, |current| {
            self.unload_heavy_runtime(
                current,
                embedded_cancellation,
                onnx_cancellation,
                runtime_activity,
            )
        });
        self.heavy_ownership = ownership;
        result
    }

    fn unload_heavy_runtime(
        &mut self,
        owner: HeavyRuntimeOwner,
        embedded_cancellation: &Arc<Mutex<Option<CancelToken>>>,
        onnx_cancellation: &Arc<Mutex<Option<Arc<dyn OnnxSupervisorControl>>>>,
        runtime_activity: &RuntimeActivity,
    ) -> Result<(), RuntimeError> {
        match owner {
            HeavyRuntimeOwner::EmbeddedGguf => {
                if let Some(runtime) = self.embedded.as_mut() {
                    SpeechEngine::unload(runtime)
                        .map_err(|error| RuntimeError::Engine(format!("{error:#}")))?;
                }
                self.discard_embedded_runtime(embedded_cancellation);
            }
            HeavyRuntimeOwner::LegacyCompatibility => {
                if let Some(runtime) = self.transcribe_cpp.as_mut() {
                    SpeechEngine::unload(runtime)
                        .map_err(|error| RuntimeError::Engine(format!("{error:#}")))?;
                }
                self.transcribe_cpp = None;
            }
            HeavyRuntimeOwner::OnnxSpeech => {
                if let Some(runtime) = self.onnx.as_mut() {
                    SpeechEngine::unload(runtime)
                        .map_err(|error| RuntimeError::Engine(format!("{error:#}")))?;
                }
                self.onnx = None;
                *onnx_cancellation
                    .lock()
                    .map_err(|_| RuntimeError::Poisoned)? = None;
                runtime_activity.force_release_streams();
            }
        }
        Ok(())
    }

    fn onnx_runtime(
        &mut self,
        factory: Arc<OnnxSupervisorFactory>,
        cancellation: Arc<Mutex<Option<Arc<dyn OnnxSupervisorControl>>>>,
        next_correlation: Arc<AtomicU64>,
        runtime_activity: RuntimeActivity,
    ) -> Result<&mut OnnxSpeechRuntime, RuntimeError> {
        if self.onnx.is_none() {
            let supervisor = factory()?;
            *cancellation.lock().map_err(|_| RuntimeError::Poisoned)? =
                Some(Arc::clone(&supervisor));
            self.onnx = Some(OnnxSpeechRuntime::new(
                supervisor,
                next_correlation,
                runtime_activity,
            ));
        }
        Ok(self
            .onnx
            .as_mut()
            .expect("the ONNX runtime was initialized"))
    }

    fn load_onnx(
        &mut self,
        model: OnnxModelSpec,
        preference: AccelerationPreference,
        factory: Arc<OnnxSupervisorFactory>,
        cancellation: Arc<Mutex<Option<Arc<dyn OnnxSupervisorControl>>>>,
        next_correlation: Arc<AtomicU64>,
        runtime_activity: RuntimeActivity,
    ) -> Result<RuntimeLoadExecution, RuntimeError> {
        let load_started = Instant::now();
        let loaded = {
            let runtime = self.onnx_runtime(
                factory,
                Arc::clone(&cancellation),
                next_correlation,
                runtime_activity.clone(),
            )?;
            runtime.load_model(model, preference).map(|warm_reused| {
                (
                    warm_reused,
                    runtime
                        .resolved_acceleration()
                        .expect("successful ONNX load resolves acceleration")
                        .clone(),
                    runtime
                        .detected_architecture()
                        .expect("successful ONNX load retains its model family"),
                    SpeechEngine::capabilities(runtime),
                )
            })
        };
        let (warm_reused, resolved_acceleration, detected_architecture, capabilities) = match loaded
        {
            Ok(loaded) => loaded,
            Err(error) => {
                self.discard_onnx_runtime(&cancellation, &runtime_activity);
                return Err(error);
            }
        };
        self.heavy_ownership.activate(HeavyRuntimeOwner::OnnxSpeech);
        let model_load_duration_ms = if warm_reused {
            0
        } else {
            load_started.elapsed().as_millis()
        };
        Ok(RuntimeLoadExecution {
            diagnostics: NativeRuntimeDiagnostics {
                resolved_acceleration,
                native_library_path: onnx_runtime_location(),
                warm_reused,
                model_load_duration_ms,
            },
            detected_architecture,
            capabilities,
        })
    }

    fn transcribe_onnx(
        &mut self,
        model: OnnxModelSpec,
        preference: AccelerationPreference,
        audio: &PreparedAudio,
        options: &TranscriptionOptions,
        factory: Arc<OnnxSupervisorFactory>,
        cancellation: Arc<Mutex<Option<Arc<dyn OnnxSupervisorControl>>>>,
        next_correlation: Arc<AtomicU64>,
        runtime_activity: RuntimeActivity,
    ) -> Result<RuntimeExecution, RuntimeError> {
        let load_started = Instant::now();
        let loaded = {
            let runtime = self.onnx_runtime(
                factory,
                Arc::clone(&cancellation),
                next_correlation,
                runtime_activity.clone(),
            )?;
            runtime.load_model(model, preference).map(|warm_reused| {
                (
                    warm_reused,
                    runtime
                        .resolved_acceleration()
                        .expect("successful ONNX load resolves acceleration")
                        .clone(),
                )
            })
        };
        let (warm_reused, resolved_acceleration) = match loaded {
            Ok(loaded) => loaded,
            Err(error) => {
                self.discard_onnx_runtime(&cancellation, &runtime_activity);
                return Err(error);
            }
        };
        self.heavy_ownership.activate(HeavyRuntimeOwner::OnnxSpeech);
        let model_load_duration_ms = if warm_reused {
            0
        } else {
            load_started.elapsed().as_millis()
        };
        let processing_started = Instant::now();
        let transcript = SpeechEngine::transcribe(
            self.onnx
                .as_mut()
                .expect("successful ONNX load retains its runtime"),
            audio,
            options,
        );
        let transcript = match transcript {
            Ok(transcript) => transcript,
            Err(error) => {
                self.discard_onnx_runtime(&cancellation, &runtime_activity);
                return Err(RuntimeError::Engine(format!("{error:#}")));
            }
        };
        Ok(RuntimeExecution {
            transcript,
            diagnostics: NativeRuntimeDiagnostics {
                resolved_acceleration,
                native_library_path: onnx_runtime_location(),
                warm_reused,
                model_load_duration_ms,
            },
            processing_duration_ms: processing_started.elapsed().as_millis(),
        })
    }

    fn health_onnx(
        &mut self,
        model: OnnxModelSpec,
        preference: AccelerationPreference,
        factory: Arc<OnnxSupervisorFactory>,
        cancellation: Arc<Mutex<Option<Arc<dyn OnnxSupervisorControl>>>>,
        next_correlation: Arc<AtomicU64>,
        runtime_activity: RuntimeActivity,
    ) -> Result<(), RuntimeError> {
        let loaded = {
            let runtime = self.onnx_runtime(
                factory,
                Arc::clone(&cancellation),
                next_correlation,
                runtime_activity.clone(),
            )?;
            runtime.load_model(model, preference)
        };
        if let Err(error) = loaded {
            self.discard_onnx_runtime(&cancellation, &runtime_activity);
            return Err(error);
        }
        self.heavy_ownership.activate(HeavyRuntimeOwner::OnnxSpeech);
        let health = SpeechEngine::health_check(
            self.onnx
                .as_mut()
                .expect("successful ONNX load retains its runtime"),
        );
        match health {
            Ok(()) => Ok(()),
            Err(error) => {
                self.discard_onnx_runtime(&cancellation, &runtime_activity);
                Err(RuntimeError::Engine(format!("{error:#}")))
            }
        }
    }

    fn embedded_is_warm(&self, model: &RuntimeModel, preference: AccelerationPreference) -> bool {
        embedded_request_is_warm(
            self.embedded_model.as_ref(),
            self.embedded.as_ref().map(|runtime| {
                (
                    runtime.model_path(),
                    runtime.preference(),
                    runtime.is_loaded(),
                )
            }),
            model,
            preference,
        )
    }

    fn embedded_runtime(
        &mut self,
        model: &RuntimeModel,
        preference: AccelerationPreference,
        cancellation: Arc<Mutex<Option<CancelToken>>>,
    ) -> Result<&mut EmbeddedRuntime, RuntimeError> {
        let reusable = self.embedded_model.as_ref() == Some(model)
            && self.embedded.as_ref().is_some_and(|runtime| {
                runtime.model_path() == model.path && runtime.preference() == preference
            });
        if !reusable {
            if let Some(runtime) = self.embedded.as_mut() {
                SpeechEngine::unload(runtime)
                    .map_err(|error| RuntimeError::Engine(format!("{error:#}")))?;
            }
            self.discard_embedded_runtime(&cancellation);
            self.embedded = Some(EmbeddedRuntime::new(model.path.clone(), preference));
            self.embedded_model = Some(model.clone());
            let token = self
                .embedded
                .as_ref()
                .expect("the embedded runtime was initialized")
                .cancellation_handle();
            match cancellation.lock() {
                Ok(mut active) => *active = Some(token),
                Err(poisoned) => {
                    *poisoned.into_inner() = None;
                    self.discard_embedded_runtime(&cancellation);
                    return Err(RuntimeError::Poisoned);
                }
            }
        }
        Ok(self
            .embedded
            .as_mut()
            .expect("the embedded runtime was initialized"))
    }

    fn load_embedded(
        &mut self,
        model: RuntimeModel,
        preference: AccelerationPreference,
        cancellation: Arc<Mutex<Option<CancelToken>>>,
    ) -> Result<RuntimeLoadExecution, RuntimeError> {
        let warm_reused = self.embedded_is_warm(&model, preference);
        verify_embedded_runtime_model(&model, warm_reused)?;
        let load_started = Instant::now();
        let loaded = {
            let runtime = self.embedded_runtime(&model, preference, Arc::clone(&cancellation))?;
            SpeechEngine::load(runtime).map(|()| {
                (
                    runtime
                        .resolved_acceleration()
                        .cloned()
                        .expect("a successfully loaded embedded runtime resolves acceleration"),
                    runtime
                        .detected_architecture()
                        .expect("a successfully loaded embedded runtime reports its architecture"),
                    SpeechEngine::capabilities(runtime),
                )
            })
        };
        let (resolved_acceleration, detected_architecture, capabilities) = match loaded {
            Ok(loaded) => loaded,
            Err(error) => {
                self.discard_embedded_runtime(&cancellation);
                return Err(RuntimeError::Engine(format!("{error:#}")));
            }
        };
        self.heavy_ownership
            .activate(HeavyRuntimeOwner::EmbeddedGguf);
        Ok(RuntimeLoadExecution {
            diagnostics: NativeRuntimeDiagnostics {
                resolved_acceleration,
                native_library_path: embedded_runtime_location(),
                warm_reused,
                model_load_duration_ms: if warm_reused {
                    0
                } else {
                    load_started.elapsed().as_millis()
                },
            },
            detected_architecture,
            capabilities,
        })
    }

    fn health_check_embedded(
        &mut self,
        model: RuntimeModel,
        preference: AccelerationPreference,
        cancellation: Arc<Mutex<Option<CancelToken>>>,
    ) -> Result<(), RuntimeError> {
        self.load_embedded(model, preference, cancellation)
            .map(|_| ())
    }

    fn transcribe_embedded(
        &mut self,
        model: RuntimeModel,
        preference: AccelerationPreference,
        audio: &PreparedAudio,
        options: &TranscriptionOptions,
        cancellation: EmbeddedCancellationContext,
    ) -> Result<RuntimeExecution, RuntimeError> {
        let warm_reused = self.embedded_is_warm(&model, preference);
        let cancellation_token = Arc::clone(&cancellation.token);
        verify_embedded_runtime_model(&model, warm_reused)?;
        let load_started = Instant::now();
        let load_result = {
            let runtime = self.embedded_runtime(&model, preference, cancellation.token)?;
            SpeechEngine::load(runtime)
        };
        if let Err(error) = load_result {
            if let Some(runtime) = self.embedded.as_mut() {
                let _ = SpeechEngine::unload(runtime);
            }
            self.discard_embedded_runtime(&cancellation_token);
            return Err(RuntimeError::Engine(format!("{error:#}")));
        }
        self.heavy_ownership
            .activate(HeavyRuntimeOwner::EmbeddedGguf);
        let model_load_duration_ms = if warm_reused {
            0
        } else {
            load_started.elapsed().as_millis()
        };
        let (result, diagnostics) = {
            let runtime = self
                .embedded
                .as_mut()
                .expect("successful embedded load retains its runtime");
            let processing_started = Instant::now();
            let result = runtime.transcribe_with_cancellation(
                audio,
                options,
                &cancellation.generation,
                cancellation.snapshot,
            );
            let resolved_acceleration = runtime.resolved_acceleration().cloned();
            let processing_duration_ms = processing_started.elapsed().as_millis();
            (
                result,
                (
                    resolved_acceleration,
                    warm_reused,
                    model_load_duration_ms,
                    processing_duration_ms,
                ),
            )
        };
        let transcript = match result {
            Ok(transcript) => transcript,
            Err(error) => {
                // A native decode error or cancellation must not leave a
                // partial session active. The next request starts from a known
                // model/session state rather than pretending it was warm.
                self.discard_embedded_runtime(&cancellation_token);
                return Err(RuntimeError::Engine(format!("{error:#}")));
            }
        };
        let (resolved_acceleration, warm_reused, model_load_duration_ms, processing_duration_ms) =
            diagnostics;
        Ok(RuntimeExecution {
            transcript,
            diagnostics: NativeRuntimeDiagnostics {
                resolved_acceleration: resolved_acceleration
                    .expect("a successful embedded decode resolves acceleration"),
                native_library_path: embedded_runtime_location(),
                warm_reused,
                model_load_duration_ms,
            },
            processing_duration_ms,
        })
    }

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
            if let Some(runtime) = self.transcribe_cpp.as_mut() {
                SpeechEngine::unload(runtime)
                    .map_err(|error| RuntimeError::Engine(format!("{error:#}")))?;
            }
            self.transcribe_cpp = None;
            self.heavy_ownership
                .clear(HeavyRuntimeOwner::LegacyCompatibility);
            self.transcribe_cpp = Some(TranscribeCppRuntime::new(
                model,
                acceleration,
                cancel_generation,
            )?);
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
        let load_started = Instant::now();
        let loaded = {
            let runtime = self.transcribe_cpp_runtime(model, preference, cancel_generation)?;
            runtime.ensure_loaded().map(|warm_reused| {
                runtime.last_used_at = Some(Instant::now());
                (
                    warm_reused,
                    runtime.acceleration.clone(),
                    runtime.package.native_library_path(),
                    SpeechEngine::capabilities(runtime),
                )
            })
        };
        let (warm_reused, acceleration, native_library_path, capabilities) = match loaded {
            Ok(loaded) => loaded,
            Err(error) => {
                self.transcribe_cpp = None;
                self.heavy_ownership
                    .clear(HeavyRuntimeOwner::LegacyCompatibility);
                return Err(error.into());
            }
        };
        self.heavy_ownership
            .activate(HeavyRuntimeOwner::LegacyCompatibility);
        let model_load_duration_ms = if warm_reused {
            0
        } else {
            load_started.elapsed().as_millis()
        };
        Ok(RuntimeLoadExecution {
            diagnostics: NativeRuntimeDiagnostics {
                resolved_acceleration: acceleration,
                native_library_path,
                warm_reused,
                model_load_duration_ms,
            },
            // GGML compatibility installs do not expose architecture through
            // the retained shim. They are outside the normal installer path;
            // keep their known adapter identity explicit rather than claiming
            // filename-derived evidence.
            detected_architecture: "whisper".to_owned(),
            capabilities,
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
        let load_started = Instant::now();
        let loaded = {
            let runtime = self.transcribe_cpp_runtime(model, preference, cancel_generation)?;
            runtime.ensure_loaded().map(|warm_reused| {
                (
                    warm_reused,
                    runtime.acceleration.clone(),
                    runtime.package.native_library_path(),
                )
            })
        };
        let (warm_reused, acceleration, native_library_path) = match loaded {
            Ok(loaded) => loaded,
            Err(error) => {
                self.transcribe_cpp = None;
                self.heavy_ownership
                    .clear(HeavyRuntimeOwner::LegacyCompatibility);
                return Err(error.into());
            }
        };
        self.heavy_ownership
            .activate(HeavyRuntimeOwner::LegacyCompatibility);
        let model_load_duration_ms = if warm_reused {
            0
        } else {
            load_started.elapsed().as_millis()
        };
        let processing_started = Instant::now();
        // Invoke the selected concrete handler through the common engine
        // contract. RuntimeRouter remains the only code that selects it.
        let runtime = self
            .transcribe_cpp
            .as_mut()
            .expect("successful compatibility load retains its runtime");
        runtime.request_cancel_snapshot = Some(cancellation_snapshot);
        let transcript = match SpeechEngine::transcribe(runtime, audio, options) {
            Ok(transcript) => transcript,
            Err(error) => {
                // A failed native decode may leave upstream context state
                // ambiguous. Discard it so the next request performs a clean
                // load and cannot be misreported as a warm reuse.
                let _ = SpeechEngine::unload(runtime);
                self.transcribe_cpp = None;
                self.heavy_ownership
                    .clear(HeavyRuntimeOwner::LegacyCompatibility);
                return Err(RuntimeError::Engine(format!("{error:#}")));
            }
        };
        let processing_duration_ms = processing_started.elapsed().as_millis();
        runtime.last_used_at = Some(Instant::now());

        Ok(RuntimeExecution {
            transcript,
            diagnostics: NativeRuntimeDiagnostics {
                resolved_acceleration: acceleration,
                native_library_path,
                warm_reused,
                model_load_duration_ms,
            },
            processing_duration_ms,
        })
    }

    fn health_check_transcribe_cpp(
        &mut self,
        model: RuntimeModel,
        preference: AccelerationPreference,
        cancel_generation: Arc<AtomicU64>,
    ) -> Result<(), RuntimeError> {
        self.load_transcribe_cpp(model, preference, cancel_generation)?;
        let health = SpeechEngine::health_check(
            self.transcribe_cpp
                .as_mut()
                .expect("successful compatibility load retains its runtime"),
        );
        match health {
            Ok(()) => Ok(()),
            Err(error) => {
                self.transcribe_cpp = None;
                self.heavy_ownership
                    .clear(HeavyRuntimeOwner::LegacyCompatibility);
                Err(RuntimeError::Engine(format!("{error:#}")))
            }
        }
    }
}

/// Private adapter from the process-isolated ONNX control protocol to the
/// runtime-neutral speech contracts.
struct OnnxSpeechRuntime {
    model: Option<OnnxModelSpec>,
    acceleration: Option<ResolvedAcceleration>,
    supervisor: Arc<dyn OnnxSupervisorControl>,
    next_correlation: Arc<AtomicU64>,
    runtime_activity: RuntimeActivity,
}

impl OnnxSpeechRuntime {
    fn new(
        supervisor: Arc<dyn OnnxSupervisorControl>,
        next_correlation: Arc<AtomicU64>,
        runtime_activity: RuntimeActivity,
    ) -> Self {
        Self {
            model: None,
            acceleration: None,
            supervisor,
            next_correlation,
            runtime_activity,
        }
    }

    fn next_id(&self) -> u64 {
        self.next_correlation
            .fetch_add(1, Ordering::AcqRel)
            .wrapping_add(1)
    }

    fn load_model(
        &mut self,
        model: OnnxModelSpec,
        preference: AccelerationPreference,
    ) -> Result<bool, RuntimeError> {
        let acceleration = resolve_cpu_only_acceleration(preference)
            .map_err(|error| RuntimeError::OnnxUnavailable(error.to_string()))?;
        let correlation = self.next_id();
        let warm_reused = self
            .supervisor
            .load(correlation, correlation, model.clone())
            .map_err(|error| RuntimeError::Engine(format!("{error:#}")))?;
        self.model = Some(model);
        self.acceleration = Some(acceleration);
        Ok(warm_reused)
    }

    fn resolved_acceleration(&self) -> Option<&ResolvedAcceleration> {
        self.acceleration.as_ref()
    }

    fn detected_architecture(&self) -> Option<String> {
        self.model.as_ref().map(|model| match model.family {
            OnnxModelFamily::Moonshine => "moonshine".to_owned(),
            OnnxModelFamily::NemoCtc => "nemo-ctc".to_owned(),
            OnnxModelFamily::Canary => "canary".to_owned(),
            OnnxModelFamily::OfflineTransducer => "offline-transducer".to_owned(),
            OnnxModelFamily::OnlineTransducer => "online-transducer".to_owned(),
        })
    }

    fn runtime_capabilities(family: OnnxModelFamily) -> RuntimeCapabilities {
        RuntimeCapabilities {
            streaming: family == OnnxModelFamily::OnlineTransducer,
            cancellation: true,
            ..RuntimeCapabilities::default()
        }
    }
}

impl SpeechEngine for OnnxSpeechRuntime {
    fn load(&mut self) -> anyhow::Result<()> {
        let model = self
            .model
            .clone()
            .ok_or_else(|| anyhow::anyhow!("no ONNX model is selected"))?;
        let preference = self
            .acceleration
            .as_ref()
            .map(|acceleration| acceleration.requested)
            .unwrap_or(AccelerationPreference::Auto);
        self.load_model(model, preference)?;
        Ok(())
    }

    fn transcribe(
        &mut self,
        audio: &PreparedAudio,
        options: &TranscriptionOptions,
    ) -> anyhow::Result<Transcript> {
        if *options != TranscriptionOptions::default() {
            return Err(anyhow::anyhow!(
                "the isolated ONNX adapter currently accepts only default transcription options"
            ));
        }
        if self.model.is_none() {
            return Err(anyhow::anyhow!("no ONNX model is loaded"));
        }
        let correlation = self.next_id();
        let text = self
            .supervisor
            .transcribe(correlation, correlation, &audio.samples)?;
        Ok(text_only_transcript(text))
    }

    fn capabilities(&self) -> RuntimeCapabilities {
        self.model
            .as_ref()
            .map(|model| Self::runtime_capabilities(model.family))
            .unwrap_or_else(|| RuntimeCapabilities {
                cancellation: true,
                ..RuntimeCapabilities::default()
            })
    }

    fn health_check(&mut self) -> anyhow::Result<()> {
        let correlation = self.next_id();
        self.supervisor.health(correlation, correlation)
    }

    fn cancel(&mut self) -> anyhow::Result<()> {
        self.supervisor.cancel_active()
    }

    fn unload(&mut self) -> anyhow::Result<()> {
        self.supervisor.unload()?;
        self.model = None;
        self.acceleration = None;
        Ok(())
    }
}

impl StreamingSpeechEngine for OnnxSpeechRuntime {
    fn start_stream(
        &mut self,
        options: &TranscriptionOptions,
    ) -> anyhow::Result<Box<dyn SpeechStream>> {
        if *options != TranscriptionOptions::default() {
            return Err(anyhow::anyhow!(
                "the isolated ONNX adapter currently accepts only default transcription options"
            ));
        }
        let model = self
            .model
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("no ONNX model is loaded"))?;
        if model.family != OnnxModelFamily::OnlineTransducer {
            return Err(anyhow::anyhow!(
                "incremental streaming requires an online ONNX transducer"
            ));
        }
        let session_id = self.next_id();
        let request_id = self.next_id();
        let activity_lease = self.runtime_activity.acquire_stream()?;
        self.supervisor.start_stream(session_id, request_id)?;
        Ok(Box::new(OnnxSpeechStream {
            supervisor: Arc::clone(&self.supervisor),
            next_correlation: Arc::clone(&self.next_correlation),
            session_id,
            active: true,
            activity_lease: Some(activity_lease),
        }))
    }
}

struct OnnxSpeechStream {
    supervisor: Arc<dyn OnnxSupervisorControl>,
    next_correlation: Arc<AtomicU64>,
    session_id: u64,
    active: bool,
    activity_lease: Option<RuntimeActivityLease>,
}

impl OnnxSpeechStream {
    fn next_request_id(&self) -> u64 {
        self.next_correlation
            .fetch_add(1, Ordering::AcqRel)
            .wrapping_add(1)
    }
}

impl SpeechStream for OnnxSpeechStream {
    fn push_audio(&mut self, samples: &[f32]) -> anyhow::Result<StreamUpdate> {
        if !self.active {
            return Err(anyhow::anyhow!("the ONNX stream is no longer active"));
        }
        let request_id = self.next_request_id();
        let tentative = self
            .supervisor
            .audio_chunk(self.session_id, request_id, samples)?;
        Ok(StreamUpdate {
            committed: String::new(),
            tentative,
        })
    }

    fn finalize(mut self: Box<Self>) -> anyhow::Result<Transcript> {
        let request_id = self.next_request_id();
        let result = self
            .supervisor
            .end_stream(self.session_id, request_id)
            .map(text_only_transcript);
        if result.is_ok() {
            self.active = false;
            self.activity_lease.take();
        }
        result
    }

    fn cancel(mut self: Box<Self>) -> anyhow::Result<()> {
        let request_id = self.next_request_id();
        let result = self.supervisor.cancel_stream(self.session_id, request_id);
        if result.is_ok() {
            self.active = false;
            self.activity_lease.take();
        }
        result
    }
}

impl Drop for OnnxSpeechStream {
    fn drop(&mut self) {
        if self.active {
            self.supervisor.abandon_stream(self.session_id);
            self.active = false;
        }
        self.activity_lease.take();
    }
}

fn text_only_transcript(text: String) -> Transcript {
    Transcript {
        text,
        segments: Vec::new(),
        detected_language: None,
        duration_ms: None,
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
    ) -> Result<Self, RuntimeError> {
        let package_root =
            model
                .package_root
                .clone()
                .ok_or_else(|| RuntimeError::MissingLegacyPackageRoot {
                    model_id: model.id.clone(),
                    path: model.path.clone(),
                })?;
        let package = NativePackage::from_root(package_root);
        Ok(Self {
            model,
            package,
            acceleration,
            loaded: None,
            last_used_at: None,
            cancel_generation,
            request_cancel_snapshot: None,
        })
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
    #[cfg(test)]
    RUNTIME_MODEL_VERIFICATION_COUNT.with(|count| count.set(count.get() + 1));

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
    if !actual.eq_ignore_ascii_case(&model.expected_sha256) {
        return Err(NativeBootstrapFailure::ModelIntegrity {
            path: model.path.clone(),
            message: format!("expected SHA-256 {}, got {actual}", model.expected_sha256),
        });
    }
    Ok(())
}

fn verify_embedded_runtime_model(
    model: &RuntimeModel,
    warm_reused: bool,
) -> Result<(), NativeBootstrapFailure> {
    if warm_reused {
        Ok(())
    } else {
        verify_runtime_model(model)
    }
}

#[cfg(test)]
thread_local! {
    static RUNTIME_MODEL_VERIFICATION_COUNT: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
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

    struct TestOnnxControl {
        loads: AtomicU64,
        unloads: AtomicU64,
        fail_unload: bool,
        events: Arc<Mutex<Vec<&'static str>>>,
    }

    impl OnnxSupervisorControl for TestOnnxControl {
        fn load(
            &self,
            _session_id: u64,
            _request_id: u64,
            _model: OnnxModelSpec,
        ) -> anyhow::Result<bool> {
            self.loads.fetch_add(1, Ordering::AcqRel);
            self.events.lock().unwrap().push("load:onnx");
            Ok(false)
        }

        fn transcribe(
            &self,
            _session_id: u64,
            _request_id: u64,
            _samples: &[f32],
        ) -> anyhow::Result<String> {
            unreachable!("the cleanup test does not transcribe")
        }

        fn start_stream(&self, _session_id: u64, _request_id: u64) -> anyhow::Result<()> {
            unreachable!("the cleanup test does not start a native stream")
        }

        fn audio_chunk(
            &self,
            _session_id: u64,
            _request_id: u64,
            _samples: &[f32],
        ) -> anyhow::Result<String> {
            unreachable!("the cleanup test does not send native audio")
        }

        fn end_stream(&self, _session_id: u64, _request_id: u64) -> anyhow::Result<String> {
            unreachable!("the cleanup test does not finalize a native stream")
        }

        fn cancel_stream(&self, _session_id: u64, _request_id: u64) -> anyhow::Result<()> {
            unreachable!("the cleanup test invalidates leases directly")
        }

        fn health(&self, _session_id: u64, _request_id: u64) -> anyhow::Result<()> {
            unreachable!("the cleanup test does not run health")
        }

        fn unload(&self) -> anyhow::Result<()> {
            self.unloads.fetch_add(1, Ordering::AcqRel);
            self.events.lock().unwrap().push("unload:onnx");
            if self.fail_unload {
                anyhow::bail!("deterministic unload failure");
            }
            Ok(())
        }

        fn cancel_active(&self) -> anyhow::Result<()> {
            Ok(())
        }

        fn abandon_stream(&self, _session_id: u64) {}
    }

    #[derive(Default)]
    struct FailingStreamState {
        active_session: Option<u64>,
        started_sessions: Vec<u64>,
        abandon_calls: usize,
        end_calls: usize,
        cancel_calls: usize,
        fail_next_end: bool,
        fail_next_cancel: bool,
    }

    #[derive(Default)]
    struct FailingStreamControl {
        state: Mutex<FailingStreamState>,
    }

    impl FailingStreamControl {
        fn fail_next_end(&self) {
            self.state.lock().unwrap().fail_next_end = true;
        }

        fn fail_next_cancel(&self) {
            self.state.lock().unwrap().fail_next_cancel = true;
        }

        fn snapshot(&self) -> (Option<u64>, Vec<u64>, usize, usize, usize) {
            let state = self.state.lock().unwrap();
            (
                state.active_session,
                state.started_sessions.clone(),
                state.abandon_calls,
                state.end_calls,
                state.cancel_calls,
            )
        }
    }

    impl OnnxSupervisorControl for FailingStreamControl {
        fn load(
            &self,
            _session_id: u64,
            _request_id: u64,
            _model: OnnxModelSpec,
        ) -> anyhow::Result<bool> {
            Ok(false)
        }

        fn transcribe(
            &self,
            _session_id: u64,
            _request_id: u64,
            _samples: &[f32],
        ) -> anyhow::Result<String> {
            anyhow::bail!("stream-only fake does not transcribe")
        }

        fn start_stream(&self, session_id: u64, _request_id: u64) -> anyhow::Result<()> {
            let mut state = self.state.lock().unwrap();
            if state.active_session.is_some() {
                anyhow::bail!("fake stream already active")
            }
            state.active_session = Some(session_id);
            state.started_sessions.push(session_id);
            Ok(())
        }

        fn audio_chunk(
            &self,
            _session_id: u64,
            _request_id: u64,
            _samples: &[f32],
        ) -> anyhow::Result<String> {
            Ok(String::new())
        }

        fn end_stream(&self, session_id: u64, _request_id: u64) -> anyhow::Result<String> {
            let mut state = self.state.lock().unwrap();
            state.end_calls += 1;
            if state.active_session != Some(session_id) {
                anyhow::bail!("fake stream session mismatch")
            }
            if std::mem::take(&mut state.fail_next_end) {
                anyhow::bail!("deterministic end-stream failure")
            }
            state.active_session = None;
            Ok("final".to_owned())
        }

        fn cancel_stream(&self, session_id: u64, _request_id: u64) -> anyhow::Result<()> {
            let mut state = self.state.lock().unwrap();
            state.cancel_calls += 1;
            if state.active_session != Some(session_id) {
                anyhow::bail!("fake stream session mismatch")
            }
            if std::mem::take(&mut state.fail_next_cancel) {
                anyhow::bail!("deterministic cancel-stream failure")
            }
            state.active_session = None;
            Ok(())
        }

        fn health(&self, _session_id: u64, _request_id: u64) -> anyhow::Result<()> {
            Ok(())
        }

        fn unload(&self) -> anyhow::Result<()> {
            self.state.lock().unwrap().active_session = None;
            Ok(())
        }

        fn cancel_active(&self) -> anyhow::Result<()> {
            Ok(())
        }

        fn abandon_stream(&self, session_id: u64) {
            let mut state = self.state.lock().unwrap();
            state.abandon_calls += 1;
            if state.active_session == Some(session_id) {
                state.active_session = None;
            }
        }
    }

    fn online_stream_runtime(
        supervisor: Arc<dyn OnnxSupervisorControl>,
        runtime_activity: RuntimeActivity,
    ) -> OnnxSpeechRuntime {
        let mut runtime =
            OnnxSpeechRuntime::new(supervisor, Arc::new(AtomicU64::new(0)), runtime_activity);
        runtime
            .load_model(
                OnnxModelSpec {
                    id: "stream-test".to_owned(),
                    root: PathBuf::from("."),
                    family: OnnxModelFamily::OnlineTransducer,
                    files: std::collections::BTreeMap::new(),
                    num_threads: 1,
                },
                AccelerationPreference::Cpu,
            )
            .unwrap();
        runtime
    }

    #[test]
    fn failed_stream_finalize_abandons_once_releases_lease_and_allows_recovery() {
        let control = Arc::new(FailingStreamControl::default());
        control.fail_next_end();
        let activity = RuntimeActivity::default();
        let mut runtime = online_stream_runtime(
            Arc::clone(&control) as Arc<dyn OnnxSupervisorControl>,
            activity.clone(),
        );

        let stream =
            StreamingSpeechEngine::start_stream(&mut runtime, &TranscriptionOptions::default())
                .unwrap();
        let error = stream.finalize().unwrap_err();
        assert!(
            error
                .to_string()
                .contains("deterministic end-stream failure")
        );
        let (active_session, started_sessions, abandon_calls, end_calls, cancel_calls) =
            control.snapshot();
        assert_eq!(active_session, None);
        assert_eq!(started_sessions.len(), 1);
        assert_eq!(abandon_calls, 1);
        assert_eq!(end_calls, 1);
        assert_eq!(cancel_calls, 0);
        assert_eq!(activity.active_streams(), 0);

        let recovered =
            StreamingSpeechEngine::start_stream(&mut runtime, &TranscriptionOptions::default())
                .unwrap();
        assert_eq!(recovered.finalize().unwrap().text, "final");
        let (active_session, started_sessions, abandon_calls, end_calls, cancel_calls) =
            control.snapshot();
        assert_eq!(active_session, None);
        assert_eq!(started_sessions.len(), 2);
        assert_ne!(started_sessions[0], started_sessions[1]);
        assert_eq!(abandon_calls, 1);
        assert_eq!(end_calls, 2);
        assert_eq!(cancel_calls, 0);
        assert_eq!(activity.active_streams(), 0);
    }

    #[test]
    fn failed_stream_cancel_abandons_once_releases_lease_and_allows_recovery() {
        let control = Arc::new(FailingStreamControl::default());
        control.fail_next_cancel();
        let activity = RuntimeActivity::default();
        let mut runtime = online_stream_runtime(
            Arc::clone(&control) as Arc<dyn OnnxSupervisorControl>,
            activity.clone(),
        );

        let stream =
            StreamingSpeechEngine::start_stream(&mut runtime, &TranscriptionOptions::default())
                .unwrap();
        let error = stream.cancel().unwrap_err();
        assert!(
            error
                .to_string()
                .contains("deterministic cancel-stream failure")
        );
        let (active_session, started_sessions, abandon_calls, end_calls, cancel_calls) =
            control.snapshot();
        assert_eq!(active_session, None);
        assert_eq!(started_sessions.len(), 1);
        assert_eq!(abandon_calls, 1);
        assert_eq!(end_calls, 0);
        assert_eq!(cancel_calls, 1);
        assert_eq!(activity.active_streams(), 0);

        let recovered =
            StreamingSpeechEngine::start_stream(&mut runtime, &TranscriptionOptions::default())
                .unwrap();
        recovered.cancel().unwrap();
        let (active_session, started_sessions, abandon_calls, end_calls, cancel_calls) =
            control.snapshot();
        assert_eq!(active_session, None);
        assert_eq!(started_sessions.len(), 2);
        assert_ne!(started_sessions[0], started_sessions[1]);
        assert_eq!(abandon_calls, 1);
        assert_eq!(end_calls, 0);
        assert_eq!(cancel_calls, 2);
        assert_eq!(activity.active_streams(), 0);
    }

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
    fn heavy_runtime_ownership_orders_bidirectional_switches_and_never_exceeds_one() {
        let mut ownership = HeavyRuntimeOwnership::default();
        let mut heavy_count = 1_usize;
        let mut maximum_heavy_count = heavy_count;
        let mut events = Vec::new();
        ownership.activate(HeavyRuntimeOwner::EmbeddedGguf);

        for requested in [
            HeavyRuntimeOwner::OnnxSpeech,
            HeavyRuntimeOwner::LegacyCompatibility,
            HeavyRuntimeOwner::OnnxSpeech,
            HeavyRuntimeOwner::EmbeddedGguf,
        ] {
            ownership
                .transition(requested, |current| {
                    assert_eq!(heavy_count, 1);
                    events.push(format!("unload:{current:?}"));
                    heavy_count -= 1;
                    Ok(())
                })
                .unwrap();
            assert_eq!(heavy_count, 0, "the previous runtime must unload first");
            events.push(format!("load:{requested:?}"));
            heavy_count += 1;
            maximum_heavy_count = maximum_heavy_count.max(heavy_count);
            ownership.activate(requested);
        }

        ownership
            .transition(HeavyRuntimeOwner::EmbeddedGguf, |_| {
                panic!("same-runtime warm reuse must not unload")
            })
            .unwrap();
        assert_eq!(maximum_heavy_count, 1);
        assert_eq!(heavy_count, 1);
        assert_eq!(
            events,
            [
                "unload:EmbeddedGguf",
                "load:OnnxSpeech",
                "unload:OnnxSpeech",
                "load:LegacyCompatibility",
                "unload:LegacyCompatibility",
                "load:OnnxSpeech",
                "unload:OnnxSpeech",
                "load:EmbeddedGguf",
            ]
        );
    }

    #[test]
    fn failed_heavy_runtime_unload_blocks_next_load_and_preserves_owner() {
        let mut ownership = HeavyRuntimeOwnership::default();
        ownership.activate(HeavyRuntimeOwner::OnnxSpeech);

        let error = ownership
            .transition(HeavyRuntimeOwner::EmbeddedGguf, |current| {
                assert_eq!(current, HeavyRuntimeOwner::OnnxSpeech);
                Err(RuntimeError::Engine(
                    "deterministic unload failure".to_owned(),
                ))
            })
            .unwrap_err();

        assert!(error.to_string().contains("deterministic unload failure"));
        assert_eq!(ownership.current, Some(HeavyRuntimeOwner::OnnxSpeech));
    }

    #[test]
    fn router_state_switches_gguf_onnx_gguf_and_clears_independent_cancel_handles() {
        let embedded_cancellation = Arc::new(Mutex::new(None));
        let onnx_cancellation = Arc::new(Mutex::new(None));
        let activity = RuntimeActivity::default();
        let events = Arc::new(Mutex::new(vec!["load:gguf"]));
        let control = Arc::new(TestOnnxControl {
            loads: AtomicU64::new(0),
            unloads: AtomicU64::new(0),
            fail_unload: false,
            events: Arc::clone(&events),
        });
        let factory: Arc<OnnxSupervisorFactory> = {
            let control = Arc::clone(&control);
            Arc::new(move || Ok(Arc::clone(&control) as Arc<dyn OnnxSupervisorControl>))
        };
        let model = RuntimeModel {
            id: ModelId::new("test-gguf"),
            path: PathBuf::from("test.gguf"),
            format: ArtifactFormat::Gguf,
            package_root: None,
            expected_size_bytes: 1,
            expected_sha256: "0".repeat(64),
        };
        let mut state = RouterState::default();
        let embedded = EmbeddedRuntime::new(model.path.clone(), AccelerationPreference::Cpu);
        *embedded_cancellation.lock().unwrap() = Some(embedded.cancellation_handle());
        state.embedded = Some(embedded);
        state.embedded_model = Some(model.clone());
        state
            .heavy_ownership
            .activate(HeavyRuntimeOwner::EmbeddedGguf);

        state
            .prepare_heavy_runtime(
                HeavyRuntimeOwner::OnnxSpeech,
                &embedded_cancellation,
                &onnx_cancellation,
                &activity,
            )
            .unwrap();
        assert!(state.embedded.is_none());
        assert!(embedded_cancellation.lock().unwrap().is_none());
        assert_eq!(state.heavy_ownership.current, None);
        events.lock().unwrap().push("unload:gguf");
        let spec = OnnxModelSpec {
            id: "test-onnx".to_owned(),
            root: PathBuf::from("."),
            family: OnnxModelFamily::NemoCtc,
            files: std::collections::BTreeMap::new(),
            num_threads: 1,
        };
        state
            .load_onnx(
                spec,
                AccelerationPreference::Cpu,
                factory,
                Arc::clone(&onnx_cancellation),
                Arc::new(AtomicU64::new(0)),
                activity.clone(),
            )
            .unwrap();
        assert!(state.onnx.is_some());
        assert!(onnx_cancellation.lock().unwrap().is_some());
        assert_eq!(
            state.heavy_ownership.current,
            Some(HeavyRuntimeOwner::OnnxSpeech)
        );
        assert_eq!(
            usize::from(state.embedded.is_some())
                + usize::from(state.transcribe_cpp.is_some())
                + usize::from(state.onnx.is_some()),
            1
        );

        state
            .prepare_heavy_runtime(
                HeavyRuntimeOwner::EmbeddedGguf,
                &embedded_cancellation,
                &onnx_cancellation,
                &activity,
            )
            .unwrap();
        assert!(state.onnx.is_none());
        assert!(onnx_cancellation.lock().unwrap().is_none());
        assert_eq!(control.loads.load(Ordering::Acquire), 1);
        assert_eq!(control.unloads.load(Ordering::Acquire), 1);
        assert_eq!(state.heavy_ownership.current, None);

        let embedded = EmbeddedRuntime::new(model.path.clone(), AccelerationPreference::Cpu);
        *embedded_cancellation.lock().unwrap() = Some(embedded.cancellation_handle());
        state.embedded = Some(embedded);
        state.embedded_model = Some(model);
        state
            .heavy_ownership
            .activate(HeavyRuntimeOwner::EmbeddedGguf);
        events.lock().unwrap().push("load:gguf");
        assert_eq!(
            usize::from(state.embedded.is_some())
                + usize::from(state.transcribe_cpp.is_some())
                + usize::from(state.onnx.is_some()),
            1
        );
        assert_eq!(
            *events.lock().unwrap(),
            [
                "load:gguf",
                "unload:gguf",
                "load:onnx",
                "unload:onnx",
                "load:gguf",
            ]
        );
    }

    #[test]
    fn idle_timeout_uses_last_successful_activity_and_unloads_only_when_expired() {
        let activity = RuntimeActivity::default();
        let base = Instant::now();
        activity.force_release_streams_at(base);
        assert_eq!(WARM_MODEL_TTL, Duration::from_secs(5 * 60));

        let mut failed_request = activity.acquire_request().unwrap();
        assert_eq!(
            activity.timeout_action_at(
                base + WARM_MODEL_TTL - Duration::from_secs(1),
                WARM_MODEL_TTL,
            ),
            IdleTimeoutAction::Defer(WARM_MODEL_TTL)
        );
        failed_request.release_at(base + WARM_MODEL_TTL - Duration::from_secs(1));
        assert_eq!(
            activity.timeout_action_at(
                base + WARM_MODEL_TTL - Duration::from_secs(1),
                WARM_MODEL_TTL,
            ),
            IdleTimeoutAction::Defer(Duration::from_secs(1)),
            "a failed request must retain the original remaining idle deadline"
        );
        assert_eq!(
            activity.timeout_action_at(base + WARM_MODEL_TTL, WARM_MODEL_TTL),
            IdleTimeoutAction::Unload,
            "the next timeout must expire from the last successful use"
        );

        let success_at = base + WARM_MODEL_TTL * 3;
        let mut successful_request = activity.acquire_request().unwrap();
        successful_request.complete_successfully_at(success_at);
        assert_eq!(
            activity.timeout_action_at(
                success_at + WARM_MODEL_TTL - Duration::from_millis(1),
                WARM_MODEL_TTL,
            ),
            IdleTimeoutAction::Defer(Duration::from_millis(1))
        );
        assert_eq!(
            activity.timeout_action_at(success_at + WARM_MODEL_TTL, WARM_MODEL_TTL),
            IdleTimeoutAction::Unload
        );
    }

    #[test]
    fn active_stream_and_request_defer_idle_unload_until_full_idle_interval() {
        let activity = RuntimeActivity::default();
        let base = Instant::now();
        activity.force_release_streams_at(base);
        let mut stream = activity.acquire_stream().unwrap();
        let mut request = activity.acquire_request().unwrap();
        let long_after_expiry = base + WARM_MODEL_TTL * 4;

        assert_eq!(
            activity.timeout_action_at(long_after_expiry, WARM_MODEL_TTL),
            IdleTimeoutAction::Defer(WARM_MODEL_TTL)
        );
        request.release_at(long_after_expiry);
        assert_eq!(
            activity.timeout_action_at(long_after_expiry, WARM_MODEL_TTL),
            IdleTimeoutAction::Defer(WARM_MODEL_TTL)
        );
        stream.release_at(long_after_expiry);
        assert_eq!(
            activity.timeout_action_at(
                long_after_expiry + WARM_MODEL_TTL - Duration::from_millis(1),
                WARM_MODEL_TTL,
            ),
            IdleTimeoutAction::Defer(Duration::from_millis(1))
        );
        assert_eq!(
            activity.timeout_action_at(long_after_expiry + WARM_MODEL_TTL, WARM_MODEL_TTL),
            IdleTimeoutAction::Unload
        );
    }

    #[test]
    fn unload_all_cleans_every_onnx_handle_and_lease_while_preserving_first_error() {
        let control = Arc::new(TestOnnxControl {
            loads: AtomicU64::new(0),
            unloads: AtomicU64::new(0),
            fail_unload: true,
            events: Arc::new(Mutex::new(Vec::new())),
        });
        let router = RuntimeRouter::with_test_onnx_factory({
            let control = Arc::clone(&control);
            move || Ok(Arc::clone(&control) as Arc<dyn OnnxSupervisorControl>)
        });
        let supervisor = Arc::clone(&control) as Arc<dyn OnnxSupervisorControl>;
        {
            let mut state = router.inner.lock().unwrap();
            state.onnx = Some(OnnxSpeechRuntime::new(
                Arc::clone(&supervisor),
                Arc::new(AtomicU64::new(0)),
                router.runtime_activity(),
            ));
            state
                .heavy_ownership
                .activate(HeavyRuntimeOwner::OnnxSpeech);
        }
        *router.onnx_cancellation.lock().unwrap() = Some(supervisor);
        let stale_stream_lease = router.runtime_activity.acquire_stream().unwrap();

        let error = router.unload_all().unwrap_err();

        assert!(error.to_string().contains("deterministic unload failure"));
        assert_eq!(control.unloads.load(Ordering::Acquire), 1);
        assert!(router.inner.lock().unwrap().onnx.is_none());
        assert_eq!(router.inner.lock().unwrap().heavy_ownership.current, None);
        assert!(router.onnx_cancellation.lock().unwrap().is_none());
        assert_eq!(router.runtime_activity.active_streams(), 0);
        drop(stale_stream_lease);
        assert_eq!(router.runtime_activity.active_streams(), 0);
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
    fn safe_gguf_catalog_artifacts_do_not_require_a_managed_runtime_package() {
        let router = RuntimeRouter::new();

        for id in [
            "whisper_cpp_tiny_en",
            "whisper_cpp_base_en",
            "whisper_cpp_small_en",
            "whisper_cpp_medium_en",
        ] {
            assert_eq!(router.managed_runtime_id(&ModelId::new(id)), None);
        }
    }

    #[test]
    fn embedded_model_verification_runs_for_cold_or_changed_paths_but_not_warm_reuse() {
        let suffix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let first_path = std::env::temp_dir().join(format!(
            "scribe-embedded-verification-{}-{suffix}-first.gguf",
            std::process::id()
        ));
        let second_path = std::env::temp_dir().join(format!(
            "scribe-embedded-verification-{}-{suffix}-second.gguf",
            std::process::id()
        ));
        std::fs::write(&first_path, b"first verified model").unwrap();
        std::fs::write(&second_path, b"second verified model").unwrap();
        let first = RuntimeModel {
            id: ModelId::new("first"),
            path: first_path.clone(),
            format: ArtifactFormat::Gguf,
            package_root: None,
            expected_size_bytes: std::fs::metadata(&first_path).unwrap().len(),
            expected_sha256: sha256_file(&first_path).unwrap(),
        };
        let second = RuntimeModel {
            id: ModelId::new("second"),
            path: second_path.clone(),
            format: ArtifactFormat::Gguf,
            package_root: None,
            expected_size_bytes: std::fs::metadata(&second_path).unwrap().len(),
            expected_sha256: sha256_file(&second_path).unwrap(),
        };
        RUNTIME_MODEL_VERIFICATION_COUNT.with(|count| count.set(0));

        let cold = embedded_request_is_warm(
            Some(&first),
            Some((first.path.as_path(), AccelerationPreference::Auto, false)),
            &first,
            AccelerationPreference::Auto,
        );
        assert!(!cold);
        verify_embedded_runtime_model(&first, cold).unwrap();

        let warm = embedded_request_is_warm(
            Some(&first),
            Some((first.path.as_path(), AccelerationPreference::Auto, true)),
            &first,
            AccelerationPreference::Auto,
        );
        assert!(warm);
        verify_embedded_runtime_model(&first, warm).unwrap();

        let changed_path = embedded_request_is_warm(
            Some(&first),
            Some((first.path.as_path(), AccelerationPreference::Auto, true)),
            &second,
            AccelerationPreference::Auto,
        );
        assert!(!changed_path);
        verify_embedded_runtime_model(&second, changed_path).unwrap();

        RUNTIME_MODEL_VERIFICATION_COUNT.with(|count| assert_eq!(count.get(), 2));
        std::fs::remove_file(first_path).unwrap();
        std::fs::remove_file(second_path).unwrap();
    }

    #[test]
    fn embedded_preload_verification_failure_preserves_only_the_prior_identity() {
        let missing = std::env::temp_dir().join(format!(
            "scribe-missing-changed-gguf-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let prior = RuntimeModel {
            id: ModelId::new("prior-gguf"),
            path: PathBuf::from("prior.gguf"),
            format: ArtifactFormat::Gguf,
            package_root: None,
            expected_size_bytes: 1,
            expected_sha256: "0".repeat(64),
        };
        let changed = RuntimeModel {
            id: ModelId::new("changed-gguf"),
            path: missing,
            format: ArtifactFormat::Gguf,
            package_root: None,
            expected_size_bytes: 1,
            expected_sha256: "1".repeat(64),
        };
        let cancellation = Arc::new(Mutex::new(None));
        let mut state = RouterState::default();
        let embedded = EmbeddedRuntime::new(prior.path.clone(), AccelerationPreference::Cpu);
        *cancellation.lock().unwrap() = Some(embedded.cancellation_handle());
        state.embedded = Some(embedded);
        state.embedded_model = Some(prior.clone());
        state
            .heavy_ownership
            .activate(HeavyRuntimeOwner::EmbeddedGguf);

        let error = state
            .load_embedded(
                changed,
                AccelerationPreference::Cpu,
                Arc::clone(&cancellation),
            )
            .unwrap_err();

        assert!(matches!(
            error,
            RuntimeError::Bootstrap(NativeBootstrapFailure::ModelIntegrity { .. })
        ));
        assert_eq!(state.embedded_model.as_ref(), Some(&prior));
        assert_eq!(
            state.embedded.as_ref().map(EmbeddedRuntime::model_path),
            Some(prior.path.as_path())
        );
        assert_eq!(
            state.heavy_ownership.current,
            Some(HeavyRuntimeOwner::EmbeddedGguf)
        );
        assert!(cancellation.lock().unwrap().is_some());
    }

    #[test]
    fn trusted_gguf_format_routes_staged_and_final_paths_to_the_embedded_runtime() {
        for path in [
            "whisper-base.en-Q8_0.gguf.partial",
            "whisper-base.en-Q8_0.gguf",
        ] {
            let model = RuntimeModel {
                id: ModelId::new("trusted-gguf"),
                path: PathBuf::from(path),
                format: ArtifactFormat::Gguf,
                package_root: None,
                expected_size_bytes: 1,
                expected_sha256: "0".repeat(64),
            };

            assert!(is_gguf_model(&model), "{path}");
            assert!(matches!(
                runtime_kind_for_runtime_model(&model),
                Some(RuntimeKind::TranscribeCpp)
            ));
        }
    }

    #[test]
    fn existing_runtime_models_convert_to_the_same_typed_routes() {
        let gguf = RuntimeModel {
            id: ModelId::new("trusted-gguf"),
            path: PathBuf::from("model.gguf"),
            format: ArtifactFormat::Gguf,
            package_root: None,
            expected_size_bytes: 1,
            expected_sha256: "0".repeat(64),
        };
        let legacy = RuntimeModel {
            id: ModelId::new("whisper_cpp_base_en"),
            path: PathBuf::from("ggml-base.en.bin"),
            format: ArtifactFormat::LegacyGgml,
            package_root: Some(PathBuf::from("runtime")),
            expected_size_bytes: 1,
            expected_sha256: "0".repeat(64),
        };

        assert!(matches!(
            RuntimeArtifact::from(gguf),
            RuntimeArtifact::Gguf(_)
        ));
        assert!(matches!(
            RuntimeArtifact::from(legacy),
            RuntimeArtifact::LegacyCompatibility(_)
        ));
    }

    #[test]
    fn rootless_legacy_ggml_returns_an_actionable_typed_error() {
        let model = RuntimeModel {
            id: ModelId::new("whisper_cpp_base_en"),
            path: PathBuf::from("ggml-base.en.bin"),
            format: ArtifactFormat::LegacyGgml,
            package_root: None,
            expected_size_bytes: 1,
            expected_sha256: "0".repeat(64),
        };

        let Err(error) = TranscribeCppRuntime::new(
            model,
            resolve_acceleration(AccelerationPreference::Cpu).unwrap(),
            Arc::new(AtomicU64::new(0)),
        ) else {
            panic!("rootless legacy GGML must fail before runtime construction");
        };
        assert!(matches!(
            error,
            RuntimeError::MissingLegacyPackageRoot { .. }
        ));
        assert!(error.to_string().contains("install or repair"));
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
                .is_some_and(|name| name == "runtime_router.rs" || name == "architecture_guard.rs")
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
            if path
                .file_name()
                .is_some_and(|name| name == "architecture_guard.rs")
            {
                continue;
            }
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
            if path
                .file_name()
                .is_some_and(|name| name == "architecture_guard.rs")
            {
                continue;
            }
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
