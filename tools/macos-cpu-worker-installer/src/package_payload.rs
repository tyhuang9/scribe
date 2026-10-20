//! Pure compiled-image admission; no path or execution authority.
use crate::macos_worker_layout::{validate_cpu_digest, validate_cpu_size};
use anyhow::{Result, bail};
use sha2::{Digest, Sha256};

pub(crate) fn verify(bytes: &[u8], expected: &str) -> Result<()> {
    validate_cpu_digest(expected)?;
    validate_cpu_size(u64::try_from(bytes.len())?)?;
    if format!("{:x}", Sha256::digest(bytes)) != expected {
        bail!("embedded CPU image differs from the compiled final-image anchor");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn package_payload_admits_only_exact_frozen_bytes() {
        let digest = format!("{:x}", Sha256::digest(b"signed fixture CPU image"));
        verify(b"signed fixture CPU image", &digest).unwrap();
        assert!(verify(b"forged fixture CPU image", &digest).is_err());
        assert!(verify(b"signed fixture CPU imag", &digest).is_err());
    }
    #[test]
    fn package_payload_rejects_empty_or_unanchored_bytes() {
        let digest = format!("{:x}", Sha256::digest(b""));
        assert!(verify(b"", &digest).is_err());
        for digest in ["", "invalid", &"AB".repeat(32)] {
            assert!(verify(b"worker", digest).is_err());
        }
    }
}
