//! Dependency-light private contract shared by the desktop and inference worker.
//!
//! Wire-visible values live here once so serde compatibility cannot drift
//! between executables. Application orchestration stays in `transcription.rs`.

use std::fmt;

use anyhow::Result;
use serde::{Deserialize, Serialize};

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

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ResolvedAcceleration {
    pub requested: AccelerationPreference,
    pub resolved: ComputeDevice,
    pub diagnostic: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) selection: Option<crate::backend_policy::BackendSelection>,
}

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
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Transcript {
    pub text: String,
    pub segments: Vec<TranscriptSegment>,
    pub detected_language: Option<String>,
    pub duration_ms: Option<u128>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TranscriptSegment {
    pub text: String,
    pub start_ms: Option<u64>,
    pub end_ms: Option<u64>,
    pub confidence: Option<f32>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct TranscriptionOptions {
    pub language: Option<String>,
    pub translate_to_english: bool,
    pub enable_timestamps: bool,
    pub initial_prompt: Option<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct RuntimeCapabilities {
    pub streaming: bool,
    pub cancellation: bool,
    pub translation: bool,
    pub timestamps: bool,
    pub language_detection: bool,
    pub confidence_scores: bool,
    pub custom_vocabulary: bool,
    pub supported_languages: Vec<String>,
}

pub trait SpeechEngine: Send {
    fn load(&mut self) -> Result<()>;
    fn capabilities(&self) -> RuntimeCapabilities;
    fn unload(&mut self) -> Result<()>;
}
