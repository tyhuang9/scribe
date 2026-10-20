//! Portable transaction ordering and bounded byte operations. No filesystem authority.
#![cfg_attr(not(target_os = "macos"), allow(dead_code))]
use std::io::{Read, Write};

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};

use crate::macos_worker_layout::{validate_cpu_digest, validate_cpu_size};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Phase {
    Prepare,
    Existing,
    Stage,
    Populate,
    Seal,
    Publish,
    Cleanup,
    Confirm,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Publication {
    NotPublished,
    PossiblyPublished,
    Published,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Outcome {
    PublishedNew,
    ReusedExisting,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PublishResult {
    New,
    Collision,
}

#[derive(Debug)]
pub(crate) struct InstallError {
    pub(crate) phase: Phase,
    pub(crate) publication: Publication,
    cause: anyhow::Error,
    cleanup: Option<anyhow::Error>,
}
impl std::fmt::Display for InstallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "CPU installer {:?} failed ({:?}): {:#}",
            self.phase, self.publication, self.cause
        )?;
        if let Some(error) = &self.cleanup {
            write!(f, "; staging retained or cleanup failed: {error:#}")?;
        }
        Ok(())
    }
}
impl std::error::Error for InstallError {}

pub(crate) trait Operations {
    fn prepare(&mut self) -> Result<()>;
    /// Existing means independently verified, never merely present.
    fn existing(&mut self) -> Result<bool>;
    fn stage(&mut self) -> Result<()>;
    fn populate(&mut self) -> Result<()>;
    fn seal(&mut self) -> Result<()>;
    fn publish(&mut self) -> Result<PublishResult>;
    /// Remove only proven invocation-owned staging entries, never final entries.
    fn cleanup(&mut self) -> Result<()>;
    fn confirm(&mut self) -> Result<()>;
}

pub(crate) fn install(ops: &mut impl Operations) -> std::result::Result<Outcome, InstallError> {
    let mut phase = Phase::Prepare;
    let mut publication = Publication::NotPublished;
    let mut staged = false;
    let result = (|| -> Result<Outcome> {
        ops.prepare()?;
        phase = Phase::Existing;
        if ops.existing()? {
            return Ok(Outcome::ReusedExisting);
        }
        phase = Phase::Stage;
        staged = true; // A partially created stage also requires bounded cleanup.
        ops.stage()?;
        phase = Phase::Populate;
        ops.populate()?;
        phase = Phase::Seal;
        ops.seal()?;
        phase = Phase::Publish;
        publication = Publication::PossiblyPublished; // Set before the native rename.
        match ops.publish()? {
            PublishResult::New => {
                publication = Publication::Published;
                Ok(Outcome::PublishedNew)
            }
            PublishResult::Collision => {
                publication = Publication::NotPublished;
                if !ops.existing()? {
                    bail!("destination disappeared after no-replace collision");
                }
                Ok(Outcome::ReusedExisting)
            }
        }
    })();
    let cleanup = if staged && publication != Publication::Published {
        ops.cleanup().err()
    } else {
        None
    };
    let outcome = match result {
        Err(cause) => {
            return Err(InstallError {
                phase,
                publication,
                cause,
                cleanup,
            });
        }
        Ok(_) if cleanup.is_some() => {
            return Err(InstallError {
                phase: Phase::Cleanup,
                publication,
                cause: cleanup.expect("checked cleanup error"),
                cleanup: None,
            });
        }
        Ok(outcome) => outcome,
    };
    ops.confirm().map_err(|cause| InstallError {
        phase: Phase::Confirm,
        publication,
        cause,
        cleanup: None,
    })?;
    Ok(outcome)
}

pub(crate) fn copy_exact(
    reader: &mut impl Read,
    writer: &mut impl Write,
    length: u64,
) -> Result<()> {
    validate_cpu_size(length)?;
    let mut remaining = length;
    let mut bytes = [0u8; 64 * 1024];
    while remaining > 0 {
        let limit = usize::try_from(remaining.min(bytes.len() as u64))?;
        let read = read_retry(reader, &mut bytes[..limit])?;
        if read == 0 {
            bail!("candidate was truncated while copying");
        }
        writer
            .write_all(&bytes[..read])
            .context("could not write fresh worker image")?;
        remaining -= read as u64;
    }
    if read_retry(reader, &mut bytes[..1])? != 0 {
        bail!("candidate grew while copying");
    }
    Ok(())
}

pub(crate) fn verify_bytes(reader: &mut impl Read, length: u64, expected: &str) -> Result<()> {
    validate_cpu_size(length)?;
    validate_cpu_digest(expected)?;
    let mut remaining = length;
    let mut hash = Sha256::new();
    let mut bytes = [0u8; 64 * 1024];
    while remaining > 0 {
        let limit = usize::try_from(remaining.min(bytes.len() as u64))?;
        let read = read_retry(reader, &mut bytes[..limit])?;
        if read == 0 {
            bail!("copied worker image was truncated");
        }
        hash.update(&bytes[..read]);
        remaining -= read as u64;
    }
    if read_retry(reader, &mut bytes[..1])? != 0 {
        bail!("copied worker image grew");
    }
    if format!("{:x}", hash.finalize()) != expected {
        bail!("copied worker digest differs from compiled authority");
    }
    Ok(())
}

fn read_retry(reader: &mut impl Read, bytes: &mut [u8]) -> std::io::Result<usize> {
    loop {
        match reader.read(bytes) {
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            result => return result,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{self, Cursor};

    #[derive(Default)]
    struct Fake {
        calls: Vec<&'static str>,
        fail: Option<&'static str>,
        exists: bool,
        final_exists: bool,
        stage_exists: bool,
        collision: bool,
        ambiguous: bool,
        cleanup_fails: bool,
    }
    impl Fake {
        fn call(&mut self, name: &'static str) -> Result<()> {
            self.calls.push(name);
            if self.fail == Some(name) {
                bail!("injected {name}");
            }
            Ok(())
        }
    }
    impl Operations for Fake {
        fn prepare(&mut self) -> Result<()> {
            self.call("prepare")
        }
        fn existing(&mut self) -> Result<bool> {
            self.call("existing")?;
            Ok(self.exists || self.final_exists)
        }
        fn stage(&mut self) -> Result<()> {
            self.stage_exists = true;
            self.call("stage")
        }
        fn populate(&mut self) -> Result<()> {
            self.call("populate")
        }
        fn seal(&mut self) -> Result<()> {
            self.call("seal")
        }
        fn publish(&mut self) -> Result<PublishResult> {
            self.call("publish")?;
            if self.collision {
                self.final_exists = true;
                return Ok(PublishResult::Collision);
            }
            self.final_exists = true;
            self.stage_exists = false;
            if self.ambiguous {
                bail!("rename applied then reported error");
            }
            Ok(PublishResult::New)
        }
        fn cleanup(&mut self) -> Result<()> {
            self.call("cleanup")?;
            if self.cleanup_fails || !self.stage_exists {
                bail!("cannot prove old staging name identity");
            }
            self.stage_exists = false;
            Ok(())
        }
        fn confirm(&mut self) -> Result<()> {
            self.call("confirm")
        }
    }
    #[test]
    fn installer_transaction_orders_copy_seal_publish_and_confirm() {
        let mut fake = Fake::default();
        assert_eq!(install(&mut fake).unwrap(), Outcome::PublishedNew);
        assert_eq!(
            fake.calls,
            [
                "prepare", "existing", "stage", "populate", "seal", "publish", "confirm"
            ]
        );
    }
    #[test]
    fn installer_transaction_reuse_never_stages_or_writes() {
        let mut fake = Fake {
            exists: true,
            ..Fake::default()
        };
        assert_eq!(install(&mut fake).unwrap(), Outcome::ReusedExisting);
        assert_eq!(fake.calls, ["prepare", "existing", "confirm"]);
    }
    #[test]
    fn installer_transaction_failure_at_every_boundary_preserves_publication_state() {
        for (name, phase) in [
            ("prepare", Phase::Prepare),
            ("existing", Phase::Existing),
            ("stage", Phase::Stage),
            ("populate", Phase::Populate),
            ("seal", Phase::Seal),
            ("publish", Phase::Publish),
            ("confirm", Phase::Confirm),
        ] {
            let mut fake = Fake {
                fail: Some(name),
                ..Fake::default()
            };
            let error = install(&mut fake).unwrap_err();
            assert_eq!(error.phase, phase, "{name}");
            assert_eq!(
                error.publication,
                match phase {
                    Phase::Publish => Publication::PossiblyPublished,
                    Phase::Confirm => Publication::Published,
                    _ => Publication::NotPublished,
                }
            );
            assert_eq!(
                fake.calls.contains(&"cleanup"),
                matches!(
                    phase,
                    Phase::Stage | Phase::Populate | Phase::Seal | Phase::Publish
                )
            );
            assert!(!fake.stage_exists);
        }
    }
    #[test]
    fn installer_transaction_collision_verifies_existing_and_removes_only_stage() {
        let mut fake = Fake {
            collision: true,
            ..Fake::default()
        };
        assert_eq!(install(&mut fake).unwrap(), Outcome::ReusedExisting);
        assert_eq!(
            fake.calls,
            [
                "prepare", "existing", "stage", "populate", "seal", "publish", "existing",
                "cleanup", "confirm"
            ]
        );
        assert!(fake.final_exists && !fake.stage_exists);
    }
    #[test]
    fn installer_transaction_ambiguous_rename_retains_final_and_retry_reuses_it() {
        let mut fake = Fake {
            ambiguous: true,
            ..Fake::default()
        };
        let error = install(&mut fake).unwrap_err();
        assert_eq!(error.publication, Publication::PossiblyPublished);
        assert!(error.cleanup.is_some() && fake.final_exists);
        fake.calls.clear();
        assert_eq!(install(&mut fake).unwrap(), Outcome::ReusedExisting);
        assert_eq!(fake.calls, ["prepare", "existing", "confirm"]);
    }
    #[test]
    fn installer_transaction_preserves_primary_and_cleanup_errors() {
        for (name, phase) in [("stage", Phase::Stage), ("populate", Phase::Populate)] {
            let mut fake = Fake {
                fail: Some(name),
                cleanup_fails: true,
                ..Fake::default()
            };
            let error = install(&mut fake).unwrap_err();
            assert_eq!(error.phase, phase);
            assert_eq!(error.publication, Publication::NotPublished);
            assert!(error.cleanup.is_some() && fake.stage_exists && !fake.final_exists);
            assert!(error.to_string().contains(&format!("injected {name}")));
            assert!(!fake.calls.contains(&"confirm"));
        }
    }
    #[test]
    fn installer_transaction_cleanup_failure_does_not_confirm_collision() {
        let mut fake = Fake {
            collision: true,
            cleanup_fails: true,
            ..Fake::default()
        };
        assert_eq!(install(&mut fake).unwrap_err().phase, Phase::Cleanup);
        assert!(!fake.calls.contains(&"confirm") && fake.final_exists);
    }
    struct ShortWriter {
        bytes: Vec<u8>,
        fail: bool,
    }
    impl Write for ShortWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if self.fail {
                return Err(io::Error::other("ENOSPC"));
            }
            let size = bytes.len().min(2);
            self.bytes.extend_from_slice(&bytes[..size]);
            Ok(size)
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    #[test]
    fn installer_bytes_handle_partial_writes_and_reject_write_failure() {
        let mut writer = ShortWriter {
            bytes: vec![],
            fail: false,
        };
        copy_exact(&mut Cursor::new(b"worker"), &mut writer, 6).unwrap();
        assert_eq!(writer.bytes, b"worker");
        writer.fail = true;
        assert!(copy_exact(&mut Cursor::new(b"worker"), &mut writer, 6).is_err());
    }
    #[test]
    fn installer_bytes_reject_short_growing_empty_and_oversized_input() {
        for length in [
            0,
            5,
            7,
            crate::macos_worker_layout::MAX_CPU_WORKER_BYTES + 1,
        ] {
            assert!(copy_exact(&mut Cursor::new(b"worker"), &mut Vec::new(), length).is_err());
        }
    }
    #[test]
    fn installer_bytes_independent_readback_rejects_tampering_and_length_changes() {
        let digest = format!("{:x}", Sha256::digest(b"worker"));
        verify_bytes(&mut Cursor::new(b"worker"), 6, &digest).unwrap();
        for (bytes, length) in [
            (b"tamper".as_slice(), 6),
            (b"worker", 5),
            (b"worker", 7),
            (b"worker", 0),
        ] {
            assert!(verify_bytes(&mut Cursor::new(bytes), length, &digest).is_err());
        }
        assert!(verify_bytes(&mut Cursor::new(b"worker"), 6, "invalid").is_err());
    }
    #[test]
    fn installer_bytes_retries_interrupted_reads() {
        struct Interrupt {
            first: bool,
            data: Cursor<&'static [u8]>,
        }
        impl Read for Interrupt {
            fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
                if !self.first {
                    self.first = true;
                    return Err(io::ErrorKind::Interrupted.into());
                }
                self.data.read(bytes)
            }
        }
        let mut reader = Interrupt {
            first: false,
            data: Cursor::new(b"worker"),
        };
        let mut bytes = vec![];
        copy_exact(&mut reader, &mut bytes, 6).unwrap();
        assert_eq!(bytes, b"worker");
    }
}
