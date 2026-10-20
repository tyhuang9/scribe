//! CPU-specific admission bounds around shared installer mechanics.
#![cfg_attr(not(target_os = "macos"), allow(dead_code))]
use anyhow::Result;
use std::io::{Read, Write};

#[cfg(target_os = "macos")]
pub(crate) use crate::installer_transaction::PublishResult;
pub(crate) use crate::installer_transaction::{InstallError, Operations, Outcome};
use crate::macos_worker_layout::{MAX_CPU_WORKER_BYTES, validate_cpu_digest, validate_cpu_size};

pub(crate) fn install(ops: &mut impl Operations) -> std::result::Result<Outcome, InstallError> {
    crate::installer_transaction::run_transaction(ops, "CPU")
}

pub(crate) fn copy_exact(
    reader: &mut impl Read,
    writer: &mut impl Write,
    length: u64,
) -> Result<()> {
    validate_cpu_size(length)?;
    crate::installer_transaction::copy_exact(reader, writer, length, MAX_CPU_WORKER_BYTES)
}

pub(crate) fn verify_bytes(reader: &mut impl Read, length: u64, expected: &str) -> Result<()> {
    validate_cpu_size(length)?;
    validate_cpu_digest(expected)?;
    crate::installer_transaction::verify_bytes(reader, length, expected, MAX_CPU_WORKER_BYTES)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};
    use std::io::Cursor;

    #[test]
    fn cpu_copy_preserves_the_cpu_specific_size_limit() {
        for length in [0, MAX_CPU_WORKER_BYTES + 1] {
            assert!(copy_exact(&mut Cursor::new(b"worker"), &mut Vec::new(), length).is_err());
        }
        let mut copied = Vec::new();
        copy_exact(&mut Cursor::new(b"worker"), &mut copied, 6).unwrap();
        assert_eq!(copied, b"worker");
    }

    #[test]
    fn cpu_readback_preserves_compiled_digest_and_size_validation() {
        let digest = format!("{:x}", Sha256::digest(b"worker"));
        verify_bytes(&mut Cursor::new(b"worker"), 6, &digest).unwrap();
        for (length, expected) in [
            (0, digest.as_str()),
            (MAX_CPU_WORKER_BYTES + 1, &digest),
            (6, "invalid"),
        ] {
            assert!(verify_bytes(&mut Cursor::new(b"worker"), length, expected).is_err());
        }
    }
}
