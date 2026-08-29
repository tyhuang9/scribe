//! Desktop-only inference-router stub.
//!
//! Production desktop code supervises the dedicated worker and must not own a
//! native GGUF runtime. VAD uses this cancellation-only shell because its
//! legacy server loop shares a watchdog implementation with inference.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::prepared_audio::PreparedAudio;
use crate::runtime_artifact::RuntimeArtifact;
use crate::runtime_contract::{RuntimeError, RuntimeExecution, RuntimeLoadExecution};
use crate::transcription::{AccelerationPreference, TranscriptionOptions};

#[derive(Clone, Default)]
pub(crate) struct RuntimeRouter {
    cancellation_generation: Arc<AtomicU64>,
}

impl RuntimeRouter {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn load(
        &self,
        artifact: RuntimeArtifact,
        _preference: AccelerationPreference,
    ) -> Result<RuntimeLoadExecution, RuntimeError> {
        Err(RuntimeError::UnsupportedModel(artifact.model_id()))
    }

    pub(crate) fn transcribe(
        &self,
        artifact: RuntimeArtifact,
        _preference: AccelerationPreference,
        _audio: &PreparedAudio,
        _options: &TranscriptionOptions,
        _cancellation_snapshot: u64,
    ) -> Result<RuntimeExecution, RuntimeError> {
        Err(RuntimeError::UnsupportedModel(artifact.model_id()))
    }

    pub(crate) fn cancel_active(&self) {
        self.cancellation_generation.fetch_add(1, Ordering::AcqRel);
    }

    pub(crate) fn cancellation_snapshot(&self) -> u64 {
        self.cancellation_generation.load(Ordering::Acquire)
    }

    pub(crate) fn unload_all(&self) -> Result<(), RuntimeError> {
        Ok(())
    }
}
