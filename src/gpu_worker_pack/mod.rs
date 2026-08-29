//! Verified GPU worker-pack infrastructure.
//!
//! Stage 3 deliberately has no production trust root or registry entries. The
//! types in this module are private infrastructure for accepting externally
//! signed packs later without making a GPU provider discoverable today.

pub(crate) mod health;
pub(crate) mod manifest;
pub(crate) mod store;

// Stage 4 consumes the bridge re-export; Stage 3's production registry is
// deliberately empty, so the non-test binary has no implementation yet.
#[allow(unused_imports)]
pub(crate) use launch_binding::{ResolverHelloBindingBridge, VerifiedPackLaunchBinding};

mod launch_binding {
    #[cfg(unix)]
    use std::fs::File;
    use std::sync::Arc;

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
        fn hello_runtime_abi(&self) -> u16;
        fn hello_backend(&self) -> PackBackend;
        fn hello_provider(&self) -> &str;
        fn hello_stable_device_identity(&self) -> &str;
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
    #[derive(Debug)]
    pub(crate) struct VerifiedPackLaunchBinding {
        verified_pack_lease: Arc<VerifiedPackLease>,
        stable_device_identity: String,
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
            let stable_device_is_canonical = !stable_device_identity.is_empty()
                && stable_device_identity.len() <= 256
                && stable_device_identity
                    .bytes()
                    .all(|byte| (0x20..=0x7e).contains(&byte))
                && stable_device_identity == stable_device_identity.to_ascii_lowercase();
            (stable_device_is_canonical
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
                && bridge.hello_runtime_abi() == pack.runtime_abi_version
                && bridge.hello_backend() == pack.backend
                && bridge.hello_provider() == pack.provider)
                .then(|| Self {
                    verified_pack_lease: lease,
                    stable_device_identity: stable_device_identity.to_owned(),
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

        pub(crate) fn stable_device_identity(&self) -> &str {
            &self.stable_device_identity
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
}

/// Production discovery remains fail closed until a persistent signing key and
/// declared pack catalog are provisioned by a later release stage.
pub(crate) fn production_registry() -> ProductionPackRegistry {
    ProductionPackRegistry::empty()
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
