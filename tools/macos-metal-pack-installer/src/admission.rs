//! Content admission only: no filesystem protection or installation authority.
#![cfg_attr(not(target_os = "macos"), allow(dead_code))]
use anyhow::{Context, Result, bail};

use crate::manifest::{self, PackBackend, PackVerifier, PinnedPackRoot, VerifiedPackLease};
#[cfg(any(target_os = "macos", test))]
use crate::manifest::{Compatibility, ProductionTrustRoot};
use crate::release_authority::{
    PackReleaseAuthority, PackReleaseAuthorityEntry, ReleaseBuildIdentity,
    validated_release_authority,
};

const WORKER: &str = "worker/scribe-inference-worker";
const PROVIDER: &str = "transcribe-cpp-metal";
const CATALOG: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/catalog.json"));
const AUTHORITY: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/authority.json"));

fn build_identity() -> ReleaseBuildIdentity<'static> {
    ReleaseBuildIdentity {
        app_version: env!("CARGO_PKG_VERSION"),
        build_revision: env!("SCRIBE_BUILD_REVISION"),
        app_protocol_version: manifest::APP_PROTOCOL_VERSION,
    }
}

pub(crate) fn compiled_release() -> Result<PackReleaseAuthority> {
    validated_release_authority(CATALOG, AUTHORITY, build_identity())
        .context("compiled catalog or release authority is invalid")
}

/// Constructible only inside this module from the compiled release (or private
/// unit fixtures). A positive epoch or project signature alone is not a permit.
struct MetalReleasePermit {
    entry: PackReleaseAuthorityEntry,
}

impl MetalReleasePermit {
    fn for_native_release(release: PackReleaseAuthority) -> Result<Self> {
        let mut entries = release.entries.into_iter().filter(|entry| {
            entry.backend == PackBackend::Metal
                && entry.target_os == "macos"
                && entry.target_arch == std::env::consts::ARCH
        });
        let entry = entries
            .next()
            .context("release includes no native Metal pack")?;
        if entries.next().is_some() {
            bail!("release includes ambiguous native Metal packs");
        }
        if release.release_security_epoch == 0
            || entry.security_epoch != release.release_security_epoch
            || !entry.pack_id.is_canonical()
            || !entry.pack_version.is_canonical()
            || !manifest::is_canonical_sha256(&entry.pack_digest)
            || entry.provider != PROVIDER
            || entry.runtime_abi_version != manifest::RUNTIME_ABI_VERSION
            || entry.worker_relative_path != WORKER
            || entry.installed_size_bytes == 0
            || entry.installed_size_bytes > manifest::MAX_AGGREGATE_BYTES
            || entry.compressed_size_bytes == 0
        {
            bail!("release entry is incompatible with the Metal installer");
        }
        let root = format!(
            "workers/packs/{}/{}/{}",
            entry.pack_id.as_str(),
            entry.pack_version.as_str(),
            entry.pack_digest
        );
        let files = [manifest::MANIFEST_NAME, manifest::SIGNATURE_NAME, WORKER]
            .map(|path| format!("{root}/{path}"));
        if entry.root != root || entry.files != files {
            bail!("release entry does not authorize the exact single-worker tree");
        }
        Ok(Self { entry })
    }

    fn matches(&self, lease: &VerifiedPackLease) -> bool {
        let pack = lease.verified_pack();
        let entry = &self.entry;
        let files = lease.copy_entries();
        pack.pack_id == entry.pack_id
            && pack.pack_version == entry.pack_version
            && pack.pack_digest == entry.pack_digest
            && pack.security_epoch == entry.security_epoch
            && pack.runtime_abi_version == entry.runtime_abi_version
            && pack.backend == entry.backend
            && pack.provider == entry.provider
            && pack.target_os == entry.target_os
            && pack.target_arch == entry.target_arch
            && pack.worker_relative_path == entry.worker_relative_path
            && files.len() == 3
            && files.iter().map(|file| file.path.as_str()).eq([
                manifest::MANIFEST_NAME,
                manifest::SIGNATURE_NAME,
                WORKER,
            ])
            && files
                .iter()
                .try_fold(0u64, |sum, file| sum.checked_add(file.size_bytes))
                == Some(entry.installed_size_bytes)
    }
}

/// Retained content authority, not a path/boolean or protected launch approval.
/// Future copying must keep this alive, reverify, copy through lease handles,
/// hash readback independently, then admit the protected destination separately.
pub(crate) struct AdmittedSource<'a> {
    permit: MetalReleasePermit,
    lease: VerifiedPackLease,
    verifier: PackVerifier<'a>,
}

impl AdmittedSource<'_> {
    pub(crate) fn reverify(&self) -> Result<()> {
        self.verifier.launchable_worker(&self.lease)?; // Verification only; never spawn.
        if !self.permit.matches(&self.lease) {
            bail!("source no longer matches its exact release permit");
        }
        Ok(())
    }
}

fn admit_pinned<'a>(
    permit: MetalReleasePermit,
    pinned: PinnedPackRoot,
    verifier: PackVerifier<'a>,
) -> Result<AdmittedSource<'a>> {
    let lease = verifier.verify_pinned(pinned)?;
    if !permit.matches(&lease) {
        bail!("signed source is not the release-authorized Metal pack");
    }
    Ok(AdmittedSource {
        permit,
        lease,
        verifier,
    })
}

#[cfg(target_os = "macos")]
pub(crate) fn admit_source(candidate: &std::path::Path) -> Result<AdmittedSource<'static>> {
    // No candidate opening when the compiled release is empty/default-deny.
    let permit = MetalReleasePermit::for_native_release(compiled_release()?)?;
    let pinned = pin_candidate(candidate)?;
    let verifier = PackVerifier::new(
        &ProductionTrustRoot,
        Compatibility {
            target_os: "macos",
            ..Compatibility::current(&[PackBackend::Metal])
        },
    );
    admit_pinned(permit, pinned, verifier)
}

#[cfg(target_os = "macos")]
fn pin_candidate(candidate: &std::path::Path) -> Result<PinnedPackRoot> {
    use crate::macos_worker_fs::{NodeKind, open_absolute_directory, openat_node};
    use std::os::fd::AsRawFd;
    use std::path::{Component, Path, PathBuf};

    if !candidate.is_absolute() || candidate.as_os_str().len() > 4096 {
        bail!("candidate must be a bounded absolute path");
    }
    // Path equality normalizes separators/dots; compare raw spellings instead.
    let rebuilt = candidate.components().collect::<PathBuf>();
    if rebuilt.as_os_str() != candidate.as_os_str()
        || candidate
            .components()
            .any(|part| !matches!(part, Component::RootDir | Component::Normal(_)))
    {
        bail!("candidate path contains an alias or non-normal component");
    }
    let mut handles = vec![open_absolute_directory(Path::new("/"))?];
    for component in candidate.components().skip(1) {
        let Component::Normal(name) = component else {
            unreachable!("validated component")
        };
        let next = openat_node(
            handles.last().unwrap().as_raw_fd(),
            name,
            NodeKind::Directory,
        )?;
        handles.push(next);
    }
    Ok(PinnedPackRoot::from_anchored_handles(
        candidate.to_path_buf(),
        handles,
    )?)
}

#[cfg(test)]
mod tests;
