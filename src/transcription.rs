//! Runtime-neutral transcription contracts for static GGUF and native ONNX.
//!
//! Application code should depend on [`TranscriptionService`] and the types in
//! this module rather than on a concrete STT backend. The current adapters are
//! deliberately kept behind one private batch bridge until a later phase
//! replaces them with the consolidated runtime implementation.

// Phase 1 establishes the complete stable contract before native streaming,
// lifecycle wiring, and capability UI are introduced in later phases.
use std::fmt;
#[cfg(test)]
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, sync_channel};
use std::sync::{Arc, Mutex, TryLockError};
use std::time::{Duration, Instant};

#[cfg(test)]
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};

use crate::config::{self, AppConfig};
use crate::installations::InstallCancellation;
use crate::model_catalog::{
    ArtifactFormat, model_descriptor, normal_model_descriptors, runtime_artifact_manifest_for_path,
};
pub use crate::model_catalog::{CompatibilityStatus, ModelDescriptor};
use crate::models::SttModelInfo;
#[cfg(test)]
use crate::onnx_model_bundles::OnnxBundleManifest;
use crate::onnx_worker::InferenceWorkerRegistry;
#[cfg(test)]
use crate::onnx_worker::InferenceWorkerSupervisor;
use crate::prepared_audio::{PREPARED_SAMPLE_RATE, PreparedAudio};
use crate::runtime_artifact::{OnnxModelSpec, RuntimeArtifact, RuntimeModel};
use crate::runtime_contract::{
    RuntimeError, RuntimeExecution, RuntimeLoadExecution, WARM_MODEL_TTL,
};
#[cfg(test)]
use crate::runtime_router::IdleTimeoutAction;
#[cfg(test)]
use crate::runtime_router::RuntimeRouter;
use crate::streaming::{
    HypothesisWord, PreviewAudioPublisher, PreviewEvent, RollingPreviewSession, StreamIdentity,
    TranscriptHypothesis, TranscriptStabilizer,
};

const INSTALL_SMOKE_TIMEOUT: Duration = Duration::from_secs(120);
const INSTALL_SMOKE_POLL_INTERVAL: Duration = Duration::from_millis(25);
const INSTALL_SMOKE_HELPER_FLAG: &str = "--scribe-install-smoke";
const INSTALL_SMOKE_PARENT_FLAG: &str = "--scribe-install-smoke-parent";
const DROP_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);
const SHUTDOWN_POLL_INTERVAL: Duration = Duration::from_millis(1);

fn preserve_primary_smoke_error<T>(primary: Result<T>, cleanup: Result<()>) -> Result<T> {
    match (primary, cleanup) {
        (Err(primary), _) => Err(primary),
        (Ok(_), Err(cleanup)) => Err(cleanup),
        (Ok(value), Ok(())) => Ok(value),
    }
}

/// Identifies one user dictation session.
///
/// The application allocates monotonically increasing values. The service only
/// carries the value through its outcome so callers can reject stale work.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash)]
pub struct SessionId(pub u64);

/// Identifies one transcription request within or across sessions.
///
/// The application allocates monotonically increasing values. The service only
/// carries the value through its outcome so callers can reject stale work.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash)]
pub struct RequestId(pub u64);

/// Application-facing acceleration preference.
///
/// Concrete accelerator APIs and runtime-specific device names stay below the
/// service boundary. `Auto` is resolved by the selected runtime's health
/// check and the resolved choice is returned in diagnostics.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccelerationPreference {
    #[default]
    Auto,
    Cpu,
    #[serde(alias = "cuda", alias = "prefer_gpu")]
    Gpu,
}

impl AccelerationPreference {
    #[cfg(test)]
    pub const ALL: [Self; 3] = [Self::Auto, Self::Gpu, Self::Cpu];

    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "Auto",
            Self::Cpu => "CPU only",
            Self::Gpu => "GPU",
        }
    }
}

/// Runtime-neutral compute device selected for one request.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ComputeDevice {
    Cpu,
    Gpu { name: String },
}

impl ComputeDevice {
    pub fn label(&self) -> &str {
        match self {
            Self::Cpu => "CPU",
            Self::Gpu { name } => name,
        }
    }
}

/// Observable result of resolving an acceleration preference.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ResolvedAcceleration {
    pub requested: AccelerationPreference,
    pub resolved: ComputeDevice,
    /// Explains an automatic fallback or other material resolution decision.
    pub diagnostic: Option<String>,
    /// Typed GGUF selection details. Older receipts and CPU-only runtimes omit
    /// this field and continue to deserialize without migration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) selection: Option<crate::backend_policy::BackendSelection>,
}

/// A runtime-neutral reference to a configured model catalog entry.
#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ModelId(String);

impl ModelId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_inner(self) -> String {
        self.0
    }
}

impl AsRef<str> for ModelId {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl fmt::Display for ModelId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl From<String> for ModelId {
    fn from(value: String) -> Self {
        Self::new(value)
    }
}

impl From<&str> for ModelId {
    fn from(value: &str) -> Self {
        Self::new(value)
    }
}

/// Normalized final transcript returned by a speech engine.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Transcript {
    pub text: String,
    pub segments: Vec<TranscriptSegment>,
    /// `None` means the selected legacy backend did not report a language.
    pub detected_language: Option<String>,
    /// `None` means the selected runtime did not report audio-timeline
    /// duration. Phase 1 legacy adapters report only decode wall-clock time,
    /// which is retained separately on [`TranscriptionOutcome`].
    pub duration_ms: Option<u128>,
}

/// A portion of a normalized transcript.
///
/// Timing and confidence are optional because the current command-line
/// adapters do not consistently provide them for every configured backend.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TranscriptSegment {
    pub text: String,
    pub start_ms: Option<u64>,
    pub end_ms: Option<u64>,
    pub confidence: Option<f32>,
}

fn transcript_hypothesis(
    identity: StreamIdentity,
    window_start_frame: u64,
    window_end_frame: u64,
    transcript: &Transcript,
    preview_options: PreviewDecodeOptions,
) -> TranscriptHypothesis {
    let window_frames = window_end_frame.saturating_sub(window_start_frame);
    let mut words = Vec::new();
    for segment in &transcript.segments {
        let displays = segment.text.split_whitespace().collect::<Vec<_>>();
        if displays.is_empty() {
            continue;
        }
        let timed_span = preview_options
            .use_segment_timestamps
            .then(|| segment.start_ms.zip(segment.end_ms))
            .flatten()
            .and_then(|(start, end)| {
                let frames_per_ms = u64::from(PREPARED_SAMPLE_RATE) / 1_000;
                let start_frame = start.saturating_mul(frames_per_ms).min(window_frames);
                let end_frame = end.saturating_mul(frames_per_ms).min(window_frames);
                (end_frame > start_frame).then_some((start_frame, end_frame))
            });
        for (index, display) in displays.iter().enumerate() {
            let mut word = HypothesisWord::new(*display);
            if let Some((start_frame, end_frame)) = timed_span {
                let count = displays.len() as u64;
                let span = end_frame - start_frame;
                let word_start = start_frame + span.saturating_mul(index as u64) / count;
                let word_end = start_frame + span.saturating_mul(index as u64 + 1) / count;
                word = word.at_absolute_frames(
                    window_start_frame.saturating_add(word_start),
                    window_start_frame.saturating_add(word_end),
                );
            }
            words.push(word);
        }
    }
    if words.is_empty() {
        return TranscriptHypothesis::from_text(
            identity,
            window_start_frame,
            window_end_frame,
            &transcript.text,
        );
    }
    TranscriptHypothesis {
        identity,
        window_start_frame,
        window_end_frame,
        words,
    }
}

/// Caller-selected decoding behavior.
///
///
/// Phase 1 represents the options needed by the future common contract, but
/// the legacy command-line route only accepts its default behavior. The
/// service rejects an unsupported non-default option instead of ignoring it.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct TranscriptionOptions {
    pub language: Option<String>,
    pub translate_to_english: bool,
    pub enable_timestamps: bool,
    pub initial_prompt: Option<String>,
}

/// Internal rolling-preview policy. It is converted to decoder options only
/// inside the preview worker, leaving caller-facing final transcription
/// options and their default behavior unchanged.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct PreviewDecodeOptions {
    use_segment_timestamps: bool,
}

impl PreviewDecodeOptions {
    pub(crate) fn for_capabilities(capabilities: &RuntimeCapabilities) -> Self {
        Self {
            use_segment_timestamps: capabilities.timestamps,
        }
    }

    pub(crate) fn transcription_options(self) -> TranscriptionOptions {
        TranscriptionOptions {
            enable_timestamps: self.use_segment_timestamps,
            ..TranscriptionOptions::default()
        }
    }
}

/// Features that the selected model/backend can currently expose.
///
/// `timestamps` means final results may include timestamp metadata; it does
/// not mean that the Phase 1 legacy bridge can enable timestamps on request.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct RuntimeCapabilities {
    pub streaming: bool,
    pub cancellation: bool,
    pub translation: bool,
    pub timestamps: bool,
    pub language_detection: bool,
    pub confidence_scores: bool,
    pub custom_vocabulary: bool,
    /// Empty until a backend's language support is verified through this
    /// common contract rather than inferred from catalog prose.
    pub supported_languages: Vec<String>,
}

/// A fully staged model artifact that has not yet been activated.
/// Concrete runtime selection remains private to the router used by the
/// service's dedicated verification worker.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct InstallationCandidate {
    pub(crate) model_id: ModelId,
    pub(crate) model_path: PathBuf,
    pub(crate) artifact_format: ArtifactFormat,
    pub(crate) expected_size_bytes: u64,
    pub(crate) expected_sha256: String,
}

impl InstallationCandidate {
    #[cfg(test)]
    pub(crate) fn normalized(model_id: ModelId, model_path: PathBuf) -> Result<Self> {
        let manifest =
            runtime_artifact_manifest_for_path(&model_id, &model_path).ok_or_else(|| {
                anyhow!(
                    "model {model_id} has no pinned artifact manifest for {}",
                    model_path.display()
                )
            })?;
        Ok(Self {
            model_id,
            model_path,
            artifact_format: manifest.format,
            expected_size_bytes: manifest.size_bytes,
            expected_sha256: manifest.sha256.to_owned(),
        })
    }

    pub(crate) fn pinned(
        model_id: ModelId,
        model_path: PathBuf,
        artifact_format: ArtifactFormat,
        expected_size_bytes: u64,
        expected_sha256: String,
    ) -> Self {
        Self {
            model_id,
            model_path,
            artifact_format,
            expected_size_bytes,
            expected_sha256,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct InstallSmoke {
    pub(crate) resolved_acceleration: ResolvedAcceleration,
    /// Read from the successfully loaded runtime model, never inferred from
    /// the catalog filename or presentation metadata.
    pub(crate) detected_architecture: String,
    /// The runtime's observed capabilities for this installed artifact.
    pub(crate) capabilities: RuntimeCapabilities,
    pub(crate) health_duration_ms: u128,
    pub(crate) load_duration_ms: u128,
    pub(crate) decode_duration_ms: u128,
    pub(crate) reload_duration_ms: u128,
    /// The isolated native smoke rejected a request carrying a stale
    /// cancellation generation before final unload/reload verification.
    #[serde(default)]
    pub(crate) cancellation_verified: bool,
}

/// Unforgeable outside this module: only the real `TranscriptionService`
/// smoke path can create the witness consumed by `StagedOnnxBundle`.
pub(crate) struct VerifiedOnnxBundleSmoke {
    root: PathBuf,
    receipt: crate::onnx_model_bundles::OnnxBundleReceipt,
    spec: OnnxModelSpec,
    cancellation: InstallCancellation,
    smoke: InstallSmoke,
}

impl VerifiedOnnxBundleSmoke {
    pub(crate) fn into_parts(
        self,
    ) -> (
        PathBuf,
        crate::onnx_model_bundles::OnnxBundleReceipt,
        OnnxModelSpec,
        InstallCancellation,
        InstallSmoke,
    ) {
        (
            self.root,
            self.receipt,
            self.spec,
            self.cancellation,
            self.smoke,
        )
    }
}

/// Single-use installation evidence sealed by the isolated verifier and bound
/// to the exact staged artifact and cancellation handle it checked.
pub(crate) struct VerifiedInstallationCandidate {
    candidate: InstallationCandidate,
    cancellation: InstallCancellation,
    smoke: InstallSmoke,
}

impl VerifiedInstallationCandidate {
    pub(crate) fn authorize_activation(
        self,
        model_id: &ModelId,
        model_path: &Path,
        expected_size_bytes: u64,
        expected_sha256: &str,
        cancellation: &InstallCancellation,
    ) -> Result<InstallSmoke> {
        if !self.cancellation.same_handle(cancellation) {
            return Err(anyhow!(
                "verified installation candidate was presented with a different cancellation handle"
            ));
        }
        if self.candidate.model_id != *model_id
            || self.candidate.model_path != model_path
            || self.candidate.expected_size_bytes != expected_size_bytes
            || !self
                .candidate
                .expected_sha256
                .eq_ignore_ascii_case(expected_sha256)
        {
            return Err(anyhow!(
                "verified installation candidate no longer matches the staged artifact"
            ));
        }
        cancellation
            .try_commit_activation()
            .map_err(|state| match state {
                crate::installations::ActivationCommitError::Cancelled => {
                    anyhow!("installation verification was cancelled before activation committed")
                }
                crate::installations::ActivationCommitError::AlreadyCommitted => {
                    anyhow!("verified installation candidate was already consumed for activation")
                }
            })?;
        Ok(self.smoke)
    }
}

/// The normalized install plan derived from the trusted catalog. These plans
/// are artifact-only: worker readiness follows verification and activation,
/// never a compatibility-provider runtime request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum InstallPlan {
    PinnedGguf,
    ReceiptBackedOnnx,
}

/// A streaming decoder update with stable and revisable portions separated.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct StreamUpdate {
    pub committed: String,
    pub tentative: String,
}

/// Common synchronous native-audio speech engine contract.
pub trait SpeechEngine: Send {
    fn load(&mut self) -> Result<()>;

    fn capabilities(&self) -> RuntimeCapabilities;

    fn unload(&mut self) -> Result<()>;
}

/// A prepared-audio request that preserves application correlation IDs.
#[derive(Clone, Debug, PartialEq)]
pub struct TranscriptionRequest {
    pub session_id: SessionId,
    pub request_id: RequestId,
    pub audio: Arc<PreparedAudio>,
    /// A stable catalog identifier, resolved against the service configuration.
    pub model_id: ModelId,
    /// Optional per-request override for a configured model location.
    pub model_path: Option<PathBuf>,
    pub options: TranscriptionOptions,
}

/// Opaque, runtime-neutral cancellation state captured before dispatching a
/// transcription task to a worker thread.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TranscriptionTicket {
    native_generation: u64,
    process_generation: crate::stt::CancellationSnapshot,
}

/// Owned registration that spans native audio preparation and the complete
/// transcription request on a worker thread.
pub struct TranscriptionTask {
    ticket: TranscriptionTicket,
    _registration: crate::stt::RegisteredRequest,
}

impl TranscriptionRequest {
    pub fn new(
        session_id: SessionId,
        request_id: RequestId,
        audio: Arc<PreparedAudio>,
        model_id: impl Into<ModelId>,
    ) -> Self {
        Self {
            session_id,
            request_id,
            audio,
            model_id: model_id.into(),
            model_path: None,
            options: TranscriptionOptions::default(),
        }
    }
}

/// A normalized completed transcription with UI-facing diagnostics retained.
#[derive(Clone, Debug, PartialEq)]
pub struct TranscriptionOutcome {
    pub session_id: SessionId,
    pub request_id: RequestId,
    pub model_id: ModelId,
    pub model_name: String,
    /// A human-readable label for diagnostics and the model playground.
    pub backend_label: String,
    pub transcript: Transcript,
    /// Wall-clock processing time reported by the selected legacy adapter.
    ///
    /// This is deliberately distinct from [`Transcript::duration_ms`], which
    /// represents utterance duration on the audio timeline.
    pub processing_duration_ms: Option<u128>,
    /// Runtime-neutral acceleration resolution for the completed request.
    pub resolved_acceleration: Option<ResolvedAcceleration>,
    /// Time spent loading the retained model for this request. Zero means a
    /// warm native model was reused.
    pub model_load_duration_ms: Option<u128>,
    pub warm_model_reused: bool,
    pub stdout: String,
    pub stderr: String,
}

impl TranscriptionOutcome {
    /// Runtime-neutral technical label for diagnostics. Application code uses
    /// this accessor instead of depending on the private handler selection.
    pub fn resolved_backend_label(&self) -> &str {
        &self.backend_label
    }
}

/// Runtime-neutral diagnostics from an explicit model preload.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelLoadOutcome {
    pub model_id: ModelId,
    pub resolved_acceleration: ResolvedAcceleration,
    pub model_load_duration_ms: u128,
    pub warm_model_reused: bool,
}

/// Text-only application handle for one rolling batch-preview session. Audio
/// publication is split into a separate opaque producer handed straight to
/// native capture, so UI coordination cannot inspect preview PCM.
pub(crate) struct RollingPreviewHandle {
    identity: StreamIdentity,
    session: RollingPreviewSession<anyhow::Error>,
}

impl RollingPreviewHandle {
    pub(crate) fn identity(&self) -> &StreamIdentity {
        &self.identity
    }

    pub(crate) fn try_next(&self) -> Option<PreviewEvent<anyhow::Error>> {
        self.session.try_next()
    }

    pub(crate) fn close(&self) {
        self.session.close();
    }

    pub(crate) fn invalidate(&self) {
        self.session.invalidate();
    }

    pub(crate) fn is_finished(&self) -> bool {
        self.session.is_finished()
    }

    pub(crate) fn stop_and_join(&mut self, timeout: Duration) -> bool {
        self.session.stop_and_join(timeout)
    }

    #[cfg(test)]
    pub(crate) fn simulated<F>(
        identity: StreamIdentity,
        decode: F,
    ) -> std::io::Result<(PreviewAudioPublisher, Self)>
    where
        F: FnMut(crate::streaming::PreviewSnapshot) -> Result<StreamUpdate, anyhow::Error>
            + Send
            + 'static,
    {
        let session = RollingPreviewSession::new(decode)?;
        let publisher = session.audio_publisher(
            identity.session_id,
            identity.request_id,
            identity.model_id.clone(),
        );
        Ok((publisher, Self { identity, session }))
    }
}

enum RuntimeCommand {
    Transcribe {
        artifact: RuntimeArtifact,
        preference: AccelerationPreference,
        audio: Arc<PreparedAudio>,
        options: TranscriptionOptions,
        cancellation_snapshot: u64,
        reply: SyncSender<Result<RuntimeExecution, RuntimeError>>,
    },
    Load {
        artifact: RuntimeArtifact,
        preference: AccelerationPreference,
        reply: SyncSender<Result<RuntimeLoadExecution, RuntimeError>>,
    },
    Health {
        artifact: RuntimeArtifact,
        preference: AccelerationPreference,
        reply: SyncSender<Result<(), RuntimeError>>,
    },
    Unload {
        reply: SyncSender<Result<(), RuntimeError>>,
    },
    Shutdown {
        reply: SyncSender<Result<(), RuntimeError>>,
    },
}

/// A bounded, dedicated native worker. Application-created task threads may
/// wait on this facade, but all concrete engine lifecycle and inference work
/// is serialized on the one named native worker.
#[derive(Clone)]
struct RuntimeWorker {
    inner: Arc<RuntimeWorkerInner>,
}

struct RuntimeWorkerInner {
    commands: SyncSender<RuntimeCommand>,
    worker: Mutex<Option<std::thread::JoinHandle<()>>>,
    shutdown_gate: Mutex<()>,
    cancellation_generation: Arc<AtomicU64>,
    #[cfg(test)]
    in_process_router: Option<RuntimeRouter>,
    inference: Option<InferenceWorkerRegistry>,
}

impl RuntimeWorker {
    #[cfg(test)]
    fn new(router: RuntimeRouter) -> Self {
        let (commands, receiver) = sync_channel(1);
        let worker_router = router.clone();
        let worker = std::thread::Builder::new()
            .name("scribe-native-runtime".to_owned())
            .spawn(move || runtime_worker_loop(worker_router, receiver))
            .expect("Scribe could not create its native runtime worker");
        Self {
            inner: Arc::new(RuntimeWorkerInner {
                commands,
                worker: Mutex::new(Some(worker)),
                shutdown_gate: Mutex::new(()),
                cancellation_generation: Arc::new(AtomicU64::new(0)),
                in_process_router: Some(router),
                inference: None,
            }),
        }
    }

    fn new_process() -> Self {
        let inference = InferenceWorkerRegistry::cpu_only();
        let worker_inference = inference.clone();
        let cancellation_generation = Arc::new(AtomicU64::new(0));
        let worker_cancellation = cancellation_generation.clone();
        let (commands, receiver) = sync_channel(1);
        let worker = std::thread::Builder::new()
            .name("scribe-inference-dispatch".to_owned())
            .spawn(move || {
                inference_worker_dispatch_loop(worker_inference, worker_cancellation, receiver)
            })
            .expect("Scribe could not create its inference dispatch worker");
        Self {
            inner: Arc::new(RuntimeWorkerInner {
                commands,
                worker: Mutex::new(Some(worker)),
                shutdown_gate: Mutex::new(()),
                cancellation_generation,
                #[cfg(test)]
                in_process_router: None,
                inference: Some(inference),
            }),
        }
    }

    #[cfg(test)]
    fn new_process_for_executable(executable: PathBuf) -> Self {
        let inference = InferenceWorkerRegistry::with_cpu_supervisor(
            InferenceWorkerSupervisor::unstarted_for_executable(executable),
        );
        let worker_inference = inference.clone();
        let cancellation_generation = Arc::new(AtomicU64::new(0));
        let worker_cancellation = cancellation_generation.clone();
        let (commands, receiver) = sync_channel(1);
        let worker = std::thread::Builder::new()
            .name("scribe-inference-diagnostic-dispatch".to_owned())
            .spawn(move || {
                inference_worker_dispatch_loop(worker_inference, worker_cancellation, receiver)
            })
            .expect("Scribe could not create its diagnostic inference dispatch worker");
        Self {
            inner: Arc::new(RuntimeWorkerInner {
                commands,
                worker: Mutex::new(Some(worker)),
                shutdown_gate: Mutex::new(()),
                cancellation_generation,
                in_process_router: None,
                inference: Some(inference),
            }),
        }
    }

    fn cancel_active(&self) {
        self.inner
            .cancellation_generation
            .fetch_add(1, Ordering::AcqRel);
        #[cfg(test)]
        if let Some(router) = &self.inner.in_process_router {
            router.cancel_active();
        }
        if let Some(inference) = &self.inner.inference {
            inference.cancel_active();
        }
    }

    fn cancellation_snapshot(&self) -> u64 {
        self.inner.cancellation_generation.load(Ordering::Acquire)
    }

    fn transcribe(
        &self,
        artifact: impl Into<RuntimeArtifact>,
        preference: AccelerationPreference,
        audio: Arc<PreparedAudio>,
        options: TranscriptionOptions,
        cancellation_snapshot: u64,
    ) -> Result<RuntimeExecution, RuntimeError> {
        if self.cancellation_snapshot() != cancellation_snapshot {
            return Err(RuntimeError::Engine(
                "transcription request was cancelled before inference dispatch".to_owned(),
            ));
        }
        let (reply, response) = sync_channel(1);
        self.inner
            .commands
            .send(RuntimeCommand::Transcribe {
                artifact: artifact.into(),
                preference,
                audio,
                options,
                cancellation_snapshot,
                reply,
            })
            .map_err(|error| RuntimeError::WorkerUnavailable(error.to_string()))?;
        response
            .recv()
            .map_err(|error| RuntimeError::WorkerUnavailable(error.to_string()))?
    }

    fn load(
        &self,
        artifact: impl Into<RuntimeArtifact>,
        preference: AccelerationPreference,
    ) -> Result<RuntimeLoadExecution, RuntimeError> {
        let (reply, response) = sync_channel(1);
        self.inner
            .commands
            .send(RuntimeCommand::Load {
                artifact: artifact.into(),
                preference,
                reply,
            })
            .map_err(|error| RuntimeError::WorkerUnavailable(error.to_string()))?;
        response
            .recv()
            .map_err(|error| RuntimeError::WorkerUnavailable(error.to_string()))?
    }

    fn health_check(
        &self,
        artifact: impl Into<RuntimeArtifact>,
        preference: AccelerationPreference,
    ) -> Result<(), RuntimeError> {
        let (reply, response) = sync_channel(1);
        self.inner
            .commands
            .send(RuntimeCommand::Health {
                artifact: artifact.into(),
                preference,
                reply,
            })
            .map_err(|error| RuntimeError::WorkerUnavailable(error.to_string()))?;
        response
            .recv()
            .map_err(|error| RuntimeError::WorkerUnavailable(error.to_string()))?
    }

    fn unload(&self) -> Result<(), RuntimeError> {
        let (reply, response) = sync_channel(1);
        self.inner
            .commands
            .send(RuntimeCommand::Unload { reply })
            .map_err(|error| RuntimeError::WorkerUnavailable(error.to_string()))?;
        response
            .recv()
            .map_err(|error| RuntimeError::WorkerUnavailable(error.to_string()))?
    }

    fn shutdown_and_join(&self, timeout: Duration) -> bool {
        self.inner.shutdown_and_join(timeout)
    }
}

impl RuntimeWorkerInner {
    fn shutdown_and_join(&self, timeout: Duration) -> bool {
        self.cancellation_generation.fetch_add(1, Ordering::AcqRel);
        #[cfg(test)]
        if let Some(router) = &self.in_process_router {
            router.cancel_active();
        }
        if let Some(inference) = &self.inference {
            inference.cancel_active();
        }
        let deadline = Instant::now() + timeout;
        let _shutdown_guard = loop {
            match self.shutdown_gate.try_lock() {
                Ok(guard) => break guard,
                Err(TryLockError::Poisoned(poisoned)) => break poisoned.into_inner(),
                Err(TryLockError::WouldBlock) => {
                    if Instant::now() >= deadline {
                        return false;
                    }
                    std::thread::sleep(SHUTDOWN_POLL_INTERVAL);
                }
            }
        };

        let mut worker_guard = self
            .worker
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(worker) = worker_guard.as_ref() else {
            return true;
        };
        if worker.is_finished() {
            let worker = worker_guard.take().expect("worker checked above");
            let _ = worker.join();
            return true;
        }

        let (reply, response) = sync_channel(1);
        let mut shutdown = RuntimeCommand::Shutdown { reply };
        loop {
            match self.commands.try_send(shutdown) {
                Ok(()) => break,
                Err(std::sync::mpsc::TrySendError::Full(command)) => {
                    shutdown = command;
                    if Instant::now() >= deadline {
                        return false;
                    }
                    std::thread::sleep(SHUTDOWN_POLL_INTERVAL);
                }
                Err(std::sync::mpsc::TrySendError::Disconnected(_)) => break,
            }
        }

        if !worker.is_finished() {
            let remaining = deadline.saturating_duration_since(Instant::now());
            match response.recv_timeout(remaining) {
                Ok(_) | Err(RecvTimeoutError::Disconnected) => {}
                Err(RecvTimeoutError::Timeout) if !worker.is_finished() => return false,
                Err(RecvTimeoutError::Timeout) => {}
            }
        }
        while !worker.is_finished() {
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(SHUTDOWN_POLL_INTERVAL);
        }
        let worker = worker_guard.take().expect("worker checked above");
        // A panic/disconnect is an operational failure, but the native thread
        // is gone and it is therefore safe to continue process teardown.
        let _ = worker.join();
        true
    }
}

impl Drop for RuntimeWorkerInner {
    fn drop(&mut self) {
        if !self.shutdown_and_join(DROP_SHUTDOWN_TIMEOUT) {
            // Never detach a native worker in-process: that reintroduces the
            // observed Windows access violation when the DLL unloads. Also do
            // not hang forever. A hard abort skips destructors/DLL teardown and
            // is the last-resort process-safe exit policy.
            eprintln!("native runtime worker exceeded the shutdown deadline; aborting safely");
            std::process::abort();
        }
    }
}

impl fmt::Debug for RuntimeWorker {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RuntimeWorker")
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
fn runtime_worker_loop(router: RuntimeRouter, commands: Receiver<RuntimeCommand>) {
    let activity = router.runtime_activity();
    let mut idle_wait = WARM_MODEL_TTL;
    loop {
        let (succeeded, mut request_activity) = match commands.recv_timeout(idle_wait) {
            Ok(RuntimeCommand::Transcribe {
                artifact,
                preference,
                audio,
                options,
                cancellation_snapshot,
                reply,
            }) => {
                let request_activity = activity.acquire_request().ok();
                let result = router.transcribe(
                    artifact,
                    preference,
                    &audio,
                    &options,
                    cancellation_snapshot,
                );
                let succeeded = result.is_ok();
                let _ = reply.send(result);
                (succeeded, request_activity)
            }
            Ok(RuntimeCommand::Load {
                artifact,
                preference,
                reply,
            }) => {
                let request_activity = activity.acquire_request().ok();
                let result = router.load(artifact, preference);
                let succeeded = result.is_ok();
                let _ = reply.send(result);
                (succeeded, request_activity)
            }
            Ok(RuntimeCommand::Health {
                artifact,
                preference,
                reply,
            }) => {
                let request_activity = activity.acquire_request().ok();
                let result = router.health_check(artifact, preference);
                let succeeded = result.is_ok();
                let _ = reply.send(result);
                (succeeded, request_activity)
            }
            Ok(RuntimeCommand::Unload { reply }) => {
                let request_activity = activity.acquire_request().ok();
                let result = router.unload_all();
                let succeeded = result.is_ok();
                let _ = reply.send(result);
                (succeeded, request_activity)
            }
            Ok(RuntimeCommand::Shutdown { reply }) => {
                let result = router.unload_all();
                let _ = reply.send(result);
                break;
            }
            Err(RecvTimeoutError::Timeout) => {
                match activity.timeout_action(WARM_MODEL_TTL) {
                    IdleTimeoutAction::Unload => {
                        if router.unload_all().is_ok() {
                            activity.mark_command_complete();
                        }
                        idle_wait = WARM_MODEL_TTL;
                    }
                    IdleTimeoutAction::Defer(remaining) => {
                        idle_wait = remaining;
                    }
                }
                continue;
            }
            Err(RecvTimeoutError::Disconnected) => {
                let _ = router.unload_all();
                break;
            }
        };
        if succeeded {
            if let Some(request_activity) = request_activity.as_mut() {
                request_activity.complete_successfully();
            } else {
                activity.mark_command_complete();
            }
        }
        drop(request_activity);
        idle_wait = match activity.timeout_action(WARM_MODEL_TTL) {
            IdleTimeoutAction::Unload => Duration::ZERO,
            IdleTimeoutAction::Defer(remaining) => remaining,
        };
    }
}

fn inference_worker_dispatch_loop(
    inference: InferenceWorkerRegistry,
    cancellation_generation: Arc<AtomicU64>,
    commands: Receiver<RuntimeCommand>,
) {
    let mut idle_wait = WARM_MODEL_TTL;
    loop {
        let succeeded = match commands.recv_timeout(idle_wait) {
            Ok(RuntimeCommand::Transcribe {
                artifact,
                preference,
                audio,
                options,
                cancellation_snapshot,
                reply,
            }) => {
                let result = if cancellation_generation.load(Ordering::Acquire)
                    != cancellation_snapshot
                {
                    Err(RuntimeError::Engine(
                        "transcription request was cancelled before inference dispatch".to_owned(),
                    ))
                } else {
                    inference.transcribe(
                        artifact,
                        preference,
                        &audio,
                        options,
                        cancellation_snapshot,
                        &cancellation_generation,
                    )
                };
                let succeeded = result.is_ok();
                let _ = reply.send(result);
                succeeded
            }
            Ok(RuntimeCommand::Load {
                artifact,
                preference,
                reply,
            }) => {
                let result = inference.load(artifact, preference);
                let succeeded = result.is_ok();
                let _ = reply.send(result);
                succeeded
            }
            Ok(RuntimeCommand::Health {
                artifact,
                preference,
                reply,
            }) => {
                let result = inference.health(artifact, preference);
                let succeeded = result.is_ok();
                let _ = reply.send(result);
                succeeded
            }
            Ok(RuntimeCommand::Unload { reply }) => {
                let result = inference.unload();
                let succeeded = result.is_ok();
                let _ = reply.send(result);
                succeeded
            }
            Ok(RuntimeCommand::Shutdown { reply }) => {
                let result = inference.shutdown();
                let _ = reply.send(result);
                break;
            }
            Err(RecvTimeoutError::Timeout) => {
                // The five-minute TTL unloads only the worker-owned model. A
                // healthy process generation remains available for the next
                // cold load and Hello is not repeated.
                let _ = inference.unload_if_idle();
                idle_wait = WARM_MODEL_TTL;
                continue;
            }
            Err(RecvTimeoutError::Disconnected) => {
                let _ = inference.shutdown();
                break;
            }
        };
        idle_wait = if succeeded {
            WARM_MODEL_TTL
        } else {
            // Failed changed loads are cold in the child. Keep the process,
            // but do not claim a fresh warm interval for a model.
            WARM_MODEL_TTL
        };
    }
}

/// Application-facing boundary for all transcription work.
#[derive(Clone, Debug)]
pub struct TranscriptionService {
    config: AppConfig,
    #[cfg(test)]
    router: RuntimeRouter,
    #[cfg(test)]
    current_receipt_manifest: Option<OnnxBundleManifest>,
    worker: RuntimeWorker,
}

impl TranscriptionService {
    pub fn new(config: AppConfig) -> Self {
        #[cfg(not(test))]
        let worker = RuntimeWorker::new_process();
        #[cfg(test)]
        let router = RuntimeRouter::new();
        #[cfg(test)]
        let worker = RuntimeWorker::new(router.clone());
        Self {
            config,
            worker,
            #[cfg(test)]
            router,
            #[cfg(test)]
            current_receipt_manifest: None,
        }
    }

    /// Returns a service snapshot with current settings while retaining the
    /// same serialized runtime worker and its warm model.
    pub fn with_config(&self, config: AppConfig) -> Self {
        Self {
            config,
            #[cfg(test)]
            router: self.router.clone(),
            #[cfg(test)]
            current_receipt_manifest: self.current_receipt_manifest.clone(),
            worker: self.worker.clone(),
        }
    }

    #[cfg(test)]
    pub(crate) fn with_process_executable(config: AppConfig, executable: PathBuf) -> Self {
        Self {
            config,
            worker: RuntimeWorker::new_process_for_executable(executable),
            router: RuntimeRouter::new(),
            current_receipt_manifest: None,
        }
    }

    #[cfg(test)]
    fn with_test_current_receipt_manifest(mut self, manifest: OnnxBundleManifest) -> Self {
        self.current_receipt_manifest = Some(manifest);
        self
    }

    #[cfg(test)]
    pub(crate) fn unload_runtime_artifacts(&self) -> Result<()> {
        self.worker.unload().map_err(Into::into)
    }

    /// Resolves a private installed ONNX bundle from its durable exact receipt
    /// only when that receipt still equals a currently embedded available
    /// manifest. This path is local-only and never grants a self-authored or
    /// retired receipt executable trust.
    fn onnx_artifact_from_receipt(&self, root: &Path) -> Result<RuntimeArtifact> {
        #[cfg(not(test))]
        let spec = crate::onnx_model_bundles::current_executable_receipt_at(root)
            .map(|(_, spec)| spec)
            .map_err(|error| anyhow!("installed ONNX bundle verification failed: {error}"))?;
        #[cfg(test)]
        let spec = self
            .current_receipt_manifest
            .as_ref()
            .map_or_else(
                || crate::onnx_model_bundles::current_executable_receipt_at(root),
                |manifest| {
                    crate::onnx_model_bundles::current_executable_receipt_at_with_manifest_for_test(
                        root, manifest,
                    )
                },
            )
            .map(|(_, spec)| spec)
            .map_err(|error| anyhow!("installed ONNX bundle verification failed: {error}"))?;
        Ok(RuntimeArtifact::OnnxBundle(spec))
    }

    pub(crate) fn preload_onnx_bundle_from_receipt(
        &self,
        root: &Path,
    ) -> Result<RuntimeLoadExecution> {
        self.worker
            .load(
                self.onnx_artifact_from_receipt(root)?,
                AccelerationPreference::Cpu,
            )
            .map_err(Into::into)
    }

    #[cfg(test)]
    pub(crate) fn transcribe_onnx_bundle_from_receipt(
        &self,
        root: &Path,
        audio: Arc<PreparedAudio>,
        options: TranscriptionOptions,
    ) -> Result<RuntimeExecution> {
        self.worker
            .transcribe(
                self.onnx_artifact_from_receipt(root)?,
                AccelerationPreference::Cpu,
                audio,
                options,
                self.worker.cancellation_snapshot(),
            )
            .map_err(Into::into)
    }

    /// Runs the staged bundle through the process-isolated ONNX worker before
    /// activation. CPU is fixed here because the delivery unit carries no GPU
    /// support or evidence.
    pub(crate) fn verify_onnx_bundle_for_installation(
        &self,
        staged: crate::onnx_model_bundles::StagedOnnxBundle,
        cancellation: &InstallCancellation,
    ) -> Result<crate::onnx_model_bundles::VerifiedStagedOnnxBundle> {
        ensure_install_not_cancelled(cancellation)?;
        let root = staged.root().to_path_buf();
        let (receipt, spec) = crate::onnx_model_bundles::current_executable_receipt_at(&root)
            .map_err(|error| anyhow!("staged ONNX bundle verification failed: {error}"))?;
        if staged.receipt() != &receipt || staged.spec() != &spec {
            return Err(anyhow!(
                "staged ONNX bundle changed before service smoke verification"
            ));
        }
        let artifact = RuntimeArtifact::OnnxBundle(spec.clone());
        let smoke = self.verify_onnx_artifact_smoke(artifact, cancellation)?;
        ensure_install_not_cancelled(cancellation)?;
        staged
            .bind_verified(VerifiedOnnxBundleSmoke {
                root,
                receipt,
                spec,
                cancellation: cancellation.clone(),
                smoke,
            })
            .map_err(Into::into)
    }

    fn verify_onnx_artifact_smoke(
        &self,
        artifact: RuntimeArtifact,
        cancellation: &InstallCancellation,
    ) -> Result<InstallSmoke> {
        let preference = AccelerationPreference::Cpu;

        let health_started = Instant::now();
        let health_result = (|| {
            self.worker
                .health_check(artifact.clone(), preference)
                .map_err(|error| anyhow!("staged ONNX bundle health check failed: {error}"))?;
            let duration = health_started.elapsed().as_millis();
            ensure_install_not_cancelled(cancellation)?;
            Ok(duration)
        })();
        let health_unload = self
            .worker
            .unload()
            .map_err(|error| anyhow!("staged ONNX bundle health unload failed: {error}"));
        let health_duration_ms = preserve_primary_smoke_error(health_result, health_unload)?;

        let load_started = Instant::now();
        let load_decode_result = (|| {
            let load = self
                .worker
                .load(artifact.clone(), preference)
                .map_err(|error| anyhow!("staged ONNX bundle load failed: {error}"))?;
            let load_duration_ms = load_started.elapsed().as_millis();
            ensure_install_not_cancelled(cancellation)?;
            let audio = Arc::new(PreparedAudio::from_captured_mono(
                vec![0.0; PREPARED_SAMPLE_RATE as usize],
                PREPARED_SAMPLE_RATE,
                1,
                PREPARED_SAMPLE_RATE as usize,
            )?);
            let decode_started = Instant::now();
            self.worker
                .transcribe(
                    artifact.clone(),
                    preference,
                    audio,
                    TranscriptionOptions::default(),
                    self.worker.cancellation_snapshot(),
                )
                .map_err(|error| anyhow!("staged ONNX bundle decode smoke failed: {error}"))?;
            ensure_install_not_cancelled(cancellation)?;
            Ok((load, load_duration_ms, decode_started.elapsed().as_millis()))
        })();
        let unload_result = self
            .worker
            .unload()
            .map_err(|error| anyhow!("staged ONNX bundle unload failed: {error}"));
        let (load, load_duration_ms, decode_duration_ms) =
            preserve_primary_smoke_error(load_decode_result, unload_result)?;

        let reload_started = Instant::now();
        let reload_result = (|| {
            self.worker
                .load(artifact, preference)
                .map_err(|error| anyhow!("staged ONNX bundle reload failed: {error}"))?;
            let duration = reload_started.elapsed().as_millis();
            ensure_install_not_cancelled(cancellation)?;
            Ok(duration)
        })();
        let unload_result = self
            .worker
            .unload()
            .map_err(|error| anyhow!("staged ONNX bundle final unload failed: {error}"));
        let reload_duration_ms = preserve_primary_smoke_error(reload_result, unload_result)?;

        Ok(InstallSmoke {
            resolved_acceleration: load.diagnostics.resolved_acceleration,
            detected_architecture: load.detected_architecture,
            capabilities: load.capabilities,
            health_duration_ms,
            load_duration_ms,
            decode_duration_ms,
            reload_duration_ms,
            cancellation_verified: false,
        })
    }

    #[cfg(test)]
    pub(crate) fn configured_acceleration_preference(&self) -> AccelerationPreference {
        self.config.performance.acceleration_preference
    }

    /// Returns normal user-facing, runtime-neutral descriptors.
    pub fn model_descriptors(&self) -> Vec<ModelDescriptor> {
        normal_model_descriptors()
            .into_iter()
            .map(|descriptor| self.effective_descriptor(descriptor))
            .collect()
    }

    /// Resolves one runtime-neutral catalog descriptor.
    pub fn model_descriptor(&self, model_id: &ModelId) -> Result<ModelDescriptor> {
        model_descriptor(model_id)
            .map(|descriptor| self.effective_descriptor(descriptor))
            .ok_or_else(|| anyhow!("unknown normalized transcription model: {model_id}"))
    }

    pub(crate) fn install_plan(&self, model_id: &ModelId) -> Option<InstallPlan> {
        Self::install_plan_for_config(&self.config, model_id)
    }

    pub(crate) fn install_plan_for_config(
        _config: &AppConfig,
        model_id: &ModelId,
    ) -> Option<InstallPlan> {
        match crate::model_catalog::normalized_install_artifact(model_id) {
            Some(crate::model_catalog::NormalizedInstallArtifact::SingleGguf(_)) => {
                Some(InstallPlan::PinnedGguf)
            }
            Some(crate::model_catalog::NormalizedInstallArtifact::ReceiptBackedBundle {
                ..
            }) => Some(InstallPlan::ReceiptBackedOnnx),
            None => None,
        }
    }

    /// Returns the conservative feature set for a configured model.
    pub fn capabilities_for(&self, model_id: &ModelId) -> Result<RuntimeCapabilities> {
        let model = self.resolve_model(model_id, None)?;
        let persisted_capabilities = model
            .local_path
            .as_deref()
            .and_then(|path| {
                let validation =
                    if config::imported_gguf_artifact(&self.config, model_id.as_str()).is_some() {
                        crate::installed_manifest::imported_runtime_validation_for(
                            model_id,
                            path,
                            &config::model_storage_dir(&self.config),
                        )
                    } else {
                        crate::installed_manifest::runtime_validation_for(model_id, path)
                    };
                validation.ok().flatten()
            })
            .filter(|validation| validation.package_free)
            .map(|validation| validation.capabilities);
        if config::remote_gguf_artifact(&self.config, model_id.as_str()).is_some()
            || config::imported_gguf_artifact(&self.config, model_id.as_str()).is_some()
        {
            return Ok(persisted_capabilities
                .unwrap_or_else(crate::runtime_contract::embedded_runtime_capabilities));
        }
        if crate::runtime_contract::handles_model_id(model_id) {
            let runtime_capabilities = persisted_capabilities
                .or_else(|| crate::runtime_contract::capabilities_for_model(model_id))
                .ok_or_else(|| anyhow!("runtime router rejected its own selected model"))?;
            let descriptor = model_descriptor(model_id)
                .ok_or_else(|| anyhow!("unknown normalized transcription model: {model_id}"))?;
            return Ok(intersect_capabilities(&runtime_capabilities, &descriptor));
        }
        Err(anyhow!(
            "model {model_id} has no supported native transcription runtime"
        ))
    }

    fn effective_descriptor(&self, mut descriptor: ModelDescriptor) -> ModelDescriptor {
        if let Ok(effective) = self.capabilities_for(&descriptor.id) {
            descriptor.capabilities.native_streaming = effective.streaming;
            descriptor.capabilities.cancellation = effective.cancellation;
            descriptor.capabilities.translation = effective.translation;
            descriptor.capabilities.timestamps = effective.timestamps;
            descriptor.capabilities.language_detection = effective.language_detection;
            descriptor.capabilities.confidence_scores = effective.confidence_scores;
            descriptor.capabilities.custom_vocabulary = effective.custom_vocabulary;
        }
        descriptor
    }

    /// Loads a primary-runtime model on the dedicated worker. Phase 4 uses
    /// this to overlap model startup with native capture.
    pub fn preload_model(
        &self,
        model_id: &ModelId,
        model_path: Option<PathBuf>,
    ) -> Result<ModelLoadOutcome> {
        if let Some(root) = config::installed_onnx_bundle_root(&self.config, model_id) {
            if model_path.as_ref().is_some_and(|path| path != &root) {
                return Err(anyhow!(
                    "selected ONNX bundle path is not its canonical receipt root"
                ));
            }
            let execution = self.preload_onnx_bundle_from_receipt(&root)?;
            return Ok(ModelLoadOutcome {
                model_id: model_id.clone(),
                resolved_acceleration: execution.diagnostics.resolved_acceleration,
                model_load_duration_ms: execution.diagnostics.model_load_duration_ms,
                warm_model_reused: execution.diagnostics.warm_reused,
            });
        }
        let model = self.resolve_model(model_id, model_path)?;
        let runtime_model = self.resolve_runtime_model(model)?;
        let execution = self
            .worker
            .load(
                runtime_model,
                self.config.performance.acceleration_preference,
            )
            .map_err(|error| anyhow!(error))?;
        Ok(ModelLoadOutcome {
            model_id: model_id.clone(),
            resolved_acceleration: execution.diagnostics.resolved_acceleration,
            model_load_duration_ms: execution.diagnostics.model_load_duration_ms,
            warm_model_reused: execution.diagnostics.warm_reused,
        })
    }

    /// Checks the selected primary runtime package and model without allowing
    /// UI or coordinator code to name the concrete handler.
    pub fn health_check(&self, model_id: &ModelId, model_path: Option<PathBuf>) -> Result<()> {
        if let Some(root) = config::installed_onnx_bundle_root(&self.config, model_id) {
            if model_path.as_ref().is_some_and(|path| path != &root) {
                return Err(anyhow!(
                    "selected ONNX bundle path is not its canonical receipt root"
                ));
            }
            return self
                .worker
                .health_check(
                    self.onnx_artifact_from_receipt(&root)?,
                    AccelerationPreference::Cpu,
                )
                .map_err(Into::into);
        }
        let model = self.resolve_model(model_id, model_path)?;
        let runtime_model = self.resolve_runtime_model(model)?;
        self.worker
            .health_check(
                runtime_model,
                self.config.performance.acceleration_preference,
            )
            .map_err(|error| anyhow!(error))
    }

    pub(crate) fn startup_runtime_health_and_load(
        &self,
        model_id: &ModelId,
        model_path: Option<PathBuf>,
    ) -> Result<()> {
        let model = self.resolve_model(model_id, model_path)?;
        let runtime_model = self.resolve_runtime_model(model)?;
        verify_runtime_model_artifact(&runtime_model)?;
        let mut smoke_config = self.config.clone();
        smoke_config.performance.acceleration_preference = AccelerationPreference::Cpu;
        self.with_config(smoke_config)
            .verify_installation_candidate(
                InstallationCandidate::pinned(
                    runtime_model.id,
                    runtime_model.path,
                    runtime_model.format,
                    runtime_model.expected_size_bytes,
                    runtime_model.expected_sha256,
                ),
                &InstallCancellation::default(),
            )
            .map(|_| ())
    }

    /// Verifies a selected model independently from runtime startup. Startup
    /// recovery must never roll back a healthy runtime because model bytes are
    /// missing or corrupt.
    pub(crate) fn verify_model_artifact_for_installation(
        &self,
        model_id: &ModelId,
        model_path: Option<PathBuf>,
    ) -> Result<()> {
        let model = self.resolve_model(model_id, model_path)?;
        let path = model
            .local_path
            .ok_or_else(|| anyhow!("download {} before verification", model.name))?;
        let imported_artifact = config::imported_gguf_artifact(&self.config, model_id.as_str());
        let (expected_size_bytes, expected_sha256) = if let Some(artifact) = imported_artifact {
            (artifact.expected_size_bytes, artifact.expected_sha256)
        } else {
            let artifact =
                runtime_artifact_manifest_for_path(model_id, &path).ok_or_else(|| {
                    anyhow!(
                        "model {} has no pinned artifact for {}",
                        model.name,
                        path.display()
                    )
                })?;
            (artifact.size_bytes, artifact.sha256.to_owned())
        };
        crate::installations::verify_file(&path, expected_size_bytes, &expected_sha256)
            .map_err(|error| anyhow!("model integrity verification failed: {error}"))
    }

    /// Requests lock-free cancellation of native work submitted before this
    /// call. Later requests capture the new generation and are unaffected.
    pub fn cancel_active(&self) {
        self.worker.cancel_active();
        crate::stt::cancel_active_requests();
    }

    /// Cancels active work and waits for service requests and compatibility
    /// processes to release their transient audio resources.
    pub fn cancel_active_and_wait(&self, timeout: Duration) -> bool {
        self.worker.cancel_active();
        crate::stt::cancel_active_requests_and_wait(timeout)
    }

    /// Stops the dedicated native runtime and joins it within the caller's
    /// process-exit budget. A false result requires an immediate hard abort;
    /// continuing normal teardown would permit a live worker to race DLL unload.
    pub(crate) fn shutdown_runtime_and_wait(&self, timeout: Duration) -> bool {
        self.worker.shutdown_and_join(timeout)
    }

    /// Captures cancellation state synchronously before a caller dispatches
    /// audio preparation or transcription to another thread.
    pub fn transcription_ticket(&self) -> TranscriptionTicket {
        TranscriptionTicket {
            native_generation: self.worker.cancellation_snapshot(),
            process_generation: crate::stt::cancellation_snapshot(),
        }
    }

    /// Registers work synchronously before the caller starts an audio worker.
    pub fn begin_transcription_task(&self) -> Result<TranscriptionTask> {
        let ticket = self.transcription_ticket();
        let registration = crate::stt::register_cancellable_request(ticket.process_generation)
            .map_err(|error| anyhow!(error))?;
        if self.worker.cancellation_snapshot() != ticket.native_generation {
            return Err(anyhow!(
                "transcription request was cancelled before dispatch"
            ));
        }
        Ok(TranscriptionTask {
            ticket,
            _registration: registration,
        })
    }

    /// Drops all retained native model state on the dedicated worker.
    pub fn unload_runtime(&self) -> Result<()> {
        self.worker.unload().map_err(|error| anyhow!(error))
    }

    /// Exercises a staged artifact pair through a fresh private router before
    /// either artifact becomes observable in active settings.
    pub(crate) fn verify_installation_candidate(
        &self,
        candidate: InstallationCandidate,
        cancellation: &InstallCancellation,
    ) -> Result<InstallSmoke> {
        let executable = std::env::current_exe()
            .map_err(|error| anyhow!("could not locate the installation smoke helper: {error}"))?;
        let acceleration = match self.config.performance.acceleration_preference {
            AccelerationPreference::Auto => "auto",
            AccelerationPreference::Cpu => "cpu",
            AccelerationPreference::Gpu => "gpu",
        };
        let mut child = Command::new(executable)
            .arg(INSTALL_SMOKE_HELPER_FLAG)
            .arg(candidate.model_id.as_str())
            .arg(&candidate.model_path)
            .arg("gguf")
            .arg(candidate.expected_size_bytes.to_string())
            .arg(&candidate.expected_sha256)
            .arg(acceleration)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| anyhow!("could not start the isolated native smoke test: {error}"))?;
        let started = Instant::now();
        loop {
            if cancellation.is_cancelled() {
                let _ = child.kill();
                let _ = child.wait();
                return Err(anyhow!(
                    "installation verification was cancelled; the isolated native smoke process was terminated"
                ));
            }
            if started.elapsed() >= INSTALL_SMOKE_TIMEOUT {
                let _ = child.kill();
                let _ = child.wait();
                return Err(anyhow!(
                    "isolated native smoke test exceeded the {:?} deadline and was terminated",
                    INSTALL_SMOKE_TIMEOUT
                ));
            }
            match child.try_wait() {
                Ok(Some(_)) => {
                    let output = child.wait_with_output().map_err(|error| {
                        anyhow!("could not collect native smoke output: {error}")
                    })?;
                    if !output.status.success() {
                        return Err(anyhow!(
                            "isolated native smoke test failed with {}: {}",
                            output.status,
                            String::from_utf8_lossy(&output.stderr).trim()
                        ));
                    }
                    return serde_json::from_slice(&output.stdout).map_err(|error| {
                        anyhow!(
                            "isolated native smoke test returned invalid diagnostics: {error}; stderr: {}",
                            String::from_utf8_lossy(&output.stderr).trim()
                        )
                    });
                }
                Ok(None) => std::thread::sleep(INSTALL_SMOKE_POLL_INTERVAL),
                Err(error) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(anyhow!("could not poll native smoke process: {error}"));
                }
            }
        }
    }

    /// Issues activation evidence only after the real isolated verifier has
    /// accepted the exact candidate under the retained cancellation handle.
    pub(crate) fn verify_installation_candidate_for_activation(
        &self,
        candidate: InstallationCandidate,
        cancellation: &InstallCancellation,
    ) -> Result<VerifiedInstallationCandidate> {
        let verified_candidate = candidate.clone();
        let smoke = self.verify_installation_candidate(candidate, cancellation)?;
        ensure_install_not_cancelled(cancellation)?;
        Ok(VerifiedInstallationCandidate {
            candidate: verified_candidate,
            cancellation: cancellation.clone(),
            smoke,
        })
    }

    #[cfg(test)]
    pub(crate) fn verify_installation_candidate_for_activation_with(
        &self,
        candidate: InstallationCandidate,
        cancellation: &InstallCancellation,
        verify: impl FnOnce(&InstallationCandidate) -> Result<InstallSmoke>,
    ) -> Result<VerifiedInstallationCandidate> {
        ensure_install_not_cancelled(cancellation)?;
        let smoke = verify(&candidate)?;
        ensure_install_not_cancelled(cancellation)?;
        Ok(VerifiedInstallationCandidate {
            candidate,
            cancellation: cancellation.clone(),
            smoke,
        })
    }

    fn verify_installation_candidate_blocking(
        &self,
        candidate: InstallationCandidate,
        cancellation: &InstallCancellation,
    ) -> Result<InstallSmoke> {
        let runtime_model = RuntimeModel {
            id: candidate.model_id,
            path: candidate.model_path,
            format: candidate.artifact_format,
            expected_size_bytes: candidate.expected_size_bytes,
            expected_sha256: candidate.expected_sha256,
        };
        // The smoke helper itself remains lightweight and launches a fresh,
        // disposable unified inference child so regular warm state is never
        // loaded, unloaded, or invalidated by installation verification.
        let worker = RuntimeWorker::new_process();
        let preference = self.config.performance.acceleration_preference;
        ensure_install_not_cancelled(cancellation)?;

        let health_started = Instant::now();
        worker
            .health_check(runtime_model.clone(), preference)
            .map_err(|error| anyhow!("staged runtime health check failed: {error}"))?;
        let health_duration_ms = health_started.elapsed().as_millis();
        ensure_install_not_cancelled(cancellation)?;

        let load_started = Instant::now();
        let load = worker
            .load(runtime_model.clone(), preference)
            .map_err(|error| anyhow!("staged model load failed: {error}"))?;
        let load_duration_ms = load_started.elapsed().as_millis();
        ensure_install_not_cancelled(cancellation)?;

        let audio = Arc::new(PreparedAudio::from_captured_mono(
            vec![0.0; PREPARED_SAMPLE_RATE as usize],
            PREPARED_SAMPLE_RATE,
            1,
            PREPARED_SAMPLE_RATE as usize,
        )?);
        let decode_started = Instant::now();
        worker
            .transcribe(
                runtime_model.clone(),
                preference,
                Arc::clone(&audio),
                TranscriptionOptions::default(),
                worker.cancellation_snapshot(),
            )
            .map_err(|error| anyhow!("staged transcription smoke failed: {error}"))?;
        let decode_duration_ms = decode_started.elapsed().as_millis();
        ensure_install_not_cancelled(cancellation)?;

        let stale_generation = worker.cancellation_snapshot();
        worker.cancel_active();
        let cancellation_error = worker
            .transcribe(
                runtime_model.clone(),
                preference,
                audio,
                TranscriptionOptions::default(),
                stale_generation,
            )
            .err()
            .ok_or_else(|| anyhow!("staged cancellation smoke unexpectedly accepted stale work"))?;
        if !cancellation_error
            .to_string()
            .to_ascii_lowercase()
            .contains("cancel")
        {
            return Err(anyhow!(
                "staged cancellation smoke returned an unexpected error: {cancellation_error}"
            ));
        }
        ensure_install_not_cancelled(cancellation)?;

        worker
            .unload()
            .map_err(|error| anyhow!("staged runtime unload failed: {error}"))?;
        let reload_started = Instant::now();
        worker
            .load(runtime_model, preference)
            .map_err(|error| anyhow!("staged model reload failed: {error}"))?;
        let reload_duration_ms = reload_started.elapsed().as_millis();
        worker
            .unload()
            .map_err(|error| anyhow!("staged runtime final unload failed: {error}"))?;

        Ok(InstallSmoke {
            resolved_acceleration: load.diagnostics.resolved_acceleration,
            detected_architecture: load.detected_architecture,
            capabilities: load.capabilities,
            health_duration_ms,
            load_duration_ms,
            decode_duration_ms,
            reload_duration_ms,
            cancellation_verified: true,
        })
    }

    /// Transcribes canonical prepared audio. The router receives the first
    /// opportunity to handle every model; unretired providers remain behind a
    /// private compatibility bridge until Phase 11 retirement evidence exists.
    pub fn transcribe(&self, request: TranscriptionRequest) -> Result<TranscriptionOutcome> {
        let task = self.begin_transcription_task()?;
        self.transcribe_task(request, task)
    }

    /// Starts the fixed Phase 7 rolling-preview scheduler for a primary native
    /// model. This is batch preview, not a native streaming capability claim.
    pub(crate) fn start_rolling_preview(
        &self,
        session_id: SessionId,
        request_id: RequestId,
        model_id: ModelId,
        model_path: Option<PathBuf>,
    ) -> Result<(PreviewAudioPublisher, RollingPreviewHandle)> {
        if !crate::runtime_contract::handles_model_id(&model_id)
            && config::remote_gguf_artifact(&self.config, model_id.as_str()).is_none()
            && config::imported_gguf_artifact(&self.config, model_id.as_str()).is_none()
        {
            return Err(anyhow!(
                "rolling preview is unavailable for this model's verified native runtime"
            ));
        }
        // Resolve before capture starts so a missing artifact degrades to the
        // final-only path instead of emitting repeated asynchronous errors.
        self.resolve_runtime_model(self.resolve_model(&model_id, model_path.clone())?)?;
        let preview_options =
            PreviewDecodeOptions::for_capabilities(&self.capabilities_for(&model_id)?);

        let identity = StreamIdentity {
            session_id,
            request_id,
            model_id: model_id.clone(),
            sequence: 0,
        };
        let service = self.clone();
        let cancellation = service.clone();
        let mut stabilizer = TranscriptStabilizer::new(session_id, request_id, model_id.clone());
        let decode_model_id = model_id.clone();
        let session = RollingPreviewSession::new_with_cancel(
            move |snapshot| {
                let hypothesis_identity = snapshot.identity.clone();
                let window_start_frame = snapshot.window_start_frame;
                let window_end_frame = snapshot.window_end_frame;
                let mut request = TranscriptionRequest::new(
                    session_id,
                    request_id,
                    Arc::clone(&snapshot.audio),
                    decode_model_id.clone(),
                );
                request.model_path = model_path.clone();
                request.options = preview_options.transcription_options();
                let outcome = service.transcribe_preview(request)?;
                let hypothesis = transcript_hypothesis(
                    hypothesis_identity,
                    window_start_frame,
                    window_end_frame,
                    &outcome.transcript,
                    preview_options,
                );
                let state = stabilizer
                    .push(hypothesis)
                    .map_err(|error| anyhow!(error))?;
                Ok(StreamUpdate {
                    committed: state.committed,
                    tentative: state.tentative,
                })
            },
            move || cancellation.cancel_active(),
        )
        .map_err(|error| anyhow!("failed to start rolling preview worker: {error}"))?;
        let publisher = session.audio_publisher(session_id, request_id, model_id);
        Ok((publisher, RollingPreviewHandle { identity, session }))
    }

    /// Runs one rolling-preview batch strictly through the primary native
    /// router path. Preview must never fall back to a CLI/process adapter,
    /// because repeated filesystem/process work would violate the bounded
    /// native preview contract. The final full-utterance request keeps its
    /// existing compatibility fallback behavior.
    pub(crate) fn transcribe_preview(
        &self,
        request: TranscriptionRequest,
    ) -> Result<TranscriptionOutcome> {
        let task = self.begin_transcription_task()?;
        let ticket = task.ticket;
        if self.worker.cancellation_snapshot() != ticket.native_generation {
            return Err(anyhow!(
                "rolling preview was cancelled before native dispatch"
            ));
        }
        if !crate::runtime_contract::handles_model_id(&request.model_id)
            && config::remote_gguf_artifact(&self.config, request.model_id.as_str()).is_none()
            && config::imported_gguf_artifact(&self.config, request.model_id.as_str()).is_none()
        {
            return Err(anyhow!(
                "rolling preview is unavailable for this model's verified native runtime"
            ));
        }
        validate_preview_options(&request.options)?;
        let model = self.resolve_model(&request.model_id, request.model_path.clone())?;
        let runtime_model = self.resolve_runtime_model(model.clone())?;
        let execution = self
            .worker
            .transcribe(
                runtime_model,
                self.config.performance.acceleration_preference,
                Arc::clone(&request.audio),
                request.options.clone(),
                ticket.native_generation,
            )
            .map_err(|error| anyhow!(error))?;
        Ok(map_native_execution(request, model, execution))
    }

    #[cfg(test)]
    pub fn transcribe_with_ticket(
        &self,
        request: TranscriptionRequest,
        ticket: TranscriptionTicket,
    ) -> Result<TranscriptionOutcome> {
        let registration = crate::stt::register_cancellable_request(ticket.process_generation)
            .map_err(|error| anyhow!(error))?;
        self.transcribe_task(
            request,
            TranscriptionTask {
                ticket,
                _registration: registration,
            },
        )
    }

    pub fn transcribe_task(
        &self,
        request: TranscriptionRequest,
        task: TranscriptionTask,
    ) -> Result<TranscriptionOutcome> {
        let ticket = task.ticket;
        if self.worker.cancellation_snapshot() != ticket.native_generation {
            return Err(anyhow!(
                "transcription request was cancelled before dispatch"
            ));
        }
        let model = self.resolve_model(&request.model_id, request.model_path.clone())?;
        if let Some(root) = config::installed_onnx_bundle_root(&self.config, &request.model_id) {
            if request
                .model_path
                .as_ref()
                .is_some_and(|path| path != &root)
            {
                return Err(anyhow!(
                    "selected ONNX bundle path is not its canonical receipt root"
                ));
            }
            validate_default_options(&request.options)?;
            let artifact = self.onnx_artifact_from_receipt(&root)?;
            let execution = self
                .worker
                .transcribe(
                    artifact,
                    AccelerationPreference::Cpu,
                    Arc::clone(&request.audio),
                    request.options.clone(),
                    ticket.native_generation,
                )
                .map_err(|error| anyhow!(error))?;
            return Ok(map_native_execution(request, model, execution));
        }
        if crate::runtime_contract::handles_model_id(&request.model_id)
            || config::remote_gguf_artifact(&self.config, request.model_id.as_str()).is_some()
            || config::imported_gguf_artifact(&self.config, request.model_id.as_str()).is_some()
        {
            return self.transcribe_primary(request, model, ticket);
        }

        Err(anyhow!(
            "model {} has no supported native transcription runtime",
            request.model_id
        ))
    }

    fn transcribe_primary(
        &self,
        request: TranscriptionRequest,
        model: SttModelInfo,
        ticket: TranscriptionTicket,
    ) -> Result<TranscriptionOutcome> {
        validate_default_options(&request.options)?;
        let runtime_model = self.resolve_runtime_model(model.clone())?;
        self.worker
            .transcribe(
                runtime_model,
                self.config.performance.acceleration_preference,
                Arc::clone(&request.audio),
                request.options.clone(),
                ticket.native_generation,
            )
            .map(|execution| map_native_execution(request, model, execution))
            .map_err(|error| anyhow!(error))
    }

    fn resolve_runtime_model(&self, model: SttModelInfo) -> Result<RuntimeModel> {
        let path = model
            .local_path
            .clone()
            .ok_or_else(|| anyhow!("download {} before transcribing", model.name))?;
        let model_id = ModelId::new(model.id.clone());
        let remote_artifact = config::remote_gguf_artifact(&self.config, &model.id);
        let imported_artifact = config::imported_gguf_artifact(&self.config, &model.id);
        let (artifact_format, expected_size_bytes, expected_sha256) =
            if let Some(artifact) = remote_artifact {
                (
                    ArtifactFormat::Gguf,
                    artifact.expected_size_bytes,
                    artifact.expected_sha256,
                )
            } else if let Some(artifact) = imported_artifact {
                (
                    ArtifactFormat::Gguf,
                    artifact.expected_size_bytes,
                    artifact.expected_sha256,
                )
            } else {
                let artifact =
                    runtime_artifact_manifest_for_path(&model_id, &path).ok_or_else(|| {
                        anyhow!(
                            "model {} has no pinned size and SHA-256 evidence for {}",
                            model.name,
                            path.display()
                        )
                    })?;
                (
                    artifact.format,
                    artifact.size_bytes,
                    artifact.sha256.to_owned(),
                )
            };
        Ok(RuntimeModel {
            id: model.id.into(),
            path,
            format: artifact_format,
            expected_size_bytes,
            expected_sha256,
        })
    }

    fn resolve_model(
        &self,
        model_id: &ModelId,
        model_path: Option<PathBuf>,
    ) -> Result<SttModelInfo> {
        let mut model = config::configured_models(&self.config)
            .into_iter()
            .find(|model| model.id == model_id.as_str())
            .ok_or_else(|| anyhow!("unknown configured transcription model: {model_id}"))?;

        if let Some(model_path) = model_path {
            model.local_path = Some(model_path);
        }

        Ok(model)
    }
}

/// Runs the isolated install-smoke entrypoint before the desktop framework is
/// initialized. The parent process owns the deadline and can terminate this
/// process on cancellation, native crashes, or hangs.
pub(crate) fn maybe_run_installation_smoke_helper() -> Option<i32> {
    let mut args = std::env::args_os();
    let _program = args.next();
    let mode = args.next()?;
    let isolated_parent = mode == std::ffi::OsStr::new(INSTALL_SMOKE_PARENT_FLAG);
    if !isolated_parent && mode != std::ffi::OsStr::new(INSTALL_SMOKE_HELPER_FLAG) {
        return None;
    }
    if !isolated_parent {
        suppress_native_smoke_crash_dialogs();
    }
    let result = (|| -> Result<InstallSmoke> {
        let model_id = args
            .next()
            .ok_or_else(|| anyhow!("missing model ID"))?
            .into_string()
            .map_err(|_| anyhow!("model ID is not valid Unicode"))?;
        let model_path = args.next().ok_or_else(|| anyhow!("missing model path"))?;
        let artifact_format = args
            .next()
            .ok_or_else(|| anyhow!("missing artifact format"))?
            .into_string()
            .map_err(|_| anyhow!("artifact format is not valid Unicode"))?;
        let artifact_format = match artifact_format.as_str() {
            "gguf" => ArtifactFormat::Gguf,
            _ => return Err(anyhow!("invalid artifact format")),
        };
        let expected_size_bytes = args
            .next()
            .ok_or_else(|| anyhow!("missing expected model size"))?
            .into_string()
            .map_err(|_| anyhow!("expected model size is not valid Unicode"))?
            .parse::<u64>()
            .map_err(|_| anyhow!("expected model size is invalid"))?;
        let expected_sha256 = args
            .next()
            .ok_or_else(|| anyhow!("missing expected model SHA-256"))?
            .into_string()
            .map_err(|_| anyhow!("expected model SHA-256 is not valid Unicode"))?;
        if expected_sha256.len() != 64
            || !expected_sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(anyhow!("expected model SHA-256 is invalid"));
        }
        let acceleration = args
            .next()
            .ok_or_else(|| anyhow!("missing acceleration preference"))?
            .into_string()
            .map_err(|_| anyhow!("acceleration preference is not valid Unicode"))?;
        if args.next().is_some() {
            return Err(anyhow!("unexpected installation smoke arguments"));
        }
        let acceleration = match acceleration.as_str() {
            "auto" => AccelerationPreference::Auto,
            "cpu" => AccelerationPreference::Cpu,
            "gpu" => AccelerationPreference::Gpu,
            _ => return Err(anyhow!("invalid acceleration preference")),
        };
        let mut config = AppConfig::default();
        config.performance.acceleration_preference = acceleration;
        let service = TranscriptionService::new(config);
        let candidate = InstallationCandidate::pinned(
            ModelId::new(model_id),
            PathBuf::from(model_path),
            artifact_format,
            expected_size_bytes,
            expected_sha256,
        );
        if isolated_parent {
            service.verify_installation_candidate(candidate, &InstallCancellation::default())
        } else {
            service
                .verify_installation_candidate_blocking(candidate, &InstallCancellation::default())
        }
    })();
    match result {
        Ok(smoke) => match serde_json::to_writer(std::io::stdout().lock(), &smoke) {
            Ok(()) => Some(0),
            Err(error) => {
                eprintln!("could not serialize installation smoke diagnostics: {error}");
                Some(1)
            }
        },
        Err(error) => {
            eprintln!("{error:#}");
            Some(1)
        }
    }
}

#[cfg(windows)]
fn suppress_native_smoke_crash_dialogs() {
    use windows_sys::Win32::System::Diagnostics::Debug::{
        SEM_FAILCRITICALERRORS, SEM_NOGPFAULTERRORBOX, SetErrorMode,
    };

    unsafe {
        SetErrorMode(SEM_FAILCRITICALERRORS | SEM_NOGPFAULTERRORBOX);
    }
}

#[cfg(not(windows))]
fn suppress_native_smoke_crash_dialogs() {}

fn verify_runtime_model_artifact(runtime_model: &RuntimeModel) -> Result<()> {
    crate::installations::verify_file(
        &runtime_model.path,
        runtime_model.expected_size_bytes,
        &runtime_model.expected_sha256,
    )
    .map_err(|error| anyhow!("model integrity verification failed: {error}"))
}

#[cfg(test)]
fn model_uses_embedded_gguf(model_id: &ModelId) -> bool {
    crate::model_catalog::model_uses_embedded_runtime(model_id)
}

fn ensure_install_not_cancelled(cancellation: &InstallCancellation) -> Result<()> {
    if cancellation.is_cancelled() {
        Err(anyhow!("installation verification was cancelled"))
    } else {
        Ok(())
    }
}

fn map_native_execution(
    request: TranscriptionRequest,
    model: SttModelInfo,
    execution: RuntimeExecution,
) -> TranscriptionOutcome {
    TranscriptionOutcome {
        session_id: request.session_id,
        request_id: request.request_id,
        model_id: request.model_id,
        model_name: model.name,
        backend_label: "transcribe-cpp".to_owned(),
        transcript: execution.transcript,
        processing_duration_ms: Some(execution.processing_duration_ms),
        resolved_acceleration: Some(execution.diagnostics.resolved_acceleration),
        model_load_duration_ms: Some(execution.diagnostics.model_load_duration_ms),
        warm_model_reused: execution.diagnostics.warm_reused,
        stdout: String::new(),
        stderr: format!(
            "runtime={} warm_reused={}",
            execution.diagnostics.runtime_location.display(),
            execution.diagnostics.warm_reused
        ),
    }
}

fn validate_default_options(options: &TranscriptionOptions) -> Result<()> {
    if options.language.is_some() {
        return Err(anyhow!(
            "language selection is not supported by the selected transcription capability"
        ));
    }
    if options.translate_to_english {
        return Err(anyhow!(
            "translation is not supported by the selected transcription capability"
        ));
    }
    if options.enable_timestamps {
        return Err(anyhow!(
            "requesting timestamps is not supported as a decoding option by the selected transcription capability"
        ));
    }
    if options.initial_prompt.is_some() {
        return Err(anyhow!(
            "initial prompts are not supported by the selected transcription capability"
        ));
    }

    Ok(())
}

fn validate_preview_options(options: &TranscriptionOptions) -> Result<()> {
    let options_without_timestamps = TranscriptionOptions {
        enable_timestamps: false,
        ..options.clone()
    };
    validate_default_options(&options_without_timestamps)
}

fn intersect_capabilities(
    runtime: &RuntimeCapabilities,
    descriptor: &ModelDescriptor,
) -> RuntimeCapabilities {
    let manifest = descriptor.capabilities;
    RuntimeCapabilities {
        streaming: runtime.streaming && manifest.native_streaming,
        cancellation: runtime.cancellation && manifest.cancellation,
        translation: runtime.translation && manifest.translation,
        timestamps: runtime.timestamps && manifest.timestamps,
        language_detection: runtime.language_detection && manifest.language_detection,
        confidence_scores: runtime.confidence_scores && manifest.confidence_scores,
        custom_vocabulary: runtime.custom_vocabulary && manifest.custom_vocabulary,
        supported_languages: runtime
            .supported_languages
            .iter()
            .filter(|language| {
                descriptor
                    .languages
                    .iter()
                    .any(|allowed| allowed == &language.as_str())
            })
            .cloned()
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime_artifact::{OnnxFileRole, OnnxModelFamily};
    use crate::runtime_router::NativeRuntimeDiagnostics;
    use sha2::{Digest, Sha256};
    use std::io::Cursor;

    const MAX_DIAGNOSTIC_ONNX_WAV_BYTES: u64 = 256 * 1024 * 1024;

    fn prepared_audio() -> Arc<PreparedAudio> {
        Arc::new(PreparedAudio {
            samples: vec![0.0; 160],
            sample_rate: 16_000,
            source_sample_rate: 16_000,
            source_channels: 1,
            source_frames: 160,
        })
    }

    fn normalize_fixture_transcript(value: &str) -> String {
        value
            .chars()
            .map(|character| {
                if character.is_alphanumeric() {
                    character.to_ascii_lowercase()
                } else {
                    ' '
                }
            })
            .collect::<String>()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn decode_digest_pinned_diagnostic_wav_with_hook(
        path: &Path,
        expected_sha256: &str,
        after_verified_read: impl FnOnce(),
    ) -> Result<PreparedAudio> {
        anyhow::ensure!(
            expected_sha256.len() == 64
                && expected_sha256
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
            "diagnostic WAV SHA-256 must be exactly 64 lowercase hexadecimal characters"
        );
        let bytes =
            crate::installations::read_regular_file_no_follow(path, MAX_DIAGNOSTIC_ONNX_WAV_BYTES)?;
        anyhow::ensure!(!bytes.is_empty(), "diagnostic WAV fixture is empty");
        let actual_sha256 = format!("{:x}", Sha256::digest(&bytes));
        anyhow::ensure!(
            actual_sha256 == expected_sha256,
            "diagnostic WAV fixture checksum mismatch: expected {expected_sha256}, got {actual_sha256}"
        );
        after_verified_read();
        PreparedAudio::from_wav_reader(Cursor::new(bytes))
    }

    fn diagnostic_wav_bytes(samples: &[i16]) -> Vec<u8> {
        let mut cursor = Cursor::new(Vec::new());
        {
            let mut writer = hound::WavWriter::new(
                &mut cursor,
                hound::WavSpec {
                    channels: 1,
                    sample_rate: PREPARED_SAMPLE_RATE,
                    bits_per_sample: 16,
                    sample_format: hound::SampleFormat::Int,
                },
            )
            .unwrap();
            for sample in samples {
                writer.write_sample(*sample).unwrap();
            }
            writer.finalize().unwrap();
        }
        cursor.into_inner()
    }

    #[test]
    fn diagnostic_wav_decode_is_bound_to_verified_bytes_not_a_reopened_path() {
        let root = std::env::temp_dir().join(format!(
            "scribe-diagnostic-wav-binding-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        let path = root.join("fixture.wav");
        let original = diagnostic_wav_bytes(&[i16::MAX / 2, i16::MAX / 4]);
        let replacement = diagnostic_wav_bytes(&[-i16::MAX / 2, -i16::MAX / 4]);
        fs::write(&path, &original).unwrap();
        let expected_sha256 = format!("{:x}", Sha256::digest(&original));
        let moved = root.join("verified-original.wav");

        let audio = decode_digest_pinned_diagnostic_wav_with_hook(&path, &expected_sha256, {
            let path = path.clone();
            let moved = moved.clone();
            let replacement = replacement.clone();
            move || {
                fs::rename(&path, &moved).unwrap();
                fs::write(&path, replacement).unwrap();
            }
        })
        .unwrap();

        assert!(audio.samples[0] > 0.0);
        assert_eq!(fs::read(&path).unwrap(), replacement);
        fs::remove_dir_all(root).unwrap();
    }

    // Developer diagnostic only. It is deliberately non-promotional and
    // cannot support a compatibility claim unless a canonical WAV digest and
    // normalized transcript later become versioned repository evidence.
    #[test]
    #[ignore = "non-promotional diagnostic: downloads an exact pinned Hugging Face bundle and requires a digest-pinned spoken WAV fixture"]
    fn diagnostic_real_hugging_face_bundle_install_load_and_decode() {
        if std::env::var("SCRIBE_ONNX_BUNDLE_TEST").as_deref() != Ok("1") {
            return;
        }
        let model_id = std::env::var("SCRIBE_ONNX_BUNDLE_MODEL_ID")
            .unwrap_or_else(|_| "moonshine-tiny-en-int8-onnx".to_owned());
        let storage = PathBuf::from(
            std::env::var_os("SCRIBE_ONNX_BUNDLE_STORAGE_DIR")
                .expect("set SCRIBE_ONNX_BUNDLE_STORAGE_DIR to a dedicated test directory"),
        );
        let audio_path = PathBuf::from(
            std::env::var_os("SCRIBE_ONNX_BUNDLE_WAV")
                .expect("set SCRIBE_ONNX_BUNDLE_WAV to a known spoken PCM WAV"),
        );
        let expected_wav_sha256 = std::env::var("SCRIBE_ONNX_BUNDLE_WAV_SHA256")
            .expect("set SCRIBE_ONNX_BUNDLE_WAV_SHA256 to the exact lowercase WAV SHA-256");
        let expected_text = std::env::var("SCRIBE_ONNX_BUNDLE_EXPECTED_TRANSCRIPT")
            .expect("set SCRIBE_ONNX_BUNDLE_EXPECTED_TRANSCRIPT to the required spoken text");
        let worker_executable = PathBuf::from(
            std::env::var_os("SCRIBE_ONNX_WORKER_EXE")
                .expect("set SCRIBE_ONNX_WORKER_EXE to a separately built Scribe executable"),
        );
        assert!(
            worker_executable.is_file(),
            "SCRIBE_ONNX_WORKER_EXE must name an existing Scribe executable"
        );
        assert!(
            !normalize_fixture_transcript(&expected_text).is_empty(),
            "the required expected transcript must contain letters or numbers"
        );
        let cancellation = InstallCancellation::default();
        let audio = Arc::new(
            decode_digest_pinned_diagnostic_wav_with_hook(&audio_path, &expected_wav_sha256, || {})
                .expect(
                    "the configured WAV fixture must match its digest and decode from exact bytes",
                ),
        );
        fs::create_dir_all(&storage).unwrap();
        let staged = crate::onnx_model_bundles::stage_onnx_bundle_install(
            &model_id,
            &storage,
            &cancellation,
            &|_| {},
        )
        .unwrap();
        let mut config = AppConfig::default();
        config.performance.acceleration_preference = AccelerationPreference::Cpu;
        let service = TranscriptionService::with_process_executable(config, worker_executable);
        let verified = service
            .verify_onnx_bundle_for_installation(staged, &cancellation)
            .unwrap();
        assert_eq!(
            verified.smoke().resolved_acceleration.resolved,
            ComputeDevice::Cpu
        );
        let execution_result = service.transcribe_onnx_bundle_from_receipt(
            verified.root(),
            audio,
            TranscriptionOptions::default(),
        );
        let unload_result = service.unload_runtime_artifacts();
        let execution = match (execution_result, unload_result) {
            (Ok(execution), Ok(())) => execution,
            (Err(error), _) => {
                let _ = verified.discard();
                panic!("pre-activation fixture decode failed: {error:#}");
            }
            (Ok(_), Err(error)) => {
                let _ = verified.discard();
                panic!("pre-activation fixture unload failed: {error:#}");
            }
        };
        let actual = normalize_fixture_transcript(&execution.transcript.text);
        let expected = normalize_fixture_transcript(&expected_text);
        if actual != expected {
            let _ = verified.discard();
            panic!(
                "fixture transcript must equal the required normalized expected text before activation: expected {expected:?}, got {actual:?}"
            );
        }
        let activated = verified.activate().unwrap();
        activated.commit().unwrap();
    }

    #[test]
    fn preview_hypothesis_preserves_and_offsets_native_segment_timing() {
        let identity = StreamIdentity {
            session_id: SessionId(1),
            request_id: RequestId(2),
            model_id: ModelId::new("whisper_cpp_base_en"),
            sequence: 3,
        };
        let transcript = Transcript {
            text: "hello timed world".to_owned(),
            segments: vec![
                TranscriptSegment {
                    text: "hello timed".to_owned(),
                    start_ms: Some(100),
                    end_ms: Some(500),
                    confidence: None,
                },
                TranscriptSegment {
                    text: "world".to_owned(),
                    start_ms: None,
                    end_ms: None,
                    confidence: None,
                },
            ],
            detected_language: None,
            duration_ms: Some(1_000),
        };

        let hypothesis = transcript_hypothesis(
            identity,
            16_000,
            32_000,
            &transcript,
            PreviewDecodeOptions {
                use_segment_timestamps: true,
            },
        );

        assert_eq!(hypothesis.words.len(), 3);
        assert_eq!(hypothesis.words[0].start_frame, Some(17_600));
        assert_eq!(hypothesis.words[0].end_frame, Some(20_800));
        assert_eq!(hypothesis.words[1].start_frame, Some(20_800));
        assert_eq!(hypothesis.words[1].end_frame, Some(24_000));
        assert_eq!(hypothesis.words[2].start_frame, None);
        assert_eq!(hypothesis.words[2].end_frame, None);
    }

    #[test]
    fn preview_hypothesis_falls_back_to_full_text_without_segments() {
        let identity = StreamIdentity {
            session_id: SessionId(1),
            request_id: RequestId(2),
            model_id: ModelId::new("whisper_cpp_base_en"),
            sequence: 3,
        };
        let transcript = Transcript {
            text: "fallback words".to_owned(),
            segments: Vec::new(),
            detected_language: None,
            duration_ms: None,
        };

        let hypothesis = transcript_hypothesis(
            identity,
            0,
            16_000,
            &transcript,
            PreviewDecodeOptions::default(),
        );

        assert_eq!(
            hypothesis
                .words
                .iter()
                .map(|word| word.display.as_str())
                .collect::<Vec<_>>(),
            vec!["fallback", "words"]
        );
        assert!(
            hypothesis
                .words
                .iter()
                .all(|word| word.start_frame.is_none() && word.end_frame.is_none())
        );
    }

    #[test]
    fn capable_preview_route_requests_and_accepts_segment_timestamps() {
        let preview = PreviewDecodeOptions::for_capabilities(&RuntimeCapabilities {
            timestamps: true,
            ..RuntimeCapabilities::default()
        });

        let request_options = preview.transcription_options();

        assert!(preview.use_segment_timestamps);
        assert!(request_options.enable_timestamps);
        assert!(validate_preview_options(&request_options).is_ok());
    }

    #[test]
    fn incapable_preview_route_keeps_default_text_only_options_and_fallback() {
        let preview = PreviewDecodeOptions::for_capabilities(&RuntimeCapabilities::default());
        let request_options = preview.transcription_options();
        assert!(!preview.use_segment_timestamps);
        assert_eq!(request_options, TranscriptionOptions::default());
        assert!(validate_preview_options(&request_options).is_ok());

        let transcript = Transcript {
            text: "fallback words".to_owned(),
            segments: vec![TranscriptSegment {
                text: "fallback words".to_owned(),
                start_ms: Some(100),
                end_ms: Some(500),
                confidence: None,
            }],
            detected_language: None,
            duration_ms: Some(1_000),
        };
        let hypothesis = transcript_hypothesis(
            StreamIdentity {
                session_id: SessionId(1),
                request_id: RequestId(2),
                model_id: ModelId::new("untimed-preview-model"),
                sequence: 1,
            },
            16_000,
            32_000,
            &transcript,
            preview,
        );

        assert!(
            hypothesis
                .words
                .iter()
                .all(|word| { word.start_frame.is_none() && word.end_frame.is_none() })
        );
    }

    #[test]
    fn public_final_route_keeps_timestamps_disabled_and_rejects_an_explicit_request() {
        assert!(!TranscriptionOptions::default().enable_timestamps);
        let timestamp_request = TranscriptionOptions {
            enable_timestamps: true,
            ..TranscriptionOptions::default()
        };

        assert!(validate_default_options(&timestamp_request).is_err());
        assert!(validate_preview_options(&timestamp_request).is_ok());
    }

    #[test]
    fn imported_gguf_is_not_a_normalized_install_plan() {
        let root = std::env::temp_dir().join(format!(
            "scribe-imported-gguf-service-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let model_path = root.join("external").join("imported.gguf");
        std::fs::create_dir_all(model_path.parent().unwrap()).unwrap();
        std::fs::write(&model_path, b"fixture").unwrap();
        let sha256 = format!("{:x}", Sha256::digest(b"fixture"));
        let id = config::imported_gguf_model_id(&sha256).unwrap();
        let mut config = AppConfig::default();
        config.general.model_storage_dir = root.join("scribe-storage");
        config.general.imported_gguf_models.insert(
            id.clone(),
            config::ImportedGgufModelInstall::validated(
                std::fs::canonicalize(&model_path).unwrap(),
                7,
                sha256,
                "Imported fixture".to_owned(),
            ),
        );
        config::normalize_config(&mut config);
        let service = TranscriptionService::new(config);

        assert_eq!(service.install_plan(&ModelId::new(id)), None);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn trusted_remote_gguf_is_not_a_normalized_install_plan() {
        let root =
            std::env::temp_dir().join(format!("scribe-remote-gguf-service-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let repository = "handy-computer/example-asr-gguf";
        let revision = "0123456789abcdef0123456789abcdef01234567";
        let filename = "example-Q4_K_M.gguf";
        let id = config::managed_remote_model_id(repository, revision, filename).unwrap();
        let path = root.join(filename);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(&path, b"fixture").unwrap();
        let mut config = AppConfig::default();
        config.general.managed_remote_models.insert(
            id.clone(),
            config::ManagedRemoteModelInstall::trusted(
                config::RemoteGgufArtifact {
                    repository: repository.to_owned(),
                    revision: revision.to_owned(),
                    filename: filename.to_owned(),
                    expected_size_bytes: 7,
                    expected_sha256: format!("{:x}", Sha256::digest(b"fixture")),
                },
                path,
                "Remote fixture".to_owned(),
                "Trusted remote fixture".to_owned(),
                vec!["en".to_owned()],
                false,
            ),
        );
        let service = TranscriptionService::new(config);

        assert_eq!(service.install_plan(&ModelId::new(id)), None);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn normalized_catalog_exposes_only_neutral_experimental_descriptors() {
        let service = TranscriptionService::new(AppConfig::default());
        let descriptors = service.model_descriptors();

        assert_eq!(descriptors.len(), 5);
        assert!(
            descriptors
                .iter()
                .any(|descriptor| descriptor.id.as_str() == "moonshine-tiny-en-int8-onnx")
        );
        for descriptor in descriptors {
            assert!(matches!(
                descriptor.compatibility,
                CompatibilityStatus::Experimental { .. }
            ));
            assert!(descriptor.roles.is_empty());
            assert!(!descriptor.capabilities.native_streaming);
            assert!(
                descriptor
                    .languages
                    .iter()
                    .all(|language| *language == "en")
            );
        }
    }

    #[test]
    fn effective_capabilities_are_a_fail_closed_intersection() {
        let descriptor = model_descriptor(&ModelId::new("whisper_cpp_base_en")).unwrap();
        let runtime = RuntimeCapabilities {
            streaming: true,
            cancellation: true,
            translation: true,
            timestamps: true,
            language_detection: true,
            confidence_scores: true,
            custom_vocabulary: true,
            supported_languages: vec!["en".to_owned(), "fr".to_owned()],
        };

        let effective = intersect_capabilities(&runtime, &descriptor);

        assert!(!effective.streaming);
        assert!(effective.cancellation);
        assert!(!effective.translation);
        assert!(effective.timestamps);
        assert!(!effective.language_detection);
        assert!(!effective.confidence_scores);
        assert!(!effective.custom_vocabulary);
        assert_eq!(effective.supported_languages, ["en"]);
    }

    #[test]
    fn normalized_base_descriptor_is_present_in_the_normal_catalog() {
        let service = TranscriptionService::new(AppConfig::default());
        let singular = service
            .model_descriptor(&ModelId::new("whisper_cpp_base_en"))
            .unwrap();

        assert!(
            service
                .model_descriptors()
                .into_iter()
                .any(|descriptor| descriptor.id == singular.id)
        );
    }

    #[test]
    fn service_rejects_unknown_models_without_needing_a_runtime() {
        let service = TranscriptionService::new(AppConfig::default());
        let error = service
            .transcribe(TranscriptionRequest::new(
                SessionId(4),
                RequestId(9),
                prepared_audio(),
                "not-a-configured-model",
            ))
            .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("unknown configured transcription model")
        );
    }

    #[test]
    fn catalog_identifies_gguf_as_the_embedded_runtime_route() {
        assert!(model_uses_embedded_gguf(&ModelId::new(
            "whisper_cpp_tiny_en"
        )));
        assert!(model_uses_embedded_gguf(&ModelId::new(
            "whisper_cpp_base_en"
        )));
    }

    #[test]
    fn ticket_captured_before_cancellation_cannot_dispatch_later() {
        let _test_lock = crate::stt::cancellation_test_lock();
        let service = TranscriptionService::new(AppConfig::default());
        let ticket = service.transcription_ticket();
        service.cancel_active();
        let request = TranscriptionRequest::new(
            SessionId(91),
            RequestId(92),
            prepared_audio(),
            "whisper_cpp_tiny_en",
        );

        let error = service.transcribe_with_ticket(request, ticket).unwrap_err();

        assert!(error.to_string().contains("cancelled"));
    }

    #[test]
    fn registered_task_keeps_shutdown_waiting_through_audio_cleanup() {
        let _test_lock = crate::stt::cancellation_test_lock();
        let service = TranscriptionService::new(AppConfig::default());
        let task = service.begin_transcription_task().unwrap();
        let path = std::env::temp_dir().join(format!(
            "scribe-dispatch-cleanup-{}-{}.wav",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        fs::write(&path, b"pcm").unwrap();
        let worker_path = path.clone();
        let worker = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            fs::remove_file(&worker_path).unwrap();
            drop(task);
        });

        assert!(service.cancel_active_and_wait(Duration::from_secs(2)));

        worker.join().unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn model_id_exposes_a_neutral_stable_reference() {
        let model_id = ModelId::new("whisper_cpp_tiny_en");

        assert_eq!(model_id.as_str(), "whisper_cpp_tiny_en");
        assert_eq!(model_id.to_string(), "whisper_cpp_tiny_en");
        assert_eq!(model_id.into_inner(), "whisper_cpp_tiny_en");
    }

    #[test]
    fn stream_update_owns_its_value_data() {
        let original = StreamUpdate {
            committed: "settled".to_owned(),
            tentative: "draft".to_owned(),
        };
        let mut copy = original.clone();
        copy.tentative.push_str(" revision");

        assert_eq!(original.committed, "settled");
        assert_eq!(original.tentative, "draft");
        assert_eq!(copy.tentative, "draft revision");
    }

    #[test]
    fn request_and_outcome_keep_correlation_ids() {
        let request = TranscriptionRequest::new(
            SessionId(11),
            RequestId(29),
            prepared_audio(),
            "whisper_cpp_tiny_en",
        );
        let outcome = TranscriptionOutcome {
            session_id: request.session_id,
            request_id: request.request_id,
            model_id: ModelId::new("whisper_cpp_tiny_en"),
            model_name: "whisper.cpp tiny.en".to_owned(),
            backend_label: "whisper.cpp".to_owned(),
            transcript: Transcript {
                text: "done".to_owned(),
                segments: Vec::new(),
                detected_language: None,
                duration_ms: None,
            },
            processing_duration_ms: None,
            resolved_acceleration: None,
            model_load_duration_ms: None,
            warm_model_reused: false,
            stdout: String::new(),
            stderr: String::new(),
        };

        assert_eq!(outcome.session_id, SessionId(11));
        assert_eq!(outcome.request_id, RequestId(29));
    }

    #[test]
    fn last_runtime_worker_handle_synchronously_shuts_down_its_thread() {
        let worker = RuntimeWorker::new(RuntimeRouter::new());
        let retained = worker.clone();
        let weak = Arc::downgrade(&worker.inner);

        drop(worker);
        assert!(weak.upgrade().is_some());
        drop(retained);

        assert!(weak.upgrade().is_none());
    }

    fn simulated_runtime_worker<F>(worker_loop: F) -> RuntimeWorker
    where
        F: FnOnce(Receiver<RuntimeCommand>) + Send + 'static,
    {
        let (commands, receiver) = sync_channel(1);
        let worker = std::thread::spawn(move || worker_loop(receiver));
        RuntimeWorker {
            inner: Arc::new(RuntimeWorkerInner {
                commands,
                worker: Mutex::new(Some(worker)),
                shutdown_gate: Mutex::new(()),
                cancellation_generation: Arc::new(AtomicU64::new(0)),
                in_process_router: Some(RuntimeRouter::new()),
                inference: None,
            }),
        }
    }

    fn service_onnx_spec(label: &str) -> (PathBuf, OnnxModelSpec) {
        service_onnx_spec_with(
            label,
            "zipformer-streaming-en-20m-int8-onnx",
            OnnxModelFamily::OnlineTransducer,
            1,
            &[
                OnnxFileRole::Encoder,
                OnnxFileRole::Decoder,
                OnnxFileRole::Joiner,
                OnnxFileRole::Tokens,
            ],
        )
    }

    fn service_onnx_spec_with(
        label: &str,
        id: &str,
        family: OnnxModelFamily,
        num_threads: u16,
        roles: &[OnnxFileRole],
    ) -> (PathBuf, OnnxModelSpec) {
        let root = std::env::temp_dir().join(format!(
            "scribe-service-onnx-{label}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        let files = roles
            .iter()
            .copied()
            .map(|role| {
                let relative = PathBuf::from(format!("{role:?}.fixture").to_ascii_lowercase());
                fs::write(root.join(&relative), format!("{label}-{role:?}")).unwrap();
                (role, relative)
            })
            .collect();
        let spec = OnnxModelSpec {
            id: id.to_owned(),
            root: root.clone(),
            family,
            files,
            num_threads,
        };
        (root, spec)
    }

    fn test_load_execution() -> RuntimeLoadExecution {
        RuntimeLoadExecution {
            diagnostics: NativeRuntimeDiagnostics {
                resolved_acceleration: ResolvedAcceleration {
                    requested: AccelerationPreference::Cpu,
                    resolved: ComputeDevice::Cpu,
                    diagnostic: None,
                    selection: None,
                },
                runtime_location: PathBuf::from("<test-inference-child>"),
                warm_reused: false,
                model_load_duration_ms: 1,
            },
            detected_architecture: "nemo-ctc".to_owned(),
            capabilities: RuntimeCapabilities::default(),
        }
    }

    #[test]
    fn exact_current_receipt_dispatches_and_self_authored_receipt_is_rejected() {
        let (root, spec) = service_onnx_spec_with(
            "receipt-dispatch",
            "moonshine-tiny-en-int8-onnx",
            OnnxModelFamily::Moonshine,
            4,
            &[
                OnnxFileRole::Encoder,
                OnnxFileRole::MergedDecoder,
                OnnxFileRole::Tokens,
            ],
        );
        let current_manifest =
            crate::onnx_model_bundles::write_test_receipt_for_spec(&spec).unwrap();
        let (self_authored_root, self_authored_spec) = service_onnx_spec_with(
            "self-authored-receipt",
            "moonshine-tiny-en-int8-onnx",
            OnnxModelFamily::Moonshine,
            4,
            &[
                OnnxFileRole::Encoder,
                OnnxFileRole::MergedDecoder,
                OnnxFileRole::Tokens,
            ],
        );
        crate::onnx_model_bundles::write_test_receipt_for_spec(&self_authored_spec).unwrap();
        let production_error =
            crate::onnx_model_bundles::current_executable_receipt_at(&self_authored_root)
                .unwrap_err();
        assert!(
            production_error
                .to_string()
                .contains("does not match the current embedded manifest")
        );
        let expected = spec.clone();
        let worker = simulated_runtime_worker(move |receiver| {
            match receiver.recv().unwrap() {
                RuntimeCommand::Load {
                    artifact: RuntimeArtifact::OnnxBundle(actual),
                    preference,
                    reply,
                } => {
                    assert_eq!(actual, expected);
                    assert_eq!(preference, AccelerationPreference::Cpu);
                    reply.send(Ok(test_load_execution())).unwrap();
                }
                _ => panic!("receipt preload must dispatch an ONNX load command"),
            }
            match receiver.recv().unwrap() {
                RuntimeCommand::Transcribe {
                    artifact: RuntimeArtifact::OnnxBundle(actual),
                    preference,
                    reply,
                    ..
                } => {
                    assert_eq!(actual, expected);
                    assert_eq!(preference, AccelerationPreference::Cpu);
                    reply
                        .send(Ok(RuntimeExecution {
                            transcript: Transcript {
                                text: "service-boundary".to_owned(),
                                segments: Vec::new(),
                                detected_language: None,
                                duration_ms: None,
                            },
                            diagnostics: test_load_execution().diagnostics,
                            processing_duration_ms: 1,
                        }))
                        .unwrap();
                }
                _ => panic!("receipt transcription must dispatch an ONNX batch command"),
            }
            if let RuntimeCommand::Shutdown { reply } = receiver.recv().unwrap() {
                reply.send(Ok(())).unwrap();
            }
        });
        let service = TranscriptionService {
            config: AppConfig::default(),
            router: RuntimeRouter::new(),
            current_receipt_manifest: None,
            worker,
        }
        .with_test_current_receipt_manifest(current_manifest);

        let error = service
            .preload_onnx_bundle_from_receipt(&self_authored_root)
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("does not match the controlled current manifest")
        );

        service.preload_onnx_bundle_from_receipt(&root).unwrap();
        let execution = service
            .transcribe_onnx_bundle_from_receipt(
                &root,
                prepared_audio(),
                TranscriptionOptions::default(),
            )
            .unwrap();
        assert_eq!(execution.transcript.text, "service-boundary");
        drop(service);
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(self_authored_root).unwrap();
    }

    #[test]
    fn public_moonshine_operations_verify_each_receipt_once_before_dispatch() {
        let model_id = ModelId::new("moonshine-tiny-en-int8-onnx");
        let (fixture_root, mut spec) = service_onnx_spec_with(
            "public-receipt-observer",
            model_id.as_str(),
            OnnxModelFamily::Moonshine,
            4,
            &[
                OnnxFileRole::Encoder,
                OnnxFileRole::MergedDecoder,
                OnnxFileRole::Tokens,
            ],
        );
        let storage = fixture_root.with_file_name(format!(
            "scribe-service-public-receipt-storage-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let root = storage.join("onnx-bundles").join(model_id.as_str());
        fs::create_dir_all(root.parent().unwrap()).unwrap();
        fs::rename(&fixture_root, &root).unwrap();
        spec.root = root.clone();
        let current_manifest =
            crate::onnx_model_bundles::write_test_receipt_for_spec(&spec).unwrap();

        let expected = spec.clone();
        let worker_dispatches = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let worker_dispatches_for_thread = Arc::clone(&worker_dispatches);
        let worker = simulated_runtime_worker(move |receiver| {
            while let Ok(command) = receiver.recv() {
                match command {
                    RuntimeCommand::Load {
                        artifact: RuntimeArtifact::OnnxBundle(actual),
                        preference,
                        reply,
                    } => {
                        worker_dispatches_for_thread.fetch_add(1, Ordering::SeqCst);
                        assert_eq!(actual, expected);
                        assert_eq!(preference, AccelerationPreference::Cpu);
                        reply.send(Ok(test_load_execution())).unwrap();
                    }
                    RuntimeCommand::Health {
                        artifact: RuntimeArtifact::OnnxBundle(actual),
                        preference,
                        reply,
                    } => {
                        worker_dispatches_for_thread.fetch_add(1, Ordering::SeqCst);
                        assert_eq!(actual, expected);
                        assert_eq!(preference, AccelerationPreference::Cpu);
                        reply.send(Ok(())).unwrap();
                    }
                    RuntimeCommand::Transcribe {
                        artifact: RuntimeArtifact::OnnxBundle(actual),
                        preference,
                        reply,
                        ..
                    } => {
                        worker_dispatches_for_thread.fetch_add(1, Ordering::SeqCst);
                        assert_eq!(actual, expected);
                        assert_eq!(preference, AccelerationPreference::Cpu);
                        reply
                            .send(Ok(RuntimeExecution {
                                transcript: Transcript {
                                    text: "public-operation".to_owned(),
                                    segments: Vec::new(),
                                    detected_language: None,
                                    duration_ms: None,
                                },
                                diagnostics: test_load_execution().diagnostics,
                                processing_duration_ms: 1,
                            }))
                            .unwrap();
                    }
                    RuntimeCommand::Shutdown { reply } => {
                        reply.send(Ok(())).unwrap();
                        break;
                    }
                    _ => panic!("unexpected public ONNX command"),
                }
            }
        });
        let mut config = AppConfig::default();
        config.general.model_storage_dir = storage.clone();
        let service = TranscriptionService {
            config,
            router: RuntimeRouter::new(),
            current_receipt_manifest: None,
            worker,
        }
        .with_test_current_receipt_manifest(current_manifest);

        let (preload, preload_stats) =
            crate::onnx_model_bundles::observe_receipt_verifications_for_test(|| {
                service.preload_model(&model_id, None)
            });
        let (health, health_stats) =
            crate::onnx_model_bundles::observe_receipt_verifications_for_test(|| {
                service.health_check(&model_id, None)
            });
        let (transcribe, transcribe_stats) =
            crate::onnx_model_bundles::observe_receipt_verifications_for_test(|| {
                service.transcribe(TranscriptionRequest::new(
                    SessionId(730),
                    RequestId(731),
                    prepared_audio(),
                    model_id.clone(),
                ))
            });
        preload.unwrap();
        health.unwrap();
        transcribe.unwrap();
        for stats in [&preload_stats, &health_stats, &transcribe_stats] {
            assert_eq!(stats.calls, 1);
            assert!(stats.verified_bytes > 0);
            assert_eq!(stats.durations.len(), 1);
        }
        assert_eq!(worker_dispatches.load(Ordering::SeqCst), 3);

        fs::write(root.join(&spec.files[&OnnxFileRole::Encoder]), b"tampered").unwrap();
        let (tampered, tampered_stats) =
            crate::onnx_model_bundles::observe_receipt_verifications_for_test(|| {
                service.preload_model(&model_id, None)
            });
        assert!(tampered.is_err());
        assert_eq!(tampered_stats.calls, 1);
        assert_eq!(tampered_stats.verified_bytes, 0);
        assert_eq!(tampered_stats.durations.len(), 1);
        assert_eq!(worker_dispatches.load(Ordering::SeqCst), 3);

        let (tampered_health, tampered_health_stats) =
            crate::onnx_model_bundles::observe_receipt_verifications_for_test(|| {
                service.health_check(&model_id, None)
            });
        assert!(tampered_health.is_err());
        assert_eq!(tampered_health_stats.calls, 1);
        assert_eq!(tampered_health_stats.verified_bytes, 0);
        assert_eq!(tampered_health_stats.durations.len(), 1);
        assert_eq!(worker_dispatches.load(Ordering::SeqCst), 3);

        let (tampered_transcribe, tampered_transcribe_stats) =
            crate::onnx_model_bundles::observe_receipt_verifications_for_test(|| {
                service.transcribe(TranscriptionRequest::new(
                    SessionId(732),
                    RequestId(733),
                    prepared_audio(),
                    model_id.clone(),
                ))
            });
        assert!(tampered_transcribe.is_err());
        assert_eq!(tampered_transcribe_stats.calls, 1);
        assert_eq!(tampered_transcribe_stats.verified_bytes, 0);
        assert_eq!(tampered_transcribe_stats.durations.len(), 1);
        assert_eq!(worker_dispatches.load(Ordering::SeqCst), 3);

        drop(service);
        fs::remove_dir_all(storage).unwrap();
    }

    #[test]
    fn staged_smoke_preserves_decode_failure_and_unloads_across_worker_boundary() {
        let (root, spec) = service_onnx_spec("decode-cleanup");
        let unloads = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let worker_unloads = Arc::clone(&unloads);
        let worker = simulated_runtime_worker(move |receiver| {
            while let Ok(command) = receiver.recv() {
                match command {
                    RuntimeCommand::Health { reply, .. } => reply.send(Ok(())).unwrap(),
                    RuntimeCommand::Load { reply, .. } => {
                        reply.send(Ok(test_load_execution())).unwrap()
                    }
                    RuntimeCommand::Transcribe { reply, .. } => reply
                        .send(Err(RuntimeError::Engine(
                            "deterministic service decode failure".to_owned(),
                        )))
                        .unwrap(),
                    RuntimeCommand::Unload { reply } => {
                        worker_unloads.fetch_add(1, Ordering::AcqRel);
                        reply.send(Ok(())).unwrap();
                    }
                    RuntimeCommand::Shutdown { reply } => {
                        reply.send(Ok(())).unwrap();
                        break;
                    }
                }
            }
        });
        let service = TranscriptionService {
            config: AppConfig::default(),
            router: RuntimeRouter::new(),
            current_receipt_manifest: None,
            worker,
        };

        let error = service
            .verify_onnx_artifact_smoke(
                RuntimeArtifact::OnnxBundle(spec),
                &InstallCancellation::default(),
            )
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("deterministic service decode failure")
        );
        assert_eq!(unloads.load(Ordering::Acquire), 2);
        drop(service);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn staged_smoke_health_failure_still_unloads_across_worker_boundary() {
        let (root, spec) = service_onnx_spec("health-cleanup");
        let unloads = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let worker_unloads = Arc::clone(&unloads);
        let worker = simulated_runtime_worker(move |receiver| {
            while let Ok(command) = receiver.recv() {
                match command {
                    RuntimeCommand::Health { reply, .. } => reply
                        .send(Err(RuntimeError::Engine(
                            "deterministic service health failure".to_owned(),
                        )))
                        .unwrap(),
                    RuntimeCommand::Unload { reply } => {
                        worker_unloads.fetch_add(1, Ordering::AcqRel);
                        reply.send(Ok(())).unwrap();
                    }
                    RuntimeCommand::Shutdown { reply } => {
                        reply.send(Ok(())).unwrap();
                        break;
                    }
                    _ => panic!("health failure must stop staged smoke before load/decode"),
                }
            }
        });
        let service = TranscriptionService {
            config: AppConfig::default(),
            router: RuntimeRouter::new(),
            current_receipt_manifest: None,
            worker,
        };

        let error = service
            .verify_onnx_artifact_smoke(
                RuntimeArtifact::OnnxBundle(spec),
                &InstallCancellation::default(),
            )
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("deterministic service health failure")
        );
        assert_eq!(unloads.load(Ordering::Acquire), 1);
        drop(service);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn native_shutdown_deadline_is_bounded_while_a_command_is_stuck() {
        let (started_sender, started_receiver) = sync_channel(1);
        let release = Arc::new((std::sync::Condvar::new(), Mutex::new(false)));
        let worker_release = Arc::clone(&release);
        let worker = simulated_runtime_worker(move |receiver| {
            if let RuntimeCommand::Unload { reply } = receiver.recv().unwrap() {
                started_sender.send(()).unwrap();
                let (wake, lock) = &*worker_release;
                let mut released = lock.lock().unwrap();
                while !*released {
                    released = wake.wait(released).unwrap();
                }
                let _ = reply.send(Ok(()));
            }
            if let Ok(RuntimeCommand::Shutdown { reply }) = receiver.recv() {
                let _ = reply.send(Ok(()));
            }
        });
        let (reply, _response) = sync_channel(1);
        worker
            .inner
            .commands
            .send(RuntimeCommand::Unload { reply })
            .unwrap();
        started_receiver
            .recv_timeout(Duration::from_secs(1))
            .unwrap();

        let started = Instant::now();
        assert!(!worker.shutdown_and_join(Duration::from_millis(20)));
        assert!(started.elapsed() < Duration::from_millis(500));

        let (wake, lock) = &*release;
        *lock.lock().unwrap() = true;
        wake.notify_one();
        assert!(worker.shutdown_and_join(Duration::from_secs(1)));
    }

    #[test]
    fn disconnected_panicked_native_worker_is_joined_without_hanging() {
        let worker = simulated_runtime_worker(|_receiver| panic!("injected worker panic"));
        let started = Instant::now();

        assert!(worker.shutdown_and_join(Duration::from_secs(1)));
        assert!(started.elapsed() < Duration::from_millis(500));
    }

    #[test]
    fn concurrent_last_clone_drop_stress_leaves_no_native_worker_owner() {
        for _ in 0..20 {
            let worker = RuntimeWorker::new(RuntimeRouter::new());
            let weak = Arc::downgrade(&worker.inner);
            let barrier = Arc::new(std::sync::Barrier::new(9));
            let droppers = (0..8)
                .map(|_| {
                    let retained = worker.clone();
                    let barrier = Arc::clone(&barrier);
                    std::thread::spawn(move || {
                        barrier.wait();
                        drop(retained);
                    })
                })
                .collect::<Vec<_>>();
            drop(worker);
            barrier.wait();
            for dropper in droppers {
                dropper.join().unwrap();
            }
            assert!(weak.upgrade().is_none());
        }
    }

    #[cfg(unix)]

    fn percentile(values: &[u128], percentile: usize) -> u128 {
        let mut sorted = values.to_vec();
        sorted.sort_unstable();
        let rank = (percentile * sorted.len()).div_ceil(100).saturating_sub(1);
        sorted[rank]
    }
}
