//! Build-only snapshot, also exercised directly by private source tests.
use crate::macos_worker_layout::{MAX_CPU_WORKER_BYTES, validate_cpu_digest, validate_cpu_size};
use anyhow::{Context, Result, bail};
use std::fs::{self, File, Metadata, OpenOptions};
use std::io::{Read, Write};
use std::path::Path;

fn check_file(metadata: &Metadata) -> Result<u64> {
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        bail!("package worker must be a regular non-symlink build input");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.nlink() != 1 {
            bail!("package worker build input is hard-linked");
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            bail!("reparse-point build input");
        }
    }
    validate_cpu_size(metadata.len())?;
    Ok(metadata.len())
}

fn open_regular(path: &Path) -> Result<(File, u64)> {
    let before = fs::symlink_metadata(path)?;
    let length = check_file(&before)?;
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(0x0020_0000); // FILE_FLAG_OPEN_REPARSE_POINT
    }
    let file = options.open(path)?;
    if check_file(&file.metadata()?)? != length {
        bail!("build input changed while opening");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let opened = file.metadata()?;
        if (opened.dev(), opened.ino()) != (before.dev(), before.ino()) {
            bail!("build input identity changed while opening");
        }
    }
    Ok((file, length))
}

pub(crate) fn validate_source_path(source: &Path) -> Result<()> {
    let name = source
        .to_str()
        .context("package build input must be Unicode")?;
    if !source.is_absolute() || name.len() > 32767 || name.bytes().any(|b| b.is_ascii_control()) {
        bail!("package worker build input must be an absolute directive-safe path");
    }
    Ok(())
}

pub(crate) fn snapshot(source: &Path, destination: &Path, expected: &str) -> Result<()> {
    validate_cpu_digest(expected)?; // Reject missing authority before path access.
    validate_source_path(source)?;
    let (mut input, length) = open_regular(source)?;
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)?;
    let copied = std::io::copy(
        &mut std::io::Read::by_ref(&mut input).take(length),
        &mut output,
    )?;
    let mut extra = [0];
    if copied != length || input.read(&mut extra)? != 0 {
        bail!("build input changed while copying");
    }
    output.flush()?;
    output.sync_all()?;
    drop(output);
    // Independently reopen the snapshot, not the mutable source. Allocation is
    // bounded by the CPU image limit before reading; this is build-time only.
    let (mut copied, copied_length) = open_regular(destination)?;
    if copied_length != length {
        bail!("copied build input changed length");
    }
    let mut bytes = Vec::new();
    std::io::Read::by_ref(&mut copied)
        .take(MAX_CPU_WORKER_BYTES + 1)
        .read_to_end(&mut bytes)?;
    crate::package_payload::verify(&bytes, expected).context("copied package image is unadmitted")
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    struct Fixture {
        root: std::path::PathBuf,
    }
    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
                "scribe-pkg-build-test-{}-{}",
                std::process::id(),
                SEQUENCE.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&root).unwrap();
            fs::write(root.join("source"), b"synthetic CPU package image\n").unwrap();
            Self { root }
        }
        fn source(&self) -> std::path::PathBuf {
            self.root.join("source")
        }
        fn output(&self) -> std::path::PathBuf {
            self.root.join("snapshot")
        }
        fn digest(&self) -> String {
            format!("{:x}", Sha256::digest(b"synthetic CPU package image\n"))
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.root).unwrap();
        }
    }
    #[test]
    fn package_snapshot_copies_and_independently_admits_the_frozen_image() {
        let f = Fixture::new();
        snapshot(&f.source(), &f.output(), &f.digest()).unwrap();
        assert_eq!(fs::read(f.source()).unwrap(), fs::read(f.output()).unwrap());
    }
    #[test]
    fn package_snapshot_rejects_wrong_digest_and_changed_source_bytes() {
        let f = Fixture::new();
        fs::write(f.source(), b"forged CPU package image\n").unwrap();
        assert!(snapshot(&f.source(), &f.output(), &f.digest()).is_err());
    }
    #[test]
    fn package_snapshot_never_replaces_an_existing_output() {
        let f = Fixture::new();
        fs::write(f.output(), b"preserved").unwrap();
        assert!(snapshot(&f.source(), &f.output(), &f.digest()).is_err());
        assert_eq!(fs::read(f.output()).unwrap(), b"preserved");
    }
    #[test]
    fn package_snapshot_rejects_missing_authority_before_source_access() {
        let f = Fixture::new();
        let error = snapshot(&f.root.join("absent"), &f.output(), "invalid").unwrap_err();
        assert!(error.to_string().contains("compiled final-image"));
        assert!(snapshot(Path::new("relative"), &f.output(), &f.digest()).is_err());
        assert!(validate_source_path(&f.root.join("injected\ncargo:rustc-env=bad")).is_err());
    }
    #[test]
    fn package_snapshot_bounds_and_regular_file_checks_fail_closed() {
        let f = Fixture::new();
        assert!(snapshot(&f.root, &f.output(), &f.digest()).is_err());
        fs::write(f.source(), []).unwrap();
        assert!(snapshot(&f.source(), &f.output(), &f.digest()).is_err());
        for size in [0, MAX_CPU_WORKER_BYTES + 1, u64::MAX] {
            assert!(validate_cpu_size(size).is_err());
        }
    }
    #[cfg(unix)]
    #[test]
    fn package_snapshot_native_alias_and_special_file_checks() {
        use std::os::unix::fs::symlink;
        let f = Fixture::new();
        let link = f.root.join("link");
        symlink(f.source(), &link).unwrap();
        assert!(snapshot(&link, &f.output(), &f.digest()).is_err());
        fs::hard_link(f.source(), f.root.join("alias")).unwrap();
        assert!(snapshot(&f.source(), &f.output(), &f.digest()).is_err());
        let fifo = f.root.join("fifo");
        use std::os::unix::ffi::OsStrExt;
        let path = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
        assert!(snapshot(&fifo, &f.output(), &f.digest()).is_err());
    }
}
