use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};

#[cfg(unix)]
use std::ffi::CString;
#[cfg(unix)]
use std::os::fd::{AsRawFd, FromRawFd};

use ring::signature::{ED25519, UnparsedPublicKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

pub(crate) const MANIFEST_NAME: &str = "pack-manifest.json";
pub(crate) const SIGNATURE_NAME: &str = "pack-manifest.sig";
pub(crate) const PACK_SCHEMA_VERSION: u16 = 1;
pub(crate) const APP_PROTOCOL_VERSION: u16 = crate::onnx_worker::PROTOCOL_VERSION as u16;
pub(crate) const RUNTIME_ABI_VERSION: u16 = crate::onnx_worker::WORKER_ABI_VERSION;
pub(crate) const EMBEDDED_MINIMUM_SECURITY_EPOCH: u64 = 1;

const MAX_MANIFEST_BYTES: u64 = 256 * 1024;
const MAX_SIGNATURE_BYTES: u64 = 4 * 1024;
const MAX_FILES: usize = 256;
const MAX_DEPTH: usize = 12;
const MAX_NAME_BYTES: usize = 128;
const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const MAX_AGGREGATE_BYTES: u64 = 4 * 1024 * 1024 * 1024;
const PACK_DIGEST_DOMAIN: &[u8] = b"scribe-gpu-worker-pack-digest-v1\0";

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PackBackend {
    Cuda,
    Vulkan,
    Metal,
}

/// A canonical single filesystem component suitable for the immutable store
/// hierarchy. Serde remains transparent so signed and persisted schemas keep
/// their string representation; every trust boundary revalidates the value.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub(crate) struct StoreComponent(String);

impl StoreComponent {
    pub(crate) fn new(value: impl Into<String>) -> Option<Self> {
        let value = value.into();
        is_canonical_store_component(&value).then_some(Self(value))
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }

    pub(crate) fn is_canonical(&self) -> bool {
        is_canonical_store_component(&self.0)
    }

    #[cfg(test)]
    pub(crate) fn test_unchecked(value: impl Into<String>) -> Self {
        Self(value.into())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PayloadEntry {
    pub(crate) path: String,
    pub(crate) size_bytes: u64,
    pub(crate) sha256: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PackManifest {
    pub(crate) schema_version: u16,
    pub(crate) pack_id: StoreComponent,
    pub(crate) pack_version: StoreComponent,
    pub(crate) pack_digest: String,
    pub(crate) security_epoch: u64,
    pub(crate) app_protocol_version: u16,
    pub(crate) worker_protocol_version: u16,
    pub(crate) runtime_abi_version: u16,
    pub(crate) app_build: String,
    pub(crate) worker_build: String,
    pub(crate) backend: PackBackend,
    pub(crate) provider: String,
    pub(crate) target_os: String,
    pub(crate) target_arch: String,
    pub(crate) worker_path: String,
    pub(crate) payload: Vec<PayloadEntry>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DetachedSignature {
    schema_version: u16,
    key_id: String,
    signature_hex: String,
}

#[derive(Serialize)]
struct DigestMaterial<'a> {
    schema_version: u16,
    pack_id: &'a str,
    pack_version: &'a str,
    security_epoch: u64,
    app_protocol_version: u16,
    worker_protocol_version: u16,
    runtime_abi_version: u16,
    app_build: &'a str,
    worker_build: &'a str,
    backend: PackBackend,
    provider: &'a str,
    target_os: &'a str,
    target_arch: &'a str,
    worker_path: &'a str,
    payload: &'a [PayloadEntry],
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct VerifiedPack {
    pub(crate) pack_id: StoreComponent,
    pub(crate) pack_version: StoreComponent,
    pub(crate) pack_digest: String,
    pub(crate) security_epoch: u64,
    pub(crate) runtime_abi_version: u16,
    pub(crate) backend: PackBackend,
    pub(crate) provider: String,
    pub(crate) target_os: String,
    pub(crate) target_arch: String,
    pub(crate) worker_relative_path: String,
    pub(crate) root: PathBuf,
}

impl VerifiedPack {
    pub(crate) fn worker_path(&self) -> PathBuf {
        self.root.join(&self.worker_relative_path)
    }
}

/// Retained filesystem authority for one verified immutable pack.  The
/// descriptor is metadata only; callers that need executable authority must
/// retain this value so each immutable-store ancestor remains pinned.
pub(crate) struct VerifiedPackLease {
    verified_pack: VerifiedPack,
    root: PinnedPackRoot,
    _retained_files: Vec<File>,
}

impl std::fmt::Debug for VerifiedPackLease {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("VerifiedPackLease")
            .field("verified_pack", &self.verified_pack)
            .finish_non_exhaustive()
    }
}

impl VerifiedPackLease {
    pub(crate) fn verified_pack(&self) -> &VerifiedPack {
        &self.verified_pack
    }

    pub(crate) fn worker_path(&self) -> PathBuf {
        self.verified_pack.worker_path()
    }

    pub(crate) fn recheck(&self) -> Result<(), PackVerificationError> {
        self.root.recheck()
    }
}

/// Borrowed launch authority. Its lifetime prevents the retained pack lease
/// from being dropped before Stage 2 consumes the exact worker target.
pub(crate) struct LaunchableWorker<'lease> {
    path: PathBuf,
    _lease: &'lease VerifiedPackLease,
}

impl LaunchableWorker<'_> {
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

/// A no-follow handle chain from the canonical pack store root through the
/// exact pack-id/version/digest components.
pub(crate) struct PinnedPackRoot {
    path: PathBuf,
    handles: Vec<File>,
}

impl PinnedPackRoot {
    pub(crate) fn from_anchored_handles(
        path: PathBuf,
        handles: Vec<File>,
    ) -> Result<Self, PackVerificationError> {
        if handles.is_empty() {
            return Err(PackVerificationError::UnsafePackStoreAncestor(path));
        }
        let lease = Self { path, handles };
        lease.recheck()?;
        Ok(lease)
    }

    pub(crate) fn open(
        canonical_store_root: &Path,
        components: [&StoreComponent; 2],
        digest: &str,
    ) -> Result<Self, PackVerificationError> {
        if !components.iter().all(|component| component.is_canonical())
            || !is_canonical_sha256(digest)
        {
            return Err(PackVerificationError::UnsafePackStoreAncestor(
                canonical_store_root.to_path_buf(),
            ));
        }
        open_pinned_pack_root(canonical_store_root, components, digest)
    }

    fn verification_root(&self) -> PathBuf {
        #[cfg(target_os = "linux")]
        {
            return PathBuf::from(format!(
                "/proc/self/fd/{}",
                self.handles.last().expect("digest handle").as_raw_fd()
            ));
        }
        #[cfg(all(unix, not(target_os = "linux")))]
        {
            return PathBuf::from(format!(
                "/dev/fd/{}",
                self.handles.last().expect("digest handle").as_raw_fd()
            ));
        }
        #[cfg(windows)]
        {
            self.path.clone()
        }
        #[cfg(not(any(unix, windows)))]
        {
            self.path.clone()
        }
    }

    fn open_regular(&self, relative: &Path) -> Result<File, PackVerificationError> {
        #[cfg(unix)]
        {
            return open_regular_at(
                self.handles.last().expect("digest handle").as_raw_fd(),
                relative,
            );
        }
        #[cfg(windows)]
        {
            open_regular_no_follow(&self.path.join(relative))
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = relative;
            Err(PackVerificationError::UnsupportedLeasePlatform)
        }
    }

    fn recheck(&self) -> Result<(), PackVerificationError> {
        let mut path = self.path.clone();
        for handle in self.handles.iter().rev() {
            if same_directory_identity(handle, &path)? {
                path.pop();
            } else {
                return Err(PackVerificationError::PackStoreAncestorChanged(path));
            }
        }
        Ok(())
    }
}

pub(crate) trait TrustRoot: Send + Sync {
    fn public_key(&self, key_id: &str) -> Option<&[u8]>;
}

/// Intentionally empty in Stage 3. Adding a production key is an explicit
/// release-security event, not a build-time fallback.
pub(crate) struct ProductionTrustRoot;

impl TrustRoot for ProductionTrustRoot {
    fn public_key(&self, _key_id: &str) -> Option<&[u8]> {
        None
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Compatibility<'a> {
    pub(crate) app_build: &'a str,
    pub(crate) worker_build: &'a str,
    pub(crate) target_os: &'a str,
    pub(crate) target_arch: &'a str,
    pub(crate) allowed_backends: &'a [PackBackend],
}

impl Compatibility<'static> {
    pub(crate) fn current(allowed_backends: &'static [PackBackend]) -> Self {
        Self {
            app_build: crate::onnx_worker::DESKTOP_BUILD_ID,
            worker_build: crate::onnx_worker::INFERENCE_WORKER_BUILD_ID,
            target_os: std::env::consts::OS,
            target_arch: std::env::consts::ARCH,
            allowed_backends,
        }
    }
}

pub(crate) struct PackVerifier<'a> {
    trust_root: &'a dyn TrustRoot,
    compatibility: Compatibility<'a>,
}

impl<'a> PackVerifier<'a> {
    pub(crate) fn new(trust_root: &'a dyn TrustRoot, compatibility: Compatibility<'a>) -> Self {
        Self {
            trust_root,
            compatibility,
        }
    }

    pub(crate) fn verify(&self, root: &Path) -> Result<VerifiedPack, PackVerificationError> {
        let (verified, _) = self.verify_inner(root, None)?;
        Ok(verified)
    }

    pub(crate) fn verify_pinned(
        &self,
        root: PinnedPackRoot,
    ) -> Result<VerifiedPackLease, PackVerificationError> {
        root.recheck()?;
        let verification_root = root.verification_root();
        let (verified_pack, retained_files) = self.verify_inner(&verification_root, Some(&root))?;
        root.recheck()?;
        Ok(VerifiedPackLease {
            verified_pack,
            root,
            _retained_files: retained_files,
        })
    }

    fn verify_inner(
        &self,
        root: &Path,
        pinned: Option<&PinnedPackRoot>,
    ) -> Result<(VerifiedPack, Vec<File>), PackVerificationError> {
        if pinned.is_none() {
            validate_root(root)?;
        }
        let (manifest_bytes, manifest_file) =
            read_bounded_regular_from(root, pinned, Path::new(MANIFEST_NAME), MAX_MANIFEST_BYTES)?;
        let (signature_bytes, signature_file) = read_bounded_regular_from(
            root,
            pinned,
            Path::new(SIGNATURE_NAME),
            MAX_SIGNATURE_BYTES,
        )?;

        // No inventory-controlled payload path is touched before the bounded
        // exact signed envelope authenticates successfully.
        let signature: DetachedSignature = parse_canonical_json(&signature_bytes, "signature")?;
        if signature.schema_version != PACK_SCHEMA_VERSION {
            return Err(PackVerificationError::UnsupportedSchema);
        }
        let public_key = self
            .trust_root
            .public_key(&signature.key_id)
            .ok_or(PackVerificationError::UnknownKey)?;
        let signature_raw = decode_hex_exact(&signature.signature_hex, 64)
            .ok_or(PackVerificationError::InvalidSignatureEncoding)?;
        UnparsedPublicKey::new(&ED25519, public_key)
            .verify(&manifest_bytes, &signature_raw)
            .map_err(|_| PackVerificationError::BadSignature)?;

        let manifest: PackManifest = parse_canonical_json(&manifest_bytes, "manifest")?;
        self.validate_manifest(&manifest)?;
        verify_exact_tree(root, &manifest.payload)?;
        let mut retained_files = verify_payload(root, pinned, &manifest.payload)?;
        retained_files.push(manifest_file);
        retained_files.push(signature_file);

        let descriptor_root = pinned.map_or_else(|| root.to_path_buf(), |lease| lease.path.clone());
        Ok((
            VerifiedPack {
                pack_id: manifest.pack_id,
                pack_version: manifest.pack_version,
                pack_digest: manifest.pack_digest,
                security_epoch: manifest.security_epoch,
                runtime_abi_version: manifest.runtime_abi_version,
                backend: manifest.backend,
                provider: manifest.provider,
                target_os: manifest.target_os,
                target_arch: manifest.target_arch,
                worker_relative_path: manifest.worker_path,
                root: descriptor_root,
            },
            retained_files,
        ))
    }

    /// Re-verifies the complete signed tree immediately before Stage 2's
    /// exact-path/image-handle launcher receives the worker path.
    pub(crate) fn launchable_worker<'lease>(
        &self,
        expected: &'lease VerifiedPackLease,
    ) -> Result<LaunchableWorker<'lease>, PackVerificationError> {
        expected.recheck()?;
        let verification_root = expected.root.verification_root();
        let (observed, _launch_files) =
            self.verify_inner(&verification_root, Some(&expected.root))?;
        if &observed != expected.verified_pack() {
            return Err(PackVerificationError::DescriptorChanged);
        }
        expected.recheck()?;
        // The original retained worker handle remains part of the lease.
        // Stage 4 must hand this lease into the exact-image launcher instead
        // of reconstructing authority from this display path.
        Ok(LaunchableWorker {
            path: expected.worker_path(),
            _lease: expected,
        })
    }

    fn validate_manifest(&self, manifest: &PackManifest) -> Result<(), PackVerificationError> {
        if manifest.schema_version != PACK_SCHEMA_VERSION {
            return Err(PackVerificationError::UnsupportedSchema);
        }
        validate_store_component(&manifest.pack_id, "pack id")?;
        validate_store_component(&manifest.pack_version, "pack version")?;
        validate_identifier(&manifest.provider, "provider")?;
        validate_build_identity(&manifest.app_build, "app build")?;
        validate_build_identity(&manifest.worker_build, "worker build")?;
        validate_sha256(&manifest.pack_digest)?;
        if manifest.security_epoch < EMBEDDED_MINIMUM_SECURITY_EPOCH {
            return Err(PackVerificationError::SecurityEpochTooOld);
        }
        if manifest.app_protocol_version != APP_PROTOCOL_VERSION
            || manifest.worker_protocol_version != APP_PROTOCOL_VERSION
        {
            return Err(PackVerificationError::ProtocolMismatch);
        }
        if manifest.runtime_abi_version != RUNTIME_ABI_VERSION {
            return Err(PackVerificationError::AbiMismatch);
        }
        if manifest.app_build != self.compatibility.app_build
            || manifest.worker_build != self.compatibility.worker_build
        {
            return Err(PackVerificationError::BuildMismatch);
        }
        if manifest.target_os != self.compatibility.target_os
            || manifest.target_arch != self.compatibility.target_arch
        {
            return Err(PackVerificationError::ArchitectureMismatch);
        }
        if !self
            .compatibility
            .allowed_backends
            .contains(&manifest.backend)
        {
            return Err(PackVerificationError::BackendMismatch);
        }
        validate_relative_path(&manifest.worker_path)?;
        validate_inventory(&manifest.payload)?;
        if !manifest
            .payload
            .iter()
            .any(|item| item.path == manifest.worker_path)
        {
            return Err(PackVerificationError::WorkerMissing);
        }
        if manifest.pack_digest != compute_pack_digest(manifest)? {
            return Err(PackVerificationError::DigestMismatch);
        }
        Ok(())
    }
}

/// Pack digest is SHA-256(domain || canonical JSON(identity metadata and the
/// strictly path-sorted complete inventory)). It excludes `pack_digest` and
/// both envelope files, avoiding circular hashing while binding all payload.
pub(crate) fn compute_pack_digest(
    manifest: &PackManifest,
) -> Result<String, PackVerificationError> {
    let material = DigestMaterial {
        schema_version: manifest.schema_version,
        pack_id: manifest.pack_id.as_str(),
        pack_version: manifest.pack_version.as_str(),
        security_epoch: manifest.security_epoch,
        app_protocol_version: manifest.app_protocol_version,
        worker_protocol_version: manifest.worker_protocol_version,
        runtime_abi_version: manifest.runtime_abi_version,
        app_build: &manifest.app_build,
        worker_build: &manifest.worker_build,
        backend: manifest.backend,
        provider: &manifest.provider,
        target_os: &manifest.target_os,
        target_arch: &manifest.target_arch,
        worker_path: &manifest.worker_path,
        payload: &manifest.payload,
    };
    let canonical = serde_json::to_vec(&material).map_err(PackVerificationError::Json)?;
    let mut hasher = Sha256::new();
    hasher.update(PACK_DIGEST_DOMAIN);
    hasher.update(canonical);
    Ok(format!("{:x}", hasher.finalize()))
}

fn parse_canonical_json<T>(bytes: &[u8], label: &'static str) -> Result<T, PackVerificationError>
where
    T: for<'de> Deserialize<'de> + Serialize,
{
    let parsed = serde_json::from_slice::<T>(bytes).map_err(PackVerificationError::Json)?;
    let canonical = serde_json::to_vec(&parsed).map_err(PackVerificationError::Json)?;
    if bytes != canonical {
        return Err(PackVerificationError::NonCanonical(label));
    }
    Ok(parsed)
}

fn validate_inventory(payload: &[PayloadEntry]) -> Result<(), PackVerificationError> {
    if payload.is_empty() || payload.len() > MAX_FILES {
        return Err(PackVerificationError::InvalidFileCount);
    }
    let mut prior: Option<&str> = None;
    let mut casefolded = BTreeSet::new();
    let mut aggregate = 0_u64;
    for item in payload {
        validate_relative_path(&item.path)?;
        validate_sha256(&item.sha256)?;
        if item.size_bytes > MAX_FILE_BYTES {
            return Err(PackVerificationError::FileTooLarge);
        }
        aggregate = aggregate
            .checked_add(item.size_bytes)
            .ok_or(PackVerificationError::AggregateTooLarge)?;
        if aggregate > MAX_AGGREGATE_BYTES {
            return Err(PackVerificationError::AggregateTooLarge);
        }
        if prior.is_some_and(|value| value >= item.path.as_str()) {
            return Err(PackVerificationError::InventoryNotSorted);
        }
        prior = Some(&item.path);
        if !casefolded.insert(item.path.to_ascii_lowercase()) {
            return Err(PackVerificationError::CaseCollision);
        }
    }
    Ok(())
}

fn validate_identifier(value: &str, label: &'static str) -> Result<(), PackVerificationError> {
    if value.is_empty()
        || value.len() > 96
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b':'))
    {
        return Err(PackVerificationError::InvalidIdentifier(label));
    }
    Ok(())
}

fn validate_store_component(
    value: &StoreComponent,
    label: &'static str,
) -> Result<(), PackVerificationError> {
    if !value.is_canonical() {
        return Err(PackVerificationError::InvalidIdentifier(label));
    }
    Ok(())
}

fn is_canonical_store_component(value: &str) -> bool {
    let bytes = value.as_bytes();
    !value.is_empty()
        && value.len() <= 96
        && value != "."
        && value != ".."
        && bytes
            .first()
            .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        && bytes
            .last()
            .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        && bytes.iter().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_' | b'-')
        })
        && !is_reserved_windows_name(value)
}

fn validate_build_identity(value: &str, label: &'static str) -> Result<(), PackVerificationError> {
    if value.len() < 12
        || value.len() > 192
        || !value.bytes().all(|byte| (0x21..=0x7e).contains(&byte))
    {
        return Err(PackVerificationError::InvalidIdentifier(label));
    }
    Ok(())
}

fn validate_sha256(value: &str) -> Result<(), PackVerificationError> {
    if !is_canonical_sha256(value) {
        return Err(PackVerificationError::InvalidSha256);
    }
    Ok(())
}

pub(super) fn is_canonical_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn validate_relative_path(value: &str) -> Result<(), PackVerificationError> {
    if value.is_empty() || value.contains('\\') || value.contains(':') || value.starts_with('/') {
        return Err(PackVerificationError::UnsafePath(value.to_owned()));
    }
    let components = Path::new(value).components().collect::<Vec<_>>();
    if components.is_empty() || components.len() > MAX_DEPTH {
        return Err(PackVerificationError::UnsafePath(value.to_owned()));
    }
    for component in components {
        let Component::Normal(name) = component else {
            return Err(PackVerificationError::UnsafePath(value.to_owned()));
        };
        let name = name
            .to_str()
            .ok_or_else(|| PackVerificationError::UnsafePath(value.to_owned()))?;
        if name.is_empty()
            || name == "."
            || name == ".."
            || name.len() > MAX_NAME_BYTES
            || name.ends_with(['.', ' '])
            || name.bytes().any(|byte| byte < 0x20)
            || is_reserved_windows_name(name)
            || matches!(name, MANIFEST_NAME | SIGNATURE_NAME)
        {
            return Err(PackVerificationError::UnsafePath(value.to_owned()));
        }
    }
    Ok(())
}

fn is_reserved_windows_name(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or(name).to_ascii_uppercase();
    matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL" | "CLOCK$")
        || (stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && matches!(stem.as_bytes()[3], b'1'..=b'9'))
}

fn validate_root(root: &Path) -> Result<(), PackVerificationError> {
    let metadata = fs::symlink_metadata(root).map_err(PackVerificationError::Io)?;
    if !metadata.is_dir() || is_link_or_reparse(&metadata) {
        return Err(PackVerificationError::NonRegularEntry(root.to_path_buf()));
    }
    reject_named_streams(root)?;
    Ok(())
}

fn verify_exact_tree(root: &Path, inventory: &[PayloadEntry]) -> Result<(), PackVerificationError> {
    let expected = inventory
        .iter()
        .map(|entry| entry.path.as_str())
        .collect::<BTreeSet<_>>();
    let mut observed = BTreeSet::new();
    let mut observed_casefolded = BTreeSet::new();
    let mut expected_directories = BTreeSet::new();
    for entry in inventory {
        let mut parent = Path::new(&entry.path).parent();
        while let Some(path) = parent {
            if path.as_os_str().is_empty() {
                break;
            }
            expected_directories.insert(path.to_string_lossy().replace('\\', "/"));
            parent = path.parent();
        }
    }
    let mut pending = vec![(root.to_path_buf(), 0_usize)];
    while let Some((directory, depth)) = pending.pop() {
        if depth > MAX_DEPTH {
            return Err(PackVerificationError::UnsafePath(
                directory.display().to_string(),
            ));
        }
        for entry in fs::read_dir(&directory).map_err(PackVerificationError::Io)? {
            let entry = entry.map_err(PackVerificationError::Io)?;
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path).map_err(PackVerificationError::Io)?;
            if is_link_or_reparse(&metadata) {
                return Err(PackVerificationError::NonRegularEntry(path));
            }
            let relative = path
                .strip_prefix(root)
                .map_err(|_| PackVerificationError::UnsafePath(path.display().to_string()))?
                .components()
                .map(|part| part.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/");
            if !observed_casefolded.insert(relative.to_ascii_lowercase()) {
                return Err(PackVerificationError::CaseCollision);
            }
            if metadata.is_dir() {
                reject_named_streams(&path)?;
                validate_relative_path(&relative)?;
                if !expected_directories.contains(&relative) {
                    return Err(PackVerificationError::TreeMismatch);
                }
                pending.push((path, depth + 1));
            } else if metadata.is_file() {
                reject_named_streams(&path)?;
                observed.insert(relative);
            } else {
                return Err(PackVerificationError::NonRegularEntry(path));
            }
        }
    }
    let reserved = BTreeSet::from([MANIFEST_NAME.to_owned(), SIGNATURE_NAME.to_owned()]);
    let payload_observed = observed
        .difference(&reserved)
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    if payload_observed != expected || !reserved.iter().all(|name| observed.contains(name)) {
        return Err(PackVerificationError::TreeMismatch);
    }
    Ok(())
}

fn verify_payload(
    root: &Path,
    pinned: Option<&PinnedPackRoot>,
    inventory: &[PayloadEntry],
) -> Result<Vec<File>, PackVerificationError> {
    let mut retained = Vec::with_capacity(inventory.len());
    for entry in inventory {
        let path = root.join(&entry.path);
        let mut file = match pinned {
            Some(lease) => lease.open_regular(Path::new(&entry.path))?,
            None => open_regular_no_follow(&path)?,
        };
        let metadata = file.metadata().map_err(PackVerificationError::Io)?;
        if metadata.len() != entry.size_bytes {
            return Err(PackVerificationError::SizeMismatch(entry.path.clone()));
        }
        reject_hardlink(&file, &metadata, &path)?;
        let mut hasher = Sha256::new();
        let mut buffer = [0_u8; 64 * 1024];
        loop {
            let read = file.read(&mut buffer).map_err(PackVerificationError::Io)?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
        }
        if format!("{:x}", hasher.finalize()) != entry.sha256 {
            return Err(PackVerificationError::PayloadDigestMismatch(
                entry.path.clone(),
            ));
        }
        retained.push(file);
    }
    Ok(retained)
}

fn read_bounded_regular_from(
    root: &Path,
    pinned: Option<&PinnedPackRoot>,
    relative: &Path,
    max_bytes: u64,
) -> Result<(Vec<u8>, File), PackVerificationError> {
    let path = root.join(relative);
    let mut file = match pinned {
        Some(lease) => lease.open_regular(relative)?,
        None => open_regular_no_follow(&path)?,
    };
    let metadata = file.metadata().map_err(PackVerificationError::Io)?;
    if metadata.len() > max_bytes {
        return Err(PackVerificationError::EnvelopeTooLarge);
    }
    reject_hardlink(&file, &metadata, &path)?;
    reject_named_streams(&path)?;
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.read_to_end(&mut bytes)
        .map_err(PackVerificationError::Io)?;
    Ok((bytes, file))
}

fn open_regular_no_follow(path: &Path) -> Result<File, PackVerificationError> {
    let mut options = OpenOptions::new();
    options.read(true);
    configure_no_follow(&mut options);
    let file = options.open(path).map_err(PackVerificationError::Io)?;
    let metadata = file.metadata().map_err(PackVerificationError::Io)?;
    if !metadata.is_file() || is_link_or_reparse(&metadata) {
        return Err(PackVerificationError::NonRegularEntry(path.to_path_buf()));
    }
    Ok(file)
}

#[cfg(unix)]
fn configure_no_follow(options: &mut OpenOptions) {
    use std::os::unix::fs::OpenOptionsExt;
    options.custom_flags(libc::O_NOFOLLOW);
}

#[cfg(windows)]
fn configure_no_follow(options: &mut OpenOptions) {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::{FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ};
    options
        .share_mode(FILE_SHARE_READ)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
}

#[cfg(not(any(unix, windows)))]
fn configure_no_follow(_options: &mut OpenOptions) {}

#[cfg(unix)]
fn open_regular_at(directory_fd: i32, relative: &Path) -> Result<File, PackVerificationError> {
    use std::os::unix::ffi::OsStrExt;

    let components = relative.components().collect::<Vec<_>>();
    if components.is_empty()
        || components
            .iter()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err(PackVerificationError::UnsafePath(
            relative.display().to_string(),
        ));
    }
    let duplicated = unsafe { libc::dup(directory_fd) };
    if duplicated < 0 {
        return Err(PackVerificationError::Io(io::Error::last_os_error()));
    }
    let mut current = unsafe { File::from_raw_fd(duplicated) };
    for component in &components[..components.len() - 1] {
        let name = CString::new(component.as_os_str().as_bytes())
            .map_err(|_| PackVerificationError::UnsafePath(relative.display().to_string()))?;
        let fd = unsafe {
            libc::openat(
                current.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(PackVerificationError::Io(io::Error::last_os_error()));
        }
        current = unsafe { File::from_raw_fd(fd) };
    }
    let name = CString::new(components.last().expect("non-empty").as_os_str().as_bytes())
        .map_err(|_| PackVerificationError::UnsafePath(relative.display().to_string()))?;
    let fd = unsafe {
        libc::openat(
            current.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(PackVerificationError::Io(io::Error::last_os_error()));
    }
    let file = unsafe { File::from_raw_fd(fd) };
    let metadata = file.metadata().map_err(PackVerificationError::Io)?;
    if !metadata.is_file() {
        return Err(PackVerificationError::NonRegularEntry(
            relative.to_path_buf(),
        ));
    }
    Ok(file)
}

#[cfg(windows)]
fn is_link_or_reparse(metadata: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    metadata.file_type().is_symlink()
        || metadata.file_attributes()
            & windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT
            != 0
}

#[cfg(not(windows))]
fn is_link_or_reparse(metadata: &fs::Metadata) -> bool {
    metadata.file_type().is_symlink()
}

#[cfg(unix)]
fn reject_hardlink(
    _file: &File,
    metadata: &fs::Metadata,
    path: &Path,
) -> Result<(), PackVerificationError> {
    use std::os::unix::fs::MetadataExt;
    if metadata.nlink() != 1 {
        return Err(PackVerificationError::Hardlink(path.to_path_buf()));
    }
    Ok(())
}

#[cfg(windows)]
fn reject_hardlink(
    file: &File,
    _metadata: &fs::Metadata,
    path: &Path,
) -> Result<(), PackVerificationError> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
    };
    let mut information = unsafe { std::mem::zeroed::<BY_HANDLE_FILE_INFORMATION>() };
    let succeeded = unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut information) };
    if succeeded == 0 {
        return Err(PackVerificationError::Io(io::Error::last_os_error()));
    }
    if information.nNumberOfLinks != 1 {
        return Err(PackVerificationError::Hardlink(path.to_path_buf()));
    }
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn reject_hardlink(
    _file: &File,
    _metadata: &fs::Metadata,
    _path: &Path,
) -> Result<(), PackVerificationError> {
    Ok(())
}

fn decode_hex_exact(value: &str, bytes: usize) -> Option<Vec<u8>> {
    if value.len() != bytes * 2 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    (0..bytes)
        .map(|index| u8::from_str_radix(&value[index * 2..index * 2 + 2], 16).ok())
        .collect()
}

#[cfg(windows)]
pub(super) fn reject_named_streams(path: &Path) -> Result<(), PackVerificationError> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
    use windows_sys::Win32::Storage::FileSystem::{
        FindClose, FindFirstStreamW, FindNextStreamW, FindStreamInfoStandard,
        WIN32_FIND_STREAM_DATA,
    };

    let wide = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let mut data = unsafe { std::mem::zeroed::<WIN32_FIND_STREAM_DATA>() };
    let handle = unsafe {
        FindFirstStreamW(
            wide.as_ptr(),
            FindStreamInfoStandard,
            &mut data as *mut _ as *mut _,
            0,
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        let error = io::Error::last_os_error();
        // Directories with no streams report ERROR_HANDLE_EOF immediately.
        return if error.raw_os_error() == Some(38) {
            Ok(())
        } else {
            Err(PackVerificationError::Io(error))
        };
    }
    let has_named_stream = |stream: &WIN32_FIND_STREAM_DATA| {
        let length = stream
            .cStreamName
            .iter()
            .position(|character| *character == 0)
            .unwrap_or(stream.cStreamName.len());
        String::from_utf16_lossy(&stream.cStreamName[..length]) != "::$DATA"
    };
    let mut named_stream = has_named_stream(&data);
    while !named_stream && unsafe { FindNextStreamW(handle, &mut data as *mut _ as *mut _) } != 0 {
        named_stream = has_named_stream(&data);
    }
    let final_error = io::Error::last_os_error();
    unsafe { FindClose(handle) };
    if named_stream {
        return Err(PackVerificationError::AlternateDataStream(
            path.to_path_buf(),
        ));
    }
    // ERROR_HANDLE_EOF (38) is the only successful enumeration terminator.
    if final_error.raw_os_error() != Some(38) {
        return Err(PackVerificationError::Io(final_error));
    }
    Ok(())
}

#[cfg(not(windows))]
pub(super) fn reject_named_streams(_path: &Path) -> Result<(), PackVerificationError> {
    Ok(())
}

#[cfg(unix)]
fn open_pinned_pack_root(
    canonical_store_root: &Path,
    components: [&StoreComponent; 2],
    digest: &str,
) -> Result<PinnedPackRoot, PackVerificationError> {
    use std::os::unix::ffi::OsStrExt;

    let root_name = CString::new(canonical_store_root.as_os_str().as_bytes()).map_err(|_| {
        PackVerificationError::UnsafePackStoreAncestor(canonical_store_root.to_path_buf())
    })?;
    let root_fd = unsafe {
        libc::open(
            root_name.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if root_fd < 0 {
        return Err(PackVerificationError::Io(io::Error::last_os_error()));
    }
    let mut handles = vec![unsafe { File::from_raw_fd(root_fd) }];
    let names = [components[0].as_str(), components[1].as_str(), digest];
    let mut path = canonical_store_root.to_path_buf();
    for name in names {
        let name_c = CString::new(name).expect("canonical store component has no NUL");
        let fd = unsafe {
            libc::openat(
                handles.last().expect("parent handle").as_raw_fd(),
                name_c.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(PackVerificationError::Io(io::Error::last_os_error()));
        }
        handles.push(unsafe { File::from_raw_fd(fd) });
        path.push(name);
    }
    let lease = PinnedPackRoot { path, handles };
    lease.recheck()?;
    Ok(lease)
}

#[cfg(windows)]
fn open_pinned_pack_root(
    canonical_store_root: &Path,
    components: [&StoreComponent; 2],
    digest: &str,
) -> Result<PinnedPackRoot, PackVerificationError> {
    let mut path = canonical_store_root.to_path_buf();
    let mut handles = Vec::with_capacity(4);
    handles.push(open_directory_no_follow(&path)?);
    for name in [components[0].as_str(), components[1].as_str(), digest] {
        path.push(name);
        handles.push(open_directory_no_follow(&path)?);
    }
    let lease = PinnedPackRoot { path, handles };
    lease.recheck()?;
    Ok(lease)
}

#[cfg(not(any(unix, windows)))]
fn open_pinned_pack_root(
    canonical_store_root: &Path,
    _components: [&StoreComponent; 2],
    _digest: &str,
) -> Result<PinnedPackRoot, PackVerificationError> {
    Err(PackVerificationError::UnsafePackStoreAncestor(
        canonical_store_root.to_path_buf(),
    ))
}

#[cfg(windows)]
fn open_directory_no_follow(path: &Path) -> Result<File, PackVerificationError> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
        FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    };
    let mut options = OpenOptions::new();
    options
        .read(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT);
    let file = options.open(path).map_err(PackVerificationError::Io)?;
    let metadata = file.metadata().map_err(PackVerificationError::Io)?;
    use std::os::windows::fs::MetadataExt;
    if !metadata.is_dir() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(PackVerificationError::UnsafePackStoreAncestor(
            path.to_path_buf(),
        ));
    }
    reject_named_streams(path)?;
    Ok(file)
}

#[cfg(unix)]
fn same_directory_identity(handle: &File, path: &Path) -> Result<bool, PackVerificationError> {
    use std::os::unix::fs::MetadataExt;
    let held = handle.metadata().map_err(PackVerificationError::Io)?;
    let observed = fs::symlink_metadata(path).map_err(PackVerificationError::Io)?;
    Ok(observed.is_dir()
        && !observed.file_type().is_symlink()
        && held.dev() == observed.dev()
        && held.ino() == observed.ino())
}

#[cfg(windows)]
fn same_directory_identity(handle: &File, path: &Path) -> Result<bool, PackVerificationError> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_REPARSE_POINT, GetFileInformationByHandle,
    };
    let observed = match open_directory_no_follow(path) {
        Ok(file) => file,
        Err(_) => return Ok(false),
    };
    let information = |file: &File| -> Result<BY_HANDLE_FILE_INFORMATION, PackVerificationError> {
        let mut value = unsafe { std::mem::zeroed::<BY_HANDLE_FILE_INFORMATION>() };
        if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut value) } == 0 {
            return Err(PackVerificationError::Io(io::Error::last_os_error()));
        }
        Ok(value)
    };
    let held = information(handle)?;
    let observed = information(&observed)?;
    Ok(held.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT == 0
        && held.dwVolumeSerialNumber == observed.dwVolumeSerialNumber
        && held.nFileIndexHigh == observed.nFileIndexHigh
        && held.nFileIndexLow == observed.nFileIndexLow)
}

#[cfg(not(any(unix, windows)))]
fn same_directory_identity(_handle: &File, _path: &Path) -> Result<bool, PackVerificationError> {
    Err(PackVerificationError::UnsupportedLeasePlatform)
}

#[derive(Debug, Error)]
pub(crate) enum PackVerificationError {
    #[error("worker-pack I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("worker-pack JSON is invalid: {0}")]
    Json(serde_json::Error),
    #[error("worker-pack {0} is not canonical JSON")]
    NonCanonical(&'static str),
    #[error("unsupported worker-pack schema")]
    UnsupportedSchema,
    #[error("worker-pack signature key is not trusted")]
    UnknownKey,
    #[error("worker-pack signature encoding is invalid")]
    InvalidSignatureEncoding,
    #[error("worker-pack signature is invalid")]
    BadSignature,
    #[error("worker-pack envelope exceeds its structural bound")]
    EnvelopeTooLarge,
    #[error("worker-pack {0} is invalid")]
    InvalidIdentifier(&'static str),
    #[error("worker-pack SHA-256 must be lowercase hexadecimal")]
    InvalidSha256,
    #[error("unsafe worker-pack path: {0}")]
    UnsafePath(String),
    #[error("worker-pack contains a nonregular or linked entry: {0}")]
    NonRegularEntry(PathBuf),
    #[error("worker-pack contains a hardlink: {0}")]
    Hardlink(PathBuf),
    #[error("worker-pack file contains an alternate data stream: {0}")]
    AlternateDataStream(PathBuf),
    #[error("worker-pack inventory must be strictly sorted and unique")]
    InventoryNotSorted,
    #[error("worker-pack inventory contains a case-colliding path")]
    CaseCollision,
    #[error("worker-pack inventory file count is invalid")]
    InvalidFileCount,
    #[error("worker-pack payload file is too large")]
    FileTooLarge,
    #[error("worker-pack aggregate payload is too large")]
    AggregateTooLarge,
    #[error("worker-pack exact tree does not equal its signed inventory")]
    TreeMismatch,
    #[error("worker-pack payload size mismatch: {0}")]
    SizeMismatch(String),
    #[error("worker-pack payload digest mismatch: {0}")]
    PayloadDigestMismatch(String),
    #[error("worker-pack identity digest mismatch")]
    DigestMismatch,
    #[error("worker-pack security epoch is below the embedded floor")]
    SecurityEpochTooOld,
    #[error("worker-pack protocol is incompatible")]
    ProtocolMismatch,
    #[error("worker-pack runtime ABI is incompatible")]
    AbiMismatch,
    #[error("worker-pack build identity is incompatible")]
    BuildMismatch,
    #[error("worker-pack target OS or architecture is incompatible")]
    ArchitectureMismatch,
    #[error("worker-pack backend is not allowed by this build")]
    BackendMismatch,
    #[error("worker-pack worker path is absent from the inventory")]
    WorkerMissing,
    #[error("verified worker-pack descriptor changed before launch")]
    DescriptorChanged,
    #[error("worker-pack immutable-store ancestor is unsafe: {0}")]
    UnsafePackStoreAncestor(PathBuf),
    #[error("worker-pack immutable-store ancestor identity changed: {0}")]
    PackStoreAncestorChanged(PathBuf),
    #[cfg(not(any(unix, windows)))]
    #[error("verified worker-pack leases are unsupported on this platform")]
    UnsupportedLeasePlatform,
}

#[cfg(test)]
pub(super) mod test_support {
    use super::*;
    use ring::signature::{Ed25519KeyPair, KeyPair};
    use std::time::{SystemTime, UNIX_EPOCH};

    const TEST_SEED: [u8; 32] = [7; 32];

    pub(crate) struct FixtureTrustRoot {
        public_key: Vec<u8>,
    }

    impl TrustRoot for FixtureTrustRoot {
        fn public_key(&self, key_id: &str) -> Option<&[u8]> {
            (key_id == "fixture-ed25519-v1").then_some(self.public_key.as_slice())
        }
    }

    pub(crate) fn temp_root(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "scribe-pack-{label}-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(&root).unwrap();
        root
    }

    pub(crate) fn base_manifest() -> PackManifest {
        PackManifest {
            schema_version: 1,
            pack_id: StoreComponent::new("scribe-vulkan").unwrap(),
            pack_version: StoreComponent::new("1.2.3").unwrap(),
            pack_digest: "0".repeat(64),
            security_epoch: 1,
            app_protocol_version: 5,
            worker_protocol_version: 5,
            runtime_abi_version: 1,
            app_build: crate::onnx_worker::DESKTOP_BUILD_ID.to_owned(),
            worker_build: crate::onnx_worker::INFERENCE_WORKER_BUILD_ID.to_owned(),
            backend: PackBackend::Vulkan,
            provider: "scribe-vulkan".to_owned(),
            target_os: std::env::consts::OS.to_owned(),
            target_arch: std::env::consts::ARCH.to_owned(),
            worker_path: "bin/worker.exe".to_owned(),
            payload: vec![PayloadEntry {
                path: "bin/worker.exe".to_owned(),
                size_bytes: 13,
                sha256: format!("{:x}", Sha256::digest(b"signed worker")),
            }],
        }
    }

    pub(crate) fn write_signed(
        root: &Path,
        mut manifest: PackManifest,
    ) -> &'static FixtureTrustRoot {
        fs::create_dir_all(root.join("bin")).unwrap();
        fs::write(root.join("bin/worker.exe"), b"signed worker").unwrap();
        manifest.pack_digest = compute_pack_digest(&manifest).unwrap();
        let manifest_bytes = serde_json::to_vec(&manifest).unwrap();
        fs::write(root.join(MANIFEST_NAME), &manifest_bytes).unwrap();
        let key_pair = Ed25519KeyPair::from_seed_unchecked(&TEST_SEED).unwrap();
        let signature = DetachedSignature {
            schema_version: 1,
            key_id: "fixture-ed25519-v1".to_owned(),
            signature_hex: key_pair
                .sign(&manifest_bytes)
                .as_ref()
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect(),
        };
        fs::write(
            root.join(SIGNATURE_NAME),
            serde_json::to_vec(&signature).unwrap(),
        )
        .unwrap();
        Box::leak(Box::new(FixtureTrustRoot {
            public_key: key_pair.public_key().as_ref().to_vec(),
        }))
    }

    pub(crate) fn fixture(root: &Path) -> (PackVerifier<'static>, VerifiedPack) {
        let trust = write_signed(root, base_manifest());
        let verifier = PackVerifier::new(
            trust,
            Compatibility {
                app_build: crate::onnx_worker::DESKTOP_BUILD_ID,
                worker_build: crate::onnx_worker::INFERENCE_WORKER_BUILD_ID,
                target_os: std::env::consts::OS,
                target_arch: std::env::consts::ARCH,
                allowed_backends: &[PackBackend::Vulkan],
            },
        );
        let verified = verifier.verify(root).unwrap();
        (verifier, verified)
    }

    pub(crate) fn leased_fixture(root: &Path) -> (PackVerifier<'static>, VerifiedPackLease) {
        let source = root.join("source");
        let (verifier, descriptor) = fixture(&source);
        let store_root = root.join("workers/packs");
        let parent = store_root
            .join(descriptor.pack_id.as_str())
            .join(descriptor.pack_version.as_str());
        fs::create_dir_all(&parent).unwrap();
        let final_root = parent.join(&descriptor.pack_digest);
        fs::rename(source, &final_root).unwrap();
        let pinned = PinnedPackRoot::open(
            &fs::canonicalize(&store_root).unwrap(),
            [&descriptor.pack_id, &descriptor.pack_version],
            &descriptor.pack_digest,
        )
        .unwrap();
        let lease = verifier.verify_pinned(pinned).unwrap();
        (verifier, lease)
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::*;
    use super::*;

    #[test]
    fn signed_exact_tree_verifies_and_is_reverified_for_launch() {
        let root = temp_root("valid");
        let (verifier, verified) = leased_fixture(&root);
        assert_eq!(
            verifier.launchable_worker(&verified).unwrap().path(),
            &verified.verified_pack().root.join("bin/worker.exe")
        );
        #[cfg(windows)]
        {
            assert!(fs::write(verified.worker_path(), b"tampered work").is_err());
            verifier.launchable_worker(&verified).unwrap();
        }
        #[cfg(unix)]
        {
            fs::write(verified.worker_path(), b"tampered work").unwrap();
            assert!(matches!(
                verifier.launchable_worker(&verified),
                Err(PackVerificationError::PayloadDigestMismatch(_))
            ));
        }
        drop(verified);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn bad_signature_and_unknown_key_fail_closed() {
        let root = temp_root("signature");
        let (verifier, _) = fixture(&root);
        let mut bytes = fs::read(root.join(MANIFEST_NAME)).unwrap();
        bytes.push(b'\n');
        fs::write(root.join(MANIFEST_NAME), bytes).unwrap();
        assert!(matches!(
            verifier.verify(&root),
            Err(PackVerificationError::BadSignature)
        ));
        fs::remove_dir_all(root).unwrap();

        let root = temp_root("unknown-key");
        let (_verifier, _) = fixture(&root);
        let production = PackVerifier::new(
            &ProductionTrustRoot,
            Compatibility::current(&[PackBackend::Vulkan]),
        );
        assert!(matches!(
            production.verify(&root),
            Err(PackVerificationError::UnknownKey)
        ));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn exact_tree_and_hostile_paths_fail_closed() {
        let root = temp_root("tree");
        let (verifier, _) = fixture(&root);
        fs::write(root.join("extra.dll"), b"extra").unwrap();
        assert!(matches!(
            verifier.verify(&root),
            Err(PackVerificationError::TreeMismatch)
        ));
        fs::remove_file(root.join("extra.dll")).unwrap();
        fs::remove_file(root.join("bin/worker.exe")).unwrap();
        assert!(matches!(
            verifier.verify(&root),
            Err(PackVerificationError::TreeMismatch)
        ));
        for hostile in [
            "../worker.exe",
            "/worker.exe",
            "C:/worker.exe",
            "bin/worker.exe:evil",
            "bin/CON.dll",
            "bin/worker.exe.",
        ] {
            assert!(
                validate_relative_path(hostile).is_err(),
                "accepted {hostile}"
            );
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn inventory_bounds_case_and_compatibility_are_enforced() {
        let collision = vec![
            PayloadEntry {
                path: "Bin/a.dll".into(),
                size_bytes: 1,
                sha256: "a".repeat(64),
            },
            PayloadEntry {
                path: "bin/A.dll".into(),
                size_bytes: 1,
                sha256: "b".repeat(64),
            },
        ];
        assert!(matches!(
            validate_inventory(&collision),
            Err(PackVerificationError::CaseCollision)
        ));
        let root = temp_root("metadata");
        let (verifier, _) = fixture(&root);
        let mut manifest = base_manifest();
        manifest.runtime_abi_version = 2;
        assert!(matches!(
            verifier.validate_manifest(&manifest),
            Err(PackVerificationError::AbiMismatch)
        ));
        manifest.runtime_abi_version = 1;
        manifest.target_arch = "wrong-arch".into();
        assert!(matches!(
            verifier.validate_manifest(&manifest),
            Err(PackVerificationError::ArchitectureMismatch)
        ));
        manifest.target_arch = std::env::consts::ARCH.into();
        manifest.backend = PackBackend::Cuda;
        assert!(matches!(
            verifier.validate_manifest(&manifest),
            Err(PackVerificationError::BackendMismatch)
        ));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn pack_identity_components_reject_store_path_ambiguity() {
        let root = temp_root("identity-components");
        let (verifier, _) = fixture(&root);
        for hostile in [
            ".",
            "..",
            "...",
            "a/b",
            "a\\b",
            "c:escape",
            "name:stream",
            "con",
            "con.txt",
            "prn.log",
            "aux.dll",
            "nul.json",
            "com1",
            "com1.dll",
            "lpt9",
            "lpt9.sys",
            "name.",
            "name ",
            "Uppercase",
            "café",
            "bad\u{1f}",
            "-leading",
            "trailing-",
        ] {
            let mut manifest = base_manifest();
            manifest.pack_id = StoreComponent::test_unchecked(hostile);
            assert!(
                matches!(
                    verifier.validate_manifest(&manifest),
                    Err(PackVerificationError::InvalidIdentifier("pack id"))
                ),
                "accepted pack id {hostile:?}"
            );

            let mut manifest = base_manifest();
            manifest.pack_version = StoreComponent::test_unchecked(hostile);
            assert!(
                matches!(
                    verifier.validate_manifest(&manifest),
                    Err(PackVerificationError::InvalidIdentifier("pack version"))
                ),
                "accepted pack version {hostile:?}"
            );
        }

        let signed_hostile_root = temp_root("signed-hostile-identity");
        let mut signed_hostile = base_manifest();
        signed_hostile.pack_id = StoreComponent::test_unchecked("..");
        let trust = write_signed(&signed_hostile_root, signed_hostile);
        let signed_hostile_verifier = PackVerifier::new(
            trust,
            Compatibility {
                app_build: crate::onnx_worker::DESKTOP_BUILD_ID,
                worker_build: crate::onnx_worker::INFERENCE_WORKER_BUILD_ID,
                target_os: std::env::consts::OS,
                target_arch: std::env::consts::ARCH,
                allowed_backends: &[PackBackend::Vulkan],
            },
        );
        assert!(matches!(
            signed_hostile_verifier.verify(&signed_hostile_root),
            Err(PackVerificationError::InvalidIdentifier("pack id"))
        ));

        let semantic_root = temp_root("semantic-version-component");
        let mut manifest = base_manifest();
        manifest.pack_version = StoreComponent::new("1.2.3-beta.1").unwrap();
        let trust = write_signed(&semantic_root, manifest);
        let semantic_verifier = PackVerifier::new(
            trust,
            Compatibility {
                app_build: crate::onnx_worker::DESKTOP_BUILD_ID,
                worker_build: crate::onnx_worker::INFERENCE_WORKER_BUILD_ID,
                target_os: std::env::consts::OS,
                target_arch: std::env::consts::ARCH,
                allowed_backends: &[PackBackend::Vulkan],
            },
        );
        assert_eq!(
            semantic_verifier
                .verify(&semantic_root)
                .unwrap()
                .pack_version
                .as_str(),
            "1.2.3-beta.1"
        );
        fs::remove_dir_all(signed_hostile_root).unwrap();
        fs::remove_dir_all(semantic_root).unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn schema_protocol_build_worker_digest_and_size_bounds_are_enforced() {
        let root = temp_root("metadata-bounds");
        let (verifier, _) = fixture(&root);

        let mut manifest = base_manifest();
        manifest.schema_version = PACK_SCHEMA_VERSION + 1;
        assert!(matches!(
            verifier.validate_manifest(&manifest),
            Err(PackVerificationError::UnsupportedSchema)
        ));

        let mut manifest = base_manifest();
        manifest.worker_protocol_version = APP_PROTOCOL_VERSION - 1;
        assert!(matches!(
            verifier.validate_manifest(&manifest),
            Err(PackVerificationError::ProtocolMismatch)
        ));

        let mut manifest = base_manifest();
        manifest.worker_build = "incompatible-worker-build-v1".into();
        assert!(matches!(
            verifier.validate_manifest(&manifest),
            Err(PackVerificationError::BuildMismatch)
        ));

        let mut manifest = base_manifest();
        manifest.worker_path = "bin/missing-worker.exe".into();
        assert!(matches!(
            verifier.validate_manifest(&manifest),
            Err(PackVerificationError::WorkerMissing)
        ));

        let mut manifest = base_manifest();
        manifest.pack_digest = "f".repeat(64);
        assert!(matches!(
            verifier.validate_manifest(&manifest),
            Err(PackVerificationError::DigestMismatch)
        ));

        let too_many = (0..=MAX_FILES)
            .map(|index| PayloadEntry {
                path: format!("files/{index:03}.bin"),
                size_bytes: 1,
                sha256: "a".repeat(64),
            })
            .collect::<Vec<_>>();
        assert!(matches!(
            validate_inventory(&too_many),
            Err(PackVerificationError::InvalidFileCount)
        ));

        let too_large = vec![PayloadEntry {
            path: "files/large.bin".into(),
            size_bytes: MAX_FILE_BYTES + 1,
            sha256: "a".repeat(64),
        }];
        assert!(matches!(
            validate_inventory(&too_large),
            Err(PackVerificationError::FileTooLarge)
        ));

        let aggregate_too_large = (0..3)
            .map(|index| PayloadEntry {
                path: format!("files/{index}.bin"),
                size_bytes: MAX_FILE_BYTES,
                sha256: "a".repeat(64),
            })
            .collect::<Vec<_>>();
        assert!(matches!(
            validate_inventory(&aggregate_too_large),
            Err(PackVerificationError::AggregateTooLarge)
        ));
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn alternate_data_streams_and_hardlinks_are_rejected() {
        let root = temp_root("windows-links");
        let (verifier, _) = fixture(&root);
        let worker = root.join("bin/worker.exe");
        let mut stream = worker.as_os_str().to_os_string();
        stream.push(":untrusted");
        fs::write(PathBuf::from(stream), b"hidden").unwrap();
        assert!(matches!(
            verifier.verify(&root),
            Err(PackVerificationError::AlternateDataStream(_))
        ));

        let mut stream = worker.as_os_str().to_os_string();
        stream.push(":untrusted");
        fs::remove_file(PathBuf::from(stream)).unwrap();
        let external = root.with_extension("external");
        fs::write(&external, b"signed worker").unwrap();
        fs::remove_file(&worker).unwrap();
        fs::hard_link(&external, &worker).unwrap();
        assert!(matches!(
            verifier.verify(&root),
            Err(PackVerificationError::Hardlink(_))
        ));
        fs::remove_dir_all(root).unwrap();
        fs::remove_file(external).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn root_and_nested_directory_alternate_data_streams_are_rejected() {
        let root = temp_root("windows-directory-streams");
        let (verifier, _) = fixture(&root);
        let mut root_stream = root.as_os_str().to_os_string();
        root_stream.push(":untrusted-root");
        let root_stream = PathBuf::from(root_stream);
        fs::write(&root_stream, b"hidden").unwrap();
        assert!(matches!(
            verifier.verify(&root),
            Err(PackVerificationError::AlternateDataStream(path)) if path == root
        ));
        fs::remove_file(&root_stream).unwrap();

        let directory = root.join("bin");
        let mut directory_stream = directory.as_os_str().to_os_string();
        directory_stream.push(":untrusted-directory");
        let directory_stream = PathBuf::from(directory_stream);
        fs::write(&directory_stream, b"hidden").unwrap();
        assert!(matches!(
            verifier.verify(&root),
            Err(PackVerificationError::AlternateDataStream(path)) if path == directory
        ));
        fs::remove_file(directory_stream).unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_and_hardlinks_are_rejected() {
        use std::os::unix::fs::symlink;
        let root = temp_root("links");
        let (verifier, _) = fixture(&root);
        let original = root.join("bin/worker.exe");
        let external = root.with_extension("external");
        fs::write(&external, b"signed worker").unwrap();
        fs::remove_file(&original).unwrap();
        symlink(&external, &original).unwrap();
        assert!(matches!(
            verifier.verify(&root),
            Err(PackVerificationError::NonRegularEntry(_))
        ));
        fs::remove_file(&original).unwrap();
        fs::hard_link(&external, &original).unwrap();
        assert!(matches!(
            verifier.verify(&root),
            Err(PackVerificationError::Hardlink(_))
        ));
        fs::remove_dir_all(root).unwrap();
        fs::remove_file(external).unwrap();
    }
}
