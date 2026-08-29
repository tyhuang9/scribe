//! Safe `transcribe-cpp` adapter.
//!
//! `worker-only native runtime`: production construction occurs exclusively
//! inside the private inference child. In-process use is limited to focused
//! unit tests that exercise the adapter without launching the application.
//!
//! This module is deliberately below the application-facing transcription
//! boundary. It owns the safe crate's model/session lifecycle and converts its
//! owned output into Scribe's neutral transcript contract. No caller above
//! `RuntimeRouter` can observe a model family, FFI handle, or native backend.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Result, anyhow};
use thiserror::Error;
use transcribe_cpp::{
    Backend, CancelToken, Device, DeviceType, Error as NativeError, Feature, Model, ModelOptions,
    RunOptions, Session, Task, TimestampKind,
};

use crate::backend_policy::{
    BackendCandidate, BackendEnvironmentFingerprint, BackendKind, BackendQualificationPolicy,
    BackendSelection, BackendSelectionError, BackendSnapshot, BackendTarget, CandidateAvailability,
    DeviceClass, DeviceIdentity, GpuVendor, OperatingSystem, PowerSource, ProviderIdentity,
    select_backend,
};
use crate::prepared_audio::{PREPARED_SAMPLE_RATE, PreparedAudio};
use crate::transcription::{
    AccelerationPreference, ComputeDevice, ResolvedAcceleration, RuntimeCapabilities, SpeechEngine,
    Transcript, TranscriptSegment, TranscriptionOptions,
};

/// Exact safe-wrapper/native release compiled into this Scribe build.
#[allow(
    dead_code,
    reason = "the test-only embedded-runtime module retains the pinned version contract"
)]
pub(crate) const TRANSCRIBE_CPP_VERSION: &str = "0.1.3";

static BACKENDS: OnceLock<std::result::Result<(), String>> = OnceLock::new();

/// Stable categories translated from the safe wrapper's native errors.
#[derive(Debug, Error)]
pub(crate) enum EmbeddedRuntimeError {
    #[error("RuntimeInitializationFailed: {0}")]
    RuntimeInitializationFailed(String),
    #[error("ModelNotInstalled: {0}")]
    ModelNotInstalled(PathBuf),
    #[error("ModelIncompatible: {0}")]
    ModelIncompatible(String),
    #[error("ModelRequiresNewerRuntime: {0}")]
    ModelRequiresNewerRuntime(String),
    #[error("DecodeFailed: {0}")]
    DecodeFailed(String),
    #[error("Cancelled")]
    Cancelled,
    #[error("OutOfMemory: {0}")]
    OutOfMemory(String),
    #[error("BackendUnavailable: {0}")]
    BackendUnavailable(String),
    #[error("UnsupportedOperation: {0}")]
    UnsupportedOperation(String),
}

/// A retained safe-wrapper model plus its serialized native session.
///
/// `Session` is `Send` and all mutation happens through `&mut self`; Scribe's
/// existing bounded runtime worker remains responsible for serializing calls
/// and preventing unload/model switches during an active dictation.
pub(crate) struct EmbeddedRuntime {
    model_path: PathBuf,
    preference: AccelerationPreference,
    model: Option<Model>,
    session: Option<Session>,
    cancellation: CancelToken,
    capabilities: RuntimeCapabilities,
    detected_architecture: Option<String>,
    resolved_acceleration: Option<ResolvedAcceleration>,
    backend_environment: Option<BackendEnvironmentFingerprint>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct RuntimeBackendEnvironment {
    selection: BackendSelection,
    fingerprint: BackendEnvironmentFingerprint,
}

impl EmbeddedRuntime {
    pub(crate) fn new(model_path: PathBuf, preference: AccelerationPreference) -> Self {
        Self {
            model_path,
            preference,
            model: None,
            session: None,
            cancellation: CancelToken::new(),
            capabilities: RuntimeCapabilities::default(),
            detected_architecture: None,
            resolved_acceleration: None,
            backend_environment: None,
        }
    }

    pub(crate) fn resolved_acceleration(&self) -> Option<&ResolvedAcceleration> {
        self.resolved_acceleration.as_ref()
    }

    pub(crate) fn model_path(&self) -> &Path {
        &self.model_path
    }

    pub(crate) fn preference(&self) -> AccelerationPreference {
        self.preference
    }

    pub(crate) fn is_loaded(&self) -> bool {
        self.session.is_some()
            && self.backend_environment.as_ref().is_some_and(|loaded| {
                current_runtime_backend_environment(self.preference)
                    .is_ok_and(|current| backend_environment_matches(loaded, &current.fingerprint))
            })
    }

    /// Returns the `general.architecture` string supplied by the loaded GGUF.
    pub(crate) fn detected_architecture(&self) -> Option<String> {
        self.detected_architecture.clone()
    }

    pub(crate) fn cancellation_handle(&self) -> CancelToken {
        self.cancellation.clone()
    }

    pub(crate) fn transcribe_with_cancellation(
        &mut self,
        audio: &PreparedAudio,
        options: &TranscriptionOptions,
        cancellation_generation: &AtomicU64,
        cancellation_snapshot: u64,
    ) -> Result<Transcript> {
        validate_audio(audio)?;
        validate_options(options)?;
        self.load_model()?;
        let session = self
            .session
            .as_mut()
            .expect("load_model must retain a session");
        let native = run_if_not_cancelled(
            &self.cancellation,
            cancellation_generation,
            cancellation_snapshot,
            || {
                session
                    .run(&audio.samples, &run_options(options))
                    .map_err(map_native_error)
            },
        )?;
        Ok(normalize_transcript(native, audio))
    }

    fn ensure_backends() -> Result<()> {
        match BACKENDS.get_or_init(|| {
            transcribe_cpp::init_backends_default().map_err(|error| error.to_string())
        }) {
            Ok(()) => Ok(()),
            Err(message) => Err(anyhow!(EmbeddedRuntimeError::RuntimeInitializationFailed(
                message.clone()
            ))),
        }
    }

    fn load_model(&mut self) -> Result<()> {
        Self::ensure_backends()?;
        if !self.model_path.is_file() {
            return Err(anyhow!(EmbeddedRuntimeError::ModelNotInstalled(
                self.model_path.clone()
            )));
        }

        let environment = match current_runtime_backend_environment(self.preference) {
            Ok(environment) => environment,
            Err(error) => {
                self.clear_loaded_state();
                return Err(error);
            }
        };
        if self.session.is_some()
            && self
                .backend_environment
                .as_ref()
                .is_some_and(|loaded| backend_environment_matches(loaded, &environment.fingerprint))
        {
            return Ok(());
        }
        self.clear_loaded_state();

        let RuntimeBackendEnvironment {
            mut selection,
            fingerprint,
        } = environment;
        let gpu_device = selected_process_index(&selection)?;

        let model = Model::load_with(
            &self.model_path,
            &ModelOptions {
                backend: requested_backend(selection.target.backend),
                gpu_device,
            },
        )
        .map_err(map_native_error)?;
        let detected_architecture = model.arch();
        let native_capabilities = model.capabilities();
        let resolved_backend = model.backend();
        let resolved_device = model.device().map_err(map_native_error)?;
        reconcile_observed_target(&mut selection, &resolved_backend, &resolved_device)?;
        let resolved_acceleration = resolved_acceleration(
            self.preference,
            &resolved_backend,
            &resolved_device,
            Some(selection),
        );
        let capabilities = RuntimeCapabilities {
            streaming: native_capabilities.supports_streaming,
            cancellation: model.supports(Feature::Cancellation),
            translation: native_capabilities.supports_translate,
            timestamps: native_capabilities.max_timestamp_kind != TimestampKind::None,
            language_detection: native_capabilities.supports_language_detect,
            confidence_scores: false,
            custom_vocabulary: false,
            supported_languages: native_capabilities.languages,
        };
        let mut session = model.session().map_err(map_native_error)?;
        session.set_cancel_token(&self.cancellation);

        self.capabilities = capabilities;
        self.detected_architecture = Some(detected_architecture);
        self.resolved_acceleration = Some(resolved_acceleration);
        self.backend_environment = Some(fingerprint);
        self.model = Some(model);
        self.session = Some(session);
        Ok(())
    }

    fn clear_loaded_state(&mut self) {
        // Dropping the session first guarantees that the safe wrapper retains
        // the model only until no native session can still reference it.
        self.session = None;
        self.model = None;
        self.cancellation.reset();
        self.capabilities = RuntimeCapabilities::default();
        self.detected_architecture = None;
        self.resolved_acceleration = None;
        self.backend_environment = None;
    }
}

impl SpeechEngine for EmbeddedRuntime {
    fn load(&mut self) -> Result<()> {
        self.load_model()
    }

    fn capabilities(&self) -> RuntimeCapabilities {
        self.capabilities.clone()
    }

    fn unload(&mut self) -> Result<()> {
        self.clear_loaded_state();
        Ok(())
    }
}

fn run_if_not_cancelled<T>(
    cancellation: &CancelToken,
    cancellation_generation: &AtomicU64,
    cancellation_snapshot: u64,
    inference: impl FnOnce() -> Result<T>,
) -> Result<T> {
    cancellation.reset();
    if cancellation_generation.load(Ordering::Acquire) != cancellation_snapshot {
        return Err(anyhow!(EmbeddedRuntimeError::Cancelled));
    }
    inference()
}

fn validate_options(options: &TranscriptionOptions) -> Result<()> {
    if options.initial_prompt.is_some() {
        return Err(anyhow!(EmbeddedRuntimeError::UnsupportedOperation(
            "initial prompts require a model-specific run extension and are not enabled for the generic GGUF adapter"
                .to_owned()
        )));
    }
    Ok(())
}

fn current_runtime_backend_environment(
    preference: AccelerationPreference,
) -> Result<RuntimeBackendEnvironment> {
    let snapshot = runtime_backend_snapshot();
    select_backend_environment(preference, &snapshot)
}

fn runtime_backend_snapshot() -> BackendSnapshot {
    let mut candidates = transcribe_cpp::devices()
        .into_iter()
        .filter_map(native_backend_candidate)
        .collect::<Vec<_>>();
    mark_ambiguous_derived_devices(&mut candidates);
    apply_verified_pack_device_override(&mut candidates);
    if !candidates
        .iter()
        .any(|candidate| candidate.target.backend == BackendKind::Cpu)
        && transcribe_cpp::backend_available(Backend::Cpu)
    {
        candidates.push(BackendCandidate::available(BackendTarget::cpu()));
    }
    BackendSnapshot {
        operating_system: OperatingSystem::current(),
        power_source: PowerSource::current(),
        candidates,
        // Stage 1 intentionally keeps all production GPU providers opt-in.
        // Versioned qualification entries arrive only with verified packs and
        // release qualification in later stages.
        qualification_policy: BackendQualificationPolicy::stage_one_default_deny(),
    }
}

#[cfg(feature = "inference-worker")]
fn apply_verified_pack_device_override(candidates: &mut [BackendCandidate]) {
    let stable_id = std::env::var(crate::onnx_worker::PACK_DEVICE_ID_ENV).ok();
    let driver = std::env::var(crate::onnx_worker::PACK_DRIVER_ID_ENV).ok();
    let provider = std::env::var(crate::onnx_worker::PACK_PROVIDER_ENV).ok();
    apply_verified_pack_device_override_values(
        candidates,
        stable_id.as_deref(),
        driver.as_deref(),
        provider.as_deref(),
    );
}

fn apply_verified_pack_device_override_values(
    candidates: &mut [BackendCandidate],
    stable_id: Option<&str>,
    driver: Option<&str>,
    provider: Option<&str>,
) {
    let Some(stable_id) = stable_id.filter(|value| {
        !value.is_empty()
            && value.len() <= 256
            && *value == value.to_ascii_lowercase()
            && value.bytes().all(|byte| (0x20..=0x7e).contains(&byte))
    }) else {
        return;
    };
    let driver = driver
        .filter(|value| {
            !value.is_empty()
                && value.len() <= 128
                && value.bytes().all(|byte| (0x20..=0x7e).contains(&byte))
        })
        .map(str::to_owned);
    let provider = provider
        .filter(|value| {
            !value.is_empty()
                && value.len() <= 128
                && *value == value.to_ascii_lowercase()
                && value.bytes().all(|byte| (0x20..=0x7e).contains(&byte))
        })
        .map(ProviderIdentity::new);
    for candidate in candidates.iter_mut().filter(|candidate| {
        candidate.target.backend.is_gpu() && candidate.target.device_id.as_str() != stable_id
    }) {
        candidate.availability = CandidateAvailability::Incompatible;
    }
    if let Some(candidate) = candidates.iter_mut().find(|candidate| {
        candidate.target.backend.is_gpu() && candidate.target.device_id.as_str() == stable_id
    }) {
        candidate.target.driver_version = driver;
        if let Some(provider) = provider {
            candidate.target.provider_id = provider;
        }
    }
}

#[cfg(not(feature = "inference-worker"))]
fn apply_verified_pack_device_override(_candidates: &mut [BackendCandidate]) {}

fn select_backend_environment(
    preference: AccelerationPreference,
    snapshot: &BackendSnapshot,
) -> Result<RuntimeBackendEnvironment> {
    let selection = select_backend(preference, snapshot)
        .map_err(|error| anyhow!(map_selection_error(error)))?;
    Ok(RuntimeBackendEnvironment {
        selection,
        fingerprint: snapshot.environment_fingerprint(),
    })
}

fn backend_environment_matches(
    loaded: &BackendEnvironmentFingerprint,
    current: &BackendEnvironmentFingerprint,
) -> bool {
    loaded == current
}

fn mark_ambiguous_derived_devices(candidates: &mut [BackendCandidate]) {
    for index in 0..candidates.len() {
        if !candidates[index].target.device_id.is_derived() {
            continue;
        }
        let duplicate_count = candidates
            .iter()
            .filter(|candidate| {
                candidate.target.backend == candidates[index].target.backend
                    && candidate.target.provider_id == candidates[index].target.provider_id
                    && candidate.target.device_id == candidates[index].target.device_id
            })
            .count();
        if duplicate_count > 1 {
            candidates[index].availability = CandidateAvailability::Unaddressable;
        }
    }
}

fn map_selection_error(error: BackendSelectionError) -> EmbeddedRuntimeError {
    EmbeddedRuntimeError::BackendUnavailable(error.to_string())
}

fn native_backend_candidate(device: Device) -> Option<BackendCandidate> {
    let backend = backend_kind(&device.kind, device.device_type)?;
    native_backend_candidate_for_backend(device, backend)
}

fn native_backend_candidate_for_backend(
    device: Device,
    backend: BackendKind,
) -> Option<BackendCandidate> {
    let provider_id = ProviderIdentity::new(format!(
        "transcribe-cpp:{}",
        backend.label().to_ascii_lowercase()
    ));
    let vendor = match backend {
        BackendKind::Cuda => GpuVendor::Nvidia,
        BackendKind::Metal => GpuVendor::Apple,
        BackendKind::Vulkan => infer_gpu_vendor(&device),
        BackendKind::Cpu => GpuVendor::Unknown,
    };
    let device_class = match device.device_type {
        DeviceType::Cpu => DeviceClass::Cpu,
        DeviceType::Accel => DeviceClass::Accelerator,
        DeviceType::Gpu if backend == BackendKind::Metal => DeviceClass::UnifiedGpu,
        DeviceType::Gpu => DeviceClass::DiscreteGpu,
        DeviceType::Igpu if backend == BackendKind::Metal => DeviceClass::UnifiedGpu,
        DeviceType::Igpu => DeviceClass::IntegratedGpu,
        DeviceType::Unknown if backend == BackendKind::Cpu => DeviceClass::Cpu,
        DeviceType::Unknown => DeviceClass::Unknown,
    };
    let display_name = [&device.description, &device.name]
        .into_iter()
        .map(|value| value.trim())
        .find(|value| !value.is_empty())
        .unwrap_or_else(|| backend.label())
        .to_owned();
    let device_id = if backend == BackendKind::Cpu {
        DeviceIdentity::new("cpu:system")
    } else {
        device
            .device_id
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| DeviceIdentity::new(format!("native:{}", value.to_ascii_lowercase())))
            .unwrap_or_else(|| {
                DeviceIdentity::new(format!(
                    "derived:{}:{}:{vendor:?}:{device_class:?}:{}:{}:{}",
                    provider_id.as_str(),
                    backend.label().to_ascii_lowercase(),
                    device.memory_total,
                    normalized_identity_component(&device.name),
                    normalized_identity_component(&device.description)
                ))
            })
    };
    Some(BackendCandidate {
        target: BackendTarget {
            backend,
            provider_id,
            driver_version: None,
            device_id,
            display_name,
            vendor,
            device_class,
            memory_total_bytes: device.memory_total,
            memory_available_bytes: device.memory_free,
            pack: None,
            process_index: device.index,
        },
        availability: CandidateAvailability::Available,
    })
}

fn backend_kind(kind: &str, device_type: DeviceType) -> Option<BackendKind> {
    match kind.trim().to_ascii_lowercase().as_str() {
        "cuda" => Some(BackendKind::Cuda),
        "vulkan" => Some(BackendKind::Vulkan),
        "metal" => Some(BackendKind::Metal),
        "cpu" | "cpu_accel" | "accel" => Some(BackendKind::Cpu),
        _ if matches!(device_type, DeviceType::Cpu | DeviceType::Accel) => Some(BackendKind::Cpu),
        _ => None,
    }
}

fn observed_backend_kind(backend: &str, device: &Device) -> Option<BackendKind> {
    let normalized = backend.trim().to_ascii_lowercase();
    if normalized.contains("cuda") {
        Some(BackendKind::Cuda)
    } else if normalized.contains("vulkan") {
        Some(BackendKind::Vulkan)
    } else if normalized.contains("metal") {
        Some(BackendKind::Metal)
    } else if matches!(normalized.as_str(), "cpu" | "cpu_accel") {
        Some(BackendKind::Cpu)
    } else {
        backend_kind(&device.kind, device.device_type)
    }
}

fn infer_gpu_vendor(device: &Device) -> GpuVendor {
    let description = format!(
        "{} {} {}",
        device.description,
        device.name,
        device.device_id.as_deref().unwrap_or_default()
    )
    .to_ascii_lowercase();
    if description.contains("nvidia")
        || description.contains("geforce")
        || description.contains("quadro")
    {
        GpuVendor::Nvidia
    } else if description.contains("advanced micro devices")
        || description.contains("amd")
        || description.contains("radeon")
    {
        GpuVendor::Amd
    } else if description.contains("intel") {
        GpuVendor::Intel
    } else if description.contains("apple") {
        GpuVendor::Apple
    } else if description.trim().is_empty() {
        GpuVendor::Unknown
    } else {
        GpuVendor::Other
    }
}

fn normalized_identity_component(value: &str) -> String {
    let normalized = value
        .trim()
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | ':' | '.') {
                character.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>();
    normalized.trim_matches('-').to_owned()
}

fn requested_backend(backend: BackendKind) -> Backend {
    match backend {
        BackendKind::Cpu => Backend::Cpu,
        BackendKind::Cuda => Backend::Cuda,
        BackendKind::Vulkan => Backend::Vulkan,
        BackendKind::Metal => Backend::Metal,
    }
}

fn selected_process_index(selection: &BackendSelection) -> Result<i32> {
    if !selection.target.backend.is_gpu() {
        return Ok(0);
    }
    let index = selection.target.process_index.ok_or_else(|| {
        anyhow!(EmbeddedRuntimeError::BackendUnavailable(format!(
            "selected {} device {} has no current process index",
            selection.target.backend.label(),
            selection.target.device_id.as_str()
        )))
    })?;
    i32::try_from(index).map_err(|_| {
        anyhow!(EmbeddedRuntimeError::BackendUnavailable(format!(
            "selected {} device index is out of range",
            selection.target.backend.label()
        )))
    })
}

fn reconcile_observed_target(
    selection: &mut BackendSelection,
    backend: &str,
    device: &Device,
) -> Result<()> {
    let observed_backend = observed_backend_kind(backend, device).ok_or_else(|| {
        anyhow!(EmbeddedRuntimeError::BackendUnavailable(format!(
            "native runtime reported an unknown backend {backend:?}"
        )))
    })?;
    if observed_backend != selection.target.backend {
        return Err(anyhow!(EmbeddedRuntimeError::BackendUnavailable(format!(
            "requested {}, but the native runtime resolved {}",
            selection.target.backend.label(),
            observed_backend.label()
        ))));
    }
    let process_index = selection.target.process_index;
    let observed = native_backend_candidate_for_backend(device.clone(), observed_backend)
        .map(|candidate| candidate.target)
        .ok_or_else(|| {
            anyhow!(EmbeddedRuntimeError::BackendUnavailable(
                "native runtime did not report a selectable compute device".to_owned()
            ))
        })?;
    let driver_matches = observed.driver_version.is_none()
        || observed.driver_version == selection.target.driver_version;
    if observed.backend != selection.target.backend
        || observed.backend.is_gpu() != selection.target.backend.is_gpu()
        || observed.device_class != selection.target.device_class
        || observed.provider_id != selection.target.provider_id
        || !driver_matches
        || observed.vendor != selection.target.vendor
        || observed.device_id != selection.target.device_id
    {
        return Err(anyhow!(EmbeddedRuntimeError::BackendUnavailable(format!(
            "selected {} device {} ({:?}, {}, {:?}), but the native runtime resolved {} device {} ({:?}, {}, {:?})",
            selection.target.backend.label(),
            selection.target.device_id.as_str(),
            selection.target.device_class,
            selection.target.provider_id.as_str(),
            selection.target.vendor,
            observed.backend.label(),
            observed.device_id.as_str(),
            observed.device_class,
            observed.provider_id.as_str(),
            observed.vendor
        ))));
    }
    let pack = selection.target.pack.clone();
    let driver_version = selection.target.driver_version.clone();
    selection.target = BackendTarget {
        pack,
        driver_version,
        process_index,
        ..observed
    };
    Ok(())
}

fn resolved_acceleration(
    requested: AccelerationPreference,
    backend: &str,
    device: &Device,
    selection: Option<BackendSelection>,
) -> ResolvedAcceleration {
    let resolved = match device.device_type {
        DeviceType::Cpu | DeviceType::Accel => ComputeDevice::Cpu,
        DeviceType::Gpu | DeviceType::Igpu => ComputeDevice::Gpu {
            name: resolved_gpu_name(backend, device),
        },
        DeviceType::Unknown => {
            let backend = backend.trim();
            if matches!(backend.to_ascii_lowercase().as_str(), "cpu" | "cpu_accel") {
                ComputeDevice::Cpu
            } else {
                ComputeDevice::Gpu {
                    name: resolved_gpu_name(backend, device),
                }
            }
        }
    };
    let typed_auto_diagnostic = selection
        .as_ref()
        .and_then(BackendSelection::auto_cpu_diagnostic);
    let diagnostic = match (&requested, &resolved) {
        (AccelerationPreference::Auto, ComputeDevice::Cpu) => typed_auto_diagnostic
            .or_else(|| Some("No compatible GPU was available; Auto fell back to CPU.".to_owned())),
        (AccelerationPreference::Gpu, ComputeDevice::Cpu) => {
            Some("The strict GPU request unexpectedly resolved to CPU.".to_owned())
        }
        _ => None,
    };
    ResolvedAcceleration {
        requested,
        resolved,
        diagnostic,
        selection,
    }
}

fn resolved_gpu_name(backend: &str, device: &Device) -> String {
    [&device.description, &device.name]
        .into_iter()
        .map(|value| value.trim())
        .find(|value| !value.is_empty())
        .unwrap_or_else(|| backend.trim())
        .to_owned()
}

fn validate_audio(audio: &PreparedAudio) -> Result<()> {
    if audio.sample_rate != PREPARED_SAMPLE_RATE
        || audio.samples.is_empty()
        || audio
            .samples
            .iter()
            .any(|sample| !sample.is_finite() || !(-1.0..=1.0).contains(sample))
    {
        return Err(anyhow!(EmbeddedRuntimeError::DecodeFailed(
            "expected non-empty canonical mono 16 kHz finite f32 audio".to_owned()
        )));
    }
    Ok(())
}

fn run_options(options: &TranscriptionOptions) -> RunOptions {
    RunOptions {
        task: if options.translate_to_english {
            Task::Translate
        } else {
            Task::Transcribe
        },
        timestamps: if options.enable_timestamps {
            TimestampKind::Segment
        } else {
            TimestampKind::None
        },
        language: options.language.clone(),
        ..RunOptions::default()
    }
}

fn normalize_transcript(native: transcribe_cpp::Transcript, audio: &PreparedAudio) -> Transcript {
    Transcript {
        text: native.text,
        segments: native
            .segments
            .into_iter()
            .map(|segment| TranscriptSegment {
                text: segment.text,
                start_ms: u64::try_from(segment.t0_ms).ok(),
                end_ms: u64::try_from(segment.t1_ms).ok(),
                confidence: None,
            })
            .collect(),
        detected_language: native.language,
        duration_ms: Some(
            (audio.samples.len() as u128).saturating_mul(1_000) / u128::from(PREPARED_SAMPLE_RATE),
        ),
    }
}

fn map_native_error(error: NativeError) -> anyhow::Error {
    use EmbeddedRuntimeError as Error;

    let mapped = match error {
        NativeError::ModelFileNotFound(message) => Error::ModelNotInstalled(PathBuf::from(message)),
        NativeError::ModelLoad(message) => Error::ModelIncompatible(message),
        NativeError::VersionMismatch(message) | NativeError::BadStructSize(message) => {
            Error::ModelRequiresNewerRuntime(message)
        }
        NativeError::OutOfMemory(message) => Error::OutOfMemory(message),
        NativeError::Backend(message) | NativeError::Busy(message) => {
            Error::BackendUnavailable(message)
        }
        NativeError::Unsupported(message) | NativeError::NotImplemented(message) => {
            Error::UnsupportedOperation(message)
        }
        NativeError::Aborted { .. } => Error::Cancelled,
        NativeError::OutputTruncated { message, .. }
        | NativeError::InputTooLong(message)
        | NativeError::InvalidArgument(message)
        | NativeError::Other(message) => Error::DecodeFailed(message),
        NativeError::Nul(message) => Error::DecodeFailed(message.to_string()),
        other => Error::DecodeFailed(other.to_string()),
    };
    anyhow!(mapped)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transcription::PreviewDecodeOptions;

    #[test]
    fn queued_request_cancelled_before_router_transcribe_never_starts_native_inference() {
        let generation = std::sync::Arc::new(AtomicU64::new(7));
        let cancellation = CancelToken::new();
        let cancellation_handle = cancellation.clone();
        let snapshot = generation.load(Ordering::Acquire);
        let inference_started = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let worker_generation = std::sync::Arc::clone(&generation);
        let worker_inference_started = std::sync::Arc::clone(&inference_started);
        let (dequeued, request_dequeued) = std::sync::mpsc::sync_channel(0);
        let (resume, request_resumed) = std::sync::mpsc::sync_channel(0);
        let worker = std::thread::spawn(move || {
            dequeued.send(()).unwrap();
            request_resumed.recv().unwrap();
            run_if_not_cancelled(&cancellation, &worker_generation, snapshot, || {
                worker_inference_started.store(true, Ordering::Release);
                Ok(())
            })
        });

        request_dequeued.recv().unwrap();
        generation.fetch_add(1, Ordering::AcqRel);
        cancellation_handle.cancel();
        resume.send(()).unwrap();

        let error = worker.join().unwrap().unwrap_err();
        assert!(matches!(
            error.downcast_ref::<EmbeddedRuntimeError>(),
            Some(EmbeddedRuntimeError::Cancelled)
        ));
        assert!(!inference_started.load(Ordering::Acquire));
    }

    #[test]
    fn native_backend_initialization_uses_the_safe_wrapper() {
        EmbeddedRuntime::ensure_backends().unwrap();
    }

    #[test]
    fn missing_model_is_rejected_before_native_load() {
        let missing = std::env::temp_dir().join(format!(
            "scribe-missing-transcribe-cpp-model-{}",
            std::process::id()
        ));
        let mut runtime = EmbeddedRuntime::new(missing.clone(), AccelerationPreference::Cpu);

        let error = runtime.load().unwrap_err().to_string();

        assert!(error.starts_with("ModelNotInstalled:"));
        assert!(error.contains(&missing.display().to_string()));
    }

    #[test]
    fn generic_adapter_refuses_an_unimplemented_initial_prompt() {
        let options = TranscriptionOptions {
            initial_prompt: Some("important terms".to_owned()),
            ..TranscriptionOptions::default()
        };

        let error = validate_options(&options).unwrap_err().to_string();

        assert!(error.starts_with("UnsupportedOperation:"));
        assert!(error.contains("initial prompts"));
    }

    #[test]
    fn runtime_options_preserve_neutral_translation_and_timestamps() {
        let options = TranscriptionOptions {
            language: Some("en".to_owned()),
            translate_to_english: true,
            enable_timestamps: true,
            initial_prompt: None,
        };
        let native = run_options(&options);

        assert_eq!(native.task, Task::Translate);
        assert_eq!(native.timestamps, TimestampKind::Segment);
        assert_eq!(native.language.as_deref(), Some("en"));
    }

    #[test]
    fn capable_preview_policy_requests_native_segment_timestamps() {
        let preview = PreviewDecodeOptions::for_capabilities(&RuntimeCapabilities {
            timestamps: true,
            ..RuntimeCapabilities::default()
        });

        let native = run_options(&preview.transcription_options());

        assert_eq!(native.timestamps, TimestampKind::Segment);
    }

    #[test]
    fn acceleration_preferences_preserve_native_fallback_and_strictness() {
        assert_eq!(requested_backend(BackendKind::Cpu), Backend::Cpu);
        assert_eq!(requested_backend(BackendKind::Cuda), Backend::Cuda);
        assert_eq!(requested_backend(BackendKind::Vulkan), Backend::Vulkan);
        assert_eq!(requested_backend(BackendKind::Metal), Backend::Metal);
    }

    #[test]
    fn resolved_device_type_drives_truthful_acceleration_reporting() {
        let cpu = resolved_acceleration(
            AccelerationPreference::Auto,
            "cpu",
            &device(DeviceType::Cpu, "CPU", ""),
            None,
        );
        assert_eq!(cpu.resolved, ComputeDevice::Cpu);
        assert_eq!(
            cpu.diagnostic.as_deref(),
            Some("No compatible GPU was available; Auto fell back to CPU.")
        );

        let gpu = resolved_acceleration(
            AccelerationPreference::Gpu,
            "Vulkan0",
            &device(DeviceType::Gpu, "Vulkan0", "NVIDIA GeForce RTX test device"),
            None,
        );
        assert_eq!(
            gpu.resolved,
            ComputeDevice::Gpu {
                name: "NVIDIA GeForce RTX test device".to_owned()
            }
        );
        assert_eq!(gpu.diagnostic, None);
    }

    #[test]
    fn host_memory_accelerators_are_reported_as_cpu_compute() {
        let resolved = resolved_acceleration(
            AccelerationPreference::Cpu,
            "cpu_accel",
            &device(DeviceType::Accel, "AMX", "Host accelerator"),
            None,
        );

        assert_eq!(resolved.resolved, ComputeDevice::Cpu);
        assert_eq!(resolved.diagnostic, None);
    }

    fn device(device_type: DeviceType, name: &str, description: &str) -> Device {
        Device {
            name: name.to_owned(),
            description: description.to_owned(),
            kind: match device_type {
                DeviceType::Cpu => "cpu",
                DeviceType::Accel => "accel",
                DeviceType::Gpu | DeviceType::Igpu | DeviceType::Unknown => "vulkan",
            }
            .to_owned(),
            device_type,
            device_id: None,
            memory_total: 0,
            memory_free: 0,
            index: None,
        }
    }

    fn snapshot(
        operating_system: OperatingSystem,
        power_source: PowerSource,
        candidates: Vec<BackendCandidate>,
    ) -> BackendSnapshot {
        BackendSnapshot {
            operating_system,
            power_source,
            candidates,
            qualification_policy: BackendQualificationPolicy::stage_one_default_deny(),
        }
    }

    fn qualified_snapshot(
        operating_system: OperatingSystem,
        power_source: PowerSource,
        candidates: Vec<BackendCandidate>,
    ) -> BackendSnapshot {
        let qualification_policy =
            BackendQualificationPolicy::qualify_all_for_testing(operating_system, &candidates);
        BackendSnapshot {
            operating_system,
            power_source,
            candidates,
            qualification_policy,
        }
    }

    #[test]
    fn native_device_candidates_keep_stable_identity_separate_from_process_index() {
        let native = Device {
            name: "Vulkan0".to_owned(),
            description: "NVIDIA GeForce RTX test device".to_owned(),
            kind: "vulkan".to_owned(),
            device_type: DeviceType::Gpu,
            device_id: Some("PCI:0000:01:00.0".to_owned()),
            memory_total: 8 * 1024 * 1024 * 1024,
            memory_free: 6 * 1024 * 1024 * 1024,
            index: Some(3),
        };

        let candidate = native_backend_candidate(native).unwrap();

        assert_eq!(candidate.target.backend, BackendKind::Vulkan);
        assert_eq!(candidate.target.vendor, GpuVendor::Nvidia);
        assert_eq!(candidate.target.device_class, DeviceClass::DiscreteGpu);
        assert_eq!(
            candidate.target.device_id.as_str(),
            "native:pci:0000:01:00.0"
        );
        assert_eq!(candidate.target.process_index, Some(3));
    }

    #[test]
    fn verified_pack_override_selects_exact_stable_device_and_driver() {
        let candidate = |identity: &str, index: usize| {
            let mut native = device(DeviceType::Gpu, "Vulkan", "NVIDIA GPU");
            native.device_id = Some(identity.to_owned());
            native.index = Some(index);
            native_backend_candidate(native).unwrap()
        };
        let mut candidates = vec![candidate("0000:02:00.0", 4), candidate("0000:01:00.0", 9)];
        apply_verified_pack_device_override_values(
            &mut candidates,
            Some("native:0000:01:00.0"),
            Some("windows-display:32.0.15.8088"),
            Some("transcribe-cpp-ggml-vulkan"),
        );
        assert_eq!(
            candidates[0].availability,
            CandidateAvailability::Incompatible
        );
        assert_eq!(candidates[1].availability, CandidateAvailability::Available);
        assert_eq!(
            candidates[1].target.driver_version.as_deref(),
            Some("windows-display:32.0.15.8088")
        );
        assert_eq!(
            candidates[1].target.provider_id.as_str(),
            "transcribe-cpp-ggml-vulkan"
        );
        let selected = select_backend(
            AccelerationPreference::Gpu,
            &snapshot(OperatingSystem::Windows, PowerSource::Ac, candidates),
        )
        .unwrap();
        assert_eq!(selected.target.device_id.as_str(), "native:0000:01:00.0");
        assert_eq!(selected.target.process_index, Some(9));
    }

    #[test]
    fn native_name_fallback_identity_is_deterministic() {
        let first = device(
            DeviceType::Igpu,
            "Vulkan0",
            "Intel(R) Arc(TM) Integrated Graphics",
        );
        let second = first.clone();

        let first = native_backend_candidate(first).unwrap().target;
        let second = native_backend_candidate(second).unwrap().target;

        assert_eq!(first.device_id, second.device_id);
        assert_eq!(first.vendor, GpuVendor::Intel);
        assert_eq!(first.device_class, DeviceClass::IntegratedGpu);
        assert!(first.device_id.as_str().starts_with("derived:"));
    }

    #[test]
    fn ambiguous_derived_devices_fail_closed_without_using_process_indices_as_identity() {
        let mut first = device(DeviceType::Gpu, "Vulkan0", "Same unnamed GPU");
        first.memory_total = 8 * 1024 * 1024 * 1024;
        first.index = Some(2);
        let mut second = first.clone();
        second.index = Some(7);
        let mut candidates = vec![
            native_backend_candidate(first).unwrap(),
            native_backend_candidate(second).unwrap(),
        ];

        assert_eq!(
            candidates[0].target.device_id,
            candidates[1].target.device_id
        );
        mark_ambiguous_derived_devices(&mut candidates);
        assert!(
            candidates
                .iter()
                .all(|candidate| candidate.availability == CandidateAvailability::Unaddressable)
        );
        let error = select_backend(
            AccelerationPreference::Gpu,
            &snapshot(OperatingSystem::Windows, PowerSource::Ac, candidates),
        )
        .unwrap_err();
        assert_eq!(error, BackendSelectionError::NoGpuTarget);
    }

    #[test]
    fn derived_identity_uses_stable_disambiguating_device_facts() {
        let mut first = device(DeviceType::Gpu, "Vulkan0", "Same unnamed GPU");
        first.memory_total = 8 * 1024 * 1024 * 1024;
        first.index = Some(2);
        let mut second = first.clone();
        second.memory_total = 12 * 1024 * 1024 * 1024;
        second.index = Some(7);

        let first = native_backend_candidate(first).unwrap().target;
        let second = native_backend_candidate(second).unwrap().target;

        assert_ne!(first.device_id, second.device_id);
        assert!(!first.device_id.as_str().contains(":2:"));
        assert!(!second.device_id.as_str().contains(":7:"));
    }

    #[test]
    fn native_backend_and_vendor_parsers_cover_supported_runtime_spellings() {
        for (kind, device_type, expected) in [
            (" CUDA ", DeviceType::Gpu, Some(BackendKind::Cuda)),
            ("VULKAN", DeviceType::Gpu, Some(BackendKind::Vulkan)),
            ("metal", DeviceType::Igpu, Some(BackendKind::Metal)),
            ("cpu", DeviceType::Cpu, Some(BackendKind::Cpu)),
            ("cpu_accel", DeviceType::Accel, Some(BackendKind::Cpu)),
            ("accel", DeviceType::Accel, Some(BackendKind::Cpu)),
            ("future-cpu", DeviceType::Cpu, Some(BackendKind::Cpu)),
            ("future-gpu", DeviceType::Gpu, None),
        ] {
            assert_eq!(backend_kind(kind, device_type), expected, "{kind:?}");
        }

        let mut reported = device(DeviceType::Gpu, "Provider0", "Provider GPU");
        for (backend, expected) in [
            ("CUDA0", BackendKind::Cuda),
            ("ggml-vulkan", BackendKind::Vulkan),
            ("Metal", BackendKind::Metal),
        ] {
            assert_eq!(observed_backend_kind(backend, &reported), Some(expected));
        }
        reported.kind = "cpu".to_owned();
        reported.device_type = DeviceType::Cpu;
        assert_eq!(
            observed_backend_kind("cpu_accel", &reported),
            Some(BackendKind::Cpu)
        );
        reported.kind = "vulkan".to_owned();
        reported.device_type = DeviceType::Gpu;
        assert_eq!(
            observed_backend_kind("provider-specific", &reported),
            Some(BackendKind::Vulkan)
        );

        for (name, description, expected) in [
            ("CUDA0", "NVIDIA RTX", GpuVendor::Nvidia),
            ("GPU", "GeForce RTX", GpuVendor::Nvidia),
            ("GPU", "Quadro P4000", GpuVendor::Nvidia),
            ("GPU", "Advanced Micro Devices GPU", GpuVendor::Amd),
            ("GPU", "AMD Radeon", GpuVendor::Amd),
            ("GPU", "Intel Arc", GpuVendor::Intel),
            ("Metal", "Apple M4 Max", GpuVendor::Apple),
            ("GPU", "Mesa Gallium", GpuVendor::Other),
            ("", "", GpuVendor::Unknown),
        ] {
            assert_eq!(
                infer_gpu_vendor(&device(DeviceType::Gpu, name, description)),
                expected,
                "{name:?} {description:?}"
            );
        }

        let cuda = Device {
            kind: "cuda".to_owned(),
            ..device(DeviceType::Gpu, "Provider0", "Misleading AMD label")
        };
        let metal = Device {
            kind: "metal".to_owned(),
            ..device(DeviceType::Igpu, "Provider0", "Misleading Intel label")
        };
        let vulkan = device(DeviceType::Gpu, "Provider0", "AMD Radeon");
        assert_eq!(
            native_backend_candidate(cuda).unwrap().target.vendor,
            GpuVendor::Nvidia
        );
        assert_eq!(
            native_backend_candidate(metal).unwrap().target.vendor,
            GpuVendor::Apple
        );
        assert_eq!(
            native_backend_candidate(vulkan).unwrap().target.vendor,
            GpuVendor::Amd
        );
    }

    fn fake_vulkan_candidates(class: DeviceClass) -> Vec<BackendCandidate> {
        let device_type = match class {
            DeviceClass::IntegratedGpu => DeviceType::Igpu,
            _ => DeviceType::Gpu,
        };
        let mut gpu = device(device_type, "Vulkan0", "NVIDIA test GPU");
        gpu.device_id = Some("PCI:0000:01:00.0".to_owned());
        gpu.memory_total = 8 * 1024 * 1024 * 1024;
        gpu.memory_free = 6 * 1024 * 1024 * 1024;
        gpu.index = Some(1);
        vec![
            native_backend_candidate(gpu).unwrap(),
            BackendCandidate::available(BackendTarget::cpu()),
        ]
    }

    #[test]
    fn warm_environment_is_invalidated_in_both_ac_battery_directions() {
        let candidates = fake_vulkan_candidates(DeviceClass::DiscreteGpu);
        let ac = qualified_snapshot(
            OperatingSystem::Windows,
            PowerSource::Ac,
            candidates.clone(),
        );
        let battery =
            qualified_snapshot(OperatingSystem::Windows, PowerSource::Battery, candidates);
        let ac_environment = select_backend_environment(AccelerationPreference::Auto, &ac).unwrap();
        let battery_environment =
            select_backend_environment(AccelerationPreference::Auto, &battery).unwrap();

        assert_eq!(ac_environment.selection.target.backend, BackendKind::Vulkan);
        assert_eq!(
            battery_environment.selection.target.backend,
            BackendKind::Cpu
        );
        assert!(!backend_environment_matches(
            &ac_environment.fingerprint,
            &battery_environment.fingerprint
        ));
        assert!(!backend_environment_matches(
            &battery_environment.fingerprint,
            &ac_environment.fingerprint
        ));
    }

    #[test]
    fn warm_environment_tracks_device_provider_driver_and_qualification_facts() {
        let candidates = fake_vulkan_candidates(DeviceClass::DiscreteGpu);
        let base = qualified_snapshot(
            OperatingSystem::Windows,
            PowerSource::Ac,
            candidates.clone(),
        );
        let loaded = select_backend_environment(AccelerationPreference::Auto, &base)
            .unwrap()
            .fingerprint;

        let mut changed_snapshots = Vec::new();

        let mut identity_changed = base.clone();
        identity_changed.candidates[0].target.device_id =
            DeviceIdentity::new("native:pci:0000:02:00.0");
        identity_changed.qualification_policy = BackendQualificationPolicy::qualify_all_for_testing(
            identity_changed.operating_system,
            &identity_changed.candidates,
        );
        changed_snapshots.push(identity_changed);

        let mut provider_changed = base.clone();
        provider_changed.candidates[0].target.provider_id =
            ProviderIdentity::new("transcribe-cpp:alternate-vulkan");
        provider_changed.qualification_policy = BackendQualificationPolicy::qualify_all_for_testing(
            provider_changed.operating_system,
            &provider_changed.candidates,
        );
        changed_snapshots.push(provider_changed);

        let mut driver_changed = base.clone();
        driver_changed.candidates[0].target.driver_version = Some("driver-2".to_owned());
        changed_snapshots.push(driver_changed);

        let mut class_changed = base.clone();
        class_changed.candidates[0].target.device_class = DeviceClass::IntegratedGpu;
        changed_snapshots.push(class_changed);

        let mut availability_changed = base.clone();
        availability_changed.candidates[0].availability = CandidateAvailability::Unhealthy;
        changed_snapshots.push(availability_changed);

        let mut backend_changed = base.clone();
        backend_changed.candidates[0].target.backend = BackendKind::Cuda;
        backend_changed.candidates[0].target.provider_id =
            ProviderIdentity::new("transcribe-cpp:cuda");
        backend_changed.qualification_policy = BackendQualificationPolicy::qualify_all_for_testing(
            backend_changed.operating_system,
            &backend_changed.candidates,
        );
        changed_snapshots.push(backend_changed);

        let mut qualification_changed = base.clone();
        qualification_changed.qualification_policy =
            BackendQualificationPolicy::stage_one_default_deny();
        changed_snapshots.push(qualification_changed);

        for changed in changed_snapshots {
            let current = select_backend_environment(AccelerationPreference::Auto, &changed)
                .unwrap()
                .fingerprint;
            assert!(!backend_environment_matches(&loaded, &current));
        }
    }

    #[test]
    fn volatile_memory_and_process_index_do_not_invalidate_a_stable_loaded_device() {
        let candidates = fake_vulkan_candidates(DeviceClass::DiscreteGpu);
        let base = qualified_snapshot(OperatingSystem::Windows, PowerSource::Ac, candidates);
        let mut changed = base.clone();
        changed.candidates[0].target.memory_available_bytes = 2 * 1024 * 1024 * 1024;
        changed.candidates[0].target.process_index = Some(9);

        assert!(backend_environment_matches(
            &base.environment_fingerprint(),
            &changed.environment_fingerprint()
        ));
    }

    #[test]
    fn enumeration_reordering_preserves_the_environment_fingerprint() {
        let candidates = fake_vulkan_candidates(DeviceClass::DiscreteGpu);
        let base = qualified_snapshot(OperatingSystem::Windows, PowerSource::Ac, candidates);
        let mut reordered = base.clone();
        reordered.candidates.reverse();

        assert_eq!(
            base.environment_fingerprint(),
            reordered.environment_fingerprint()
        );
    }

    #[test]
    fn final_acceleration_diagnostic_uses_the_typed_selection_reason() {
        let snapshot = snapshot(
            OperatingSystem::Windows,
            PowerSource::Ac,
            fake_vulkan_candidates(DeviceClass::DiscreteGpu),
        );
        let selection = select_backend(AccelerationPreference::Auto, &snapshot).unwrap();
        let expected_diagnostic = selection.auto_cpu_diagnostic();

        let resolved = resolved_acceleration(
            AccelerationPreference::Auto,
            "cpu",
            &device(DeviceType::Cpu, "CPU", ""),
            Some(selection),
        );

        assert_eq!(resolved.resolved, ComputeDevice::Cpu);
        assert_eq!(resolved.diagnostic, expected_diagnostic);
        assert_eq!(
            resolved.diagnostic.as_deref(),
            Some(
                "Auto selected CPU because available GPU backends are not qualified for automatic use."
            )
        );
    }

    #[test]
    fn selected_gpu_process_index_must_be_present_and_fit_the_native_type() {
        let selection_with_index = |process_index| {
            let mut native = device(DeviceType::Gpu, "Vulkan0", "NVIDIA GPU");
            native.device_id = Some("0000:01:00.0".to_owned());
            native.index = process_index;
            select_backend(
                AccelerationPreference::Gpu,
                &snapshot(
                    OperatingSystem::Windows,
                    PowerSource::Ac,
                    vec![native_backend_candidate(native).unwrap()],
                ),
            )
            .unwrap()
        };

        let missing = selected_process_index(&selection_with_index(None))
            .unwrap_err()
            .to_string();
        assert!(missing.starts_with("BackendUnavailable:"));
        assert!(missing.contains("has no current process index"));

        let out_of_range = usize::try_from(i32::MAX).unwrap() + 1;
        let oversized = selected_process_index(&selection_with_index(Some(out_of_range)))
            .unwrap_err()
            .to_string();
        assert!(oversized.starts_with("BackendUnavailable:"));
        assert!(oversized.contains("device index is out of range"));
    }

    #[test]
    fn observed_device_reconciliation_preserves_only_the_fresh_process_index() {
        let enumerated = Device {
            name: "Vulkan0".to_owned(),
            description: "NVIDIA GeForce RTX test device".to_owned(),
            kind: "vulkan".to_owned(),
            device_type: DeviceType::Gpu,
            device_id: Some("0000:01:00.0".to_owned()),
            memory_total: 8 * 1024 * 1024 * 1024,
            memory_free: 6 * 1024 * 1024 * 1024,
            index: Some(3),
        };
        let candidate = native_backend_candidate(enumerated).unwrap();
        let mut selection = select_backend(
            AccelerationPreference::Gpu,
            &snapshot(OperatingSystem::Windows, PowerSource::Ac, vec![candidate]),
        )
        .unwrap();
        let observed = Device {
            name: "Vulkan0".to_owned(),
            description: "NVIDIA GeForce RTX test device".to_owned(),
            kind: "vulkan".to_owned(),
            device_type: DeviceType::Gpu,
            device_id: Some("0000:01:00.0".to_owned()),
            memory_total: 8 * 1024 * 1024 * 1024,
            memory_free: 5 * 1024 * 1024 * 1024,
            index: None,
        };

        reconcile_observed_target(&mut selection, "vulkan", &observed).unwrap();

        assert_eq!(selection.target.process_index, Some(3));
        assert_eq!(
            selection.target.memory_available_bytes,
            5 * 1024 * 1024 * 1024
        );
    }

    #[test]
    fn observed_device_reconciliation_selects_the_stable_identity_from_multiple_devices() {
        let first = Device {
            name: "Vulkan0".to_owned(),
            description: "NVIDIA GPU A".to_owned(),
            kind: "vulkan".to_owned(),
            device_type: DeviceType::Gpu,
            device_id: Some("0000:02:00.0".to_owned()),
            memory_total: 8 * 1024 * 1024 * 1024,
            memory_free: 6 * 1024 * 1024 * 1024,
            index: Some(4),
        };
        let second = Device {
            name: "Vulkan1".to_owned(),
            description: "NVIDIA GPU B".to_owned(),
            device_id: Some("0000:01:00.0".to_owned()),
            index: Some(9),
            ..first.clone()
        };
        let mut selection = select_backend(
            AccelerationPreference::Gpu,
            &snapshot(
                OperatingSystem::Windows,
                PowerSource::Ac,
                vec![
                    native_backend_candidate(first).unwrap(),
                    native_backend_candidate(second.clone()).unwrap(),
                ],
            ),
        )
        .unwrap();

        assert_eq!(selection.target.device_id.as_str(), "native:0000:01:00.0");
        reconcile_observed_target(&mut selection, "vulkan", &second).unwrap();
        assert_eq!(selection.target.device_id.as_str(), "native:0000:01:00.0");
        assert_eq!(selection.target.process_index, Some(9));
    }

    #[test]
    fn observed_device_reconciliation_rejects_backend_or_identity_drift() {
        let enumerated = Device {
            name: "Vulkan0".to_owned(),
            description: "NVIDIA GPU A".to_owned(),
            kind: "vulkan".to_owned(),
            device_type: DeviceType::Gpu,
            device_id: Some("0000:01:00.0".to_owned()),
            memory_total: 8 * 1024 * 1024 * 1024,
            memory_free: 6 * 1024 * 1024 * 1024,
            index: Some(2),
        };
        let selection = || {
            select_backend(
                AccelerationPreference::Gpu,
                &snapshot(
                    OperatingSystem::Windows,
                    PowerSource::Ac,
                    vec![native_backend_candidate(enumerated.clone()).unwrap()],
                ),
            )
            .unwrap()
        };
        let wrong_device = Device {
            device_id: Some("0000:02:00.0".to_owned()),
            index: None,
            ..enumerated.clone()
        };
        let mut wrong_device_selection = selection();
        let identity_error =
            reconcile_observed_target(&mut wrong_device_selection, "vulkan", &wrong_device)
                .unwrap_err()
                .to_string();
        assert!(identity_error.starts_with("BackendUnavailable:"));
        assert!(identity_error.contains("native:0000:02:00.0"));

        let cpu = Device {
            name: "CPU".to_owned(),
            description: "CPU".to_owned(),
            kind: "cpu".to_owned(),
            device_type: DeviceType::Cpu,
            device_id: None,
            memory_total: 0,
            memory_free: 0,
            index: None,
        };
        let mut wrong_backend_selection = selection();
        let backend_error = reconcile_observed_target(&mut wrong_backend_selection, "cpu", &cpu)
            .unwrap_err()
            .to_string();
        assert!(backend_error.starts_with("BackendUnavailable:"));
        assert!(backend_error.contains("requested Vulkan"));

        let mut wrong_class = enumerated.clone();
        wrong_class.device_type = DeviceType::Igpu;
        let mut wrong_class_selection = selection();
        let class_error =
            reconcile_observed_target(&mut wrong_class_selection, "vulkan", &wrong_class)
                .unwrap_err()
                .to_string();
        assert!(class_error.starts_with("BackendUnavailable:"));
        assert!(class_error.contains("IntegratedGpu"));

        let mut wrong_provider_selection = selection();
        wrong_provider_selection.target.provider_id =
            ProviderIdentity::new("transcribe-cpp:alternate-vulkan");
        let provider_error =
            reconcile_observed_target(&mut wrong_provider_selection, "vulkan", &enumerated)
                .unwrap_err()
                .to_string();
        assert!(provider_error.starts_with("BackendUnavailable:"));
        assert!(provider_error.contains("alternate-vulkan"));

        let mut wrong_vendor_selection = selection();
        wrong_vendor_selection.target.vendor = GpuVendor::Amd;
        let vendor_error =
            reconcile_observed_target(&mut wrong_vendor_selection, "vulkan", &enumerated)
                .unwrap_err()
                .to_string();
        assert!(vendor_error.starts_with("BackendUnavailable:"));
        assert!(vendor_error.contains("Amd"));
        assert!(vendor_error.contains("Nvidia"));

        let mut wrong_driver_selection = selection();
        wrong_driver_selection.target.driver_version = Some("driver-2".to_owned());
        reconcile_observed_target(&mut wrong_driver_selection, "vulkan", &enumerated).unwrap();
        assert_eq!(
            wrong_driver_selection.target.driver_version.as_deref(),
            Some("driver-2")
        );
    }

    #[test]
    #[ignore = "requires SCRIBE_TRANSCRIBE_CPP_GGUF to name a compatible local GGUF fixture"]
    fn compatible_gguf_loads_and_reports_runtime_capabilities() {
        let path = PathBuf::from(
            std::env::var_os("SCRIBE_TRANSCRIBE_CPP_GGUF")
                .expect("set SCRIBE_TRANSCRIBE_CPP_GGUF to a trusted GGUF fixture"),
        );
        let mut runtime = EmbeddedRuntime::new(path, AccelerationPreference::Cpu);

        runtime.load().unwrap();

        assert!(runtime.resolved_acceleration().is_some());
        assert!(runtime.capabilities().timestamps);
        runtime.unload().unwrap();
    }

    #[test]
    #[ignore = "requires SCRIBE_TRANSCRIBE_CPP_GGUF and SCRIBE_TRANSCRIBE_CPP_AUDIO fixtures"]
    fn compatible_gguf_transcribes_canonical_audio_in_process() {
        let model_path = PathBuf::from(
            std::env::var_os("SCRIBE_TRANSCRIBE_CPP_GGUF")
                .expect("set SCRIBE_TRANSCRIBE_CPP_GGUF to a trusted GGUF fixture"),
        );
        let audio_path = PathBuf::from(
            std::env::var_os("SCRIBE_TRANSCRIBE_CPP_AUDIO")
                .expect("set SCRIBE_TRANSCRIBE_CPP_AUDIO to a WAV fixture"),
        );
        let audio = PreparedAudio::from_wav_path(audio_path).unwrap();
        let mut runtime = EmbeddedRuntime::new(model_path, AccelerationPreference::Cpu);

        let generation = AtomicU64::new(0);
        let transcript = runtime
            .transcribe_with_cancellation(&audio, &TranscriptionOptions::default(), &generation, 0)
            .unwrap();

        assert!(!transcript.text.trim().is_empty());
        assert!(transcript.duration_ms.is_some());
        runtime.unload().unwrap();
    }
}
