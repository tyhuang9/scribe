//! Integrity-checked materialization of assets embedded in the Scribe binary.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};

pub(crate) const SILERO_VAD_FILE_NAME: &str = "silero_vad.int8.onnx";
pub(crate) const SILERO_VAD_SIZE: u64 = 212_860;
pub(crate) const SILERO_VAD_SHA256: &str =
    "c36d490aff5ab924ca6c7aeec4d8f6bd3d22db6fa17611b9c5b17eae58ac3a20";
const SILERO_VAD_BYTES: &[u8] = include_bytes!("../resources/silero-vad/silero_vad.int8.onnx");
const SUPPORT_ASSET_RELATIVE_DIR: &str = "support-assets/silero-vad";
static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub(crate) fn materialize_bundled_support_assets() -> Result<PathBuf> {
    let data_root = crate::config::project_dirs()?
        .data_local_dir()
        .to_path_buf();
    materialize_silero_vad_in(&data_root, SILERO_VAD_BYTES)
}

fn materialize_silero_vad_in(data_root: &Path, embedded: &[u8]) -> Result<PathBuf> {
    verify_bytes(embedded).context("bundled Silero VAD bytes failed integrity verification")?;
    let support_dir = ensure_private_support_dir(data_root)?;
    let target = support_dir.join(SILERO_VAD_FILE_NAME);
    match verify_regular_file(&target) {
        Ok(()) => return Ok(target),
        Err(error) if is_unsafe_entry(&target)? => return Err(error),
        Err(_) => {}
    }

    let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let temporary = support_dir.join(format!(
        ".{SILERO_VAD_FILE_NAME}.{}.{}.tmp",
        std::process::id(),
        sequence
    ));
    let result = (|| -> Result<()> {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(&temporary)
            .with_context(|| format!("could not create {}", temporary.display()))?;
        secure_file(&temporary)?;
        file.write_all(embedded)?;
        file.flush()?;
        file.sync_all()?;
        drop(file);
        verify_regular_file(&temporary)?;
        replace_file(&temporary, &target)
            .with_context(|| format!("could not activate {}", target.display()))?;
        sync_directory(&support_dir)?;
        verify_regular_file(&target)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result?;
    Ok(target)
}

fn ensure_private_support_dir(data_root: &Path) -> Result<PathBuf> {
    if data_root.as_os_str().is_empty() {
        bail!("support asset data root is empty");
    }
    fs::create_dir_all(data_root).with_context(|| {
        format!(
            "could not create support asset root {}",
            data_root.display()
        )
    })?;
    reject_unsafe_directory(data_root)?;
    secure_directory(data_root)?;
    let canonical_root = fs::canonicalize(data_root)?;
    let mut current = data_root.to_path_buf();
    for component in Path::new(SUPPORT_ASSET_RELATIVE_DIR).components() {
        let Component::Normal(component) = component else {
            bail!("support asset path contains an unsafe component");
        };
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(_) => reject_unsafe_directory(&current)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                fs::create_dir(&current)
                    .with_context(|| format!("could not create {}", current.display()))?;
                reject_unsafe_directory(&current)?;
            }
            Err(error) => return Err(error.into()),
        }
        secure_directory(&current)?;
        let canonical = fs::canonicalize(&current)?;
        if !canonical.starts_with(&canonical_root) {
            bail!(
                "support asset directory escaped its private root: {}",
                current.display()
            );
        }
    }
    Ok(current)
}

fn reject_unsafe_directory(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir() || metadata_is_link_or_reparse(&metadata) {
        bail!(
            "support asset path is not a safe directory: {}",
            path.display()
        );
    }
    Ok(())
}

fn is_unsafe_entry(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => Ok(!metadata.is_file() || metadata_is_link_or_reparse(&metadata)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn verify_regular_file(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("support asset is unavailable: {}", path.display()))?;
    if !metadata.is_file() || metadata_is_link_or_reparse(&metadata) {
        bail!(
            "support asset is not a safe regular file: {}",
            path.display()
        );
    }
    if metadata.len() != SILERO_VAD_SIZE {
        bail!(
            "support asset size mismatch for {}: expected {SILERO_VAD_SIZE}, got {}",
            path.display(),
            metadata.len()
        );
    }
    let mut file = open_read_no_follow(path)?;
    let mut hasher = Sha256::new();
    let copied = io::copy(&mut file, &mut hasher)?;
    if copied != SILERO_VAD_SIZE {
        bail!("support asset changed while reading: {}", path.display());
    }
    let actual = format!("{:x}", hasher.finalize());
    if actual != SILERO_VAD_SHA256 {
        bail!(
            "support asset checksum mismatch for {}: expected {SILERO_VAD_SHA256}, got {actual}",
            path.display()
        );
    }
    Ok(())
}

fn verify_bytes(bytes: &[u8]) -> Result<()> {
    if u64::try_from(bytes.len()).ok() != Some(SILERO_VAD_SIZE) {
        bail!(
            "expected {SILERO_VAD_SIZE} embedded Silero VAD bytes, got {}",
            bytes.len()
        );
    }
    let actual = format!("{:x}", Sha256::digest(bytes));
    if actual != SILERO_VAD_SHA256 {
        bail!("expected embedded Silero VAD SHA-256 {SILERO_VAD_SHA256}, got {actual}");
    }
    Ok(())
}

fn open_read_no_follow(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT);
    }
    options.open(path)
}

#[cfg(windows)]
fn metadata_is_link_or_reparse(metadata: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    metadata.file_type().is_symlink()
        || metadata.file_attributes()
            & windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT
            != 0
}

#[cfg(not(windows))]
fn metadata_is_link_or_reparse(metadata: &fs::Metadata) -> bool {
    metadata.file_type().is_symlink()
}

#[cfg(unix)]
fn secure_directory(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

#[cfg(unix)]
fn secure_file(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(not(unix))]
fn secure_directory(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(not(unix))]
fn secure_file(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(not(windows))]
fn replace_file(source: &Path, destination: &Path) -> io::Result<()> {
    fs::rename(source, destination)
}

#[cfg(windows)]
fn replace_file(source: &Path, destination: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };
    let source = source
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let destination = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let moved = unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if moved == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> io::Result<()> {
    File::open(path)?.sync_all()
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    static TEST_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    fn test_root(name: &str) -> PathBuf {
        let sequence = TEST_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "scribe-support-assets-{name}-{}-{sequence}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        root
    }

    #[test]
    fn embedded_asset_matches_pinned_release_facts() {
        verify_bytes(SILERO_VAD_BYTES).unwrap();
    }

    #[test]
    fn missing_cache_is_materialized_and_verified() {
        let root = test_root("missing");
        let path = materialize_silero_vad_in(&root, SILERO_VAD_BYTES).unwrap();
        verify_regular_file(&path).unwrap();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn corrupt_cache_is_atomically_replaced() {
        let root = test_root("corrupt");
        let path = materialize_silero_vad_in(&root, SILERO_VAD_BYTES).unwrap();
        fs::write(&path, b"corrupt").unwrap();
        let repaired = materialize_silero_vad_in(&root, SILERO_VAD_BYTES).unwrap();
        assert_eq!(repaired, path);
        verify_regular_file(&repaired).unwrap();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn interrupted_temporary_file_is_never_accepted_as_the_asset() {
        let root = test_root("interrupted");
        let support_dir = ensure_private_support_dir(&root).unwrap();
        let interrupted = support_dir.join(format!(".{SILERO_VAD_FILE_NAME}.stale.tmp"));
        fs::write(&interrupted, b"partial").unwrap();
        let path = materialize_silero_vad_in(&root, SILERO_VAD_BYTES).unwrap();
        assert_ne!(path, interrupted);
        verify_regular_file(&path).unwrap();
        assert_eq!(fs::read(interrupted).unwrap(), b"partial");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn non_regular_cache_entry_is_rejected_without_replacement() {
        let root = test_root("directory-target");
        let support_dir = ensure_private_support_dir(&root).unwrap();
        let target = support_dir.join(SILERO_VAD_FILE_NAME);
        fs::create_dir(&target).unwrap();
        let error = materialize_silero_vad_in(&root, SILERO_VAD_BYTES).unwrap_err();
        assert!(error.to_string().contains("safe regular file"));
        assert!(target.is_dir());
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn linked_cache_entry_is_rejected_without_following_it() {
        use std::os::unix::fs::symlink;

        let root = test_root("linked-target");
        let external = test_root("linked-external");
        fs::create_dir_all(&external).unwrap();
        let external_file = external.join("outside.onnx");
        fs::write(&external_file, SILERO_VAD_BYTES).unwrap();
        let support_dir = ensure_private_support_dir(&root).unwrap();
        let target = support_dir.join(SILERO_VAD_FILE_NAME);
        symlink(&external_file, &target).unwrap();
        assert!(materialize_silero_vad_in(&root, SILERO_VAD_BYTES).is_err());
        assert_eq!(fs::read(&external_file).unwrap(), SILERO_VAD_BYTES);
        let _ = fs::remove_dir_all(root);
        let _ = fs::remove_dir_all(external);
    }

    #[cfg(windows)]
    #[test]
    fn reparse_cache_entry_is_rejected_without_following_it() {
        use std::os::windows::fs::symlink_file;

        let root = test_root("linked-target");
        let external = test_root("linked-external");
        fs::create_dir_all(&external).unwrap();
        let external_file = external.join("outside.onnx");
        fs::write(&external_file, SILERO_VAD_BYTES).unwrap();
        let support_dir = ensure_private_support_dir(&root).unwrap();
        let target = support_dir.join(SILERO_VAD_FILE_NAME);
        if symlink_file(&external_file, &target).is_err() {
            let _ = fs::remove_dir_all(root);
            let _ = fs::remove_dir_all(external);
            return;
        }
        assert!(materialize_silero_vad_in(&root, SILERO_VAD_BYTES).is_err());
        assert_eq!(fs::read(&external_file).unwrap(), SILERO_VAD_BYTES);
        let _ = fs::remove_dir_all(root);
        let _ = fs::remove_dir_all(external);
    }
}
