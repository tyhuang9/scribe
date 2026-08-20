//! Runtime-neutral transcription contracts and the Phase 1 legacy bridge.
//!
//! Application code should depend on [`TranscriptionService`] and the types in
//! this module rather than on a concrete STT backend. The current adapters are
//! deliberately kept behind one private batch bridge until a later phase
//! replaces them with the consolidated runtime implementation.

// Phase 1 establishes the complete stable contract before native streaming,
// lifecycle wiring, and capability UI are introduced in later phases.
#![allow(dead_code)]

use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, sync_channel};
use std::sync::{Arc, Mutex, TryLockError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};

use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};

use crate::config::{self, AppConfig};
use crate::installations::{
    InstallCancellation, previous_runtime_root, rollback_to_previous_runtime, verify_runtime_tree,
};
use crate::model_catalog::{
    ArtifactFormat, model_descriptor, normal_model_descriptors, runtime_artifact_manifest_for_path,
    runtime_model_manifest,
};
#[allow(unused_imports)]
pub use crate::model_catalog::{
    CompatibilityStatus, ModelCapabilities, ModelDescriptor, ModelRole,
};
use crate::models::{SttModelInfo, TranscriptResult as LegacyTranscriptResult};
use crate::onnx_worker::OnnxModelSpec;
use crate::prepared_audio::{PREPARED_SAMPLE_RATE, PreparedAudio};
use crate::runtime_router::{
    IdleTimeoutAction, NativeBootstrapFailure, RuntimeArtifact, RuntimeError, RuntimeExecution,
    RuntimeLoadExecution, RuntimeModel, RuntimeRouter, WARM_MODEL_TTL, verify_compatibility_cli,
};
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
}

/// A runtime-neutral reference to a configured model catalog entry.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
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
#[derive(Clone, Debug, PartialEq)]
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
#[derive(Clone, Debug, PartialEq)]
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
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TranscriptionOptions {
    pub language: Option<String>,
    pub translate_to_english: bool,
    pub enable_timestamps: bool,
    pub initial_prompt: Option<String>,
}

/// Internal rolling-preview policy. Native runtimes that expose segment
/// timing already return it with their ordinary decode result, so preview can
/// opt into using that metadata without changing the caller-facing final
/// transcription options or sending an unsupported decoder option.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct PreviewDecodeOptions {
    use_segment_timestamps: bool,
}

impl PreviewDecodeOptions {
    fn for_capabilities(capabilities: &RuntimeCapabilities) -> Self {
        Self {
            use_segment_timestamps: capabilities.timestamps,
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

/// A fully staged model/runtime pair that has not yet been activated.
/// Concrete runtime selection remains private to the router used by the
/// service's dedicated verification worker.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct InstallationCandidate {
    pub(crate) model_id: ModelId,
    pub(crate) model_path: PathBuf,
    pub(crate) artifact_format: ArtifactFormat,
    pub(crate) runtime_package_root: Option<PathBuf>,
    pub(crate) expected_size_bytes: u64,
    pub(crate) expected_sha256: String,
}

impl InstallationCandidate {
    pub(crate) fn normalized(
        model_id: ModelId,
        model_path: PathBuf,
        runtime_package_root: Option<PathBuf>,
    ) -> Result<Self> {
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
            runtime_package_root,
            expected_size_bytes: manifest.size_bytes,
            expected_sha256: manifest.sha256.to_owned(),
        })
    }

    pub(crate) fn pinned(
        model_id: ModelId,
        model_path: PathBuf,
        artifact_format: ArtifactFormat,
        runtime_package_root: Option<PathBuf>,
        expected_size_bytes: u64,
        expected_sha256: String,
    ) -> Self {
        Self {
            model_id,
            model_path,
            artifact_format,
            runtime_package_root,
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct InstallationBinding {
    pub(crate) managed_runtime_id: String,
    pub(crate) installed_package_root: Option<PathBuf>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RuntimeRecovery {
    pub(crate) managed_runtime_id: String,
    pub(crate) entrypoint: PathBuf,
    pub(crate) version: String,
    pub(crate) archive_sha256: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum VerifiedInstallationCapability {
    Available { package_version: String },
    Unavailable { reason: String },
}

/// Reports verified installation support for a normalized model. Concrete
/// package and catalog selection stay below the application-facing service
/// boundary, while callers can distinguish a legacy model from a normalized
/// model whose package is unavailable on this platform.
pub(crate) fn verified_installation_capability(
    model_id: &ModelId,
) -> Option<VerifiedInstallationCapability> {
    runtime_model_manifest(model_id)?;
    let archive = config::runtime_storage_dir()
        .join(".downloads")
        .join("whisper-cpp-v1.9.1-windows-x64-cpu.zip");
    Some(
        match crate::runtime_catalog::primary_runtime_install_spec(archive) {
            Ok(package) => VerifiedInstallationCapability::Available {
                package_version: package.version,
            },
            Err(reason) => VerifiedInstallationCapability::Unavailable { reason },
        },
    )
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

    fn transcribe(
        &mut self,
        audio: &PreparedAudio,
        options: &TranscriptionOptions,
    ) -> Result<Transcript>;

    fn capabilities(&self) -> RuntimeCapabilities;

    fn health_check(&mut self) -> Result<()>;

    fn cancel(&mut self) -> Result<()>;

    fn unload(&mut self) -> Result<()>;
}

/// Optional extension for engines that can decode incrementally.
pub trait StreamingSpeechEngine: SpeechEngine {
    fn start_stream(&mut self, options: &TranscriptionOptions) -> Result<Box<dyn SpeechStream>>;
}

/// A live speech-decoding session.
pub trait SpeechStream: Send {
    fn push_audio(&mut self, samples: &[f32]) -> Result<StreamUpdate>;

    fn finalize(self: Box<Self>) -> Result<Transcript>;

    fn cancel(self: Box<Self>) -> Result<()>;
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
    StartStream {
        artifact: RuntimeArtifact,
        preference: AccelerationPreference,
        options: TranscriptionOptions,
        reply: SyncSender<Result<Box<dyn SpeechStream>, RuntimeError>>,
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
    cancellation: RuntimeRouter,
}

impl RuntimeWorker {
    fn new(router: RuntimeRouter) -> Self {
        cleanup_stale_temporary_audio();
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
                cancellation: router,
            }),
        }
    }

    fn transcribe(
        &self,
        artifact: impl Into<RuntimeArtifact>,
        preference: AccelerationPreference,
        audio: Arc<PreparedAudio>,
        options: TranscriptionOptions,
        cancellation_snapshot: u64,
    ) -> Result<RuntimeExecution, RuntimeError> {
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

    fn start_stream(
        &self,
        artifact: impl Into<RuntimeArtifact>,
        preference: AccelerationPreference,
        options: TranscriptionOptions,
    ) -> Result<Box<dyn SpeechStream>, RuntimeError> {
        let (reply, response) = sync_channel(1);
        self.inner
            .commands
            .send(RuntimeCommand::StartStream {
                artifact: artifact.into(),
                preference,
                options,
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
        self.cancellation.cancel_active();
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
            Ok(RuntimeCommand::StartStream {
                artifact,
                preference,
                options,
                reply,
            }) => {
                let request_activity = activity.acquire_request().ok();
                let result = router.start_stream(artifact, preference, &options);
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

/// Application-facing boundary for all transcription work.
#[derive(Clone, Debug)]
pub struct TranscriptionService {
    config: AppConfig,
    router: RuntimeRouter,
    worker: RuntimeWorker,
}

impl TranscriptionService {
    pub fn new(config: AppConfig) -> Self {
        let router = RuntimeRouter::new();
        Self {
            config,
            worker: RuntimeWorker::new(router.clone()),
            router,
        }
    }

    /// Returns a service snapshot with current settings while retaining the
    /// same serialized runtime worker and its warm model.
    pub fn with_config(&self, config: AppConfig) -> Self {
        Self {
            config,
            router: self.router.clone(),
            worker: self.worker.clone(),
        }
    }

    #[cfg(test)]
    pub(crate) fn with_runtime_router(config: AppConfig, router: RuntimeRouter) -> Self {
        Self {
            config,
            worker: RuntimeWorker::new(router.clone()),
            router,
        }
    }

    pub(crate) fn preload_runtime_artifact(
        &self,
        artifact: RuntimeArtifact,
    ) -> Result<RuntimeLoadExecution> {
        self.worker
            .load(artifact, self.config.performance.acceleration_preference)
            .map_err(Into::into)
    }

    pub(crate) fn transcribe_runtime_artifact(
        &self,
        artifact: RuntimeArtifact,
        audio: Arc<PreparedAudio>,
        options: TranscriptionOptions,
    ) -> Result<RuntimeExecution> {
        self.worker
            .transcribe(
                artifact,
                self.config.performance.acceleration_preference,
                audio,
                options,
                self.router.cancellation_snapshot(),
            )
            .map_err(Into::into)
    }

    pub(crate) fn health_check_runtime_artifact(&self, artifact: RuntimeArtifact) -> Result<()> {
        self.worker
            .health_check(artifact, self.config.performance.acceleration_preference)
            .map_err(Into::into)
    }

    pub(crate) fn start_runtime_stream(
        &self,
        artifact: RuntimeArtifact,
        options: TranscriptionOptions,
    ) -> Result<Box<dyn SpeechStream>> {
        self.worker
            .start_stream(
                artifact,
                self.config.performance.acceleration_preference,
                options,
            )
            .map_err(Into::into)
    }

    pub(crate) fn unload_runtime_artifacts(&self) -> Result<()> {
        self.worker.unload().map_err(Into::into)
    }

    /// Resolves a private installed ONNX bundle from its durable exact receipt
    /// only when that receipt still equals a currently embedded available
    /// manifest. This path is local-only and never grants a self-authored or
    /// retired receipt executable trust.
    fn onnx_artifact_from_receipt(&self, root: &Path) -> Result<RuntimeArtifact> {
        let (_, spec) = crate::onnx_model_bundles::current_executable_receipt_at(root)
            .map_err(|error| anyhow!("installed ONNX bundle verification failed: {error}"))?;
        Ok(RuntimeArtifact::OnnxBundle(spec))
    }

    pub(crate) fn preload_onnx_bundle_from_receipt(
        &self,
        root: &Path,
    ) -> Result<RuntimeLoadExecution> {
        self.preload_runtime_artifact(self.onnx_artifact_from_receipt(root)?)
    }

    pub(crate) fn transcribe_onnx_bundle_from_receipt(
        &self,
        root: &Path,
        audio: Arc<PreparedAudio>,
        options: TranscriptionOptions,
    ) -> Result<RuntimeExecution> {
        self.transcribe_runtime_artifact(self.onnx_artifact_from_receipt(root)?, audio, options)
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
                    self.router.cancellation_snapshot(),
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

    pub(crate) fn installation_binding(&self, model_id: &ModelId) -> Result<InstallationBinding> {
        if model_uses_embedded_gguf(model_id)
            || config::remote_gguf_artifact(&self.config, model_id.as_str()).is_some()
            || config::imported_gguf_artifact(&self.config, model_id.as_str()).is_some()
        {
            return Ok(InstallationBinding {
                // This stable internal token lets the existing operation
                // machinery correlate the request without representing a
                // downloadable or executable runtime.
                managed_runtime_id: "embedded-transcribe-cpp".to_owned(),
                installed_package_root: None,
            });
        }
        let runtime_id = self
            .router
            .managed_runtime_id(model_id)
            .ok_or_else(|| anyhow!("model {model_id} has no installable native runtime"))?;
        let installed_package_root = configured_managed_runtime_root(&self.config, runtime_id)?
            .or_else(|| primary_runtime_package_root(&self.config));
        Ok(InstallationBinding {
            managed_runtime_id: runtime_id.to_owned(),
            installed_package_root,
        })
    }

    /// Returns the deterministic catalog target without trusting persisted
    /// runtime paths. Startup uses it to find recovery state even when a
    /// managed-runtime settings record is absent or malformed.
    pub(crate) fn recovery_installation_binding(
        &self,
        model_id: &ModelId,
    ) -> Result<InstallationBinding> {
        if model_uses_embedded_gguf(model_id) {
            return Err(anyhow!(
                "embedded GGUF models do not have a recoverable runtime package"
            ));
        }
        let runtime_id = self
            .router
            .managed_runtime_id(model_id)
            .ok_or_else(|| anyhow!("model {model_id} has no installable native runtime"))?;
        Ok(InstallationBinding {
            managed_runtime_id: runtime_id.to_owned(),
            installed_package_root: Some(config::runtime_storage_dir().join(runtime_id)),
        })
    }

    pub(crate) fn rollback_to_previous_runtime(
        &self,
        model_id: &ModelId,
    ) -> Result<Option<RuntimeRecovery>> {
        let runtime_id = self
            .router
            .managed_runtime_id(model_id)
            .ok_or_else(|| anyhow!("model {model_id} has no managed native runtime"))?;
        let target = config::runtime_storage_dir().join(runtime_id);
        let previous = previous_runtime_root(&target);
        if !previous.exists() {
            return Ok(None);
        }
        let archive = config::runtime_storage_dir()
            .join(".downloads")
            .join("whisper-cpp-v1.9.1-windows-x64-cpu.zip");
        let spec = crate::runtime_catalog::primary_runtime_install_spec(archive)
            .map_err(|error| anyhow!(error))?;
        verify_runtime_tree(&previous, &spec.archive.files)
            .map_err(|error| anyhow!("previous runtime failed manifest verification: {error}"))?;
        if !rollback_to_previous_runtime(&target).map_err(|error| anyhow!(error))? {
            return Ok(None);
        }
        Ok(Some(RuntimeRecovery {
            managed_runtime_id: runtime_id.to_owned(),
            entrypoint: target.join(spec.compatibility_entrypoint),
            version: spec.version,
            archive_sha256: spec.archive.artifact.sha256,
        }))
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
            return Ok(
                persisted_capabilities.unwrap_or_else(|| self.router.embedded_capabilities())
            );
        }
        if self.router.handles_model(model_id) {
            let runtime_capabilities = persisted_capabilities
                .or_else(|| self.router.capabilities(model_id))
                .ok_or_else(|| anyhow!("runtime router rejected its own selected model"))?;
            let descriptor = model_descriptor(model_id)
                .ok_or_else(|| anyhow!("unknown normalized transcription model: {model_id}"))?;
            return Ok(intersect_capabilities(&runtime_capabilities, &descriptor));
        }
        Ok(capabilities_for_legacy_model(&model))
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
        if let Some(package_root) = runtime_model.package_root.as_deref() {
            verify_primary_runtime_package_tree(package_root)?;
        }
        verify_runtime_model_artifact(&runtime_model)?;
        let mut smoke_config = self.config.clone();
        smoke_config.performance.acceleration_preference = AccelerationPreference::Cpu;
        self.with_config(smoke_config)
            .verify_installation_candidate(
                InstallationCandidate::pinned(
                    runtime_model.id,
                    runtime_model.path,
                    runtime_model.format,
                    runtime_model.package_root,
                    runtime_model.expected_size_bytes,
                    runtime_model.expected_sha256,
                ),
                &InstallCancellation::default(),
            )
            .map(|_| ())
    }

    /// Verifies only the immutable package shipped beside the application.
    /// Developer and managed runtime paths are deliberately excluded so a
    /// recovery message can never claim that an arbitrary CLI was bundled.
    pub(crate) fn startup_bundled_runtime_health_and_load(
        &self,
        model_id: &ModelId,
        model_path: Option<PathBuf>,
    ) -> Result<()> {
        let package_root = crate::compatibility_bridge::primary_bundled_runtime_package_root()
            .ok_or_else(|| anyhow!("the application executable has no package directory"))?;
        let model = self.resolve_model(model_id, model_path)?;
        let path = model
            .local_path
            .ok_or_else(|| anyhow!("download {} before verification", model.name))?;
        let manifest = runtime_artifact_manifest_for_path(model_id, &path).ok_or_else(|| {
            anyhow!(
                "model {} has no pinned size and SHA-256 evidence for {}",
                model.name,
                path.display()
            )
        })?;
        let runtime_model = RuntimeModel {
            id: model_id.clone(),
            path,
            format: manifest.format,
            package_root: Some(package_root),
            expected_size_bytes: manifest.size_bytes,
            expected_sha256: manifest.sha256.to_owned(),
        };
        if let Some(package_root) = runtime_model.package_root.as_deref() {
            verify_primary_runtime_package_tree(package_root)?;
        }
        verify_runtime_model_artifact(&runtime_model)?;
        let mut smoke_config = self.config.clone();
        smoke_config.performance.acceleration_preference = AccelerationPreference::Cpu;
        self.with_config(smoke_config)
            .verify_installation_candidate(
                InstallationCandidate::pinned(
                    runtime_model.id,
                    runtime_model.path,
                    runtime_model.format,
                    runtime_model.package_root,
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
        self.router.cancel_active();
        crate::stt::cancel_active_processes();
    }

    /// Cancels active work and waits for service requests and compatibility
    /// processes to release their transient audio resources.
    pub fn cancel_active_and_wait(&self, timeout: Duration) -> bool {
        self.router.cancel_active();
        crate::stt::cancel_active_processes_and_wait(timeout)
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
            native_generation: self.router.cancellation_snapshot(),
            process_generation: crate::stt::cancellation_snapshot(),
        }
    }

    /// Registers work synchronously before the caller starts an audio worker.
    pub fn begin_transcription_task(&self) -> Result<TranscriptionTask> {
        let ticket = self.transcription_ticket();
        let registration = crate::stt::register_cancellable_request(ticket.process_generation)
            .map_err(|error| anyhow!(error))?;
        if self.router.cancellation_snapshot() != ticket.native_generation {
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
            .arg(match candidate.artifact_format {
                ArtifactFormat::Gguf => "gguf",
                ArtifactFormat::LegacyGgml => "legacy-ggml",
            })
            .arg(
                candidate
                    .runtime_package_root
                    .as_deref()
                    .unwrap_or_else(|| Path::new("-")),
            )
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
            package_root: candidate.runtime_package_root,
            expected_size_bytes: candidate.expected_size_bytes,
            expected_sha256: candidate.expected_sha256,
        };
        if let Some(package_root) = runtime_model.package_root.as_deref() {
            verify_primary_runtime_package_tree(package_root)?;
        }
        let router = RuntimeRouter::new();
        let worker = RuntimeWorker::new(router.clone());
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
                audio,
                TranscriptionOptions::default(),
                router.cancellation_snapshot(),
            )
            .map_err(|error| anyhow!("staged transcription smoke failed: {error}"))?;
        let decode_duration_ms = decode_started.elapsed().as_millis();
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
        if !self.router.handles_model(&model_id)
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
                request.options = TranscriptionOptions::default();
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
        if self.router.cancellation_snapshot() != ticket.native_generation {
            return Err(anyhow!(
                "rolling preview was cancelled before native dispatch"
            ));
        }
        if !self.router.handles_model(&request.model_id)
            && config::remote_gguf_artifact(&self.config, request.model_id.as_str()).is_none()
            && config::imported_gguf_artifact(&self.config, request.model_id.as_str()).is_none()
        {
            return Err(anyhow!(
                "rolling preview is unavailable for this model's verified native runtime"
            ));
        }
        validate_default_options(&request.options)?;
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
        if self.router.cancellation_snapshot() != ticket.native_generation {
            return Err(anyhow!(
                "transcription request was cancelled before dispatch"
            ));
        }
        let model = self.resolve_model(&request.model_id, request.model_path.clone())?;
        if self.router.handles_model(&request.model_id)
            || config::remote_gguf_artifact(&self.config, request.model_id.as_str()).is_some()
            || config::imported_gguf_artifact(&self.config, request.model_id.as_str()).is_some()
        {
            return self.transcribe_primary(request, model, ticket);
        }

        self.transcribe_legacy(request, model, ticket)
    }

    fn transcribe_primary(
        &self,
        request: TranscriptionRequest,
        model: SttModelInfo,
        ticket: TranscriptionTicket,
    ) -> Result<TranscriptionOutcome> {
        validate_default_options(&request.options)?;
        let runtime_model = self.resolve_runtime_model(model.clone())?;
        match self.worker.transcribe(
            runtime_model,
            self.config.performance.acceleration_preference,
            Arc::clone(&request.audio),
            request.options.clone(),
            ticket.native_generation,
        ) {
            Ok(execution) => Ok(map_native_execution(request, model, execution)),
            Err(crate::runtime_router::RuntimeError::Bootstrap(failure))
                if failure.cli_fallback_eligible() =>
            {
                self.transcribe_legacy_with_fallback_reason(request, model, failure, ticket)
            }
            Err(error) => Err(anyhow!(error)),
        }
    }

    fn transcribe_legacy(
        &self,
        request: TranscriptionRequest,
        model: SttModelInfo,
        ticket: TranscriptionTicket,
    ) -> Result<TranscriptionOutcome> {
        self.transcribe_legacy_inner(request, model, None, ticket)
    }

    fn transcribe_legacy_with_fallback_reason(
        &self,
        request: TranscriptionRequest,
        model: SttModelInfo,
        failure: NativeBootstrapFailure,
        ticket: TranscriptionTicket,
    ) -> Result<TranscriptionOutcome> {
        self.transcribe_legacy_inner(request, model, Some(failure.to_string()), ticket)
    }

    fn transcribe_legacy_inner(
        &self,
        request: TranscriptionRequest,
        model: SttModelInfo,
        fallback_reason: Option<String>,
        ticket: TranscriptionTicket,
    ) -> Result<TranscriptionOutcome> {
        if fallback_reason.is_some() {
            let cli = crate::compatibility_bridge::primary_runtime_entrypoint(&self.config)
                .ok_or_else(|| anyhow!("the verified compatibility CLI is unavailable"))?;
            verify_compatibility_cli(&cli).map_err(|error| anyhow!(error))?;
        }
        let mut engine =
            LegacyBatchAdapter::new(self.config.clone(), model, ticket.process_generation);
        engine.load()?;
        let transcription = engine.transcribe(&request.audio, &request.options);
        let unload_result = engine.unload();
        let transcript = transcription?;
        unload_result?;
        let diagnostics = engine.take_diagnostics().ok_or_else(|| {
            anyhow!("legacy transcription completed without diagnostics; this is a service bug")
        })?;
        validate_response_model_id(&request.model_id, &diagnostics)?;

        let mut stderr = diagnostics.stderr;
        if let Some(reason) = fallback_reason {
            if !stderr.is_empty() {
                stderr.push('\n');
            }
            stderr.push_str("Native bootstrap fallback: ");
            stderr.push_str(&reason);
        }

        Ok(TranscriptionOutcome {
            session_id: request.session_id,
            request_id: request.request_id,
            model_id: diagnostics.model_id,
            model_name: diagnostics.model_name,
            backend_label: diagnostics.backend_label,
            transcript,
            processing_duration_ms: diagnostics.processing_duration_ms,
            resolved_acceleration: None,
            model_load_duration_ms: None,
            warm_model_reused: false,
            stdout: diagnostics.stdout,
            stderr,
        })
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
        let package_root = if artifact_format == ArtifactFormat::Gguf {
            None
        } else {
            Some(
                match self.router.managed_runtime_id(&model_id) {
                    Some(runtime_id) => {
                        configured_managed_runtime_root(&self.config, runtime_id)?
                    }
                    None => None,
                }
                .or_else(|| primary_runtime_package_root(&self.config))
                .ok_or_else(|| {
                    anyhow!(
                        "the verified native runtime package is not installed; install it from Models or configure the compatibility CLI"
                    )
                })?,
            )
        };
        Ok(RuntimeModel {
            id: model.id.into(),
            path,
            format: artifact_format,
            package_root,
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
            "legacy-ggml" => ArtifactFormat::LegacyGgml,
            _ => return Err(anyhow!("invalid artifact format")),
        };
        let runtime_package_root = args
            .next()
            .ok_or_else(|| anyhow!("missing runtime package root"))?;
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
            (runtime_package_root != "-").then(|| PathBuf::from(runtime_package_root)),
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

fn verify_primary_runtime_package_tree(package_root: &Path) -> Result<()> {
    let archive = config::runtime_storage_dir()
        .join(".downloads")
        .join("whisper-cpp-v1.9.1-windows-x64-cpu.zip");
    let spec = crate::runtime_catalog::primary_runtime_install_spec(archive)
        .map_err(|error| anyhow!("could not resolve the pinned runtime manifest: {error}"))?;
    verify_runtime_tree(package_root, &spec.archive.files).map_err(|error| {
        anyhow!("runtime package tree failed exact manifest verification: {error}")
    })
}

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

fn primary_runtime_package_root(config: &AppConfig) -> Option<PathBuf> {
    let entrypoint = crate::compatibility_bridge::primary_runtime_entrypoint(config)?;
    let bin_dir = entrypoint.parent()?;
    if bin_dir
        .file_name()
        .is_some_and(|name| name.eq_ignore_ascii_case("bin"))
    {
        bin_dir.parent().map(Path::to_path_buf)
    } else {
        Some(bin_dir.to_path_buf())
    }
}

fn configured_managed_runtime_root(
    config: &AppConfig,
    runtime_id: &str,
) -> Result<Option<PathBuf>> {
    configured_managed_runtime_root_in(config, runtime_id, &config::runtime_storage_dir())
}

fn configured_managed_runtime_root_in(
    config: &AppConfig,
    runtime_id: &str,
    storage_dir: &Path,
) -> Result<Option<PathBuf>> {
    let Some(install) = config.general.managed_runtimes.get(runtime_id) else {
        return Ok(None);
    };
    if install
        .path
        .components()
        .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(anyhow!(
            "managed runtime {runtime_id} contains a parent-directory path component"
        ));
    }
    let expected = storage_dir.join(runtime_id);
    if !install.path.starts_with(storage_dir) {
        return Err(anyhow!(
            "managed runtime {runtime_id} points outside its catalog target: configured {}, expected {}",
            install.path.display(),
            expected.display()
        ));
    }
    if runtime_path_has_link_or_reparse_below(storage_dir, &expected)
        || runtime_path_has_link_or_reparse_below(storage_dir, &install.path)
    {
        return Err(anyhow!(
            "managed runtime {runtime_id} crosses a symbolic link or Windows reparse point"
        ));
    }
    let archive = storage_dir
        .join(".downloads")
        .join("whisper-cpp-v1.9.1-windows-x64-cpu.zip");
    let spec = crate::runtime_catalog::primary_runtime_install_spec(archive)
        .map_err(|error| anyhow!("could not resolve the pinned runtime entrypoint: {error}"))?;
    let expected_entrypoint = expected.join(spec.compatibility_entrypoint);
    let configured_entrypoint = install.path.canonicalize().map_err(|error| {
        anyhow!(
            "managed runtime {runtime_id} entrypoint {} is unavailable: {error}",
            install.path.display()
        )
    })?;
    let expected_entrypoint_canonical = expected_entrypoint.canonicalize().map_err(|error| {
        anyhow!(
            "managed runtime {runtime_id} pinned entrypoint {} is unavailable: {error}",
            expected_entrypoint.display()
        )
    })?;
    if configured_entrypoint != expected_entrypoint_canonical {
        return Err(anyhow!(
            "managed runtime {runtime_id} does not name its exact pinned entrypoint: configured {}, expected {}",
            install.path.display(),
            expected_entrypoint.display()
        ));
    }
    let configured = package_root_from_entrypoint(&install.path).ok_or_else(|| {
        anyhow!(
            "managed runtime {runtime_id} has no package root for {}",
            install.path.display()
        )
    })?;
    let expected_canonical = expected.canonicalize().map_err(|error| {
        anyhow!(
            "managed runtime {runtime_id} target {} is unavailable: {error}",
            expected.display()
        )
    })?;
    let configured_canonical = configured.canonicalize().map_err(|error| {
        anyhow!(
            "managed runtime {runtime_id} configured root {} is unavailable: {error}",
            configured.display()
        )
    })?;
    if configured_canonical != expected_canonical {
        return Err(anyhow!(
            "managed runtime {runtime_id} points outside its catalog target: configured {}, expected {}",
            configured.display(),
            expected.display()
        ));
    }
    Ok(Some(expected))
}

fn runtime_path_has_link_or_reparse_below(root: &Path, path: &Path) -> bool {
    let Ok(relative) = path.strip_prefix(root) else {
        return true;
    };
    let mut current = root.to_path_buf();
    for component in relative.components() {
        if !matches!(component, std::path::Component::Normal(_)) {
            return true;
        }
        current.push(component.as_os_str());
        let Ok(metadata) = fs::symlink_metadata(&current) else {
            return true;
        };
        if runtime_metadata_is_link_or_reparse(&metadata) {
            return true;
        }
    }
    false
}

#[cfg(windows)]
fn runtime_metadata_is_link_or_reparse(metadata: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    metadata.file_type().is_symlink()
        || metadata.file_attributes()
            & windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT
            != 0
}

#[cfg(not(windows))]
fn runtime_metadata_is_link_or_reparse(metadata: &fs::Metadata) -> bool {
    metadata.file_type().is_symlink()
}

fn package_root_from_entrypoint(entrypoint: &Path) -> Option<PathBuf> {
    let parent = entrypoint.parent()?;
    if parent
        .file_name()
        .is_some_and(|name| name.eq_ignore_ascii_case("bin"))
    {
        parent.parent().map(Path::to_path_buf)
    } else {
        Some(parent.to_path_buf())
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
            "native_library={} warm_reused={}",
            execution.diagnostics.native_library_path.display(),
            execution.diagnostics.warm_reused
        ),
    }
}

/// The sole Phase 1 adapter for the pre-existing command-line backend path.
///
/// It intentionally delegates to `stt::transcribe_with_config` unchanged so
/// all existing configured model paths and runtime resolution behavior remain
/// intact during extraction.
struct LegacyBatchAdapter {
    config: AppConfig,
    model: SttModelInfo,
    cancellation_snapshot: crate::stt::CancellationSnapshot,
    diagnostics: Option<LegacyDiagnostics>,
}

impl LegacyBatchAdapter {
    fn new(
        config: AppConfig,
        model: SttModelInfo,
        cancellation_snapshot: crate::stt::CancellationSnapshot,
    ) -> Self {
        Self {
            config,
            model,
            cancellation_snapshot,
            diagnostics: None,
        }
    }

    fn take_diagnostics(&mut self) -> Option<LegacyDiagnostics> {
        self.diagnostics.take()
    }
}

impl SpeechEngine for LegacyBatchAdapter {
    fn load(&mut self) -> Result<()> {
        // The legacy route starts a fresh child process for each request, so
        // there is no persistent engine to preload or validate here.
        Ok(())
    }

    fn transcribe(
        &mut self,
        audio: &PreparedAudio,
        options: &TranscriptionOptions,
    ) -> Result<Transcript> {
        validate_default_options(options)?;
        let prepared_wav = TemporaryPreparedWav::create(audio)?;

        let result = crate::stt::transcribe_with_config(
            &self.config,
            prepared_wav.path().to_path_buf(),
            self.model.clone(),
            self.cancellation_snapshot,
        )?;
        let (transcript, diagnostics) = map_legacy_result(result);
        self.diagnostics = Some(diagnostics);
        Ok(transcript)
    }

    fn capabilities(&self) -> RuntimeCapabilities {
        capabilities_for_legacy_model(&self.model)
    }

    fn health_check(&mut self) -> Result<()> {
        Err(anyhow!(
            "legacy command-line health checks are not implemented in Phase 1"
        ))
    }

    fn cancel(&mut self) -> Result<()> {
        crate::stt::cancel_active_processes();
        Ok(())
    }

    fn unload(&mut self) -> Result<()> {
        // Each legacy invocation is a child process, so there is no loaded
        // in-process engine state to release.
        Ok(())
    }
}

static TEMP_AUDIO_SEQUENCE: AtomicU64 = AtomicU64::new(1);

/// Compatibility artifact for the transitional process adapters. Paths never
/// cross the application-facing service contract, and the file is removed by
/// RAII on every success and error path.
struct TemporaryPreparedWav {
    path: PathBuf,
}

impl TemporaryPreparedWav {
    fn create(audio: &PreparedAudio) -> Result<Self> {
        if audio.sample_rate != 16_000
            || audio.samples.is_empty()
            || audio
                .samples
                .iter()
                .any(|sample| !sample.is_finite() || !(-1.0..=1.0).contains(sample))
        {
            return Err(anyhow!(
                "legacy compatibility bridge received invalid prepared audio"
            ));
        }

        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let sequence = TEMP_AUDIO_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let directory = private_temporary_audio_dir()?;
        let path = directory.join(format!(
            "scribe-prepared-{}-{nonce}-{sequence}.wav",
            std::process::id()
        ));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(0o600);
        let file = options
            .open(&path)
            .map_err(|err| anyhow!("failed to create private prepared-audio WAV: {err}"))?;
        let temporary = Self { path };
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 16_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::new(BufWriter::new(file), spec)
            .map_err(|err| anyhow!("failed to initialize prepared-audio WAV: {err}"))?;
        for sample in &audio.samples {
            let pcm = (sample * i16::MAX as f32).round() as i16;
            writer
                .write_sample(pcm)
                .map_err(|err| anyhow!("failed to write prepared-audio WAV: {err}"))?;
        }
        writer
            .finalize()
            .map_err(|err| anyhow!("failed to finalize prepared-audio WAV: {err}"))?;

        Ok(temporary)
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

fn private_temporary_audio_dir() -> Result<PathBuf> {
    #[cfg(test)]
    let root = std::env::temp_dir().join("scribe-test-private-data");
    #[cfg(not(test))]
    let runtime_dir = config::runtime_storage_dir();
    #[cfg(not(test))]
    let root = runtime_dir
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(std::env::temp_dir);
    let directory = root.join("transient-audio");
    if !directory.is_dir() {
        let mut builder = fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        builder.mode(0o700);
        builder.create(&directory).map_err(|error| {
            anyhow!(
                "failed to create private prepared-audio directory {}: {error}",
                directory.display()
            )
        })?;
    }
    #[cfg(unix)]
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).map_err(|error| {
        anyhow!(
            "failed to secure prepared-audio directory {}: {error}",
            directory.display()
        )
    })?;
    Ok(directory)
}

fn cleanup_stale_temporary_audio() {
    let Ok(directory) = private_temporary_audio_dir() else {
        return;
    };
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    let now = SystemTime::now();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("scribe-prepared-") && name.ends_with(".wav"))
        {
            continue;
        }
        let is_stale = entry
            .metadata()
            .ok()
            .and_then(|metadata| metadata.modified().ok())
            .and_then(|modified| now.duration_since(modified).ok())
            .is_some_and(|age| age >= std::time::Duration::from_secs(24 * 60 * 60));
        if is_stale {
            let _ = fs::remove_file(path);
        }
    }
}

impl Drop for TemporaryPreparedWav {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

#[derive(Debug)]
struct LegacyDiagnostics {
    model_id: ModelId,
    model_name: String,
    backend_label: String,
    processing_duration_ms: Option<u128>,
    stdout: String,
    stderr: String,
}

fn map_legacy_result(result: LegacyTranscriptResult) -> (Transcript, LegacyDiagnostics) {
    let transcript = Transcript {
        text: result.text,
        segments: result
            .segments
            .into_iter()
            .map(|segment| TranscriptSegment {
                text: segment.text,
                start_ms: segment.start_ms,
                end_ms: segment.end_ms,
                confidence: None,
            })
            .collect(),
        detected_language: None,
        duration_ms: None,
    };
    let diagnostics = LegacyDiagnostics {
        model_id: result.model_id.into(),
        model_name: result.model_name,
        backend_label: result.backend,
        processing_duration_ms: result.duration_ms,
        stdout: result.stdout,
        stderr: result.stderr,
    };

    (transcript, diagnostics)
}

fn validate_response_model_id(
    requested_model_id: &ModelId,
    diagnostics: &LegacyDiagnostics,
) -> Result<()> {
    if diagnostics.model_id != *requested_model_id {
        return Err(anyhow!(
            "legacy transcription returned model {} for request model {}",
            diagnostics.model_id,
            requested_model_id
        ));
    }

    Ok(())
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

fn capabilities_for_legacy_model(model: &SttModelInfo) -> RuntimeCapabilities {
    RuntimeCapabilities {
        // Only the current Vosk and faster-whisper adapters reliably expose
        // timestamp values. whisper.cpp strips its text timing and the sherpa
        // family currently reports null segment bounds.
        timestamps: matches!(model.backend.as_str(), "faster-whisper" | "Vosk"),
        cancellation: true,
        ..RuntimeCapabilities::default()
    }
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
    use crate::models::TranscriptSegment as LegacyTranscriptSegment;
    use crate::onnx_worker::{OnnxFileRole, OnnxModelFamily, OnnxModelSpec};
    use crate::runtime_router::{OnnxSupervisorControl, RuntimeArtifact};
    use sha2::{Digest, Sha256};
    use std::collections::BTreeMap;
    use std::io::Cursor;
    use std::sync::Condvar;

    const MAX_DIAGNOSTIC_ONNX_WAV_BYTES: u64 = 256 * 1024 * 1024;

    #[derive(Default)]
    struct FakeOnnxState {
        loaded: Option<OnnxModelSpec>,
        load_calls: usize,
        maximum_loaded_models: usize,
        events: Vec<String>,
        fail_next_load: bool,
        fail_load_call: Option<usize>,
        fail_transcribe: bool,
        fail_health: bool,
        fail_start_stream: bool,
        transcribe_calls: usize,
        health_calls: usize,
        unload_calls: usize,
        unloaded_active_streams: usize,
        cancel_active_calls: usize,
        termination_calls: usize,
        stream: Option<(u64, usize)>,
        stream_cancels: usize,
        batch_started: bool,
        block_batch: bool,
        batch_cancelled: bool,
        cancel_install_on_transcribe: Option<InstallCancellation>,
        cancel_install_on_health: Option<InstallCancellation>,
        cancel_install_on_load_call: Option<(usize, InstallCancellation)>,
    }

    #[derive(Default)]
    struct FakeOnnxControl {
        state: Mutex<FakeOnnxState>,
        changed: Condvar,
    }

    impl FakeOnnxControl {
        fn set_block_batch(&self) {
            self.state.lock().unwrap().block_batch = true;
        }

        fn wait_for_batch(&self) {
            let mut state = self.state.lock().unwrap();
            while !state.batch_started {
                state = self.changed.wait(state).unwrap();
            }
        }

        fn fail_next_load(&self) {
            self.state.lock().unwrap().fail_next_load = true;
        }

        fn fail_load_call(&self, call: usize) {
            self.state.lock().unwrap().fail_load_call = Some(call);
        }

        fn fail_transcribe(&self) {
            self.state.lock().unwrap().fail_transcribe = true;
        }

        fn cancel_install_on_transcribe(&self, cancellation: InstallCancellation) {
            self.state.lock().unwrap().cancel_install_on_transcribe = Some(cancellation);
        }

        fn cancel_install_on_health(&self, cancellation: InstallCancellation) {
            self.state.lock().unwrap().cancel_install_on_health = Some(cancellation);
        }

        fn cancel_install_on_load_call(&self, call: usize, cancellation: InstallCancellation) {
            self.state.lock().unwrap().cancel_install_on_load_call = Some((call, cancellation));
        }
    }

    impl OnnxSupervisorControl for FakeOnnxControl {
        fn load(
            &self,
            _session_id: u64,
            _request_id: u64,
            model: OnnxModelSpec,
        ) -> anyhow::Result<bool> {
            let mut state = self.state.lock().unwrap();
            state.load_calls += 1;
            if state
                .cancel_install_on_load_call
                .as_ref()
                .is_some_and(|(call, _)| *call == state.load_calls)
                && let Some((_, cancellation)) = state.cancel_install_on_load_call.take()
            {
                cancellation.cancel();
            }
            let warm = state.loaded.as_ref() == Some(&model);
            if !warm {
                if let Some(previous) = state.loaded.take() {
                    state.events.push(format!("evict:{}", previous.id));
                }
                state.events.push(format!("load:{}", model.id));
            }
            if state.fail_load_call == Some(state.load_calls) {
                state.fail_load_call = None;
                state.loaded = Some(model);
                state.maximum_loaded_models = state.maximum_loaded_models.max(1);
                anyhow::bail!("deterministic ONNX load failure on call");
            }
            if std::mem::take(&mut state.fail_next_load) {
                anyhow::bail!("deterministic ONNX load failure");
            }
            state.loaded = Some(model);
            state.maximum_loaded_models = state.maximum_loaded_models.max(1);
            Ok(warm)
        }

        fn transcribe(
            &self,
            _session_id: u64,
            _request_id: u64,
            _samples: &[f32],
        ) -> anyhow::Result<String> {
            let mut state = self.state.lock().unwrap();
            state.transcribe_calls += 1;
            if let Some(cancellation) = state.cancel_install_on_transcribe.take() {
                cancellation.cancel();
            }
            if state.fail_transcribe {
                state.loaded = None;
                anyhow::bail!("deterministic ONNX transcribe failure");
            }
            state.batch_started = true;
            self.changed.notify_all();
            while state.block_batch && !state.batch_cancelled {
                state = self.changed.wait(state).unwrap();
            }
            if state.batch_cancelled {
                anyhow::bail!("fake ONNX batch cancelled");
            }
            Ok("neutral fake transcript".to_owned())
        }

        fn start_stream(&self, session_id: u64, _request_id: u64) -> anyhow::Result<()> {
            let mut state = self.state.lock().unwrap();
            if state.fail_start_stream {
                state.loaded = None;
                anyhow::bail!("deterministic ONNX start-stream failure");
            }
            if state.stream.is_some() {
                anyhow::bail!("fake ONNX stream already active");
            }
            state.stream = Some((session_id, 0));
            Ok(())
        }

        fn audio_chunk(
            &self,
            session_id: u64,
            _request_id: u64,
            _samples: &[f32],
        ) -> anyhow::Result<String> {
            let mut state = self.state.lock().unwrap();
            let stream = state
                .stream
                .as_mut()
                .filter(|stream| stream.0 == session_id)
                .ok_or_else(|| anyhow!("no matching fake ONNX stream"))?;
            stream.1 += 1;
            Ok(format!("partial-{}", stream.1))
        }

        fn end_stream(&self, session_id: u64, _request_id: u64) -> anyhow::Result<String> {
            let mut state = self.state.lock().unwrap();
            let (_, chunks) = state
                .stream
                .take()
                .filter(|stream| stream.0 == session_id)
                .ok_or_else(|| anyhow!("no matching fake ONNX stream"))?;
            Ok(format!("final-{chunks}"))
        }

        fn cancel_stream(&self, session_id: u64, _request_id: u64) -> anyhow::Result<()> {
            let mut state = self.state.lock().unwrap();
            if state
                .stream
                .is_none_or(|(active_session, _)| active_session != session_id)
            {
                anyhow::bail!("no matching fake ONNX stream");
            }
            state.stream = None;
            state.stream_cancels += 1;
            Ok(())
        }

        fn health(&self, _session_id: u64, _request_id: u64) -> anyhow::Result<()> {
            let mut state = self.state.lock().unwrap();
            state.health_calls += 1;
            if let Some(cancellation) = state.cancel_install_on_health.take() {
                cancellation.cancel();
            }
            if state.fail_health {
                state.loaded = None;
                anyhow::bail!("deterministic ONNX health failure");
            }
            Ok(())
        }

        fn unload(&self) -> anyhow::Result<()> {
            let mut state = self.state.lock().unwrap();
            state.unload_calls += 1;
            if state.stream.is_some() {
                state.unloaded_active_streams += 1;
            }
            state.stream = None;
            state.loaded = None;
            Ok(())
        }

        fn cancel_active(&self) -> anyhow::Result<()> {
            let mut state = self.state.lock().unwrap();
            state.cancel_active_calls += 1;
            state.batch_cancelled = true;
            self.changed.notify_all();
            Ok(())
        }

        fn abandon_stream(&self, session_id: u64) {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if state
                .stream
                .is_some_and(|(active_session, _)| active_session == session_id)
            {
                state.stream = None;
                state.stream_cancels += 1;
            }
        }

        fn terminate_current(&self) -> anyhow::Result<()> {
            let mut state = self.state.lock().unwrap();
            state.termination_calls += 1;
            state.events.push("terminate:onnx".to_owned());
            state.stream = None;
            state.loaded = None;
            state.batch_cancelled = true;
            self.changed.notify_all();
            Ok(())
        }
    }

    fn onnx_spec(label: &str, family: OnnxModelFamily) -> (PathBuf, OnnxModelSpec) {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "scribe-router-onnx-{label}-{}-{suffix}",
            std::process::id()
        ));
        fs::create_dir_all(&root).unwrap();
        let roles = match family {
            OnnxModelFamily::OnlineTransducer | OnnxModelFamily::OfflineTransducer => vec![
                OnnxFileRole::Encoder,
                OnnxFileRole::Decoder,
                OnnxFileRole::Joiner,
                OnnxFileRole::Tokens,
            ],
            OnnxModelFamily::NemoCtc => vec![OnnxFileRole::Model, OnnxFileRole::Tokens],
            other => panic!("test helper does not define a fixture for {other:?}"),
        };
        let files = roles
            .into_iter()
            .map(|role| {
                let relative = PathBuf::from(format!("{role:?}.fixture").to_ascii_lowercase());
                fs::write(root.join(&relative), format!("{label}-{role:?}")).unwrap();
                (role, relative)
            })
            .collect::<BTreeMap<_, _>>();
        let spec = OnnxModelSpec {
            id: format!("private-{label}"),
            root: root.clone(),
            family,
            files,
            num_threads: 1,
        };
        (root, spec)
    }

    fn onnx_test_service(
        preference: AccelerationPreference,
    ) -> (
        TranscriptionService,
        Arc<FakeOnnxControl>,
        Arc<std::sync::atomic::AtomicUsize>,
    ) {
        let control = Arc::new(FakeOnnxControl::default());
        let spawn_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let factory_control = Arc::clone(&control);
        let factory_spawn_count = Arc::clone(&spawn_count);
        let router = RuntimeRouter::with_test_onnx_factory(move || {
            factory_spawn_count.fetch_add(1, Ordering::AcqRel);
            Ok(Arc::clone(&factory_control) as Arc<dyn OnnxSupervisorControl>)
        });
        let mut config = AppConfig::default();
        config.performance.acceleration_preference = preference;
        (
            TranscriptionService::with_runtime_router(config, router),
            control,
            spawn_count,
        )
    }

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
    fn private_runtime_artifact_routes_onnx_load_health_and_transcribe_through_worker() {
        let (root, spec) = onnx_spec("online-service", OnnxModelFamily::OnlineTransducer);
        let artifact = RuntimeArtifact::OnnxBundle(spec);
        let (service, control, spawn_count) = onnx_test_service(AccelerationPreference::Cpu);

        let cold = service.preload_runtime_artifact(artifact.clone()).unwrap();
        let warm = service.preload_runtime_artifact(artifact.clone()).unwrap();
        service
            .health_check_runtime_artifact(artifact.clone())
            .unwrap();
        let execution = service
            .transcribe_runtime_artifact(
                artifact,
                prepared_audio(),
                TranscriptionOptions::default(),
            )
            .unwrap();

        assert!(!cold.diagnostics.warm_reused);
        assert!(warm.diagnostics.warm_reused);
        assert_eq!(
            cold.diagnostics.resolved_acceleration.requested,
            AccelerationPreference::Cpu
        );
        assert_eq!(
            cold.diagnostics.resolved_acceleration.resolved,
            ComputeDevice::Cpu
        );
        assert!(cold.capabilities.cancellation);
        assert!(cold.capabilities.streaming);
        assert!(!cold.capabilities.timestamps);
        assert!(!cold.capabilities.language_detection);
        assert_eq!(cold.detected_architecture, "online-transducer");
        assert_eq!(execution.transcript.text, "neutral fake transcript");
        assert!(execution.transcript.segments.is_empty());
        assert_eq!(execution.transcript.detected_language, None);
        assert_eq!(execution.transcript.duration_ms, None);
        assert!(execution.diagnostics.warm_reused);
        assert_eq!(spawn_count.load(Ordering::Acquire), 1);
        let state = control.state.lock().unwrap();
        assert_eq!(state.health_calls, 1);
        assert_eq!(state.transcribe_calls, 1);
        drop(state);
        service.unload_runtime_artifacts().unwrap();
        assert_eq!(control.state.lock().unwrap().unload_calls, 1);

        drop(service);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn receipt_backed_onnx_bundle_routes_below_transcription_service() {
        let (root, spec) = onnx_spec("receipt-service", OnnxModelFamily::OnlineTransducer);
        crate::onnx_model_bundles::write_test_receipt_for_spec(&spec).unwrap();
        let (service, control, _) = onnx_test_service(AccelerationPreference::Cpu);

        assert!(service.preload_onnx_bundle_from_receipt(&root).is_err());
        assert!(
            service
                .transcribe_onnx_bundle_from_receipt(
                    &root,
                    prepared_audio(),
                    TranscriptionOptions::default(),
                )
                .is_err()
        );
        assert_eq!(control.state.lock().unwrap().transcribe_calls, 0);

        fs::write(root.join("unexpected.onnx"), b"unexpected").unwrap();
        assert!(service.preload_onnx_bundle_from_receipt(&root).is_err());
        service.unload_runtime_artifacts().unwrap();
        drop(service);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn staged_receipt_smoke_uses_fixed_cpu_and_observed_worker_evidence() {
        let (root, spec) = onnx_spec("receipt-smoke", OnnxModelFamily::OnlineTransducer);
        crate::onnx_model_bundles::write_test_receipt_for_spec(&spec).unwrap();
        let (service, control, _) = onnx_test_service(AccelerationPreference::Gpu);
        let smoke = service
            .verify_onnx_artifact_smoke(
                RuntimeArtifact::OnnxBundle(spec),
                &InstallCancellation::default(),
            )
            .unwrap();

        assert_eq!(
            smoke.resolved_acceleration.requested,
            AccelerationPreference::Cpu
        );
        assert_eq!(smoke.resolved_acceleration.resolved, ComputeDevice::Cpu);
        assert_eq!(smoke.detected_architecture, "online-transducer");
        assert!(smoke.capabilities.streaming);
        let state = control.state.lock().unwrap();
        assert_eq!(state.health_calls, 1);
        assert_eq!(state.transcribe_calls, 1);
        assert_eq!(state.load_calls, 4);
        assert_eq!(state.unload_calls, 3);
        assert!(state.loaded.is_none());
        drop(state);
        drop(service);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn staged_onnx_smoke_unloads_after_decode_failure_and_preserves_primary_error() {
        let (root, spec) = onnx_spec("smoke-decode-failure", OnnxModelFamily::OnlineTransducer);
        let (service, control, _) = onnx_test_service(AccelerationPreference::Cpu);
        control.fail_transcribe();

        let error = service
            .verify_onnx_artifact_smoke(
                RuntimeArtifact::OnnxBundle(spec),
                &InstallCancellation::default(),
            )
            .unwrap_err();

        assert!(error.to_string().contains("decode smoke failed"));
        let state = control.state.lock().unwrap();
        assert_eq!(state.unload_calls, 2);
        assert!(state.loaded.is_none());
        drop(state);
        assert_eq!(service.router.onnx_state_for_test(), (false, false, false));
        drop(service);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn staged_onnx_smoke_unloads_when_install_is_cancelled_during_decode() {
        let (root, spec) = onnx_spec("smoke-cancel", OnnxModelFamily::OnlineTransducer);
        let (service, control, _) = onnx_test_service(AccelerationPreference::Cpu);
        let cancellation = InstallCancellation::default();
        control.cancel_install_on_transcribe(cancellation.clone());

        let error = service
            .verify_onnx_artifact_smoke(RuntimeArtifact::OnnxBundle(spec), &cancellation)
            .unwrap_err();

        assert!(error.to_string().contains("cancelled"));
        let state = control.state.lock().unwrap();
        assert_eq!(state.unload_calls, 2);
        assert!(state.loaded.is_none());
        drop(state);
        assert_eq!(service.router.onnx_state_for_test(), (false, false, false));
        drop(service);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn staged_onnx_smoke_unloads_when_cancelled_after_health() {
        let (root, spec) = onnx_spec("smoke-cancel-health", OnnxModelFamily::OnlineTransducer);
        let (service, control, _) = onnx_test_service(AccelerationPreference::Cpu);
        let cancellation = InstallCancellation::default();
        control.cancel_install_on_health(cancellation.clone());

        let error = service
            .verify_onnx_artifact_smoke(RuntimeArtifact::OnnxBundle(spec), &cancellation)
            .unwrap_err();

        assert!(error.to_string().contains("cancelled"));
        let state = control.state.lock().unwrap();
        assert_eq!(state.health_calls, 1);
        assert_eq!(state.unload_calls, 1);
        assert!(state.loaded.is_none());
        drop(state);
        assert_eq!(service.router.onnx_state_for_test(), (false, false, false));
        drop(service);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn staged_onnx_smoke_unloads_when_cancelled_immediately_after_load() {
        let (root, spec) = onnx_spec("smoke-cancel-load", OnnxModelFamily::OnlineTransducer);
        let (service, control, _) = onnx_test_service(AccelerationPreference::Cpu);
        let cancellation = InstallCancellation::default();
        control.cancel_install_on_load_call(2, cancellation.clone());

        let error = service
            .verify_onnx_artifact_smoke(RuntimeArtifact::OnnxBundle(spec), &cancellation)
            .unwrap_err();

        assert!(error.to_string().contains("cancelled"));
        let state = control.state.lock().unwrap();
        assert_eq!(state.health_calls, 1);
        assert_eq!(state.load_calls, 2);
        assert_eq!(state.unload_calls, 2);
        assert!(state.loaded.is_none());
        drop(state);
        assert_eq!(service.router.onnx_state_for_test(), (false, false, false));
        drop(service);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn staged_onnx_smoke_unloads_after_reload_load_failure() {
        let (root, spec) = onnx_spec("smoke-reload-failure", OnnxModelFamily::OnlineTransducer);
        let (service, control, _) = onnx_test_service(AccelerationPreference::Cpu);
        control.fail_load_call(4);

        let error = service
            .verify_onnx_artifact_smoke(
                RuntimeArtifact::OnnxBundle(spec),
                &InstallCancellation::default(),
            )
            .unwrap_err();

        assert!(error.to_string().contains("reload failed"));
        let state = control.state.lock().unwrap();
        assert_eq!(state.load_calls, 4);
        assert_eq!(state.unload_calls, 3);
        assert!(state.loaded.is_none());
        drop(state);
        assert_eq!(service.router.onnx_state_for_test(), (false, false, false));
        drop(service);
        fs::remove_dir_all(root).unwrap();
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
        let service = TranscriptionService::new(config);
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
    fn onnx_same_model_reuses_warm_and_changed_model_evicts_first() {
        let (first_root, first) = onnx_spec("same-model-first", OnnxModelFamily::NemoCtc);
        let (second_root, second) = onnx_spec("same-model-second", OnnxModelFamily::NemoCtc);
        let (service, control, spawn_count) = onnx_test_service(AccelerationPreference::Cpu);

        let cold = service
            .preload_runtime_artifact(RuntimeArtifact::OnnxBundle(first.clone()))
            .unwrap();
        let warm = service
            .preload_runtime_artifact(RuntimeArtifact::OnnxBundle(first))
            .unwrap();
        let replacement = service
            .preload_runtime_artifact(RuntimeArtifact::OnnxBundle(second))
            .unwrap();

        assert!(!cold.diagnostics.warm_reused);
        assert!(warm.diagnostics.warm_reused);
        assert!(!replacement.diagnostics.warm_reused);
        assert_eq!(spawn_count.load(Ordering::Acquire), 1);
        let state = control.state.lock().unwrap();
        assert_eq!(state.maximum_loaded_models, 1);
        assert_eq!(
            state.events,
            [
                "load:private-same-model-first",
                "evict:private-same-model-first",
                "load:private-same-model-second",
            ]
        );
        drop(state);

        service.unload_runtime_artifacts().unwrap();
        drop(service);
        fs::remove_dir_all(first_root).unwrap();
        fs::remove_dir_all(second_root).unwrap();
    }

    #[test]
    fn failed_onnx_load_discards_adapter_owner_and_cancellation_then_recovers_cold() {
        let (first_root, first) = onnx_spec("failed-load-first", OnnxModelFamily::NemoCtc);
        let (second_root, second) = onnx_spec("failed-load-second", OnnxModelFamily::NemoCtc);
        let (service, control, spawn_count) = onnx_test_service(AccelerationPreference::Cpu);
        service
            .preload_runtime_artifact(RuntimeArtifact::OnnxBundle(first.clone()))
            .unwrap();
        control.fail_next_load();

        let error = service
            .preload_runtime_artifact(RuntimeArtifact::OnnxBundle(second))
            .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("deterministic ONNX load failure")
        );
        assert_eq!(service.router.onnx_state_for_test(), (false, false, false));
        assert_eq!(service.router.runtime_activity().active_streams(), 0);
        assert!(control.state.lock().unwrap().loaded.is_none());
        let recovered = service
            .preload_runtime_artifact(RuntimeArtifact::OnnxBundle(first))
            .unwrap();
        assert!(!recovered.diagnostics.warm_reused);
        assert_eq!(spawn_count.load(Ordering::Acquire), 2);

        service.unload_runtime_artifacts().unwrap();
        drop(service);
        fs::remove_dir_all(first_root).unwrap();
        fs::remove_dir_all(second_root).unwrap();
    }

    #[test]
    fn failed_model_replacement_during_stream_unloads_before_discard_and_recovers() {
        let (first_root, first) = onnx_spec(
            "stream-replacement-first",
            OnnxModelFamily::OnlineTransducer,
        );
        let (second_root, second) = onnx_spec(
            "stream-replacement-second",
            OnnxModelFamily::OnlineTransducer,
        );
        let (service, control, spawn_count) = onnx_test_service(AccelerationPreference::Cpu);
        let mut stream = service
            .start_runtime_stream(
                RuntimeArtifact::OnnxBundle(first.clone()),
                TranscriptionOptions::default(),
            )
            .unwrap();
        assert_eq!(service.router.runtime_activity().active_streams(), 1);
        control.fail_next_load();

        let error = service
            .preload_runtime_artifact(RuntimeArtifact::OnnxBundle(second))
            .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("deterministic ONNX load failure")
        );
        {
            let state = control.state.lock().unwrap();
            assert_eq!(state.unload_calls, 1);
            assert_eq!(state.termination_calls, 0);
            assert!(state.loaded.is_none());
            assert!(state.stream.is_none());
            assert_eq!(state.maximum_loaded_models, 1);
        }
        assert_eq!(service.router.onnx_state_for_test(), (false, false, false));
        assert_eq!(service.router.runtime_activity().active_streams(), 0);
        assert!(stream.push_audio(&[0.1]).is_err());
        assert_eq!(spawn_count.load(Ordering::Acquire), 1);

        let recovered = service
            .preload_runtime_artifact(RuntimeArtifact::OnnxBundle(first))
            .unwrap();
        assert!(!recovered.diagnostics.warm_reused);
        assert_eq!(spawn_count.load(Ordering::Acquire), 2);
        drop(stream);

        service.unload_runtime_artifacts().unwrap();
        drop(service);
        fs::remove_dir_all(first_root).unwrap();
        fs::remove_dir_all(second_root).unwrap();
    }

    #[test]
    fn failed_onnx_transcribe_discards_adapter_owner_and_cancellation() {
        let (root, spec) = onnx_spec("failed-transcribe", OnnxModelFamily::NemoCtc);
        let artifact = RuntimeArtifact::OnnxBundle(spec);
        let (service, control, spawn_count) = onnx_test_service(AccelerationPreference::Cpu);
        service.preload_runtime_artifact(artifact.clone()).unwrap();
        control.fail_transcribe();

        let error = service
            .transcribe_runtime_artifact(
                artifact.clone(),
                prepared_audio(),
                TranscriptionOptions::default(),
            )
            .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("deterministic ONNX transcribe failure")
        );
        assert_eq!(service.router.onnx_state_for_test(), (false, false, false));
        assert_eq!(service.router.runtime_activity().active_streams(), 0);
        let recovered = service.preload_runtime_artifact(artifact).unwrap();
        assert!(!recovered.diagnostics.warm_reused);
        assert_eq!(spawn_count.load(Ordering::Acquire), 2);

        service.unload_runtime_artifacts().unwrap();
        drop(service);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn failed_onnx_health_and_stream_start_discard_adapter_state() {
        let (health_root, health_spec) = onnx_spec("failed-health", OnnxModelFamily::NemoCtc);
        let (health_service, health_control, _) = onnx_test_service(AccelerationPreference::Cpu);
        health_service
            .preload_runtime_artifact(RuntimeArtifact::OnnxBundle(health_spec.clone()))
            .unwrap();
        health_control.state.lock().unwrap().fail_health = true;
        assert!(
            health_service
                .health_check_runtime_artifact(RuntimeArtifact::OnnxBundle(health_spec))
                .is_err()
        );
        assert_eq!(
            health_service.router.onnx_state_for_test(),
            (false, false, false)
        );

        let (stream_root, stream_spec) =
            onnx_spec("failed-start", OnnxModelFamily::OnlineTransducer);
        let (stream_service, stream_control, _) = onnx_test_service(AccelerationPreference::Cpu);
        stream_control.state.lock().unwrap().fail_start_stream = true;
        assert!(
            stream_service
                .start_runtime_stream(
                    RuntimeArtifact::OnnxBundle(stream_spec),
                    TranscriptionOptions::default(),
                )
                .is_err()
        );
        assert_eq!(
            stream_service.router.onnx_state_for_test(),
            (false, false, false)
        );
        assert_eq!(stream_service.router.runtime_activity().active_streams(), 0);

        drop(health_service);
        drop(stream_service);
        fs::remove_dir_all(health_root).unwrap();
        fs::remove_dir_all(stream_root).unwrap();
    }

    #[test]
    fn private_onnx_gpu_request_fails_before_supervisor_spawn() {
        let (root, spec) = onnx_spec("gpu-rejected", OnnxModelFamily::NemoCtc);
        let (service, _control, spawn_count) = onnx_test_service(AccelerationPreference::Gpu);

        let error = service
            .preload_runtime_artifact(RuntimeArtifact::OnnxBundle(spec))
            .unwrap_err();

        assert!(error.to_string().contains("CPU-only"));
        assert!(error.to_string().contains("Auto or CPU only"));
        assert_eq!(spawn_count.load(Ordering::Acquire), 0);
        drop(service);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn private_onnx_stream_maps_partial_final_cancel_and_rejects_offline_bundle() {
        let (online_root, online) = onnx_spec("online-stream", OnnxModelFamily::OnlineTransducer);
        let (offline_root, offline) = onnx_spec("offline-stream", OnnxModelFamily::NemoCtc);
        let (service, control, _spawn_count) = onnx_test_service(AccelerationPreference::Auto);

        let mut stream = service
            .start_runtime_stream(
                RuntimeArtifact::OnnxBundle(online.clone()),
                TranscriptionOptions::default(),
            )
            .unwrap();
        assert_eq!(service.router.runtime_activity().active_streams(), 1);
        assert_eq!(
            stream.push_audio(&[0.1]).unwrap(),
            StreamUpdate {
                committed: String::new(),
                tentative: "partial-1".to_owned(),
            }
        );
        assert_eq!(stream.push_audio(&[-0.1]).unwrap().tentative, "partial-2");
        let final_transcript = stream.finalize().unwrap();
        assert_eq!(service.router.runtime_activity().active_streams(), 0);
        assert_eq!(final_transcript.text, "final-2");
        assert!(final_transcript.segments.is_empty());
        assert_eq!(final_transcript.detected_language, None);

        let stream = service
            .start_runtime_stream(
                RuntimeArtifact::OnnxBundle(online.clone()),
                TranscriptionOptions::default(),
            )
            .unwrap();
        assert_eq!(service.router.runtime_activity().active_streams(), 1);
        stream.cancel().unwrap();
        assert_eq!(service.router.runtime_activity().active_streams(), 0);
        assert_eq!(control.state.lock().unwrap().stream_cancels, 1);

        let stream = service
            .start_runtime_stream(
                RuntimeArtifact::OnnxBundle(online.clone()),
                TranscriptionOptions::default(),
            )
            .unwrap();
        assert_eq!(service.router.runtime_activity().active_streams(), 1);
        drop(stream);
        assert_eq!(service.router.runtime_activity().active_streams(), 0);
        assert_eq!(control.state.lock().unwrap().stream_cancels, 2);

        let error = service
            .start_runtime_stream(
                RuntimeArtifact::OnnxBundle(offline),
                TranscriptionOptions::default(),
            )
            .err()
            .expect("offline ONNX bundles must not return a stream handle");
        assert!(error.to_string().contains("online ONNX transducer"));

        drop(service);
        fs::remove_dir_all(online_root).unwrap();
        fs::remove_dir_all(offline_root).unwrap();
    }

    #[test]
    fn explicit_unload_clears_active_stream_lease_and_native_stream() {
        let (root, spec) = onnx_spec("explicit-stream-unload", OnnxModelFamily::OnlineTransducer);
        let (service, control, _) = onnx_test_service(AccelerationPreference::Cpu);
        let stream = service
            .start_runtime_stream(
                RuntimeArtifact::OnnxBundle(spec),
                TranscriptionOptions::default(),
            )
            .unwrap();
        assert_eq!(service.router.runtime_activity().active_streams(), 1);

        service.unload_runtime_artifacts().unwrap();

        assert_eq!(service.router.runtime_activity().active_streams(), 0);
        assert_eq!(control.state.lock().unwrap().unloaded_active_streams, 1);
        assert_eq!(service.router.onnx_state_for_test(), (false, false, false));
        drop(stream);
        assert_eq!(service.router.runtime_activity().active_streams(), 0);

        drop(service);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn active_onnx_batch_cancel_does_not_wait_for_router_state_lock() {
        let (root, spec) = onnx_spec("batch-cancel", OnnxModelFamily::NemoCtc);
        let artifact = RuntimeArtifact::OnnxBundle(spec);
        let (service, control, _spawn_count) = onnx_test_service(AccelerationPreference::Cpu);
        service.preload_runtime_artifact(artifact.clone()).unwrap();
        control.set_block_batch();

        let worker_service = service.clone();
        let batch = std::thread::spawn(move || {
            worker_service.transcribe_runtime_artifact(
                artifact,
                prepared_audio(),
                TranscriptionOptions::default(),
            )
        });
        control.wait_for_batch();
        let cancel_started = Instant::now();
        service.cancel_active();
        let error = batch.join().unwrap().unwrap_err();

        assert!(
            cancel_started.elapsed() <= Duration::from_millis(250),
            "ONNX cancellation waited for the router state mutex"
        );
        assert!(error.to_string().contains("cancelled"));
        assert_eq!(control.state.lock().unwrap().cancel_active_calls, 1);

        drop(service);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    #[ignore = "manual: requires local GGUF, WAV, and retained legacy GGML paths"]
    fn manual_known_wav_gguf_migration_smoke_uses_the_pinned_candidate_handoff() {
        let gguf = PathBuf::from(
            std::env::var("SCRIBE_TRANSCRIBE_CPP_GGUF")
                .expect("set SCRIBE_TRANSCRIBE_CPP_GGUF to the exact pinned base Q8 GGUF"),
        );
        let wav = PathBuf::from(
            std::env::var("SCRIBE_TRANSCRIBE_CPP_AUDIO")
                .expect("set SCRIBE_TRANSCRIBE_CPP_AUDIO to a known spoken WAV"),
        );
        let legacy = PathBuf::from(
            std::env::var("SCRIBE_TRANSCRIBE_CPP_LEGACY")
                .expect("set SCRIBE_TRANSCRIBE_CPP_LEGACY to the retained ggml-base.en.bin"),
        );
        assert!(legacy.is_file(), "the retained legacy GGML file must exist");
        let model_id = ModelId::new("whisper_cpp_base_en");
        let candidate = InstallationCandidate::normalized(model_id.clone(), gguf.clone(), None)
            .expect("the supplied GGUF filename must match the catalog-pinned base Q8 artifact");
        let expected_size_bytes = candidate.expected_size_bytes;
        let expected_sha256 = candidate.expected_sha256.clone();
        let audio = Arc::new(PreparedAudio::from_wav_path(&wav).expect("load the supplied WAV"));
        assert!(
            !audio.samples.is_empty(),
            "the supplied WAV must contain audio"
        );

        let mut config = AppConfig::default();
        config.general.selected_default_model = model_id.as_str().to_owned();
        config
            .general
            .model_paths
            .insert(model_id.as_str().to_owned(), legacy.clone());
        let prior = serde_json::to_value(&config).unwrap();
        let service = TranscriptionService::new(config.clone());
        let cancellation = InstallCancellation::default();
        let verified = service
            .verify_installation_candidate_for_activation_with(
                candidate,
                &cancellation,
                |candidate| {
                    let smoke =
                        service.verify_installation_candidate(candidate.clone(), &cancellation)?;
                    let mut request = TranscriptionRequest::new(
                        SessionId(1),
                        RequestId(1),
                        Arc::clone(&audio),
                        model_id.clone(),
                    );
                    request.model_path = Some(gguf.clone());
                    let transcript = service.transcribe(request)?.transcript.text;
                    assert!(
                        !transcript.trim().is_empty(),
                        "known-WAV smoke must produce a non-empty transcript"
                    );
                    if let Ok(expected) = std::env::var("SCRIBE_TRANSCRIBE_CPP_EXPECTED_TRANSCRIPT")
                    {
                        assert!(
                            transcript.contains(&expected),
                            "transcript did not contain SCRIBE_TRANSCRIBE_CPP_EXPECTED_TRANSCRIPT"
                        );
                    }
                    Ok(smoke)
                },
            )
            .expect("the local GGUF must pass its catalog pin and known-WAV smoke");
        verified
            .authorize_activation(
                &model_id,
                &gguf,
                expected_size_bytes,
                &expected_sha256,
                &cancellation,
            )
            .expect("only a verified candidate may switch the configured path");
        config
            .general
            .model_paths
            .insert(model_id.as_str().to_owned(), gguf);
        assert_eq!(config.general.selected_default_model, model_id.as_str());
        assert_ne!(serde_json::to_value(&config).unwrap(), prior);
        assert!(
            legacy.is_file(),
            "the migration smoke must not remove legacy GGML"
        );
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
    fn preview_timestamp_policy_is_capability_gated_and_keeps_text_fallback() {
        let enabled = PreviewDecodeOptions::for_capabilities(&RuntimeCapabilities {
            timestamps: true,
            ..RuntimeCapabilities::default()
        });
        let disabled = PreviewDecodeOptions::for_capabilities(&RuntimeCapabilities::default());
        assert!(enabled.use_segment_timestamps);
        assert!(!disabled.use_segment_timestamps);

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
            disabled,
        );

        assert!(
            hypothesis
                .words
                .iter()
                .all(|word| { word.start_frame.is_none() && word.end_frame.is_none() })
        );
    }

    fn legacy_result() -> LegacyTranscriptResult {
        LegacyTranscriptResult {
            model_id: "faster_whisper_tiny_en".to_owned(),
            model_name: "faster-whisper tiny.en".to_owned(),
            backend: "faster-whisper".to_owned(),
            text: "hello world".to_owned(),
            segments: vec![LegacyTranscriptSegment {
                start_ms: Some(12),
                end_ms: Some(345),
                text: "hello world".to_owned(),
            }],
            duration_ms: Some(678),
            stdout: "runner output".to_owned(),
            stderr: "runner diagnostic".to_owned(),
        }
    }

    #[test]
    fn legacy_result_mapping_preserves_metadata_and_keeps_processing_time_separate() {
        let (transcript, diagnostics) = map_legacy_result(legacy_result());

        assert_eq!(transcript.text, "hello world");
        assert_eq!(transcript.duration_ms, None);
        assert_eq!(transcript.detected_language, None);
        assert_eq!(
            transcript.segments,
            vec![TranscriptSegment {
                text: "hello world".to_owned(),
                start_ms: Some(12),
                end_ms: Some(345),
                confidence: None,
            }]
        );
        assert_eq!(diagnostics.model_id, ModelId::new("faster_whisper_tiny_en"));
        assert_eq!(diagnostics.model_name, "faster-whisper tiny.en");
        assert_eq!(diagnostics.backend_label, "faster-whisper");
        assert_eq!(diagnostics.processing_duration_ms, Some(678));
        assert_eq!(diagnostics.stdout, "runner output");
        assert_eq!(diagnostics.stderr, "runner diagnostic");
    }

    #[test]
    fn legacy_result_mapping_preserves_unknown_processing_time_and_timestamps() {
        let mut result = legacy_result();
        result.duration_ms = None;
        result.segments = vec![LegacyTranscriptSegment {
            start_ms: None,
            end_ms: None,
            text: "unknown timing".to_owned(),
        }];

        let (transcript, diagnostics) = map_legacy_result(result);

        assert_eq!(transcript.duration_ms, None);
        assert_eq!(diagnostics.processing_duration_ms, None);
        assert_eq!(transcript.segments[0].start_ms, None);
        assert_eq!(transcript.segments[0].end_ms, None);
    }

    #[test]
    fn default_options_request_only_legacy_supported_behavior() {
        assert_eq!(
            TranscriptionOptions::default(),
            TranscriptionOptions {
                language: None,
                translate_to_english: false,
                enable_timestamps: false,
                initial_prompt: None,
            }
        );
        assert!(validate_default_options(&TranscriptionOptions::default()).is_ok());
    }

    #[test]
    fn legacy_options_fail_instead_of_being_silently_ignored() {
        let unsupported_options = [
            TranscriptionOptions {
                language: Some("en".to_owned()),
                ..TranscriptionOptions::default()
            },
            TranscriptionOptions {
                translate_to_english: true,
                ..TranscriptionOptions::default()
            },
            TranscriptionOptions {
                enable_timestamps: true,
                ..TranscriptionOptions::default()
            },
            TranscriptionOptions {
                initial_prompt: Some("domain vocabulary".to_owned()),
                ..TranscriptionOptions::default()
            },
        ];

        for options in unsupported_options {
            assert!(validate_default_options(&options).is_err());
        }
    }

    #[test]
    fn capabilities_are_conservative_for_every_legacy_backend() {
        for model in config::configured_models(&AppConfig::default()) {
            let capabilities = capabilities_for_legacy_model(&model);
            let timestamps_expected = matches!(model.backend.as_str(), "faster-whisper" | "Vosk");

            assert_eq!(
                capabilities.timestamps, timestamps_expected,
                "{} timestamp capability",
                model.backend
            );
            assert!(capabilities.cancellation, "{} cancellation", model.backend);
            assert!(!capabilities.streaming, "{} streaming", model.backend);
            assert!(!capabilities.translation, "{} translation", model.backend);
            assert!(
                !capabilities.language_detection,
                "{} language detection",
                model.backend
            );
            assert!(
                !capabilities.confidence_scores,
                "{} confidence scores",
                model.backend
            );
            assert!(
                !capabilities.custom_vocabulary,
                "{} custom vocabulary",
                model.backend
            );
            assert!(capabilities.supported_languages.is_empty());
        }
    }

    #[test]
    fn imported_gguf_uses_the_embedded_installation_binding() {
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

        assert_eq!(
            service
                .installation_binding(&ModelId::new(id))
                .unwrap()
                .managed_runtime_id,
            "embedded-transcribe-cpp"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn normalized_catalog_exposes_only_neutral_experimental_descriptors() {
        let service = TranscriptionService::new(AppConfig::default());
        let descriptors = service.model_descriptors();

        assert_eq!(descriptors.len(), 4);
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
    fn rolling_preview_rejects_legacy_models_instead_of_using_cli_fallback() {
        let service = TranscriptionService::new(AppConfig::default());
        let result = service.start_rolling_preview(
            SessionId(1),
            RequestId(1),
            ModelId::new("faster_whisper_tiny_en"),
            None,
        );

        let error = match result {
            Err(error) => error,
            Ok(_) => panic!("legacy models must not start rolling preview"),
        };
        assert!(error.to_string().contains("verified native runtime"));
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
    fn service_returns_legacy_adapter_option_errors_without_needing_a_runtime() {
        let service = TranscriptionService::new(AppConfig::default());
        let mut request = TranscriptionRequest::new(
            SessionId(4),
            RequestId(10),
            prepared_audio(),
            "whisper_cpp_tiny_en",
        );
        request.options.initial_prompt = Some("domain vocabulary".to_owned());

        let error = service.transcribe(request).unwrap_err();

        assert!(error.to_string().contains("initial prompts"));
    }

    #[test]
    fn legacy_adapter_reports_unimplemented_health_check_without_a_runtime() {
        let model = config::configured_models(&AppConfig::default())
            .into_iter()
            .find(|model| model.id == "whisper_cpp_tiny_en")
            .expect("whisper.cpp tiny model exists");
        let mut adapter = LegacyBatchAdapter::new(
            AppConfig::default(),
            model,
            crate::stt::cancellation_snapshot(),
        );

        let error = adapter.health_check().unwrap_err();

        assert!(error.to_string().contains("not implemented"));
    }

    #[test]
    fn legacy_adapter_has_explicit_stateless_load_and_process_cancel_semantics() {
        let model = config::configured_models(&AppConfig::default())
            .into_iter()
            .find(|model| model.id == "whisper_cpp_tiny_en")
            .expect("whisper.cpp tiny model exists");
        let mut adapter = LegacyBatchAdapter::new(
            AppConfig::default(),
            model,
            crate::stt::cancellation_snapshot(),
        );

        adapter
            .load()
            .expect("legacy adapter has no persistent load");
        adapter
            .cancel()
            .expect("legacy child cancellation is available");
        adapter
            .unload()
            .expect("legacy adapter has no persistent unload");
    }

    #[test]
    fn legacy_bridge_wav_is_private_canonical_and_removed_on_drop() {
        let audio = prepared_audio();
        let temporary = TemporaryPreparedWav::create(&audio).unwrap();
        let path = temporary.path().to_path_buf();
        let round_trip = PreparedAudio::from_wav_path(&path).unwrap();

        assert_eq!(round_trip.sample_rate, 16_000);
        assert_eq!(round_trip.source_channels, 1);
        assert_eq!(round_trip.samples.len(), audio.samples.len());
        drop(temporary);
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
    fn legacy_response_model_must_match_the_requested_model() {
        let (_, diagnostics) = map_legacy_result(legacy_result());
        let error = validate_response_model_id(&ModelId::new("whisper_cpp_tiny_en"), &diagnostics)
            .unwrap_err();

        assert!(error.to_string().contains("returned model"));
        assert!(error.to_string().contains("faster_whisper_tiny_en"));
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
                cancellation: RuntimeRouter::new(),
            }),
        }
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

    #[test]
    #[ignore = "requires a local whisper.cpp CLI, GGML model, and JFK WAV fixture; set SCRIBE_WHISPER_CPP_CLI, SCRIBE_WHISPER_CPP_MODEL, and SCRIBE_WHISPER_CPP_AUDIO"]
    fn transcription_service_jfk_smoke_uses_the_whisper_cpp_facade() {
        let whisper_cli = PathBuf::from(
            std::env::var_os("SCRIBE_WHISPER_CPP_CLI")
                .expect("set SCRIBE_WHISPER_CPP_CLI to the pinned whisper.cpp CLI"),
        );
        let model_path = PathBuf::from(
            std::env::var_os("SCRIBE_WHISPER_CPP_MODEL")
                .expect("set SCRIBE_WHISPER_CPP_MODEL to the pinned GGML model"),
        );
        let audio_path = PathBuf::from(
            std::env::var_os("SCRIBE_WHISPER_CPP_AUDIO")
                .expect("set SCRIBE_WHISPER_CPP_AUDIO to the JFK WAV fixture"),
        );

        let mut config = AppConfig::default();
        config.developer.whisper_executable_path = Some(whisper_cli);
        let service = TranscriptionService::new(config);
        let session_id = SessionId(701);
        let request_id = RequestId(1701);
        let audio = Arc::new(
            PreparedAudio::from_wav_path(audio_path)
                .expect("the configured JFK fixture is a readable WAV"),
        );
        let mut request =
            TranscriptionRequest::new(session_id, request_id, audio.clone(), "whisper_cpp_base_en");
        request.model_path = Some(model_path.clone());

        service
            .health_check(
                &ModelId::new("whisper_cpp_base_en"),
                request.model_path.clone(),
            )
            .expect("pinned runtime package and model pass health validation");

        let outcome = service
            .transcribe(request)
            .expect("whisper.cpp facade smoke transcription succeeds");

        assert!(!outcome.transcript.text.trim().is_empty());
        assert_eq!(outcome.session_id, session_id);
        assert_eq!(outcome.request_id, request_id);
        assert_eq!(outcome.model_id, ModelId::new("whisper_cpp_base_en"));
        assert_eq!(outcome.model_name, "English Base");
        assert_eq!(outcome.backend_label, "transcribe-cpp");
        assert!(!outcome.warm_model_reused);
        assert!(
            outcome
                .model_load_duration_ms
                .is_some_and(|value| value > 0)
        );
        assert_eq!(
            outcome
                .resolved_acceleration
                .as_ref()
                .map(|resolved| &resolved.resolved),
            Some(&ComputeDevice::Cpu)
        );

        let mut warm_request = TranscriptionRequest::new(
            session_id,
            RequestId(request_id.0 + 1),
            Arc::clone(&audio),
            "whisper_cpp_base_en",
        );
        warm_request.model_path = Some(model_path.clone());
        let warm = service
            .transcribe(warm_request)
            .expect("retained native model transcribes a second request");
        assert!(warm.warm_model_reused);
        assert_eq!(warm.model_load_duration_ms, Some(0));

        service
            .unload_runtime()
            .expect("explicit native unload succeeds");
        let mut reload_request = TranscriptionRequest::new(
            session_id,
            RequestId(request_id.0 + 2),
            audio,
            "whisper_cpp_base_en",
        );
        reload_request.model_path = Some(model_path);
        let reloaded = service
            .transcribe(reload_request)
            .expect("native model reload succeeds after explicit unload");
        assert!(!reloaded.warm_model_reused);
        eprintln!(
            "native_jfk first_load_ms={} first_decode_ms={} warm_load_ms={} warm_decode_ms={}",
            outcome.model_load_duration_ms.unwrap_or_default(),
            outcome.processing_duration_ms.unwrap_or_default(),
            warm.model_load_duration_ms.unwrap_or_default(),
            warm.processing_duration_ms.unwrap_or_default(),
        );
    }

    #[test]
    #[ignore = "requires the same local pinned whisper.cpp package, base.en model, and JFK fixture as the service smoke test"]
    fn native_runtime_jfk_cold_and_warm_benchmark() {
        use std::time::Instant;

        let cli = PathBuf::from(std::env::var_os("SCRIBE_WHISPER_CPP_CLI").unwrap());
        let model_path = PathBuf::from(std::env::var_os("SCRIBE_WHISPER_CPP_MODEL").unwrap());
        let audio_path = PathBuf::from(std::env::var_os("SCRIBE_WHISPER_CPP_AUDIO").unwrap());
        let audio = Arc::new(PreparedAudio::from_wav_path(audio_path).unwrap());
        let mut config = AppConfig::default();
        config.developer.whisper_executable_path = Some(cli);
        config.performance.acceleration_preference = AccelerationPreference::Cpu;
        let make_request = |request_id: u64| {
            let mut request = TranscriptionRequest::new(
                SessionId(8_000 + request_id),
                RequestId(request_id),
                audio.clone(),
                "whisper_cpp_base_en",
            );
            request.model_path = Some(model_path.clone());
            request
        };

        let mut cold_total = Vec::new();
        let mut cold_load = Vec::new();
        for index in 0..5_u64 {
            let service = TranscriptionService::new(config.clone());
            let started = Instant::now();
            let outcome = service.transcribe(make_request(index + 1)).unwrap();
            cold_total.push(started.elapsed().as_millis());
            cold_load.push(outcome.model_load_duration_ms.unwrap());
            assert!(!outcome.warm_model_reused);
        }

        let service = TranscriptionService::new(config);
        service.transcribe(make_request(100)).unwrap();
        let mut warm_total = Vec::new();
        let mut warm_decode = Vec::new();
        for index in 0..20_u64 {
            let started = Instant::now();
            let outcome = service.transcribe(make_request(index + 101)).unwrap();
            warm_total.push(started.elapsed().as_millis());
            warm_decode.push(outcome.processing_duration_ms.unwrap());
            assert!(outcome.warm_model_reused);
        }

        eprintln!(
            "native_jfk_benchmark cold_total_median_ms={} cold_total_p95_ms={} cold_load_median_ms={} cold_load_p95_ms={} warm_total_median_ms={} warm_total_p95_ms={} warm_decode_median_ms={} warm_decode_p95_ms={}",
            percentile(&cold_total, 50),
            percentile(&cold_total, 95),
            percentile(&cold_load, 50),
            percentile(&cold_load, 95),
            percentile(&warm_total, 50),
            percentile(&warm_total, 95),
            percentile(&warm_decode, 50),
            percentile(&warm_decode, 95),
        );
    }

    #[test]
    #[ignore = "requires the same local pinned whisper.cpp package, base.en model, and JFK fixture as the service smoke test"]
    fn rolling_preview_jfk_first_partial_benchmark() {
        use sha2::{Digest, Sha256};
        use std::io::Read;
        use std::time::Instant;

        let cli = PathBuf::from(std::env::var_os("SCRIBE_WHISPER_CPP_CLI").unwrap());
        let model_path = PathBuf::from(std::env::var_os("SCRIBE_WHISPER_CPP_MODEL").unwrap());
        let audio_path = PathBuf::from(std::env::var_os("SCRIBE_WHISPER_CPP_AUDIO").unwrap());
        let verify_file = |path: &Path, expected_size: u64, expected_sha256: &str| {
            assert_eq!(fs::metadata(path).unwrap().len(), expected_size);
            let mut file = fs::File::open(path).unwrap();
            let mut hasher = Sha256::new();
            let mut buffer = [0_u8; 64 * 1024];
            loop {
                let read = file.read(&mut buffer).unwrap();
                if read == 0 {
                    break;
                }
                hasher.update(&buffer[..read]);
            }
            assert_eq!(format!("{:x}", hasher.finalize()), expected_sha256);
        };
        verify_file(
            &model_path,
            147_964_211,
            "a03779c86df3323075f5e796cb2ce5029f00ec8869eee3fdfb897afe36c6d002",
        );
        verify_file(
            &audio_path,
            352_078,
            "59dfb9a4acb36fe2a2affc14bacbee2920ff435cb13cc314a08c13f66ba7860e",
        );
        let audio = PreparedAudio::from_wav_path(audio_path).unwrap();
        let mut config = AppConfig::default();
        config.developer.whisper_executable_path = Some(cli);
        config.performance.acceleration_preference = AccelerationPreference::Cpu;

        let run_preview = |service: &TranscriptionService, run_id: u64| {
            let model_id = ModelId::new("whisper_cpp_base_en");
            let (publisher, mut handle) = service
                .start_rolling_preview(
                    SessionId(12_000 + run_id),
                    RequestId(13_000 + run_id),
                    model_id,
                    Some(model_path.clone()),
                )
                .unwrap();
            let started = Instant::now();
            let interval = crate::prepared_audio::PREPARED_SAMPLE_RATE as usize
                * crate::streaming::DECODE_INTERVAL_MS as usize
                / 1_000;
            let window = crate::prepared_audio::PREPARED_SAMPLE_RATE as usize
                * crate::streaming::ROLLING_WINDOW_MS as usize
                / 1_000;
            let mut next_end = interval;
            let deadline = started + Duration::from_secs(30);
            let first_partial = loop {
                let elapsed_intervals = (started.elapsed().as_millis()
                    / u128::from(crate::streaming::DECODE_INTERVAL_MS))
                    as usize;
                let due_end = elapsed_intervals
                    .saturating_mul(interval)
                    .min(audio.samples.len());
                while next_end <= due_end && next_end <= audio.samples.len() {
                    let start = next_end.saturating_sub(window);
                    assert!(
                        publisher
                            .publish_window(start as u64, audio.samples[start..next_end].to_vec())
                            .unwrap()
                    );
                    next_end = next_end.saturating_add(interval);
                }
                if let Some(event) = handle.try_next() {
                    match event {
                        PreviewEvent::Update { update, .. }
                            if !update.committed.is_empty() || !update.tentative.is_empty() =>
                        {
                            let text = format!("{} {}", update.committed, update.tentative)
                                .to_ascii_lowercase();
                            assert!(
                                ["and", "fellow", "americans", "country", "ask"].iter().any(
                                    |expected| text.split_whitespace().any(|word| {
                                        word.trim_matches(|character: char| {
                                            !character.is_alphanumeric()
                                        }) == *expected
                                    })
                                ),
                                "first partial did not contain an expected JFK fixture word: {text:?}"
                            );
                            break started.elapsed().as_millis();
                        }
                        PreviewEvent::Update { .. } => {}
                        PreviewEvent::Error { error, .. } => {
                            panic!("rolling preview failed: {error}")
                        }
                    }
                }
                assert!(Instant::now() < deadline, "rolling preview timed out");
                std::thread::sleep(Duration::from_millis(2));
            };
            handle.close();
            assert!(handle.stop_and_join(Duration::from_secs(5)));
            first_partial
        };

        let mut cold_first_partial = Vec::new();
        for run_id in 0..5 {
            let service = TranscriptionService::new(config.clone());
            cold_first_partial.push(run_preview(&service, run_id));
        }

        let service = TranscriptionService::new(config);
        service
            .preload_model(
                &ModelId::new("whisper_cpp_base_en"),
                Some(model_path.clone()),
            )
            .unwrap();
        let mut warm_first_partial = Vec::new();
        for run_id in 0..20 {
            warm_first_partial.push(run_preview(&service, 100 + run_id));
        }

        eprintln!(
            "rolling_preview_jfk cold_samples_ms={cold_first_partial:?} warm_samples_ms={warm_first_partial:?} first_partial_cold_median_ms={} first_partial_cold_p95_ms={} first_partial_warm_median_ms={} first_partial_warm_p95_ms={}",
            percentile(&cold_first_partial, 50),
            percentile(&cold_first_partial, 95),
            percentile(&warm_first_partial, 50),
            percentile(&warm_first_partial, 95),
        );
    }

    #[test]
    #[ignore = "requires the same local pinned whisper.cpp package, base.en model, and JFK fixture as the service smoke test"]
    fn native_runtime_cancellation_interrupts_active_decode() {
        let cli = PathBuf::from(std::env::var_os("SCRIBE_WHISPER_CPP_CLI").unwrap());
        let model_path = PathBuf::from(std::env::var_os("SCRIBE_WHISPER_CPP_MODEL").unwrap());
        let audio_path = PathBuf::from(std::env::var_os("SCRIBE_WHISPER_CPP_AUDIO").unwrap());
        let fixture = PreparedAudio::from_wav_path(audio_path).unwrap();
        let mut samples = Vec::with_capacity(fixture.samples.len() * 20);
        for _ in 0..20 {
            samples.extend_from_slice(&fixture.samples);
        }
        let audio = Arc::new(PreparedAudio {
            source_frames: samples.len(),
            samples,
            sample_rate: fixture.sample_rate,
            source_sample_rate: fixture.sample_rate,
            source_channels: 1,
        });
        let mut config = AppConfig::default();
        config.developer.whisper_executable_path = Some(cli);
        config.performance.acceleration_preference = AccelerationPreference::Cpu;
        let service = TranscriptionService::new(config);
        service
            .preload_model(
                &ModelId::new("whisper_cpp_base_en"),
                Some(model_path.clone()),
            )
            .unwrap();

        let worker_service = service.clone();
        let worker = std::thread::spawn(move || {
            let mut request = TranscriptionRequest::new(
                SessionId(9_001),
                RequestId(9_001),
                audio,
                "whisper_cpp_base_en",
            );
            request.model_path = Some(model_path);
            worker_service.transcribe(request)
        });
        std::thread::sleep(std::time::Duration::from_millis(50));
        let cancel_started = std::time::Instant::now();
        service.cancel_active();
        let error = worker.join().unwrap().unwrap_err();
        let cancellation_latency = cancel_started.elapsed();
        eprintln!("native_cancel_ack_ms={}", cancellation_latency.as_millis());

        assert!(cancellation_latency <= std::time::Duration::from_secs(2));
        assert!(error.to_string().contains("inference failed"));
    }

    #[test]
    fn configured_managed_runtime_root_requires_exact_catalog_target() {
        let root = std::env::temp_dir().join(format!(
            "scribe-managed-runtime-root-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let storage = root.join("runtimes");
        let expected_root = storage.join("whisper_cpp");
        let expected_entrypoint = expected_root.join("bin").join("whisper-cli.exe");
        let external_entrypoint = root.join("external").join("bin").join("whisper-cli.exe");
        fs::create_dir_all(expected_entrypoint.parent().unwrap()).unwrap();
        fs::create_dir_all(external_entrypoint.parent().unwrap()).unwrap();
        fs::write(&expected_entrypoint, b"expected").unwrap();
        fs::write(&external_entrypoint, b"external").unwrap();
        let mut config = AppConfig::default();
        config.general.managed_runtimes.insert(
            "whisper_cpp".to_owned(),
            config::ManagedRuntimeInstall::app_managed(expected_entrypoint.clone(), "test"),
        );

        assert_eq!(
            configured_managed_runtime_root_in(&config, "whisper_cpp", &storage).unwrap(),
            Some(expected_root.clone())
        );

        config.general.managed_runtimes.insert(
            "whisper_cpp".to_owned(),
            config::ManagedRuntimeInstall::app_managed(external_entrypoint, "test"),
        );
        assert!(
            configured_managed_runtime_root_in(&config, "whisper_cpp", &storage)
                .unwrap_err()
                .to_string()
                .contains("outside its catalog target")
        );

        let arbitrary_entrypoint = expected_root.join("bin").join("arbitrary.exe");
        fs::write(&arbitrary_entrypoint, b"arbitrary").unwrap();
        config.general.managed_runtimes.insert(
            "whisper_cpp".to_owned(),
            config::ManagedRuntimeInstall::app_managed(arbitrary_entrypoint, "test"),
        );
        assert!(
            configured_managed_runtime_root_in(&config, "whisper_cpp", &storage)
                .unwrap_err()
                .to_string()
                .contains("exact pinned entrypoint")
        );

        config.general.managed_runtimes.insert(
            "whisper_cpp".to_owned(),
            config::ManagedRuntimeInstall::app_managed(
                expected_root.join("bin").join("..").join("whisper-cli.exe"),
                "test",
            ),
        );
        assert!(
            configured_managed_runtime_root_in(&config, "whisper_cpp", &storage)
                .unwrap_err()
                .to_string()
                .contains("parent-directory")
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn configured_managed_runtime_root_rejects_symlinked_catalog_directory() {
        use std::os::unix::fs::symlink;

        let root = std::env::temp_dir().join(format!(
            "scribe-managed-runtime-link-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let storage = root.join("runtimes");
        let external = root.join("external");
        fs::create_dir_all(&storage).unwrap();
        fs::create_dir_all(external.join("bin")).unwrap();
        fs::write(external.join("bin").join("whisper-cli"), b"external").unwrap();
        symlink(&external, storage.join("whisper_cpp")).unwrap();
        let mut config = AppConfig::default();
        config.general.managed_runtimes.insert(
            "whisper_cpp".to_owned(),
            config::ManagedRuntimeInstall::app_managed(
                storage.join("whisper_cpp").join("bin").join("whisper-cli"),
                "test",
            ),
        );

        assert!(
            configured_managed_runtime_root_in(&config, "whisper_cpp", &storage)
                .unwrap_err()
                .to_string()
                .contains("symbolic link")
        );
        fs::remove_dir_all(root).unwrap();
    }

    fn percentile(values: &[u128], percentile: usize) -> u128 {
        let mut sorted = values.to_vec();
        sorted.sort_unstable();
        let rank = (percentile * sorted.len()).div_ceil(100).saturating_sub(1);
        sorted[rank]
    }
}
