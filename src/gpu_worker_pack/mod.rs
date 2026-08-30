//! Verified GPU worker-pack infrastructure.
//!
//! Stage 4/6 discovers verified Windows and macOS packs and turns only challenge-bound
//! resolver/Hello results into explicit-GPU candidates. Production trust is
//! deliberately empty until a separate public-key review is complete, and
//! Auto remains default-denied to every GPU pack.

mod device_release_epoch;
pub(crate) mod health;
pub(crate) mod manifest;
pub(crate) mod store;

use std::fs::{File, OpenOptions};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;
#[cfg(any(
    all(windows, target_arch = "x86_64"),
    all(
        target_os = "macos",
        any(target_arch = "aarch64", target_arch = "x86_64")
    )
))]
use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Serialize};
#[cfg(any(
    all(windows, target_arch = "x86_64"),
    all(
        target_os = "macos",
        any(target_arch = "aarch64", target_arch = "x86_64")
    )
))]
use sha2::{Digest, Sha256};

const MAX_RELEASE_SECURITY_EPOCH: u64 = 9_007_199_254_740_991;

// Platform resolvers consume the bridge through this stable re-export.
#[cfg(unix)]
pub(crate) use launch_binding::UnixPackExecAuthority;
#[allow(unused_imports)]
pub(crate) use launch_binding::{ResolverHelloBindingBridge, VerifiedPackLaunchBinding};

mod launch_binding {
    #[cfg(unix)]
    use std::fs::File;
    use std::sync::Arc;

    use crate::backend_policy::{
        BackendKind, BackendPackIdentity, BackendTarget, DeviceClass, DeviceIdentity, GpuVendor,
        ProviderIdentity,
    };

    use super::manifest::{PackBackend, VerifiedPack, VerifiedPackLease};

    /// Stage 4 must implement this bridge on the concrete resolver/Hello path.
    /// Returning metadata is not sufficient by itself: the only constructor for
    /// the opaque binding compares every value with the reverified descriptor.
    pub(crate) trait ResolverHelloBindingBridge {
        fn resolver_verified_pack_lease(&self) -> Arc<VerifiedPackLease>;
        #[cfg(unix)]
        fn resolver_unix_launch_authority(&self) -> Option<Arc<UnixPackExecAuthority>>;
        fn hello_pack_id(&self) -> &str;
        fn hello_pack_version(&self) -> &str;
        fn hello_pack_digest(&self) -> &str;
        fn hello_security_epoch(&self) -> u64;
        fn hello_runtime_abi(&self) -> u16;
        fn hello_backend(&self) -> PackBackend;
        fn hello_provider(&self) -> &str;
        fn hello_stable_device_identity(&self) -> &str;
        fn hello_process_index(&self) -> Option<usize>;
        fn hello_display_name(&self) -> &str;
        fn hello_driver_version(&self) -> Option<&str>;
        fn hello_device_class(&self) -> DeviceClass;
        fn hello_vendor(&self) -> GpuVendor;
        fn hello_memory_total_bytes(&self) -> u64;
        fn hello_memory_available_bytes(&self) -> u64;
    }

    /// Stage 4's Unix resolver must produce both authorities from no-follow,
    /// descriptor-relative opens. A path plus a lease is deliberately not a
    /// substitute: the executable handle must reach `execveat`/`fexecve` and
    /// the dependency-root handle must remain live through Hello validation.
    #[cfg(unix)]
    #[derive(Debug)]
    pub(crate) struct UnixPackExecAuthority {
        verified_pack_lease: Arc<VerifiedPackLease>,
        executable_fd: File,
        dependency_root_fd: File,
    }

    #[cfg(unix)]
    impl UnixPackExecAuthority {
        pub(crate) fn from_verified_pack_lease(
            verified_pack_lease: Arc<VerifiedPackLease>,
        ) -> Result<Arc<Self>, super::manifest::PackVerificationError> {
            use std::io::{Seek, SeekFrom};
            use std::os::unix::fs::PermissionsExt;

            verified_pack_lease.recheck()?;
            let worker_path = &verified_pack_lease.verified_pack().worker_relative_path;
            let entry = verified_pack_lease
                .copy_entries()
                .iter()
                .find(|entry| entry.path == *worker_path)
                .ok_or(super::manifest::PackVerificationError::WorkerMissing)?;
            let mut executable_fd = verified_pack_lease.open_copy_file(entry)?;
            if executable_fd.metadata()?.permissions().mode() & 0o111 == 0 {
                return Err(super::manifest::PackVerificationError::WorkerNotExecutable);
            }
            let digest = super::manifest::hash_exact_length(
                &mut executable_fd,
                entry.size_bytes,
                &entry.path,
            )?;
            if digest != entry.sha256 {
                return Err(super::manifest::PackVerificationError::DigestMismatch);
            }
            executable_fd.seek(SeekFrom::Start(0))?;
            let dependency_root_fd = verified_pack_lease.open_dependency_root()?;
            verified_pack_lease.recheck()?;
            Ok(Arc::new(Self {
                verified_pack_lease,
                executable_fd,
                dependency_root_fd,
            }))
        }

        pub(crate) fn executable_fd(&self) -> &File {
            &self.executable_fd
        }

        pub(crate) fn dependency_root_fd(&self) -> &File {
            &self.dependency_root_fd
        }

        pub(crate) fn verified_pack_lease(&self) -> &Arc<VerifiedPackLease> {
            &self.verified_pack_lease
        }

        pub(crate) fn recheck(&self) -> Result<(), super::manifest::PackVerificationError> {
            use std::io::Seek;
            let lease = self.verified_pack_lease();
            lease.recheck()?;
            let worker_path = &lease.verified_pack().worker_relative_path;
            let entry = lease
                .copy_entries()
                .iter()
                .find(|entry| entry.path == *worker_path)
                .ok_or(super::manifest::PackVerificationError::WorkerMissing)?;
            let mut executable = self.executable_fd.try_clone()?;
            executable.rewind()?;
            let digest =
                super::manifest::hash_exact_length(&mut executable, entry.size_bytes, &entry.path)?;
            if digest != entry.sha256 {
                return Err(super::manifest::PackVerificationError::DigestMismatch);
            }
            lease.recheck()
        }

        #[cfg(test)]
        pub(crate) fn fixture(
            verified_pack_lease: Arc<VerifiedPackLease>,
            executable_fd: File,
            dependency_root_fd: File,
        ) -> Self {
            Self {
                verified_pack_lease,
                executable_fd,
                dependency_root_fd,
            }
        }
    }

    #[cfg(all(test, target_os = "linux", target_arch = "x86_64", target_env = "gnu"))]
    impl crate::linux_worker_launch::LinuxExecAuthority for UnixPackExecAuthority {
        fn executable_fd(&self) -> std::os::fd::RawFd {
            use std::os::fd::AsRawFd;
            self.executable_fd().as_raw_fd()
        }

        fn dependency_root_fd(&self) -> std::os::fd::RawFd {
            use std::os::fd::AsRawFd;
            self.dependency_root_fd().as_raw_fd()
        }

        fn recheck(&self) -> std::io::Result<()> {
            self.recheck().map_err(std::io::Error::other)
        }
    }

    /// Opaque proof that a concrete resolver result and worker Hello agreed on
    /// the exact verified pack and stable device. Its fields are private to this
    /// child module, so production discovery cannot fabricate one from a raw
    /// `VerifiedPack`.
    #[derive(Clone, Debug)]
    pub(crate) struct VerifiedPackLaunchBinding {
        verified_pack_lease: Arc<VerifiedPackLease>,
        stable_device_identity: String,
        process_index: usize,
        display_name: String,
        driver_version: Option<String>,
        device_class: DeviceClass,
        vendor: GpuVendor,
        memory_total_bytes: u64,
        memory_available_bytes: u64,
        #[cfg(unix)]
        unix_exec_authority: Arc<UnixPackExecAuthority>,
    }

    impl VerifiedPackLaunchBinding {
        pub(crate) fn try_from_resolver_hello_bridge(
            bridge: &impl ResolverHelloBindingBridge,
        ) -> Option<Self> {
            let lease = bridge.resolver_verified_pack_lease();
            #[cfg(unix)]
            let unix_exec_authority = bridge.resolver_unix_launch_authority()?;
            #[cfg(unix)]
            let unix_authority_matches_lease =
                Arc::ptr_eq(&lease, unix_exec_authority.verified_pack_lease());
            let pack = lease.verified_pack();
            let stable_device_identity = bridge.hello_stable_device_identity();
            let display_name = bridge.hello_display_name().trim();
            let driver_version = bridge.hello_driver_version();
            let stable_device_is_canonical = !stable_device_identity.is_empty()
                && stable_device_identity.len() <= 256
                && stable_device_identity
                    .bytes()
                    .all(|byte| (0x20..=0x7e).contains(&byte))
                && stable_device_identity == stable_device_identity.to_ascii_lowercase();
            let display_name_is_bounded = !display_name.is_empty()
                && display_name.len() <= 256
                && display_name
                    .bytes()
                    .all(|byte| (0x20..=0x7e).contains(&byte));
            let driver_is_bounded = driver_version.is_some_and(|value| {
                !value.is_empty()
                    && value.len() <= 128
                    && value.bytes().all(|byte| (0x20..=0x7e).contains(&byte))
            });
            let device_class = bridge.hello_device_class();
            let process_index = bridge.hello_process_index();
            let backend_matches_class = matches!(
                device_class,
                DeviceClass::DiscreteGpu
                    | DeviceClass::IntegratedGpu
                    | DeviceClass::UnifiedGpu
                    | DeviceClass::Unknown
            );
            (stable_device_is_canonical
                && display_name_is_bounded
                && driver_is_bounded
                && backend_matches_class
                && process_index.is_some()
                && bridge.hello_memory_available_bytes() <= bridge.hello_memory_total_bytes()
                && {
                    #[cfg(unix)]
                    {
                        unix_authority_matches_lease
                    }
                    #[cfg(not(unix))]
                    {
                        true
                    }
                }
                && bridge.hello_pack_id() == pack.pack_id.as_str()
                && bridge.hello_pack_version() == pack.pack_version.as_str()
                && bridge.hello_pack_digest() == pack.pack_digest
                && bridge.hello_security_epoch() == pack.security_epoch
                && bridge.hello_runtime_abi() == pack.runtime_abi_version
                && bridge.hello_backend() == pack.backend
                && bridge.hello_provider() == pack.provider)
                .then(|| Self {
                    verified_pack_lease: lease,
                    stable_device_identity: stable_device_identity.to_owned(),
                    process_index: process_index.expect("checked above"),
                    display_name: display_name.to_owned(),
                    driver_version: driver_version.map(str::to_owned),
                    device_class,
                    vendor: bridge.hello_vendor(),
                    memory_total_bytes: bridge.hello_memory_total_bytes(),
                    memory_available_bytes: bridge.hello_memory_available_bytes(),
                    #[cfg(unix)]
                    unix_exec_authority,
                })
        }

        pub(crate) fn verified_pack(&self) -> &VerifiedPack {
            self.verified_pack_lease.verified_pack()
        }

        pub(crate) fn verified_pack_lease(&self) -> &VerifiedPackLease {
            &self.verified_pack_lease
        }

        pub(crate) fn verified_pack_lease_arc(&self) -> Arc<VerifiedPackLease> {
            Arc::clone(&self.verified_pack_lease)
        }

        pub(crate) fn stable_device_identity(&self) -> &str {
            &self.stable_device_identity
        }

        pub(crate) fn backend_target(&self) -> BackendTarget {
            let pack = self.verified_pack();
            BackendTarget {
                backend: match pack.backend {
                    PackBackend::Cuda => BackendKind::Cuda,
                    PackBackend::Vulkan => BackendKind::Vulkan,
                    PackBackend::Metal => BackendKind::Metal,
                },
                provider_id: ProviderIdentity::new(pack.provider.clone()),
                driver_version: self.driver_version.clone(),
                device_id: DeviceIdentity::new(self.stable_device_identity.clone()),
                display_name: self.display_name.clone(),
                vendor: self.vendor,
                device_class: self.device_class,
                memory_total_bytes: self.memory_total_bytes,
                memory_available_bytes: self.memory_available_bytes,
                pack: Some(BackendPackIdentity {
                    pack_id: pack.pack_id.as_str().to_owned(),
                    pack_version: pack.pack_version.as_str().to_owned(),
                    pack_digest: pack.pack_digest.clone(),
                    security_epoch: pack.security_epoch,
                    runtime_abi: pack.runtime_abi_version,
                }),
                process_index: Some(self.process_index),
            }
        }

        #[cfg(unix)]
        pub(crate) fn unix_exec_authority(&self) -> &UnixPackExecAuthority {
            &self.unix_exec_authority
        }

        #[cfg(unix)]
        pub(crate) fn unix_exec_authority_arc(&self) -> Arc<UnixPackExecAuthority> {
            Arc::clone(&self.unix_exec_authority)
        }
    }
}

/// Production discovery can hold only opaque resolver/Hello bindings, never a
/// raw verified descriptor.
pub(crate) struct ProductionPackRegistry {
    bindings: Vec<VerifiedPackLaunchBinding>,
    diagnostics: Vec<PackDiscoveryDiagnostic>,
}

impl ProductionPackRegistry {
    pub(crate) fn empty() -> Self {
        Self {
            bindings: Vec::new(),
            diagnostics: Vec::new(),
        }
    }

    #[allow(dead_code)]
    pub(crate) fn from_launch_bindings(bindings: Vec<VerifiedPackLaunchBinding>) -> Self {
        Self {
            bindings,
            diagnostics: Vec::new(),
        }
    }

    pub(crate) fn with_diagnostics(mut self, diagnostics: Vec<PackDiscoveryDiagnostic>) -> Self {
        self.diagnostics = diagnostics;
        self
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.bindings.is_empty()
    }

    pub(crate) fn into_bindings(self) -> Vec<VerifiedPackLaunchBinding> {
        self.bindings
    }

    pub(crate) fn into_parts(
        self,
    ) -> (Vec<VerifiedPackLaunchBinding>, Vec<PackDiscoveryDiagnostic>) {
        (self.bindings, self.diagnostics)
    }
}

/// This legacy empty constructor remains a fail-closed compatibility seam.
/// Stage 4 discovery constructs a registry only from opaque launch bindings.
pub(crate) fn production_registry() -> ProductionPackRegistry {
    ProductionPackRegistry::empty()
}

const PACK_CATALOG_NAME: &str = "worker-pack-catalog.json";
const MAX_PACK_CATALOG_BYTES: u64 = 512 * 1024;
const MAX_PRODUCTION_PACKS: usize = 8;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PackDiscoveryIssue {
    UnsupportedPlatform,
    CatalogUnavailable,
    CatalogRejected,
    EntryIncompatible,
    PackRootRejected,
    SignatureOrInventoryRejected,
    CatalogInventoryMismatch,
    SecurityEpochStateRejected,
    DeviceRollbackAuthorityRejected,
    ReleaseAuthorityRejected,
    NotAutoQualified,
    ProviderProbeRejected,
    DriverVersionUnavailable,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PackDiscoveryDiagnostic {
    pub(crate) issue: PackDiscoveryIssue,
    pub(crate) pack_id: Option<String>,
    pub(crate) backend: Option<manifest::PackBackend>,
}

impl PackDiscoveryDiagnostic {
    pub(crate) fn catalog(issue: PackDiscoveryIssue) -> Self {
        Self {
            issue,
            pack_id: None,
            backend: None,
        }
    }

    pub(crate) fn pack(
        issue: PackDiscoveryIssue,
        pack_id: &manifest::StoreComponent,
        backend: manifest::PackBackend,
    ) -> Self {
        Self {
            issue,
            pack_id: Some(pack_id.as_str().to_owned()),
            backend: Some(backend),
        }
    }

    pub(crate) fn safe_summary(&self) -> String {
        let subject = match (&self.pack_id, self.backend) {
            (Some(pack_id), Some(backend)) => format!("{backend:?} pack {pack_id}"),
            (Some(pack_id), None) => format!("GPU pack {pack_id}"),
            _ => "GPU worker packs".to_owned(),
        };
        match self.issue {
            PackDiscoveryIssue::UnsupportedPlatform => {
                format!("{subject} are unsupported on this platform")
            }
            PackDiscoveryIssue::CatalogUnavailable => {
                format!("{subject} were not found in this installation")
            }
            PackDiscoveryIssue::CatalogRejected => {
                format!("{subject} catalog was rejected")
            }
            PackDiscoveryIssue::EntryIncompatible => {
                format!("{subject} is incompatible with this application build")
            }
            PackDiscoveryIssue::PackRootRejected => {
                format!("{subject} immutable root was rejected")
            }
            PackDiscoveryIssue::SignatureOrInventoryRejected => {
                format!("{subject} signature or installed inventory was rejected")
            }
            PackDiscoveryIssue::CatalogInventoryMismatch => {
                format!("{subject} does not match its catalog entry")
            }
            PackDiscoveryIssue::SecurityEpochStateRejected => {
                format!("{subject} security epoch state was rejected")
            }
            PackDiscoveryIssue::DeviceRollbackAuthorityRejected => {
                format!("{subject} device rollback authority was rejected")
            }
            PackDiscoveryIssue::ReleaseAuthorityRejected => {
                format!("{subject} does not match the signed release authority")
            }
            PackDiscoveryIssue::NotAutoQualified => {
                format!("{subject} is verified but not qualified for Auto")
            }
            PackDiscoveryIssue::ProviderProbeRejected => {
                format!("{subject} provider probe was rejected")
            }
            PackDiscoveryIssue::DriverVersionUnavailable => {
                format!("{subject} driver version is unavailable from the pinned provider API")
            }
        }
    }
}

#[derive(Clone)]
pub(crate) struct PackLeaseDiscovery {
    pub(crate) leases: Vec<Arc<manifest::VerifiedPackLease>>,
    pub(crate) diagnostics: Vec<PackDiscoveryDiagnostic>,
    pub(crate) catalog_generation: Option<String>,
}

impl PackLeaseDiscovery {
    pub(crate) fn diagnostic_summary(&self) -> String {
        diagnostic_summary(&self.diagnostics)
    }
}

pub(crate) fn diagnostic_summary(diagnostics: &[PackDiscoveryDiagnostic]) -> String {
    diagnostics
        .iter()
        .take(MAX_PRODUCTION_PACKS * 2 + 1)
        .map(PackDiscoveryDiagnostic::safe_summary)
        .collect::<Vec<_>>()
        .join("; ")
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PackCatalog {
    schema_version: u16,
    packs: Vec<PackCatalogEntry>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PackCatalogEntry {
    pack_id: manifest::StoreComponent,
    pack_version: manifest::StoreComponent,
    pack_digest: String,
    security_epoch: u64,
    runtime_abi_version: u16,
    backend: manifest::PackBackend,
    provider: String,
    target_os: String,
    target_arch: String,
    worker_relative_path: String,
    root: String,
    installed_size_bytes: u64,
    compressed_size_bytes: u64,
    files: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PackReleaseAuthority {
    schema_version: u16,
    catalog_sha256: String,
    release_security_epoch: u64,
    keychain_access_group: String,
    entries: Vec<PackReleaseAuthorityEntry>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ValidatedReleaseAuthority {
    release_security_epoch: u64,
    keychain_access_group: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PackReleaseAuthorityEntry {
    app_version: String,
    build_revision: String,
    app_protocol_version: u16,
    pack_id: manifest::StoreComponent,
    pack_version: manifest::StoreComponent,
    pack_digest: String,
    security_epoch: u64,
    runtime_abi_version: u16,
    backend: manifest::PackBackend,
    provider: String,
    target_os: String,
    target_arch: String,
    worker_relative_path: String,
    root: String,
    installed_size_bytes: u64,
    compressed_size_bytes: u64,
    files: Vec<String>,
}

impl PackReleaseAuthorityEntry {
    fn matches_catalog_entry(&self, entry: &PackCatalogEntry) -> bool {
        self.app_version == env!("CARGO_PKG_VERSION")
            && self.build_revision == env!("SCRIBE_BUILD_REVISION")
            && self.app_protocol_version == manifest::APP_PROTOCOL_VERSION
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

const EMBEDDED_PACK_RELEASE_AUTHORITY: &[u8] = include_bytes!(concat!(
    env!("OUT_DIR"),
    "/scribe_gpu_pack_release_authority.json"
));

fn validated_release_authority(
    catalog_bytes: &[u8],
    authority_bytes: &[u8],
) -> Option<ValidatedReleaseAuthority> {
    let authority = validated_release_authority_document(authority_bytes)?;
    if !manifest::is_canonical_sha256(&authority.catalog_sha256)
        || format!("{:x}", Sha256::digest(catalog_bytes)) != authority.catalog_sha256
    {
        return None;
    }
    let Ok(catalog) = serde_json::from_slice::<PackCatalog>(catalog_bytes) else {
        return None;
    };
    if catalog.schema_version != 1
        || catalog.packs.len() != authority.entries.len()
        || !catalog
            .packs
            .iter()
            .zip(&authority.entries)
            .all(|(catalog, authority)| authority.matches_catalog_entry(catalog))
    {
        return None;
    }
    Some(ValidatedReleaseAuthority {
        release_security_epoch: authority.release_security_epoch,
        keychain_access_group: authority.keychain_access_group,
    })
}

fn validated_release_authority_document(authority_bytes: &[u8]) -> Option<PackReleaseAuthority> {
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

fn catalog_matches_release_authority(catalog_bytes: &[u8], authority_bytes: &[u8]) -> bool {
    validated_release_authority(catalog_bytes, authority_bytes).is_some()
}

fn canonical_keychain_access_group(group: &str) -> bool {
    let Some((team_id, suffix)) = group.split_at_checked(10) else {
        return false;
    };
    suffix == ".com.scribe.local-transcriber"
        && team_id
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
}

fn release_epoch_identity_matches(
    authority: &ValidatedReleaseAuthority,
    target_security_epoch: u64,
    compiled_group: &str,
    reviewed_group: &str,
) -> bool {
    authority.release_security_epoch > 0
        && authority.release_security_epoch <= MAX_RELEASE_SECURITY_EPOCH
        && target_security_epoch == authority.release_security_epoch
        && compiled_group == authority.keychain_access_group
        && reviewed_group == authority.keychain_access_group
        && canonical_keychain_access_group(compiled_group)
}

/// Rechecks the non-resettable macOS release floor at the final request
/// activation boundary. A higher epoch observed after this succeeds belongs to
/// a later request; an already active transcription is never migrated.
#[cfg(all(target_os = "macos", not(test)))]
pub(crate) fn revalidate_production_device_epoch(
    target_security_epoch: Option<u64>,
) -> Result<(), ()> {
    let target_security_epoch = target_security_epoch.ok_or(())?;
    let authority = validated_release_authority_document(EMBEDDED_PACK_RELEASE_AUTHORITY)
        .map(|authority| ValidatedReleaseAuthority {
            release_security_epoch: authority.release_security_epoch,
            keychain_access_group: authority.keychain_access_group,
        })
        .ok_or(())?;
    let compiled_group =
        option_env!("SCRIBE_MACOS_GPU_ROLLBACK_KEYCHAIN_ACCESS_GROUP").unwrap_or("");
    let reviewed_group =
        option_env!("SCRIBE_REVIEWED_MACOS_GPU_ROLLBACK_KEYCHAIN_ACCESS_GROUP").unwrap_or("");
    if !release_epoch_identity_matches(
        &authority,
        target_security_epoch,
        compiled_group,
        reviewed_group,
    ) {
        return Err(());
    }
    device_release_epoch::admit(authority.release_security_epoch).map_err(|_| ())
}

#[cfg(all(not(target_os = "macos"), not(test)))]
pub(crate) fn revalidate_production_device_epoch(
    _target_security_epoch: Option<u64>,
) -> Result<(), ()> {
    Ok(())
}

#[cfg(test)]
thread_local! {
    static TEST_DEVICE_EPOCH_REJECTED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
pub(crate) fn revalidate_production_device_epoch(
    _target_security_epoch: Option<u64>,
) -> Result<(), ()> {
    TEST_DEVICE_EPOCH_REJECTED.with(|rejected| (!rejected.get()).then_some(()).ok_or(()))
}

#[cfg(test)]
pub(crate) fn set_test_device_epoch_rejected(rejected: bool) {
    TEST_DEVICE_EPOCH_REJECTED.with(|value| value.set(rejected));
}

/// Verifies the bounded installed catalog and returns retained pack leases plus
/// categorical, path-free diagnostics for every skipped entry.
/// It never loads a provider or creates launch authority. A malformed catalog,
/// absent production trust key, or incompatible pack projects to no GPU route.
#[cfg(any(
    all(windows, target_arch = "x86_64"),
    all(
        target_os = "macos",
        any(target_arch = "aarch64", target_arch = "x86_64")
    )
))]
pub(crate) fn discover_production_pack_leases() -> PackLeaseDiscovery {
    discover_pack_leases_from_current_install()
}

#[cfg(not(any(
    all(windows, target_arch = "x86_64"),
    all(
        target_os = "macos",
        any(target_arch = "aarch64", target_arch = "x86_64")
    )
)))]
pub(crate) fn discover_production_pack_leases() -> PackLeaseDiscovery {
    PackLeaseDiscovery {
        leases: Vec::new(),
        diagnostics: vec![PackDiscoveryDiagnostic::catalog(
            PackDiscoveryIssue::UnsupportedPlatform,
        )],
        catalog_generation: None,
    }
}

#[cfg(any(
    all(windows, target_arch = "x86_64"),
    all(
        target_os = "macos",
        any(target_arch = "aarch64", target_arch = "x86_64")
    )
))]
fn discover_pack_leases_from_current_install() -> PackLeaseDiscovery {
    let Ok(executable) = std::env::current_exe() else {
        return PackLeaseDiscovery {
            leases: Vec::new(),
            diagnostics: vec![PackDiscoveryDiagnostic::catalog(
                PackDiscoveryIssue::CatalogUnavailable,
            )],
            catalog_generation: None,
        };
    };
    let Some(install_root) = production_resource_root_from_executable(&executable) else {
        return PackLeaseDiscovery {
            leases: Vec::new(),
            diagnostics: vec![PackDiscoveryDiagnostic::catalog(
                PackDiscoveryIssue::CatalogUnavailable,
            )],
            catalog_generation: None,
        };
    };
    discover_pack_leases_from_install_root(&install_root)
}

fn production_resource_root_from_executable(executable: &Path) -> Option<PathBuf> {
    #[cfg(windows)]
    {
        executable.parent().map(Path::to_path_buf)
    }
    #[cfg(target_os = "macos")]
    {
        macos_resource_root_from_executable(executable)
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let _ = executable;
        None
    }
}

fn macos_resource_root_from_executable(executable: &Path) -> Option<PathBuf> {
    let macos = executable.parent()?;
    let contents = macos.parent()?;
    (macos.file_name()? == "MacOS" && contents.file_name()? == "Contents")
        .then(|| contents.join("Resources"))
}

#[cfg(any(
    all(windows, target_arch = "x86_64"),
    all(
        target_os = "macos",
        any(target_arch = "aarch64", target_arch = "x86_64")
    )
))]
fn discover_pack_leases_from_install_root(install_root: &Path) -> PackLeaseDiscovery {
    let catalog_path = install_root.join(PACK_CATALOG_NAME);
    let catalog = match read_bounded_catalog(&catalog_path, install_root) {
        Ok(catalog) => catalog,
        Err(CatalogReadFailure::Unavailable) => {
            return PackLeaseDiscovery {
                leases: Vec::new(),
                diagnostics: vec![PackDiscoveryDiagnostic::catalog(
                    PackDiscoveryIssue::CatalogUnavailable,
                )],
                catalog_generation: None,
            };
        }
        Err(CatalogReadFailure::Rejected) => {
            return PackLeaseDiscovery {
                leases: Vec::new(),
                diagnostics: vec![PackDiscoveryDiagnostic::catalog(
                    PackDiscoveryIssue::CatalogRejected,
                )],
                catalog_generation: None,
            };
        }
    };
    #[cfg(target_os = "macos")]
    let release_authority =
        match validated_release_authority(&catalog.bytes, EMBEDDED_PACK_RELEASE_AUTHORITY) {
            Some(authority) => authority,
            None => {
                return PackLeaseDiscovery {
                    leases: Vec::new(),
                    diagnostics: vec![PackDiscoveryDiagnostic::catalog(
                        PackDiscoveryIssue::ReleaseAuthorityRejected,
                    )],
                    catalog_generation: Some(catalog.fingerprint.generation_id()),
                };
            }
        };
    let cache =
        PRODUCTION_DISCOVERY_CACHE.get_or_init(|| Mutex::new(CatalogDiscoveryCache::default()));
    if let Ok(cache) = cache.lock()
        && let Some(discovery) = cache.lookup(&catalog.fingerprint)
    {
        #[cfg(target_os = "macos")]
        return enforce_production_discovery_epochs(discovery, &release_authority);
        #[cfg(windows)]
        return enforce_production_discovery_epochs(discovery);
    }
    let fingerprint = catalog.fingerprint;
    let generation = fingerprint.generation_id();
    let discovery = verify_catalog_entries(install_root, &catalog.bytes, generation);
    if let Ok(mut cache) = cache.lock() {
        cache.replace(fingerprint, discovery.clone());
    }
    #[cfg(target_os = "macos")]
    return enforce_production_discovery_epochs(discovery, &release_authority);
    #[cfg(windows)]
    enforce_production_discovery_epochs(discovery)
}

#[cfg(any(
    all(windows, target_arch = "x86_64"),
    all(
        target_os = "macos",
        any(target_arch = "aarch64", target_arch = "x86_64")
    )
))]
#[cfg(windows)]
fn enforce_production_discovery_epochs(discovery: PackLeaseDiscovery) -> PackLeaseDiscovery {
    let Ok(directories) = crate::config::project_dirs() else {
        return reject_discovery_epoch_state(discovery);
    };
    enforce_discovery_epochs_at(
        discovery,
        directories
            .data_local_dir()
            .join("gpu-worker-pack-discovery-state"),
    )
}

#[cfg(target_os = "macos")]
fn enforce_production_discovery_epochs(
    discovery: PackLeaseDiscovery,
    authority: &ValidatedReleaseAuthority,
) -> PackLeaseDiscovery {
    let compiled_group =
        option_env!("SCRIBE_MACOS_GPU_ROLLBACK_KEYCHAIN_ACCESS_GROUP").unwrap_or("");
    let reviewed_group =
        option_env!("SCRIBE_REVIEWED_MACOS_GPU_ROLLBACK_KEYCHAIN_ACCESS_GROUP").unwrap_or("");
    if authority.release_security_epoch == 0 {
        return discovery;
    }
    if !release_epoch_identity_matches(
        authority,
        authority.release_security_epoch,
        compiled_group,
        reviewed_group,
    ) || device_release_epoch::admit(authority.release_security_epoch).is_err()
    {
        return reject_device_rollback_authority(discovery);
    }

    if let Ok(directories) = crate::config::project_dirs() {
        let _ = admit_local_discovery_ledger(
            &discovery,
            directories
                .data_local_dir()
                .join("gpu-worker-pack-discovery-state"),
        );
    }
    discovery
}

fn enforce_discovery_epochs_at(
    discovery: PackLeaseDiscovery,
    state_root: PathBuf,
) -> PackLeaseDiscovery {
    if discovery.leases.is_empty() {
        return discovery;
    }
    let ledger = store::DiscoveryEpochLedger::new(state_root);
    let packs = discovery
        .leases
        .iter()
        .map(|lease| lease.verified_pack())
        .collect::<Vec<_>>();
    if ledger.admit(&packs).is_ok() {
        return discovery;
    }
    reject_discovery_epoch_state(discovery)
}

fn admit_local_discovery_ledger(
    discovery: &PackLeaseDiscovery,
    state_root: PathBuf,
) -> Result<(), store::PackStoreError> {
    if discovery.leases.is_empty() {
        return Ok(());
    }
    let ledger = store::DiscoveryEpochLedger::new(state_root);
    let packs = discovery
        .leases
        .iter()
        .map(|lease| lease.verified_pack())
        .collect::<Vec<_>>();
    ledger.admit(&packs)
}

fn reject_discovery_epoch_state(mut discovery: PackLeaseDiscovery) -> PackLeaseDiscovery {
    discovery
        .diagnostics
        .extend(discovery.leases.iter().map(|lease| {
            let pack = lease.verified_pack();
            PackDiscoveryDiagnostic::pack(
                PackDiscoveryIssue::SecurityEpochStateRejected,
                &pack.pack_id,
                pack.backend,
            )
        }));
    discovery.leases.clear();
    discovery
}

fn reject_device_rollback_authority(mut discovery: PackLeaseDiscovery) -> PackLeaseDiscovery {
    if discovery.leases.is_empty() {
        discovery.diagnostics.push(PackDiscoveryDiagnostic::catalog(
            PackDiscoveryIssue::DeviceRollbackAuthorityRejected,
        ));
    } else {
        discovery
            .diagnostics
            .extend(discovery.leases.iter().map(|lease| {
                let pack = lease.verified_pack();
                PackDiscoveryDiagnostic::pack(
                    PackDiscoveryIssue::DeviceRollbackAuthorityRejected,
                    &pack.pack_id,
                    pack.backend,
                )
            }));
    }
    discovery.leases.clear();
    discovery
}

#[cfg(test)]
fn enforce_device_epoch_with_store(
    discovery: PackLeaseDiscovery,
    authority: &ValidatedReleaseAuthority,
    compiled_group: &str,
    reviewed_group: &str,
    store: &mut impl device_release_epoch::MarkerStore,
    local_ledger: impl FnOnce(&PackLeaseDiscovery) -> Result<(), ()>,
) -> PackLeaseDiscovery {
    if authority.release_security_epoch == 0 {
        return discovery;
    }
    if !release_epoch_identity_matches(
        authority,
        authority.release_security_epoch,
        compiled_group,
        reviewed_group,
    ) || device_release_epoch::admit_with_store(authority.release_security_epoch, store).is_err()
    {
        return reject_device_rollback_authority(discovery);
    }
    let _ = local_ledger(&discovery);
    discovery
}

#[cfg(any(
    all(windows, target_arch = "x86_64"),
    all(
        target_os = "macos",
        any(target_arch = "aarch64", target_arch = "x86_64")
    )
))]
fn verify_catalog_entries(
    install_root: &Path,
    bytes: &[u8],
    catalog_generation: String,
) -> PackLeaseDiscovery {
    #[cfg(windows)]
    let allowed = &[manifest::PackBackend::Cuda, manifest::PackBackend::Vulkan][..];
    #[cfg(target_os = "macos")]
    let allowed = &[manifest::PackBackend::Metal][..];
    let verifier = manifest::PackVerifier::new(
        &manifest::ProductionTrustRoot,
        manifest::Compatibility::current(allowed),
    );
    verify_catalog_entries_with_verifier(install_root, bytes, catalog_generation, &verifier)
}

#[cfg(any(
    all(windows, target_arch = "x86_64"),
    all(
        target_os = "macos",
        any(target_arch = "aarch64", target_arch = "x86_64")
    )
))]
fn verify_catalog_entries_with_verifier(
    install_root: &Path,
    bytes: &[u8],
    catalog_generation: String,
    verifier: &manifest::PackVerifier<'_>,
) -> PackLeaseDiscovery {
    let Ok(catalog) = serde_json::from_slice::<PackCatalog>(bytes) else {
        return PackLeaseDiscovery {
            leases: Vec::new(),
            diagnostics: vec![PackDiscoveryDiagnostic::catalog(
                PackDiscoveryIssue::CatalogRejected,
            )],
            catalog_generation: Some(catalog_generation),
        };
    };
    if catalog.schema_version != 1 || catalog.packs.len() > MAX_PRODUCTION_PACKS {
        return PackLeaseDiscovery {
            leases: Vec::new(),
            diagnostics: vec![PackDiscoveryDiagnostic::catalog(
                PackDiscoveryIssue::CatalogRejected,
            )],
            catalog_generation: Some(catalog_generation),
        };
    }
    let packs_root = install_root.join("workers").join("packs");
    let mut leases = Vec::new();
    let mut diagnostics = Vec::new();
    for entry in catalog.packs {
        if entry.target_os != std::env::consts::OS
            || entry.target_arch != std::env::consts::ARCH
            || entry.runtime_abi_version != manifest::RUNTIME_ABI_VERSION
            || entry.installed_size_bytes == 0
            || entry.compressed_size_bytes == 0
            || entry.files.len() < 3
            || entry.files.len() > manifest::MAX_FILES + 2
        {
            diagnostics.push(PackDiscoveryDiagnostic::pack(
                PackDiscoveryIssue::EntryIncompatible,
                &entry.pack_id,
                entry.backend,
            ));
            continue;
        }
        let relative_root = format!(
            "workers/packs/{}/{}/{}",
            entry.pack_id.as_str(),
            entry.pack_version.as_str(),
            entry.pack_digest
        );
        if entry.root != relative_root {
            diagnostics.push(PackDiscoveryDiagnostic::pack(
                PackDiscoveryIssue::PackRootRejected,
                &entry.pack_id,
                entry.backend,
            ));
            continue;
        }
        let Ok(pinned) = manifest::PinnedPackRoot::open(
            &packs_root,
            [&entry.pack_id, &entry.pack_version],
            &entry.pack_digest,
        ) else {
            diagnostics.push(PackDiscoveryDiagnostic::pack(
                PackDiscoveryIssue::PackRootRejected,
                &entry.pack_id,
                entry.backend,
            ));
            continue;
        };
        let Ok(lease) = verifier.verify_pinned(pinned) else {
            diagnostics.push(PackDiscoveryDiagnostic::pack(
                PackDiscoveryIssue::SignatureOrInventoryRejected,
                &entry.pack_id,
                entry.backend,
            ));
            continue;
        };
        #[cfg(target_os = "macos")]
        if entry.backend == manifest::PackBackend::Metal
            && !single_executable_signed_payload(&lease)
        {
            diagnostics.push(PackDiscoveryDiagnostic::pack(
                PackDiscoveryIssue::SignatureOrInventoryRejected,
                &entry.pack_id,
                entry.backend,
            ));
            continue;
        }
        let observed = lease.verified_pack();
        let mut expected_files = lease
            .copy_entries()
            .iter()
            .map(|file| format!("{relative_root}/{}", file.path))
            .collect::<Vec<_>>();
        expected_files.sort();
        let catalog_files_are_canonical = entry
            .files
            .windows(2)
            .all(|pair| pair[0].as_str() < pair[1].as_str());
        if observed.pack_id != entry.pack_id
            || observed.pack_version != entry.pack_version
            || observed.pack_digest != entry.pack_digest
            || observed.security_epoch != entry.security_epoch
            || observed.runtime_abi_version != entry.runtime_abi_version
            || observed.backend != entry.backend
            || observed.provider != entry.provider
            || observed.target_os != entry.target_os
            || observed.target_arch != entry.target_arch
            || observed.worker_relative_path != entry.worker_relative_path
            || !catalog_files_are_canonical
            || entry.files != expected_files
        {
            diagnostics.push(PackDiscoveryDiagnostic::pack(
                PackDiscoveryIssue::CatalogInventoryMismatch,
                &entry.pack_id,
                entry.backend,
            ));
            continue;
        }
        leases.push(Arc::new(lease));
    }
    PackLeaseDiscovery {
        leases,
        diagnostics,
        catalog_generation: Some(catalog_generation),
    }
}

/// Stage 6 macOS packs deliberately carry no auxiliary payload. Retained
/// executable authority is sufficient for the one signed worker, while
/// descriptor-bound dependency loading remains a separately deferred design.
fn single_executable_signed_payload(lease: &manifest::VerifiedPackLease) -> bool {
    let worker = lease.verified_pack().worker_relative_path.as_str();
    let mut payload = lease.copy_entries().iter().filter(|entry| {
        entry.path != manifest::MANIFEST_NAME && entry.path != manifest::SIGNATURE_NAME
    });
    payload.next().is_some_and(|entry| entry.path == worker) && payload.next().is_none()
}

#[cfg(any(
    all(windows, target_arch = "x86_64"),
    all(
        target_os = "macos",
        any(target_arch = "aarch64", target_arch = "x86_64")
    )
))]
#[derive(Clone, Debug, Eq, PartialEq)]
struct CatalogFingerprint {
    install_root: PathBuf,
    #[cfg(windows)]
    volume_serial_number: u32,
    #[cfg(windows)]
    file_index: u64,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
    content_sha256: [u8; 32],
}

#[cfg(any(
    all(windows, target_arch = "x86_64"),
    all(
        target_os = "macos",
        any(target_arch = "aarch64", target_arch = "x86_64")
    )
))]
impl CatalogFingerprint {
    fn generation_id(&self) -> String {
        #[cfg(windows)]
        {
            format!(
                "{:08x}{:016x}{}",
                self.volume_serial_number,
                self.file_index,
                self.content_sha256
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect::<String>()
            )
        }
        #[cfg(unix)]
        {
            format!(
                "{:016x}{:016x}{}",
                self.device,
                self.inode,
                self.content_sha256
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect::<String>()
            )
        }
    }
}

#[cfg(any(
    all(windows, target_arch = "x86_64"),
    all(
        target_os = "macos",
        any(target_arch = "aarch64", target_arch = "x86_64")
    )
))]
struct CatalogSnapshot {
    bytes: Vec<u8>,
    fingerprint: CatalogFingerprint,
}

#[cfg(any(
    all(windows, target_arch = "x86_64"),
    all(
        target_os = "macos",
        any(target_arch = "aarch64", target_arch = "x86_64")
    )
))]
#[derive(Default)]
struct CatalogDiscoveryCache {
    entry: Option<(CatalogFingerprint, PackLeaseDiscovery)>,
}

#[cfg(any(
    all(windows, target_arch = "x86_64"),
    all(
        target_os = "macos",
        any(target_arch = "aarch64", target_arch = "x86_64")
    )
))]
impl CatalogDiscoveryCache {
    fn lookup(&self, fingerprint: &CatalogFingerprint) -> Option<PackLeaseDiscovery> {
        self.entry
            .as_ref()
            .filter(|(cached, _)| cached == fingerprint)
            .map(|(_, discovery)| discovery.clone())
    }

    fn replace(&mut self, fingerprint: CatalogFingerprint, discovery: PackLeaseDiscovery) {
        self.entry = Some((fingerprint, discovery));
    }
}

#[cfg(any(
    all(windows, target_arch = "x86_64"),
    all(
        target_os = "macos",
        any(target_arch = "aarch64", target_arch = "x86_64")
    )
))]
static PRODUCTION_DISCOVERY_CACHE: OnceLock<Mutex<CatalogDiscoveryCache>> = OnceLock::new();

#[cfg(any(
    all(windows, target_arch = "x86_64"),
    all(
        target_os = "macos",
        any(target_arch = "aarch64", target_arch = "x86_64")
    )
))]
#[derive(Debug)]
enum CatalogReadFailure {
    Unavailable,
    Rejected,
}

#[cfg(all(windows, target_arch = "x86_64"))]
fn read_bounded_catalog(
    path: &Path,
    install_root: &Path,
) -> Result<CatalogSnapshot, CatalogReadFailure> {
    use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_OPEN_REPARSE_POINT,
        FILE_SHARE_READ, GetFileInformationByHandle,
    };

    let mut options = OpenOptions::new();
    options
        .read(true)
        // Refuse replacement or mutation while the exact opened handle is
        // parsed; installer activation replaces the complete catalog later.
        .share_mode(FILE_SHARE_READ)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    let file = options.open(path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            CatalogReadFailure::Unavailable
        } else {
            CatalogReadFailure::Rejected
        }
    })?;
    let metadata = file.metadata().map_err(|_| CatalogReadFailure::Rejected)?;
    if !metadata.is_file()
        || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
        || metadata.len() > MAX_PACK_CATALOG_BYTES
    {
        return Err(CatalogReadFailure::Rejected);
    }
    let mut information = unsafe { std::mem::zeroed::<BY_HANDLE_FILE_INFORMATION>() };
    // SAFETY: file owns a valid live Windows file handle and information is a
    // correctly sized writable output structure.
    if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut information) } == 0
        || information.nNumberOfLinks != 1
    {
        return Err(CatalogReadFailure::Rejected);
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    File::take(file, MAX_PACK_CATALOG_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| CatalogReadFailure::Rejected)?;
    if bytes.len() as u64 > MAX_PACK_CATALOG_BYTES {
        return Err(CatalogReadFailure::Rejected);
    }
    let content_sha256: [u8; 32] = Sha256::digest(&bytes).into();
    Ok(CatalogSnapshot {
        bytes,
        fingerprint: CatalogFingerprint {
            install_root: install_root.to_path_buf(),
            volume_serial_number: information.dwVolumeSerialNumber,
            file_index: (u64::from(information.nFileIndexHigh) << 32)
                | u64::from(information.nFileIndexLow),
            content_sha256,
        },
    })
}

#[cfg(all(
    target_os = "macos",
    any(target_arch = "aarch64", target_arch = "x86_64")
))]
fn read_bounded_catalog(
    path: &Path,
    install_root: &Path,
) -> Result<CatalogSnapshot, CatalogReadFailure> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};

    let mut options = OpenOptions::new();
    options
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    let file = options.open(path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            CatalogReadFailure::Unavailable
        } else {
            CatalogReadFailure::Rejected
        }
    })?;
    let metadata = file.metadata().map_err(|_| CatalogReadFailure::Rejected)?;
    if !metadata.is_file() || metadata.nlink() != 1 || metadata.len() > MAX_PACK_CATALOG_BYTES {
        return Err(CatalogReadFailure::Rejected);
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    File::take(file, MAX_PACK_CATALOG_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| CatalogReadFailure::Rejected)?;
    if bytes.len() as u64 > MAX_PACK_CATALOG_BYTES {
        return Err(CatalogReadFailure::Rejected);
    }
    let content_sha256: [u8; 32] = Sha256::digest(&bytes).into();
    Ok(CatalogSnapshot {
        bytes,
        fingerprint: CatalogFingerprint {
            install_root: install_root.to_path_buf(),
            device: metadata.dev(),
            inode: metadata.ino(),
            content_sha256,
        },
    })
}

/// Private packaging entrypoint. Stage 4's empty production trust root means a
/// non-empty release pack declaration always fails until key provisioning is
/// deliberately completed in a later stage.
pub(crate) fn maybe_run_pack_verifier() -> Option<i32> {
    let arguments = std::env::args_os().skip(1).collect::<Vec<_>>();
    let private_flag = std::ffi::OsStr::new("--scribe-verify-worker-pack");
    if !arguments.iter().any(|argument| argument == private_flag) {
        return None;
    }
    if arguments.len() != 2 || arguments[0] != private_flag {
        eprintln!("invalid private worker-pack verification invocation");
        return Some(2);
    }
    let root = std::path::PathBuf::from(&arguments[1]);
    let verifier = manifest::PackVerifier::new(
        &manifest::ProductionTrustRoot,
        manifest::Compatibility::current(&[
            manifest::PackBackend::Cuda,
            manifest::PackBackend::Vulkan,
            manifest::PackBackend::Metal,
        ]),
    );
    match verifier.verify(&root) {
        Ok(descriptor) => match serde_json::to_string(&descriptor) {
            Ok(serialized) => {
                println!("{serialized}");
                Some(0)
            }
            Err(error) => {
                eprintln!("worker-pack descriptor serialization failed: {error}");
                Some(1)
            }
        },
        Err(error) => {
            eprintln!("worker-pack verification failed: {error}");
            Some(1)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::sync::Arc;

    use super::manifest::PackBackend;
    use super::manifest::VerifiedPackLease;
    use super::{PackDiscoveryDiagnostic, PackDiscoveryIssue};
    use super::{ResolverHelloBindingBridge, VerifiedPackLaunchBinding};

    #[derive(Default)]
    struct FixtureDeviceStore {
        markers: Vec<super::device_release_epoch::EpochMarker>,
        scans: usize,
    }

    impl super::device_release_epoch::MarkerStore for FixtureDeviceStore {
        fn scan(
            &mut self,
        ) -> Result<
            Vec<super::device_release_epoch::EpochMarker>,
            super::device_release_epoch::AdmissionError,
        > {
            self.scans += 1;
            Ok(self.markers.clone())
        }

        fn append(
            &mut self,
            marker: &super::device_release_epoch::EpochMarker,
        ) -> Result<(), super::device_release_epoch::AdmissionError> {
            if !self.markers.contains(marker) {
                self.markers.push(marker.clone());
            }
            Ok(())
        }
    }

    fn release_authority(epoch: u64) -> super::ValidatedReleaseAuthority {
        super::ValidatedReleaseAuthority {
            release_security_epoch: epoch,
            keychain_access_group: "ABCDE12345.com.scribe.local-transcriber".to_owned(),
        }
    }

    #[test]
    fn keychain_access_group_is_bound_to_the_reviewed_app_namespace() {
        assert!(super::canonical_keychain_access_group(
            "ABCDE12345.com.scribe.local-transcriber"
        ));
        for rejected in [
            "",
            "abcde12345.com.scribe.local-transcriber",
            "ABCDE1234.com.scribe.local-transcriber",
            "ABCDE12345.com.scribe.other",
            "ABCDE12345.*",
        ] {
            assert!(
                !super::canonical_keychain_access_group(rejected),
                "unexpected Keychain namespace accepted: {rejected}"
            );
        }

        let authority = release_authority(1);
        assert!(super::release_epoch_identity_matches(
            &authority,
            1,
            "ABCDE12345.com.scribe.local-transcriber",
            "ABCDE12345.com.scribe.local-transcriber",
        ));
        assert!(!super::release_epoch_identity_matches(
            &authority,
            1,
            "ABCDE12345.com.scribe.local-transcriber",
            "OTHER12345.com.scribe.local-transcriber",
        ));
    }

    #[test]
    fn macos_bundle_executable_maps_to_resources_catalog_root() {
        let executable = std::path::Path::new("/Applications/Scribe.app/Contents/MacOS/Scribe");
        assert_eq!(
            super::macos_resource_root_from_executable(executable),
            Some(std::path::PathBuf::from(
                "/Applications/Scribe.app/Contents/Resources"
            ))
        );
        assert!(
            super::macos_resource_root_from_executable(std::path::Path::new(
                "/Applications/Scribe.app/contents/MacOS/Scribe"
            ))
            .is_none()
        );
        assert!(
            super::macos_resource_root_from_executable(std::path::Path::new(
                "/usr/local/bin/Scribe"
            ))
            .is_none()
        );
    }

    fn fixture_catalog_bytes(lease: &VerifiedPackLease, files: Vec<String>) -> Vec<u8> {
        let pack = lease.verified_pack();
        let relative_root = format!(
            "workers/packs/{}/{}/{}",
            pack.pack_id.as_str(),
            pack.pack_version.as_str(),
            pack.pack_digest
        );
        serde_json::to_vec(&serde_json::json!({
            "schema_version": 1,
            "packs": [{
                "pack_id": pack.pack_id.as_str(),
                "pack_version": pack.pack_version.as_str(),
                "pack_digest": pack.pack_digest,
                "security_epoch": pack.security_epoch,
                "runtime_abi_version": pack.runtime_abi_version,
                "backend": "vulkan",
                "provider": pack.provider,
                "target_os": pack.target_os,
                "target_arch": pack.target_arch,
                "worker_relative_path": pack.worker_relative_path,
                "root": relative_root,
                "installed_size_bytes": 1,
                "compressed_size_bytes": 1,
                "files": files,
            }]
        }))
        .unwrap()
    }

    fn authority_bytes_for_catalog(catalog_bytes: &[u8]) -> Vec<u8> {
        use sha2::{Digest, Sha256};

        let catalog: super::PackCatalog = serde_json::from_slice(catalog_bytes).unwrap();
        let release_security_epoch = catalog
            .packs
            .first()
            .map(|entry| entry.security_epoch)
            .unwrap_or(1);
        assert!(
            catalog
                .packs
                .iter()
                .all(|entry| entry.security_epoch == release_security_epoch),
            "release authority fixtures require one release-wide security epoch"
        );
        let entries = catalog
            .packs
            .into_iter()
            .map(|entry| super::PackReleaseAuthorityEntry {
                app_version: env!("CARGO_PKG_VERSION").to_owned(),
                build_revision: env!("SCRIBE_BUILD_REVISION").to_owned(),
                app_protocol_version: super::manifest::APP_PROTOCOL_VERSION,
                pack_id: entry.pack_id,
                pack_version: entry.pack_version,
                pack_digest: entry.pack_digest,
                security_epoch: entry.security_epoch,
                runtime_abi_version: entry.runtime_abi_version,
                backend: entry.backend,
                provider: entry.provider,
                target_os: entry.target_os,
                target_arch: entry.target_arch,
                worker_relative_path: entry.worker_relative_path,
                root: entry.root,
                installed_size_bytes: entry.installed_size_bytes,
                compressed_size_bytes: entry.compressed_size_bytes,
                files: entry.files,
            })
            .collect();
        serde_json::to_vec(&super::PackReleaseAuthority {
            schema_version: 2,
            catalog_sha256: format!("{:x}", Sha256::digest(catalog_bytes)),
            release_security_epoch,
            keychain_access_group: "ABCDE12345.com.scribe.local-transcriber".to_owned(),
            entries,
        })
        .unwrap()
    }

    #[test]
    fn embedded_empty_release_authority_is_canonical_exact_and_default_deny() {
        let empty_catalog = br#"{"schema_version":1,"packs":[]}"#;
        assert_eq!(
            serde_json::to_vec(
                &serde_json::from_slice::<super::PackReleaseAuthority>(
                    super::EMBEDDED_PACK_RELEASE_AUTHORITY
                )
                .unwrap()
            )
            .unwrap(),
            super::EMBEDDED_PACK_RELEASE_AUTHORITY
        );
        assert!(super::catalog_matches_release_authority(
            empty_catalog,
            super::EMBEDDED_PACK_RELEASE_AUTHORITY
        ));

        let state = super::manifest::test_support::temp_root("empty-authority-no-state")
            .join("discovery-state");
        let discovery = super::PackLeaseDiscovery {
            leases: Vec::new(),
            diagnostics: Vec::new(),
            catalog_generation: Some("empty-authority".to_owned()),
        };
        assert!(
            super::enforce_discovery_epochs_at(discovery, state.clone())
                .leases
                .is_empty()
        );
        assert!(
            !state.exists(),
            "empty authority must not create epoch state"
        );
    }

    #[test]
    fn release_authority_requires_exact_build_pack_target_and_inventory_binding() {
        let root = super::manifest::test_support::temp_root("release-authority-binding");
        let (_, lease) = super::manifest::test_support::leased_fixture(&root);
        let pack = lease.verified_pack();
        let relative_root = format!(
            "workers/packs/{}/{}/{}",
            pack.pack_id.as_str(),
            pack.pack_version.as_str(),
            pack.pack_digest
        );
        let mut files = lease
            .copy_entries()
            .iter()
            .map(|entry| format!("{relative_root}/{}", entry.path))
            .collect::<Vec<_>>();
        files.sort();
        let catalog = fixture_catalog_bytes(&lease, files);
        let exact = authority_bytes_for_catalog(&catalog);
        assert!(super::catalog_matches_release_authority(&catalog, &exact));

        for field in [
            "pack_digest",
            "security_epoch",
            "runtime_abi_version",
            "provider",
            "target_os",
            "target_arch",
            "files",
            "build_revision",
            "app_version",
            "app_protocol_version",
            "release_security_epoch",
            "keychain_access_group",
        ] {
            let mut authority: super::PackReleaseAuthority =
                serde_json::from_slice(&exact).unwrap();
            let entry = &mut authority.entries[0];
            match field {
                "pack_digest" => entry.pack_digest = "0".repeat(64),
                "security_epoch" => entry.security_epoch = 99,
                "runtime_abi_version" => entry.runtime_abi_version = 99,
                "provider" => entry.provider = "wrong-provider".to_owned(),
                "target_os" => entry.target_os = "wrong-target_os".to_owned(),
                "target_arch" => entry.target_arch = "wrong-target_arch".to_owned(),
                "files" => entry.files = vec!["replacement.dylib".to_owned()],
                "build_revision" => entry.build_revision = "wrong-build_revision".to_owned(),
                "app_version" => entry.app_version = "wrong-app_version".to_owned(),
                "app_protocol_version" => entry.app_protocol_version = 99,
                "release_security_epoch" | "keychain_access_group" => {}
                _ => unreachable!(),
            }
            if field == "release_security_epoch" {
                authority.release_security_epoch = 0;
            } else if field == "keychain_access_group" {
                authority.keychain_access_group = "not canonical".to_owned();
            }
            let tampered = serde_json::to_vec(&authority).unwrap();
            assert!(
                !super::catalog_matches_release_authority(&catalog, &tampered),
                "authority mismatch in {field} was accepted"
            );
        }

        let mut whitespace_tamper = exact.clone();
        whitespace_tamper.push(b'\n');
        assert!(
            !super::catalog_matches_release_authority(&catalog, &whitespace_tamper),
            "noncanonical authority bytes were accepted"
        );

        let mut unsafe_epoch: super::PackReleaseAuthority = serde_json::from_slice(&exact).unwrap();
        unsafe_epoch.release_security_epoch = super::MAX_RELEASE_SECURITY_EPOCH + 1;
        for entry in &mut unsafe_epoch.entries {
            entry.security_epoch = unsafe_epoch.release_security_epoch;
        }
        assert!(
            super::validated_release_authority_document(
                &serde_json::to_vec(&unsafe_epoch).unwrap()
            )
            .is_none(),
            "epochs above the exact JSON integer range were accepted"
        );

        let empty_catalog = br#"{"schema_version":1,"packs":[]}"#;
        let mut empty_positive: super::PackReleaseAuthority =
            serde_json::from_slice(&authority_bytes_for_catalog(empty_catalog)).unwrap();
        empty_positive.entries.clear();
        assert!(super::catalog_matches_release_authority(
            empty_catalog,
            &serde_json::to_vec(&empty_positive).unwrap()
        ));
        empty_positive.release_security_epoch = 0;
        empty_positive.keychain_access_group.clear();
        assert!(super::catalog_matches_release_authority(
            empty_catalog,
            &serde_json::to_vec(&empty_positive).unwrap()
        ));
        empty_positive.keychain_access_group = "ABCDE12345.com.scribe.local-transcriber".to_owned();
        assert!(!super::catalog_matches_release_authority(
            empty_catalog,
            &serde_json::to_vec(&empty_positive).unwrap()
        ));
        drop(lease);
        std::fs::remove_dir_all(root).unwrap();
    }

    struct FixtureBridge {
        lease: Arc<VerifiedPackLease>,
        hello_digest: String,
        stable_device_identity: String,
    }

    impl ResolverHelloBindingBridge for FixtureBridge {
        fn resolver_verified_pack_lease(&self) -> Arc<VerifiedPackLease> {
            Arc::clone(&self.lease)
        }

        #[cfg(unix)]
        fn resolver_unix_launch_authority(
            &self,
        ) -> Option<Arc<super::launch_binding::UnixPackExecAuthority>> {
            use std::os::unix::fs::OpenOptionsExt;
            let executable = std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open(self.lease.worker_path())
                .unwrap();
            let dependency_root = std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open(&self.lease.verified_pack().root)
                .unwrap();
            Some(Arc::new(
                super::launch_binding::UnixPackExecAuthority::fixture(
                    Arc::clone(&self.lease),
                    executable,
                    dependency_root,
                ),
            ))
        }

        fn hello_pack_id(&self) -> &str {
            self.lease.verified_pack().pack_id.as_str()
        }

        fn hello_pack_version(&self) -> &str {
            self.lease.verified_pack().pack_version.as_str()
        }

        fn hello_pack_digest(&self) -> &str {
            &self.hello_digest
        }

        fn hello_security_epoch(&self) -> u64 {
            self.lease.verified_pack().security_epoch
        }

        fn hello_runtime_abi(&self) -> u16 {
            self.lease.verified_pack().runtime_abi_version
        }

        fn hello_backend(&self) -> PackBackend {
            self.lease.verified_pack().backend
        }

        fn hello_provider(&self) -> &str {
            &self.lease.verified_pack().provider
        }

        fn hello_stable_device_identity(&self) -> &str {
            &self.stable_device_identity
        }

        fn hello_process_index(&self) -> Option<usize> {
            Some(0)
        }

        fn hello_display_name(&self) -> &str {
            "Fixture GPU"
        }

        fn hello_driver_version(&self) -> Option<&str> {
            Some("fixture-driver-1")
        }

        fn hello_device_class(&self) -> crate::backend_policy::DeviceClass {
            crate::backend_policy::DeviceClass::DiscreteGpu
        }

        fn hello_vendor(&self) -> crate::backend_policy::GpuVendor {
            crate::backend_policy::GpuVendor::Nvidia
        }

        fn hello_memory_total_bytes(&self) -> u64 {
            8 * 1024 * 1024 * 1024
        }

        fn hello_memory_available_bytes(&self) -> u64 {
            6 * 1024 * 1024 * 1024
        }
    }

    fn fixture_bridge() -> (std::path::PathBuf, FixtureBridge) {
        let root = super::manifest::test_support::temp_root("launch-binding");
        let (_, lease) = super::manifest::test_support::leased_fixture(&root);
        let digest = lease.verified_pack().pack_digest.clone();
        let bridge = FixtureBridge {
            lease: Arc::new(lease),
            hello_digest: digest,
            stable_device_identity: "pci:0000:01:00.0".to_owned(),
        };
        (root, bridge)
    }

    #[test]
    fn stage_four_production_trust_root_and_legacy_registry_are_empty() {
        assert!(super::production_registry().is_empty());
        assert!(
            super::manifest::TrustRoot::public_key(
                &super::manifest::ProductionTrustRoot,
                "fixture-ed25519-v1"
            )
            .is_none()
        );
    }

    #[test]
    fn stage_six_single_worker_payload_gate_rejects_auxiliary_files_and_replacement() {
        use sha2::{Digest, Sha256};

        let valid_root = super::manifest::test_support::temp_root("single-worker-payload");
        let (_, valid) = super::manifest::test_support::leased_fixture(&valid_root);
        assert!(super::single_executable_signed_payload(&valid));

        let source_root = super::manifest::test_support::temp_root("auxiliary-payload-source");
        let source = source_root.join("pack");
        std::fs::create_dir_all(source.join("lib")).unwrap();
        std::fs::write(source.join("lib/replaceable.dylib"), b"signed auxiliary").unwrap();
        let mut manifest = super::manifest::test_support::base_manifest();
        manifest.payload.push(super::manifest::PayloadEntry {
            path: "lib/replaceable.dylib".to_owned(),
            size_bytes: b"signed auxiliary".len() as u64,
            sha256: format!("{:x}", Sha256::digest(b"signed auxiliary")),
        });
        manifest
            .payload
            .sort_by(|left, right| left.path.cmp(&right.path));
        super::manifest::test_support::write_signed(&source, manifest);
        let (owner, auxiliary) =
            super::manifest::test_support::lease_existing_fixture(&source).unwrap();
        assert!(!super::single_executable_signed_payload(&auxiliary));

        let auxiliary_path = auxiliary.verified_pack().root.join("lib/replaceable.dylib");
        if std::fs::remove_file(&auxiliary_path).is_ok() {
            std::fs::write(&auxiliary_path, b"replacement auxiliary").unwrap();
        }
        assert!(
            !super::single_executable_signed_payload(&auxiliary),
            "a retained lease with any auxiliary inventory is rejected before route construction"
        );

        drop(valid);
        std::fs::remove_dir_all(valid_root).unwrap();
        drop(auxiliary);
        std::fs::remove_dir_all(owner).unwrap();
        std::fs::remove_dir_all(source_root).unwrap();
    }

    #[test]
    fn cached_discovery_is_readmitted_against_persistent_epoch_state() {
        fn lease_at_epoch(
            label: &str,
            epoch: u64,
        ) -> (
            std::path::PathBuf,
            std::path::PathBuf,
            Arc<VerifiedPackLease>,
        ) {
            let source_root = super::manifest::test_support::temp_root(label);
            let source = source_root.join("pack");
            let mut manifest = super::manifest::test_support::base_manifest();
            manifest.security_epoch = epoch;
            super::manifest::test_support::write_signed(&source, manifest);
            let (owner, lease) =
                super::manifest::test_support::lease_existing_fixture(&source).unwrap();
            (source_root, owner, Arc::new(lease))
        }

        let state_parent = super::manifest::test_support::temp_root("catalog-epoch-state");
        let state = state_parent.join("private");
        let (high_source, high_owner, high) = lease_at_epoch("catalog-epoch-high", 3);
        let high_discovery = super::PackLeaseDiscovery {
            leases: vec![Arc::clone(&high)],
            diagnostics: Vec::new(),
            catalog_generation: Some("high-cache-entry".to_owned()),
        };
        assert_eq!(
            super::enforce_discovery_epochs_at(high_discovery.clone(), state.clone())
                .leases
                .len(),
            1
        );
        assert_eq!(
            super::enforce_discovery_epochs_at(high_discovery, state.clone())
                .leases
                .len(),
            1,
            "a cached same-epoch lease remains admissible"
        );

        let (low_source, low_owner, low) = lease_at_epoch("catalog-epoch-low", 2);
        let rejected = super::enforce_discovery_epochs_at(
            super::PackLeaseDiscovery {
                leases: vec![Arc::clone(&low)],
                diagnostics: Vec::new(),
                catalog_generation: Some("rolled-back-cache-entry".to_owned()),
            },
            state,
        );
        assert!(rejected.leases.is_empty());
        assert_eq!(
            rejected.diagnostics[0].issue,
            PackDiscoveryIssue::SecurityEpochStateRejected
        );

        drop(high);
        drop(low);
        for root in [high_source, high_owner, low_source, low_owner, state_parent] {
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn fresh_and_cached_discovery_fail_closed_quickly_on_epoch_lock_contention() {
        let pack_root = super::manifest::test_support::temp_root("discovery-contention-pack");
        let (_, lease) = super::manifest::test_support::leased_fixture(&pack_root);
        let discovery = super::PackLeaseDiscovery {
            leases: vec![Arc::new(lease)],
            diagnostics: Vec::new(),
            catalog_generation: Some("contended-catalog".to_owned()),
        };
        let state_parent = super::manifest::test_support::temp_root("discovery-contention-state");
        let state = state_parent.join("private");
        let held =
            super::store::exclusive_file_lock(&state.join(super::store::DISCOVERY_EPOCH_LOCK_NAME))
                .unwrap();

        for admission in ["fresh", "cached"] {
            let started = std::time::Instant::now();
            let rejected = super::enforce_discovery_epochs_at(discovery.clone(), state.clone());
            assert!(rejected.leases.is_empty(), "{admission} admission escaped");
            assert_eq!(
                rejected.diagnostics[0].issue,
                PackDiscoveryIssue::SecurityEpochStateRejected
            );
            assert!(
                started.elapsed() < std::time::Duration::from_secs(2),
                "{admission} discovery lock contention was not bounded"
            );
        }
        assert!(
            !state.join("discovery-security-epochs.json").exists(),
            "contended discovery mutated epoch state"
        );

        drop(held);
        drop(discovery);
        std::fs::remove_dir_all(pack_root).unwrap();
        std::fs::remove_dir_all(state_parent).unwrap();
    }

    #[test]
    fn exact_release_authority_survives_whole_ledger_directory_deletion() {
        fn catalog_for_lease(lease: &VerifiedPackLease) -> Vec<u8> {
            let pack = lease.verified_pack();
            let relative_root = format!(
                "workers/packs/{}/{}/{}",
                pack.pack_id.as_str(),
                pack.pack_version.as_str(),
                pack.pack_digest
            );
            let mut files = lease
                .copy_entries()
                .iter()
                .map(|entry| format!("{relative_root}/{}", entry.path))
                .collect::<Vec<_>>();
            files.sort();
            fixture_catalog_bytes(lease, files)
        }

        fn lease_at_epoch(
            label: &str,
            epoch: u64,
        ) -> (std::path::PathBuf, std::path::PathBuf, VerifiedPackLease) {
            let root = super::manifest::test_support::temp_root(label);
            let source = root.join("pack");
            let mut manifest = super::manifest::test_support::base_manifest();
            manifest.security_epoch = epoch;
            super::manifest::test_support::write_signed(&source, manifest);
            let (owner, lease) =
                super::manifest::test_support::lease_existing_fixture(&source).unwrap();
            (root, owner, lease)
        }

        let state_parent = super::manifest::test_support::temp_root("authority-delete-state");
        let state = state_parent.join("dedicated-ledger");
        let (high_root, high_owner, high) = lease_at_epoch("authority-high-pack", 3);
        let high_catalog = catalog_for_lease(&high);
        let embedded_authority = authority_bytes_for_catalog(&high_catalog);
        assert!(super::catalog_matches_release_authority(
            &high_catalog,
            &embedded_authority
        ));
        super::store::DiscoveryEpochLedger::new(&state)
            .admit(&[high.verified_pack()])
            .unwrap();
        std::fs::remove_dir_all(&state).unwrap();

        let (low_root, low_owner, low) = lease_at_epoch("authority-low-pack", 2);
        let low_catalog = catalog_for_lease(&low);
        assert!(
            !super::catalog_matches_release_authority(&low_catalog, &embedded_authority),
            "deleting the whole ledger must not authorize an older signed pack"
        );
        assert!(
            !state.exists(),
            "release-authority rejection must happen before ledger admission"
        );

        drop(high);
        drop(low);
        for root in [high_root, high_owner, low_root, low_owner, state_parent] {
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn deleted_app_data_cannot_reset_device_release_floor() {
        let mut device = FixtureDeviceStore::default();
        super::device_release_epoch::admit_with_store(8, &mut device).unwrap();

        let root = super::manifest::test_support::temp_root("device-floor-old-app");
        let (_, lease) = super::manifest::test_support::leased_fixture(&root);
        let discovery = super::PackLeaseDiscovery {
            leases: vec![Arc::new(lease)],
            diagnostics: Vec::new(),
            catalog_generation: Some("matching-old-signed-authority".to_owned()),
        };
        let local_ledger_called = Cell::new(false);
        let rejected = super::enforce_device_epoch_with_store(
            discovery,
            &release_authority(7),
            "ABCDE12345.com.scribe.local-transcriber",
            "ABCDE12345.com.scribe.local-transcriber",
            &mut device,
            |_| {
                local_ledger_called.set(true);
                Ok(())
            },
        );
        assert!(rejected.leases.is_empty());
        assert_eq!(
            rejected.diagnostics[0].issue,
            PackDiscoveryIssue::DeviceRollbackAuthorityRejected
        );
        assert!(
            !local_ledger_called.get(),
            "device denial must precede the deleted app-data ledger"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn cached_discovery_rechecks_device_authority_and_local_ledger_is_advisory() {
        let root = super::manifest::test_support::temp_root("device-floor-cached");
        let (_, lease) = super::manifest::test_support::leased_fixture(&root);
        let discovery = super::PackLeaseDiscovery {
            leases: vec![Arc::new(lease)],
            diagnostics: Vec::new(),
            catalog_generation: Some("cached-authority".to_owned()),
        };
        let mut device = FixtureDeviceStore::default();
        for _ in ["fresh", "cached"] {
            let admitted = super::enforce_device_epoch_with_store(
                discovery.clone(),
                &release_authority(5),
                "ABCDE12345.com.scribe.local-transcriber",
                "ABCDE12345.com.scribe.local-transcriber",
                &mut device,
                |_| Err(()),
            );
            assert_eq!(
                admitted.leases.len(),
                1,
                "a failed app-data ledger must not override the device authority"
            );
        }
        assert_eq!(device.scans, 4, "fresh and cached paths each scan twice");
        drop(discovery);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn positive_empty_catalog_revocation_raises_release_floor() {
        let empty = super::PackLeaseDiscovery {
            leases: Vec::new(),
            diagnostics: Vec::new(),
            catalog_generation: Some("empty-revocation".to_owned()),
        };
        let mut device = FixtureDeviceStore::default();
        let admitted = super::enforce_device_epoch_with_store(
            empty.clone(),
            &release_authority(12),
            "ABCDE12345.com.scribe.local-transcriber",
            "ABCDE12345.com.scribe.local-transcriber",
            &mut device,
            |_| Ok(()),
        );
        assert!(admitted.diagnostics.is_empty());
        let rejected = super::enforce_device_epoch_with_store(
            empty,
            &release_authority(11),
            "ABCDE12345.com.scribe.local-transcriber",
            "ABCDE12345.com.scribe.local-transcriber",
            &mut device,
            |_| Ok(()),
        );
        assert_eq!(
            rejected.diagnostics[0].issue,
            PackDiscoveryIssue::DeviceRollbackAuthorityRejected
        );
    }

    #[test]
    fn positive_epoch_requires_exact_compiled_keychain_group() {
        let discovery = super::PackLeaseDiscovery {
            leases: Vec::new(),
            diagnostics: Vec::new(),
            catalog_generation: Some("group-mismatch".to_owned()),
        };
        for compiled in ["", "ABCDE12345.com.scribe.other"] {
            let mut device = FixtureDeviceStore::default();
            let rejected = super::enforce_device_epoch_with_store(
                discovery.clone(),
                &release_authority(1),
                compiled,
                "ABCDE12345.com.scribe.local-transcriber",
                &mut device,
                |_| Ok(()),
            );
            assert_eq!(
                rejected.diagnostics[0].issue,
                PackDiscoveryIssue::DeviceRollbackAuthorityRejected
            );
            assert_eq!(
                device.scans, 0,
                "namespace mismatch must not touch Keychain"
            );
        }
    }

    #[test]
    fn launch_binding_requires_exact_resolver_and_hello_metadata() {
        let (root, bridge) = fixture_bridge();
        let binding = VerifiedPackLaunchBinding::try_from_resolver_hello_bridge(&bridge)
            .expect("matching resolver and Hello metadata bind");
        assert_eq!(binding.verified_pack(), bridge.lease.verified_pack());
        assert_eq!(
            binding.verified_pack_lease().verified_pack(),
            bridge.lease.verified_pack()
        );
        assert_eq!(binding.stable_device_identity(), "pci:0000:01:00.0");

        let (mismatched_root, mut mismatched) = fixture_bridge();
        mismatched.hello_digest = "b".repeat(64);
        assert!(VerifiedPackLaunchBinding::try_from_resolver_hello_bridge(&mismatched).is_none());
        drop(binding);
        drop(bridge);
        std::fs::remove_dir_all(root).unwrap();
        drop(mismatched);
        std::fs::remove_dir_all(mismatched_root).unwrap();
    }

    #[test]
    fn discovery_diagnostics_are_categorical_path_free_and_bounded() {
        let pack_id = super::manifest::StoreComponent::new("scribe-vulkan-windows-x64").unwrap();
        let diagnostics = vec![
            PackDiscoveryDiagnostic::catalog(PackDiscoveryIssue::CatalogRejected),
            PackDiscoveryDiagnostic::pack(
                PackDiscoveryIssue::SignatureOrInventoryRejected,
                &pack_id,
                PackBackend::Vulkan,
            ),
            PackDiscoveryDiagnostic::pack(
                PackDiscoveryIssue::NotAutoQualified,
                &pack_id,
                PackBackend::Vulkan,
            ),
            PackDiscoveryDiagnostic::pack(
                PackDiscoveryIssue::ProviderProbeRejected,
                &pack_id,
                PackBackend::Vulkan,
            ),
        ];
        let summary = super::diagnostic_summary(&diagnostics);
        assert!(summary.contains("catalog was rejected"));
        assert!(summary.contains("signature or installed inventory was rejected"));
        assert!(summary.contains("not qualified for Auto"));
        assert!(summary.contains("provider probe was rejected"));
        assert!(!summary.contains(':'));
        assert!(summary.len() < 2_048);
    }

    #[cfg(windows)]
    #[test]
    fn catalog_reader_retains_exact_handle_and_rejects_hardlinks() {
        let root = super::manifest::test_support::temp_root("pack-catalog-reader");
        let catalog = root.join(super::PACK_CATALOG_NAME);
        std::fs::write(&catalog, br#"{"schema_version":1,"packs":[]}"#).unwrap();
        assert!(super::read_bounded_catalog(&catalog, &root).is_ok());

        let alias = root.join("catalog-alias.json");
        std::fs::hard_link(&catalog, &alias).unwrap();
        assert!(matches!(
            super::read_bounded_catalog(&catalog, &root),
            Err(super::CatalogReadFailure::Rejected)
        ));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn catalog_discovery_cache_is_single_entry_and_fingerprint_bound() {
        let root = super::manifest::test_support::temp_root("pack-catalog-cache");
        let catalog = root.join(super::PACK_CATALOG_NAME);
        std::fs::write(&catalog, br#"{"schema_version":1,"packs":[]}"#).unwrap();
        let first = super::read_bounded_catalog(&catalog, &root)
            .unwrap()
            .fingerprint;
        let discovery = super::PackLeaseDiscovery {
            leases: Vec::new(),
            diagnostics: vec![PackDiscoveryDiagnostic::catalog(
                PackDiscoveryIssue::CatalogUnavailable,
            )],
            catalog_generation: Some(first.generation_id()),
        };
        let mut cache = super::CatalogDiscoveryCache::default();
        cache.replace(first.clone(), discovery);
        assert_eq!(
            cache.lookup(&first).unwrap().diagnostics[0].issue,
            PackDiscoveryIssue::CatalogUnavailable
        );

        std::fs::write(
            &catalog,
            br#"{"schema_version":1,"packs":[],"invalid":true}"#,
        )
        .unwrap();
        let changed = super::read_bounded_catalog(&catalog, &root)
            .unwrap()
            .fingerprint;
        assert_ne!(first, changed);
        assert!(cache.lookup(&changed).is_none());
        cache.replace(
            changed,
            super::PackLeaseDiscovery {
                leases: Vec::new(),
                diagnostics: vec![PackDiscoveryDiagnostic::catalog(
                    PackDiscoveryIssue::CatalogRejected,
                )],
                catalog_generation: Some("f".repeat(88)),
            },
        );
        assert!(
            cache.lookup(&first).is_none(),
            "the cache remains bounded to one catalog"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn verified_nonempty_catalog_requires_canonical_exact_inventory() {
        let root = super::manifest::test_support::temp_root("verified-pack-catalog");
        let (verifier, fixture_lease) = super::manifest::test_support::leased_fixture(&root);
        let pack = fixture_lease.verified_pack();
        let relative_root = format!(
            "workers/packs/{}/{}/{}",
            pack.pack_id.as_str(),
            pack.pack_version.as_str(),
            pack.pack_digest
        );
        let mut canonical_files = fixture_lease
            .copy_entries()
            .iter()
            .map(|entry| format!("{relative_root}/{}", entry.path))
            .collect::<Vec<_>>();
        canonical_files.sort();

        let success = super::verify_catalog_entries_with_verifier(
            &root,
            &fixture_catalog_bytes(&fixture_lease, canonical_files.clone()),
            "fixture-generation".to_owned(),
            &verifier,
        );
        assert_eq!(success.leases.len(), 1);
        assert!(
            success.diagnostics.is_empty(),
            "verified catalog discovery must not label an explicit-GPU pack as Auto-ineligible"
        );

        let assert_inventory_rejected = |files: Vec<String>, issue: PackDiscoveryIssue| {
            let discovery = super::verify_catalog_entries_with_verifier(
                &root,
                &fixture_catalog_bytes(&fixture_lease, files),
                "fixture-generation".to_owned(),
                &verifier,
            );
            assert!(discovery.leases.is_empty());
            assert_eq!(
                discovery.diagnostics,
                vec![PackDiscoveryDiagnostic::pack(
                    issue,
                    &pack.pack_id,
                    PackBackend::Vulkan,
                )]
            );
        };

        let mut reordered = canonical_files.clone();
        reordered.swap(0, 1);
        assert_inventory_rejected(reordered, PackDiscoveryIssue::CatalogInventoryMismatch);

        let mut duplicate = canonical_files.clone();
        duplicate[1] = duplicate[0].clone();
        assert_inventory_rejected(duplicate, PackDiscoveryIssue::CatalogInventoryMismatch);

        let mut missing = canonical_files.clone();
        missing.pop();
        assert_inventory_rejected(missing, PackDiscoveryIssue::EntryIncompatible);

        let mut unexpected = canonical_files.clone();
        unexpected.push(format!("{relative_root}/unexpected.dll"));
        unexpected.sort();
        assert_inventory_rejected(unexpected, PackDiscoveryIssue::CatalogInventoryMismatch);

        drop(success);
        drop(fixture_lease);
        std::fs::remove_dir_all(root).unwrap();
    }
}
