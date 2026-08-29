//! Verified GPU worker-pack infrastructure.
//!
//! Stage 3 deliberately has no production trust root or registry entries. The
//! types in this module are private infrastructure for accepting externally
//! signed packs later without making a GPU provider discoverable today.

pub(crate) mod health;
pub(crate) mod manifest;
pub(crate) mod store;

use std::fs;
use std::sync::Arc;

use serde::Deserialize;

// Stage 4 consumes the bridge re-export; Stage 3's production registry is
// deliberately empty, so the non-test binary has no implementation yet.
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
        fn resolver_unix_launch_authority(&self) -> Arc<UnixPackExecAuthority>;
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
            let unix_exec_authority = bridge.resolver_unix_launch_authority();
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
            let driver_is_bounded = driver_version.is_none_or(|value| {
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
/// raw verified descriptor. Stage 3 deliberately constructs the empty value.
pub(crate) struct ProductionPackRegistry {
    bindings: Vec<VerifiedPackLaunchBinding>,
}

impl ProductionPackRegistry {
    pub(crate) fn empty() -> Self {
        Self {
            bindings: Vec::new(),
        }
    }

    #[allow(dead_code)]
    pub(crate) fn from_launch_bindings(bindings: Vec<VerifiedPackLaunchBinding>) -> Self {
        Self { bindings }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.bindings.is_empty()
    }

    pub(crate) fn into_bindings(self) -> Vec<VerifiedPackLaunchBinding> {
        self.bindings
    }
}

/// Production discovery remains fail closed until a persistent signing key and
/// declared pack catalog are provisioned by a later release stage.
pub(crate) fn production_registry() -> ProductionPackRegistry {
    ProductionPackRegistry::empty()
}

const PACK_CATALOG_NAME: &str = "worker-pack-catalog.json";
const MAX_PACK_CATALOG_BYTES: u64 = 512 * 1024;
const MAX_PRODUCTION_PACKS: usize = 8;

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

/// Verifies the bounded installed catalog and returns retained pack leases.
/// It never loads a provider or creates launch authority. A malformed catalog,
/// absent production trust key, or incompatible pack projects to no GPU route.
#[cfg(all(windows, target_arch = "x86_64"))]
pub(crate) fn discover_production_pack_leases() -> Vec<Arc<manifest::VerifiedPackLease>> {
    discover_pack_leases_from_current_install().unwrap_or_default()
}

#[cfg(not(all(windows, target_arch = "x86_64")))]
pub(crate) fn discover_production_pack_leases() -> Vec<Arc<manifest::VerifiedPackLease>> {
    Vec::new()
}

#[cfg(all(windows, target_arch = "x86_64"))]
fn discover_pack_leases_from_current_install()
-> Result<Vec<Arc<manifest::VerifiedPackLease>>, String> {
    let executable = std::env::current_exe().map_err(|error| error.to_string())?;
    let install_root = executable
        .parent()
        .ok_or_else(|| "Scribe executable has no install root".to_owned())?;
    let catalog_path = install_root.join(PACK_CATALOG_NAME);
    let metadata = fs::symlink_metadata(&catalog_path).map_err(|error| error.to_string())?;
    if !metadata.is_file() || metadata.len() > MAX_PACK_CATALOG_BYTES {
        return Err("worker-pack catalog is absent, linked, or oversized".to_owned());
    }
    let bytes = fs::read(&catalog_path).map_err(|error| error.to_string())?;
    let catalog: PackCatalog = serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
    if catalog.schema_version != 1 || catalog.packs.len() > MAX_PRODUCTION_PACKS {
        return Err("worker-pack catalog schema or count is invalid".to_owned());
    }
    let packs_root = install_root.join("workers").join("packs");
    let verifier = manifest::PackVerifier::new(
        &manifest::ProductionTrustRoot,
        manifest::Compatibility::current(&[
            manifest::PackBackend::Cuda,
            manifest::PackBackend::Vulkan,
        ]),
    );
    let mut leases = Vec::new();
    for entry in catalog.packs {
        if entry.target_os != "windows"
            || entry.target_arch != "x86_64"
            || entry.runtime_abi_version != manifest::RUNTIME_ABI_VERSION
            || entry.installed_size_bytes == 0
            || entry.compressed_size_bytes == 0
            || entry.files.len() < 3
            || entry.files.len() > manifest::MAX_FILES + 2
        {
            continue;
        }
        let relative_root = format!(
            "workers/packs/{}/{}/{}",
            entry.pack_id.as_str(),
            entry.pack_version.as_str(),
            entry.pack_digest
        );
        if entry.root != relative_root {
            continue;
        }
        let Ok(pinned) = manifest::PinnedPackRoot::open(
            &packs_root,
            [&entry.pack_id, &entry.pack_version],
            &entry.pack_digest,
        ) else {
            continue;
        };
        let Ok(lease) = verifier.verify_pinned(pinned) else {
            continue;
        };
        let observed = lease.verified_pack();
        let expected_files = lease
            .copy_entries()
            .iter()
            .map(|file| format!("{relative_root}/{}", file.path))
            .collect::<Vec<_>>();
        let mut catalog_files = entry.files;
        catalog_files.sort();
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
            || catalog_files != expected_files
        {
            continue;
        }
        leases.push(Arc::new(lease));
    }
    Ok(leases)
}

/// Private packaging entrypoint. Stage 3's empty production trust root means a
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
    use super::{ResolverHelloBindingBridge, VerifiedPackLaunchBinding};

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
        ) -> Arc<super::launch_binding::UnixPackExecAuthority> {
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
            Arc::new(super::launch_binding::UnixPackExecAuthority::fixture(
                Arc::clone(&self.lease),
                executable,
                dependency_root,
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
    fn stage_three_production_registry_and_trust_root_are_empty() {
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
}
