use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeviceSupport {
    CpuOnly,
    CpuAndGpu,
}

impl DeviceSupport {
    pub fn supports_gpu(self) -> bool {
        matches!(self, Self::CpuAndGpu)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::CpuOnly => "CPU",
            Self::CpuAndGpu => "CPU/GPU",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DevelopmentRuntimeSpec {
    pub script_name: &'static str,
    pub destination_env: &'static str,
    pub executable_relative_path: &'static str,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BackendSpec {
    pub backend: &'static str,
    pub runtime_id: &'static str,
    pub runtime_version: Option<&'static str>,
    pub model_install_supported: bool,
    pub runtime_install_supported: bool,
    pub transcription_supported: bool,
    pub device_detection_supported: bool,
    pub device_support: DeviceSupport,
    pub runtime_storage_estimate: &'static str,
    pub runtime_storage_detail: &'static str,
    pub development_runtime: Option<DevelopmentRuntimeSpec>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ModelArtifactSpec {
    pub model_id: &'static str,
    pub storage_estimate: &'static str,
    pub download_bytes: Option<u64>,
    pub version: Option<&'static str>,
    pub sha256: Option<&'static str>,
}

const MIB: u64 = 1024 * 1024;
const GIB: u64 = 1024 * 1024 * 1024;

const BACKENDS: &[BackendSpec] = &[
    BackendSpec {
        backend: "whisper.cpp",
        runtime_id: "whisper_cpp",
        runtime_version: None,
        model_install_supported: true,
        runtime_install_supported: true,
        transcription_supported: true,
        device_detection_supported: false,
        device_support: DeviceSupport::CpuOnly,
        runtime_storage_estimate: "~20 MB+",
        runtime_storage_detail: "~20 MB for the verified CPU-only runtime package",
        development_runtime: Some(DevelopmentRuntimeSpec {
            script_name: "bundle-whisper-runtime.sh",
            destination_env: "SCRIBE_RUNTIME_DEST",
            executable_relative_path: "bin/whisper-cli",
        }),
    },
    BackendSpec {
        backend: "Vosk",
        runtime_id: "vosk",
        runtime_version: Some("0.3.45"),
        model_install_supported: true,
        runtime_install_supported: true,
        transcription_supported: true,
        device_detection_supported: false,
        device_support: DeviceSupport::CpuOnly,
        runtime_storage_estimate: "~20 MB+",
        runtime_storage_detail: "~20 MB for the pinned Python Vosk runtime",
        development_runtime: Some(DevelopmentRuntimeSpec {
            script_name: "bundle-vosk-runtime.sh",
            destination_env: "SCRIBE_VOSK_RUNTIME_DEST",
            executable_relative_path: "bin/scribe-vosk",
        }),
    },
    BackendSpec {
        backend: "sherpa-onnx",
        runtime_id: "sherpa_onnx",
        runtime_version: Some("1.13.3"),
        model_install_supported: true,
        runtime_install_supported: true,
        transcription_supported: true,
        device_detection_supported: false,
        device_support: DeviceSupport::CpuOnly,
        runtime_storage_estimate: "~100 MB+",
        runtime_storage_detail: "~100 MB+ for the sherpa-onnx Python runtime; model archives are separate",
        development_runtime: Some(DevelopmentRuntimeSpec {
            script_name: "bundle-sherpa-onnx-runtime.sh",
            destination_env: "SCRIBE_SHERPA_ONNX_RUNTIME_DEST",
            executable_relative_path: "bin/scribe-sherpa-onnx",
        }),
    },
    BackendSpec {
        backend: "faster-whisper",
        runtime_id: "faster_whisper",
        runtime_version: Some("1.2.1"),
        model_install_supported: true,
        runtime_install_supported: true,
        transcription_supported: true,
        device_detection_supported: false,
        device_support: DeviceSupport::CpuAndGpu,
        runtime_storage_estimate: "~450 MB+",
        runtime_storage_detail: "~450 MB for the CPU Python runtime; CUDA bundles are larger",
        development_runtime: Some(DevelopmentRuntimeSpec {
            script_name: "bundle-faster-whisper-runtime.sh",
            destination_env: "SCRIBE_FAST_WHISPER_RUNTIME_DEST",
            executable_relative_path: "bin/scribe-faster-whisper",
        }),
    },
    BackendSpec {
        backend: "Moonshine",
        runtime_id: "moonshine",
        runtime_version: Some("1.13.3"),
        model_install_supported: true,
        runtime_install_supported: true,
        transcription_supported: true,
        device_detection_supported: false,
        device_support: DeviceSupport::CpuOnly,
        runtime_storage_estimate: "~100 MB+",
        runtime_storage_detail: "~100 MB+ for the sherpa-onnx Python runtime; model archives are separate",
        development_runtime: Some(DevelopmentRuntimeSpec {
            script_name: "bundle-moonshine-runtime.sh",
            destination_env: "SCRIBE_MOONSHINE_RUNTIME_DEST",
            executable_relative_path: "bin/scribe-moonshine",
        }),
    },
    BackendSpec {
        backend: "Parakeet",
        runtime_id: "parakeet",
        runtime_version: Some("1.13.3"),
        model_install_supported: true,
        runtime_install_supported: true,
        transcription_supported: true,
        device_detection_supported: false,
        device_support: DeviceSupport::CpuOnly,
        runtime_storage_estimate: "~100 MB+",
        runtime_storage_detail: "~100 MB+ for the sherpa-onnx Python runtime; model archives are separate",
        development_runtime: Some(DevelopmentRuntimeSpec {
            script_name: "bundle-parakeet-runtime.sh",
            destination_env: "SCRIBE_PARAKEET_RUNTIME_DEST",
            executable_relative_path: "bin/scribe-parakeet",
        }),
    },
];

const MODEL_ARTIFACTS: &[ModelArtifactSpec] = &[
    model_artifact("faster_whisper_tiny_en", "~75 MB", Some(75 * MIB)),
    model_artifact("faster_whisper_base_en", "~150 MB", Some(150 * MIB)),
    model_artifact("faster_whisper_small_en_gpu", "~470 MB", Some(470 * MIB)),
    model_artifact("faster_whisper_medium_en_gpu", "~1.5 GB", Some(1536 * MIB)),
    model_artifact("faster_whisper_large_v3", "~3.1 GB", Some((31 * GIB) / 10)),
    model_artifact("faster_whisper_turbo", "~1.6 GB", Some((16 * GIB) / 10)),
    model_artifact(
        "faster_whisper_distil_large_v3",
        "~1.5 GB",
        Some(1536 * MIB),
    ),
    model_artifact("vosk_small_en", "~50 MB", Some(40 * MIB)),
    model_artifact("sherpa_onnx_zipformer_small", "~80 MB", Some(85 * MIB)),
    model_artifact("moonshine", "~35 MB", Some(35 * MIB)),
    model_artifact("parakeet_0_6b", "~640 MB", Some(650 * MIB)),
];

const fn model_artifact(
    model_id: &'static str,
    storage_estimate: &'static str,
    download_bytes: Option<u64>,
) -> ModelArtifactSpec {
    ModelArtifactSpec {
        model_id,
        storage_estimate,
        download_bytes,
        version: None,
        sha256: None,
    }
}

pub fn backend_specs() -> &'static [BackendSpec] {
    BACKENDS
}

pub fn backend_spec(backend: &str) -> Option<&'static BackendSpec> {
    BACKENDS.iter().find(|spec| spec.backend == backend)
}

pub fn backend_spec_for_runtime_id(runtime_id: &str) -> Option<&'static BackendSpec> {
    BACKENDS.iter().find(|spec| spec.runtime_id == runtime_id)
}

pub fn runtime_id_for_backend(backend: &str) -> String {
    backend_spec(backend)
        .map(|spec| spec.runtime_id.to_owned())
        .unwrap_or_else(|| slug_runtime_id(backend))
}

pub fn runtime_version_for_runtime_id(runtime_id: &str) -> Option<&'static str> {
    backend_spec_for_runtime_id(runtime_id).and_then(|spec| spec.runtime_version)
}

pub fn development_runtime_spec(runtime_id: &str) -> Option<DevelopmentRuntimeSpec> {
    backend_spec_for_runtime_id(runtime_id).and_then(|spec| spec.development_runtime)
}

/// Resolves a packaged runtime entrypoint using catalog data only. This keeps
/// UI/install code independent of concrete runtime modules; inference runtime
/// selection remains exclusively inside `RuntimeRouter`.
pub fn resolve_runtime_entrypoint(
    runtime_id: &str,
    roots: impl IntoIterator<Item = PathBuf>,
) -> Option<PathBuf> {
    let spec = development_runtime_spec(runtime_id)?;
    let relative = platform_executable_path(spec.executable_relative_path);
    let file_name = relative.file_name()?.to_owned();
    let mut seen = Vec::new();

    for root in roots {
        let candidates = if root.is_file() {
            vec![root]
        } else {
            vec![
                root.join("runtimes").join(runtime_id).join(&relative),
                root.join(&relative),
                root.join("bin").join(&file_name),
                root.join(&file_name),
            ]
        };
        for candidate in candidates {
            if !candidate.as_os_str().is_empty()
                && !seen.iter().any(|existing| existing == &candidate)
            {
                seen.push(candidate.clone());
                if runtime_entrypoint_is_usable(runtime_id, &candidate) {
                    return Some(candidate);
                }
            }
        }
    }
    None
}

fn runtime_entrypoint_is_usable(runtime_id: &str, path: &Path) -> bool {
    match runtime_id {
        "whisper_cpp" => path.is_file(),
        "faster_whisper" => crate::stt::faster_whisper::is_faster_whisper_runtime_usable(path),
        "vosk" => crate::stt::vosk::is_vosk_runtime_usable(path),
        "sherpa_onnx" | "moonshine" | "parakeet" => {
            crate::stt::sherpa_onnx::is_sherpa_family_runtime_usable(runtime_id, path)
        }
        _ => false,
    }
}

fn platform_executable_path(relative: &str) -> PathBuf {
    let mut path = Path::new(relative).to_path_buf();
    if cfg!(windows) && path.extension().is_none() {
        path.set_extension("exe");
    }
    path
}

pub fn model_artifact_spec(model_id: &str) -> Option<ModelArtifactSpec> {
    let normalized_id = crate::transcription::ModelId::new(model_id);
    if let Some(manifest) = crate::model_catalog::runtime_model_manifest(&normalized_id) {
        return Some(ModelArtifactSpec {
            model_id: manifest.id,
            storage_estimate: manifest.artifact_storage_estimate,
            download_bytes: Some(manifest.artifact_size_bytes),
            version: Some(manifest.artifact_revision),
            sha256: Some(manifest.artifact_sha256),
        });
    }
    MODEL_ARTIFACTS
        .iter()
        .find(|artifact| artifact.model_id == model_id)
        .copied()
}

pub fn model_storage_estimate(model_id: &str) -> &'static str {
    model_artifact_spec(model_id)
        .map(|artifact| artifact.storage_estimate)
        .unwrap_or("varies")
}

pub fn model_download_total_bytes(model_id: &str) -> Option<u64> {
    model_artifact_spec(model_id).and_then(|artifact| artifact.download_bytes)
}

fn slug_runtime_id(backend: &str) -> String {
    backend
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect::<String>()
        .split('_')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("_")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models;
    use std::collections::HashMap;

    #[test]
    fn catalog_backends_have_registry_specs() {
        for model in models::default_model_catalog() {
            assert!(
                backend_spec(&model.backend).is_some(),
                "missing backend spec for {}",
                model.backend
            );
        }
    }

    #[test]
    fn runtime_ids_stay_stable_for_known_backends() {
        assert_eq!(runtime_id_for_backend("whisper.cpp"), "whisper_cpp");
        assert_eq!(runtime_id_for_backend("sherpa-onnx"), "sherpa_onnx");
        assert_eq!(runtime_id_for_backend("faster-whisper"), "faster_whisper");
    }

    #[test]
    fn runtime_versions_follow_script_dependency_defaults() {
        let defaults = dependency_defaults();

        assert_eq!(
            runtime_version_for_runtime_id("faster_whisper"),
            defaults
                .get("SCRIBE_FASTER_WHISPER_VERSION_DEFAULT")
                .map(String::as_str)
        );
        assert_eq!(
            runtime_version_for_runtime_id("vosk"),
            defaults
                .get("SCRIBE_VOSK_VERSION_DEFAULT")
                .map(String::as_str)
        );
        for runtime_id in ["sherpa_onnx", "moonshine", "parakeet"] {
            assert_eq!(
                runtime_version_for_runtime_id(runtime_id),
                defaults
                    .get("SCRIBE_SHERPA_ONNX_VERSION_DEFAULT")
                    .map(String::as_str)
            );
        }
    }

    fn dependency_defaults() -> HashMap<String, String> {
        include_str!("../scripts/runtime-dependencies.env")
            .lines()
            .filter_map(|line| {
                let line = line.trim();
                if line.is_empty() || line.starts_with('#') {
                    return None;
                }
                let (key, value) = line.split_once('=')?;
                Some((key.to_owned(), value.to_owned()))
            })
            .collect()
    }

    #[test]
    fn model_artifact_specs_cover_managed_downloads() {
        for model in models::default_model_catalog()
            .into_iter()
            .filter(|model| model.download_model.is_some())
        {
            assert!(
                model_artifact_spec(&model.id).is_some(),
                "missing model artifact spec for {}",
                model.id
            );
        }
    }

    #[test]
    fn in_process_whisper_models_have_exact_pinned_artifacts() {
        for model_id in [
            "whisper_cpp_tiny_en",
            "whisper_cpp_base_en",
            "whisper_cpp_small_en",
            "whisper_cpp_medium_en",
        ] {
            let artifact = model_artifact_spec(model_id).unwrap();
            assert!(artifact.download_bytes.is_some());
            assert!(artifact.version.is_some());
            assert!(artifact.sha256.is_some_and(|hash| hash.len() == 64));
        }
        assert_eq!(
            backend_spec("whisper.cpp").unwrap().device_support,
            DeviceSupport::CpuOnly
        );
    }

    #[test]
    fn generic_runtime_entrypoint_resolution_uses_catalog_layout() {
        let root = std::env::temp_dir().join(format!(
            "scribe-runtime-catalog-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let entrypoint = root
            .join("runtimes")
            .join("whisper_cpp")
            .join(platform_executable_path("bin/whisper-cli"));
        std::fs::create_dir_all(entrypoint.parent().unwrap()).unwrap();
        std::fs::write(&entrypoint, b"runtime").unwrap();

        let resolved = resolve_runtime_entrypoint("whisper_cpp", [root.clone()]);
        std::fs::remove_dir_all(root).unwrap();

        assert_eq!(resolved, Some(entrypoint));
    }
}
