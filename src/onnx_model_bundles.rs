//! Private, exact ONNX model-bundle catalog and installation receipts.
//!
//! This module is deliberately below `TranscriptionService`. The embedded
//! manifest is the only authority allowed to initiate a remote installation;
//! installed receipts remain self-contained so retired bundles can still be
//! verified and inventoried without catalog or network access. Executing or
//! activating a bundle additionally requires the current embedded catalog.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Seek, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::disk_space::{
    self, CanonicalTargetIdentity, DiskSpacePreflight, PhysicalVolumeIdentity,
};
use crate::installations::{
    BundleAssemblyFile, DirectoryReplacement, GeneratedBundleFile, InstallCancellation,
    InstallError, InstallProgress, PinnedArtifact, PinnedArtifactInspectionPlan, RuntimeFileSpec,
    StagedRuntime, discard_pinned_artifact_partial, download_pinned_artifact_for_target,
    inspect_pinned_artifact_for_target, pinned_artifact_retained_partial,
    read_regular_file_no_follow, stage_file_bundle_for_target, verify_regular_directory_root,
    verify_runtime_tree,
};
use crate::runtime_artifact::{OnnxFileRole, OnnxModelFamily, OnnxModelSpec};
use crate::transcription::{InstallSmoke, VerifiedOnnxBundleSmoke};

const CATALOG_BYTES: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/resources/onnx-model-bundles-v1.json"
));
const CATALOG_SCHEMA_VERSION: u16 = 1;
const RECEIPT_SCHEMA_VERSION: u16 = 1;
const RECEIPT_FILE_NAME: &str = "install-receipt.json";
const NOTICE_FILE_NAME: &str = "NOTICE.txt";
const LOCK_DIRECTORY_NAME: &str = ".onnx-bundle-locks";
const RESERVATION_CONTROL_DIRECTORY_NAME: &str = "onnx-bundle-volume-reservations";
const APACHE_2_LICENSE_BYTES: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/resources/licenses/Apache-2.0.txt"
));
const CC_BY_4_LICENSE_BYTES: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/resources/licenses/CC-BY-4.0.txt"
));
const MOONSHINE_MIT_LICENSE_BYTES: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/resources/licenses/Moonshine-MIT.txt"
));

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
    legal_url: String,
    changes_notice: String,
    notice: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct GeneratedLicenseFileEvidence {
    path: PathBuf,
    size_bytes: u64,
    sha256: String,
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
    generated_license_files: Vec<GeneratedLicenseFileEvidence>,
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

#[cfg(test)]
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
        || bundle.license.legal_url.trim().is_empty()
        || bundle.license.changes_notice.trim().is_empty()
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
    let legal_url = url::Url::parse(&bundle.license.legal_url)
        .map_err(|error| failed(format!("invalid license legal URL: {error}")))?;
    if legal_url.scheme() != "https"
        || legal_url.host_str().is_none()
        || legal_url.username() != ""
        || legal_url.password().is_some()
        || legal_url.query().is_some()
        || legal_url.fragment().is_some()
    {
        return Err(failed("license evidence requires an exact HTTPS legal URL"));
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
        if generated_license_materials(&bundle.license)
            .iter()
            .any(|material| material.install_path == file.path)
        {
            return Err(failed(format!(
                "ONNX bundle {} collides with generated license path {}",
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
    let expected_layouts: &[&[OnnxFileRole]] = match bundle.family {
        OnnxModelFamily::Moonshine => &[
            &[
                OnnxFileRole::Encoder,
                OnnxFileRole::MergedDecoder,
                OnnxFileRole::Tokens,
            ],
            &[
                OnnxFileRole::Preprocessor,
                OnnxFileRole::Encoder,
                OnnxFileRole::UncachedDecoder,
                OnnxFileRole::CachedDecoder,
                OnnxFileRole::Tokens,
            ],
        ],
        OnnxModelFamily::NemoCtc => &[&[OnnxFileRole::Model, OnnxFileRole::Tokens]],
        OnnxModelFamily::Canary => &[&[
            OnnxFileRole::Encoder,
            OnnxFileRole::Decoder,
            OnnxFileRole::Tokens,
        ]],
        OnnxModelFamily::OfflineTransducer | OnnxModelFamily::OnlineTransducer => &[&[
            OnnxFileRole::Encoder,
            OnnxFileRole::Decoder,
            OnnxFileRole::Joiner,
            OnnxFileRole::Tokens,
        ]],
    };
    if !expected_layouts
        .iter()
        .any(|expected| roles == expected.iter().copied().collect())
    {
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
    let rendered = path.to_string_lossy();
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
        || rendered.contains('\0')
        || rendered.contains('%')
        || rendered.contains('\\')
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
    _process_lock: OsFileLock,
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
    let storage_root = target
        .parent()
        .ok_or_else(|| failed("ONNX bundle target has no storage root"))?;
    let target_name = target
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| failed("ONNX bundle target has no safe lock name"))?;
    validate_stable_id(target_name)?;
    let lock_path = storage_root
        .join(LOCK_DIRECTORY_NAME)
        .join(format!("{target_name}.lock"));
    let process_lock = match OsFileLock::acquire(&lock_path, false) {
        Ok(lock) => lock,
        Err(error) => {
            active_bundle_targets()
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .remove(&identity);
            return Err(error);
        }
    };
    Ok(BundleTargetGuard {
        identity,
        _process_lock: process_lock,
    })
}

#[derive(Debug)]
struct OsFileLock {
    file: File,
}

impl OsFileLock {
    fn acquire(path: &Path, wait: bool) -> Result<Self, InstallError> {
        let parent = path
            .parent()
            .ok_or_else(|| failed(format!("lock path {} has no parent", path.display())))?;
        fs::create_dir_all(parent).map_err(|error| {
            failed(format!(
                "could not create ONNX bundle lock directory {}: {error}",
                parent.display()
            ))
        })?;
        verify_regular_directory_root(parent)?;
        let mut options = OpenOptions::new();
        options.create(true).read(true).write(true);
        configure_lock_no_follow(&mut options);
        let file = options
            .open(path)
            .map_err(|error| failed(format!("could not open lock {}: {error}", path.display())))?;
        verify_open_lock_file(&file, path)?;
        if !lock_file(&file, wait)
            .map_err(|error| failed(format!("could not lock {}: {error}", path.display())))?
        {
            return Err(failed(format!(
                "another process owns ONNX bundle lock {}",
                path.display()
            )));
        }
        Ok(Self { file })
    }
}

#[cfg(unix)]
fn configure_lock_no_follow(options: &mut OpenOptions) {
    use std::os::unix::fs::OpenOptionsExt;

    options.custom_flags(libc::O_NOFOLLOW);
}

#[cfg(windows)]
fn configure_lock_no_follow(options: &mut OpenOptions) {
    use std::os::windows::fs::OpenOptionsExt;

    options.custom_flags(windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT);
}

fn verify_open_lock_file(file: &File, path: &Path) -> Result<(), InstallError> {
    let metadata = file.metadata().map_err(|error| {
        failed(format!(
            "could not inspect ONNX bundle lock {}: {error}",
            path.display()
        ))
    })?;
    #[cfg(windows)]
    let is_link = {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0
    };
    #[cfg(not(windows))]
    let is_link = metadata.file_type().is_symlink();
    if !metadata.is_file() || is_link {
        return Err(failed(format!(
            "ONNX bundle lock is not a regular non-link file: {}",
            path.display()
        )));
    }
    Ok(())
}

impl Drop for OsFileLock {
    fn drop(&mut self) {
        let _ = unlock_file(&self.file);
    }
}

#[derive(Debug)]
struct BundleDiskReservation {
    remaining_bytes: u64,
    reservation_root: PathBuf,
    entry_path: PathBuf,
    entry_file: Option<File>,
    entry_lock: Option<OsFileLock>,
}

impl BundleDiskReservation {
    fn consume_allocated_bytes(&mut self, allocated_bytes: u64) -> Result<(), InstallError> {
        let remaining_bytes = self.remaining_bytes.checked_sub(allocated_bytes).ok_or_else(|| {
            failed(format!(
                "ONNX bundle reservation accounting attempted to consume {allocated_bytes} bytes from a {}-byte reservation",
                self.remaining_bytes
            ))
        })?;
        let _ledger = OsFileLock::acquire(&self.reservation_root.join("ledger.lock"), true)?;
        let entry_file = self.entry_file.as_mut().ok_or_else(|| {
            failed("ONNX bundle reservation was already released before allocation completed")
        })?;
        entry_file.set_len(0).map_err(|error| {
            failed(format!(
                "could not truncate ONNX bundle reservation {}: {error}",
                self.entry_path.display()
            ))
        })?;
        entry_file.rewind().map_err(|error| {
            failed(format!(
                "could not rewind ONNX bundle reservation {}: {error}",
                self.entry_path.display()
            ))
        })?;
        writeln!(entry_file, "{remaining_bytes}").map_err(|error| {
            failed(format!(
                "could not update ONNX bundle reservation {}: {error}",
                self.entry_path.display()
            ))
        })?;
        entry_file.flush().map_err(|error| {
            failed(format!(
                "could not flush ONNX bundle reservation {}: {error}",
                self.entry_path.display()
            ))
        })?;
        entry_file.sync_all().map_err(|error| {
            failed(format!(
                "could not sync ONNX bundle reservation {}: {error}",
                self.entry_path.display()
            ))
        })?;
        self.remaining_bytes = remaining_bytes;
        Ok(())
    }

    fn release(&mut self) -> Result<(), InstallError> {
        if self.entry_lock.is_none() {
            return Ok(());
        }
        let ledger_path = self.reservation_root.join("ledger.lock");
        let _ledger = OsFileLock::acquire(&ledger_path, false)?;
        self.entry_file.take();
        match fs::remove_file(&self.entry_path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(failed(format!(
                    "could not release ONNX bundle reservation {}: {error}",
                    self.entry_path.display()
                )));
            }
        }
        self.entry_lock.take();
        let lock_path = self.entry_path.with_extension("lock");
        match fs::remove_file(lock_path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(_) => {}
        }
        self.remaining_bytes = 0;
        Ok(())
    }
}

impl Drop for BundleDiskReservation {
    fn drop(&mut self) {
        // When the ledger is contended, releasing the held entry lock makes
        // this entry stale so the next ledger owner can prune it safely.
        let _ = self.release();
    }
}

fn acquire_bundle_disk_reservation(
    target: &Path,
    requested_bytes: u64,
) -> Result<BundleDiskReservation, InstallError> {
    let cache_root = crate::config::cache_dir().map_err(|error| {
        failed(format!(
            "could not resolve Scribe cache directory: {error:#}"
        ))
    })?;
    fs::create_dir_all(&cache_root).map_err(|error| {
        failed(format!(
            "could not create Scribe cache directory {}: {error}",
            cache_root.display()
        ))
    })?;
    verify_regular_directory_root(&cache_root)?;
    let control_root = cache_root.join(RESERVATION_CONTROL_DIRECTORY_NAME);
    acquire_bundle_disk_reservation_with_control_root(&control_root, target, requested_bytes)
}

fn volume_reservation_root(
    control_root: &Path,
    volume_identity: &PhysicalVolumeIdentity,
) -> PathBuf {
    let volume_key = format!("{:x}", Sha256::digest(volume_identity.key_material()));
    control_root.join(volume_key)
}

fn create_verified_reservation_directory(path: &Path) -> Result<(), InstallError> {
    let parent = path.parent().ok_or_else(|| {
        failed(format!(
            "ONNX reservation directory {} has no parent",
            path.display()
        ))
    })?;
    verify_regular_directory_root(parent)?;
    match fs::create_dir(path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => {
            return Err(failed(format!(
                "could not create ONNX bundle reservation directory {}: {error}",
                path.display()
            )));
        }
    }
    verify_regular_directory_root(path)
}

fn acquire_bundle_disk_reservation_with_control_root(
    control_root: &Path,
    target: &Path,
    requested_bytes: u64,
) -> Result<BundleDiskReservation, InstallError> {
    let initial_volume_identity =
        disk_space::physical_volume_identity(target).map_err(InstallError::Failed)?;
    create_verified_reservation_directory(control_root)?;
    let reservation_root = volume_reservation_root(control_root, &initial_volume_identity);
    create_verified_reservation_directory(&reservation_root)?;
    let _ledger = OsFileLock::acquire(&reservation_root.join("ledger.lock"), true)?;
    let mut active_reserved_bytes = 0_u64;
    for entry in fs::read_dir(&reservation_root).map_err(|error| {
        failed(format!(
            "could not inspect ONNX bundle reservations at {}: {error}",
            reservation_root.display()
        ))
    })? {
        let path = match entry {
            Ok(entry) => entry.path(),
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(failed(format!("could not read reservation entry: {error}"))),
        };
        if path.file_name().and_then(|name| name.to_str()) == Some("ledger.lock") {
            continue;
        }
        if path.extension().and_then(|extension| extension.to_str()) != Some("reservation") {
            continue;
        }
        let lock_path = path.with_extension("lock");
        let mut options = OpenOptions::new();
        options.create(true).read(true).write(true).truncate(false);
        configure_lock_no_follow(&mut options);
        let file = options.open(&lock_path).map_err(|error| {
            failed(format!(
                "could not open ONNX bundle reservation lock {}: {error}",
                lock_path.display()
            ))
        })?;
        verify_open_lock_file(&file, &lock_path)?;
        if lock_file(&file, false).map_err(|error| {
            failed(format!(
                "could not inspect ONNX bundle reservation lock {}: {error}",
                path.display()
            ))
        })? {
            let _ = fs::remove_file(&path);
            let _ = unlock_file(&file);
            let _ = fs::remove_file(&lock_path);
            continue;
        }
        match fs::symlink_metadata(&path) {
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(failed(format!(
                    "could not inspect active ONNX bundle reservation {}: {error}",
                    path.display()
                )));
            }
        }
        let bytes = read_regular_file_no_follow(&path, 64)?;
        let contents = std::str::from_utf8(&bytes).map_err(|_| {
            failed(format!(
                "active ONNX bundle reservation {} is not UTF-8",
                path.display()
            ))
        })?;
        let reserved = contents.trim().parse::<u64>().map_err(|_| {
            failed(format!(
                "active ONNX bundle reservation {} is invalid",
                path.display()
            ))
        })?;
        active_reserved_bytes = active_reserved_bytes
            .checked_add(reserved)
            .ok_or_else(|| failed("aggregate ONNX bundle reservation overflowed"))?;
    }
    let aggregate_request = active_reserved_bytes
        .checked_add(requested_bytes)
        .ok_or_else(|| failed("aggregate ONNX bundle disk requirement overflowed"))?;
    let preflight = disk_space::preflight_download_destination(target, aggregate_request)
        .map_err(InstallError::Failed)?;
    let admitted_volume_identity =
        disk_space::physical_volume_identity(target).map_err(InstallError::Failed)?;
    if admitted_volume_identity != initial_volume_identity {
        return Err(failed(
            "ONNX bundle target changed physical volume during reservation admission",
        ));
    }
    if !preflight.has_sufficient_space() {
        return Err(failed(format!(
            "insufficient unreserved free space on {}: {} bytes are available but {} bytes are required across active ONNX bundle installs",
            preflight.volume, preflight.available_bytes, preflight.required_bytes
        )));
    }
    let mut attempt = 0_u32;
    let (path, mut file) = loop {
        let path = reservation_root.join(format!(
            "{}-{}-{}.reservation",
            std::process::id(),
            unix_seconds(),
            attempt
        ));
        match OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .open(&path)
        {
            Ok(file) => break (path, file),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                attempt = attempt
                    .checked_add(1)
                    .ok_or_else(|| failed("could not allocate unique reservation name"))?;
            }
            Err(error) => {
                return Err(failed(format!(
                    "could not create ONNX bundle reservation {}: {error}",
                    path.display()
                )));
            }
        }
    };
    writeln!(file, "{requested_bytes}").map_err(|error| {
        failed(format!(
            "could not write ONNX bundle reservation {}: {error}",
            path.display()
        ))
    })?;
    file.flush().map_err(|error| {
        failed(format!(
            "could not flush ONNX bundle reservation {}: {error}",
            path.display()
        ))
    })?;
    file.sync_all().map_err(|error| {
        failed(format!(
            "could not sync ONNX bundle reservation {}: {error}",
            path.display()
        ))
    })?;
    let lock_path = path.with_extension("lock");
    let entry_lock = OsFileLock::acquire(&lock_path, false)?;
    Ok(BundleDiskReservation {
        remaining_bytes: requested_bytes,
        reservation_root,
        entry_path: path,
        entry_file: Some(file),
        entry_lock: Some(entry_lock),
    })
}

#[cfg(unix)]
fn lock_file(file: &File, wait: bool) -> io::Result<bool> {
    use std::os::fd::AsRawFd;

    let operation = libc::LOCK_EX | if wait { 0 } else { libc::LOCK_NB };
    let result = unsafe { libc::flock(file.as_raw_fd(), operation) };
    if result == 0 {
        Ok(true)
    } else {
        let error = io::Error::last_os_error();
        if !wait && error.kind() == io::ErrorKind::WouldBlock {
            Ok(false)
        } else {
            Err(error)
        }
    }
}

#[cfg(unix)]
fn unlock_file(file: &File) -> io::Result<()> {
    use std::os::fd::AsRawFd;

    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(windows)]
fn lock_file(file: &File, wait: bool) -> io::Result<bool> {
    use std::mem::zeroed;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::ERROR_LOCK_VIOLATION;
    use windows_sys::Win32::Storage::FileSystem::{
        LOCKFILE_EXCLUSIVE_LOCK, LOCKFILE_FAIL_IMMEDIATELY, LockFileEx,
    };
    use windows_sys::Win32::System::IO::OVERLAPPED;

    let mut overlapped: OVERLAPPED = unsafe { zeroed() };
    let flags = LOCKFILE_EXCLUSIVE_LOCK | if wait { 0 } else { LOCKFILE_FAIL_IMMEDIATELY };
    let result = unsafe { LockFileEx(file.as_raw_handle(), flags, 0, 1, 0, &mut overlapped) };
    if result != 0 {
        Ok(true)
    } else {
        let error = io::Error::last_os_error();
        if !wait && error.raw_os_error() == Some(ERROR_LOCK_VIOLATION as i32) {
            Ok(false)
        } else {
            Err(error)
        }
    }
}

#[cfg(windows)]
fn unlock_file(file: &File) -> io::Result<()> {
    use std::mem::zeroed;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::UnlockFileEx;
    use windows_sys::Win32::System::IO::OVERLAPPED;

    let mut overlapped: OVERLAPPED = unsafe { zeroed() };
    if unsafe { UnlockFileEx(file.as_raw_handle(), 0, 1, 0, &mut overlapped) } != 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
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

    pub(crate) fn bind_verified(
        self,
        witness: VerifiedOnnxBundleSmoke,
    ) -> Result<VerifiedStagedOnnxBundle, InstallError> {
        let (witness_root, witness_receipt, witness_spec, cancellation, smoke) =
            witness.into_parts();
        if witness_root != self.staged.root
            || witness_receipt != self.receipt
            || witness_spec != self.spec
        {
            return Err(failed(
                "ONNX smoke evidence does not match the exact staged receipt and spec",
            ));
        }
        Ok(VerifiedStagedOnnxBundle {
            staged: self,
            cancellation,
            smoke,
        })
    }
}

/// A staged bundle carrying single-use service verification evidence. Raw
/// staged bundles deliberately expose no activation operation.
#[derive(Debug)]
pub(crate) struct VerifiedStagedOnnxBundle {
    staged: StagedOnnxBundle,
    cancellation: InstallCancellation,
    smoke: InstallSmoke,
}

impl VerifiedStagedOnnxBundle {
    #[cfg(test)]
    pub(crate) fn root(&self) -> &Path {
        self.staged.root()
    }

    pub(crate) fn smoke(&self) -> &InstallSmoke {
        &self.smoke
    }

    pub(crate) fn activate(self) -> Result<ActivatedOnnxBundle, InstallError> {
        let Self {
            staged,
            cancellation,
            ..
        } = self;
        let (receipt, spec) = current_executable_receipt_at(&staged.staged.root)?;
        if receipt != staged.receipt || spec != staged.spec {
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
        let replacement = staged.staged.activate()?;
        Ok(ActivatedOnnxBundle {
            replacement,
            retain_previous: staged.retain_previous,
            target_guard: staged.target_guard,
        })
    }

    #[cfg(test)]
    pub(crate) fn discard(self) -> Result<(), InstallError> {
        crate::installations::discard_file_bundle_staging(&self.staged.staged.target_root)?;
        Ok(())
    }
}

#[derive(Debug)]
pub(crate) struct ActivatedOnnxBundle {
    replacement: DirectoryReplacement,
    retain_previous: bool,
    target_guard: BundleTargetGuard,
}

impl ActivatedOnnxBundle {
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
    let generated_license_files = generated_license_materials(&manifest.license)
        .into_iter()
        .map(|file| GeneratedLicenseFileEvidence {
            path: file.install_path,
            size_bytes: file.bytes.len() as u64,
            sha256: format!("{:x}", Sha256::digest(&file.bytes)),
        })
        .collect();
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
        generated_license_files,
        verified_at_unix_seconds,
        state: ReceiptState::Verified,
    }
}

fn generated_license_materials(license: &LicenseEvidence) -> Vec<GeneratedBundleFile> {
    let (path, bytes): (&str, &[u8]) = match license.spdx.as_str() {
        "Apache-2.0" => ("LICENSES/Apache-2.0.txt", APACHE_2_LICENSE_BYTES),
        "CC-BY-4.0" => ("LICENSES/CC-BY-4.0.txt", CC_BY_4_LICENSE_BYTES),
        "MIT" => ("LICENSES/Moonshine-MIT.txt", MOONSHINE_MIT_LICENSE_BYTES),
        _ => return Vec::new(),
    };
    vec![GeneratedBundleFile {
        install_path: PathBuf::from(path),
        bytes: bytes.to_vec(),
    }]
}

fn notice_bytes(receipt: &OnnxBundleReceipt) -> Vec<u8> {
    format!(
        "Scribe ONNX model bundle\n\nModel: {}\nSource: https://huggingface.co/{}\nRevision: {}\nLicense: {}\nLegal text: {}\nAttribution: {}\nChanges: {}\n\n{}\n",
        receipt.model_id,
        receipt.repository,
        receipt.revision,
        receipt.license.spdx,
        receipt.license.legal_url,
        receipt.license.copyright,
        receipt.license.changes_notice,
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
    // Browse-time preflight is deliberately metadata-only. Without an
    // installation cancellation handle it must not hash multi-gigabyte cache
    // entries; reserving every pinned download is conservative and local-only.
    let additional = bundle_required_install_bytes(
        manifest,
        artifacts.iter().map(|artifact| artifact.size_bytes),
    )?;
    let target = bundle_target_root(storage_root, model_id)?;
    disk_space::preflight_download_destination(&target, additional).map_err(InstallError::Failed)
}

pub(crate) fn bundle_download_size_bytes(model_id: &str) -> Result<u64, InstallError> {
    bundle_manifest(model_id)
        .ok_or_else(|| failed(format!("unknown internal ONNX bundle {model_id}")))?
        .files
        .iter()
        .try_fold(0_u64, |total, file| {
            total
                .checked_add(file.size_bytes)
                .ok_or_else(|| failed("ONNX bundle download-size total overflowed"))
        })
}

fn bundle_required_install_bytes(
    manifest: &OnnxBundleManifest,
    required_download_bytes: impl IntoIterator<Item = u64>,
) -> Result<u64, InstallError> {
    let mut additional = manifest.files.iter().try_fold(0_u64, |total, file| {
        total
            .checked_add(file.size_bytes)
            .ok_or_else(|| failed("ONNX bundle expanded-size requirement overflowed"))
    })?;
    for remaining in required_download_bytes {
        additional = additional
            .checked_add(remaining)
            .ok_or_else(|| failed("ONNX bundle download-space requirement overflowed"))?;
    }
    let receipt = receipt_for_manifest(manifest, u64::MAX);
    additional = additional
        .checked_add(receipt_bytes(&receipt)?.len() as u64)
        .and_then(|total| total.checked_add(notice_bytes(&receipt).len() as u64))
        .ok_or_else(|| failed("ONNX bundle metadata-space requirement overflowed"))?;
    for material in generated_license_materials(&receipt.license) {
        additional = additional
            .checked_add(material.bytes.len() as u64)
            .ok_or_else(|| failed("ONNX bundle license-space requirement overflowed"))?;
    }
    Ok(additional)
}

fn inspect_bundle_artifacts(
    artifacts: &[PinnedArtifact],
    cancellation: &InstallCancellation,
) -> Result<Vec<(CanonicalTargetIdentity, PinnedArtifactInspectionPlan)>, InstallError> {
    artifacts
        .iter()
        .map(|artifact| {
            let identity = disk_space::canonical_target_identity(&artifact.destination)
                .map_err(InstallError::Failed)?;
            let inspection = inspect_pinned_artifact_for_target(artifact, &identity, cancellation)?;
            Ok((identity, inspection))
        })
        .collect()
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
    if cancellation.is_cancelled() {
        return Err(InstallError::Cancelled {
            partial_path: storage_root.to_path_buf(),
            downloaded_bytes: 0,
        });
    }
    let manifest = bundle_manifest(model_id)
        .ok_or_else(|| failed(format!("unknown internal ONNX bundle {model_id}")))?;
    let target_root = bundle_target_root(storage_root, model_id)?;
    let artifacts = pinned_files(storage_root, manifest)?;
    let inspections = inspect_bundle_artifacts(&artifacts, cancellation)?;
    let required_bytes = bundle_required_install_bytes(
        manifest,
        inspections
            .iter()
            .map(|(_, inspection)| inspection.required_download_bytes()),
    )?;
    if cancellation.is_cancelled() {
        return Err(InstallError::Cancelled {
            partial_path: storage_root.to_path_buf(),
            downloaded_bytes: 0,
        });
    }
    let target_guard = acquire_bundle_target(&target_root)?;
    recover_onnx_bundle_installation_locked(&target_root)?;
    let mut disk_reservation = acquire_bundle_disk_reservation(&target_root, required_bytes)?;
    let total_download_bytes = artifacts.iter().try_fold(0_u64, |total, artifact| {
        total
            .checked_add(artifact.size_bytes)
            .ok_or_else(|| failed("ONNX bundle download progress overflowed"))
    })?;
    let mut completed_before = 0_u64;
    let mut assembly_files = Vec::with_capacity(artifacts.len());
    for ((artifact, file), (identity, inspection)) in
        artifacts.iter().zip(&manifest.files).zip(inspections)
    {
        let reserved_download_bytes = inspection.required_download_bytes();
        let base = completed_before;
        let aggregate_progress = |event: InstallProgress| {
            progress(InstallProgress {
                stage: event.stage,
                completed_bytes: base.saturating_add(event.completed_bytes),
                total_bytes: total_download_bytes,
                bytes_per_second: event.bytes_per_second,
                download_activity: event.download_activity,
            });
        };
        let downloaded = download_pinned_artifact_for_target(
            artifact,
            &identity,
            Some(inspection),
            cancellation,
            &aggregate_progress,
        )?;
        let destination = downloaded.destination.clone();
        downloaded.activate()?.commit()?;
        disk_reservation.consume_allocated_bytes(reserved_download_bytes)?;
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
    let mut generated = vec![
        GeneratedBundleFile {
            install_path: PathBuf::from(RECEIPT_FILE_NAME),
            bytes: receipt_bytes(&receipt)?,
        },
        GeneratedBundleFile {
            install_path: PathBuf::from(NOTICE_FILE_NAME),
            bytes: notice_bytes(&receipt),
        },
    ];
    generated.extend(generated_license_materials(&receipt.license));
    let retain_previous =
        target_root.is_dir() && current_executable_receipt_at(&target_root).is_ok();
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
    // Every future allocation covered by the ledger now exists on disk. Free
    // space reflects those bytes directly, so retaining the reservation while
    // this bundle waits for serialized verification would double-count it.
    disk_reservation.release()?;
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
    let target_root = bundle_target_root(storage_root, model_id)?;
    let _target_guard = acquire_bundle_target(&target_root)?;
    let mut discarded = 0_u64;
    if crate::installations::discard_file_bundle_staging(&target_root)? {
        discarded += 1;
    }
    for artifact in pinned_files(storage_root, manifest)? {
        if discard_pinned_artifact_partial(&artifact)? {
            discarded += 1;
        }
    }
    Ok(discarded)
}

pub(crate) fn retained_onnx_bundle_partial(
    model_id: &str,
    storage_root: &Path,
) -> Result<Option<crate::installations::RetainedPartial>, InstallError> {
    let manifest = bundle_manifest(model_id)
        .ok_or_else(|| failed(format!("unknown internal ONNX bundle {model_id}")))?;
    let bytes =
        pinned_files(storage_root, manifest)?
            .iter()
            .try_fold(0_u64, |total, artifact| {
                let bytes =
                    pinned_artifact_retained_partial(artifact)?.map_or(0, |partial| partial.bytes);
                total
                    .checked_add(bytes)
                    .ok_or_else(|| failed("ONNX retained partial size overflowed"))
            })?;
    Ok((bytes != 0).then_some(crate::installations::RetainedPartial { bytes }))
}

/// Pause is represented by the downloader's cancellation token. Any current
/// file is synced before return and its exact resumable partial is retained;
/// already completed revision-cache files are also left intact.
pub(crate) fn pause_onnx_bundle_install(cancellation: &InstallCancellation) {
    cancellation.cancel();
}

#[cfg(test)]
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct ReceiptVerificationStats {
    pub(crate) calls: usize,
    /// Sum of the declared bytes hashed by successful exact-tree verification.
    pub(crate) verified_bytes: u64,
    /// Samples are diagnostic evidence only; tests must not make timing claims.
    pub(crate) durations: Vec<std::time::Duration>,
}

#[cfg(test)]
thread_local! {
    static RECEIPT_VERIFICATION_OBSERVER: std::cell::RefCell<Option<ReceiptVerificationStats>> = const {
        std::cell::RefCell::new(None)
    };
}

#[cfg(test)]
pub(crate) fn observe_receipt_verifications_for_test<T>(
    operation: impl FnOnce() -> T,
) -> (T, ReceiptVerificationStats) {
    let observer = ReceiptVerificationObserver::install();
    let result = operation();
    let stats = observer.finish();
    (result, stats)
}

#[cfg(test)]
struct ReceiptVerificationObserver {
    previous: Option<ReceiptVerificationStats>,
    active: bool,
}

#[cfg(test)]
impl ReceiptVerificationObserver {
    fn install() -> Self {
        let previous = RECEIPT_VERIFICATION_OBSERVER
            .with(|observer| observer.replace(Some(ReceiptVerificationStats::default())));
        Self {
            previous,
            active: true,
        }
    }

    fn finish(mut self) -> ReceiptVerificationStats {
        let stats = self
            .restore()
            .expect("receipt verification observer remains installed");
        self.active = false;
        stats
    }

    fn restore(&mut self) -> Option<ReceiptVerificationStats> {
        RECEIPT_VERIFICATION_OBSERVER.with(|observer| observer.replace(self.previous.take()))
    }
}

#[cfg(test)]
impl Drop for ReceiptVerificationObserver {
    fn drop(&mut self) {
        if self.active {
            let _ = self.restore();
        }
    }
}

#[cfg(test)]
struct ReceiptVerificationSample {
    started: std::time::Instant,
    verified_bytes: u64,
}

#[cfg(test)]
impl ReceiptVerificationSample {
    fn new() -> Self {
        Self {
            started: std::time::Instant::now(),
            verified_bytes: 0,
        }
    }
}

#[cfg(test)]
impl Drop for ReceiptVerificationSample {
    fn drop(&mut self) {
        RECEIPT_VERIFICATION_OBSERVER.with(|observer| {
            if let Some(stats) = observer.borrow_mut().as_mut() {
                stats.calls += 1;
                stats.verified_bytes += self.verified_bytes;
                stats.durations.push(self.started.elapsed());
            }
        });
    }
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
    })?;
    let expected = generated_license_materials(&receipt.license)
        .into_iter()
        .map(|file| GeneratedLicenseFileEvidence {
            path: file.install_path,
            size_bytes: file.bytes.len() as u64,
            sha256: format!("{:x}", Sha256::digest(&file.bytes)),
        })
        .collect::<Vec<_>>();
    if receipt.generated_license_files != expected || expected.is_empty() {
        return Err(failed(
            "ONNX bundle receipt has incomplete generated license materials",
        ));
    }
    Ok(())
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
    #[cfg(test)]
    let mut verification_sample = ReceiptVerificationSample::new();
    verify_regular_directory_root(root)?;
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
    exact_files.extend(
        receipt
            .generated_license_files
            .iter()
            .map(|file| RuntimeFileSpec {
                archive_path: file.path.clone(),
                install_path: file.path.clone(),
                size_bytes: file.size_bytes,
                sha256: file.sha256.clone(),
            }),
    );
    exact_files.push(RuntimeFileSpec {
        archive_path: PathBuf::from(NOTICE_FILE_NAME),
        install_path: PathBuf::from(NOTICE_FILE_NAME),
        size_bytes: expected_notice.len() as u64,
        sha256: format!("{:x}", Sha256::digest(&expected_notice)),
    });
    verify_runtime_tree(root, &exact_files)?;
    #[cfg(test)]
    {
        verification_sample.verified_bytes = exact_files.iter().map(|file| file.size_bytes).sum();
    }
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

#[cfg(test)]
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

/// Verifies receipt integrity and then separately establishes executable
/// trust from the currently embedded, available manifest. Retired receipts
/// intentionally remain readable through `verified_receipt_at`, but cannot
/// cross this runtime boundary until a future signed catalog exists.
pub(crate) fn current_executable_receipt_at(
    root: &Path,
) -> Result<(OnnxBundleReceipt, OnnxModelSpec), InstallError> {
    let (receipt, spec) = verified_receipt_at(root)?;
    let manifest = bundle_manifest(&receipt.model_id)
        .filter(|manifest| manifest.availability == BundleAvailability::Available)
        .ok_or_else(|| {
            failed(format!(
                "ONNX bundle {} is not executable under the current embedded manifest",
                receipt.model_id
            ))
        })?;
    if !receipt_matches_manifest(&receipt, manifest) {
        return Err(failed(format!(
            "installed ONNX bundle {} does not match the current embedded manifest",
            receipt.model_id
        )));
    }
    Ok((receipt, spec))
}

#[cfg(test)]
pub(crate) fn current_executable_receipt_at_with_manifest_for_test(
    root: &Path,
    manifest: &OnnxBundleManifest,
) -> Result<(OnnxBundleReceipt, OnnxModelSpec), InstallError> {
    let (receipt, spec) = verified_receipt_at(root)?;
    if manifest.availability != BundleAvailability::Available
        || !receipt_matches_manifest(&receipt, manifest)
    {
        return Err(failed(format!(
            "installed ONNX bundle {} does not match the controlled current manifest",
            receipt.model_id
        )));
    }
    Ok((receipt, spec))
}

#[cfg(test)]
pub(crate) fn rollback_to_previous_onnx_bundle(target_root: &Path) -> Result<bool, InstallError> {
    let _target_guard = acquire_bundle_target(target_root)?;
    let previous = crate::installations::previous_runtime_root(target_root);
    if !previous.exists() {
        return Ok(false);
    }
    current_executable_receipt_at(&previous).map_err(|error| {
        failed(format!(
            "refusing to roll back to an invalid ONNX bundle at {}: {error}",
            previous.display()
        ))
    })?;
    crate::installations::rollback_to_previous_runtime(target_root)
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
#[cfg(test)]
pub(crate) fn recover_onnx_bundle_installation(
    target_root: &Path,
) -> Result<OnnxBundleRecovery, InstallError> {
    let _guard = acquire_bundle_target(target_root)?;
    recover_onnx_bundle_installation_locked(target_root)
}

fn recover_onnx_bundle_installation_locked(
    target_root: &Path,
) -> Result<OnnxBundleRecovery, InstallError> {
    let rollback = crate::installations::directory_activation_rollback_root(target_root);
    let mut recovery = OnnxBundleRecovery::default();
    if crate::installations::path_entry_exists_no_follow(&rollback)? {
        current_executable_receipt_at(&rollback).map_err(|error| {
            InstallError::RecoveryRequired(format!(
                "interrupted ONNX bundle rollback is not exact at {}: {error}",
                rollback.display()
            ))
        })?;
        if crate::installations::path_entry_exists_no_follow(target_root)? {
            current_executable_receipt_at(target_root).map_err(|error| {
                InstallError::RecoveryRequired(format!(
                    "interrupted ONNX bundle target is not exact at {}: {error}",
                    target_root.display()
                ))
            })?;
            crate::installations::retain_interrupted_directory_replacement(target_root)?;
            recovery.retained_interrupted_previous = true;
        } else {
            crate::installations::restore_interrupted_directory_replacement(target_root)?;
            recovery.restored_interrupted_previous = true;
        }
    }
    recovery.discarded_incomplete_staging =
        crate::installations::discard_file_bundle_staging(target_root)?;
    Ok(recovery)
}

#[cfg(test)]
pub(crate) fn write_test_receipt_for_spec(
    spec: &OnnxModelSpec,
) -> Result<OnnxBundleManifest, InstallError> {
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
    for material in generated_license_materials(&receipt.license) {
        let path = spec.root.join(&material.install_path);
        std::fs::create_dir_all(path.parent().expect("license material has a parent"))
            .map_err(|error| failed(format!("failed to create test license directory: {error}")))?;
        std::fs::write(&path, material.bytes)
            .map_err(|error| failed(format!("failed to write test license material: {error}")))?;
    }
    Ok(manifest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::{Arc, Barrier, mpsc};
    use std::thread;
    use std::time::Duration;

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

    #[test]
    fn normalized_receipt_binding_matches_private_bundle_authority() {
        let model_id = crate::transcription::ModelId::new("moonshine-tiny-en-int8-onnx");
        let crate::model_catalog::NormalizedInstallArtifact::ReceiptBackedBundle {
            bundle_id,
            aggregate_size_bytes,
        } = crate::model_catalog::normalized_install_artifact(&model_id).unwrap()
        else {
            panic!("Moonshine must remain receipt-backed");
        };
        let manifest = bundle_manifest(bundle_id).unwrap();
        assert_eq!(manifest.id, bundle_id);
        assert_eq!(
            manifest
                .files
                .iter()
                .map(|file| file.size_bytes)
                .sum::<u64>(),
            aggregate_size_bytes
        );
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
        for material in generated_license_materials(&receipt.license) {
            let path = root.join(&material.install_path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, material.bytes).unwrap();
        }
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
        let mut generated = vec![
            GeneratedBundleFile {
                install_path: PathBuf::from(RECEIPT_FILE_NAME),
                bytes: receipt_bytes(receipt).unwrap(),
            },
            GeneratedBundleFile {
                install_path: PathBuf::from(NOTICE_FILE_NAME),
                bytes: notice_bytes(receipt),
            },
        ];
        generated.extend(generated_license_materials(&receipt.license));
        generated
    }

    #[test]
    fn embedded_catalog_has_exact_private_bundle_evidence() {
        let catalog = parse_catalog(CATALOG_BYTES).unwrap();
        assert_eq!(catalog.bundles.len(), 6);
        assert_eq!(available_bundle_manifests().count(), 5);
        let moonshine = bundle_manifest("moonshine-tiny-en-int8-onnx").unwrap();
        assert_eq!(
            moonshine.revision,
            "d1e6c30921780b8508d04b492dfb3ce8a51605d4"
        );
        assert_eq!(moonshine.files.len(), 4);
        assert_eq!(
            moonshine.license.source_repository,
            "moonshine-ai/moonshine"
        );
        assert_eq!(
            moonshine.license.source_revision.as_deref(),
            Some("06f74196a6212fe8642df143d87a243970f15114")
        );
        assert!(moonshine.files.iter().any(|file| {
            file.role == BundleFileRole::License && file.path == Path::new("LICENSE")
        }));
        let parakeet = bundle_manifest("parakeet-tdt-ctc-110m-en-int8-onnx").unwrap();
        assert_eq!(parakeet.availability, BundleAvailability::Unavailable);
        assert!(parakeet.files.is_empty());

        let moonshine_base = bundle_manifest("moonshine-base-en-int8-onnx").unwrap();
        assert_eq!(moonshine_base.availability, BundleAvailability::Available);
        assert_eq!(
            moonshine_base.repository,
            "csukuangfj/sherpa-onnx-moonshine-base-en-int8"
        );
        assert_eq!(
            moonshine_base.revision,
            "052b0798ad1bf046a140fdd4efcd9426530fa3f5"
        );
        assert_eq!(moonshine_base.family, OnnxModelFamily::Moonshine);
        assert_eq!(moonshine_base.num_threads, 2);
        assert_eq!(
            moonshine_base
                .files
                .iter()
                .map(|file| file.role)
                .collect::<Vec<_>>(),
            [
                BundleFileRole::Preprocessor,
                BundleFileRole::Encoder,
                BundleFileRole::UncachedDecoder,
                BundleFileRole::CachedDecoder,
                BundleFileRole::Tokens,
                BundleFileRole::License,
            ]
        );
        assert_eq!(
            moonshine_base
                .files
                .iter()
                .filter(|file| file.role != BundleFileRole::License)
                .map(|file| file.size_bytes)
                .sum::<u64>(),
            286_929_760
        );
        assert_eq!(
            moonshine_base
                .files
                .iter()
                .map(|file| file.size_bytes)
                .sum::<u64>(),
            286_930_831
        );
        assert_eq!(moonshine_base.license.spdx, "MIT");
        assert_eq!(moonshine_base.license.copyright, "Useful Sensors, 2024");
        assert_eq!(
            moonshine_base.license.source_repository,
            "usefulsensors/moonshine"
        );
        assert_eq!(moonshine_base.license.source_revision, None);
        let moonshine_base_spec = spec_from_parts(
            &moonshine_base.id,
            PathBuf::from("bundle"),
            moonshine_base.family,
            moonshine_base.num_threads,
            &moonshine_base.files,
        )
        .unwrap();
        assert_eq!(moonshine_base_spec.family, OnnxModelFamily::Moonshine);
        assert_eq!(moonshine_base_spec.files.len(), 5);
        assert!(
            moonshine_base_spec
                .files
                .contains_key(&OnnxFileRole::Preprocessor)
        );
        assert!(
            moonshine_base_spec
                .files
                .contains_key(&OnnxFileRole::UncachedDecoder)
        );
        assert!(
            moonshine_base_spec
                .files
                .contains_key(&OnnxFileRole::CachedDecoder)
        );

        let parakeet_tdt = bundle_manifest("parakeet-tdt-06b-v2-en-int8-onnx").unwrap();
        assert_eq!(parakeet_tdt.availability, BundleAvailability::Available);
        assert_eq!(
            parakeet_tdt.repository,
            "csukuangfj/sherpa-onnx-nemo-parakeet-tdt-0.6b-v2-int8"
        );
        assert_eq!(
            parakeet_tdt.revision,
            "1ab9323565ddb038682214b292f588070a538ce2"
        );
        assert_eq!(parakeet_tdt.family, OnnxModelFamily::OfflineTransducer);
        assert_eq!(parakeet_tdt.num_threads, 2);
        assert_eq!(
            parakeet_tdt
                .files
                .iter()
                .map(|file| file.role)
                .collect::<Vec<_>>(),
            [
                BundleFileRole::Encoder,
                BundleFileRole::Decoder,
                BundleFileRole::Joiner,
                BundleFileRole::Tokens,
            ]
        );
        assert_eq!(
            parakeet_tdt
                .files
                .iter()
                .map(|file| file.size_bytes)
                .sum::<u64>(),
            661_190_513
        );
        assert_eq!(parakeet_tdt.license.spdx, "CC-BY-4.0");
        assert_eq!(parakeet_tdt.license.copyright, "NVIDIA Corporation");
        assert_eq!(
            parakeet_tdt.license.source_repository,
            "nvidia/parakeet-tdt-0.6b-v2"
        );
        assert_eq!(parakeet_tdt.license.source_revision, None);
        let parakeet_tdt_spec = spec_from_parts(
            &parakeet_tdt.id,
            PathBuf::from("bundle"),
            parakeet_tdt.family,
            parakeet_tdt.num_threads,
            &parakeet_tdt.files,
        )
        .unwrap();
        assert_eq!(parakeet_tdt_spec.family, OnnxModelFamily::OfflineTransducer);
        assert_eq!(parakeet_tdt_spec.files.len(), 4);
        assert!(parakeet_tdt_spec.files.contains_key(&OnnxFileRole::Encoder));
        assert!(parakeet_tdt_spec.files.contains_key(&OnnxFileRole::Decoder));
        assert!(parakeet_tdt_spec.files.contains_key(&OnnxFileRole::Joiner));
        assert!(parakeet_tdt_spec.files.contains_key(&OnnxFileRole::Tokens));

        let canary = bundle_manifest("canary-180m-flash-int8-onnx").unwrap();
        assert_eq!(canary.capability.languages, ["en"]);
        assert!(!canary.capability.native_streaming);
        assert_eq!(
            canary.license.legal_url,
            "https://creativecommons.org/licenses/by/4.0/legalcode"
        );
        assert!(
            canary
                .license
                .changes_notice
                .contains("dynamically quantized")
        );
        let zipformer = bundle_manifest("zipformer-streaming-en-20m-int8-onnx").unwrap();
        assert!(zipformer.capability.native_streaming);
        assert_eq!(
            zipformer.license.source_repository,
            "desh2608/icefall-asr-librispeech-pruned-transducer-stateless7-streaming-small"
        );
        assert_eq!(
            zipformer.license.source_revision.as_deref(),
            Some("be162ecc09bade73063a671fad9d18220149d25b")
        );
        assert!(zipformer.license.changes_notice.contains("unpinned"));
        assert_eq!(
            catalog.runtime.source_revision,
            "3dc7c569f31ca2cd4a20ed6f7db780327e6714c5"
        );
    }

    #[test]
    fn moonshine_family_layout_accepts_only_merged_or_v1_roles() {
        let catalog = parse_catalog(CATALOG_BYTES).unwrap();
        let merged = catalog
            .bundles
            .iter()
            .find(|bundle| bundle.id == "moonshine-tiny-en-int8-onnx")
            .unwrap();
        let v1 = catalog
            .bundles
            .iter()
            .find(|bundle| bundle.id == "moonshine-base-en-int8-onnx")
            .unwrap();
        assert!(validate_family_layout(merged).is_ok());
        assert!(validate_family_layout(v1).is_ok());

        let mut partial = v1.clone();
        partial
            .files
            .retain(|file| file.role != BundleFileRole::CachedDecoder);
        assert!(validate_family_layout(&partial).is_err());

        let mut hybrid = v1.clone();
        hybrid.files.push(
            merged
                .files
                .iter()
                .find(|file| file.role == BundleFileRole::MergedDecoder)
                .unwrap()
                .clone(),
        );
        assert!(validate_family_layout(&hybrid).is_err());
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
        let mut catalog = parse_catalog(CATALOG_BYTES).unwrap();
        catalog.bundles[0].files[0].path = PathBuf::from("encoded%2fescape.onnx");
        assert!(validate_catalog(&catalog).is_err());
        let mut catalog = parse_catalog(CATALOG_BYTES).unwrap();
        catalog.bundles[0].files[0].path = PathBuf::from("nested\\escape.onnx");
        assert!(validate_catalog(&catalog).is_err());
        let mut catalog = parse_catalog(CATALOG_BYTES).unwrap();
        catalog.bundles[0].repository = "owner/repo%2fescape".to_owned();
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
    fn rollback_rejects_a_retired_receipt_without_deleting_it() {
        let root = unique_root("retired-rollback");
        let target = root.join("target");
        fs::create_dir_all(&root).unwrap();
        let previous = crate::installations::previous_runtime_root(&target);
        write_fixture_bundle(&previous, "retired-moonshine-build", "retired");

        let guard = acquire_bundle_target(&target).unwrap();
        assert!(rollback_to_previous_onnx_bundle(&target).is_err());
        assert!(previous.exists());
        drop(guard);
        assert!(rollback_to_previous_onnx_bundle(&target).is_err());
        assert!(previous.exists());
        assert!(verified_receipt_at(&previous).is_ok());
        assert!(!target.exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn partial_discard_uses_the_target_mutation_guard() {
        let root = unique_root("discard-guard");
        fs::create_dir_all(&root).unwrap();
        let model_id = "moonshine-tiny-en-int8-onnx";
        let artifact = pinned_files(&root, bundle_manifest(model_id).unwrap())
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
        fs::create_dir_all(artifact.destination.parent().unwrap()).unwrap();
        let mut partial_name = artifact.destination.file_name().unwrap().to_os_string();
        partial_name.push(".partial");
        let partial = artifact.destination.with_file_name(partial_name);
        fs::write(&partial, b"retained").unwrap();
        let target = bundle_target_root(&root, model_id).unwrap();
        let guard = acquire_bundle_target(&target).unwrap();

        assert_eq!(
            retained_onnx_bundle_partial(model_id, &root).unwrap(),
            Some(crate::installations::RetainedPartial { bytes: 8 })
        );
        assert!(discard_onnx_bundle_partials(model_id, &root).is_err());
        assert!(partial.exists());
        drop(guard);
        assert_eq!(discard_onnx_bundle_partials(model_id, &root).unwrap(), 1);
        assert_eq!(retained_onnx_bundle_partial(model_id, &root).unwrap(), None);
        assert!(!partial.exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn pre_cancelled_bundle_stage_creates_no_storage_or_coordination_state() {
        let root = unique_root("stage-pre-cancel");
        let cancellation = InstallCancellation::default();
        cancellation.cancel();

        let error =
            stage_onnx_bundle_install("moonshine-tiny-en-int8-onnx", &root, &cancellation, &|_| {})
                .unwrap_err();

        assert!(error.is_cancelled());
        assert!(!root.exists());
    }

    #[test]
    fn browse_disk_preflight_does_not_hash_complete_sized_cache_entries() {
        let root = unique_root("browse-preflight-metadata-only");
        let manifest = bundle_manifest("moonshine-tiny-en-int8-onnx").unwrap();
        let artifacts = pinned_files(&root, manifest).unwrap();
        let cached = &artifacts[0];
        fs::create_dir_all(cached.destination.parent().unwrap()).unwrap();
        File::create(&cached.destination)
            .unwrap()
            .set_len(cached.size_bytes)
            .unwrap();
        crate::installations::reset_file_hash_count(&cached.destination);

        bundle_disk_space_preflight(manifest.id.as_str(), &root).unwrap();

        assert_eq!(
            crate::installations::file_hash_count(&cached.destination),
            0
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn cancellation_during_bundle_cache_inspection_precedes_locks_and_reservations() {
        let root = unique_root("stage-inspection-cancel");
        let manifest = bundle_manifest("moonshine-tiny-en-int8-onnx").unwrap();
        let artifacts = pinned_files(&root, manifest).unwrap();
        let cached = &artifacts[0];
        fs::create_dir_all(cached.destination.parent().unwrap()).unwrap();
        File::create(&cached.destination)
            .unwrap()
            .set_len(cached.size_bytes)
            .unwrap();
        let cancellation = InstallCancellation::default();
        crate::installations::cancel_file_hash_after(&cached.destination, 1, cancellation.clone());
        let target = bundle_target_root(&root, manifest.id.as_str()).unwrap();

        let error = stage_onnx_bundle_install(manifest.id.as_str(), &root, &cancellation, &|_| {})
            .unwrap_err();

        assert!(error.is_cancelled());
        assert!(!target.exists());
        assert!(!target.parent().unwrap().join(LOCK_DIRECTORY_NAME).exists());
        assert!(!cached.destination.with_extension("ort.partial").exists());
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
    fn scoped_receipt_observer_reports_one_full_tree_verification() {
        let root = unique_root("receipt-observer");
        write_fixture_bundle(&root, "fixture-moonshine", "observer");

        let (result, stats) = observe_receipt_verifications_for_test(|| verified_receipt_at(&root));

        assert!(result.is_ok());
        assert_eq!(stats.calls, 1);
        assert!(stats.verified_bytes > 0);
        assert_eq!(stats.durations.len(), 1);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn receipt_observer_restores_nested_scopes_and_panics() {
        let root = unique_root("receipt-observer-nested");
        write_fixture_bundle(&root, "fixture-moonshine", "nested");

        let (inner_stats, outer_stats) = observe_receipt_verifications_for_test(|| {
            assert!(verified_receipt_at(&root).is_ok());
            let (_, inner_stats) =
                observe_receipt_verifications_for_test(|| verified_receipt_at(&root));
            assert!(verified_receipt_at(&root).is_ok());
            inner_stats
        });
        assert_eq!(inner_stats.calls, 1);
        assert_eq!(outer_stats.calls, 2);
        assert!(inner_stats.verified_bytes > 0);
        assert!(outer_stats.verified_bytes > inner_stats.verified_bytes);

        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = observe_receipt_verifications_for_test(|| {
                assert!(verified_receipt_at(&root).is_ok());
                panic!("receipt observer panic fixture");
            });
        }));
        assert!(panic.is_err());
        let (_, stats) = observe_receipt_verifications_for_test(|| verified_receipt_at(&root));
        assert_eq!(stats.calls, 1);
        assert!(stats.verified_bytes > 0);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn receipt_observer_counts_bytes_only_after_a_successful_tree_verification() {
        let root = unique_root("receipt-observer-failure");
        let receipt = write_fixture_bundle(&root, "fixture-moonshine", "failure");
        fs::write(root.join(&receipt.files[0].path), b"tampered").unwrap();

        let (result, stats) = observe_receipt_verifications_for_test(|| verified_receipt_at(&root));

        assert!(result.is_err());
        assert_eq!(stats.calls, 1);
        assert_eq!(stats.verified_bytes, 0);
        assert_eq!(stats.durations.len(), 1);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn receipts_cover_complete_generated_license_materials() {
        assert!(CC_BY_4_LICENSE_BYTES.len() > 16_000);
        assert!(APACHE_2_LICENSE_BYTES.len() > 11_000);
        assert!(MOONSHINE_MIT_LICENSE_BYTES.starts_with(b"MIT License"));
        let root = unique_root("license-materials");
        let receipt = write_fixture_bundle(&root, "fixture-moonshine", "licenses");
        assert_eq!(receipt.generated_license_files.len(), 1);
        let evidence = &receipt.generated_license_files[0];
        assert_eq!(evidence.path, Path::new("LICENSES/Moonshine-MIT.txt"));
        assert_eq!(
            evidence.size_bytes,
            MOONSHINE_MIT_LICENSE_BYTES.len() as u64
        );
        verified_receipt_at(&root).unwrap();

        fs::write(root.join(&evidence.path), b"tampered license").unwrap();
        assert!(verified_receipt_at(&root).is_err());
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
    fn distinct_bundle_targets_share_a_storage_root_without_serializing_each_other() {
        let root = unique_root("distinct-target-concurrency");
        fs::create_dir_all(&root).unwrap();
        let first_target = root.join("first-model");
        let second_target = root.join("second-model");
        let start = Arc::new(Barrier::new(3));
        let (held_tx, held_rx) = mpsc::channel();
        let (release_first_tx, release_first_rx) = mpsc::channel();
        let (release_second_tx, release_second_rx) = mpsc::channel();

        let spawn_holder = |label: &'static str,
                            target: PathBuf,
                            start: Arc<Barrier>,
                            held_tx: mpsc::Sender<(&'static str, Result<(), String>)>,
                            release_rx: mpsc::Receiver<()>| {
            thread::spawn(move || {
                start.wait();
                let guard = acquire_bundle_target(&target);
                let status = guard.as_ref().map(|_| ()).map_err(ToString::to_string);
                held_tx.send((label, status)).unwrap();
                if guard.is_ok() {
                    release_rx
                        .recv_timeout(Duration::from_secs(5))
                        .expect("holder release must arrive before the bounded timeout");
                }
                drop(guard);
            })
        };
        let first_thread = spawn_holder(
            "first",
            first_target.clone(),
            Arc::clone(&start),
            held_tx.clone(),
            release_first_rx,
        );
        let second_thread = spawn_holder(
            "second",
            second_target.clone(),
            Arc::clone(&start),
            held_tx,
            release_second_rx,
        );

        start.wait();
        let first_held = held_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("first distinct target acquisition must complete without blocking");
        let second_held = held_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("second distinct target acquisition must complete without blocking");
        let same_first = acquire_bundle_target(&first_target);
        let same_second = acquire_bundle_target(&second_target);

        let _ = release_first_tx.send(());
        let _ = release_second_tx.send(());
        first_thread.join().unwrap();
        second_thread.join().unwrap();

        let mut held = [first_held, second_held];
        held.sort_by_key(|(label, _)| *label);
        assert_eq!(held[0].0, "first");
        assert!(held[0].1.is_ok(), "first target failed: {:?}", held[0].1);
        assert_eq!(held[1].0, "second");
        assert!(held[1].1.is_ok(), "second target failed: {:?}", held[1].1);
        assert!(same_first.is_err());
        assert!(same_second.is_err());
        drop(same_first);
        drop(same_second);

        let first_after_release = acquire_bundle_target(&first_target).unwrap();
        let second_after_release = acquire_bundle_target(&second_target).unwrap();
        drop(first_after_release);
        drop(second_after_release);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn target_lock_remains_exclusive_without_the_in_process_registry() {
        let root = unique_root("os-lock");
        fs::create_dir_all(&root).unwrap();
        let target = root.join("model");
        let first = acquire_bundle_target(&target).unwrap();
        active_bundle_targets()
            .lock()
            .unwrap()
            .remove(&first.identity);

        let error = acquire_bundle_target(&target).unwrap_err();
        assert!(error.to_string().contains("another process owns"));
        drop(first);
        assert!(acquire_bundle_target(&target).is_ok());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn disk_reservations_are_aggregated_and_released_with_the_guard() {
        let root = unique_root("reservation-lifetime");
        let control = root.join("control");
        let first_storage = root.join("first-storage");
        let second_storage = root.join("second-storage");
        fs::create_dir_all(&first_storage).unwrap();
        fs::create_dir_all(&second_storage).unwrap();
        let first = acquire_bundle_disk_reservation_with_control_root(
            &control,
            &first_storage.join("model"),
            7,
        )
        .unwrap();
        let second = acquire_bundle_disk_reservation_with_control_root(
            &control,
            &second_storage.join("other-model"),
            11,
        )
        .unwrap();
        assert_eq!(first.reservation_root, second.reservation_root);
        assert_eq!(first.remaining_bytes, 7);
        assert_eq!(second.remaining_bytes, 11);
        assert_eq!(fs::read_to_string(&first.entry_path).unwrap().trim(), "7");
        assert_eq!(fs::read_to_string(&second.entry_path).unwrap().trim(), "11");
        let exact =
            disk_space::preflight_download_destination(&first_storage.join("model"), 18).unwrap();
        assert_eq!(exact.additional_bytes, 18);
        assert_eq!(
            exact.required_bytes,
            18 + disk_space::SAFETY_HEADROOM_BYTES,
            "one shared headroom floor is added after aggregating live requested bytes"
        );
        let reservations = first.reservation_root.clone();
        let count = || {
            fs::read_dir(&reservations)
                .unwrap()
                .filter_map(Result::ok)
                .filter(|entry| {
                    entry
                        .path()
                        .extension()
                        .is_some_and(|extension| extension == "reservation")
                })
                .count()
        };
        assert_eq!(count(), 2);
        drop(first);
        assert_eq!(count(), 1);
        drop(second);
        assert_eq!(count(), 0);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn consumed_disk_reduces_live_reservations_without_allowing_overcommit() {
        const CONSUMED_BYTES: u64 = 16 * 1024 * 1024;
        const ADMISSION_SLACK: u64 = 2 * 1024 * 1024;
        const WRITE_CHUNK_BYTES: usize = 64 * 1024;

        let root = unique_root("reservation-consumption");
        let control = root.join("control");
        let first_storage = root.join("first-storage");
        let second_storage = root.join("second-storage");
        fs::create_dir_all(&first_storage).unwrap();
        fs::create_dir_all(&second_storage).unwrap();
        let first_target = first_storage.join("model");
        let second_target = second_storage.join("other-model");
        let mut first = acquire_bundle_disk_reservation_with_control_root(
            &control,
            &first_target,
            CONSUMED_BYTES,
        )
        .unwrap();

        let allocated_path = first_storage.join("consumed.bin");
        let mut allocated = File::create(&allocated_path).unwrap();
        let chunk = [0xa5_u8; WRITE_CHUNK_BYTES];
        for _ in 0..(CONSUMED_BYTES / WRITE_CHUNK_BYTES as u64) {
            allocated.write_all(&chunk).unwrap();
        }
        allocated.sync_all().unwrap();
        drop(allocated);

        let available_after_allocation =
            disk_space::preflight_download_destination(&second_target, 0)
                .unwrap()
                .available_bytes;
        let second_request = available_after_allocation
            .checked_sub(disk_space::SAFETY_HEADROOM_BYTES + ADMISSION_SLACK)
            .expect("test volume needs enough free space for the safety floor and slack");

        let stale_error = acquire_bundle_disk_reservation_with_control_root(
            &control,
            &second_target,
            second_request,
        )
        .unwrap_err();
        assert!(stale_error.to_string().contains("insufficient unreserved"));

        first.consume_allocated_bytes(CONSUMED_BYTES).unwrap();
        assert_eq!(first.remaining_bytes, 0);
        assert_eq!(fs::read_to_string(&first.entry_path).unwrap().trim(), "0");
        let second = acquire_bundle_disk_reservation_with_control_root(
            &control,
            &second_target,
            second_request,
        )
        .unwrap();

        let overcommit = acquire_bundle_disk_reservation_with_control_root(
            &control,
            &root.join("third-storage").join("third-model"),
            ADMISSION_SLACK + 1,
        )
        .unwrap_err();
        assert!(overcommit.to_string().contains("insufficient unreserved"));

        drop(second);
        drop(first);
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn reservation_creation_rejects_preplanted_control_and_volume_links() {
        let root = unique_root("reservation-control-link");
        let target = root.join("storage").join("model");
        let control = root.join("control");
        let external = root.join("external");
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::create_dir_all(&external).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&external, &control).unwrap();
        #[cfg(windows)]
        if std::os::windows::fs::symlink_dir(&external, &control).is_err() {
            fs::remove_dir_all(root).unwrap();
            return;
        }

        let error =
            acquire_bundle_disk_reservation_with_control_root(&control, &target, 1).unwrap_err();
        assert!(
            error.to_string().contains("symbolic link") || error.to_string().contains("reparse")
        );
        assert_eq!(fs::read_dir(&external).unwrap().count(), 0);
        #[cfg(unix)]
        fs::remove_file(&control).unwrap();
        #[cfg(windows)]
        fs::remove_dir(&control).unwrap();

        fs::create_dir(&control).unwrap();
        let identity = disk_space::physical_volume_identity(&target).unwrap();
        let volume_root = volume_reservation_root(&control, &identity);
        #[cfg(unix)]
        std::os::unix::fs::symlink(&external, &volume_root).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_dir(&external, &volume_root).unwrap();

        let error =
            acquire_bundle_disk_reservation_with_control_root(&control, &target, 1).unwrap_err();
        assert!(
            error.to_string().contains("symbolic link") || error.to_string().contains("reparse")
        );
        assert_eq!(fs::read_dir(&external).unwrap().count(), 0);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn active_reservation_contention_participates_in_aggregate_admission() {
        let root = unique_root("reservation-contention");
        let control = root.join("control");
        fs::create_dir_all(&root).unwrap();
        let target = root.join("model");
        let volume_identity = disk_space::physical_volume_identity(&target).unwrap();
        fs::create_dir(&control).unwrap();
        let reservations = volume_reservation_root(&control, &volume_identity);
        fs::create_dir(&reservations).unwrap();
        let entry = reservations.join("other-process.reservation");
        fs::write(&entry, format!("{}\n", u64::MAX)).unwrap();
        let active = OsFileLock::acquire(&entry.with_extension("lock"), false).unwrap();

        let error =
            acquire_bundle_disk_reservation_with_control_root(&control, &target, 1).unwrap_err();
        assert!(error.to_string().contains("overflow"));

        drop(active);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn contended_ledger_defers_release_and_next_owner_prunes_the_stale_entry() {
        let root = unique_root("reservation-release-interleave");
        let control = root.join("control");
        fs::create_dir_all(&root).unwrap();
        let target = root.join("model");
        let reservation =
            acquire_bundle_disk_reservation_with_control_root(&control, &target, 13).unwrap();
        let reservation_root = reservation.reservation_root.clone();
        let entry_path = reservation.entry_path.clone();
        let entry_lock_path = entry_path.with_extension("lock");
        let ledger = OsFileLock::acquire(&reservation_root.join("ledger.lock"), false).unwrap();

        drop(reservation);
        assert!(entry_path.exists());
        assert!(entry_lock_path.exists());
        let stale_is_unlocked = OsFileLock::acquire(&entry_lock_path, false).unwrap();
        drop(stale_is_unlocked);
        drop(ledger);

        let replacement =
            acquire_bundle_disk_reservation_with_control_root(&control, &target, 17).unwrap();
        assert_eq!(
            fs::read_to_string(&replacement.entry_path).unwrap().trim(),
            "17"
        );
        assert_eq!(
            fs::read_dir(&reservation_root)
                .unwrap()
                .filter_map(Result::ok)
                .filter(|entry| {
                    entry
                        .path()
                        .extension()
                        .is_some_and(|value| value == "reservation")
                })
                .count(),
            1,
            "the stale reservation is pruned before the replacement is recorded"
        );
        drop(replacement);
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
    fn pause_uses_the_resumable_cancellation_state() {
        let cancellation = InstallCancellation::default();
        pause_onnx_bundle_install(&cancellation);
        assert!(cancellation.is_cancelled());
    }

    #[test]
    fn pause_interrupts_the_final_exact_tree_hash_before_activation() {
        let root = unique_root("pause-final-hash");
        let source = root.join("source");
        let target = root.join("target");
        let receipt = write_fixture_bundle(&source, "fixture-moonshine", "pause-hash");
        let cancellation = InstallCancellation::default();
        let progress_cancellation = cancellation.clone();
        let error = stage_file_bundle_for_target(
            &fixture_assembly(&source, &receipt),
            &fixture_generated(&receipt),
            &target,
            &cancellation,
            &move |event| {
                if event.stage == crate::installations::InstallStage::Extracting
                    && event.completed_bytes == event.total_bytes
                {
                    pause_onnx_bundle_install(&progress_cancellation);
                }
            },
        )
        .unwrap_err();
        assert!(error.is_cancelled());
        assert!(!target.exists());
        assert!(!root.join(".target.installing").exists());
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
    fn crash_recovery_rejects_retired_receipts_without_deleting_them() {
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
        let error = recover_onnx_bundle_installation(&target).unwrap_err();
        assert!(error.requires_recovery());
        assert_eq!(verified_receipt_at(&target).unwrap().0, new_receipt);
        let rollback = crate::installations::directory_activation_rollback_root(&target);
        assert_eq!(verified_receipt_at(&rollback).unwrap().0, old_receipt);
        assert!(target.exists());
        assert!(rollback.exists());
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
        let rollback = crate::installations::directory_activation_rollback_root(&target);
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
    fn production_stage_entrypoint_blocks_corrupt_interrupted_activation_before_http() {
        let root = unique_root("stage-crash-recovery");
        let target = bundle_target_root(&root, "moonshine-tiny-en-int8-onnx").unwrap();
        let rollback = crate::installations::directory_activation_rollback_root(&target);
        fs::create_dir_all(&rollback).unwrap();
        fs::write(rollback.join("corrupt"), b"not a verified receipt").unwrap();

        let error = stage_onnx_bundle_install(
            "moonshine-tiny-en-int8-onnx",
            &root,
            &InstallCancellation::default(),
            &|_| {},
        )
        .unwrap_err();

        assert!(error.requires_recovery());
        assert!(rollback.exists());
        assert!(!target.exists());
        assert!(!root.join(".downloads").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn local_catalog_and_receipt_paths_cannot_start_http() {
        let source = include_str!("onnx_model_bundles.rs");
        let normalized = source.replace("\r\n", "\n");
        let production = normalized
            .split("\n#[cfg(test)]\nmod tests")
            .next()
            .unwrap();
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
