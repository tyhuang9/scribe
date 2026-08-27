//! Typed artifacts crossing the private inference-worker boundary.

use crate::onnx_worker::OnnxModelSpec;
use crate::runtime_router::RuntimeModel;
use crate::transcription::ModelId;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum RuntimeArtifact {
    Gguf(RuntimeModel),
    LegacyCompatibility(RuntimeModel),
    OnnxBundle(OnnxModelSpec),
}

impl From<RuntimeModel> for RuntimeArtifact {
    fn from(model: RuntimeModel) -> Self {
        if model.is_gguf() {
            Self::Gguf(model)
        } else {
            Self::LegacyCompatibility(model)
        }
    }
}

impl RuntimeArtifact {
    pub(crate) fn model_id(&self) -> ModelId {
        match self {
            Self::Gguf(model) | Self::LegacyCompatibility(model) => model.id.clone(),
            Self::OnnxBundle(model) => ModelId::new(model.id.clone()),
        }
    }
}
