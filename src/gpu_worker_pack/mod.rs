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
