//! Root-owned, descriptor-pinned authority for installed macOS worker packs.
//!
//! The signed pack verifier remains responsible for content authentication and
//! exact inventory equality. This module contributes a narrower invariant: the
//! verifier and launcher consume one retained APFS object graph whose ownership,
//! permissions, ACLs, and descriptor-relative identities are rechecked before use.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

use crate::gpu_worker_pack::manifest::{
    MANIFEST_NAME, SIGNATURE_NAME, StoreComponent, VerifiedCopyEntry,
};

const PACKS_ROOT: &str = "/Library/Application Support/Scribe/WorkerPacks";
const MAX_AUTHORITY_FILES: usize = 258;
const ACL_EXTENDED_ALLOW: i32 = 1;
const ACL_EXTENDED_DENY: i32 = 2;
const ACL_READ_DATA: u64 = 1 << 1;
const ACL_WRITE_DATA: u64 = 1 << 2;
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
enum PlannedKind {
    Directory,
    Regular,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PlannedFile {
    relative: PathBuf,
    size: u64,
    worker: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct InventoryPlan {
    directories: Vec<PathBuf>,
    files: Vec<PlannedFile>,
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
    kind: PlannedKind,
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
        PlannedKind::Directory if !facts.directory || facts.regular => {
            bail!("protected worker directory changed type");
        }
        PlannedKind::Regular if !facts.regular || facts.directory => {
            bail!("protected worker file changed type");
        }
        PlannedKind::Regular if facts.links != 1 => {
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

fn canonical_relative(raw: &str) -> Result<PathBuf> {
    if raw.is_empty()
        || raw.starts_with('/')
        || raw.ends_with('/')
        || raw.contains('\0')
        || raw.contains('\\')
        || raw.contains(':')
    {
        bail!("protected worker inventory contains a noncanonical path");
    }
    let mut path = PathBuf::new();
    for component in raw.split('/') {
        if component.is_empty() || component == "." || component == ".." {
            bail!("protected worker inventory contains a noncanonical path");
        }
        path.push(component);
    }
    Ok(path)
}

fn inventory_plan(entries: &[VerifiedCopyEntry], worker: &str) -> Result<InventoryPlan> {
    if entries.len() < 3 || entries.len() > MAX_AUTHORITY_FILES {
        bail!("protected worker inventory has an invalid file count");
    }
    let worker = canonical_relative(worker)?;
    let mut files = Vec::with_capacity(entries.len());
    let mut seen_files = BTreeSet::new();
    let mut directories = BTreeSet::new();
    let mut worker_count = 0;
    for entry in entries {
        let relative = canonical_relative(&entry.path)?;
        if !seen_files.insert(relative.clone()) {
            bail!("protected worker inventory contains a duplicate file");
        }
        let is_worker = relative == worker;
        worker_count += usize::from(is_worker);
        let mut parent = relative.parent();
        while let Some(value) = parent {
            if value.as_os_str().is_empty() {
                break;
            }
            directories.insert(value.to_path_buf());
            parent = value.parent();
        }
        files.push(PlannedFile {
            relative,
            size: entry.size_bytes,
            worker: is_worker,
        });
    }
    if worker_count != 1
        || worker == Path::new(MANIFEST_NAME)
        || worker == Path::new(SIGNATURE_NAME)
        || !seen_files.contains(Path::new(MANIFEST_NAME))
        || !seen_files.contains(Path::new(SIGNATURE_NAME))
        || directories.iter().any(|path| seen_files.contains(path))
    {
        bail!("protected worker inventory is not an exact executable pack");
    }
    let mut directories = directories.into_iter().collect::<Vec<_>>();
    directories.sort_by(|left, right| {
        left.components()
            .count()
            .cmp(&right.components().count())
            .then_with(|| left.cmp(right))
    });
    files.sort_by(|left, right| left.relative.cmp(&right.relative));
    Ok(InventoryPlan { directories, files })
}

fn canonical_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn protected_pack_root(pack_id: &str, pack_version: &str, digest: &str) -> Result<PathBuf> {
    if StoreComponent::new(pack_id).is_none()
        || StoreComponent::new(pack_version).is_none()
        || !canonical_digest(digest)
    {
        bail!("protected worker identity is not canonical");
    }
    Ok(Path::new(PACKS_ROOT)
        .join(pack_id)
        .join(pack_version)
        .join(digest))
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

#[cfg(target_os = "macos")]
mod platform {
    use std::collections::BTreeMap;
    use std::ffi::{CStr, CString, c_int, c_void};
    use std::fs::File;
    use std::mem::MaybeUninit;
    use std::os::fd::{AsRawFd, FromRawFd, RawFd};
    use std::os::unix::ffi::OsStrExt;

    use anyhow::{Context, anyhow};

    use super::*;
    use crate::gpu_worker_pack::manifest::{PackBackend, PinnedPackRoot, VerifiedPackLease};

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
    struct NodeIdentity {
        device: u64,
        inode: u64,
        kind: PlannedKind,
    }

    #[derive(Debug)]
    struct HeldNode {
        file: File,
        identity: NodeIdentity,
    }

    #[derive(Debug)]
    struct OpenState {
        ancestry: Vec<HeldNode>,
        directories: Vec<HeldNode>,
        files: Vec<HeldNode>,
    }

    impl OpenState {
        fn identities(&self) -> impl Iterator<Item = NodeIdentity> + '_ {
            self.ancestry
                .iter()
                .chain(&self.directories)
                .chain(&self.files)
                .map(|node| node.identity)
        }
    }

    #[derive(Clone, Debug)]
    enum RootPolicy {
        Production,
        #[cfg(test)]
        Fixture {
            root: PathBuf,
            expected_uid: libc::uid_t,
        },
    }

    impl RootPolicy {
        fn pack_root(&self, source: &VerifiedPackLease) -> Result<PathBuf> {
            match self {
                Self::Production => {
                    let pack = source.verified_pack();
                    if pack.backend != PackBackend::Metal
                        || pack.target_os != "macos"
                        || !pack.pack_id.is_canonical()
                        || !pack.pack_version.is_canonical()
                        || !canonical_digest(&pack.pack_digest)
                        || source.copy_entries().len() != 3
                    {
                        bail!(
                            "protected worker authority accepts only a single-payload macOS Metal pack"
                        );
                    }
                    protected_pack_root(
                        pack.pack_id.as_str(),
                        pack.pack_version.as_str(),
                        &pack.pack_digest,
                    )
                }
                #[cfg(test)]
                Self::Fixture { root, .. } => Ok(root.clone()),
            }
        }

        fn expected_uid(&self) -> libc::uid_t {
            match self {
                Self::Production => 0,
                #[cfg(test)]
                Self::Fixture { expected_uid, .. } => *expected_uid,
            }
        }
    }

    /// Opaque retained authority for one protected installed pack.
    #[derive(Debug)]
    pub(crate) struct ProtectedPackAuthority {
        policy: RootPolicy,
        pack_root: PathBuf,
        worker_path: CString,
        plan: InventoryPlan,
        state: OpenState,
    }

    impl ProtectedPackAuthority {
        pub(crate) fn open(source: &VerifiedPackLease) -> Result<Self> {
            Self::open_with_policy(source, RootPolicy::Production)
        }

        #[cfg(test)]
        pub(crate) fn open_fixture(source: &VerifiedPackLease) -> Result<Self> {
            if source.test_reverification_trust().is_none() {
                bail!("protected worker fixture lacks explicit in-process fixture trust");
            }
            let pack = source.verified_pack();
            if !pack.pack_id.is_canonical()
                || !pack.pack_version.is_canonical()
                || !canonical_digest(&pack.pack_digest)
                || !pack.root.is_absolute()
            {
                bail!("protected worker fixture descriptor is not canonical");
            }
            Self::open_with_policy(
                source,
                RootPolicy::Fixture {
                    root: pack.root.clone(),
                    expected_uid: unsafe { libc::geteuid() },
                },
            )
        }

        fn open_with_policy(source: &VerifiedPackLease, policy: RootPolicy) -> Result<Self> {
            source
                .recheck()
                .context("source worker pack changed before protected authority creation")?;
            let pack_root = policy.pack_root(source)?;
            let plan = inventory_plan(
                source.copy_entries(),
                &source.verified_pack().worker_relative_path,
            )?;
            let state = open_state(&policy, &pack_root, &plan)?;
            source
                .recheck()
                .context("source worker pack changed during protected authority creation")?;
            let worker = pack_root.join(&source.verified_pack().worker_relative_path);
            let worker_path = CString::new(worker.as_os_str().as_bytes())
                .map_err(|_| anyhow!("protected worker path contains NUL"))?;
            Ok(Self {
                policy,
                pack_root,
                worker_path,
                plan,
                state,
            })
        }

        pub(crate) fn pack_root(&self) -> &Path {
            &self.pack_root
        }

        pub(crate) fn worker_path(&self) -> &CStr {
            &self.worker_path
        }

        pub(crate) fn recheck(&self) -> Result<()> {
            let fresh = open_state(&self.policy, &self.pack_root, &self.plan)?;
            if self.state.identities().ne(fresh.identities()) {
                bail!("protected worker object graph changed identity");
            }
            Ok(())
        }

        pub(crate) fn pinned_root(&self) -> Result<PinnedPackRoot> {
            self.recheck()?;
            let handles = self
                .state
                .ancestry
                .iter()
                .map(|node| node.file.try_clone())
                .collect::<std::io::Result<Vec<_>>>()
                .context("could not clone protected pack ancestry")?;
            PinnedPackRoot::from_anchored_handles(self.pack_root.clone(), handles)
                .context("could not construct protected pinned pack root")
        }
    }

    fn open_state(
        policy: &RootPolicy,
        pack_root: &Path,
        plan: &InventoryPlan,
    ) -> Result<OpenState> {
        let ancestry = open_ancestry(policy, pack_root)?;
        let root_fd = ancestry
            .last()
            .ok_or_else(|| anyhow!("protected worker ancestry is empty"))?
            .file
            .as_raw_fd();
        let mut directory_fds = BTreeMap::new();
        directory_fds.insert(PathBuf::new(), root_fd);
        let mut directories = Vec::with_capacity(plan.directories.len());
        for relative in &plan.directories {
            let parent = relative.parent().unwrap_or_else(|| Path::new(""));
            let parent_fd = *directory_fds
                .get(parent)
                .ok_or_else(|| anyhow!("protected worker directory parent is absent"))?;
            let name = relative
                .file_name()
                .ok_or_else(|| anyhow!("protected worker directory has no name"))?;
            let file = openat_node(parent_fd, name, PlannedKind::Directory)?;
            let identity = validate_native_node(
                &file,
                PlannedKind::Directory,
                policy.expected_uid(),
                None,
                false,
            )?;
            directory_fds.insert(relative.clone(), file.as_raw_fd());
            directories.push(HeldNode { file, identity });
        }
        let mut files = Vec::with_capacity(plan.files.len());
        for expected in &plan.files {
            let parent = expected.relative.parent().unwrap_or_else(|| Path::new(""));
            let parent_fd = *directory_fds
                .get(parent)
                .ok_or_else(|| anyhow!("protected worker file parent is absent"))?;
            let name = expected
                .relative
                .file_name()
                .ok_or_else(|| anyhow!("protected worker file has no name"))?;
            let file = openat_node(parent_fd, name, PlannedKind::Regular)?;
            let identity = validate_native_node(
                &file,
                PlannedKind::Regular,
                policy.expected_uid(),
                Some(expected.size),
                expected.worker,
            )?;
            files.push(HeldNode { file, identity });
        }
        Ok(OpenState {
            ancestry,
            directories,
            files,
        })
    }

    fn open_ancestry(policy: &RootPolicy, pack_root: &Path) -> Result<Vec<HeldNode>> {
        let mut nodes = Vec::new();
        match policy {
            RootPolicy::Production => {
                let mut current = open_absolute_directory(Path::new("/"))?;
                let identity =
                    validate_native_node(&current, PlannedKind::Directory, 0, None, false)?;
                nodes.push(HeldNode {
                    file: current,
                    identity,
                });
                let expected = Path::new(PACKS_ROOT)
                    .components()
                    .chain(pack_root.strip_prefix(PACKS_ROOT)?.components());
                for component in expected {
                    let std::path::Component::Normal(name) = component else {
                        continue;
                    };
                    current = openat_node(
                        nodes.last().expect("root node").file.as_raw_fd(),
                        name,
                        PlannedKind::Directory,
                    )?;
                    let identity =
                        validate_native_node(&current, PlannedKind::Directory, 0, None, false)?;
                    nodes.push(HeldNode {
                        file: current,
                        identity,
                    });
                }
            }
            #[cfg(test)]
            RootPolicy::Fixture { root, expected_uid } => {
                if root != pack_root || !root.is_absolute() || root == Path::new("/") {
                    bail!("protected worker fixture root is invalid");
                }
                let file = open_absolute_directory(root)?;
                let identity = validate_native_node(
                    &file,
                    PlannedKind::Directory,
                    *expected_uid,
                    None,
                    false,
                )?;
                nodes.push(HeldNode { file, identity });
            }
        }
        Ok(nodes)
    }

    fn open_absolute_directory(path: &Path) -> Result<File> {
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

    fn openat_node(parent: RawFd, name: &std::ffi::OsStr, kind: PlannedKind) -> Result<File> {
        let name = CString::new(name.as_bytes())?;
        let type_flag = match kind {
            PlannedKind::Directory => libc::O_DIRECTORY,
            PlannedKind::Regular => 0,
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

    fn validate_native_node(
        file: &File,
        kind: PlannedKind,
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

    #[cfg(test)]
    pub(super) fn open_fixture_tree_for_test(
        root: &Path,
        expected_uid: libc::uid_t,
        entries: &[VerifiedCopyEntry],
        worker: &str,
    ) -> Result<ProtectedPackAuthority> {
        let plan = inventory_plan(entries, worker)?;
        let policy = RootPolicy::Fixture {
            root: root.to_path_buf(),
            expected_uid,
        };
        let state = open_state(&policy, root, &plan)?;
        let worker_path = CString::new(root.join(worker).as_os_str().as_bytes())?;
        Ok(ProtectedPackAuthority {
            policy,
            pack_root: root.to_path_buf(),
            worker_path,
            plan,
            state,
        })
    }
}

#[cfg(target_os = "macos")]
pub(crate) use platform::ProtectedPackAuthority;

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(path: &str, size: u64) -> VerifiedCopyEntry {
        VerifiedCopyEntry {
            path: path.to_owned(),
            size_bytes: size,
            sha256: "00".repeat(32),
        }
    }

    #[test]
    fn protected_pack_inventory_plan_is_bounded_canonical_and_complete() {
        assert!(inventory_plan(&[], "bin/worker").is_err());
        let mut boundary = vec![
            entry(MANIFEST_NAME, 10),
            entry(SIGNATURE_NAME, 20),
            entry("bin/worker", 30),
        ];
        boundary
            .extend((0..MAX_AUTHORITY_FILES - 3).map(|index| entry(&format!("file-{index}"), 1)));
        assert!(inventory_plan(&boundary, "bin/worker").is_ok());
        boundary.push(entry("one-file-too-many", 1));
        assert!(inventory_plan(&boundary, "bin/worker").is_err());
        let plan = inventory_plan(
            &[
                entry(MANIFEST_NAME, 10),
                entry(SIGNATURE_NAME, 20),
                entry("bin/worker", 30),
                entry("lib/runtime.dylib", 40),
            ],
            "bin/worker",
        )
        .unwrap();
        assert_eq!(
            plan.directories,
            [PathBuf::from("bin"), PathBuf::from("lib")]
        );
        assert_eq!(plan.files.len(), 4);
        assert_eq!(plan.files.iter().filter(|file| file.worker).count(), 1);

        for invalid in [
            "../worker",
            "/worker",
            "bin//worker",
            "bin/./worker",
            "bin\\worker",
            "C:worker",
            "worker/",
        ] {
            assert!(
                inventory_plan(
                    &[
                        entry(MANIFEST_NAME, 10),
                        entry(SIGNATURE_NAME, 20),
                        entry(invalid, 30),
                    ],
                    invalid,
                )
                .is_err(),
                "accepted {invalid}"
            );
        }
    }

    #[test]
    fn protected_pack_inventory_plan_rejects_duplicates_collisions_and_missing_worker() {
        let base = [
            entry(MANIFEST_NAME, 10),
            entry(SIGNATURE_NAME, 20),
            entry("bin/worker", 30),
        ];
        let mut duplicate = base.to_vec();
        duplicate.push(entry("bin/worker", 30));
        assert!(inventory_plan(&duplicate, "bin/worker").is_err());

        let mut collision = base.to_vec();
        collision.push(entry("bin", 10));
        assert!(inventory_plan(&collision, "bin/worker").is_err());
        assert!(inventory_plan(&base, "bin/missing").is_err());
    }

    #[test]
    fn protected_pack_mode_policy_rejects_owner_mutability_links_size_and_execution_failures() {
        let safe = PortableNodeFacts {
            owner_matches: true,
            directory: false,
            regular: true,
            permissions: 0o100755,
            links: 1,
            size: 7,
        };
        validate_node_policy(safe, PlannedKind::Regular, Some(7), true).unwrap();
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
            assert!(validate_node_policy(invalid, PlannedKind::Regular, Some(7), true).is_err());
        }
    }

    #[test]
    fn protected_pack_acl_policy_rejects_mutation_unknown_rights_and_unknown_tags() {
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
    fn protected_pack_fixed_root_mapping_and_acl_absence_are_fail_closed() {
        let digest = "ab".repeat(32);
        assert_eq!(
            protected_pack_root("metal", "v1", &digest).unwrap(),
            Path::new(PACKS_ROOT).join("metal").join("v1").join(&digest)
        );
        for (pack, version, candidate_digest) in [
            ("../metal", "v1", digest.as_str()),
            ("metal", "../v1", digest.as_str()),
            ("Metal", "v1", digest.as_str()),
            ("metal", "v 1", digest.as_str()),
            ("metal", "v1", "ABCD"),
        ] {
            assert!(protected_pack_root(pack, version, candidate_digest).is_err());
        }
        assert!(classify_absent_acl(false, None).is_ok_and(|absent| !absent));
        assert!(classify_absent_acl(true, Some(2)).is_ok_and(|absent| absent));
        for error in [None, Some(22), Some(5)] {
            assert!(classify_absent_acl(true, error).is_err());
        }
    }

    #[test]
    fn protected_pack_filesystem_policy_requires_local_ownership_enforcing_apfs() {
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

    #[cfg(target_os = "macos")]
    mod native {
        use std::ffi::{c_int, c_void};
        use std::fs;
        use std::os::fd::AsRawFd;
        use std::os::unix::fs::{PermissionsExt, symlink};
        use std::sync::atomic::{AtomicU64, Ordering};

        use super::*;
        use crate::macos_worker_authority::platform::{
            ProtectedPackAuthority, open_fixture_tree_for_test,
        };

        type Acl = *mut c_void;
        type AclEntry = *mut c_void;
        type AclPermset = *mut c_void;

        unsafe extern "C" {
            fn acl_init(count: c_int) -> Acl;
            fn acl_free(object: *mut c_void) -> c_int;
            fn acl_create_entry(acl: *mut Acl, entry: *mut AclEntry) -> c_int;
            fn acl_get_permset(entry: AclEntry, permissions: *mut AclPermset) -> c_int;
            fn acl_clear_perms(permissions: AclPermset) -> c_int;
            fn acl_add_perm(permissions: AclPermset, permission: c_int) -> c_int;
            fn acl_set_tag_type(entry: AclEntry, tag: c_int) -> c_int;
            fn acl_set_qualifier(entry: AclEntry, qualifier: *const c_void) -> c_int;
            fn acl_valid(acl: Acl) -> c_int;
            fn acl_set_fd_np(descriptor: c_int, acl: Acl, acl_type: c_int) -> c_int;
            fn mbr_uid_to_uuid(uid: libc::uid_t, uuid: *mut u8) -> c_int;
        }

        struct TestAcl(Acl);

        impl Drop for TestAcl {
            fn drop(&mut self) {
                unsafe {
                    acl_free(self.0);
                }
            }
        }

        static NEXT_ROOT: AtomicU64 = AtomicU64::new(1);

        struct NativeFixture {
            root: PathBuf,
            entries: Vec<VerifiedCopyEntry>,
        }

        impl NativeFixture {
            fn new() -> Self {
                let root = std::env::temp_dir().join(format!(
                    "scribe-protected-pack-{}-{}",
                    std::process::id(),
                    NEXT_ROOT.fetch_add(1, Ordering::Relaxed)
                ));
                fs::create_dir(&root).unwrap();
                fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
                fs::create_dir(root.join("bin")).unwrap();
                fs::set_permissions(root.join("bin"), fs::Permissions::from_mode(0o755)).unwrap();
                let files = [
                    (MANIFEST_NAME, b"manifest".as_slice(), 0o644),
                    (SIGNATURE_NAME, b"signature".as_slice(), 0o644),
                    ("bin/worker", b"worker-image".as_slice(), 0o700),
                ];
                let mut entries = Vec::new();
                for (relative, bytes, mode) in files {
                    fs::write(root.join(relative), bytes).unwrap();
                    fs::set_permissions(root.join(relative), fs::Permissions::from_mode(mode))
                        .unwrap();
                    entries.push(entry(relative, bytes.len() as u64));
                }
                Self { root, entries }
            }

            fn open(&self) -> Result<ProtectedPackAuthority> {
                open_fixture_tree_for_test(
                    &self.root,
                    unsafe { libc::geteuid() },
                    &self.entries,
                    "bin/worker",
                )
            }
        }

        fn install_mutating_acl(path: &Path) {
            let file = fs::File::open(path).unwrap();
            let mut acl = unsafe { acl_init(1) };
            assert!(!acl.is_null());
            let mut entry: AclEntry = std::ptr::null_mut();
            assert_eq!(unsafe { acl_create_entry(&mut acl, &mut entry) }, 0);
            let acl_owner = TestAcl(acl);
            assert!(!entry.is_null());
            let mut permissions: AclPermset = std::ptr::null_mut();
            assert_eq!(unsafe { acl_get_permset(entry, &mut permissions) }, 0);
            assert!(!permissions.is_null());
            assert_eq!(unsafe { acl_clear_perms(permissions) }, 0);
            assert_eq!(
                unsafe { acl_add_perm(permissions, ACL_WRITE_DATA as c_int) },
                0
            );
            assert_eq!(unsafe { acl_set_tag_type(entry, ACL_EXTENDED_ALLOW) }, 0);
            let mut qualifier = [0_u8; 16];
            assert_eq!(
                unsafe { mbr_uid_to_uuid(libc::geteuid(), qualifier.as_mut_ptr()) },
                0
            );
            assert_eq!(
                unsafe { acl_set_qualifier(entry, qualifier.as_ptr().cast()) },
                0
            );
            assert_eq!(unsafe { acl_valid(acl_owner.0) }, 0);
            assert_eq!(
                unsafe { acl_set_fd_np(file.as_raw_fd(), acl_owner.0, 0x100) },
                0
            );
        }

        impl Drop for NativeFixture {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.root);
            }
        }

        #[test]
        fn protected_pack_native_authority_rechecks_swapped_worker_identity() {
            let fixture = NativeFixture::new();
            let authority = fixture.open().unwrap();
            let worker = fixture.root.join("bin/worker");
            let old = fixture.root.join("bin/old-worker");
            fs::rename(&worker, &old).unwrap();
            fs::write(&worker, b"worker-image").unwrap();
            fs::set_permissions(&worker, fs::Permissions::from_mode(0o700)).unwrap();
            assert!(authority.recheck().is_err());
        }

        #[test]
        fn protected_pack_native_authority_rejects_hardlink_and_symlink_leaves() {
            let hardlink = NativeFixture::new();
            fs::hard_link(
                hardlink.root.join("bin/worker"),
                hardlink.root.join("bin/worker-link"),
            )
            .unwrap();
            assert!(hardlink.open().is_err());

            let symbolic = NativeFixture::new();
            let worker = symbolic.root.join("bin/worker");
            fs::rename(&worker, symbolic.root.join("bin/real-worker")).unwrap();
            symlink("real-worker", &worker).unwrap();
            assert!(symbolic.open().is_err());
        }

        #[test]
        fn protected_pack_native_authority_rejects_mutable_modes_and_production_owner_policy() {
            let mutable = NativeFixture::new();
            fs::set_permissions(mutable.root.join("bin"), fs::Permissions::from_mode(0o775))
                .unwrap();
            assert!(mutable.open().is_err());

            let owner = NativeFixture::new();
            let effective_uid = unsafe { libc::geteuid() };
            assert_ne!(effective_uid, 0, "native fixture must run unprivileged");
            assert!(
                open_fixture_tree_for_test(&owner.root, 0, &owner.entries, "bin/worker").is_err()
            );
        }

        #[test]
        fn protected_pack_native_authority_rejects_mutating_extended_acl() {
            for relative in ["", "bin", "bin/worker", MANIFEST_NAME, SIGNATURE_NAME] {
                let fixture = NativeFixture::new();
                let authority = fixture.open().unwrap();
                install_mutating_acl(&fixture.root.join(relative));
                assert!(fixture.open().is_err(), "accepted ACL on {relative}");
                assert!(authority.recheck().is_err(), "reused ACL on {relative}");
            }
        }

        #[test]
        fn protected_pack_native_authority_rejects_directory_and_control_replacement() {
            for relative in ["bin", MANIFEST_NAME, SIGNATURE_NAME] {
                let fixture = NativeFixture::new();
                let authority = fixture.open().unwrap();
                let path = fixture.root.join(relative);
                let retained = path.with_extension("retained");
                fs::rename(&path, &retained).unwrap();
                if relative == "bin" {
                    fs::create_dir(&path).unwrap();
                } else {
                    fs::copy(&retained, &path).unwrap();
                }
                assert!(authority.recheck().is_err(), "reused replaced {relative}");
            }
        }

        #[test]
        fn protected_pack_native_authority_rejects_fifo_and_directory_symlink() {
            let fifo = NativeFixture::new();
            let worker = fifo.root.join("bin/worker");
            fs::remove_file(&worker).unwrap();
            use std::os::unix::ffi::OsStrExt;
            let name = std::ffi::CString::new(worker.as_os_str().as_bytes()).unwrap();
            assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o700) }, 0);
            // O_NONBLOCK ensures this is a rejection, not an open() hang.
            assert!(fifo.open().is_err());

            let linked = NativeFixture::new();
            fs::rename(linked.root.join("bin"), linked.root.join("old-bin")).unwrap();
            symlink("old-bin", linked.root.join("bin")).unwrap();
            assert!(linked.open().is_err());
        }
    }
}
