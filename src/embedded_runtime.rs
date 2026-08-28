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

use crate::prepared_audio::{PREPARED_SAMPLE_RATE, PreparedAudio};
use crate::transcription::{
    AccelerationPreference, ComputeDevice, ResolvedAcceleration, RuntimeCapabilities, SpeechEngine,
    Transcript, TranscriptSegment, TranscriptionOptions,
};

/// Exact safe-wrapper/native release compiled into this Scribe build.
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
        if self.session.is_some() {
            return Ok(());
        }
        Self::ensure_backends()?;
        if !self.model_path.is_file() {
            return Err(anyhow!(EmbeddedRuntimeError::ModelNotInstalled(
                self.model_path.clone()
            )));
        }

        let model = Model::load_with(
            &self.model_path,
            &ModelOptions {
                backend: requested_backend(self.preference),
                gpu_device: 0,
            },
        )
        .map_err(map_native_error)?;
        let detected_architecture = model.arch();
        let native_capabilities = model.capabilities();
        let resolved_backend = model.backend();
        let resolved_device = model.device().map_err(map_native_error)?;
        let resolved_acceleration =
            resolved_acceleration(self.preference, &resolved_backend, &resolved_device);
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
        self.model = Some(model);
        self.session = Some(session);
        Ok(())
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
        // Dropping the session first guarantees that the safe wrapper retains
        // the model only until no native session can still reference it.
        self.session = None;
        self.model = None;
        self.cancellation.reset();
        self.capabilities = RuntimeCapabilities::default();
        self.detected_architecture = None;
        self.resolved_acceleration = None;
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

fn requested_backend(preference: AccelerationPreference) -> Backend {
    match preference {
        AccelerationPreference::Auto => Backend::Auto,
        AccelerationPreference::Cpu => Backend::Cpu,
        AccelerationPreference::Gpu => Backend::Vulkan,
    }
}

fn resolved_acceleration(
    requested: AccelerationPreference,
    backend: &str,
    device: &Device,
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
    let diagnostic = match (&requested, &resolved) {
        (AccelerationPreference::Auto, ComputeDevice::Cpu) => {
            Some("No compatible GPU was available; Auto fell back to CPU.".to_owned())
        }
        (AccelerationPreference::Gpu, ComputeDevice::Cpu) => {
            Some("The strict GPU request unexpectedly resolved to CPU.".to_owned())
        }
        _ => None,
    };
    ResolvedAcceleration {
        requested,
        resolved,
        diagnostic,
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
        assert_eq!(
            requested_backend(AccelerationPreference::Auto),
            Backend::Auto
        );
        assert_eq!(requested_backend(AccelerationPreference::Cpu), Backend::Cpu);
        assert_eq!(
            requested_backend(AccelerationPreference::Gpu),
            Backend::Vulkan
        );
    }

    #[test]
    fn resolved_device_type_drives_truthful_acceleration_reporting() {
        let cpu = resolved_acceleration(
            AccelerationPreference::Auto,
            "cpu",
            &device(DeviceType::Cpu, "CPU", ""),
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
        );

        assert_eq!(resolved.resolved, ComputeDevice::Cpu);
        assert_eq!(resolved.diagnostic, None);
    }

    fn device(device_type: DeviceType, name: &str, description: &str) -> Device {
        Device {
            name: name.to_owned(),
            description: description.to_owned(),
            kind: String::new(),
            device_type,
            device_id: None,
            memory_total: 0,
            memory_free: 0,
            index: None,
        }
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
