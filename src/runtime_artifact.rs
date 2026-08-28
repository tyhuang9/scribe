//! Typed artifacts crossing the private inference-worker boundary.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::model_catalog::ArtifactFormat;
use crate::transcription::ModelId;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum OnnxFileRole {
    Model,
    Encoder,
    Decoder,
    Joiner,
    Tokens,
    Preprocessor,
    UncachedDecoder,
    CachedDecoder,
    MergedDecoder,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum OnnxModelFamily {
    Moonshine,
    NemoCtc,
    Canary,
    OfflineTransducer,
    OnlineTransducer,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct OnnxModelSpec {
    pub id: String,
    pub root: PathBuf,
    pub family: OnnxModelFamily,
    pub files: BTreeMap<OnnxFileRole, PathBuf>,
    pub num_threads: u16,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RuntimeModel {
    pub id: ModelId,
    pub path: PathBuf,
    pub format: ArtifactFormat,
    pub expected_size_bytes: u64,
    pub expected_sha256: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum RuntimeArtifact {
    Gguf(RuntimeModel),
    OnnxBundle(OnnxModelSpec),
}

impl From<RuntimeModel> for RuntimeArtifact {
    fn from(model: RuntimeModel) -> Self {
        Self::Gguf(model)
    }
}

impl RuntimeArtifact {
    pub(crate) fn model_id(&self) -> ModelId {
        match self {
            Self::Gguf(model) => model.id.clone(),
            Self::OnnxBundle(model) => ModelId::new(model.id.clone()),
        }
    }
}
