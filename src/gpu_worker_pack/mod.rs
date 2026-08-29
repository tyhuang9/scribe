//! Verified GPU worker-pack infrastructure.
//!
//! Stage 3 deliberately has no production trust root or registry entries. The
//! types in this module are private infrastructure for accepting externally
//! signed packs later without making a GPU provider discoverable today.

pub(crate) mod health;
pub(crate) mod manifest;
pub(crate) mod store;

pub(crate) use launch_binding::{ResolverHelloBindingBridge, VerifiedPackLaunchBinding};
pub(crate) use manifest::VerifiedPack;

mod launch_binding {
    use super::manifest::{PackBackend, VerifiedPack};

    /// Stage 4 must implement this bridge on the concrete resolver/Hello path.
    /// Returning metadata is not sufficient by itself: the only constructor for
    /// the opaque binding compares every value with the reverified descriptor.
    pub(crate) trait ResolverHelloBindingBridge {
        fn resolver_verified_pack(&self) -> &VerifiedPack;
        fn hello_pack_id(&self) -> &str;
        fn hello_pack_version(&self) -> &str;
        fn hello_pack_digest(&self) -> &str;
        fn hello_runtime_abi(&self) -> u16;
        fn hello_backend(&self) -> PackBackend;
        fn hello_provider(&self) -> &str;
        fn hello_stable_device_identity(&self) -> &str;
    }

    /// Opaque proof that a concrete resolver result and worker Hello agreed on
    /// the exact verified pack and stable device. Its fields are private to this
    /// child module, so production discovery cannot fabricate one from a raw
    /// `VerifiedPack`.
    #[derive(Clone, Debug, Eq, PartialEq)]
    pub(crate) struct VerifiedPackLaunchBinding {
        verified_pack: VerifiedPack,
        stable_device_identity: String,
    }

    impl VerifiedPackLaunchBinding {
        pub(crate) fn try_from_resolver_hello_bridge(
            bridge: &impl ResolverHelloBindingBridge,
        ) -> Option<Self> {
            let pack = bridge.resolver_verified_pack();
            let stable_device_identity = bridge.hello_stable_device_identity();
            let stable_device_is_canonical = !stable_device_identity.is_empty()
                && stable_device_identity.len() <= 256
                && stable_device_identity
                    .bytes()
                    .all(|byte| (0x20..=0x7e).contains(&byte))
                && stable_device_identity == stable_device_identity.to_ascii_lowercase();
            (stable_device_is_canonical
                && bridge.hello_pack_id() == pack.pack_id.as_str()
                && bridge.hello_pack_version() == pack.pack_version.as_str()
                && bridge.hello_pack_digest() == pack.pack_digest
                && bridge.hello_runtime_abi() == pack.runtime_abi_version
                && bridge.hello_backend() == pack.backend
                && bridge.hello_provider() == pack.provider)
                .then(|| Self {
                    verified_pack: pack.clone(),
                    stable_device_identity: stable_device_identity.to_owned(),
                })
        }

        pub(crate) fn verified_pack(&self) -> &VerifiedPack {
            &self.verified_pack
        }

        pub(crate) fn stable_device_identity(&self) -> &str {
            &self.stable_device_identity
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
    use std::path::PathBuf;

    use super::manifest::PackBackend;
    use super::{ResolverHelloBindingBridge, VerifiedPack, VerifiedPackLaunchBinding};

    struct FixtureBridge {
        pack: VerifiedPack,
        hello_digest: String,
        stable_device_identity: String,
    }

    impl ResolverHelloBindingBridge for FixtureBridge {
        fn resolver_verified_pack(&self) -> &VerifiedPack {
            &self.pack
        }

        fn hello_pack_id(&self) -> &str {
            self.pack.pack_id.as_str()
        }

        fn hello_pack_version(&self) -> &str {
            self.pack.pack_version.as_str()
        }

        fn hello_pack_digest(&self) -> &str {
            &self.hello_digest
        }

        fn hello_runtime_abi(&self) -> u16 {
            self.pack.runtime_abi_version
        }

        fn hello_backend(&self) -> PackBackend {
            self.pack.backend
        }

        fn hello_provider(&self) -> &str {
            &self.pack.provider
        }

        fn hello_stable_device_identity(&self) -> &str {
            &self.stable_device_identity
        }
    }

    fn fixture_bridge() -> FixtureBridge {
        let digest = "a".repeat(64);
        FixtureBridge {
            pack: VerifiedPack {
                pack_id: super::manifest::StoreComponent::new("fixture-pack").unwrap(),
                pack_version: super::manifest::StoreComponent::new("1.0.0").unwrap(),
                pack_digest: digest.clone(),
                security_epoch: 1,
                runtime_abi_version: 1,
                backend: PackBackend::Vulkan,
                provider: "fixture:vulkan".to_owned(),
                target_os: "windows".to_owned(),
                target_arch: "x86_64".to_owned(),
                worker_relative_path: "worker.exe".to_owned(),
                root: PathBuf::from("fixture-root"),
            },
            hello_digest: digest,
            stable_device_identity: "pci:0000:01:00.0".to_owned(),
        }
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
        let bridge = fixture_bridge();
        let binding = VerifiedPackLaunchBinding::try_from_resolver_hello_bridge(&bridge)
            .expect("matching resolver and Hello metadata bind");
        assert_eq!(binding.verified_pack(), &bridge.pack);
        assert_eq!(binding.stable_device_identity(), "pci:0000:01:00.0");

        let mut mismatched = fixture_bridge();
        mismatched.hello_digest = "b".repeat(64);
        assert!(VerifiedPackLaunchBinding::try_from_resolver_hello_bridge(&mismatched).is_none());
    }
}
