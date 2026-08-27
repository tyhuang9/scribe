use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::installations::{PinnedArtifact, RuntimeArchiveSpec, RuntimeFileSpec};
use crate::model_catalog::RuntimeRequirement;

const PRIMARY_RUNTIME_MANIFEST: &str =
    include_str!("../runtime-manifests/whisper-cpp-v1.9.1-windows-x64.json");
const PRIMARY_RUNTIME_ARCHIVE_URL: &str =
    "https://github.com/ggml-org/whisper.cpp/releases/download/v1.9.1/whisper-bin-x64.zip";
const PRIMARY_RUNTIME_ARCHIVE_SHA256: &str =
    "7d8be46ecd31828e1eb7a2ecdd0d6b314feafd82163038ab6092594b0a063539";

#[derive(Debug, Deserialize)]
struct RuntimePackageDocument {
    schema_version: u16,
    logical_runtime: String,
    package_id: String,
    platform_triple: String,
    archive_prefix: String,
    upstream: RuntimeUpstreamDocument,
    archive: RuntimeArchiveDocument,
    entrypoints: RuntimeEntrypointsDocument,
    files: Vec<RuntimeFileDocument>,
}

#[derive(Debug, Deserialize)]
struct RuntimeUpstreamDocument {
    tag: String,
    commit: String,
}

#[derive(Debug, Deserialize)]
struct RuntimeArchiveDocument {
    url: String,
    size_bytes: u64,
    sha256: String,
}

#[derive(Debug, Deserialize)]
struct RuntimeEntrypointsDocument {
    native_library: String,
    compatibility_cli: String,
}

#[derive(Debug, Deserialize)]
struct RuntimeFileDocument {
    path: String,
    size_bytes: u64,
    sha256: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RuntimePackageInstallSpec {
    pub(crate) requirement: RuntimeRequirement,
    pub(crate) package_id: String,
    pub(crate) version: String,
    pub(crate) commit: String,
    pub(crate) platform_triple: String,
    pub(crate) archive: RuntimeArchiveSpec,
    pub(crate) native_entrypoint: PathBuf,
    pub(crate) compatibility_entrypoint: PathBuf,
}

pub(crate) fn primary_runtime_install_spec(
    archive_destination: PathBuf,
) -> Result<RuntimePackageInstallSpec, String> {
    let document: RuntimePackageDocument = serde_json::from_str(PRIMARY_RUNTIME_MANIFEST)
        .map_err(|error| format!("invalid embedded primary runtime manifest: {error}"))?;
    validate_runtime_package(&document)?;
    let files = document
        .files
        .iter()
        .map(|file| {
            let installed = PathBuf::from(&file.path);
            let file_name = installed
                .file_name()
                .ok_or_else(|| format!("runtime file has no filename: {}", file.path))?;
            Ok(RuntimeFileSpec {
                archive_path: Path::new(&document.archive_prefix).join(file_name),
                install_path: installed,
                size_bytes: file.size_bytes,
                sha256: file.sha256.clone(),
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    Ok(RuntimePackageInstallSpec {
        requirement: RuntimeRequirement::PrimaryNative,
        package_id: document.package_id.clone(),
        version: document.upstream.tag,
        commit: document.upstream.commit,
        platform_triple: document.platform_triple,
        archive: RuntimeArchiveSpec {
            package_id: document.package_id.clone(),
            artifact: PinnedArtifact {
                id: document.package_id,
                url: document.archive.url,
                size_bytes: document.archive.size_bytes,
                sha256: document.archive.sha256,
                destination: archive_destination,
            },
            manifest_json: PRIMARY_RUNTIME_MANIFEST.to_owned(),
            files,
        },
        native_entrypoint: PathBuf::from(document.entrypoints.native_library),
        compatibility_entrypoint: PathBuf::from(document.entrypoints.compatibility_cli),
    })
}

fn validate_runtime_package(document: &RuntimePackageDocument) -> Result<(), String> {
    let expected_platform = if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        "x86_64-pc-windows-msvc"
    } else {
        return Err(
            "the pinned primary runtime package is available only for Windows x64".to_owned(),
        );
    };
    let safe_relative = |value: &str| {
        let path = Path::new(value);
        !value.is_empty()
            && !path.is_absolute()
            && path
                .components()
                .all(|component| matches!(component, std::path::Component::Normal(_)))
    };
    let valid_hash =
        |value: &str| value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit());
    if document.schema_version != 1
        || document.logical_runtime != "transcribe-cpp"
        || document.package_id != "whisper-cpp-v1.9.1-windows-x64-cpu"
        || document.platform_triple != expected_platform
        || document.archive_prefix != "Release"
        || document.upstream.tag != "v1.9.1"
        || document.upstream.commit != "f049fff95a089aa9969deb009cdd4892b3e74916"
        || document.archive.url != PRIMARY_RUNTIME_ARCHIVE_URL
        || document.archive.size_bytes != 7_982_101
        || !document
            .archive
            .sha256
            .eq_ignore_ascii_case(PRIMARY_RUNTIME_ARCHIVE_SHA256)
        || !safe_relative(&document.entrypoints.native_library)
        || !safe_relative(&document.entrypoints.compatibility_cli)
        || document.files.is_empty()
        || document.files.len() > 64
    {
        return Err(
            "embedded primary runtime manifest violates the pinned package policy".to_owned(),
        );
    }
    let mut paths = std::collections::HashSet::new();
    for file in &document.files {
        if !safe_relative(&file.path)
            || !file.path.starts_with("bin/")
            || file.size_bytes == 0
            || !valid_hash(&file.sha256)
            || !paths.insert(file.path.to_ascii_lowercase())
        {
            return Err(format!("invalid or duplicate runtime file: {}", file.path));
        }
    }
    for entrypoint in [
        &document.entrypoints.native_library,
        &document.entrypoints.compatibility_cli,
    ] {
        if !document.files.iter().any(|file| &file.path == entrypoint) {
            return Err(format!(
                "runtime entrypoint is not allowlisted: {entrypoint}"
            ));
        }
    }
    Ok(())
}

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

pub(crate) fn runtime_entrypoint_is_usable(runtime_id: &str, path: &Path) -> bool {
    crate::stt::runtime_entrypoint_is_usable(runtime_id, path)
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
    if let Some(crate::model_catalog::NormalizedInstallArtifact::ReceiptBackedBundle {
        bundle_id,
        aggregate_size_bytes,
    }) = crate::model_catalog::normalized_install_artifact(&normalized_id)
    {
        return Some(ModelArtifactSpec {
            model_id: bundle_id,
            storage_estimate: crate::model_catalog::normalized_model_storage_estimate(
                &normalized_id,
            )
            .expect("receipt-backed normalized model must have storage guidance"),
            download_bytes: Some(aggregate_size_bytes),
            version: None,
            sha256: None,
        });
    }
    MODEL_ARTIFACTS
        .iter()
        .find(|artifact| artifact.model_id == model_id)
        .copied()
}

#[cfg(test)]
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
    fn receipt_backed_moonshine_artifact_uses_its_aggregate_download_size() {
        let artifact = model_artifact_spec("moonshine-tiny-en-int8-onnx").unwrap();

        assert_eq!(artifact.model_id, "moonshine-tiny-en-int8-onnx");
        assert_eq!(artifact.storage_estimate, "~42 MB");
        assert_eq!(artifact.download_bytes, Some(44_256_550));
        assert_eq!(artifact.version, None);
        assert_eq!(artifact.sha256, None);
        assert_eq!(
            backend_spec("sherpa-onnx").unwrap().device_support,
            DeviceSupport::CpuOnly
        );
    }

    #[test]
    fn primary_runtime_package_is_an_exact_single_handler_allowlist() {
        if !cfg!(all(target_os = "windows", target_arch = "x86_64")) {
            assert!(primary_runtime_install_spec(PathBuf::from("runtime.zip")).is_err());
            return;
        }
        let package = primary_runtime_install_spec(PathBuf::from("runtime.zip")).unwrap();
        assert_eq!(package.requirement, RuntimeRequirement::PrimaryNative);
        assert_eq!(package.package_id, "whisper-cpp-v1.9.1-windows-x64-cpu");
        assert_eq!(package.version, "v1.9.1");
        assert_eq!(package.commit, "f049fff95a089aa9969deb009cdd4892b3e74916");
        assert_eq!(package.platform_triple, "x86_64-pc-windows-msvc");
        assert_eq!(package.native_entrypoint, Path::new("bin/whisper.dll"));
        assert_eq!(
            package.compatibility_entrypoint,
            Path::new("bin/whisper-cli.exe")
        );
        assert_eq!(package.archive.files.len(), 13);
        for file in &package.archive.files {
            assert!(file.install_path.starts_with("bin"));
            assert_eq!(
                file.archive_path,
                Path::new("Release").join(file.install_path.file_name().unwrap())
            );
        }
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
