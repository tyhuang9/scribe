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
            "faster_whisper_tiny_en",
            "faster-whisper tiny.en",
            "faster-whisper",
            "1 GB",
            "Basic",
            "Fastest GPU",
            false,
        ),
        model(
            "faster_whisper_base_en",
            "faster-whisper base.en",
            "faster-whisper",
            "1 GB",
            "Good",
            "Fast GPU",
            false,
        ),
        model(
            "faster_whisper_small_en_gpu",
            "faster-whisper small.en",
            "faster-whisper",
            "1-2 GB",
            "Good",
            "Fast GPU",
            false,
        ),
        model(
            "faster_whisper_medium_en_gpu",
            "faster-whisper medium.en",
            "faster-whisper",
            "3-6 GB",
            "High",
            "Medium GPU",
            false,
        ),
        model(
            "faster_whisper_large_v3",
            "faster-whisper large-v3",
            "faster-whisper",
            "5-10 GB",
            "Highest",
            "Slow GPU",
            false,
        ),
        model(
            "faster_whisper_turbo",
            "faster-whisper turbo",
            "faster-whisper",
            "4-8 GB",
            "High",
            "Fast GPU",
            false,
        ),
        model(
            "faster_whisper_distil_large_v3",
            "faster-whisper distil-large-v3",
            "faster-whisper",
            "3-6 GB",
            "High",
            "Fast GPU",
            false,
        ),
        model(
            "sherpa_onnx_zipformer_small",
            "sherpa-onnx Zipformer Small",
            "sherpa-onnx",
            "1-2 GB",
            "Good",
            "Streaming",
            false,
        ),
        model(
            "moonshine",
            "Moonshine",
            "Moonshine",
            "1-2 GB",
            "Good",
            "Fast",
            false,
        ),
        model(
            "parakeet_0_6b",
            "Parakeet 0.6B",
            "Parakeet",
            "2-4 GB",
            "High",
            "Medium",
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
