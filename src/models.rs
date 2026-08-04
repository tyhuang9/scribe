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
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub enum ModelInstallStatus {
    #[default]
    NotInstalled,
    Downloading {
        downloaded_bytes: u64,
        total_bytes: Option<u64>,
        #[serde(default)]
        bytes_per_second: Option<u64>,
    },
    InstallingRuntime,
    Installed,
    Missing,
    RuntimeError(String),
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
                bytes_per_second,
            } => {
                let progress = match total_bytes {
                    Some(total) if *total > 0 => {
                        let percent =
                            (*downloaded_bytes as f64 / *total as f64 * 100.0).clamp(0.0, 100.0);
                        format!("Downloading {:.0}%", percent)
                    }
                    _ => format!("Downloading {}", format_bytes(*downloaded_bytes)),
                };
                match bytes_per_second.filter(|speed| *speed > 0) {
                    Some(speed) => format!("{progress} · {}/s", format_bytes(speed)),
                    None => progress,
                }
            }
            Self::InstallingRuntime => "Verifying and installing artifacts".to_owned(),
            Self::Installed => "Installed".to_owned(),
            Self::Missing => "Missing file".to_owned(),
            Self::RuntimeError(message) => format!("Runtime error: {message}"),
            Self::Error(message) => format!("Error: {message}"),
        }
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
            Self::NotImplemented => write!(f, "Runtime unavailable"),
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
        "faster-whisper" => BackendCapabilities {
            runnable: true,
            supports_local_files: true,
            supports_downloads: true,
            streaming: false,
            experimental: false,
        },
        "Vosk" => BackendCapabilities {
            runnable: true,
            supports_local_files: true,
            supports_downloads: true,
            streaming: false,
            experimental: false,
        },
        "sherpa-onnx" | "Moonshine" | "Parakeet" => BackendCapabilities {
            runnable: true,
            supports_local_files: true,
            supports_downloads: true,
            streaming: false,
            experimental: true,
        },
        _ => BackendCapabilities {
            runnable: false,
            supports_local_files: false,
            supports_downloads: false,
            streaming: false,
            experimental: true,
        },
    }
}

pub fn vosk_model_download_url(model_name: &str) -> Option<&'static str> {
    match model_name {
        "vosk-model-small-en-us-0.15" => {
            Some("https://alphacephei.com/vosk/models/vosk-model-small-en-us-0.15.zip")
        }
        _ => None,
    }
}

pub fn sherpa_model_download_url(model_name: &str) -> Option<&'static str> {
    match model_name {
        "sherpa-onnx-zipformer-small-en-2023-06-26" => Some(
            "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-zipformer-small-en-2023-06-26.tar.bz2",
        ),
        "sherpa-onnx-moonshine-tiny-en-quantized-2026-02-27" => Some(
            "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-moonshine-tiny-en-quantized-2026-02-27.tar.bz2",
        ),
        "sherpa-onnx-nemo-parakeet-unified-en-0.6b-int8-non-streaming" => Some(
            "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-nemo-parakeet-unified-en-0.6b-int8-non-streaming.tar.bz2",
        ),
        _ => None,
    }
}

pub fn default_model_catalog() -> Vec<SttModelInfo> {
    let mut models = crate::model_catalog::model_descriptors()
        .into_iter()
        .map(|descriptor| {
            let runtime = crate::model_catalog::runtime_model_manifest(&descriptor.id)
                .expect("every normalized descriptor must have a runtime manifest");
            let download_model = runtime
                .artifact_filename
                .strip_prefix("ggml-")
                .and_then(|name| name.strip_suffix(".bin"))
                .map(str::to_owned);
            SttModelInfo {
                id: descriptor.id.into_inner(),
                name: descriptor.display_name.to_owned(),
                backend: "whisper.cpp".to_owned(),
                description: descriptor.description.to_owned(),
                expected_ram: descriptor.expected_ram.to_owned(),
                accuracy_tier: descriptor.accuracy_guidance.to_owned(),
                speed_tier: descriptor.speed_guidance.to_owned(),
                local_path: None,
                install_status: ModelInstallStatus::NotInstalled,
                download_model,
            }
        })
        .collect::<Vec<_>>();
    // These entries remain solely for the private Phase 1 compatibility
    // bridge and existing configuration migration. They are intentionally
    // absent from the normalized UI/service catalog and provide no evidence
    // for a shipped runtime handler.
    models.extend([
        legacy_model(
            "vosk_small_en",
            "Vosk small English",
            "Vosk",
            "Offline Apache 2.0 English compatibility model.",
            "1 GB",
            "Basic",
            "Fast",
            "vosk-model-small-en-us-0.15",
        ),
        legacy_model(
            "faster_whisper_tiny_en",
            "faster-whisper tiny.en",
            "faster-whisper",
            "Legacy compatibility model retained for migration.",
            "1 GB",
            "Basic",
            "Fastest",
            "tiny.en",
        ),
        legacy_model(
            "faster_whisper_base_en",
            "faster-whisper base.en",
            "faster-whisper",
            "Legacy compatibility model retained for migration.",
            "1 GB",
            "Good",
            "Fast",
            "base.en",
        ),
        legacy_model(
            "faster_whisper_small_en_gpu",
            "faster-whisper small.en",
            "faster-whisper",
            "Legacy compatibility model retained for migration.",
            "1-2 GB",
            "Good",
            "Fast",
            "small.en",
        ),
        legacy_model(
            "faster_whisper_medium_en_gpu",
            "faster-whisper medium.en",
            "faster-whisper",
            "Legacy compatibility model retained for migration.",
            "3-6 GB",
            "High",
            "Medium",
            "medium.en",
        ),
        legacy_model(
            "faster_whisper_large_v3",
            "faster-whisper large-v3",
            "faster-whisper",
            "Legacy compatibility model retained for migration.",
            "5-10 GB",
            "Highest",
            "Slow",
            "large-v3",
        ),
        legacy_model(
            "faster_whisper_turbo",
            "faster-whisper turbo",
            "faster-whisper",
            "Legacy compatibility model retained for migration.",
            "4-8 GB",
            "High",
            "Fast",
            "turbo",
        ),
        legacy_model(
            "faster_whisper_distil_large_v3",
            "faster-whisper distil-large-v3",
            "faster-whisper",
            "Legacy compatibility model retained for migration.",
            "3-6 GB",
            "High",
            "Fast",
            "distil-large-v3",
        ),
        legacy_model(
            "sherpa_onnx_zipformer_small",
            "sherpa-onnx Zipformer Small",
            "sherpa-onnx",
            "Legacy offline compatibility model; not the streaming candidate.",
            "1-2 GB",
            "Good",
            "Fast",
            "sherpa-onnx-zipformer-small-en-2023-06-26",
        ),
        legacy_model(
            "moonshine",
            "Moonshine tiny English",
            "Moonshine",
            "Legacy compatibility model retained for migration.",
            "1-2 GB",
            "Good",
            "Fast",
            "sherpa-onnx-moonshine-tiny-en-quantized-2026-02-27",
        ),
        legacy_model(
            "parakeet_0_6b",
            "Parakeet Unified 0.6B int8",
            "Parakeet",
            "Legacy compatibility model retained for migration.",
            "2-4 GB",
            "High",
            "Medium",
            "sherpa-onnx-nemo-parakeet-unified-en-0.6b-int8-non-streaming",
        ),
    ]);
    models
}

#[allow(clippy::too_many_arguments)]
fn legacy_model(
    id: &str,
    name: &str,
    backend: &str,
    description: &str,
    expected_ram: &str,
    accuracy_tier: &str,
    speed_tier: &str,
    download_model: &str,
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
        download_model: Some(download_model.to_owned()),
    }
}

pub fn format_bytes(bytes: u64) -> String {
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
    fn vosk_download_url_uses_official_model_catalog_entry() {
        assert_eq!(
            vosk_model_download_url("vosk-model-small-en-us-0.15"),
            Some("https://alphacephei.com/vosk/models/vosk-model-small-en-us-0.15.zip")
        );
    }

    #[test]
    fn sherpa_family_download_urls_use_supported_model_archives() {
        assert_eq!(
            sherpa_model_download_url("sherpa-onnx-zipformer-small-en-2023-06-26"),
            Some(
                "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-zipformer-small-en-2023-06-26.tar.bz2"
            )
        );
        assert_eq!(
            sherpa_model_download_url("sherpa-onnx-moonshine-tiny-en-quantized-2026-02-27"),
            Some(
                "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-moonshine-tiny-en-quantized-2026-02-27.tar.bz2"
            )
        );
        assert_eq!(
            sherpa_model_download_url(
                "sherpa-onnx-nemo-parakeet-unified-en-0.6b-int8-non-streaming"
            ),
            Some(
                "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-nemo-parakeet-unified-en-0.6b-int8-non-streaming.tar.bz2"
            )
        );
    }

    #[test]
    fn install_status_labels_progress() {
        let status = ModelInstallStatus::Downloading {
            downloaded_bytes: 25,
            total_bytes: Some(100),
            bytes_per_second: None,
        };

        assert_eq!(status.label(), "Downloading 25%");
        assert!(ModelInstallStatus::Installed.is_runnable());
        assert!(!ModelInstallStatus::NotInstalled.is_runnable());
        assert_eq!(
            ModelInstallStatus::InstallingRuntime.label(),
            "Verifying and installing artifacts"
        );
    }

    #[test]
    fn install_status_labels_download_speed() {
        let status = ModelInstallStatus::Downloading {
            downloaded_bytes: 1024 * 1024,
            total_bytes: None,
            bytes_per_second: Some(2 * 1024 * 1024),
        };

        assert_eq!(status.label(), "Downloading 1 MB · 2 MB/s");
    }

    #[test]
    fn bundled_phase_supports_all_managed_runtime_backends() {
        assert!(backend_capabilities("whisper.cpp").runnable);
        assert!(backend_capabilities("faster-whisper").runnable);
        assert!(backend_capabilities("faster-whisper").supports_downloads);
        assert!(backend_capabilities("Vosk").runnable);
        assert!(backend_capabilities("Vosk").supports_downloads);
        assert!(backend_capabilities("sherpa-onnx").runnable);
        assert!(backend_capabilities("sherpa-onnx").supports_downloads);
        assert!(!backend_capabilities("sherpa-onnx").streaming);
        assert!(backend_capabilities("Moonshine").runnable);
        assert!(backend_capabilities("Moonshine").supports_downloads);
        assert!(backend_capabilities("Parakeet").runnable);
        assert!(backend_capabilities("Parakeet").supports_downloads);
    }
}
