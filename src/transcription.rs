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
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, sync_channel};
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};

use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};

use crate::config::{self, AppConfig};
use crate::models::{SttModelInfo, TranscriptResult as LegacyTranscriptResult};
use crate::prepared_audio::PreparedAudio;
use crate::runtime_router::{
    NativeBootstrapFailure, RuntimeError, RuntimeExecution, RuntimeLoadExecution, RuntimeModel,
    RuntimeRouter, WARM_MODEL_TTL, verify_compatibility_cli,
};

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
#[derive(Clone, Debug, Eq, PartialEq)]
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
#[derive(Clone, Debug, Eq, PartialEq)]
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

/// Features that the selected model/backend can currently expose.
///
/// `timestamps` means final results may include timestamp metadata; it does
/// not mean that the Phase 1 legacy bridge can enable timestamps on request.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
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

/// Runtime-neutral diagnostics from an explicit model preload.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelLoadOutcome {
    pub model_id: ModelId,
    pub resolved_acceleration: ResolvedAcceleration,
    pub model_load_duration_ms: u128,
    pub warm_model_reused: bool,
}

enum RuntimeCommand {
    Transcribe {
        model: RuntimeModel,
        preference: AccelerationPreference,
        audio: Arc<PreparedAudio>,
        options: TranscriptionOptions,
        cancellation_snapshot: u64,
        reply: SyncSender<Result<RuntimeExecution, RuntimeError>>,
    },
    Load {
        model: RuntimeModel,
        preference: AccelerationPreference,
        reply: SyncSender<Result<RuntimeLoadExecution, RuntimeError>>,
    },
    Health {
        model: RuntimeModel,
        preference: AccelerationPreference,
        reply: SyncSender<Result<(), RuntimeError>>,
    },
    Unload {
        reply: SyncSender<Result<(), RuntimeError>>,
    },
}

/// A bounded, dedicated native worker. Application-created task threads may
/// wait on this facade, but all concrete engine lifecycle and inference work
/// is serialized on the one named native worker.
#[derive(Clone)]
struct RuntimeWorker {
    commands: SyncSender<RuntimeCommand>,
}

impl RuntimeWorker {
    fn new(router: RuntimeRouter) -> Self {
        cleanup_stale_temporary_audio();
        let (commands, receiver) = sync_channel(1);
        std::thread::Builder::new()
            .name("scribe-native-runtime".to_owned())
            .spawn(move || runtime_worker_loop(router, receiver))
            .expect("Scribe could not create its native runtime worker");
        Self { commands }
    }

    fn transcribe(
        &self,
        model: RuntimeModel,
        preference: AccelerationPreference,
        audio: Arc<PreparedAudio>,
        options: TranscriptionOptions,
        cancellation_snapshot: u64,
    ) -> Result<RuntimeExecution, RuntimeError> {
        let (reply, response) = sync_channel(1);
        self.commands
            .send(RuntimeCommand::Transcribe {
                model,
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
        model: RuntimeModel,
        preference: AccelerationPreference,
    ) -> Result<RuntimeLoadExecution, RuntimeError> {
        let (reply, response) = sync_channel(1);
        self.commands
            .send(RuntimeCommand::Load {
                model,
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
        model: RuntimeModel,
        preference: AccelerationPreference,
    ) -> Result<(), RuntimeError> {
        let (reply, response) = sync_channel(1);
        self.commands
            .send(RuntimeCommand::Health {
                model,
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
        self.commands
            .send(RuntimeCommand::Unload { reply })
            .map_err(|error| RuntimeError::WorkerUnavailable(error.to_string()))?;
        response
            .recv()
            .map_err(|error| RuntimeError::WorkerUnavailable(error.to_string()))?
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
    loop {
        match commands.recv_timeout(WARM_MODEL_TTL) {
            Ok(RuntimeCommand::Transcribe {
                model,
                preference,
                audio,
                options,
                cancellation_snapshot,
                reply,
            }) => {
                let _ = reply.send(router.transcribe(
                    model,
                    preference,
                    &audio,
                    &options,
                    cancellation_snapshot,
                ));
            }
            Ok(RuntimeCommand::Load {
                model,
                preference,
                reply,
            }) => {
                let _ = reply.send(router.load(model, preference));
            }
            Ok(RuntimeCommand::Health {
                model,
                preference,
                reply,
            }) => {
                let _ = reply.send(router.health_check(model, preference));
            }
            Ok(RuntimeCommand::Unload { reply }) => {
                let _ = reply.send(router.unload_all());
            }
            Err(RecvTimeoutError::Timeout) => {
                let _ = router.unload_all();
            }
            Err(RecvTimeoutError::Disconnected) => {
                let _ = router.unload_all();
                break;
            }
        }
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

    /// Returns the conservative feature set for a configured model.
    pub fn capabilities_for(&self, model_id: &ModelId) -> Result<RuntimeCapabilities> {
        let model = self.resolve_model(model_id, None)?;
        if self.router.handles_model(model_id) {
            let runtime_model = self.resolve_runtime_model(model)?;
            return self
                .router
                .capabilities(&runtime_model)
                .ok_or_else(|| anyhow!("runtime router rejected its own selected model"));
        }
        Ok(capabilities_for_legacy_model(&model))
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
            .load(runtime_model, self.config.acceleration_preference)
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
            .health_check(runtime_model, self.config.acceleration_preference)
            .map_err(|error| anyhow!(error))
    }

    /// Requests lock-free cancellation of native work submitted before this
    /// call. Later requests capture the new generation and are unaffected.
    pub fn cancel_active(&self) {
        self.router.cancel_active();
    }

    /// Drops all retained native model state on the dedicated worker.
    pub fn unload_runtime(&self) -> Result<()> {
        self.worker.unload().map_err(|error| anyhow!(error))
    }

    /// Transcribes canonical prepared audio. The router receives the first
    /// opportunity to handle every model; unretired providers remain behind a
    /// private compatibility bridge until Phase 11 retirement evidence exists.
    pub fn transcribe(&self, request: TranscriptionRequest) -> Result<TranscriptionOutcome> {
        let model = self.resolve_model(&request.model_id, request.model_path.clone())?;
        if self.router.handles_model(&request.model_id) {
            return self.transcribe_primary(request, model);
        }

        self.transcribe_legacy(request, model)
    }

    fn transcribe_primary(
        &self,
        request: TranscriptionRequest,
        model: SttModelInfo,
    ) -> Result<TranscriptionOutcome> {
        validate_default_options(&request.options)?;
        let runtime_model = self.resolve_runtime_model(model.clone())?;
        match self.worker.transcribe(
            runtime_model,
            self.config.acceleration_preference,
            Arc::clone(&request.audio),
            request.options.clone(),
            self.router.cancellation_snapshot(),
        ) {
            Ok(execution) => Ok(map_native_execution(request, model, execution)),
            Err(crate::runtime_router::RuntimeError::Bootstrap(failure))
                if failure.cli_fallback_eligible() =>
            {
                self.transcribe_legacy_with_fallback_reason(request, model, failure)
            }
            Err(error) => Err(anyhow!(error)),
        }
    }

    fn transcribe_legacy(
        &self,
        request: TranscriptionRequest,
        model: SttModelInfo,
    ) -> Result<TranscriptionOutcome> {
        self.transcribe_legacy_inner(request, model, None)
    }

    fn transcribe_legacy_with_fallback_reason(
        &self,
        request: TranscriptionRequest,
        model: SttModelInfo,
        failure: NativeBootstrapFailure,
    ) -> Result<TranscriptionOutcome> {
        self.transcribe_legacy_inner(request, model, Some(failure.to_string()))
    }

    fn transcribe_legacy_inner(
        &self,
        request: TranscriptionRequest,
        model: SttModelInfo,
        fallback_reason: Option<String>,
    ) -> Result<TranscriptionOutcome> {
        if fallback_reason.is_some() {
            let cli = crate::stt::whisper_cpp::resolve_whisper_cpp_executable(&self.config)
                .ok_or_else(|| anyhow!("the verified compatibility CLI is unavailable"))?;
            verify_compatibility_cli(&cli).map_err(|error| anyhow!(error))?;
        }
        let mut engine = LegacyBatchAdapter::new(self.config.clone(), model);
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
        let package_root = primary_runtime_package_root(&self.config).ok_or_else(|| {
            anyhow!(
                "the verified native runtime package is not installed; install it from Models or configure the compatibility CLI"
            )
        })?;
        let artifact = crate::runtime_catalog::model_artifact_spec(&model.id)
            .filter(|artifact| artifact.download_bytes.is_some() && artifact.sha256.is_some())
            .ok_or_else(|| {
                anyhow!(
                    "model {} has no pinned size and SHA-256 evidence for in-process native loading",
                    model.name
                )
            })?;
        Ok(RuntimeModel {
            id: model.id.into(),
            path,
            package_root,
            expected_size_bytes: artifact.download_bytes.expect("checked above"),
            expected_sha256: artifact.sha256.expect("checked above"),
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

fn primary_runtime_package_root(config: &AppConfig) -> Option<PathBuf> {
    let entrypoint = crate::stt::whisper_cpp::resolve_whisper_cpp_executable(config)?;
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
    diagnostics: Option<LegacyDiagnostics>,
}

impl LegacyBatchAdapter {
    fn new(config: AppConfig, model: SttModelInfo) -> Self {
        Self {
            config,
            model,
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
        Err(anyhow!(
            "cancellation is not supported by the Phase 1 legacy transcription path"
        ))
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
        ..RuntimeCapabilities::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::TranscriptSegment as LegacyTranscriptSegment;

    fn prepared_audio() -> Arc<PreparedAudio> {
        Arc::new(PreparedAudio {
            samples: vec![0.0; 160],
            sample_rate: 16_000,
            source_sample_rate: 16_000,
            source_channels: 1,
            source_frames: 160,
        })
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
        let mut adapter = LegacyBatchAdapter::new(AppConfig::default(), model);

        let error = adapter.health_check().unwrap_err();

        assert!(error.to_string().contains("not implemented"));
    }

    #[test]
    fn legacy_adapter_has_explicit_stateless_load_and_unsupported_cancel_semantics() {
        let model = config::configured_models(&AppConfig::default())
            .into_iter()
            .find(|model| model.id == "whisper_cpp_tiny_en")
            .expect("whisper.cpp tiny model exists");
        let mut adapter = LegacyBatchAdapter::new(AppConfig::default(), model);

        adapter
            .load()
            .expect("legacy adapter has no persistent load");
        let error = adapter.cancel().unwrap_err();
        adapter
            .unload()
            .expect("legacy adapter has no persistent unload");

        assert!(error.to_string().contains("cancellation is not supported"));
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

        let config = AppConfig {
            whisper_executable_path: Some(whisper_cli),
            ..AppConfig::default()
        };
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
        assert_eq!(outcome.model_name, "whisper.cpp base.en");
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
        let config = AppConfig {
            whisper_executable_path: Some(cli),
            acceleration_preference: AccelerationPreference::Cpu,
            ..AppConfig::default()
        };
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
        let service = TranscriptionService::new(AppConfig {
            whisper_executable_path: Some(cli),
            acceleration_preference: AccelerationPreference::Cpu,
            ..AppConfig::default()
        });
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

    fn percentile(values: &[u128], percentile: usize) -> u128 {
        let mut sorted = values.to_vec();
        sorted.sort_unstable();
        let rank = (percentile * sorted.len()).div_ceil(100).saturating_sub(1);
        sorted[rank]
    }
}
