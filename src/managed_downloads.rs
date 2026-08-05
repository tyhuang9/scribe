//! Manifest-driven download preparation for the normalized model catalog.
//!
//! Pre-revamp runner-specific download helpers were unreachable after Phase 9
//! switched the Models flow to pinned transactional artifacts. They were
//! removed in Phase 11; legacy configuration aliases and existing unmanaged
//! artifacts remain untouched.

use std::path::{Path, PathBuf};

use crate::config;
use crate::config::AppConfig;
use crate::disk_space::DiskSpacePreflight;
use crate::huggingface_catalog::TrustedArtifact;
use crate::installations::{
    DownloadedArtifact, InstallCancellation, InstallError, InstallProgress, PinnedArtifact,
    StagedRuntime, download_pinned_artifact, pinned_artifact_disk_space_preflight,
    stage_runtime_archive,
};
use crate::transcription::ModelId;

pub(crate) fn prepare_model(
    config: &AppConfig,
    model_id: &ModelId,
    cancellation: &InstallCancellation,
    progress: &dyn Fn(InstallProgress),
) -> Result<DownloadedArtifact, InstallError> {
    let artifact = normalized_model_download_spec(config, model_id)?;
    download_pinned_artifact(&artifact, cancellation, progress)
}

/// Reads the conservative free-space requirement for a normalized catalog
/// model without starting a download or changing any model state.
pub(crate) fn normalized_model_download_space_preflight(
    config: &AppConfig,
    model_id: &ModelId,
) -> Result<DiskSpacePreflight, InstallError> {
    let artifact = normalized_model_download_spec(config, model_id)?;
    pinned_artifact_disk_space_preflight(&artifact)
}

fn normalized_model_download_spec(
    config: &AppConfig,
    model_id: &ModelId,
) -> Result<PinnedArtifact, InstallError> {
    let model = config::configured_models(config)
        .into_iter()
        .find(|model| model.id == model_id.as_str())
        .ok_or_else(|| InstallError::Failed(format!("Unknown configured model: {model_id}")))?;
    let destination = config::downloaded_model_path(config, &model).ok_or_else(|| {
        InstallError::Failed("No model storage directory is configured.".to_owned())
    })?;
    let artifact = crate::model_catalog::runtime_model_manifest(model_id).ok_or_else(|| {
        InstallError::Failed("The model has no normalized pinned artifact manifest.".to_owned())
    })?;
    let url = crate::model_catalog::runtime_model_download_url(model_id)
        .ok_or_else(|| InstallError::Failed("The model has no pinned download URL.".to_owned()))?;
    Ok(PinnedArtifact {
        id: model_id.as_str().to_owned(),
        url,
        size_bytes: artifact.artifact_size_bytes,
        sha256: artifact.artifact_sha256.to_owned(),
        destination,
    })
}

/// Resolves a selected trusted GGUF artifact into the existing transactional
/// downloader. Callers receive no arbitrary URL input: the repository,
/// full revision, filename, size, and digest were accepted by the backend
/// catalog service before this function is reached.
pub(crate) fn prepare_trusted_gguf_model(
    config: &AppConfig,
    artifact: &TrustedArtifact,
    cancellation: &InstallCancellation,
    progress: &dyn Fn(InstallProgress),
) -> Result<DownloadedArtifact, InstallError> {
    let pinned = trusted_gguf_download_spec(config, artifact)?;
    download_pinned_artifact(&pinned, cancellation, progress)
}

/// Reads the conservative free-space requirement for a backend-validated
/// trusted GGUF artifact without starting a download or changing model state.
pub(crate) fn trusted_gguf_download_space_preflight(
    config: &AppConfig,
    artifact: &TrustedArtifact,
) -> Result<DiskSpacePreflight, InstallError> {
    let pinned = trusted_gguf_download_spec(config, artifact)?;
    pinned_artifact_disk_space_preflight(&pinned)
}

fn trusted_gguf_download_spec(
    config: &AppConfig,
    artifact: &TrustedArtifact,
) -> Result<PinnedArtifact, InstallError> {
    let (organization, repository) = artifact
        .model_id
        .split_once('/')
        .filter(|(organization, repository)| {
            *organization == "handy-computer"
                && is_safe_identifier(repository)
                && !repository.is_empty()
        })
        .ok_or_else(|| {
            InstallError::Failed("untrusted Hugging Face model identifier".to_owned())
        })?;
    if !is_full_revision(&artifact.revision)
        || !is_safe_relative_gguf(&artifact.filename)
        || artifact.size_bytes == 0
        || !is_sha256(&artifact.expected_sha256)
    {
        return Err(InstallError::Failed(
            "trusted Hugging Face artifact metadata failed validation".to_owned(),
        ));
    }
    let destination = config::model_storage_dir(config)
        .join("huggingface")
        .join(organization)
        .join(repository)
        .join(&artifact.revision)
        .join(
            config::managed_remote_model_id(
                &artifact.model_id,
                &artifact.revision,
                &artifact.filename,
            )
            .ok_or_else(|| {
                InstallError::Failed("trusted Hugging Face artifact identity is invalid".to_owned())
            })?,
        )
        .join(&artifact.filename);
    Ok(PinnedArtifact {
        id: format!(
            "hf:{}@{}:{}",
            artifact.model_id, artifact.revision, artifact.filename
        ),
        url: format!(
            "https://huggingface.co/{}/resolve/{}/{}",
            artifact.model_id, artifact.revision, artifact.filename
        ),
        size_bytes: artifact.size_bytes,
        sha256: artifact.expected_sha256.to_ascii_lowercase(),
        destination,
    })
}

fn is_safe_identifier(value: &str) -> bool {
    value
        .bytes()
        .next()
        .is_some_and(|byte| byte.is_ascii_alphanumeric())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn is_full_revision(value: &str) -> bool {
    value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn is_safe_relative_gguf(value: &str) -> bool {
    let path = Path::new(value);
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("gguf"))
        && !path.is_absolute()
        && path.components().all(|component| {
            matches!(component, std::path::Component::Normal(_))
                && component.as_os_str() != "."
                && component.as_os_str() != ".."
                && component
                    .as_os_str()
                    .to_str()
                    .is_some_and(is_safe_identifier)
        })
}

#[derive(Debug)]
pub(crate) struct PreparedRuntimeInstall {
    pub(crate) staged: StagedRuntime,
    pub(crate) installed_entrypoint: PathBuf,
    pub(crate) version: String,
    pub(crate) package_id: String,
    pub(crate) archive_sha256: String,
}

pub(crate) fn prepare_primary_runtime(
    target_root: &Path,
    cancellation: &InstallCancellation,
    progress: &dyn Fn(InstallProgress),
) -> Result<PreparedRuntimeInstall, InstallError> {
    let downloads = config::runtime_storage_dir().join(".downloads");
    let archive_path = downloads.join("whisper-cpp-v1.9.1-windows-x64-cpu.zip");
    let spec = crate::runtime_catalog::primary_runtime_install_spec(archive_path)
        .map_err(InstallError::Failed)?;
    let staged = stage_runtime_archive(
        &spec.archive,
        target_root,
        &spec.compatibility_entrypoint,
        cancellation,
        progress,
    )?;
    Ok(PreparedRuntimeInstall {
        installed_entrypoint: target_root.join(&spec.compatibility_entrypoint),
        staged,
        version: spec.version,
        package_id: spec.package_id,
        archive_sha256: spec.archive.artifact.sha256,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trusted_artifact() -> TrustedArtifact {
        TrustedArtifact {
            model_id: "handy-computer/whisper-tiny.en-gguf".to_owned(),
            revision: "becb8bcb804405dc97b380a523d9975888820986".to_owned(),
            filename: "whisper-tiny.en-Q4_K_M.gguf".to_owned(),
            size_bytes: 43_545_248,
            expected_sha256: "3bfa6200aa12a21409445401f7871b5c733546dc45a29eb4871fcb3c7954e08b"
                .to_owned(),
        }
    }

    #[test]
    fn trusted_gguf_download_uses_only_a_pinned_huggingface_resolution_url() {
        let mut config = AppConfig::default();
        config.general.model_storage_dir = PathBuf::from("C:/scribe-models");

        let spec = trusted_gguf_download_spec(&config, &trusted_artifact()).unwrap();

        assert_eq!(
            spec.url,
            "https://huggingface.co/handy-computer/whisper-tiny.en-gguf/resolve/becb8bcb804405dc97b380a523d9975888820986/whisper-tiny.en-Q4_K_M.gguf"
        );
        assert_eq!(
            spec.destination,
            PathBuf::from("C:/scribe-models")
                .join("huggingface")
                .join("handy-computer")
                .join("whisper-tiny.en-gguf")
                .join("becb8bcb804405dc97b380a523d9975888820986")
                .join(
                    config::managed_remote_model_id(
                        "handy-computer/whisper-tiny.en-gguf",
                        "becb8bcb804405dc97b380a523d9975888820986",
                        "whisper-tiny.en-Q4_K_M.gguf",
                    )
                    .unwrap()
                )
                .join("whisper-tiny.en-Q4_K_M.gguf")
        );
    }

    #[test]
    fn trusted_gguf_download_rejects_untrusted_or_unsafe_artifact_metadata() {
        for (model_id, revision, filename) in [
            (
                "other-org/model",
                "becb8bcb804405dc97b380a523d9975888820986",
                "model.gguf",
            ),
            ("handy-computer/model", "main", "model.gguf"),
            (
                "handy-computer/model",
                "becb8bcb804405dc97b380a523d9975888820986",
                "../model.gguf",
            ),
            (
                "handy-computer/model",
                "becb8bcb804405dc97b380a523d9975888820986",
                "model.bin",
            ),
            (
                "handy-computer/.",
                "becb8bcb804405dc97b380a523d9975888820986",
                "model.gguf",
            ),
            (
                "handy-computer/..",
                "becb8bcb804405dc97b380a523d9975888820986",
                "model.gguf",
            ),
        ] {
            let mut artifact = trusted_artifact();
            artifact.model_id = model_id.to_owned();
            artifact.revision = revision.to_owned();
            artifact.filename = filename.to_owned();
            assert!(trusted_gguf_download_spec(&AppConfig::default(), &artifact).is_err());
        }
    }
}
