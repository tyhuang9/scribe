use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SttModelInfo {
    pub id: String,
    pub name: String,
    pub backend: String,
    pub description: String,
    pub expected_ram: String,
    pub accuracy_tier: String,
    pub speed_tier: String,
    pub local_path: Option<PathBuf>,
    pub install_status: ModelInstallStatus,
    pub download_model: Option<String>,
    pub enabled: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ModelInstallStatus {
    NotInstalled,
    Downloading {
        downloaded_bytes: u64,
        total_bytes: Option<u64>,
    },
    Installed,
    Missing,
    Error(String),
}

impl ModelInstallStatus {
    pub fn is_runnable(&self) -> bool {
        matches!(self, Self::Installed)
    }

    pub fn label(&self) -> String {
        match self {
            Self::NotInstalled => "Not installed".to_owned(),
            Self::Downloading {
                downloaded_bytes,
                total_bytes,
            } => match total_bytes {
                Some(total) if *total > 0 => {
                    let percent =
                        (*downloaded_bytes as f64 / *total as f64 * 100.0).clamp(0.0, 100.0);
                    format!("Downloading {:.0}%", percent)
                }
                _ => format!("Downloading {}", format_bytes(*downloaded_bytes)),
            },
            Self::Installed => "Installed".to_owned(),
            Self::Missing => "Missing file".to_owned(),
            Self::Error(message) => format!("Error: {message}"),
        }
    }
}

impl Default for ModelInstallStatus {
    fn default() -> Self {
        Self::NotInstalled
    }
}

impl fmt::Display for ModelInstallStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.label())
    }
}

#[allow(dead_code)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecordingStatus {
    Idle,
    Recording,
    Finalizing,
    Error,
}

impl fmt::Display for RecordingStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Idle => write!(f, "Idle"),
            Self::Recording => write!(f, "Recording"),
            Self::Finalizing => write!(f, "Finalizing"),
            Self::Error => write!(f, "Error"),
        }
    }
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
    NotInstalled,
    Downloading,
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
            Self::NotInstalled => write!(f, "Not installed"),
            Self::Downloading => write!(f, "Downloading"),
            Self::Running => write!(f, "Running"),
            Self::Disabled => write!(f, "Disabled"),
            Self::NotImplemented => write!(f, "Placeholder"),
            Self::Error(message) => write!(f, "Error: {message}"),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BackendCapabilities {
    pub runnable: bool,
    pub supports_local_files: bool,
    pub supports_downloads: bool,
    pub streaming: bool,
    pub experimental: bool,
}

pub fn backend_capabilities(backend: &str) -> BackendCapabilities {
    match backend {
        "whisper.cpp" => BackendCapabilities {
            runnable: true,
            supports_local_files: true,
            supports_downloads: true,
            streaming: false,
            experimental: false,
        },
        "Vosk" | "sherpa-onnx" | "faster-whisper" | "Moonshine" | "Parakeet" => {
            BackendCapabilities {
                runnable: false,
                supports_local_files: true,
                supports_downloads: false,
                streaming: backend == "sherpa-onnx",
                experimental: true,
            }
        }
        _ => BackendCapabilities {
            runnable: false,
            supports_local_files: false,
            supports_downloads: false,
            streaming: false,
            experimental: true,
        },
    }
}

pub fn whisper_cpp_download_url(model_name: &str) -> String {
    format!("https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-{model_name}.bin")
}

pub fn default_model_catalog() -> Vec<SttModelInfo> {
    vec![
        model(
            "whisper_cpp_tiny_en",
            "whisper.cpp tiny.en",
            "whisper.cpp",
            "Smallest local English model for quick testing and low-resource machines.",
            "1 GB",
            "Basic",
            "Fastest",
            Some("tiny.en"),
            true,
        ),
        model(
            "whisper_cpp_base_en",
            "whisper.cpp base.en",
            "whisper.cpp",
            "Recommended first-run local English model with a better speed/quality balance.",
            "1 GB",
            "Good",
            "Fast",
            Some("base.en"),
            false,
        ),
        model(
            "whisper_cpp_small_en",
            "whisper.cpp small.en",
            "whisper.cpp",
            "More accurate local English model for longer dictation and cleaner audio.",
            "2 GB",
            "Better",
            "Medium",
            Some("small.en"),
            false,
        ),
        model(
            "whisper_cpp_medium_en",
            "whisper.cpp medium.en",
            "whisper.cpp",
            "Higher-accuracy local English model for machines with more memory.",
            "5 GB",
            "High",
            "Slower",
            Some("medium.en"),
            false,
        ),
        model(
            "vosk_small_en",
            "Vosk small English placeholder",
            "Vosk",
            "Planned offline runtime; catalog metadata only in this phase.",
            "1 GB",
            "Basic",
            "Fast",
            None,
            false,
        ),
        model(
            "faster_whisper_tiny_en",
            "faster-whisper tiny.en",
            "faster-whisper",
            "Planned GPU-oriented runtime; catalog metadata only in this phase.",
            "1 GB",
            "Basic",
            "Fastest GPU",
            None,
            false,
        ),
        model(
            "faster_whisper_base_en",
            "faster-whisper base.en",
            "faster-whisper",
            "Planned GPU-oriented runtime; catalog metadata only in this phase.",
            "1 GB",
            "Good",
            "Fast GPU",
            None,
            false,
        ),
        model(
            "faster_whisper_small_en_gpu",
            "faster-whisper small.en",
            "faster-whisper",
            "Planned GPU-oriented runtime; catalog metadata only in this phase.",
            "1-2 GB",
            "Good",
            "Fast GPU",
            None,
            false,
        ),
        model(
            "faster_whisper_medium_en_gpu",
            "faster-whisper medium.en",
            "faster-whisper",
            "Planned GPU-oriented runtime; catalog metadata only in this phase.",
            "3-6 GB",
            "High",
            "Medium GPU",
            None,
            false,
        ),
        model(
            "faster_whisper_large_v3",
            "faster-whisper large-v3",
            "faster-whisper",
            "Planned GPU-oriented runtime; catalog metadata only in this phase.",
            "5-10 GB",
            "Highest",
            "Slow GPU",
            None,
            false,
        ),
        model(
            "faster_whisper_turbo",
            "faster-whisper turbo",
            "faster-whisper",
            "Planned GPU-oriented runtime; catalog metadata only in this phase.",
            "4-8 GB",
            "High",
            "Fast GPU",
            None,
            false,
        ),
        model(
            "faster_whisper_distil_large_v3",
            "faster-whisper distil-large-v3",
            "faster-whisper",
            "Planned GPU-oriented runtime; catalog metadata only in this phase.",
            "3-6 GB",
            "High",
            "Fast GPU",
            None,
            false,
        ),
        model(
            "sherpa_onnx_zipformer_small",
            "sherpa-onnx Zipformer Small",
            "sherpa-onnx",
            "Planned streaming runtime; catalog metadata only in this phase.",
            "1-2 GB",
            "Good",
            "Streaming",
            None,
            false,
        ),
        model(
            "moonshine",
            "Moonshine",
            "Moonshine",
            "Planned lightweight runtime; catalog metadata only in this phase.",
            "1-2 GB",
            "Good",
            "Fast",
            None,
            false,
        ),
        model(
            "parakeet_0_6b",
            "Parakeet 0.6B",
            "Parakeet",
            "Planned experimental runtime; catalog metadata only in this phase.",
            "2-4 GB",
            "High",
            "Medium",
            None,
            false,
        ),
    ]
}

fn model(
    id: &str,
    name: &str,
    backend: &str,
    description: &str,
    expected_ram: &str,
    accuracy_tier: &str,
    speed_tier: &str,
    download_model: Option<&str>,
    enabled: bool,
) -> SttModelInfo {
    SttModelInfo {
        id: id.to_owned(),
        name: name.to_owned(),
        backend: backend.to_owned(),
        description: description.to_owned(),
        expected_ram: expected_ram.to_owned(),
        accuracy_tier: accuracy_tier.to_owned(),
        speed_tier: speed_tier.to_owned(),
        local_path: None,
        install_status: ModelInstallStatus::NotInstalled,
        download_model: download_model.map(str::to_owned),
        enabled,
    }
}

fn format_bytes(bytes: u64) -> String {
    const MIB: f64 = 1024.0 * 1024.0;
    const GIB: f64 = 1024.0 * 1024.0 * 1024.0;
    let bytes = bytes as f64;
    if bytes >= GIB {
        format!("{:.1} GB", bytes / GIB)
    } else if bytes >= MIB {
        format!("{:.0} MB", bytes / MIB)
    } else {
        format!("{bytes:.0} B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whisper_download_url_uses_official_whisper_cpp_pattern() {
        assert_eq!(
            whisper_cpp_download_url("base.en"),
            "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.en.bin"
        );
    }

    #[test]
    fn install_status_labels_progress() {
        let status = ModelInstallStatus::Downloading {
            downloaded_bytes: 25,
            total_bytes: Some(100),
        };

        assert_eq!(status.label(), "Downloading 25%");
        assert!(ModelInstallStatus::Installed.is_runnable());
        assert!(!ModelInstallStatus::NotInstalled.is_runnable());
    }

    #[test]
    fn only_whisper_cpp_is_runnable_in_this_phase() {
        assert!(backend_capabilities("whisper.cpp").runnable);
        assert!(!backend_capabilities("faster-whisper").runnable);
        assert!(!backend_capabilities("Vosk").supports_downloads);
    }
}
