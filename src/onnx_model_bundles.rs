//! Private, exact ONNX model-bundle catalog and installation receipts.
//!
//! This module is deliberately below `TranscriptionService`. The embedded
//! manifest is the only authority allowed to initiate a remote installation;
//! installed receipts remain self-contained so retired bundles can still be
//! verified and opened without catalog or network access.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Component, Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::disk_space::{self, CanonicalTargetIdentity, DiskSpacePreflight};
use crate::installations::{
    BundleAssemblyFile, DirectoryReplacement, GeneratedBundleFile, InstallCancellation,
    InstallError, InstallProgress, PinnedArtifact, RuntimeFileSpec, StagedRuntime,
    directory_activation_rollback_root, discard_file_bundle_staging,
    discard_pinned_artifact_partial, download_pinned_artifact_for_target,
    path_entry_exists_no_follow, pinned_artifact_retained_partial, read_regular_file_no_follow,
    restore_interrupted_directory_replacement, retain_interrupted_directory_replacement,
    rollback_to_previous_runtime, stage_file_bundle_for_target, verify_runtime_tree,
};
use crate::onnx_worker::{OnnxFileRole, OnnxModelFamily, OnnxModelSpec};

const CATALOG_BYTES: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/resources/onnx-model-bundles-v1.json"
));
const CATALOG_SCHEMA_VERSION: u16 = 1;
const RECEIPT_SCHEMA_VERSION: u16 = 1;
const RECEIPT_FILE_NAME: &str = "install-receipt.json";
const NOTICE_FILE_NAME: &str = "NOTICE.txt";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct BundleCatalog {
    schema_version: u16,
    runtime: RuntimeEvidence,
    bundles: Vec<OnnxBundleManifest>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RuntimeEvidence {
    name: String,
    version: String,
    source_revision: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum BundleAvailability {
    Available,
    Unavailable,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum CompatibilityEvidence {
    Experimental,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum BundleFileRole {
    Model,
    Encoder,
    Decoder,
    Joiner,
    Tokens,
    Preprocessor,
    UncachedDecoder,
    CachedDecoder,
    MergedDecoder,
    License,
}

impl BundleFileRole {
    fn runtime_role(self) -> Option<OnnxFileRole> {
        Some(match self {
            Self::Model => OnnxFileRole::Model,
            Self::Encoder => OnnxFileRole::Encoder,
            Self::Decoder => OnnxFileRole::Decoder,
            Self::Joiner => OnnxFileRole::Joiner,
            Self::Tokens => OnnxFileRole::Tokens,
            Self::Preprocessor => OnnxFileRole::Preprocessor,
            Self::UncachedDecoder => OnnxFileRole::UncachedDecoder,
            Self::CachedDecoder => OnnxFileRole::CachedDecoder,
            Self::MergedDecoder => OnnxFileRole::MergedDecoder,
            Self::License => return None,
        })
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BundleFileManifest {
    pub(crate) role: BundleFileRole,
    pub(crate) path: PathBuf,
    pub(crate) size_bytes: u64,
    pub(crate) sha256: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct CapabilityEvidence {
    decode_mode: String,
    languages: Vec<String>,
    native_streaming: bool,
    notes: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct LicenseEvidence {
    spdx: String,
    copyright: String,
    source_repository: String,
    source_revision: Option<String>,
    notice: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct OnnxBundleManifest {
    pub(crate) id: String,
    pub(crate) availability: BundleAvailability,
    compatibility: CompatibilityEvidence,
    pub(crate) repository: String,
    pub(crate) revision: String,
    pub(crate) family: OnnxModelFamily,
    pub(crate) num_threads: u16,
    #[serde(default)]
    unavailable_reason: Option<String>,
    capability: CapabilityEvidence,
    license: LicenseEvidence,
    pub(crate) files: Vec<BundleFileManifest>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum ReceiptState {
    Verified,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct OnnxBundleReceipt {
    schema_version: u16,
    manifest_schema_version: u16,
    manifest_sha256: String,
    model_id: String,
    runtime: RuntimeEvidence,
    repository: String,
    revision: String,
    family: OnnxModelFamily,
    num_threads: u16,
    files: Vec<BundleFileManifest>,
    capability: CapabilityEvidence,
    license: LicenseEvidence,
    verified_at_unix_seconds: u64,
    state: ReceiptState,
}

fn failed(message: impl Into<String>) -> InstallError {
    InstallError::Failed(message.into())
}

fn parse_catalog(bytes: &[u8]) -> Result<BundleCatalog, InstallError> {
    let catalog: BundleCatalog = serde_json::from_slice(bytes)
        .map_err(|error| failed(format!("invalid embedded ONNX bundle catalog: {error}")))?;
    validate_catalog(&catalog)?;
    Ok(catalog)
}

fn catalog() -> &'static BundleCatalog {
    static CATALOG: OnceLock<BundleCatalog> = OnceLock::new();
    CATALOG.get_or_init(|| {
        parse_catalog(CATALOG_BYTES).expect("embedded ONNX bundle catalog must be valid")
    })
}

pub(crate) fn bundle_manifest(model_id: &str) -> Option<&'static OnnxBundleManifest> {
    catalog()
        .bundles
        .iter()
        .find(|bundle| bundle.id == model_id)
}

pub(crate) fn available_bundle_manifests() -> impl Iterator<Item = &'static OnnxBundleManifest> {
    catalog()
        .bundles
        .iter()
        .filter(|bundle| bundle.availability == BundleAvailability::Available)
}

fn validate_catalog(catalog: &BundleCatalog) -> Result<(), InstallError> {
    if catalog.schema_version != CATALOG_SCHEMA_VERSION {
        return Err(failed(format!(
            "unsupported ONNX bundle catalog schema {}",
            catalog.schema_version
        )));
    }
    validate_runtime_evidence(&catalog.runtime)?;
    let mut ids = HashSet::new();
    for bundle in &catalog.bundles {
        if !ids.insert(bundle.id.as_str()) {
            return Err(failed(format!(
                "ONNX bundle catalog repeats model id {}",
                bundle.id
            )));
        }
        validate_bundle_manifest(bundle)?;
    }
    Ok(())
}

fn validate_runtime_evidence(runtime: &RuntimeEvidence) -> Result<(), InstallError> {
    if runtime.name != "sherpa-onnx" || runtime.version != "1.13.5" {
        return Err(failed(
            "ONNX bundle runtime evidence is not sherpa-onnx 1.13.5",
        ));
    }
    validate_revision(&runtime.source_revision, "runtime source revision")
}

fn validate_bundle_manifest(bundle: &OnnxBundleManifest) -> Result<(), InstallError> {
    validate_stable_id(&bundle.id)?;
    validate_repository(&bundle.repository)?;
    validate_revision(&bundle.revision, "bundle revision")?;
    if bundle.num_threads == 0 || bundle.num_threads > 256 {
        return Err(failed(format!(
            "ONNX bundle {} has an invalid fixed thread count",
            bundle.id
        )));
    }
    if bundle.capability.languages.is_empty()
        || bundle
            .capability
            .languages
            .iter()
            .any(|language| language != "en")
        || bundle.capability.notes.trim().is_empty()
    {
        return Err(failed(format!(
            "ONNX bundle {} makes an unsupported language/capability claim",
            bundle.id
        )));
    }
    if bundle.license.spdx.trim().is_empty()
        || bundle.license.copyright.trim().is_empty()
        || bundle.license.notice.trim().is_empty()
    {
        return Err(failed(format!(
            "ONNX bundle {} has incomplete license evidence",
            bundle.id
        )));
    }
    validate_repository(&bundle.license.source_repository)?;
    if let Some(revision) = &bundle.license.source_revision {
        validate_revision(revision, "license source revision")?;
    }
    match bundle.availability {
        BundleAvailability::Available => {
            if bundle.files.is_empty() || bundle.unavailable_reason.is_some() {
                return Err(failed(format!(
                    "available ONNX bundle {} has no exact files or has an unavailable reason",
                    bundle.id
                )));
            }
        }
        BundleAvailability::Unavailable => {
            if !bundle.files.is_empty()
                || bundle
                    .unavailable_reason
                    .as_deref()
                    .is_none_or(|reason| reason.trim().is_empty())
            {
                return Err(failed(format!(
                    "unavailable ONNX bundle {} must have a reason and no files",
                    bundle.id
                )));
            }
            return Ok(());
        }
    }

    let mut paths = HashSet::new();
    let mut roles = BTreeSet::new();
    for file in &bundle.files {
        validate_relative_path(&file.path)?;
        if file.path == Path::new(RECEIPT_FILE_NAME) || file.path == Path::new(NOTICE_FILE_NAME) {
            return Err(failed(format!(
                "ONNX bundle {} reserves file path {}",
                bundle.id,
                file.path.display()
            )));
        }
        if file.size_bytes == 0 {
            return Err(failed(format!(
                "ONNX bundle {} file {} has zero size",
                bundle.id,
                file.path.display()
            )));
        }
        validate_sha256(&file.sha256)?;
        let normalized = file.path.to_string_lossy().to_ascii_lowercase();
        if !paths.insert(normalized) {
            return Err(failed(format!(
                "ONNX bundle {} repeats a case-insensitive path",
                bundle.id
            )));
        }
        if !roles.insert(file.role) {
            return Err(failed(format!(
                "ONNX bundle {} repeats file role {:?}",
                bundle.id, file.role
            )));
        }
    }
    validate_family_layout(bundle)
}

fn validate_family_layout(bundle: &OnnxBundleManifest) -> Result<(), InstallError> {
    let roles = bundle
        .files
        .iter()
        .filter_map(|file| file.role.runtime_role())
        .collect::<BTreeSet<_>>();
    let expected = match bundle.family {
        OnnxModelFamily::Moonshine => [
            OnnxFileRole::Encoder,
            OnnxFileRole::MergedDecoder,
            OnnxFileRole::Tokens,
        ]
        .into_iter()
        .collect(),
        OnnxModelFamily::NemoCtc => [OnnxFileRole::Model, OnnxFileRole::Tokens]
            .into_iter()
            .collect(),
        OnnxModelFamily::Canary => [
            OnnxFileRole::Encoder,
            OnnxFileRole::Decoder,
            OnnxFileRole::Tokens,
        ]
        .into_iter()
        .collect(),
        OnnxModelFamily::OfflineTransducer | OnnxModelFamily::OnlineTransducer => [
            OnnxFileRole::Encoder,
            OnnxFileRole::Decoder,
            OnnxFileRole::Joiner,
            OnnxFileRole::Tokens,
        ]
        .into_iter()
        .collect(),
    };
    if roles != expected {
        return Err(failed(format!(
            "ONNX bundle {} has the wrong typed role layout for {:?}",
            bundle.id, bundle.family
        )));
    }
    let expected_streaming = bundle.family == OnnxModelFamily::OnlineTransducer;
    if bundle.capability.native_streaming != expected_streaming {
        return Err(failed(format!(
            "ONNX bundle {} has inconsistent streaming evidence",
            bundle.id
        )));
    }
    Ok(())
}

fn validate_stable_id(value: &str) -> Result<(), InstallError> {
    let valid = !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        && !value.starts_with('-')
        && !value.ends_with('-');
    if valid {
        Ok(())
    } else {
        Err(failed("ONNX bundle id is not a safe stable identifier"))
    }
}

fn validate_repository(value: &str) -> Result<(), InstallError> {
    let mut parts = value.split('/');
    let valid_part = |part: &str| {
        !part.is_empty()
            && part.len() <= 96
            && part
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
            && part != "."
            && part != ".."
    };
    if parts.next().is_some_and(valid_part)
        && parts.next().is_some_and(valid_part)
        && parts.next().is_none()
    {
        Ok(())
    } else {
        Err(failed(format!(
            "unsafe Hugging Face repository identifier {value:?}"
        )))
    }
}

fn validate_revision(value: &str, label: &str) -> Result<(), InstallError> {
    if value.len() == 40
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        Ok(())
    } else {
        Err(failed(format!(
            "{label} must be a full lowercase Git commit"
        )))
    }
}

fn validate_sha256(value: &str) -> Result<(), InstallError> {
    if value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        Ok(())
    } else {
        Err(failed(
            "ONNX bundle SHA-256 must be 64 lowercase hex characters",
        ))
    }
}

fn validate_relative_path(path: &Path) -> Result<(), InstallError> {
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
        || path.to_string_lossy().contains('\0')
        || path.to_str().is_none()
    {
        return Err(failed(format!(
            "unsafe ONNX bundle relative path {}",
            path.display()
        )));
    }
    Ok(())
}

fn embedded_manifest_sha256() -> String {
    format!("{:x}", Sha256::digest(CATALOG_BYTES))
}

fn spec_from_parts(
    model_id: &str,
    root: PathBuf,
    family: OnnxModelFamily,
    num_threads: u16,
    files: &[BundleFileManifest],
) -> Result<OnnxModelSpec, InstallError> {
    let mut runtime_files = BTreeMap::new();
    for file in files {
        validate_relative_path(&file.path)?;
        if let Some(role) = file.role.runtime_role()
            && runtime_files.insert(role, file.path.clone()).is_some()
        {
            return Err(failed(format!(
                "ONNX receipt repeats runtime role {:?}",
                file.role
            )));
        }
    }
    Ok(OnnxModelSpec {
        id: model_id.to_owned(),
        root,
        family,
        files: runtime_files,
        num_threads,
    })
}

#[derive(Debug)]
struct BundleTargetGuard {
    identity: CanonicalTargetIdentity,
}

impl Drop for BundleTargetGuard {
    fn drop(&mut self) {
        active_bundle_targets()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&self.identity);
    }
}

fn active_bundle_targets() -> &'static Mutex<HashSet<CanonicalTargetIdentity>> {
    static TARGETS: OnceLock<Mutex<HashSet<CanonicalTargetIdentity>>> = OnceLock::new();
    TARGETS.get_or_init(|| Mutex::new(HashSet::new()))
}

fn acquire_bundle_target(target: &Path) -> Result<BundleTargetGuard, InstallError> {
    let identity = disk_space::canonical_target_identity(target).map_err(InstallError::Failed)?;
    let inserted = active_bundle_targets()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(identity.clone());
    if !inserted {
        return Err(failed(format!(
            "an ONNX bundle installation already owns target {}",
            target.display()
        )));
    }
    Ok(BundleTargetGuard { identity })
}

#[derive(Debug)]
pub(crate) struct StagedOnnxBundle {
    staged: StagedRuntime,
    receipt: OnnxBundleReceipt,
    spec: OnnxModelSpec,
    retain_previous: bool,
    target_guard: BundleTargetGuard,
}

impl StagedOnnxBundle {
    pub(crate) fn root(&self) -> &Path {
        &self.staged.root
    }

    pub(crate) fn receipt(&self) -> &OnnxBundleReceipt {
        &self.receipt
    }

    pub(crate) fn spec(&self) -> &OnnxModelSpec {
        &self.spec
    }

    pub(crate) fn activate(
        self,
        cancellation: &InstallCancellation,
    ) -> Result<ActivatedOnnxBundle, InstallError> {
        let (receipt, spec) = verified_receipt_at(&self.staged.root)?;
        if receipt != self.receipt || spec != self.spec {
            return Err(failed(
                "staged ONNX bundle changed after verification and before activation",
            ));
        }
        cancellation
            .try_commit_activation()
            .map_err(|state| match state {
                crate::installations::ActivationCommitError::Cancelled => failed(
                    "ONNX bundle activation was cancelled before its filesystem transaction began",
                ),
                crate::installations::ActivationCommitError::AlreadyCommitted => {
                    failed("ONNX bundle activation authorization was already consumed")
                }
            })?;
        let target_root = self.staged.target_root.clone();
        let replacement = self.staged.activate()?;
        let spec = spec_from_parts(
            &self.receipt.model_id,
            target_root,
            self.receipt.family,
            self.receipt.num_threads,
            &self.receipt.files,
        )?;
        Ok(ActivatedOnnxBundle {
            replacement,
            receipt: self.receipt,
            spec,
            retain_previous: self.retain_previous,
            target_guard: self.target_guard,
        })
    }
}

#[derive(Debug)]
pub(crate) struct ActivatedOnnxBundle {
    replacement: DirectoryReplacement,
    receipt: OnnxBundleReceipt,
    spec: OnnxModelSpec,
    retain_previous: bool,
    target_guard: BundleTargetGuard,
}

impl ActivatedOnnxBundle {
    pub(crate) fn receipt(&self) -> &OnnxBundleReceipt {
        &self.receipt
    }

    pub(crate) fn spec(&self) -> &OnnxModelSpec {
        &self.spec
    }

    pub(crate) fn commit(self) -> Result<(), InstallError> {
        let Self {
            replacement,
            target_guard,
            retain_previous,
            ..
        } = self;
        replacement.commit_with_previous_policy(retain_previous)?;
        drop(target_guard);
        Ok(())
    }

    pub(crate) fn rollback(self) -> Result<(), InstallError> {
        let Self {
            replacement,
            target_guard,
            ..
        } = self;
        replacement.rollback()?;
        drop(target_guard);
        Ok(())
    }
}

pub(crate) fn bundle_target_root(
    storage_root: &Path,
    model_id: &str,
) -> Result<PathBuf, InstallError> {
    validate_stable_id(model_id)?;
    Ok(storage_root.join(model_id))
}

fn bundle_download_root(storage_root: &Path, manifest: &OnnxBundleManifest) -> PathBuf {
    storage_root
        .join(".downloads")
        .join(&manifest.id)
        .join(&manifest.revision)
}

fn hugging_face_file_url(
    repository: &str,
    revision: &str,
    path: &Path,
) -> Result<String, InstallError> {
    validate_repository(repository)?;
    validate_revision(revision, "bundle revision")?;
    validate_relative_path(path)?;
    let relative = path
        .iter()
        .map(|component| component.to_str().expect("validated UTF-8 path"))
        .collect::<Vec<_>>()
        .join("/");
    let value = format!("https://huggingface.co/{repository}/resolve/{revision}/{relative}");
    let url = url::Url::parse(&value).map_err(|error| {
        failed(format!(
            "could not construct pinned Hugging Face URL: {error}"
        ))
    })?;
    if url.scheme() != "https"
        || url.host_str() != Some("huggingface.co")
        || url.port_or_known_default() != Some(443)
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(failed(
            "constructed Hugging Face URL violated the exact source policy",
        ));
    }
    Ok(value)
}

fn pinned_files(
    storage_root: &Path,
    manifest: &OnnxBundleManifest,
) -> Result<Vec<PinnedArtifact>, InstallError> {
    validate_bundle_manifest(manifest)?;
    if manifest.availability != BundleAvailability::Available {
        return Err(failed(format!(
            "ONNX bundle {} is unavailable: {}",
            manifest.id,
            manifest
                .unavailable_reason
                .as_deref()
                .unwrap_or("no exact artifacts are published")
        )));
    }
    let root = bundle_download_root(storage_root, manifest);
    manifest
        .files
        .iter()
        .map(|file| {
            Ok(PinnedArtifact {
                id: format!("{}:{:?}", manifest.id, file.role),
                url: hugging_face_file_url(&manifest.repository, &manifest.revision, &file.path)?,
                size_bytes: file.size_bytes,
                sha256: file.sha256.clone(),
                destination: root.join(&file.path),
            })
        })
        .collect()
}

fn receipt_for_manifest(
    manifest: &OnnxBundleManifest,
    verified_at_unix_seconds: u64,
) -> OnnxBundleReceipt {
    OnnxBundleReceipt {
        schema_version: RECEIPT_SCHEMA_VERSION,
        manifest_schema_version: CATALOG_SCHEMA_VERSION,
        manifest_sha256: embedded_manifest_sha256(),
        model_id: manifest.id.clone(),
        runtime: catalog().runtime.clone(),
        repository: manifest.repository.clone(),
        revision: manifest.revision.clone(),
        family: manifest.family,
        num_threads: manifest.num_threads,
        files: manifest.files.clone(),
        capability: manifest.capability.clone(),
        license: manifest.license.clone(),
        verified_at_unix_seconds,
        state: ReceiptState::Verified,
    }
}

fn notice_bytes(receipt: &OnnxBundleReceipt) -> Vec<u8> {
    format!(
        "Scribe ONNX model bundle\n\nModel: {}\nSource: https://huggingface.co/{}\nRevision: {}\nLicense: {}\nAttribution: {}\n\n{}\n",
        receipt.model_id,
        receipt.repository,
        receipt.revision,
        receipt.license.spdx,
        receipt.license.copyright,
        receipt.license.notice
    )
    .into_bytes()
}

fn receipt_bytes(receipt: &OnnxBundleReceipt) -> Result<Vec<u8>, InstallError> {
    let mut bytes = serde_json::to_vec_pretty(receipt)
        .map_err(|error| failed(format!("could not serialize ONNX bundle receipt: {error}")))?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub(crate) fn bundle_disk_space_preflight(
    model_id: &str,
    storage_root: &Path,
) -> Result<DiskSpacePreflight, InstallError> {
    let manifest = bundle_manifest(model_id)
        .ok_or_else(|| failed(format!("unknown internal ONNX bundle {model_id}")))?;
    let artifacts = pinned_files(storage_root, manifest)?;
    let mut additional = manifest.files.iter().try_fold(0_u64, |total, file| {
        total
            .checked_add(file.size_bytes)
            .ok_or_else(|| failed("ONNX bundle expanded-size requirement overflowed"))
    })?;
    for artifact in &artifacts {
        let partial =
            pinned_artifact_retained_partial(artifact)?.map_or(0, |partial| partial.bytes);
        let remaining = if partial > artifact.size_bytes {
            artifact.size_bytes
        } else {
            artifact.size_bytes - partial
        };
        additional = additional
            .checked_add(remaining)
            .ok_or_else(|| failed("ONNX bundle download-space requirement overflowed"))?;
    }
    let receipt = receipt_for_manifest(manifest, u64::MAX);
    additional = additional
        .checked_add(receipt_bytes(&receipt)?.len() as u64)
        .and_then(|total| total.checked_add(notice_bytes(&receipt).len() as u64))
        .ok_or_else(|| failed("ONNX bundle metadata-space requirement overflowed"))?;
    let target = bundle_target_root(storage_root, model_id)?;
    disk_space::preflight_download_destination(&target, additional).map_err(InstallError::Failed)
}

/// The sole production entry point that may contact Hugging Face for an ONNX
/// bundle. Catalog reads, installed receipt reads, startup resolution, and
/// rollback validation are all local-only operations.
pub(crate) fn stage_onnx_bundle_install(
    model_id: &str,
    storage_root: &Path,
    cancellation: &InstallCancellation,
    progress: &dyn Fn(InstallProgress),
) -> Result<StagedOnnxBundle, InstallError> {
    let manifest = bundle_manifest(model_id)
        .ok_or_else(|| failed(format!("unknown internal ONNX bundle {model_id}")))?;
    let target_root = bundle_target_root(storage_root, model_id)?;
    let target_guard = acquire_bundle_target(&target_root)?;
    let preflight = bundle_disk_space_preflight(model_id, storage_root)?;
    if !preflight.has_sufficient_space() {
        return Err(failed(format!(
            "insufficient free space on {}: {} bytes are available but {} bytes are required",
            preflight.volume, preflight.available_bytes, preflight.required_bytes
        )));
    }
    let artifacts = pinned_files(storage_root, manifest)?;
    let total_download_bytes = artifacts.iter().try_fold(0_u64, |total, artifact| {
        total
            .checked_add(artifact.size_bytes)
            .ok_or_else(|| failed("ONNX bundle download progress overflowed"))
    })?;
    let mut completed_before = 0_u64;
    let mut assembly_files = Vec::with_capacity(artifacts.len());
    for (artifact, file) in artifacts.iter().zip(&manifest.files) {
        let identity = disk_space::canonical_target_identity(&artifact.destination)
            .map_err(InstallError::Failed)?;
        let base = completed_before;
        let aggregate_progress = |event: InstallProgress| {
            progress(InstallProgress {
                stage: event.stage,
                completed_bytes: base.saturating_add(event.completed_bytes),
                total_bytes: total_download_bytes,
                bytes_per_second: event.bytes_per_second,
            });
        };
        let downloaded = download_pinned_artifact_for_target(
            artifact,
            &identity,
            cancellation,
            &aggregate_progress,
        )?;
        let destination = downloaded.destination.clone();
        downloaded.activate()?.commit()?;
        assembly_files.push(BundleAssemblyFile {
            source_path: destination,
            install_path: file.path.clone(),
            size_bytes: file.size_bytes,
            sha256: file.sha256.clone(),
        });
        completed_before = completed_before
            .checked_add(file.size_bytes)
            .ok_or_else(|| failed("ONNX bundle progress overflowed"))?;
    }
    let receipt = receipt_for_manifest(manifest, unix_seconds());
    let generated = vec![
        GeneratedBundleFile {
            install_path: PathBuf::from(RECEIPT_FILE_NAME),
            bytes: receipt_bytes(&receipt)?,
        },
        GeneratedBundleFile {
            install_path: PathBuf::from(NOTICE_FILE_NAME),
            bytes: notice_bytes(&receipt),
        },
    ];
    let retain_previous = target_root.is_dir() && verified_receipt_at(&target_root).is_ok();
    let staged = stage_file_bundle_for_target(
        &assembly_files,
        &generated,
        &target_root,
        cancellation,
        progress,
    )?;
    let (verified_receipt, spec) = verified_receipt_at(&staged.root)?;
    if verified_receipt != receipt || !receipt_matches_manifest(&verified_receipt, manifest) {
        return Err(failed(
            "fresh ONNX bundle receipt did not match its embedded manifest authority",
        ));
    }
    Ok(StagedOnnxBundle {
        staged,
        receipt,
        spec,
        retain_previous,
        target_guard,
    })
}

pub(crate) fn discard_onnx_bundle_partials(
    model_id: &str,
    storage_root: &Path,
) -> Result<u64, InstallError> {
    let manifest = bundle_manifest(model_id)
        .ok_or_else(|| failed(format!("unknown internal ONNX bundle {model_id}")))?;
    let mut discarded = 0_u64;
    for artifact in pinned_files(storage_root, manifest)? {
        if discard_pinned_artifact_partial(&artifact)? {
            discarded += 1;
        }
    }
    Ok(discarded)
}

fn validate_receipt(receipt: &OnnxBundleReceipt) -> Result<(), InstallError> {
    if receipt.schema_version != RECEIPT_SCHEMA_VERSION
        || receipt.manifest_schema_version != CATALOG_SCHEMA_VERSION
        || receipt.verified_at_unix_seconds == 0
        || receipt.state != ReceiptState::Verified
    {
        return Err(failed(
            "unsupported or incomplete ONNX bundle receipt state",
        ));
    }
    validate_sha256(&receipt.manifest_sha256)?;
    validate_runtime_evidence(&receipt.runtime)?;
    validate_bundle_manifest(&OnnxBundleManifest {
        id: receipt.model_id.clone(),
        availability: BundleAvailability::Available,
        compatibility: CompatibilityEvidence::Experimental,
        repository: receipt.repository.clone(),
        revision: receipt.revision.clone(),
        family: receipt.family,
        num_threads: receipt.num_threads,
        unavailable_reason: None,
        capability: receipt.capability.clone(),
        license: receipt.license.clone(),
        files: receipt.files.clone(),
    })
}

fn receipt_matches_manifest(receipt: &OnnxBundleReceipt, manifest: &OnnxBundleManifest) -> bool {
    receipt.manifest_schema_version == CATALOG_SCHEMA_VERSION
        && receipt.manifest_sha256 == embedded_manifest_sha256()
        && receipt.model_id == manifest.id
        && receipt.runtime == catalog().runtime
        && receipt.repository == manifest.repository
        && receipt.revision == manifest.revision
        && receipt.family == manifest.family
        && receipt.num_threads == manifest.num_threads
        && receipt.files == manifest.files
        && receipt.capability == manifest.capability
        && receipt.license == manifest.license
}

pub(crate) fn verified_receipt_at(
    root: &Path,
) -> Result<(OnnxBundleReceipt, OnnxModelSpec), InstallError> {
    const MAX_RECEIPT_BYTES: u64 = 256 * 1024;
    let receipt_path = root.join(RECEIPT_FILE_NAME);
    let bytes = read_regular_file_no_follow(&receipt_path, MAX_RECEIPT_BYTES)?;
    let receipt: OnnxBundleReceipt = serde_json::from_slice(&bytes)
        .map_err(|error| failed(format!("invalid ONNX bundle receipt: {error}")))?;
    validate_receipt(&receipt)?;
    let expected_notice = notice_bytes(&receipt);
    let mut exact_files = receipt
        .files
        .iter()
        .map(|file| RuntimeFileSpec {
            archive_path: file.path.clone(),
            install_path: file.path.clone(),
            size_bytes: file.size_bytes,
            sha256: file.sha256.clone(),
        })
        .collect::<Vec<_>>();
    exact_files.push(RuntimeFileSpec {
        archive_path: PathBuf::from(RECEIPT_FILE_NAME),
        install_path: PathBuf::from(RECEIPT_FILE_NAME),
        size_bytes: bytes.len() as u64,
        sha256: format!("{:x}", Sha256::digest(&bytes)),
    });
    exact_files.push(RuntimeFileSpec {
        archive_path: PathBuf::from(NOTICE_FILE_NAME),
        install_path: PathBuf::from(NOTICE_FILE_NAME),
        size_bytes: expected_notice.len() as u64,
        sha256: format!("{:x}", Sha256::digest(&expected_notice)),
    });
    verify_runtime_tree(root, &exact_files)?;
    let spec = spec_from_parts(
        &receipt.model_id,
        root.to_path_buf(),
        receipt.family,
        receipt.num_threads,
        &receipt.files,
    )?;
    spec.validate().map_err(|error| {
        failed(format!(
            "installed ONNX bundle receipt did not produce a valid runtime spec: {error:#}"
        ))
    })?;
    Ok((receipt, spec))
}

pub(crate) fn current_verified_receipt_at(
    model_id: &str,
    root: &Path,
) -> Result<(OnnxBundleReceipt, OnnxModelSpec), InstallError> {
    let manifest = bundle_manifest(model_id)
        .filter(|manifest| manifest.availability == BundleAvailability::Available)
        .ok_or_else(|| failed(format!("no current downloadable ONNX bundle {model_id}")))?;
    let (receipt, spec) = verified_receipt_at(root)?;
    if !receipt_matches_manifest(&receipt, manifest) {
        return Err(failed(format!(
            "installed ONNX bundle {model_id} does not match the current embedded manifest"
        )));
    }
    Ok((receipt, spec))
}

pub(crate) fn rollback_to_previous_onnx_bundle(target_root: &Path) -> Result<bool, InstallError> {
    let previous = crate::installations::previous_runtime_root(target_root);
    if !previous.exists() {
        return Ok(false);
    }
    verified_receipt_at(&previous).map_err(|error| {
        failed(format!(
            "refusing to roll back to an invalid ONNX bundle at {}: {error}",
            previous.display()
        ))
    })?;
    rollback_to_previous_runtime(target_root)
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct OnnxBundleRecovery {
    pub(crate) restored_interrupted_previous: bool,
    pub(crate) retained_interrupted_previous: bool,
    pub(crate) discarded_incomplete_staging: bool,
}

/// Reconciles only transaction-owned directory names and never contacts the
/// network. Active or previous bundles are mutated only after their complete
/// self-contained receipts and exact trees verify.
pub(crate) fn recover_onnx_bundle_installation(
    target_root: &Path,
) -> Result<OnnxBundleRecovery, InstallError> {
    let _guard = acquire_bundle_target(target_root)?;
    let rollback = directory_activation_rollback_root(target_root);
    let mut recovery = OnnxBundleRecovery::default();
    if path_entry_exists_no_follow(&rollback)? {
        verified_receipt_at(&rollback).map_err(|error| {
            InstallError::RecoveryRequired(format!(
                "interrupted ONNX bundle rollback is not exact at {}: {error}",
                rollback.display()
            ))
        })?;
        if path_entry_exists_no_follow(target_root)? {
            verified_receipt_at(target_root).map_err(|error| {
                InstallError::RecoveryRequired(format!(
                    "interrupted ONNX bundle target is not exact at {}: {error}",
                    target_root.display()
                ))
            })?;
            retain_interrupted_directory_replacement(target_root)?;
            recovery.retained_interrupted_previous = true;
        } else {
            restore_interrupted_directory_replacement(target_root)?;
            recovery.restored_interrupted_previous = true;
        }
    }
    recovery.discarded_incomplete_staging = discard_file_bundle_staging(target_root)?;
    Ok(recovery)
}

#[cfg(test)]
pub(crate) fn write_test_receipt_for_spec(spec: &OnnxModelSpec) -> Result<(), InstallError> {
    let template = catalog()
        .bundles
        .iter()
        .find(|manifest| {
            manifest.availability == BundleAvailability::Available && manifest.family == spec.family
        })
        .ok_or_else(|| failed("no embedded bundle template for test ONNX family"))?;
    let mut manifest = template.clone();
    manifest.id = spec.id.clone();
    manifest.num_threads = spec.num_threads;
    manifest
        .files
        .retain(|file| file.role != BundleFileRole::License);
    for file in &mut manifest.files {
        let runtime_role = file
            .role
            .runtime_role()
            .ok_or_else(|| failed("test receipt unexpectedly retained a license role"))?;
        file.path = spec
            .files
            .get(&runtime_role)
            .cloned()
            .ok_or_else(|| failed("test ONNX spec is missing a template runtime role"))?;
        let bytes = read_regular_file_no_follow(&spec.root.join(&file.path), 16 * 1024 * 1024)?;
        file.size_bytes = bytes.len() as u64;
        file.sha256 = format!("{:x}", Sha256::digest(&bytes));
    }
    validate_bundle_manifest(&manifest)?;
    let receipt = receipt_for_manifest(&manifest, 1);
    std::fs::write(spec.root.join(RECEIPT_FILE_NAME), receipt_bytes(&receipt)?)
        .map_err(|error| failed(format!("failed to write test ONNX receipt: {error}")))?;
    std::fs::write(spec.root.join(NOTICE_FILE_NAME), notice_bytes(&receipt))
        .map_err(|error| failed(format!("failed to write test ONNX notice: {error}")))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn unique_root(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "scribe-onnx-bundle-{label}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ))
    }

    fn write_fixture_bundle(root: &Path, model_id: &str, marker: &str) -> OnnxBundleReceipt {
        fs::create_dir_all(root).unwrap();
        let mut manifest = bundle_manifest("moonshine-tiny-en-int8-onnx")
            .unwrap()
            .clone();
        manifest.id = model_id.to_owned();
        for (index, file) in manifest.files.iter_mut().enumerate() {
            let bytes = format!("{marker}-{index}-{:?}", file.role).into_bytes();
            file.size_bytes = bytes.len() as u64;
            file.sha256 = format!("{:x}", Sha256::digest(&bytes));
            fs::write(root.join(&file.path), bytes).unwrap();
        }
        let receipt = receipt_for_manifest(&manifest, 1);
        fs::write(
            root.join(RECEIPT_FILE_NAME),
            receipt_bytes(&receipt).unwrap(),
        )
        .unwrap();
        fs::write(root.join(NOTICE_FILE_NAME), notice_bytes(&receipt)).unwrap();
        receipt
    }

    fn fixture_assembly(root: &Path, receipt: &OnnxBundleReceipt) -> Vec<BundleAssemblyFile> {
        receipt
            .files
            .iter()
            .map(|file| BundleAssemblyFile {
                source_path: root.join(&file.path),
                install_path: file.path.clone(),
                size_bytes: file.size_bytes,
                sha256: file.sha256.clone(),
            })
            .collect()
    }

    fn fixture_generated(receipt: &OnnxBundleReceipt) -> Vec<GeneratedBundleFile> {
        vec![
            GeneratedBundleFile {
                install_path: PathBuf::from(RECEIPT_FILE_NAME),
                bytes: receipt_bytes(receipt).unwrap(),
            },
            GeneratedBundleFile {
                install_path: PathBuf::from(NOTICE_FILE_NAME),
                bytes: notice_bytes(receipt),
            },
        ]
    }

    #[test]
    fn embedded_catalog_has_exact_first_wave_evidence() {
        let catalog = parse_catalog(CATALOG_BYTES).unwrap();
        assert_eq!(catalog.bundles.len(), 4);
        assert_eq!(available_bundle_manifests().count(), 3);
        let moonshine = bundle_manifest("moonshine-tiny-en-int8-onnx").unwrap();
        assert_eq!(
            moonshine.revision,
            "d1e6c30921780b8508d04b492dfb3ce8a51605d4"
        );
        assert_eq!(moonshine.files.len(), 4);
        let parakeet = bundle_manifest("parakeet-tdt-ctc-110m-en-int8-onnx").unwrap();
        assert_eq!(parakeet.availability, BundleAvailability::Unavailable);
        assert!(parakeet.files.is_empty());
        let canary = bundle_manifest("canary-180m-flash-int8-onnx").unwrap();
        assert_eq!(canary.capability.languages, ["en"]);
        assert!(!canary.capability.native_streaming);
        let zipformer = bundle_manifest("zipformer-streaming-en-20m-int8-onnx").unwrap();
        assert!(zipformer.capability.native_streaming);
        assert_eq!(
            catalog.runtime.source_revision,
            "3dc7c569f31ca2cd4a20ed6f7db780327e6714c5"
        );
    }

    #[test]
    fn catalog_rejects_unsafe_and_ambiguous_authority() {
        let mut catalog = parse_catalog(CATALOG_BYTES).unwrap();
        catalog.bundles[0].repository = "owner/repo/extra".to_owned();
        assert!(validate_catalog(&catalog).is_err());
        let mut catalog = parse_catalog(CATALOG_BYTES).unwrap();
        catalog.bundles[0].revision = "main".to_owned();
        assert!(validate_catalog(&catalog).is_err());
        let mut catalog = parse_catalog(CATALOG_BYTES).unwrap();
        catalog.bundles[0].files[0].path = PathBuf::from("../escape.onnx");
        assert!(validate_catalog(&catalog).is_err());
        let mut catalog = parse_catalog(CATALOG_BYTES).unwrap();
        catalog.bundles[0].files[1].path = catalog.bundles[0].files[0].path.clone();
        assert!(validate_catalog(&catalog).is_err());
        let mut catalog = parse_catalog(CATALOG_BYTES).unwrap();
        catalog.bundles[0].files[0].sha256 = "A".repeat(64);
        assert!(validate_catalog(&catalog).is_err());
        let mut catalog = parse_catalog(CATALOG_BYTES).unwrap();
        catalog.bundles[0].files[0].size_bytes = 0;
        assert!(validate_catalog(&catalog).is_err());
    }

    #[test]
    fn license_role_is_never_exposed_to_the_runtime_spec() {
        let bundle = bundle_manifest("moonshine-tiny-en-int8-onnx").unwrap();
        let spec = spec_from_parts(
            &bundle.id,
            PathBuf::from("bundle"),
            bundle.family,
            bundle.num_threads,
            &bundle.files,
        )
        .unwrap();
        assert_eq!(spec.files.len(), 3);
        assert!(!spec.files.values().any(|path| path == Path::new("LICENSE")));
    }

    #[test]
    fn manifest_digest_is_stable_and_lowercase_sha256() {
        let digest = embedded_manifest_sha256();
        validate_sha256(&digest).unwrap();
    }

    #[test]
    fn exact_hugging_face_urls_are_commit_pinned_and_path_safe() {
        let manifest = bundle_manifest("canary-180m-flash-int8-onnx").unwrap();
        let url = hugging_face_file_url(
            &manifest.repository,
            &manifest.revision,
            &manifest.files[0].path,
        )
        .unwrap();
        assert_eq!(
            url,
            "https://huggingface.co/csukuangfj/sherpa-onnx-nemo-canary-180m-flash-en-es-de-fr-int8/resolve/9077164e0d3dd1d5353743e89ceaa1d3a770838c/encoder.int8.onnx"
        );
        assert!(hugging_face_file_url("owner/repo", "main", Path::new("model.onnx")).is_err());
        assert!(
            hugging_face_file_url(
                "owner/repo",
                "0123456789012345678901234567890123456789",
                Path::new("../model.onnx")
            )
            .is_err()
        );
    }

    #[test]
    fn self_contained_receipt_reconstructs_a_retired_bundle_without_catalog_authority() {
        let root = unique_root("retired-receipt");
        write_fixture_bundle(&root, "retired-moonshine-build", "retired");
        let (receipt, spec) = verified_receipt_at(&root).unwrap();
        assert_eq!(receipt.model_id, "retired-moonshine-build");
        assert_eq!(spec.id, "retired-moonshine-build");
        assert_eq!(spec.files.len(), 3);
        assert!(
            current_verified_receipt_at("moonshine-tiny-en-int8-onnx", &root).is_err(),
            "retired receipts remain readable but never become download authority"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn receipt_verification_rejects_extras_hash_changes_and_notice_changes() {
        let root = unique_root("receipt-negative");
        let receipt = write_fixture_bundle(&root, "fixture-moonshine", "exact");
        fs::write(root.join("unexpected.bin"), b"unexpected").unwrap();
        assert!(verified_receipt_at(&root).is_err());
        fs::remove_file(root.join("unexpected.bin")).unwrap();
        fs::write(root.join(&receipt.files[0].path), b"wrong").unwrap();
        assert!(verified_receipt_at(&root).is_err());
        let receipt = write_fixture_bundle(&root, "fixture-moonshine", "exact");
        fs::write(root.join(NOTICE_FILE_NAME), b"wrong notice").unwrap();
        assert!(verified_receipt_at(&root).is_err());
        assert!(!receipt.files.is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn receipt_parser_rejects_unknown_fields_and_unsafe_tuples() {
        let root = unique_root("receipt-schema");
        let receipt = write_fixture_bundle(&root, "fixture-moonshine", "schema");
        let mut value = serde_json::to_value(&receipt).unwrap();
        value
            .as_object_mut()
            .unwrap()
            .insert("future_authority".to_owned(), serde_json::json!(true));
        fs::write(
            root.join(RECEIPT_FILE_NAME),
            serde_json::to_vec_pretty(&value).unwrap(),
        )
        .unwrap();
        assert!(verified_receipt_at(&root).is_err());
        let mut receipt = receipt;
        receipt.files[0].path = PathBuf::from("../escape.onnx");
        fs::write(
            root.join(RECEIPT_FILE_NAME),
            receipt_bytes(&receipt).unwrap(),
        )
        .unwrap();
        assert!(verified_receipt_at(&root).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn same_target_installations_are_serialized_by_canonical_identity() {
        let root = unique_root("concurrency");
        fs::create_dir_all(&root).unwrap();
        let target = root.join("model");
        let first = acquire_bundle_target(&target).unwrap();
        assert!(acquire_bundle_target(&target).is_err());
        drop(first);
        assert!(acquire_bundle_target(&target).is_ok());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn fresh_assembly_is_cancel_safe_and_replaces_stale_staging_only() {
        let root = unique_root("cancel-assembly");
        let source = root.join("source");
        let target = root.join("target");
        let receipt = write_fixture_bundle(&source, "fixture-moonshine", "cancel");
        let stale = root.join(".target.installing");
        fs::create_dir_all(&stale).unwrap();
        fs::write(stale.join("stale"), b"stale").unwrap();
        let cancellation = InstallCancellation::default();
        cancellation.cancel();
        assert!(
            stage_file_bundle_for_target(
                &fixture_assembly(&source, &receipt),
                &fixture_generated(&receipt),
                &target,
                &cancellation,
                &|_| {},
            )
            .unwrap_err()
            .is_cancelled()
        );
        assert!(!stale.exists());
        assert!(!target.exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn directory_activation_can_commit_one_verified_previous_or_roll_back() {
        let root = unique_root("atomic");
        let target = root.join("target");
        let source = root.join("source");
        let old_receipt = write_fixture_bundle(&target, "old-moonshine", "old");
        let new_receipt = write_fixture_bundle(&source, "new-moonshine", "new");
        let staged = stage_file_bundle_for_target(
            &fixture_assembly(&source, &new_receipt),
            &fixture_generated(&new_receipt),
            &target,
            &InstallCancellation::default(),
            &|_| {},
        )
        .unwrap();
        let replacement = staged.activate().unwrap();
        assert_eq!(verified_receipt_at(&target).unwrap().0, new_receipt);
        replacement.rollback().unwrap();
        assert_eq!(verified_receipt_at(&target).unwrap().0, old_receipt);

        let staged = stage_file_bundle_for_target(
            &fixture_assembly(&source, &new_receipt),
            &fixture_generated(&new_receipt),
            &target,
            &InstallCancellation::default(),
            &|_| {},
        )
        .unwrap();
        staged
            .activate()
            .unwrap()
            .commit_with_previous_policy(true)
            .unwrap();
        assert_eq!(verified_receipt_at(&target).unwrap().0, new_receipt);
        assert_eq!(
            verified_receipt_at(&crate::installations::previous_runtime_root(&target))
                .unwrap()
                .0,
            old_receipt
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn crash_recovery_retains_or_restores_only_exact_directory_transactions() {
        let root = unique_root("crash-recovery");
        let target = root.join("target");
        let source = root.join("source");
        let old_receipt = write_fixture_bundle(&target, "old-moonshine", "old");
        let new_receipt = write_fixture_bundle(&source, "new-moonshine", "new");
        let staged = stage_file_bundle_for_target(
            &fixture_assembly(&source, &new_receipt),
            &fixture_generated(&new_receipt),
            &target,
            &InstallCancellation::default(),
            &|_| {},
        )
        .unwrap();
        drop(staged.activate().unwrap());
        let recovery = recover_onnx_bundle_installation(&target).unwrap();
        assert!(recovery.retained_interrupted_previous);
        assert_eq!(verified_receipt_at(&target).unwrap().0, new_receipt);
        assert_eq!(
            verified_receipt_at(&crate::installations::previous_runtime_root(&target))
                .unwrap()
                .0,
            old_receipt
        );

        let newer_source = root.join("newer-source");
        let newer_receipt = write_fixture_bundle(&newer_source, "newer-moonshine", "newer");
        let staged = stage_file_bundle_for_target(
            &fixture_assembly(&newer_source, &newer_receipt),
            &fixture_generated(&newer_receipt),
            &target,
            &InstallCancellation::default(),
            &|_| {},
        )
        .unwrap();
        drop(staged.activate().unwrap());
        fs::remove_dir_all(&target).unwrap();
        let incomplete = crate::installations::file_bundle_staging_root(&target).unwrap();
        fs::create_dir(&incomplete).unwrap();
        let recovery = recover_onnx_bundle_installation(&target).unwrap();
        assert!(recovery.restored_interrupted_previous);
        assert!(recovery.discarded_incomplete_staging);
        assert_eq!(verified_receipt_at(&target).unwrap().0, new_receipt);
        assert!(!incomplete.exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn crash_recovery_preserves_ambiguous_or_corrupt_state_for_operator_review() {
        let root = unique_root("crash-ambiguous");
        let target = root.join("target");
        let source = root.join("source");
        write_fixture_bundle(&target, "old-moonshine", "old");
        let new_receipt = write_fixture_bundle(&source, "new-moonshine", "new");
        let staged = stage_file_bundle_for_target(
            &fixture_assembly(&source, &new_receipt),
            &fixture_generated(&new_receipt),
            &target,
            &InstallCancellation::default(),
            &|_| {},
        )
        .unwrap();
        drop(staged.activate().unwrap());
        let rollback = directory_activation_rollback_root(&target);
        fs::write(rollback.join("unexpected"), b"corrupt").unwrap();
        assert!(
            recover_onnx_bundle_installation(&target)
                .unwrap_err()
                .requires_recovery()
        );
        assert!(target.exists());
        assert!(rollback.exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn local_catalog_and_receipt_paths_cannot_start_http() {
        let source = include_str!("onnx_model_bundles.rs");
        let production = source.split("\n#[cfg(test)]").next().unwrap();
        assert_eq!(
            production
                .matches("download_pinned_artifact_for_target(")
                .count(),
            1,
            "only the explicit stage_onnx_bundle_install path may invoke HTTP"
        );
        assert!(bundle_manifest("moonshine-tiny-en-int8-onnx").is_some());
    }

    #[test]
    fn parakeet_is_not_downloadable_while_its_pinned_repo_is_empty() {
        let root = unique_root("parakeet-unavailable");
        fs::create_dir_all(&root).unwrap();
        let error = pinned_files(
            &root,
            bundle_manifest("parakeet-tdt-ctc-110m-en-int8-onnx").unwrap(),
        )
        .unwrap_err();
        assert!(error.to_string().contains("unavailable"));
        assert!(!root.join(".downloads").exists());
        fs::remove_dir_all(root).unwrap();
    }
}
