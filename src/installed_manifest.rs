//! Durable provenance for one verified, activated model artifact.
//!
//! The manifest is staged through the same replacement primitive as the model
//! file. It is therefore never a best-effort note that can silently disagree
//! with the active artifact after an interrupted install.

use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::installations::{DownloadedArtifact, FileReplacement, InstallError};
use crate::model_catalog::runtime_model_manifest;
use crate::transcription::{InstallSmoke, ModelId};

const SCHEMA_VERSION: u16 = 4;

/// Persistent, inspectable provenance for one active model file.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct InstalledModelManifest {
    pub(crate) schema_version: u16,
    pub(crate) model_id: String,
    pub(crate) source: ArtifactSource,
    pub(crate) local: LocalArtifact,
    pub(crate) runtime: RuntimeValidation,
    pub(crate) validated_at_unix_seconds: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct ArtifactSource {
    pub(crate) provenance: ArtifactProvenance,
    pub(crate) verification: ArtifactVerification,
    pub(crate) repository: String,
    pub(crate) revision: String,
    pub(crate) filename: String,
    pub(crate) expected_size_bytes: u64,
    pub(crate) expected_sha256: String,
}

/// Describes how Scribe learned the artifact facts. A local fingerprint is
/// intentionally not represented as a trusted upstream checksum.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ArtifactProvenance {
    NormalizedCatalog,
    TrustedHuggingFace,
    LocalImport,
}

/// States whether the source digest was independently pinned before download
/// or observed only after a user supplied a local file. Both values are
/// reverified before use; they have different trust meaning.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ArtifactVerification {
    PinnedSourceDigest,
    LocallyObservedFingerprint,
}

impl ArtifactSource {
    pub(crate) fn normalized(model_id: &ModelId) -> Result<Self, InstallError> {
        let artifact = runtime_model_manifest(model_id).ok_or_else(|| {
            InstallError::Failed(format!("unknown normalized model for manifest: {model_id}"))
        })?;
        Ok(Self {
            provenance: ArtifactProvenance::NormalizedCatalog,
            verification: ArtifactVerification::PinnedSourceDigest,
            repository: artifact.artifact_repository.to_owned(),
            revision: artifact.artifact_revision.to_owned(),
            filename: artifact.artifact_filename.to_owned(),
            expected_size_bytes: artifact.artifact_size_bytes,
            expected_sha256: artifact.artifact_sha256.to_owned(),
        })
    }

    pub(crate) fn trusted_gguf(
        repository: String,
        revision: String,
        filename: String,
        expected_size_bytes: u64,
        expected_sha256: String,
    ) -> Self {
        Self {
            provenance: ArtifactProvenance::TrustedHuggingFace,
            verification: ArtifactVerification::PinnedSourceDigest,
            repository,
            revision,
            filename,
            expected_size_bytes,
            expected_sha256: expected_sha256.to_ascii_lowercase(),
        }
    }

    pub(crate) fn local_import(
        filename: String,
        observed_size_bytes: u64,
        observed_sha256: String,
    ) -> Self {
        Self {
            provenance: ArtifactProvenance::LocalImport,
            verification: ArtifactVerification::LocallyObservedFingerprint,
            repository: "local-file".to_owned(),
            revision: "unversioned".to_owned(),
            filename,
            expected_size_bytes: observed_size_bytes,
            expected_sha256: observed_sha256.to_ascii_lowercase(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct LocalArtifact {
    pub(crate) path: PathBuf,
    pub(crate) size_bytes: u64,
    pub(crate) sha256: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct RuntimeValidation {
    pub(crate) implementation: String,
    pub(crate) version: String,
    pub(crate) package_free: bool,
    pub(crate) resolved_acceleration: crate::transcription::ResolvedAcceleration,
    /// `general.architecture`, read from the successfully loaded model.
    #[serde(default)]
    pub(crate) detected_architecture: String,
    /// Runtime-observed capabilities for this exact loaded artifact.
    #[serde(default)]
    pub(crate) capabilities: crate::transcription::RuntimeCapabilities,
    pub(crate) health_duration_ms: u128,
    pub(crate) load_duration_ms: u128,
    pub(crate) decode_duration_ms: u128,
    pub(crate) reload_duration_ms: u128,
}

pub(crate) fn manifest_path_for(model_path: &Path) -> PathBuf {
    let extension = model_path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or("model");
    model_path.with_extension(format!("{extension}.install-manifest.json"))
}

/// Imported source files are never modified. Their receipts live below
/// Scribe's model storage, not beside a user-owned GGUF.
pub(crate) fn imported_manifest_path_for(model_storage_dir: &Path, model_id: &ModelId) -> PathBuf {
    model_storage_dir
        .join("imported-receipts")
        .join(format!("{}.install-manifest.json", model_id.as_str()))
}

/// Returns the persisted runtime receipt only when it belongs to this exact
/// activated model. Older manifests remain usable artifacts, but do not carry
/// the v4 provenance and runtime-observed facts and therefore deliberately fall back to the
/// conservative runtime defaults.
pub(crate) fn runtime_validation_for(
    model_id: &ModelId,
    model_path: &Path,
) -> Result<Option<RuntimeValidation>, InstallError> {
    runtime_validation_at(model_id, model_path, &manifest_path_for(model_path), None)
}

pub(crate) fn imported_runtime_validation_for(
    model_id: &ModelId,
    model_path: &Path,
    model_storage_dir: &Path,
) -> Result<Option<RuntimeValidation>, InstallError> {
    runtime_validation_at(
        model_id,
        model_path,
        &imported_manifest_path_for(model_storage_dir, model_id),
        Some(ArtifactProvenance::LocalImport),
    )
}

fn runtime_validation_at(
    model_id: &ModelId,
    model_path: &Path,
    manifest_path: &Path,
    expected_provenance: Option<ArtifactProvenance>,
) -> Result<Option<RuntimeValidation>, InstallError> {
    if !manifest_path.exists() {
        return Ok(None);
    }
    let bytes = fs::read(manifest_path).map_err(|error| {
        InstallError::Failed(format!(
            "could not read installed manifest {}: {error}",
            manifest_path.display()
        ))
    })?;
    let manifest: InstalledModelManifest = serde_json::from_slice(&bytes).map_err(|error| {
        InstallError::Failed(format!(
            "could not parse installed manifest {}: {error}",
            manifest_path.display()
        ))
    })?;
    if manifest.schema_version != SCHEMA_VERSION
        || manifest.model_id != model_id.as_str()
        || expected_provenance.is_some_and(|provenance| manifest.source.provenance != provenance)
    {
        return Ok(None);
    }
    let canonical_path = fs::canonicalize(model_path).map_err(|error| {
        InstallError::Failed(format!(
            "could not canonicalize model {} while reading its manifest: {error}",
            model_path.display()
        ))
    })?;
    if manifest.local.path != canonical_path {
        return Ok(None);
    }
    Ok(Some(manifest.runtime))
}

pub(crate) fn build_manifest(
    model_id: &ModelId,
    source: ArtifactSource,
    package_free: bool,
    model_path: &Path,
    model_sha256: &str,
    smoke: &InstallSmoke,
) -> Result<InstalledModelManifest, InstallError> {
    let local_path = fs::canonicalize(model_path).map_err(|error| {
        InstallError::Failed(format!(
            "could not canonicalize activated model {}: {error}",
            model_path.display()
        ))
    })?;
    let metadata = fs::metadata(&local_path).map_err(|error| {
        InstallError::Failed(format!(
            "could not inspect activated model {}: {error}",
            local_path.display()
        ))
    })?;
    Ok(InstalledModelManifest {
        schema_version: SCHEMA_VERSION,
        model_id: model_id.as_str().to_owned(),
        source,
        local: LocalArtifact {
            path: local_path,
            size_bytes: metadata.len(),
            sha256: model_sha256.to_ascii_lowercase(),
        },
        runtime: RuntimeValidation {
            implementation: if package_free {
                "transcribe-cpp".to_owned()
            } else {
                "legacy-native-package".to_owned()
            },
            version: if package_free {
                crate::embedded_runtime::TRANSCRIBE_CPP_VERSION.to_owned()
            } else {
                "1.9.1".to_owned()
            },
            package_free,
            resolved_acceleration: smoke.resolved_acceleration.clone(),
            detected_architecture: smoke.detected_architecture.clone(),
            capabilities: smoke.capabilities.clone(),
            health_duration_ms: smoke.health_duration_ms,
            load_duration_ms: smoke.load_duration_ms,
            decode_duration_ms: smoke.decode_duration_ms,
            reload_duration_ms: smoke.reload_duration_ms,
        },
        validated_at_unix_seconds: unix_seconds(),
    })
}

/// Atomically activates the manifest and returns the same rollback/commit
/// handle used by a model file replacement.
pub(crate) fn stage_manifest(
    manifest: &InstalledModelManifest,
) -> Result<FileReplacement, InstallError> {
    stage_manifest_at(manifest, manifest_path_for(&manifest.local.path))
}

pub(crate) fn stage_manifest_at(
    manifest: &InstalledModelManifest,
    destination: PathBuf,
) -> Result<FileReplacement, InstallError> {
    let parent = destination.parent().ok_or_else(|| {
        InstallError::Failed(format!(
            "manifest path has no parent: {}",
            destination.display()
        ))
    })?;
    fs::create_dir_all(parent).map_err(|error| {
        InstallError::Failed(format!("failed to create {}: {error}", parent.display()))
    })?;
    let stage_path = destination.with_extension("install-manifest.staged");
    let bytes = serde_json::to_vec_pretty(manifest)
        .map_err(|error| InstallError::Failed(format!("failed to serialize manifest: {error}")))?;
    let mut file = File::create(&stage_path).map_err(|error| {
        InstallError::Failed(format!(
            "failed to create {}: {error}",
            stage_path.display()
        ))
    })?;
    file.write_all(&bytes).map_err(|error| {
        InstallError::Failed(format!("failed to write {}: {error}", stage_path.display()))
    })?;
    file.sync_all().map_err(|error| {
        InstallError::Failed(format!("failed to sync {}: {error}", stage_path.display()))
    })?;
    drop(file);
    let sha256 = format!("{:x}", Sha256::digest(&bytes));
    let target_identity =
        crate::disk_space::canonical_target_identity(&destination).map_err(InstallError::Failed)?;
    DownloadedArtifact {
        id: format!("installed-manifest:{}", manifest.model_id),
        path: stage_path,
        destination,
        size_bytes: bytes.len() as u64,
        sha256,
        target_identity,
    }
    .activate()
}

/// Persists an app-owned receipt with the same durable, private atomic writer
/// used for settings. Local imports call this only after their configuration
/// record is durable, so a crash cannot leave an undiscoverable staged receipt
/// or rollback file containing the external source path.
pub(crate) fn persist_manifest_at(
    manifest: &InstalledModelManifest,
    destination: &Path,
) -> Result<(), InstallError> {
    let bytes = serde_json::to_vec_pretty(manifest)
        .map_err(|error| InstallError::Failed(format!("failed to serialize manifest: {error}")))?;
    crate::config::settings::atomic_write_bytes(destination, &bytes).map_err(|error| {
        InstallError::Failed(format!(
            "failed to persist installed-model manifest {}: {error:#}",
            destination.display()
        ))
    })
}

fn unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transcription::{AccelerationPreference, ComputeDevice, ResolvedAcceleration};

    fn smoke() -> InstallSmoke {
        InstallSmoke {
            resolved_acceleration: ResolvedAcceleration {
                requested: AccelerationPreference::Cpu,
                resolved: ComputeDevice::Cpu,
                diagnostic: None,
                selection: None,
            },
            detected_architecture: "whisper".to_owned(),
            capabilities: crate::transcription::RuntimeCapabilities {
                cancellation: true,
                timestamps: true,
                supported_languages: vec!["en".to_owned()],
                ..Default::default()
            },
            health_duration_ms: 1,
            load_duration_ms: 2,
            decode_duration_ms: 3,
            reload_duration_ms: 4,
            cancellation_verified: true,
        }
    }

    #[test]
    fn gguf_manifest_records_pinned_source_and_safe_runtime_evidence() {
        let root =
            std::env::temp_dir().join(format!("scribe-installed-manifest-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let model_path = root.join("whisper-tiny.en-Q4_K_M.gguf");
        fs::write(&model_path, b"fixture").unwrap();

        let manifest = build_manifest(
            &ModelId::new("whisper_cpp_tiny_en"),
            ArtifactSource::normalized(&ModelId::new("whisper_cpp_tiny_en")).unwrap(),
            true,
            &model_path,
            "3bfa6200aa12a21409445401f7871b5c733546dc45a29eb4871fcb3c7954e08b",
            &smoke(),
        )
        .unwrap();

        assert_eq!(manifest.schema_version, SCHEMA_VERSION);
        assert!(manifest.runtime.package_free);
        assert_eq!(manifest.runtime.implementation, "transcribe-cpp");
        assert_eq!(manifest.runtime.detected_architecture, "whisper");
        assert!(manifest.runtime.capabilities.cancellation);
        assert!(manifest.runtime.capabilities.timestamps);
        assert_eq!(
            manifest.source.repository,
            "handy-computer/whisper-tiny.en-gguf"
        );
        assert_eq!(
            manifest.source.verification,
            ArtifactVerification::PinnedSourceDigest
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn staged_manifest_replaces_and_rolls_back_with_the_model_transaction() {
        let root = std::env::temp_dir().join(format!(
            "scribe-installed-manifest-stage-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let model_path = root.join("whisper-tiny.en-Q4_K_M.gguf");
        fs::write(&model_path, b"fixture").unwrap();
        let manifest = build_manifest(
            &ModelId::new("whisper_cpp_tiny_en"),
            ArtifactSource::normalized(&ModelId::new("whisper_cpp_tiny_en")).unwrap(),
            true,
            &model_path,
            "3bfa6200aa12a21409445401f7871b5c733546dc45a29eb4871fcb3c7954e08b",
            &smoke(),
        )
        .unwrap();
        let path = manifest_path_for(&model_path);
        fs::write(&path, b"old").unwrap();

        let replacement = stage_manifest(&manifest).unwrap();
        assert!(
            fs::read_to_string(&path)
                .unwrap()
                .contains("transcribe-cpp")
        );
        replacement.rollback().unwrap();

        assert_eq!(fs::read(&path).unwrap(), b"old");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn current_manifest_reloads_runtime_observations_only_for_its_model_and_path() {
        let root = std::env::temp_dir().join(format!(
            "scribe-installed-manifest-read-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let model_path = root.join("whisper-tiny.en-Q4_K_M.gguf");
        fs::write(&model_path, b"fixture").unwrap();
        let manifest = build_manifest(
            &ModelId::new("whisper_cpp_tiny_en"),
            ArtifactSource::normalized(&ModelId::new("whisper_cpp_tiny_en")).unwrap(),
            true,
            &model_path,
            "3bfa6200aa12a21409445401f7871b5c733546dc45a29eb4871fcb3c7954e08b",
            &smoke(),
        )
        .unwrap();
        stage_manifest(&manifest).unwrap().commit().unwrap();

        let runtime = runtime_validation_for(&ModelId::new("whisper_cpp_tiny_en"), &model_path)
            .unwrap()
            .expect("current matching manifest");
        assert_eq!(runtime.detected_architecture, "whisper");
        assert!(runtime.capabilities.timestamps);
        assert!(
            runtime_validation_for(&ModelId::new("other"), &model_path)
                .unwrap()
                .is_none()
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn remote_gguf_manifest_preserves_the_dynamic_pinned_source() {
        let root = std::env::temp_dir().join(format!(
            "scribe-installed-remote-manifest-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let model_path = root.join("example-Q4_K_M.gguf");
        fs::write(&model_path, b"fixture").unwrap();
        let sha256 = "a".repeat(64);

        let manifest = build_manifest(
            &ModelId::new("hf-example"),
            ArtifactSource::trusted_gguf(
                "handy-computer/example-asr-gguf".to_owned(),
                "0123456789abcdef0123456789abcdef01234567".to_owned(),
                "example-Q4_K_M.gguf".to_owned(),
                7,
                sha256.clone(),
            ),
            true,
            &model_path,
            &sha256,
            &smoke(),
        )
        .unwrap();

        assert!(manifest.runtime.package_free);
        assert_eq!(
            manifest.source.repository,
            "handy-computer/example-asr-gguf"
        );
        assert_eq!(
            manifest.source.revision,
            "0123456789abcdef0123456789abcdef01234567"
        );
        assert_eq!(manifest.source.filename, "example-Q4_K_M.gguf");
        assert_eq!(
            manifest.source.verification,
            ArtifactVerification::PinnedSourceDigest
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn local_import_receipt_is_app_owned_and_never_sidecars_the_source_file() {
        let root = std::env::temp_dir().join(format!(
            "scribe-installed-local-import-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        let storage = root.join("scribe-storage");
        let source_path = root.join("external").join("imported.gguf");
        fs::create_dir_all(source_path.parent().unwrap()).unwrap();
        fs::write(&source_path, b"fixture").unwrap();
        let model_id = ModelId::new("local-aaaaaaaaaaaaaaaaaaaaaaaa");
        let manifest = build_manifest(
            &model_id,
            ArtifactSource::local_import("imported.gguf".to_owned(), 7, "a".repeat(64)),
            true,
            &source_path,
            &"a".repeat(64),
            &smoke(),
        )
        .unwrap();
        let receipt_path = imported_manifest_path_for(&storage, &model_id);
        persist_manifest_at(&manifest, &receipt_path).unwrap();

        assert_eq!(manifest.source.provenance, ArtifactProvenance::LocalImport);
        assert_eq!(
            manifest.source.verification,
            ArtifactVerification::LocallyObservedFingerprint
        );
        assert_ne!(receipt_path, manifest_path_for(&source_path));
        assert!(receipt_path.starts_with(&storage));
        assert!(!manifest_path_for(&source_path).exists());
        assert!(
            !receipt_path
                .with_extension("install-manifest.staged")
                .exists()
        );
        assert!(!receipt_path.with_extension("rollback").exists());
        assert!(
            imported_runtime_validation_for(&model_id, &source_path, &storage)
                .unwrap()
                .is_some()
        );
        let _ = fs::remove_dir_all(root);
    }
}
