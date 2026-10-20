//! Shared fixed layout for the installer-owned CPU worker.
//!
//! The application and installer compile the same final signed worker digest.
//! Runtime arguments, environment variables and catalog entries never select it.

use std::path::PathBuf;

use anyhow::{Result, bail};

pub(crate) const CPU_ROOT: &str = "/Library/Application Support/Scribe/CPU";
pub(crate) const CPU_WORKER_NAME: &str = "scribe-inference-worker";
pub(crate) const MAX_CPU_WORKER_BYTES: u64 = 512 * 1024 * 1024;

#[cfg(target_os = "macos")]
pub(crate) fn embedded_cpu_digest() -> Result<&'static str> {
    let digest = option_env!("SCRIBE_BUNDLED_WORKER_SHA256").unwrap_or("");
    validate_cpu_digest(digest)?;
    Ok(digest)
}

#[cfg(target_os = "macos")]
pub(crate) fn embedded_cpu_root() -> Result<PathBuf> {
    cpu_root(embedded_cpu_digest()?)
}

fn cpu_root(digest: &str) -> Result<PathBuf> {
    validate_cpu_digest(digest)?;
    Ok(PathBuf::from(CPU_ROOT).join(digest))
}

pub(crate) fn validate_cpu_digest(digest: &str) -> Result<()> {
    if digest.len() != 64
        || !digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        bail!("protected CPU worker requires the compiled final-image SHA-256 anchor");
    }
    Ok(())
}

pub(crate) fn validate_cpu_size(length: u64) -> Result<()> {
    if length == 0 || length > MAX_CPU_WORKER_BYTES {
        bail!("protected CPU worker image is empty or exceeds its size bound");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn macos_protected_cpu_layout_uses_only_canonical_digest_component() {
        let digest = "ab".repeat(32);
        assert_eq!(
            cpu_root(&digest).unwrap(),
            PathBuf::from(CPU_ROOT).join(&digest)
        );
        for invalid in [
            "",
            ".",
            "..",
            "../worker",
            "AB",
            &"AB".repeat(32),
            &"a".repeat(63),
            &"a".repeat(65),
            &format!("{}:", "a".repeat(63)),
            &format!("{}g", "a".repeat(63)),
        ] {
            assert!(
                cpu_root(invalid).is_err(),
                "accepted invalid digest component"
            );
        }
        assert_eq!(CPU_WORKER_NAME, "scribe-inference-worker");
    }

    #[test]
    fn macos_protected_cpu_layout_bounds_image_before_hashing_or_copying() {
        assert!(validate_cpu_size(0).is_err());
        validate_cpu_size(1).unwrap();
        validate_cpu_size(MAX_CPU_WORKER_BYTES).unwrap();
        assert!(validate_cpu_size(MAX_CPU_WORKER_BYTES + 1).is_err());
        assert!(validate_cpu_size(u64::MAX).is_err());
    }
}
