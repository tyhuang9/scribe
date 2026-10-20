//! Installer-only descriptor mechanics. Never imported by the desktop/worker.
use crate::macos_worker_fs::{NodeIdentity, NodeKind, descriptor_stat, validate_native_node};
use anyhow::{Context, Result, bail};
use std::ffi::CString;
use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, RawFd};

pub(crate) fn open_rw(parent: RawFd, name: &str, create: bool) -> Result<File> {
    validate_child(name)?;
    let name = CString::new(name)?;
    let flags = libc::O_RDWR
        | libc::O_NOFOLLOW
        | libc::O_CLOEXEC
        | libc::O_NONBLOCK
        | if create {
            libc::O_CREAT | libc::O_EXCL
        } else {
            0
        };
    owned_fd(unsafe { libc::openat(parent, name.as_ptr(), flags, 0o600) })
}
pub(crate) fn owned_fd(fd: RawFd) -> Result<File> {
    if fd < 0 {
        return Err(io::Error::last_os_error()).context("could not open installer descriptor");
    }
    Ok(unsafe { File::from_raw_fd(fd) })
}
pub(crate) fn set_mode(file: &File, mode: libc::mode_t) -> Result<()> {
    if unsafe { libc::fchmod(file.as_raw_fd(), mode) } != 0 {
        return Err(io::Error::last_os_error()).context("could not seal fresh inode permissions");
    }
    Ok(())
}
pub(crate) fn require_mode(file: &File, expected: u32) -> Result<()> {
    if u32::from(descriptor_stat(file.as_raw_fd())?.st_mode) & 0o7777 != expected {
        bail!("installer node has an unexpected mode");
    }
    Ok(())
}
pub(crate) fn checked_directory(
    file: &File,
    uid: libc::uid_t,
    exact_mode: Option<u32>,
) -> Result<NodeIdentity> {
    let identity = validate_native_node(file, NodeKind::Directory, uid, None, false)?;
    if let Some(mode) = exact_mode {
        require_mode(file, mode)?;
    } else if u32::from(descriptor_stat(file.as_raw_fd())?.st_mode) & 0o005 != 0o005 {
        bail!("protected installer ancestors must remain user-traversable");
    }
    Ok(identity)
}
pub(crate) fn checked_lock(file: &File, uid: libc::uid_t) -> Result<NodeIdentity> {
    let identity = validate_native_node(file, NodeKind::Regular, uid, Some(0), false)?;
    require_mode(file, 0o600)?;
    Ok(identity)
}
pub(crate) fn has_errno(error: &anyhow::Error, code: i32) -> bool {
    error
        .chain()
        .filter_map(|cause| cause.downcast_ref::<io::Error>())
        .any(|error| error.raw_os_error() == Some(code))
}
pub(crate) fn unlink(parent: RawFd, name: &str, directory: bool) -> Result<()> {
    validate_child(name)?;
    let name = CString::new(name)?;
    if unsafe {
        libc::unlinkat(
            parent,
            name.as_ptr(),
            if directory { libc::AT_REMOVEDIR } else { 0 },
        )
    } != 0
    {
        return Err(io::Error::last_os_error())
            .context("could not remove proven owned staging entry");
    }
    Ok(())
}
fn validate_child(name: &str) -> Result<()> {
    if name.is_empty()
        || name.len() > 255
        || matches!(name, "." | "..")
        || name
            .bytes()
            .any(|byte| matches!(byte, b'/' | b'\\' | b':' | 0))
    {
        bail!("installer write requires a single canonical child name");
    }
    Ok(())
}
