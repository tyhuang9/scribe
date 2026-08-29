//! Verified GPU worker-pack infrastructure.
//!
//! Stage 4 discovers verified Windows x64 packs and turns only challenge-bound
//! resolver/Hello results into explicit-GPU candidates. Production trust is
//! deliberately empty until a separate public-key review is complete, and
//! Auto remains default-denied to every GPU pack.

pub(crate) mod health;
pub(crate) mod manifest;
pub(crate) mod store;

use std::fs::{File, OpenOptions};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;
#[cfg(all(windows, target_arch = "x86_64"))]
use std::sync::{Mutex, OnceLock};

use serde::Deserialize;
#[cfg(all(windows, target_arch = "x86_64"))]
use sha2::{Digest, Sha256};

// Stage 4 consumes the bridge re-export from the concrete Windows resolver.
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
        pub(crate) fn executable_fd(&self) -> &File {
            &self.executable_fd
        }

        pub(crate) fn dependency_root_fd(&self) -> &File {
            &self.dependency_root_fd
        }

        pub(crate) fn verified_pack_lease(&self) -> &Arc<VerifiedPackLease> {
            &self.verified_pack_lease
        }

        #[cfg(test)]
        fn fixture(
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

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PackCatalog {
    schema_version: u16,
    packs: Vec<PackCatalogEntry>,
}

#[derive(Deserialize)]
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

/// Verifies the bounded installed catalog and returns retained pack leases plus
/// categorical, path-free diagnostics for every skipped entry.
/// It never loads a provider or creates launch authority. A malformed catalog,
/// absent production trust key, or incompatible pack projects to no GPU route.
#[cfg(all(windows, target_arch = "x86_64"))]
pub(crate) fn discover_production_pack_leases() -> PackLeaseDiscovery {
    discover_pack_leases_from_current_install()
}

#[cfg(not(all(windows, target_arch = "x86_64")))]
pub(crate) fn discover_production_pack_leases() -> PackLeaseDiscovery {
    PackLeaseDiscovery {
        leases: Vec::new(),
        diagnostics: vec![PackDiscoveryDiagnostic::catalog(
            PackDiscoveryIssue::UnsupportedPlatform,
        )],
        catalog_generation: None,
    }
}

#[cfg(all(windows, target_arch = "x86_64"))]
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
    let Some(install_root) = executable.parent() else {
        return PackLeaseDiscovery {
            leases: Vec::new(),
            diagnostics: vec![PackDiscoveryDiagnostic::catalog(
                PackDiscoveryIssue::CatalogUnavailable,
            )],
            catalog_generation: None,
        };
    };
    discover_pack_leases_from_install_root(install_root)
}

#[cfg(all(windows, target_arch = "x86_64"))]
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
    let cache =
        PRODUCTION_DISCOVERY_CACHE.get_or_init(|| Mutex::new(CatalogDiscoveryCache::default()));
    if let Ok(cache) = cache.lock()
        && let Some(discovery) = cache.lookup(&catalog.fingerprint)
    {
        return discovery;
    }
    let fingerprint = catalog.fingerprint;
    let generation = fingerprint.generation_id();
    let discovery = verify_catalog_entries(install_root, &catalog.bytes, generation);
    if let Ok(mut cache) = cache.lock() {
        cache.replace(fingerprint, discovery.clone());
    }
    discovery
}

#[cfg(all(windows, target_arch = "x86_64"))]
fn verify_catalog_entries(
    install_root: &Path,
    bytes: &[u8],
    catalog_generation: String,
) -> PackLeaseDiscovery {
    let verifier = manifest::PackVerifier::new(
        &manifest::ProductionTrustRoot,
        manifest::Compatibility::current(&[
            manifest::PackBackend::Cuda,
            manifest::PackBackend::Vulkan,
        ]),
    );
    verify_catalog_entries_with_verifier(install_root, bytes, catalog_generation, &verifier)
}

#[cfg(all(windows, target_arch = "x86_64"))]
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
        if entry.target_os != "windows"
            || entry.target_arch != "x86_64"
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
        diagnostics.push(PackDiscoveryDiagnostic::pack(
            PackDiscoveryIssue::NotAutoQualified,
            &entry.pack_id,
            entry.backend,
        ));
        leases.push(Arc::new(lease));
    }
    PackLeaseDiscovery {
        leases,
        diagnostics,
        catalog_generation: Some(catalog_generation),
    }
}

#[cfg(all(windows, target_arch = "x86_64"))]
#[derive(Clone, Debug, Eq, PartialEq)]
struct CatalogFingerprint {
    install_root: PathBuf,
    volume_serial_number: u32,
    file_index: u64,
    content_sha256: [u8; 32],
}

#[cfg(all(windows, target_arch = "x86_64"))]
impl CatalogFingerprint {
    fn generation_id(&self) -> String {
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
}

#[cfg(all(windows, target_arch = "x86_64"))]
struct CatalogSnapshot {
    bytes: Vec<u8>,
    fingerprint: CatalogFingerprint,
}

#[cfg(all(windows, target_arch = "x86_64"))]
#[derive(Default)]
struct CatalogDiscoveryCache {
    entry: Option<(CatalogFingerprint, PackLeaseDiscovery)>,
}

#[cfg(all(windows, target_arch = "x86_64"))]
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

#[cfg(all(windows, target_arch = "x86_64"))]
static PRODUCTION_DISCOVERY_CACHE: OnceLock<Mutex<CatalogDiscoveryCache>> = OnceLock::new();

#[cfg(all(windows, target_arch = "x86_64"))]
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
    use std::sync::Arc;

    use super::manifest::PackBackend;
    use super::manifest::VerifiedPackLease;
    use super::{PackDiscoveryDiagnostic, PackDiscoveryIssue};
    use super::{ResolverHelloBindingBridge, VerifiedPackLaunchBinding};

    #[cfg(windows)]
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
        assert_eq!(
            success.diagnostics,
            vec![PackDiscoveryDiagnostic::pack(
                PackDiscoveryIssue::NotAutoQualified,
                &pack.pack_id,
                PackBackend::Vulkan,
            )]
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
