//! Content admission only: no filesystem protection or installation authority.
#![cfg_attr(not(target_os = "macos"), allow(dead_code))]
use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};
use std::io::{self, Cursor, Read};

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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FileSlot {
    Manifest,
    Signature,
    Worker,
}
impl FileSlot {
    pub(crate) const ALL: [Self; 3] = [Self::Manifest, Self::Signature, Self::Worker];
    pub(crate) fn index(self) -> usize {
        match self {
            Self::Manifest => 0,
            Self::Signature => 1,
            Self::Worker => 2,
        }
    }
    pub(crate) fn path(self) -> &'static str {
        match self {
            Self::Manifest => manifest::MANIFEST_NAME,
            Self::Signature => manifest::SIGNATURE_NAME,
            Self::Worker => WORKER,
        }
    }
    pub(crate) fn basename(self) -> &'static str {
        match self {
            Self::Worker => "scribe-inference-worker",
            _ => self.path(),
        }
    }
    pub(crate) fn maximum(self) -> u64 {
        match self {
            Self::Manifest => manifest::MAX_MANIFEST_BYTES,
            Self::Signature => manifest::MAX_SIGNATURE_BYTES,
            Self::Worker => manifest::MAX_FILE_BYTES,
        }
    }
    pub(crate) fn mode(self) -> u32 {
        if self == Self::Worker { 0o755 } else { 0o644 }
    }
}

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

    fn matches_embedded(
        &self,
        pack: &manifest::PackManifest,
        files: &[manifest::VerifiedCopyEntry; 3],
    ) -> bool {
        let entry = &self.entry;
        pack.pack_id == entry.pack_id
            && pack.pack_version == entry.pack_version
            && pack.pack_digest == entry.pack_digest
            && pack.security_epoch == entry.security_epoch
            && pack.runtime_abi_version == entry.runtime_abi_version
            && pack.backend == entry.backend
            && pack.provider == entry.provider
            && pack.target_os == entry.target_os
            && pack.target_arch == entry.target_arch
            && pack.worker_path == entry.worker_relative_path
            && files
                .iter()
                .try_fold(0u64, |sum, file| sum.checked_add(file.size_bytes))
                == Some(entry.installed_size_bytes)
    }
}

/// Copy-content authority only. No fabricated filesystem lease, root, temporary
/// candidate tree or executable authority. References remain immutable/alive
/// for the complete transaction; all three exact slot identities are retained.
pub(crate) struct EmbeddedSource<'bytes> {
    permit: MetalReleasePermit,
    bytes: [&'bytes [u8]; 3],
    entries: [manifest::VerifiedCopyEntry; 3],
    verifier: PackVerifier<'static>,
}

impl<'bytes> EmbeddedSource<'bytes> {
    fn admit(
        permit: MetalReleasePermit,
        bytes: [&'bytes [u8]; 3],
        verifier: PackVerifier<'static>,
    ) -> Result<Self> {
        // Closed array, before hashing any potentially large worker input.
        for slot in FileSlot::ALL {
            validate_embedded_length(slot, bytes[slot.index()].len() as u64)?;
        }
        let (pack, approval) = verifier.authenticate_metadata(bytes[0], bytes[1])?;
        if approval.is_some() || pack.payload.len() != 1 || pack.payload[0].path != WORKER {
            bail!("embedded Metal requires the exact same-build single-worker inventory");
        }
        let entries = FileSlot::ALL.map(|slot| manifest::VerifiedCopyEntry {
            path: slot.path().to_owned(),
            size_bytes: bytes[slot.index()].len() as u64,
            sha256: format!("{:x}", Sha256::digest(bytes[slot.index()])),
        });
        if pack.payload[0].size_bytes != entries[2].size_bytes
            || pack.payload[0].sha256 != entries[2].sha256
            || !permit.matches_embedded(&pack, &entries)
        {
            bail!("embedded Metal bytes differ from the signed inventory or release permit");
        }
        Ok(Self {
            permit,
            bytes,
            entries,
            verifier,
        })
    }

    fn reverify(&self) -> Result<()> {
        // Immutable Rust slices cannot be replaced through a runtime path. Still
        // rehash at transaction boundaries, also covering copied metadata bytes.
        for slot in FileSlot::ALL {
            let mut input = Cursor::new(self.bytes[slot.index()]);
            let entry = &self.entries[slot.index()];
            crate::installer_transaction::verify_bytes(
                &mut input,
                entry.size_bytes,
                &entry.sha256,
                slot.maximum(),
            )?;
        }
        Ok(())
    }
}

fn validate_embedded_length(slot: FileSlot, length: u64) -> Result<()> {
    if length == 0 || length > slot.maximum() {
        bail!("embedded Metal slot violates its byte limit");
    }
    Ok(())
}

/// Future compiled-entry caller supplies only its embedded slices. Release and
/// trust are compiled here, never supplied by a runtime argument/environment.
#[allow(
    dead_code,
    reason = "package-entry embedding/dispatch is a dependent source unit"
)]
pub(crate) fn admit_embedded(bytes: [&[u8]; 3]) -> Result<EmbeddedSource<'_>> {
    let permit = MetalReleasePermit::for_native_release(compiled_release()?)?;
    EmbeddedSource::admit(
        permit,
        bytes,
        PackVerifier::new(
            &manifest::ProductionTrustRoot,
            manifest::Compatibility {
                target_os: "macos",
                ..manifest::Compatibility::current(&[PackBackend::Metal])
            },
        ),
    )
}

/// A closed source union, not a caller-provided reader/trust-policy callback.
pub(crate) enum CopySource<'bytes> {
    Leased(Box<AdmittedSource<'static>>),
    #[allow(
        dead_code,
        reason = "constructed by private tests until package-entry dispatch is added"
    )]
    Embedded(Box<EmbeddedSource<'bytes>>),
}

pub(crate) enum CopyReader<'bytes> {
    File(std::fs::File),
    Embedded(Cursor<&'bytes [u8]>),
}
impl Read for CopyReader<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::File(file) => file.read(buffer),
            Self::Embedded(bytes) => bytes.read(buffer),
        }
    }
}

impl CopySource<'_> {
    pub(crate) fn destination_components(&self) -> [&str; 3] {
        match self {
            Self::Leased(source) => source.destination_components(),
            Self::Embedded(source) => {
                let entry = &source.permit.entry;
                [
                    entry.pack_id.as_str(),
                    entry.pack_version.as_str(),
                    &entry.pack_digest,
                ]
            }
        }
    }
    pub(crate) fn expected(&self, slot: FileSlot) -> Result<&manifest::VerifiedCopyEntry> {
        match self {
            Self::Leased(source) => source.expected(slot),
            Self::Embedded(source) => Ok(&source.entries[slot.index()]),
        }
    }
    pub(crate) fn open_for_copy(
        &self,
        slot: FileSlot,
    ) -> Result<(&manifest::VerifiedCopyEntry, CopyReader<'_>)> {
        match self {
            Self::Leased(source) => {
                let (entry, file) = source.open_for_copy(slot)?;
                Ok((entry, CopyReader::File(file)))
            }
            Self::Embedded(source) => Ok((
                &source.entries[slot.index()],
                CopyReader::Embedded(Cursor::new(source.bytes[slot.index()])),
            )),
        }
    }
    pub(crate) fn reverify(&self) -> Result<()> {
        match self {
            Self::Leased(source) => source.reverify(),
            Self::Embedded(source) => source.reverify(),
        }
    }
    pub(crate) fn verify_materialized(&self, pinned: PinnedPackRoot) -> Result<VerifiedPackLease> {
        match self {
            Self::Leased(source) => source.verify_materialized(pinned),
            Self::Embedded(source) => {
                source.reverify()?;
                let copy = source.verifier.verify_pinned(pinned)?;
                if !source.permit.matches(&copy) || copy.copy_entries() != source.entries {
                    bail!("materialized Metal copy differs from the exact embedded slots");
                }
                source.reverify()?;
                Ok(copy)
            }
        }
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
    pub(crate) fn destination_components(&self) -> [&str; 3] {
        let entry = &self.permit.entry;
        [
            entry.pack_id.as_str(),
            entry.pack_version.as_str(),
            &entry.pack_digest,
        ]
    }

    pub(crate) fn expected(&self, slot: FileSlot) -> Result<&manifest::VerifiedCopyEntry> {
        let entry = self
            .lease
            .copy_entries()
            .get(slot.index())
            .context("admitted file slot is missing")?;
        if entry.path != slot.path() || entry.size_bytes == 0 || entry.size_bytes > slot.maximum() {
            bail!("admitted file slot violates its closed path or byte limit");
        }
        Ok(entry)
    }

    /// Reopens beneath the pinned source. The returned handle is NOT immutable;
    /// the writer must hash its independently reopened copy against this entry.
    pub(crate) fn open_for_copy(
        &self,
        slot: FileSlot,
    ) -> Result<(&manifest::VerifiedCopyEntry, std::fs::File)> {
        self.lease.recheck()?;
        let entry = self.expected(slot)?;
        let file = self.lease.open_copy_file(entry)?;
        Ok((entry, file))
    }

    pub(crate) fn verify_materialized(&self, pinned: PinnedPackRoot) -> Result<VerifiedPackLease> {
        self.reverify()?;
        let copy = self.verifier.verify_pinned(pinned)?;
        if !self.permit.matches(&copy) || copy.copy_entries() != self.lease.copy_entries() {
            bail!("materialized copy differs from the exact admitted source inventory");
        }
        self.reverify()?;
        Ok(copy)
    }

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
pub(crate) mod tests;
