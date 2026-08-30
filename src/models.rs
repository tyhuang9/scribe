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
    #[serde(default)]
    pub artifact_origin: ModelArtifactOrigin,
    pub install_status: ModelInstallStatus,
    pub download_model: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub enum ModelArtifactOrigin {
    #[default]
    Catalog,
    Managed,
    Imported,
    External,
    Bundled,
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
    /// A user-initiated pause preserves resumable bytes without treating the
    /// expected cancellation as an installation failure.
    Paused {
        downloaded_bytes: u64,
        total_bytes: Option<u64>,
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
            Self::Paused { .. } => "Paused".to_owned(),
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
            Self::Error(message) => write!(f, "Error: {message}"),
        }
    }
}

pub fn default_model_catalog() -> Vec<SttModelInfo> {
    crate::model_catalog::model_descriptors()
        .into_iter()
        .map(|descriptor| {
            let (backend, download_model) =
                match crate::model_catalog::normalized_install_artifact(&descriptor.id)
                    .expect("every normalized descriptor must have an installation binding")
                {
                    crate::model_catalog::NormalizedInstallArtifact::SingleGguf(artifact) => {
                        ("whisper.cpp", Some(artifact.filename.to_owned()))
                    }
                    crate::model_catalog::NormalizedInstallArtifact::ReceiptBackedBundle {
                        bundle_id,
                        ..
                    } => ("sherpa-onnx", Some(bundle_id.to_owned())),
                };
            SttModelInfo {
                id: descriptor.id.into_inner(),
                name: descriptor.display_name.to_owned(),
                backend: backend.to_owned(),
                description: descriptor.description.to_owned(),
                expected_ram: descriptor.expected_ram.to_owned(),
                accuracy_tier: descriptor.accuracy_guidance.to_owned(),
                speed_tier: descriptor.speed_guidance.to_owned(),
                local_path: None,
                artifact_origin: ModelArtifactOrigin::Catalog,
                install_status: ModelInstallStatus::NotInstalled,
                download_model,
            }
        })
        .collect()
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
    fn default_catalog_projects_each_receipt_backed_onnx_model_once() {
        let catalog = default_model_catalog();
        for id in [
            "moonshine-tiny-en-int8-onnx",
            "moonshine-base-en-int8-onnx",
            "parakeet-tdt-06b-v2-en-int8-onnx",
        ] {
            let model = catalog.iter().find(|model| model.id == id).unwrap();
            assert_eq!(model.backend, "sherpa-onnx");
            assert_eq!(model.download_model.as_deref(), Some(id));
            assert_eq!(catalog.iter().filter(|model| model.id == id).count(), 1);
        }
        assert_eq!(catalog.len(), 7);
        assert_eq!(
            catalog.len(),
            crate::model_catalog::model_descriptors().len()
        );
    }

    #[test]
    fn retired_provider_ids_and_aliases_are_not_supported_catalog_entries() {
        let catalog = default_model_catalog();
        for retired in [
            "vosk_small_en",
            "faster_whisper_tiny_en",
            "faster_whisper_base_en",
            "faster_whisper_small_en_gpu",
            "faster_whisper_medium_en_gpu",
            "faster_whisper_large_v3",
            "faster_whisper_turbo",
            "faster_whisper_distil_large_v3",
            "sherpa_onnx_zipformer_small",
            "moonshine",
            "parakeet_0_6b",
            "faster_whisper",
            "faster_whisper_small_en",
            "faster_whisper_medium_en",
            "sherpa_onnx_streaming",
        ] {
            assert!(
                catalog.iter().all(|model| model.id != retired),
                "retired model or alias remains recognized: {retired}"
            );
        }
    }
}
