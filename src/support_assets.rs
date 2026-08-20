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

/// A path paired with the exact handle whose bytes were verified.
///
/// The handle prevents file write/delete/reparse replacement on Windows until
/// native construction returns. The native API still reopens the path, so a
/// same-UID/current-user actor replacing an ancestor directory during that
/// cross-path handoff is outside the threat model; this is not a universally
/// end-to-end atomic handoff.
#[derive(Debug)]
pub(crate) struct VerifiedSupportAsset {
    path: PathBuf,
    _guard: File,
}

impl VerifiedSupportAsset {
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

pub(crate) fn materialize_bundled_support_assets() -> Result<VerifiedSupportAsset> {
    let data_root = crate::config::project_dirs()?
        .data_local_dir()
        .to_path_buf();
    materialize_silero_vad_in(&data_root, SILERO_VAD_BYTES)
}

fn materialize_silero_vad_in(data_root: &Path, embedded: &[u8]) -> Result<VerifiedSupportAsset> {
    verify_bytes(embedded).context("bundled Silero VAD bytes failed integrity verification")?;
    let support_dir = ensure_private_support_dir(data_root)?;
    let target = support_dir.join(SILERO_VAD_FILE_NAME);
    match fs::symlink_metadata(&target) {
        Ok(_) => {
            if is_unsafe_entry(&target)? {
                return verify_regular_file(&target);
            }
            secure_file(&target)?;
            if let Ok(verified) = verify_regular_file(&target) {
                return Ok(verified);
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
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
        drop(verify_regular_file(&temporary)?);
        replace_file(&temporary, &target)
            .with_context(|| format!("could not activate {}", target.display()))?;
        sync_directory(&support_dir)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result?;
    verify_regular_file(&target)
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

fn verify_regular_file(path: &Path) -> Result<VerifiedSupportAsset> {
    let path_metadata = fs::symlink_metadata(path)
        .with_context(|| format!("support asset is unavailable: {}", path.display()))?;
    if !path_metadata.is_file() || metadata_is_link_or_reparse(&path_metadata) {
        bail!(
            "support asset is not a safe regular file: {}",
            path.display()
        );
    }
    let mut file = open_read_no_follow(path)
        .with_context(|| format!("support asset is unavailable: {}", path.display()))?;
    let metadata = file
        .metadata()
        .with_context(|| format!("could not inspect support asset: {}", path.display()))?;
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
    Ok(VerifiedSupportAsset {
        path: path.to_path_buf(),
        _guard: file,
    })
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
        options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ;
        options.share_mode(FILE_SHARE_READ);
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

#[cfg(windows)]
fn secure_directory(path: &Path) -> Result<()> {
    secure_windows_path(path)
}

#[cfg(windows)]
fn secure_file(path: &Path) -> Result<()> {
    secure_windows_path(path)
}

#[cfg(not(any(unix, windows)))]
fn secure_directory(_path: &Path) -> Result<()> {
    bail!("support asset permission hardening is unavailable on this platform")
}

#[cfg(not(any(unix, windows)))]
fn secure_file(_path: &Path) -> Result<()> {
    bail!("support asset permission hardening is unavailable on this platform")
}

#[cfg(windows)]
fn secure_windows_path(path: &Path) -> Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use std::ptr;
    use windows_sys::Win32::Foundation::{GetLastError, LocalFree};
    use windows_sys::Win32::Security::Authorization::{
        ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
    };
    use windows_sys::Win32::Security::{
        DACL_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR,
        SetFileSecurityW,
    };

    let path_wide = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let user_sid = current_process_user_sid()?;
    // Protected DACL: full control for this process user and LocalSystem only.
    let sddl = format!("D:P(A;OICI;FA;;;{user_sid})(A;OICI;FA;;;SY)")
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let mut descriptor: PSECURITY_DESCRIPTOR = ptr::null_mut();
    let converted = unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1,
            &mut descriptor,
            ptr::null_mut(),
        )
    };
    if converted == 0 {
        return Err(io::Error::from_raw_os_error(unsafe { GetLastError() } as i32).into());
    }
    let applied = unsafe {
        SetFileSecurityW(
            path_wide.as_ptr(),
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            descriptor,
        )
    };
    let apply_error = (applied == 0).then(|| unsafe { GetLastError() });
    unsafe {
        LocalFree(descriptor);
    }
    if let Some(code) = apply_error {
        return Err(io::Error::from_raw_os_error(code as i32).into());
    }
    Ok(())
}

#[cfg(windows)]
fn current_process_user_sid() -> Result<String> {
    use std::ffi::c_void;
    use std::ptr;
    use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, HANDLE, LocalFree};
    use windows_sys::Win32::Security::Authorization::ConvertSidToStringSidW;
    use windows_sys::Win32::Security::{GetTokenInformation, TOKEN_QUERY, TOKEN_USER, TokenUser};
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    let mut token: HANDLE = ptr::null_mut();
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(io::Error::from_raw_os_error(unsafe { GetLastError() } as i32).into());
    }
    let result = (|| -> Result<String> {
        let mut length = 0;
        let queried =
            unsafe { GetTokenInformation(token, TokenUser, ptr::null_mut(), 0, &mut length) };
        let query_error = unsafe { GetLastError() };
        const ERROR_INSUFFICIENT_BUFFER: u32 = 122;
        if length == 0 || (queried == 0 && query_error != ERROR_INSUFFICIENT_BUFFER) {
            return Err(io::Error::from_raw_os_error(query_error as i32).into());
        }
        let words = usize::try_from(length)
            .map_err(|_| io::Error::other("token user data is too large"))?
            .div_ceil(std::mem::size_of::<usize>());
        let mut buffer = vec![0usize; words];
        if unsafe {
            GetTokenInformation(
                token,
                TokenUser,
                buffer.as_mut_ptr().cast::<c_void>(),
                length,
                &mut length,
            )
        } == 0
        {
            return Err(io::Error::from_raw_os_error(unsafe { GetLastError() } as i32).into());
        }
        let user = unsafe { &*buffer.as_ptr().cast::<TOKEN_USER>() };
        let mut sid_ptr = ptr::null_mut();
        if unsafe { ConvertSidToStringSidW(user.User.Sid, &mut sid_ptr) } == 0 {
            return Err(io::Error::from_raw_os_error(unsafe { GetLastError() } as i32).into());
        }
        let sid_length = unsafe { (0..).find(|&index| *sid_ptr.add(index) == 0).unwrap() };
        let sid =
            String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(sid_ptr, sid_length) });
        unsafe {
            LocalFree(sid_ptr.cast());
        }
        Ok(sid)
    })();
    let close_error = (unsafe { CloseHandle(token) } == 0).then(|| unsafe { GetLastError() });
    match result {
        Ok(sid) => {
            if let Some(code) = close_error {
                return Err(io::Error::from_raw_os_error(code as i32).into());
            }
            Ok(sid)
        }
        Err(error) => Err(error),
    }
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

    #[cfg(windows)]
    fn security_descriptor_sddl(path: &Path) -> String {
        use std::os::windows::ffi::OsStrExt;
        use std::ptr;
        use windows_sys::Win32::Foundation::{GetLastError, LocalFree};
        use windows_sys::Win32::Security::Authorization::{
            ConvertSecurityDescriptorToStringSecurityDescriptorW, GetNamedSecurityInfoW,
            SDDL_REVISION_1, SE_FILE_OBJECT,
        };
        use windows_sys::Win32::Security::{DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID};

        let path_wide = path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect::<Vec<_>>();
        let mut descriptor: PSECURITY_DESCRIPTOR = ptr::null_mut();
        let mut dacl = ptr::null_mut();
        let status = unsafe {
            GetNamedSecurityInfoW(
                path_wide.as_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION,
                ptr::null_mut::<PSID>(),
                ptr::null_mut::<PSID>(),
                &mut dacl,
                ptr::null_mut(),
                &mut descriptor,
            )
        };
        assert_eq!(status, 0, "failed to read support asset DACL: {status}");

        let mut descriptor_sddl = ptr::null_mut();
        let mut descriptor_sddl_length = 0;
        let converted = unsafe {
            ConvertSecurityDescriptorToStringSecurityDescriptorW(
                descriptor,
                SDDL_REVISION_1,
                DACL_SECURITY_INFORMATION,
                &mut descriptor_sddl,
                &mut descriptor_sddl_length,
            )
        };
        if converted == 0 {
            let error = unsafe { GetLastError() };
            unsafe {
                LocalFree(descriptor);
            }
            panic!(
                "failed to stringify support asset DACL: {}",
                io::Error::from_raw_os_error(error as i32)
            );
        }
        let length = unsafe { (0..).find(|&index| *descriptor_sddl.add(index) == 0) }
            .unwrap_or(descriptor_sddl_length as usize);
        let sddl = String::from_utf16_lossy(unsafe {
            std::slice::from_raw_parts(descriptor_sddl, length)
        });
        unsafe {
            LocalFree(descriptor_sddl.cast());
            LocalFree(descriptor);
        }
        sddl
    }

    #[test]
    fn embedded_asset_matches_pinned_release_facts() {
        verify_bytes(SILERO_VAD_BYTES).unwrap();
    }

    #[test]
    fn missing_cache_is_materialized_and_verified() {
        let root = test_root("missing");
        let asset = materialize_silero_vad_in(&root, SILERO_VAD_BYTES).unwrap();
        let path = asset.path().to_path_buf();
        drop(verify_regular_file(&path).unwrap());
        drop(asset);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn corrupt_cache_is_atomically_replaced() {
        let root = test_root("corrupt");
        let asset = materialize_silero_vad_in(&root, SILERO_VAD_BYTES).unwrap();
        let path = asset.path().to_path_buf();
        drop(asset);
        fs::write(&path, b"corrupt").unwrap();
        let repaired = materialize_silero_vad_in(&root, SILERO_VAD_BYTES).unwrap();
        assert_eq!(repaired.path(), path);
        drop(verify_regular_file(repaired.path()).unwrap());
        drop(repaired);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn interrupted_temporary_file_is_never_accepted_as_the_asset() {
        let root = test_root("interrupted");
        let support_dir = ensure_private_support_dir(&root).unwrap();
        let interrupted = support_dir.join(format!(".{SILERO_VAD_FILE_NAME}.stale.tmp"));
        fs::write(&interrupted, b"partial").unwrap();
        let asset = materialize_silero_vad_in(&root, SILERO_VAD_BYTES).unwrap();
        assert_ne!(asset.path(), interrupted);
        drop(verify_regular_file(asset.path()).unwrap());
        assert_eq!(fs::read(interrupted).unwrap(), b"partial");
        drop(asset);
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

    #[cfg(windows)]
    #[test]
    fn materialized_asset_and_directories_have_protected_user_and_system_dacls() {
        let root = test_root("windows-dacl");
        let asset = materialize_silero_vad_in(&root, SILERO_VAD_BYTES).unwrap();
        let user_sid = current_process_user_sid().unwrap();
        let support_root = root.join("support-assets");
        let support_dir = support_root.join("silero-vad");
        for path in [
            root.as_path(),
            support_root.as_path(),
            support_dir.as_path(),
            asset.path(),
        ] {
            let sddl = security_descriptor_sddl(path);
            assert!(sddl.starts_with("D:P"), "DACL is not protected: {sddl}");
            assert!(
                sddl.contains(&format!(";;;{user_sid})")),
                "DACL omitted current user SID {user_sid}: {sddl}"
            );
            assert!(sddl.contains(";;;SY)"), "DACL omitted LocalSystem: {sddl}");
            assert_eq!(
                sddl.matches('(').count(),
                2,
                "DACL contains an unexpected ACE: {sddl}"
            );
        }
        drop(asset);
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(windows)]
    #[test]
    fn verified_asset_guard_blocks_write_and_replacement_until_dropped() {
        let root = test_root("windows-locked-handoff");
        let asset = materialize_silero_vad_in(&root, SILERO_VAD_BYTES).unwrap();
        let path = asset.path().to_path_buf();
        let replacement = path.with_extension("replacement");
        fs::write(&replacement, SILERO_VAD_BYTES).unwrap();
        secure_file(&replacement).unwrap();

        let write_error = OpenOptions::new()
            .write(true)
            .open(&path)
            .expect_err("verified asset unexpectedly allowed a writer");
        assert_eq!(write_error.raw_os_error(), Some(32));
        let replace_error = replace_file(&replacement, &path)
            .expect_err("verified asset unexpectedly allowed replacement");
        assert!(
            matches!(replace_error.raw_os_error(), Some(5) | Some(32)),
            "expected access-denied or sharing-violation replacement failure, got {replace_error:?}"
        );

        drop(asset);
        replace_file(&replacement, &path).unwrap();
        fs::write(&path, b"corrupt after verified guard drop").unwrap();
        let repaired = materialize_silero_vad_in(&root, SILERO_VAD_BYTES).unwrap();
        assert_eq!(repaired.path(), path);
        drop(repaired);
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
