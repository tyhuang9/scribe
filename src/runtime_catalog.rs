#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeviceSupport {
    CpuOnly,
    CpuAndGpu,
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
        model_install_supported: true,
        runtime_install_supported: true,
        transcription_supported: true,
        device_detection_supported: false,
        device_support: DeviceSupport::CpuAndGpu,
        runtime_storage_estimate: "~20 MB+",
        runtime_storage_detail: "~20 MB for the CPU runtime; CUDA bundles are larger",
        development_runtime: Some(DevelopmentRuntimeSpec {
            script_name: "bundle-whisper-runtime.sh",
            destination_env: "SCRIBE_RUNTIME_DEST",
            executable_relative_path: "bin/whisper-cli",
        }),
    },
    BackendSpec {
        backend: "Vosk",
        runtime_id: "vosk",
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
    model_artifact("whisper_cpp_tiny_en", "~75 MB", Some(75 * MIB)),
    model_artifact("whisper_cpp_base_en", "~150 MB", Some(150 * MIB)),
    model_artifact("whisper_cpp_small_en", "~470 MB", Some(470 * MIB)),
    model_artifact("whisper_cpp_medium_en", "~1.5 GB", Some(1536 * MIB)),
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

pub fn development_runtime_spec(runtime_id: &str) -> Option<DevelopmentRuntimeSpec> {
    backend_spec_for_runtime_id(runtime_id).and_then(|spec| spec.development_runtime)
}

pub fn model_artifact_spec(model_id: &str) -> Option<&'static ModelArtifactSpec> {
    MODEL_ARTIFACTS
        .iter()
        .find(|artifact| artifact.model_id == model_id)
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
}
