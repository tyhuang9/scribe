//! Dependency-light contract shared by the desktop and inference worker.

use std::path::PathBuf;
use std::time::Duration;

use thiserror::Error;

use crate::model_catalog::{RuntimeRequirement, RuntimeVersion, runtime_model_manifest};
use crate::transcription::{ModelId, ResolvedAcceleration, RuntimeCapabilities, Transcript};

pub(crate) const TRANSCRIBE_CPP_VERSION: &str = "0.1.3";
pub(crate) const WARM_MODEL_TTL: Duration = Duration::from_secs(5 * 60);

const TRANSCRIBE_CPP_RUNTIME_VERSION: RuntimeVersion = RuntimeVersion {
    major: 1,
    minor: 9,
    patch: 1,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct NativeRuntimeDiagnostics {
    pub resolved_acceleration: ResolvedAcceleration,
    pub runtime_location: PathBuf,
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
    pub detected_architecture: String,
    pub capabilities: RuntimeCapabilities,
}

#[derive(Debug, Error)]
pub(crate) enum RuntimeError {
    #[error(
        "runtime audio must be mono 16 kHz; received {channels} channel(s) at {sample_rate_hz} Hz"
    )]
    InvalidAudio { sample_rate_hz: u32, channels: u16 },
    #[error("native inference failed: {0}")]
    Inference(String),
    #[error("native callback failed: {0}")]
    Callback(String),
    #[error("native speech engine failed: {0}")]
    Engine(String),
    #[error("GGUF artifact integrity check failed for {path}: {message}")]
    ArtifactIntegrity { path: PathBuf, message: String },
    #[error("native speech runtime lock was poisoned")]
    Poisoned,
    #[error("the model is not handled by the static GGUF runtime: {0}")]
    UnsupportedModel(ModelId),
    #[error("dedicated native runtime worker is unavailable: {0}")]
    WorkerUnavailable(String),
    #[error("dedicated native runtime worker failed before producing output: {0}")]
    RetryableWorkerFailure(String),
    #[error("transcription request was cancelled: {0}")]
    Cancelled(String),
    #[error("isolated ONNX speech runtime is unavailable: {0}")]
    OnnxUnavailable(String),
}

pub(crate) fn handles_model_id(model_id: &ModelId) -> bool {
    runtime_model_manifest(model_id).is_some_and(|manifest| {
        manifest.runtime == RuntimeRequirement::PrimaryNative
            && manifest.artifact_filename.ends_with(".gguf")
            && TRANSCRIBE_CPP_RUNTIME_VERSION >= manifest.minimum_runtime_version
    })
}

pub(crate) fn embedded_runtime_capabilities() -> RuntimeCapabilities {
    RuntimeCapabilities {
        cancellation: true,
        timestamps: true,
        language_detection: true,
        ..RuntimeCapabilities::default()
    }
}

pub(crate) fn capabilities_for_model(model_id: &ModelId) -> Option<RuntimeCapabilities> {
    handles_model_id(model_id).then(embedded_runtime_capabilities)
}
