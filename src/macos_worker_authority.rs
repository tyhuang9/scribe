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
#[cfg(target_os = "macos")]
use crate::macos_worker_fs::NodeKind as PlannedKind;

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

#[cfg(target_os = "macos")]
mod platform {
    use std::collections::BTreeMap;
    use std::ffi::{CStr, CString};
    use std::fs::File;
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::ffi::OsStrExt;

    use anyhow::{Context, anyhow};

    use super::*;
    use crate::gpu_worker_pack::manifest::{PackBackend, PinnedPackRoot, VerifiedPackLease};

    use crate::macos_worker_fs::{
        NodeIdentity, open_absolute_directory, openat_node, validate_native_node,
    };

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
        CpuProduction,
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
                Self::CpuProduction => bail!("CPU authority cannot admit a GPU worker pack"),
                #[cfg(test)]
                Self::Fixture { root, .. } => Ok(root.clone()),
            }
        }

        fn expected_uid(&self) -> libc::uid_t {
            match self {
                Self::Production | Self::CpuProduction => 0,
                #[cfg(test)]
                Self::Fixture { expected_uid, .. } => *expected_uid,
            }
        }
    }

    /// Exact compiled CPU image in an installer-owned immutable directory.
    /// This is not a GPU pack and has no catalog, activation or signing key.
    #[derive(Debug)]
    pub(crate) struct ProtectedCpuAuthority {
        policy: RootPolicy,
        root: PathBuf,
        digest: String,
        worker_path: CString,
        ancestry: Vec<HeldNode>,
        readable: HeldNode,
        executable: File,
    }

    impl ProtectedCpuAuthority {
        pub(crate) fn open() -> Result<Self> {
            Self::open_with_policy(
                crate::macos_worker_layout::embedded_cpu_root()?,
                crate::macos_worker_layout::embedded_cpu_digest()?.to_owned(),
                RootPolicy::CpuProduction,
            )
        }

        #[cfg(test)]
        pub(crate) fn open_fixture(root: &Path, digest: &str) -> Result<Self> {
            Self::open_with_policy(
                root.to_path_buf(),
                digest.to_owned(),
                RootPolicy::Fixture {
                    root: root.to_path_buf(),
                    expected_uid: unsafe { libc::geteuid() },
                },
            )
        }

        fn open_with_policy(root: PathBuf, digest: String, policy: RootPolicy) -> Result<Self> {
            use crate::macos_worker_layout::{
                CPU_WORKER_NAME, validate_cpu_digest, validate_cpu_size,
            };

            validate_cpu_digest(&digest)?;
            let ancestry = open_ancestry(&policy, &root)?;
            let root_fd = ancestry
                .last()
                .ok_or_else(|| anyhow!("CPU ancestry is empty"))?
                .file
                .as_raw_fd();
            let names = crate::macos_worker_fs::macos_directory_names(
                &ancestry.last().expect("CPU root").file,
                1,
            )?;
            if names != [CPU_WORKER_NAME] {
                bail!("protected CPU directory must contain only its exact worker image");
            }
            let file = openat_node(
                root_fd,
                std::ffi::OsStr::new(CPU_WORKER_NAME),
                PlannedKind::Regular,
            )?;
            let identity = validate_native_node(
                &file,
                PlannedKind::Regular,
                policy.expected_uid(),
                None,
                true,
            )?;
            let length = file.metadata()?.len();
            validate_cpu_size(length)?;
            if cpu_image_digest(&file, length)? != digest {
                bail!("protected CPU image does not match the compiled final-image digest");
            }
            let descriptor = unsafe {
                libc::openat(
                    root_fd,
                    c"scribe-inference-worker".as_ptr(),
                    libc::O_EXEC | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
                )
            };
            if descriptor < 0 {
                return Err(std::io::Error::last_os_error())
                    .context("could not retain protected CPU executable evidence");
            }
            let executable = unsafe { File::from_raw_fd(descriptor) };
            use std::os::unix::fs::MetadataExt;
            let executable_metadata = executable.metadata()?;
            if !executable_metadata.is_file()
                || executable_metadata.dev() != identity.device
                || executable_metadata.ino() != identity.inode
                || executable_metadata.len() != length
                || validate_native_node(
                    &file,
                    PlannedKind::Regular,
                    policy.expected_uid(),
                    Some(length),
                    true,
                )? != identity
            {
                bail!("protected CPU image changed while its evidence was opened");
            }
            let worker_path = CString::new(root.join(CPU_WORKER_NAME).as_os_str().as_bytes())?;
            Ok(Self {
                policy,
                root,
                digest,
                worker_path,
                ancestry,
                readable: HeldNode { file, identity },
                executable,
            })
        }

        pub(crate) fn recheck(&self) -> Result<()> {
            let fresh = Self::open_with_policy(
                self.root.clone(),
                self.digest.clone(),
                self.policy.clone(),
            )?;
            if self
                .ancestry
                .iter()
                .map(|node| node.identity)
                .ne(fresh.ancestry.iter().map(|node| node.identity))
                || self.readable.identity != fresh.readable.identity
            {
                bail!("protected CPU object graph changed before launch");
            }
            Ok(())
        }

        pub(crate) fn root(&self) -> &Path {
            &self.root
        }
        pub(crate) fn protected_executable_path(&self) -> &CStr {
            &self.worker_path
        }
        pub(crate) fn executable_fd(&self) -> &File {
            &self.executable
        }
        pub(crate) fn readable_executable_fd(&self) -> &File {
            &self.readable.file
        }
        pub(crate) fn dependency_root_fd(&self) -> &File {
            &self.ancestry.last().expect("CPU root").file
        }
    }

    fn cpu_image_digest(file: &File, length: u64) -> Result<String> {
        use sha2::{Digest, Sha256};
        use std::os::unix::fs::FileExt;

        let mut hasher = Sha256::new();
        let mut offset = 0_u64;
        let mut buffer = [0_u8; 64 * 1024];
        while offset < length {
            let maximum = (length - offset).min(buffer.len() as u64) as usize;
            let count = file.read_at(&mut buffer[..maximum], offset)?;
            if count == 0 {
                bail!("protected CPU image ended before its retained length");
            }
            hasher.update(&buffer[..count]);
            offset += count as u64;
        }
        if file.read_at(&mut buffer[..1], length)? != 0 {
            bail!("protected CPU image grew beyond its retained length");
        }
        Ok(format!("{:x}", hasher.finalize()))
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
            RootPolicy::Production | RootPolicy::CpuProduction => {
                let mut current = open_absolute_directory(Path::new("/"))?;
                let identity =
                    validate_native_node(&current, PlannedKind::Directory, 0, None, false)?;
                nodes.push(HeldNode {
                    file: current,
                    identity,
                });
                let base = match policy {
                    RootPolicy::CpuProduction => crate::macos_worker_layout::CPU_ROOT,
                    _ => PACKS_ROOT,
                };
                let expected = Path::new(base)
                    .components()
                    .chain(pack_root.strip_prefix(base)?.components());
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
pub(crate) use platform::{ProtectedCpuAuthority, ProtectedPackAuthority};

#[cfg(all(test, target_os = "macos"))]
mod cpu_tests {
    use sha2::{Digest, Sha256};
    use std::fs;
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;
    use crate::macos_worker_layout::CPU_WORKER_NAME;

    static NEXT_ROOT: AtomicU64 = AtomicU64::new(0);

    struct CpuFixture {
        root: PathBuf,
        digest: String,
    }

    impl CpuFixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "scribe-protected-cpu-{}-{}",
                std::process::id(),
                NEXT_ROOT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&root).unwrap();
            fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
            let bytes = b"known CPU image";
            fs::write(root.join(CPU_WORKER_NAME), bytes).unwrap();
            fs::set_permissions(
                root.join(CPU_WORKER_NAME),
                fs::Permissions::from_mode(0o700),
            )
            .unwrap();
            Self {
                root,
                digest: format!("{:x}", Sha256::digest(bytes)),
            }
        }
        fn open(&self) -> Result<ProtectedCpuAuthority> {
            ProtectedCpuAuthority::open_fixture(&self.root, &self.digest)
        }
    }

    impl Drop for CpuFixture {
        fn drop(&mut self) {
            if let Err(error) = fs::remove_dir_all(&self.root) {
                assert_eq!(
                    error.kind(),
                    std::io::ErrorKind::NotFound,
                    "remove private CPU fixture: {error}"
                );
            }
        }
    }

    #[test]
    fn macos_protected_cpu_authority_retains_the_exact_readable_and_executable_image() {
        use std::os::unix::fs::MetadataExt;
        let fixture = CpuFixture::new();
        let authority = fixture.open().unwrap();
        authority.recheck().unwrap();
        let readable = authority.readable_executable_fd().metadata().unwrap();
        let executable = authority.executable_fd().metadata().unwrap();
        assert_eq!(
            (readable.dev(), readable.ino()),
            (executable.dev(), executable.ino())
        );
        assert_eq!(authority.root(), fixture.root);
        assert_eq!(
            authority.protected_executable_path().to_bytes(),
            fixture
                .root
                .join(CPU_WORKER_NAME)
                .as_os_str()
                .as_encoded_bytes()
        );
    }

    #[test]
    fn macos_protected_cpu_authority_rejects_missing_wrong_digest_and_unexpected_inventory() {
        let fixture = CpuFixture::new();
        assert!(ProtectedCpuAuthority::open_fixture(&fixture.root, "").is_err());
        assert!(ProtectedCpuAuthority::open_fixture(&fixture.root, &"ff".repeat(32)).is_err());
        let authority = fixture.open().unwrap();
        fs::write(fixture.root.join("extra.dylib"), b"unexpected").unwrap();
        assert!(authority.recheck().is_err());
        assert!(fixture.open().is_err());
        fs::remove_file(fixture.root.join("extra.dylib")).unwrap();
        fs::remove_file(fixture.root.join(CPU_WORKER_NAME)).unwrap();
        assert!(fixture.open().is_err());
    }

    #[test]
    fn macos_protected_cpu_authority_rejects_same_length_tampering_and_same_bytes_replacement() {
        let fixture = CpuFixture::new();
        let image = fixture.root.join(CPU_WORKER_NAME);
        let authority = fixture.open().unwrap();
        fs::write(&image, b"wrong CPU image").unwrap();
        assert!(authority.recheck().is_err());
        fs::write(&image, b"known CPU image").unwrap();
        authority.recheck().unwrap();
        let replacement = fixture.root.join("replacement");
        fs::write(&replacement, b"known CPU image").unwrap();
        fs::set_permissions(&replacement, fs::Permissions::from_mode(0o700)).unwrap();
        fs::rename(&replacement, &image).unwrap();
        assert!(authority.recheck().is_err());
        fixture.open().unwrap(); // Same authorized bytes may have a fresh authority.
    }

    #[test]
    fn macos_protected_cpu_authority_rejects_links_and_mutable_or_nonexecutable_nodes() {
        let fixture = CpuFixture::new();
        let image = fixture.root.join(CPU_WORKER_NAME);
        for mode in [0o722, 0o600, 0o4700] {
            fs::set_permissions(&image, fs::Permissions::from_mode(mode)).unwrap();
            assert!(fixture.open().is_err());
        }
        fs::set_permissions(&image, fs::Permissions::from_mode(0o700)).unwrap();
        fs::set_permissions(&fixture.root, fs::Permissions::from_mode(0o770)).unwrap();
        assert!(fixture.open().is_err());
        fs::set_permissions(&fixture.root, fs::Permissions::from_mode(0o700)).unwrap();
        let alias = CpuFixture::new();
        fs::remove_file(alias.root.join(CPU_WORKER_NAME)).unwrap();
        fs::hard_link(&image, alias.root.join(CPU_WORKER_NAME)).unwrap();
        assert!(fixture.open().is_err());
        fs::remove_file(alias.root.join(CPU_WORKER_NAME)).unwrap();
        fs::remove_file(&image).unwrap();
        symlink("/usr/bin/true", &image).unwrap();
        assert!(fixture.open().is_err());
    }

    #[test]
    fn macos_protected_cpu_authority_rejects_replaced_directory_without_adopting_it() {
        let fixture = CpuFixture::new();
        let authority = fixture.open().unwrap();
        let old = fixture.root.with_extension("retained");
        fs::rename(&fixture.root, &old).unwrap();
        let retained = CpuFixture {
            root: old,
            digest: fixture.digest.clone(),
        };
        fs::create_dir(&fixture.root).unwrap();
        fs::set_permissions(&fixture.root, fs::Permissions::from_mode(0o700)).unwrap();
        fs::copy(
            retained.root.join(CPU_WORKER_NAME),
            fixture.root.join(CPU_WORKER_NAME),
        )
        .unwrap();
        fs::set_permissions(
            fixture.root.join(CPU_WORKER_NAME),
            fs::Permissions::from_mode(0o700),
        )
        .unwrap();
        assert!(authority.recheck().is_err());
    }
}

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
    fn protected_pack_fixed_root_mapping_is_fail_closed() {
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
        use crate::macos_worker_fs::{ACL_EXTENDED_ALLOW, ACL_WRITE_DATA};

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
