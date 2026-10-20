//! Compiled release inclusion, independent of discovery, health, or installation.
//! Project signatures authenticate pack bytes; this document authorizes inclusion
//! in one exact application build. Neither alone grants execution or Auto.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::manifest;

pub(crate) const MAX_RELEASE_SECURITY_EPOCH: u64 = 9_007_199_254_740_991;
pub(crate) const MAX_PRODUCTION_PACKS: usize = 8;
pub(crate) const MAX_RELEASE_AUTHORITY_BYTES: usize = 512 * 1024;

#[derive(Clone, Copy)]
pub(crate) struct ReleaseBuildIdentity<'a> {
    pub(crate) app_version: &'a str,
    pub(crate) build_revision: &'a str,
    pub(crate) app_protocol_version: u16,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PackCatalog {
    pub(crate) schema_version: u16,
    pub(crate) packs: Vec<PackCatalogEntry>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PackCatalogEntry {
    pub(crate) pack_id: manifest::StoreComponent,
    pub(crate) pack_version: manifest::StoreComponent,
    pub(crate) pack_digest: String,
    pub(crate) security_epoch: u64,
    pub(crate) runtime_abi_version: u16,
    pub(crate) backend: manifest::PackBackend,
    pub(crate) provider: String,
    pub(crate) target_os: String,
    pub(crate) target_arch: String,
    pub(crate) worker_relative_path: String,
    pub(crate) root: String,
    pub(crate) installed_size_bytes: u64,
    pub(crate) compressed_size_bytes: u64,
    pub(crate) files: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PackReleaseAuthority {
    pub(crate) schema_version: u16,
    pub(crate) catalog_sha256: String,
    pub(crate) release_security_epoch: u64,
    pub(crate) keychain_access_group: String,
    pub(crate) entries: Vec<PackReleaseAuthorityEntry>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PackReleaseAuthorityEntry {
    pub(crate) app_version: String,
    pub(crate) build_revision: String,
    pub(crate) app_protocol_version: u16,
    pub(crate) pack_id: manifest::StoreComponent,
    pub(crate) pack_version: manifest::StoreComponent,
    pub(crate) pack_digest: String,
    pub(crate) security_epoch: u64,
    pub(crate) runtime_abi_version: u16,
    pub(crate) backend: manifest::PackBackend,
    pub(crate) provider: String,
    pub(crate) target_os: String,
    pub(crate) target_arch: String,
    pub(crate) worker_relative_path: String,
    pub(crate) root: String,
    pub(crate) installed_size_bytes: u64,
    pub(crate) compressed_size_bytes: u64,
    pub(crate) files: Vec<String>,
}

impl PackReleaseAuthorityEntry {
    fn matches_catalog_entry(
        &self,
        entry: &PackCatalogEntry,
        build: ReleaseBuildIdentity<'_>,
    ) -> bool {
        self.app_version == build.app_version
            && self.build_revision == build.build_revision
            && self.app_protocol_version == build.app_protocol_version
            && self.pack_id == entry.pack_id
            && self.pack_version == entry.pack_version
            && self.pack_digest == entry.pack_digest
            && self.security_epoch == entry.security_epoch
            && self.runtime_abi_version == entry.runtime_abi_version
            && self.backend == entry.backend
            && self.provider == entry.provider
            && self.target_os == entry.target_os
            && self.target_arch == entry.target_arch
            && self.worker_relative_path == entry.worker_relative_path
            && self.root == entry.root
            && self.installed_size_bytes == entry.installed_size_bytes
            && self.compressed_size_bytes == entry.compressed_size_bytes
            && self.files == entry.files
    }
}

pub(crate) fn validated_release_authority(
    catalog_bytes: &[u8],
    authority_bytes: &[u8],
    build: ReleaseBuildIdentity<'_>,
) -> Option<PackReleaseAuthority> {
    if catalog_bytes.len() > MAX_RELEASE_AUTHORITY_BYTES {
        return None;
    }
    let authority = validated_release_authority_document(authority_bytes)?;
    if !manifest::is_canonical_sha256(&authority.catalog_sha256)
        || format!("{:x}", Sha256::digest(catalog_bytes)) != authority.catalog_sha256
    {
        return None;
    }
    let catalog = serde_json::from_slice::<PackCatalog>(catalog_bytes).ok()?;
    if catalog.schema_version != 1
        || catalog.packs.len() != authority.entries.len()
        || !catalog
            .packs
            .iter()
            .zip(&authority.entries)
            .all(|(catalog, authority)| authority.matches_catalog_entry(catalog, build))
    {
        return None;
    }
    Some(authority)
}

pub(crate) fn validated_release_authority_document(
    authority_bytes: &[u8],
) -> Option<PackReleaseAuthority> {
    if authority_bytes.len() > MAX_RELEASE_AUTHORITY_BYTES {
        return None;
    }
    let authority = serde_json::from_slice::<PackReleaseAuthority>(authority_bytes).ok()?;
    if authority.schema_version != 2
        || authority.entries.len() > MAX_PRODUCTION_PACKS
        || serde_json::to_vec(&authority).ok().as_deref() != Some(authority_bytes)
        || authority.release_security_epoch > MAX_RELEASE_SECURITY_EPOCH
        || !authority
            .entries
            .iter()
            .all(|entry| entry.security_epoch == authority.release_security_epoch)
        || (authority.release_security_epoch == 0
            && (!authority.entries.is_empty() || !authority.keychain_access_group.is_empty()))
        || (authority.release_security_epoch > 0
            && !canonical_keychain_access_group(&authority.keychain_access_group))
    {
        return None;
    }
    Some(authority)
}

pub(crate) fn canonical_keychain_access_group(group: &str) -> bool {
    let Some((team_id, suffix)) = group.split_at_checked(10) else {
        return false;
    };
    suffix == ".com.scribe.local-transcriber"
        && team_id
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
}
