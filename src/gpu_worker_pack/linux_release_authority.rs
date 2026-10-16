//! Linux release-manifest semantics for the embedded GPU catalog boundary.
//!
//! A successful result binds canonical release metadata to canonical catalog bytes. It does not
//! authenticate a pack, verify a signature, admit an epoch floor, or authorize installation.
//! A positive epoch with no entries is structurally valid future metadata that binds zero packs.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::manifest::{self, PackBackend, StoreComponent};

const LINUX_AUTHORITY_SCHEMA_VERSION: u16 = 1;
const PACK_CATALOG_SCHEMA_VERSION: u16 = 1;
const LINUX_TARGET: &str = "x86_64-unknown-linux-gnu";
const LINUX_TARGET_OS: &str = "linux";
const LINUX_TARGET_ARCH: &str = "x86_64";
const MAX_DOCUMENT_BYTES: usize = 512 * 1024;
const MAX_PRODUCTION_PACKS: usize = 8;
const MAX_RELEASE_SECURITY_EPOCH: u64 = 9_007_199_254_740_991;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ExpectedBuild<'a> {
    pub(crate) app_version: &'a str,
    pub(crate) build_revision: &'a str,
    pub(crate) app_protocol_version: u16,
    pub(crate) runtime_abi_version: u16,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct LinuxCatalogReleaseBinding {
    pub(crate) release_security_epoch: u64,
    pub(crate) entry_count: usize,
}

/// The only Linux release outcomes available at the embedded-catalog boundary.
///
/// This is crate-private so platform dispatch can map the result to its stable
/// diagnostic without exposing a pack-admission API.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LinuxReleasePolicyDecision {
    SupportDisabled,
    MetadataRejected,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LinuxReleaseAuthority {
    schema_version: u16,
    target: String,
    catalog_sha256: String,
    release_security_epoch: u64,
    entries: Vec<LinuxReleaseAuthorityEntry>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LinuxReleaseAuthorityEntry {
    app_version: String,
    build_revision: String,
    app_protocol_version: u16,
    pack_id: StoreComponent,
    pack_version: StoreComponent,
    pack_digest: String,
    security_epoch: u64,
    runtime_abi_version: u16,
    backend: PackBackend,
    provider: String,
    worker_relative_path: String,
    root: String,
    installed_size_bytes: u64,
    compressed_size_bytes: u64,
    files: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PackCatalog {
    schema_version: u16,
    packs: Vec<PackCatalogEntry>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PackCatalogEntry {
    pack_id: StoreComponent,
    pack_version: StoreComponent,
    pack_digest: String,
    security_epoch: u64,
    runtime_abi_version: u16,
    backend: PackBackend,
    provider: String,
    target_os: String,
    target_arch: String,
    worker_relative_path: String,
    root: String,
    installed_size_bytes: u64,
    compressed_size_bytes: u64,
    files: Vec<String>,
}

pub(crate) fn validate_catalog_release_binding(
    catalog_bytes: &[u8],
    authority_bytes: &[u8],
    expected: ExpectedBuild<'_>,
) -> Option<LinuxCatalogReleaseBinding> {
    if !bounded_document(catalog_bytes) || !bounded_document(authority_bytes) {
        return None;
    }

    let authority = parse_canonical::<LinuxReleaseAuthority>(authority_bytes)?;
    if authority.schema_version != LINUX_AUTHORITY_SCHEMA_VERSION
        || authority.target != LINUX_TARGET
        || authority.entries.len() > MAX_PRODUCTION_PACKS
        || !manifest::is_canonical_sha256(&authority.catalog_sha256)
        || format!("{:x}", Sha256::digest(catalog_bytes)) != authority.catalog_sha256
        || authority.release_security_epoch > MAX_RELEASE_SECURITY_EPOCH
        || (authority.release_security_epoch == 0 && !authority.entries.is_empty())
        || !authority.entries.iter().all(|entry| {
            entry.security_epoch == authority.release_security_epoch
                && validate_authority_entry(entry, expected)
        })
    {
        return None;
    }

    let catalog = parse_canonical::<PackCatalog>(catalog_bytes)?;
    if catalog.schema_version != PACK_CATALOG_SCHEMA_VERSION
        || catalog.packs.len() != authority.entries.len()
        || catalog.packs.len() > MAX_PRODUCTION_PACKS
        || !catalog_roots_are_unique(&catalog.packs)
        || !catalog
            .packs
            .iter()
            .zip(&authority.entries)
            .all(|(catalog_entry, authority_entry)| {
                validate_catalog_entry(catalog_entry)
                    && entries_match(catalog_entry, authority_entry)
            })
    {
        return None;
    }

    Some(LinuxCatalogReleaseBinding {
        release_security_epoch: authority.release_security_epoch,
        entry_count: authority.entries.len(),
    })
}

/// Applies the Linux release's deliberately narrow default-deny policy.
///
/// Structural validation deliberately accepts future positive epochs and
/// populated catalogs. The current Linux package, however, has authority only
/// for the reviewed empty epoch-zero catalog; every other input is rejected.
pub(crate) fn linux_release_policy_decision(
    catalog_bytes: &[u8],
    authority_bytes: &[u8],
    expected: ExpectedBuild<'_>,
) -> LinuxReleasePolicyDecision {
    match validate_catalog_release_binding(catalog_bytes, authority_bytes, expected) {
        Some(binding) if binding.release_security_epoch == 0 && binding.entry_count == 0 => {
            LinuxReleasePolicyDecision::SupportDisabled
        }
        _ => LinuxReleasePolicyDecision::MetadataRejected,
    }
}

fn catalog_roots_are_unique(entries: &[PackCatalogEntry]) -> bool {
    let mut canonical_roots = BTreeSet::new();
    entries
        .iter()
        .all(|entry| canonical_roots.insert(entry.root.to_ascii_lowercase()))
}

fn bounded_document(bytes: &[u8]) -> bool {
    !bytes.is_empty() && bytes.len() <= MAX_DOCUMENT_BYTES
}

fn parse_canonical<T>(bytes: &[u8]) -> Option<T>
where
    T: for<'de> Deserialize<'de> + Serialize,
{
    let document = serde_json::from_slice::<T>(bytes).ok()?;
    (serde_json::to_vec(&document).ok()?.as_slice() == bytes).then_some(document)
}

fn validate_authority_entry(
    entry: &LinuxReleaseAuthorityEntry,
    expected: ExpectedBuild<'_>,
) -> bool {
    entry.app_version == expected.app_version
        && entry.build_revision == expected.build_revision
        && entry.app_protocol_version == expected.app_protocol_version
        && entry.runtime_abi_version == expected.runtime_abi_version
        && canonical_app_version(&entry.app_version)
        && manifest::validate_build_identity(&entry.build_revision, "release build revision")
            .is_ok()
        && entry.app_protocol_version != 0
        && entry.runtime_abi_version != 0
        && valid_common_entry(
            &entry.pack_id,
            &entry.pack_version,
            &entry.pack_digest,
            entry.security_epoch,
            entry.backend,
            &entry.provider,
            &entry.worker_relative_path,
            &entry.root,
            entry.installed_size_bytes,
            entry.compressed_size_bytes,
            &entry.files,
        )
}

fn canonical_app_version(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 96
        && value.bytes().all(|byte| (0x21..=0x7e).contains(&byte))
}

fn validate_catalog_entry(entry: &PackCatalogEntry) -> bool {
    entry.target_os == LINUX_TARGET_OS
        && entry.target_arch == LINUX_TARGET_ARCH
        && valid_common_entry(
            &entry.pack_id,
            &entry.pack_version,
            &entry.pack_digest,
            entry.security_epoch,
            entry.backend,
            &entry.provider,
            &entry.worker_relative_path,
            &entry.root,
            entry.installed_size_bytes,
            entry.compressed_size_bytes,
            &entry.files,
        )
}

#[allow(clippy::too_many_arguments)]
fn valid_common_entry(
    pack_id: &StoreComponent,
    pack_version: &StoreComponent,
    pack_digest: &str,
    security_epoch: u64,
    backend: PackBackend,
    provider: &str,
    worker_relative_path: &str,
    root: &str,
    installed_size_bytes: u64,
    compressed_size_bytes: u64,
    files: &[String],
) -> bool {
    let expected_provider = match backend {
        PackBackend::Cuda => "transcribe-cpp-ggml-cuda",
        PackBackend::Vulkan => "transcribe-cpp-ggml-vulkan",
        PackBackend::Metal => return false,
    };
    let expected_root = format!(
        "workers/packs/{}/{}/{}",
        pack_id.as_str(),
        pack_version.as_str(),
        pack_digest
    );

    pack_id.is_canonical()
        && pack_version.is_canonical()
        && manifest::is_canonical_sha256(pack_digest)
        && (1..=MAX_RELEASE_SECURITY_EPOCH).contains(&security_epoch)
        && provider == expected_provider
        && canonical_payload_path(worker_relative_path)
        && root == expected_root
        && (1..=manifest::MAX_AGGREGATE_BYTES).contains(&installed_size_bytes)
        && (1..=manifest::MAX_AGGREGATE_BYTES).contains(&compressed_size_bytes)
        && validate_full_inventory(root, worker_relative_path, files)
}

fn validate_full_inventory(root: &str, worker_relative_path: &str, files: &[String]) -> bool {
    if !(3..=manifest::MAX_FILES + 2).contains(&files.len()) {
        return false;
    }

    let prefix = format!("{root}/");
    let required_worker = format!("{prefix}{worker_relative_path}");
    let required_manifest = format!("{prefix}{}", manifest::MANIFEST_NAME);
    let required_signature = format!("{prefix}{}", manifest::SIGNATURE_NAME);
    let mut previous: Option<&str> = None;
    let mut casefolded = BTreeSet::new();
    for file in files {
        if previous.is_some_and(|prior| prior >= file.as_str())
            || !casefolded.insert(file.to_ascii_lowercase())
        {
            return false;
        }
        previous = Some(file);

        let Some(relative) = file.strip_prefix(&prefix) else {
            return false;
        };
        if relative != manifest::MANIFEST_NAME
            && relative != manifest::SIGNATURE_NAME
            && !canonical_payload_path(relative)
        {
            return false;
        }
    }

    files.binary_search(&required_worker).is_ok()
        && files.binary_search(&required_manifest).is_ok()
        && files.binary_search(&required_signature).is_ok()
}

fn canonical_payload_path(path: &str) -> bool {
    !path
        .split('/')
        .any(|component| component.is_empty() || matches!(component, "." | ".."))
        && manifest::validate_relative_path(path).is_ok()
}

fn entries_match(catalog: &PackCatalogEntry, authority: &LinuxReleaseAuthorityEntry) -> bool {
    catalog.pack_id == authority.pack_id
        && catalog.pack_version == authority.pack_version
        && catalog.pack_digest == authority.pack_digest
        && catalog.security_epoch == authority.security_epoch
        && catalog.runtime_abi_version == authority.runtime_abi_version
        && catalog.backend == authority.backend
        && catalog.provider == authority.provider
        && catalog.worker_relative_path == authority.worker_relative_path
        && catalog.root == authority.root
        && catalog.installed_size_bytes == authority.installed_size_bytes
        && catalog.compressed_size_bytes == authority.compressed_size_bytes
        && catalog.files == authority.files
}
