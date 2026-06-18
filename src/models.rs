use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SttModelInfo {
    pub id: String,
    pub name: String,
    pub backend: String,
    pub expected_ram: String,
    pub accuracy_tier: String,
    pub speed_tier: String,
    pub local_path: Option<PathBuf>,
    pub download_status: String,
    pub enabled: bool,
}

#[derive(Clone, Debug)]
pub struct TranscriptSegment {
    pub start_ms: Option<u64>,
    pub end_ms: Option<u64>,
    pub text: String,
}

#[derive(Clone, Debug)]
pub struct TranscriptResult {
    pub model_id: String,
    pub model_name: String,
    pub backend: String,
    pub text: String,
    pub segments: Vec<TranscriptSegment>,
    pub duration_ms: Option<u128>,
    pub stdout: String,
    pub stderr: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TranscriptionStatus {
    Idle,
    Listening,
    Transcribing,
    Error,
}

impl fmt::Display for TranscriptionStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Idle => write!(f, "Idle"),
            Self::Listening => write!(f, "Listening"),
            Self::Transcribing => write!(f, "Transcribing"),
            Self::Error => write!(f, "Error"),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ModelRuntimeStatus {
    Ready,
    MissingConfiguration,
    Running,
    Disabled,
    NotImplemented,
    Error(String),
}

impl fmt::Display for ModelRuntimeStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ready => write!(f, "Ready"),
            Self::MissingConfiguration => write!(f, "Missing configuration"),
            Self::Running => write!(f, "Running"),
            Self::Disabled => write!(f, "Disabled"),
            Self::NotImplemented => write!(f, "Placeholder"),
            Self::Error(message) => write!(f, "Error: {message}"),
        }
    }
}

pub fn default_model_catalog() -> Vec<SttModelInfo> {
    vec![
        model(
            "whisper_cpp_tiny_en",
            "whisper.cpp tiny.en",
            "whisper.cpp",
            "1 GB",
            "Basic",
            "Fastest",
            true,
        ),
        model(
            "whisper_cpp_base_en",
            "whisper.cpp base.en",
            "whisper.cpp",
            "1 GB",
            "Good",
            "Fast",
            false,
        ),
        model(
            "whisper_cpp_small_en",
            "whisper.cpp small.en",
            "whisper.cpp",
            "2 GB",
            "Better",
            "Medium",
            false,
        ),
        model(
            "whisper_cpp_medium_en",
            "whisper.cpp medium.en",
            "whisper.cpp",
            "5 GB",
            "High",
            "Slower",
            false,
        ),
        model(
            "vosk_small_en",
            "Vosk small English placeholder",
            "Vosk",
            "1 GB",
            "Basic",
            "Fast",
            false,
        ),
        model(
            "sherpa_onnx_streaming",
            "sherpa-onnx streaming placeholder",
            "sherpa-onnx",
            "1-2 GB",
            "Good",
            "Streaming",
            false,
        ),
        model(
            "faster_whisper",
            "faster-whisper placeholder",
            "faster-whisper",
            "2-6 GB",
            "High",
            "GPU-friendly",
            false,
        ),
    ]
}

fn model(
    id: &str,
    name: &str,
    backend: &str,
    expected_ram: &str,
    accuracy_tier: &str,
    speed_tier: &str,
    enabled: bool,
) -> SttModelInfo {
    SttModelInfo {
        id: id.to_owned(),
        name: name.to_owned(),
        backend: backend.to_owned(),
        expected_ram: expected_ram.to_owned(),
        accuracy_tier: accuracy_tier.to_owned(),
        speed_tier: speed_tier.to_owned(),
        local_path: None,
        download_status: "Not configured".to_owned(),
        enabled,
    }
}
