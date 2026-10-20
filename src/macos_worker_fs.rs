//! Shared read-only macOS worker filesystem safety policy.
//!
//! Runtime verification and the forthcoming installer share bounded primitives.
//! Opening a descriptor alone is never trusted ancestry or executable authority;
//! fixed-root walking, content authentication and publication remain caller-owned.

use anyhow::{Result, bail};

pub(crate) const ACL_EXTENDED_ALLOW: i32 = 1;
const ACL_EXTENDED_DENY: i32 = 2;
const ACL_READ_DATA: u64 = 1 << 1;
pub(crate) const ACL_WRITE_DATA: u64 = 1 << 2;
const ACL_EXECUTE: u64 = 1 << 3;
const ACL_DELETE: u64 = 1 << 4;
const ACL_APPEND_DATA: u64 = 1 << 5;
const ACL_DELETE_CHILD: u64 = 1 << 6;
const ACL_READ_ATTRIBUTES: u64 = 1 << 7;
const ACL_WRITE_ATTRIBUTES: u64 = 1 << 8;
const ACL_READ_EXTATTRIBUTES: u64 = 1 << 9;
const ACL_WRITE_EXTATTRIBUTES: u64 = 1 << 10;
const ACL_READ_SECURITY: u64 = 1 << 11;
const ACL_WRITE_SECURITY: u64 = 1 << 12;
const ACL_CHANGE_OWNER: u64 = 1 << 13;
const ACL_SYNCHRONIZE: u64 = 1 << 20;
const ACL_KNOWN_RIGHTS: u64 = ACL_READ_DATA
    | ACL_WRITE_DATA
    | ACL_EXECUTE
    | ACL_DELETE
    | ACL_APPEND_DATA
    | ACL_DELETE_CHILD
    | ACL_READ_ATTRIBUTES
    | ACL_WRITE_ATTRIBUTES
    | ACL_READ_EXTATTRIBUTES
    | ACL_WRITE_EXTATTRIBUTES
    | ACL_READ_SECURITY
    | ACL_WRITE_SECURITY
    | ACL_CHANGE_OWNER
    | ACL_SYNCHRONIZE;
const ACL_MUTATION_RIGHTS: u64 = ACL_WRITE_DATA
    | ACL_DELETE
    | ACL_APPEND_DATA
    | ACL_DELETE_CHILD
    | ACL_WRITE_ATTRIBUTES
    | ACL_WRITE_EXTATTRIBUTES
    | ACL_WRITE_SECURITY
    | ACL_CHANGE_OWNER;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum NodeKind {
    Directory,
    Regular,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PortableNodeFacts {
    owner_matches: bool,
    directory: bool,
    regular: bool,
    permissions: u32,
    links: u64,
    size: u64,
}

fn validate_node_policy(
    facts: PortableNodeFacts,
    kind: NodeKind,
    expected_size: Option<u64>,
    executable: bool,
) -> Result<()> {
    if !facts.owner_matches {
        bail!("protected worker node has an unexpected owner");
    }
    if facts.permissions & 0o022 != 0 || facts.permissions & 0o6000 != 0 {
        bail!("protected worker node has mutable or set-id mode bits");
    }
    match kind {
        NodeKind::Directory if !facts.directory || facts.regular => {
            bail!("protected worker directory changed type");
        }
        NodeKind::Regular if !facts.regular || facts.directory => {
            bail!("protected worker file changed type");
        }
        NodeKind::Regular if facts.links != 1 => {
            bail!("protected worker file is hard-linked");
        }
        _ => {}
    }
    if expected_size.is_some_and(|expected| facts.size != expected) {
        bail!("protected worker file changed size");
    }
    if executable && facts.permissions & 0o111 == 0 {
        bail!("protected worker executable has no execute permission");
    }
    Ok(())
}

fn validate_acl_entry(tag: i32, rights: u64) -> Result<()> {
    if !matches!(tag, ACL_EXTENDED_ALLOW | ACL_EXTENDED_DENY) {
        bail!("protected worker ACL contains an unknown tag");
    }
    if rights & !ACL_KNOWN_RIGHTS != 0 {
        bail!("protected worker ACL contains unknown rights");
    }
    // Inheritance and entry ordering never make a mutation grant admissible.
    if tag == ACL_EXTENDED_ALLOW && rights & ACL_MUTATION_RIGHTS != 0 {
        bail!("protected worker ACL grants mutation authority");
    }
    Ok(())
}

fn classify_absent_acl(null_acl: bool, raw_error: Option<i32>) -> Result<bool> {
    if !null_acl {
        return Ok(false);
    }
    // Darwin errno.h: ENOENT == 2. Keep this boundary host-testable without
    // adding the Unix-only libc dependency to Windows production builds.
    if raw_error == Some(2) {
        return Ok(true);
    }
    bail!("protected worker ACL query failed")
}

fn validate_filesystem_policy(name: &[u8], local: bool, ignores_ownership: bool) -> Result<()> {
    if name != b"apfs" || !local || ignores_ownership {
        bail!("protected worker authority requires ownership-enforcing local APFS");
    }
    Ok(())
}

fn validate_child_name(name: &[u8]) -> Result<()> {
    if name.is_empty()
        || name == b"."
        || name == b".."
        || name
            .iter()
            .any(|byte| matches!(*byte, b'/' | b'\\' | b':' | 0))
    {
        bail!("protected worker child name is not a single canonical component");
    }
    Ok(())
}

#[derive(Debug, Eq, PartialEq)]
enum EntryNameError {
    NonUtf8,
    Empty,
}

fn decode_entry_name(bytes: &[u8]) -> std::result::Result<&str, EntryNameError> {
    let name = std::str::from_utf8(bytes).map_err(|_| EntryNameError::NonUtf8)?;
    if name.is_empty() {
        return Err(EntryNameError::Empty);
    }
    Ok(name)
}

#[cfg(target_os = "macos")]
mod platform {
    use super::*;
    use anyhow::{Context, anyhow};
    use std::ffi::{CString, c_int, c_void};
    use std::fs::File;
    use std::io;
    use std::mem::MaybeUninit;
    use std::os::fd::{AsRawFd, FromRawFd, RawFd};
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;

    const ACL_TYPE_EXTENDED: c_int = 0x100;
    const ACL_FIRST_ENTRY: c_int = 0;
    const ACL_NEXT_ENTRY: c_int = -1;
    const MAX_ACL_ENTRIES: usize = 128;

    type Acl = *mut c_void;
    type AclEntry = *mut c_void;

    unsafe extern "C" {
        fn acl_get_fd_np(descriptor: c_int, acl_type: c_int) -> Acl;
        fn acl_free(object: *mut c_void) -> c_int;
        fn acl_valid(acl: Acl) -> c_int;
        fn acl_get_entry(acl: Acl, entry_id: c_int, entry: *mut AclEntry) -> c_int;
        fn acl_get_tag_type(entry: AclEntry, tag: *mut c_int) -> c_int;
        fn acl_get_permset_mask_np(entry: AclEntry, rights: *mut u64) -> c_int;
    }

    #[derive(Debug)]
    struct OwnedAcl(Acl);

    impl Drop for OwnedAcl {
        fn drop(&mut self) {
            unsafe {
                acl_free(self.0);
            }
        }
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub(crate) struct NodeIdentity {
        pub(crate) device: u64,
        pub(crate) inode: u64,
        pub(crate) kind: NodeKind,
    }

    pub(crate) fn open_absolute_directory(path: &Path) -> Result<File> {
        if !path.is_absolute() {
            bail!("protected worker directory path is not absolute");
        }
        let path = CString::new(path.as_os_str().as_bytes())?;
        let descriptor = unsafe {
            libc::open(
                path.as_ptr(),
                libc::O_RDONLY
                    | libc::O_DIRECTORY
                    | libc::O_NOFOLLOW
                    | libc::O_CLOEXEC
                    | libc::O_NONBLOCK,
            )
        };
        if descriptor < 0 {
            return Err(std::io::Error::last_os_error())
                .context("could not open protected worker directory");
        }
        Ok(unsafe { File::from_raw_fd(descriptor) })
    }

    pub(crate) fn openat_node(
        parent: RawFd,
        name: &std::ffi::OsStr,
        kind: NodeKind,
    ) -> Result<File> {
        validate_child_name(name.as_bytes())?;
        let name = CString::new(name.as_bytes())?;
        let type_flag = match kind {
            NodeKind::Directory => libc::O_DIRECTORY,
            NodeKind::Regular => 0,
        };
        let descriptor = unsafe {
            libc::openat(
                parent,
                name.as_ptr(),
                libc::O_RDONLY | type_flag | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
            )
        };
        if descriptor < 0 {
            return Err(std::io::Error::last_os_error())
                .context("could not descriptor-open protected worker node");
        }
        Ok(unsafe { File::from_raw_fd(descriptor) })
    }

    pub(crate) fn validate_native_node(
        file: &File,
        kind: NodeKind,
        expected_uid: libc::uid_t,
        expected_size: Option<u64>,
        executable: bool,
    ) -> Result<NodeIdentity> {
        validate_apfs(file.as_raw_fd())?;
        let status = stat_with_acl(file.as_raw_fd())?;
        let file_kind = status.st_mode & libc::S_IFMT;
        let directory = file_kind == libc::S_IFDIR;
        let regular = file_kind == libc::S_IFREG;
        let size = u64::try_from(status.st_size)
            .map_err(|_| anyhow!("protected worker node has a negative size"))?;
        validate_node_policy(
            PortableNodeFacts {
                owner_matches: status.st_uid == expected_uid,
                directory,
                regular,
                permissions: status.st_mode as u32,
                links: status.st_nlink as u64,
                size,
            },
            kind,
            expected_size,
            executable,
        )?;
        Ok(NodeIdentity {
            device: status.st_dev as u64,
            inode: status.st_ino,
            kind,
        })
    }

    fn validate_apfs(descriptor: RawFd) -> Result<()> {
        let mut filesystem = MaybeUninit::<libc::statfs>::zeroed();
        if unsafe { libc::fstatfs(descriptor, filesystem.as_mut_ptr()) } != 0 {
            return Err(std::io::Error::last_os_error())
                .context("could not inspect protected worker filesystem");
        }
        let filesystem = unsafe { filesystem.assume_init() };
        let end = filesystem
            .f_fstypename
            .iter()
            .position(|byte| *byte == 0)
            .unwrap_or(filesystem.f_fstypename.len());
        let filesystem_name = filesystem.f_fstypename[..end]
            .iter()
            .map(|byte| *byte as u8)
            .collect::<Vec<_>>();
        let flags = filesystem.f_flags as c_int;
        validate_filesystem_policy(
            &filesystem_name,
            flags & libc::MNT_LOCAL != 0,
            flags & libc::MNT_IGNORE_OWNERSHIP != 0,
        )
    }

    fn stat_with_acl(descriptor: RawFd) -> Result<libc::stat> {
        let mut status = MaybeUninit::<libc::stat>::zeroed();
        if unsafe { libc::fstat(descriptor, status.as_mut_ptr()) } != 0 {
            return Err(std::io::Error::last_os_error())
                .context("could not stat protected worker node");
        }
        // Apple Libc's acl_get_fd_np reports ENOENT specifically when the
        // filesec query found no extended ACL. No other null result is empty:
        // https://github.com/apple-oss-distributions/Libc/blob/main/posix1e/acl_file.c
        let acl = unsafe { acl_get_fd_np(descriptor, ACL_TYPE_EXTENDED) };
        if acl.is_null() {
            let error = std::io::Error::last_os_error();
            classify_absent_acl(true, error.raw_os_error())
                .context("could not read protected worker ACL")?;
        } else {
            validate_acl(OwnedAcl(acl))?;
        }
        Ok(unsafe { status.assume_init() })
    }

    fn validate_acl(acl: OwnedAcl) -> Result<()> {
        if unsafe { acl_valid(acl.0) } != 0 {
            return Err(std::io::Error::last_os_error()).context("protected worker ACL is invalid");
        }
        let mut entry: AclEntry = std::ptr::null_mut();
        let mut entry_id = ACL_FIRST_ENTRY;
        for count in 0..=MAX_ACL_ENTRIES {
            let result = unsafe { acl_get_entry(acl.0, entry_id, &mut entry) };
            if result == -1 {
                let error = std::io::Error::last_os_error();
                if error.raw_os_error() == Some(libc::EINVAL) {
                    return Ok(());
                }
                return Err(error).context("could not enumerate protected worker ACL");
            }
            if result != 0 || entry.is_null() || count == MAX_ACL_ENTRIES {
                bail!("protected worker ACL exceeds its entry bound");
            }
            let mut tag = 0;
            let mut rights = 0;
            if unsafe { acl_get_tag_type(entry, &mut tag) } != 0
                || unsafe { acl_get_permset_mask_np(entry, &mut rights) } != 0
            {
                return Err(std::io::Error::last_os_error())
                    .context("could not inspect protected worker ACL entry");
            }
            validate_acl_entry(tag, rights)?;
            entry_id = ACL_NEXT_ENTRY;
        }
        unreachable!("bounded ACL loop returns on every terminal state")
    }

    #[derive(Debug)]
    pub(crate) enum DirectoryReadError {
        Io(io::Error),
        AncestorChanged,
        InvalidName(&'static str),
        TooManyEntries,
    }

    impl std::fmt::Display for DirectoryReadError {
        fn fmt(&self, output: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self {
                Self::Io(error) => {
                    write!(output, "could not read protected worker directory: {error}")
                }
                Self::AncestorChanged => {
                    output.write_str("protected worker directory identity changed")
                }
                Self::InvalidName(reason) => output.write_str(reason),
                Self::TooManyEntries => {
                    output.write_str("protected worker directory exceeds its inventory bound")
                }
            }
        }
    }

    impl std::error::Error for DirectoryReadError {}

    struct MacosDirectoryStream(*mut libc::DIR);

    impl MacosDirectoryStream {
        fn close(mut self) -> Result<(), DirectoryReadError> {
            let stream = std::mem::replace(&mut self.0, std::ptr::null_mut());
            if unsafe { libc::closedir(stream) } != 0 {
                return Err(DirectoryReadError::Io(io::Error::last_os_error()));
            }
            Ok(())
        }
    }

    impl Drop for MacosDirectoryStream {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe {
                    libc::closedir(self.0);
                }
            }
        }
    }

    pub(crate) fn macos_directory_names(
        directory: &File,
        maximum_entries: usize,
    ) -> Result<Vec<String>, DirectoryReadError> {
        use std::os::unix::io::AsRawFd;

        let expected = descriptor_stat(directory.as_raw_fd()).map_err(DirectoryReadError::Io)?;
        let duplicated = unsafe {
            libc::openat(
                directory.as_raw_fd(),
                b".\0".as_ptr().cast(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if duplicated < 0 {
            return Err(DirectoryReadError::Io(io::Error::last_os_error()));
        }
        let reopened = match descriptor_stat(duplicated) {
            Ok(stat) => stat,
            Err(error) => {
                unsafe {
                    libc::close(duplicated);
                }
                return Err(DirectoryReadError::Io(error));
            }
        };
        if reopened.st_mode & libc::S_IFMT != libc::S_IFDIR
            || reopened.st_dev != expected.st_dev
            || reopened.st_ino != expected.st_ino
        {
            unsafe {
                libc::close(duplicated);
            }
            return Err(DirectoryReadError::AncestorChanged);
        }
        let raw_stream = unsafe { libc::fdopendir(duplicated) };
        if raw_stream.is_null() {
            let error = io::Error::last_os_error();
            unsafe {
                libc::close(duplicated);
            }
            return Err(DirectoryReadError::Io(error));
        }
        let stream = MacosDirectoryStream(raw_stream);
        let mut names = Vec::new();
        loop {
            unsafe {
                *libc::__error() = 0;
            }
            let entry = unsafe { libc::readdir(stream.0) };
            if entry.is_null() {
                let error_number = unsafe { *libc::__error() };
                if error_number != 0 {
                    return Err(DirectoryReadError::Io(io::Error::from_raw_os_error(
                        error_number,
                    )));
                }
                break;
            }
            let bytes = unsafe { std::ffi::CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
            if bytes == b"." || bytes == b".." {
                continue;
            }
            let name = decode_entry_name(bytes).map_err(|error| {
                DirectoryReadError::InvalidName(match error {
                    EntryNameError::NonUtf8 => "non-UTF-8 pack entry",
                    EntryNameError::Empty => "empty pack entry",
                })
            })?;
            names.push(name.to_owned());
            if names.len() > maximum_entries {
                return Err(DirectoryReadError::TooManyEntries);
            }
        }
        stream.close()?;
        names.sort_unstable();
        Ok(names)
    }

    pub(crate) fn descriptor_stat(descriptor: libc::c_int) -> std::io::Result<libc::stat> {
        use std::mem::MaybeUninit;

        let mut stat = MaybeUninit::<libc::stat>::uninit();
        if unsafe { libc::fstat(descriptor, stat.as_mut_ptr()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(unsafe { stat.assume_init() })
    }
}

#[cfg(target_os = "macos")]
#[allow(
    unused_imports,
    reason = "shared policy consumers use different bounded primitive subsets"
)]
pub(crate) use platform::{
    DirectoryReadError, NodeIdentity, descriptor_stat, macos_directory_names,
    open_absolute_directory, openat_node, validate_native_node,
};

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn macos_worker_fs_mode_policy_rejects_owner_mutability_links_size_and_execution_failures() {
        let safe = PortableNodeFacts {
            owner_matches: true,
            directory: false,
            regular: true,
            permissions: 0o100755,
            links: 1,
            size: 7,
        };
        validate_node_policy(safe, NodeKind::Regular, Some(7), true).unwrap();
        let directory = PortableNodeFacts {
            directory: true,
            regular: false,
            permissions: 0o40755,
            links: 2,
            ..safe
        };
        validate_node_policy(directory, NodeKind::Directory, None, false).unwrap();
        assert!(validate_node_policy(directory, NodeKind::Regular, None, false).is_err());
        assert!(validate_node_policy(safe, NodeKind::Directory, None, false).is_err());
        for invalid in [
            PortableNodeFacts {
                owner_matches: false,
                ..safe
            },
            PortableNodeFacts {
                permissions: 0o100775,
                ..safe
            },
            PortableNodeFacts {
                permissions: 0o104755,
                ..safe
            },
            PortableNodeFacts { links: 2, ..safe },
            PortableNodeFacts { size: 8, ..safe },
            PortableNodeFacts {
                permissions: 0o100644,
                ..safe
            },
        ] {
            assert!(validate_node_policy(invalid, NodeKind::Regular, Some(7), true).is_err());
        }
    }

    #[test]
    fn macos_worker_fs_acl_policy_rejects_mutation_unknown_rights_and_unknown_tags() {
        validate_acl_entry(ACL_EXTENDED_ALLOW, ACL_READ_DATA | ACL_EXECUTE).unwrap();
        validate_acl_entry(ACL_EXTENDED_DENY, ACL_MUTATION_RIGHTS).unwrap();
        assert!(validate_acl_entry(ACL_EXTENDED_ALLOW, ACL_WRITE_DATA).is_err());
        for bit in 0..64 {
            let right = 1_u64 << bit;
            if right & ACL_MUTATION_RIGHTS != 0 {
                assert!(validate_acl_entry(ACL_EXTENDED_ALLOW, right).is_err());
            }
        }
        assert!(validate_acl_entry(ACL_EXTENDED_DENY, 1 << 63).is_err());
        assert!(validate_acl_entry(99, ACL_READ_DATA).is_err());
    }

    #[test]
    fn macos_worker_fs_filesystem_policy_requires_local_ownership_enforcing_apfs() {
        validate_filesystem_policy(b"apfs", true, false).unwrap();
        for (name, local, ignored) in [
            (b"hfs".as_slice(), true, false),
            (b"nfs".as_slice(), false, false),
            (b"apfs".as_slice(), false, false),
            (b"apfs".as_slice(), true, true),
            (b"".as_slice(), true, false),
        ] {
            assert!(validate_filesystem_policy(name, local, ignored).is_err());
        }
    }

    #[test]
    fn macos_worker_fs_acl_absence_rejects_every_other_query_failure() {
        assert!(classify_absent_acl(false, None).is_ok_and(|absent| !absent));
        assert!(classify_absent_acl(true, Some(2)).is_ok_and(|absent| absent));
        for error in [None, Some(22), Some(5)] {
            assert!(classify_absent_acl(true, error).is_err());
        }
    }

    #[test]
    fn macos_worker_fs_child_names_reject_traversal_and_separator_aliases() {
        validate_child_name(b"scribe-inference-worker").unwrap();
        for name in [
            b"".as_slice(),
            b".",
            b"..",
            b"/worker",
            b"bin/worker",
            b"bin\\worker",
            b"worker:stream",
            b"worker\0tail",
        ] {
            assert!(validate_child_name(name).is_err(), "accepted {name:?}");
        }
    }

    #[test]
    fn macos_worker_fs_entry_names_reject_non_utf8_and_empty() {
        assert_eq!(decode_entry_name(b"worker"), Ok("worker"));
        assert_eq!(decode_entry_name(b""), Err(EntryNameError::Empty));
        assert_eq!(decode_entry_name(&[0xff]), Err(EntryNameError::NonUtf8));
    }

    #[cfg(target_os = "macos")]
    mod native {
        use super::super::*;
        use std::ffi::OsStr;
        use std::fs;
        use std::os::fd::AsRawFd;
        use std::os::unix::ffi::OsStrExt;
        use std::os::unix::fs::{PermissionsExt, symlink};
        use std::sync::atomic::{AtomicU64, Ordering};

        static NEXT_ROOT: AtomicU64 = AtomicU64::new(0);
        struct Fixture(std::path::PathBuf);
        impl Fixture {
            fn new() -> Self {
                let path = std::env::temp_dir().join(format!(
                    "scribe-worker-fs-{}-{}",
                    std::process::id(),
                    NEXT_ROOT.fetch_add(1, Ordering::Relaxed)
                ));
                fs::create_dir(&path).unwrap();
                fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
                Self(path)
            }
        }
        impl Drop for Fixture {
            fn drop(&mut self) {
                fs::remove_dir_all(&self.0).expect("clean owned filesystem fixture");
            }
        }

        #[test]
        fn macos_worker_fs_directory_enumeration_is_bounded_repeatable_and_descriptor_relative() {
            let fixture = Fixture::new();
            let original = fixture.0.join("original");
            fs::create_dir(&original).unwrap();
            fs::write(original.join("b"), b"b").unwrap();
            fs::write(original.join("a"), b"a").unwrap();
            let directory = open_absolute_directory(&original).unwrap();
            assert_eq!(macos_directory_names(&directory, 2).unwrap(), ["a", "b"]);
            assert!(matches!(
                macos_directory_names(&directory, 1),
                Err(DirectoryReadError::TooManyEntries)
            ));
            assert_eq!(macos_directory_names(&directory, 2).unwrap(), ["a", "b"]);
            fs::rename(&original, fixture.0.join("retained")).unwrap();
            fs::create_dir(&original).unwrap();
            fs::write(original.join("decoy"), b"decoy").unwrap();
            assert_eq!(macos_directory_names(&directory, 2).unwrap(), ["a", "b"]);
            assert!(
                macos_directory_names(&fs::File::open(original.join("decoy")).unwrap(), 1).is_err()
            );
        }

        #[test]
        fn macos_worker_fs_child_open_never_follows_links_or_accepts_path_components() {
            let fixture = Fixture::new();
            fs::write(fixture.0.join("worker"), b"image").unwrap();
            fs::set_permissions(fixture.0.join("worker"), fs::Permissions::from_mode(0o700))
                .unwrap();
            symlink("worker", fixture.0.join("alias")).unwrap();
            let root = open_absolute_directory(&fixture.0).unwrap();
            assert!(open_absolute_directory(std::path::Path::new(".")).is_err());
            for name in [
                "",
                ".",
                "..",
                "../worker",
                "worker/child",
                "worker:stream",
                "alias",
            ] {
                assert!(
                    openat_node(root.as_raw_fd(), OsStr::new(name), NodeKind::Regular).is_err()
                );
            }
            let worker =
                openat_node(root.as_raw_fd(), OsStr::new("worker"), NodeKind::Regular).unwrap();
            let identity = validate_native_node(
                &worker,
                NodeKind::Regular,
                unsafe { libc::geteuid() },
                Some(5),
                true,
            )
            .unwrap();
            assert_eq!(identity.kind, NodeKind::Regular);
            assert!(
                validate_native_node(
                    &worker,
                    NodeKind::Directory,
                    unsafe { libc::geteuid() },
                    None,
                    false
                )
                .is_err()
            );
            let fifo_path =
                std::ffi::CString::new(fixture.0.join("fifo").as_os_str().as_bytes()).unwrap();
            assert_eq!(unsafe { libc::mkfifo(fifo_path.as_ptr(), 0o600) }, 0);
            let fifo =
                openat_node(root.as_raw_fd(), OsStr::new("fifo"), NodeKind::Regular).unwrap();
            assert!(
                validate_native_node(
                    &fifo,
                    NodeKind::Regular,
                    unsafe { libc::geteuid() },
                    None,
                    false
                )
                .is_err()
            );
            fs::hard_link(fixture.0.join("worker"), fixture.0.join("hardlink")).unwrap();
            assert!(
                validate_native_node(
                    &worker,
                    NodeKind::Regular,
                    unsafe { libc::geteuid() },
                    Some(5),
                    true
                )
                .is_err()
            );
        }
    }
}
