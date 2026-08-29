//! Verified GPU worker-pack infrastructure.
//!
//! Stage 3 deliberately has no production trust root or registry entries. The
//! types in this module are private infrastructure for accepting externally
//! signed packs later without making a GPU provider discoverable today.

pub(crate) mod health;
pub(crate) mod manifest;
pub(crate) mod store;

pub(crate) use manifest::VerifiedPack;

/// Production discovery remains fail closed until a persistent signing key and
/// declared pack catalog are provisioned by a later release stage.
pub(crate) fn production_registry() -> Vec<VerifiedPack> {
    Vec::new()
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
}
