//! Read-only Linux GPU release-security epoch admission.
//!
//! This module consumes an epoch only after a future caller has authenticated
//! the candidate pack. It never authenticates a pack, advances the floor, or
//! creates, repairs, or removes the authority file.

use std::fmt;

const AUTHORITY_PATH: &str = "/var/lib/scribe/gpu-release-security-epoch-v1";
const PAYLOAD_PREFIX: &[u8] =
    b"scribe-linux-gpu-release-security-epoch-v1:x86_64-unknown-linux-gnu:";
const EPOCH_DECIMAL_WIDTH: usize = 20;
const AUTHORITY_BYTES: usize = PAYLOAD_PREFIX.len() + EPOCH_DECIMAL_WIDTH + 1;
const MAX_RELEASE_SECURITY_EPOCH: u64 = 9_007_199_254_740_991;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LinuxReleaseEpochError {
    UnsupportedPlatform,
    InvalidCandidate,
    AuthorityUnavailable,
    UnsafeAuthority,
    CorruptAuthority,
    AuthorityChanged,
    Downgrade { candidate: u64, floor: u64 },
    Uncommitted { candidate: u64, floor: u64 },
}

impl fmt::Display for LinuxReleaseEpochError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::UnsupportedPlatform => "Linux GPU release epoch authority is unsupported",
            Self::InvalidCandidate => "Linux GPU release epoch candidate is invalid",
            Self::AuthorityUnavailable => "Linux GPU release epoch authority is unavailable",
            Self::UnsafeAuthority => "Linux GPU release epoch authority metadata is unsafe",
            Self::CorruptAuthority => "Linux GPU release epoch authority is corrupt",
            Self::AuthorityChanged => "Linux GPU release epoch authority changed",
            Self::Downgrade { .. } => "Linux GPU release epoch rejected a downgrade",
            Self::Uncommitted { .. } => "Linux GPU release epoch is not committed",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for LinuxReleaseEpochError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum NodeKind {
    Directory,
    RegularFile,
    Other,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ObservedMetadata {
    kind: NodeKind,
    device: u64,
    inode: u64,
    owner: u32,
    group: u32,
    mode: u32,
    links: u64,
    length: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OwnerPolicy {
    Root,
    #[cfg(test)]
    Fixture {
        owner: u32,
        group: u32,
    },
}

impl OwnerPolicy {
    fn accepts(self, metadata: ObservedMetadata) -> bool {
        match self {
            Self::Root => metadata.owner == 0 && metadata.group == 0,
            #[cfg(test)]
            Self::Fixture { owner, group } => metadata.owner == owner && metadata.group == group,
        }
    }
}

fn validate_candidate(candidate: u64) -> Result<u64, LinuxReleaseEpochError> {
    (1..=MAX_RELEASE_SECURITY_EPOCH)
        .contains(&candidate)
        .then_some(candidate)
        .ok_or(LinuxReleaseEpochError::InvalidCandidate)
}

fn parse_floor(bytes: &[u8]) -> Result<u64, LinuxReleaseEpochError> {
    if bytes.len() != AUTHORITY_BYTES
        || !bytes.starts_with(PAYLOAD_PREFIX)
        || bytes.last() != Some(&b'\n')
    {
        return Err(LinuxReleaseEpochError::CorruptAuthority);
    }
    let digits = &bytes[PAYLOAD_PREFIX.len()..PAYLOAD_PREFIX.len() + EPOCH_DECIMAL_WIDTH];
    if !digits.iter().all(u8::is_ascii_digit) {
        return Err(LinuxReleaseEpochError::CorruptAuthority);
    }
    let floor = digits.iter().try_fold(0_u64, |value, digit| {
        value.checked_mul(10)?.checked_add(u64::from(*digit - b'0'))
    });
    floor
        .filter(|floor| (1..=MAX_RELEASE_SECURITY_EPOCH).contains(floor))
        .ok_or(LinuxReleaseEpochError::CorruptAuthority)
}

fn require_committed_candidate(candidate: u64, floor: u64) -> Result<(), LinuxReleaseEpochError> {
    validate_candidate(candidate)?;
    match candidate.cmp(&floor) {
        std::cmp::Ordering::Less => Err(LinuxReleaseEpochError::Downgrade { candidate, floor }),
        std::cmp::Ordering::Equal => Ok(()),
        std::cmp::Ordering::Greater => {
            Err(LinuxReleaseEpochError::Uncommitted { candidate, floor })
        }
    }
}

fn validate_owner_and_mode(
    metadata: ObservedMetadata,
    owner_policy: OwnerPolicy,
) -> Result<(), LinuxReleaseEpochError> {
    if !owner_policy.accepts(metadata) || metadata.mode & 0o022 != 0 {
        return Err(LinuxReleaseEpochError::UnsafeAuthority);
    }
    Ok(())
}

fn validate_ancestor_metadata(
    metadata: ObservedMetadata,
    owner_policy: OwnerPolicy,
) -> Result<(), LinuxReleaseEpochError> {
    if metadata.kind != NodeKind::Directory {
        return Err(LinuxReleaseEpochError::UnsafeAuthority);
    }
    validate_owner_and_mode(metadata, owner_policy)
}

fn validate_floor_metadata(
    metadata: ObservedMetadata,
    owner_policy: OwnerPolicy,
) -> Result<(), LinuxReleaseEpochError> {
    if metadata.kind != NodeKind::RegularFile || metadata.links != 1 {
        return Err(LinuxReleaseEpochError::UnsafeAuthority);
    }
    validate_owner_and_mode(metadata, owner_policy)?;
    if metadata.length != AUTHORITY_BYTES as u64 {
        return Err(LinuxReleaseEpochError::CorruptAuthority);
    }
    Ok(())
}

/// A descriptor-retaining admission snapshot for one already-authenticated
/// candidate epoch. This is not an updater lock or pack-authentication proof.
pub(crate) struct LinuxReleaseEpochLease {
    #[cfg(all(target_os = "linux", target_arch = "x86_64", target_env = "gnu"))]
    inner: linux::LeaseInner,
}

impl LinuxReleaseEpochLease {
    pub(crate) fn open_production(
        authenticated_candidate_epoch: u64,
    ) -> Result<Self, LinuxReleaseEpochError> {
        #[cfg(all(target_os = "linux", target_arch = "x86_64", target_env = "gnu"))]
        {
            linux::LeaseInner::open_production(authenticated_candidate_epoch)
                .map(|inner| Self { inner })
        }
        #[cfg(not(all(target_os = "linux", target_arch = "x86_64", target_env = "gnu")))]
        {
            let _ = authenticated_candidate_epoch;
            Err(LinuxReleaseEpochError::UnsupportedPlatform)
        }
    }

    pub(crate) fn recheck(&self) -> Result<(), LinuxReleaseEpochError> {
        #[cfg(all(target_os = "linux", target_arch = "x86_64", target_env = "gnu"))]
        {
            self.inner.recheck()
        }
        #[cfg(not(all(target_os = "linux", target_arch = "x86_64", target_env = "gnu")))]
        {
            Err(LinuxReleaseEpochError::UnsupportedPlatform)
        }
    }
}

#[cfg(all(target_os = "linux", target_arch = "x86_64", target_env = "gnu"))]
mod linux {
    use std::ffi::{CStr, CString, c_long};
    use std::fs::{File, Metadata};
    use std::io::ErrorKind;
    use std::mem::size_of;
    use std::os::fd::{AsRawFd, FromRawFd, RawFd};
    use std::os::unix::fs::{FileExt, MetadataExt};

    use super::{
        AUTHORITY_BYTES, LinuxReleaseEpochError, NodeKind, ObservedMetadata, OwnerPolicy,
        require_committed_candidate, validate_ancestor_metadata, validate_candidate,
        validate_floor_metadata,
    };

    const AUTHORITY_COMPONENTS: &[&CStr] = &[c"var", c"lib", c"scribe"];
    const AUTHORITY_FILE_NAME: &CStr = c"gpu-release-security-epoch-v1";
    const RESOLVE_NO_MAGICLINKS: u64 = 0x02;
    const RESOLVE_NO_SYMLINKS: u64 = 0x04;
    const RESOLVE_BENEATH: u64 = 0x08;
    const OPEN_RESOLVE: u64 = RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS | RESOLVE_NO_MAGICLINKS;
    const O_RDONLY: u64 = 0;
    const O_NONBLOCK: u64 = 0o4000;
    const O_CLOEXEC: u64 = 0o2000000;
    const O_DIRECTORY: u64 = 0o200000;
    const O_NOFOLLOW: u64 = 0o400000;
    const SYS_OPENAT2: c_long = 437;

    #[repr(C)]
    struct OpenHow {
        flags: u64,
        mode: u64,
        resolve: u64,
    }

    unsafe extern "C" {
        fn syscall(number: c_long, ...) -> c_long;
        #[cfg(test)]
        fn mkfifo(path: *const std::ffi::c_char, mode: u32) -> std::ffi::c_int;
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    struct Identity {
        device: u64,
        inode: u64,
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    struct SafeMetadata {
        kind: NodeKind,
        owner: u32,
        group: u32,
        mode: u32,
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    struct DirectorySnapshot {
        identity: Identity,
        safe: SafeMetadata,
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    struct FloorSnapshot {
        identity: Identity,
        safe: SafeMetadata,
        links: u64,
        length: u64,
    }

    impl DirectorySnapshot {
        fn from_observed(metadata: ObservedMetadata) -> Self {
            Self {
                identity: Identity {
                    device: metadata.device,
                    inode: metadata.inode,
                },
                safe: SafeMetadata {
                    kind: metadata.kind,
                    owner: metadata.owner,
                    group: metadata.group,
                    mode: metadata.mode,
                },
            }
        }
    }

    impl FloorSnapshot {
        fn from_observed(metadata: ObservedMetadata) -> Self {
            Self {
                identity: Identity {
                    device: metadata.device,
                    inode: metadata.inode,
                },
                safe: SafeMetadata {
                    kind: metadata.kind,
                    owner: metadata.owner,
                    group: metadata.group,
                    mode: metadata.mode,
                },
                links: metadata.links,
                length: metadata.length,
            }
        }
    }

    struct RetainedDirectory {
        descriptor: File,
        snapshot: DirectorySnapshot,
    }

    struct RetainedFloor {
        descriptor: File,
        snapshot: FloorSnapshot,
    }

    pub(super) struct LeaseInner {
        components: Vec<CString>,
        file_name: CString,
        owner_policy: OwnerPolicy,
        directories: Vec<RetainedDirectory>,
        floor: RetainedFloor,
        floor_bytes: Vec<u8>,
        candidate: u64,
    }

    #[derive(Clone, Copy)]
    enum Phase {
        Initial,
        Recheck,
    }

    impl Phase {
        fn unavailable(self) -> LinuxReleaseEpochError {
            match self {
                Self::Initial => LinuxReleaseEpochError::AuthorityUnavailable,
                Self::Recheck => LinuxReleaseEpochError::AuthorityChanged,
            }
        }

        fn validation(self, error: LinuxReleaseEpochError) -> LinuxReleaseEpochError {
            match self {
                Self::Initial => error,
                Self::Recheck => LinuxReleaseEpochError::AuthorityChanged,
            }
        }
    }

    #[cfg(test)]
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum RecheckStep {
        AfterRetainedRead,
        AfterFirstRebind,
    }

    impl LeaseInner {
        pub(super) fn open_production(
            authenticated_candidate_epoch: u64,
        ) -> Result<Self, LinuxReleaseEpochError> {
            validate_candidate(authenticated_candidate_epoch)?;
            let anchor =
                File::open("/").map_err(|_| LinuxReleaseEpochError::AuthorityUnavailable)?;
            let components = AUTHORITY_COMPONENTS
                .iter()
                .map(|component| {
                    CString::new(component.to_bytes()).expect("static authority component")
                })
                .collect();
            Self::open_from_anchor(
                anchor,
                components,
                CString::new(AUTHORITY_FILE_NAME.to_bytes()).expect("static authority file name"),
                OwnerPolicy::Root,
                authenticated_candidate_epoch,
            )
        }

        fn open_from_anchor(
            anchor: File,
            components: Vec<CString>,
            file_name: CString,
            owner_policy: OwnerPolicy,
            candidate: u64,
        ) -> Result<Self, LinuxReleaseEpochError> {
            validate_candidate(candidate)?;
            let (directories, floor, floor_bytes) = open_bound(
                &anchor,
                &components,
                &file_name,
                owner_policy,
                Phase::Initial,
            )?;
            let parsed_floor = super::parse_floor(&floor_bytes)?;
            require_committed_candidate(candidate, parsed_floor)?;
            let lease = Self {
                components,
                file_name,
                owner_policy,
                directories,
                floor,
                floor_bytes,
                candidate,
            };
            lease.recheck()?;
            Ok(lease)
        }

        pub(super) fn recheck(&self) -> Result<(), LinuxReleaseEpochError> {
            self.recheck_inner(|_| {})
        }

        fn recheck_inner(
            &self,
            mut checkpoint: impl FnMut(RecheckCheckpoint),
        ) -> Result<(), LinuxReleaseEpochError> {
            self.recheck_retained()?;
            checkpoint(RecheckCheckpoint::AfterRetainedRead);

            let anchor = &self
                .directories
                .first()
                .ok_or(LinuxReleaseEpochError::AuthorityChanged)?
                .descriptor;
            let first = open_bound(
                anchor,
                &self.components,
                &self.file_name,
                self.owner_policy,
                Phase::Recheck,
            )?;
            self.require_same_binding(&first)?;
            checkpoint(RecheckCheckpoint::AfterFirstRebind);

            self.recheck_retained()?;
            let final_binding = open_bound(
                anchor,
                &self.components,
                &self.file_name,
                self.owner_policy,
                Phase::Recheck,
            )?;
            self.require_same_binding(&final_binding)?;
            self.recheck_retained()
        }

        fn recheck_retained(&self) -> Result<(), LinuxReleaseEpochError> {
            for retained in &self.directories {
                let observed = observed_file(&retained.descriptor, Phase::Recheck)?;
                validate_ancestor_metadata(observed, self.owner_policy)
                    .map_err(|_| LinuxReleaseEpochError::AuthorityChanged)?;
                if DirectorySnapshot::from_observed(observed) != retained.snapshot {
                    return Err(LinuxReleaseEpochError::AuthorityChanged);
                }
            }

            let observed = observed_file(&self.floor.descriptor, Phase::Recheck)?;
            validate_floor_metadata(observed, self.owner_policy)
                .map_err(|_| LinuxReleaseEpochError::AuthorityChanged)?;
            if FloorSnapshot::from_observed(observed) != self.floor.snapshot {
                return Err(LinuxReleaseEpochError::AuthorityChanged);
            }
            let bytes =
                read_exact_floor(&self.floor.descriptor, self.floor.snapshot, Phase::Recheck)?;
            if bytes != self.floor_bytes {
                return Err(LinuxReleaseEpochError::AuthorityChanged);
            }
            let floor =
                super::parse_floor(&bytes).map_err(|_| LinuxReleaseEpochError::AuthorityChanged)?;
            require_committed_candidate(self.candidate, floor)
        }

        fn require_same_binding(
            &self,
            rebound: &(Vec<RetainedDirectory>, RetainedFloor, Vec<u8>),
        ) -> Result<(), LinuxReleaseEpochError> {
            let (directories, floor, floor_bytes) = rebound;
            if directories.len() != self.directories.len()
                || directories
                    .iter()
                    .zip(&self.directories)
                    .any(|(left, right)| left.snapshot != right.snapshot)
                || floor.snapshot != self.floor.snapshot
                || floor_bytes != &self.floor_bytes
            {
                return Err(LinuxReleaseEpochError::AuthorityChanged);
            }
            let floor = super::parse_floor(floor_bytes)
                .map_err(|_| LinuxReleaseEpochError::AuthorityChanged)?;
            require_committed_candidate(self.candidate, floor)
        }

        #[cfg(test)]
        fn open_test_fixture(
            anchor_path: &std::path::Path,
            candidate: u64,
        ) -> Result<Self, LinuxReleaseEpochError> {
            let anchor = File::open(anchor_path)
                .map_err(|_| LinuxReleaseEpochError::AuthorityUnavailable)?;
            let metadata = anchor
                .metadata()
                .map_err(|_| LinuxReleaseEpochError::AuthorityUnavailable)?;
            Self::open_from_anchor(
                anchor,
                AUTHORITY_COMPONENTS
                    .iter()
                    .map(|component| {
                        CString::new(component.to_bytes()).expect("static authority component")
                    })
                    .collect(),
                CString::new(AUTHORITY_FILE_NAME.to_bytes()).expect("static authority file name"),
                OwnerPolicy::Fixture {
                    owner: metadata.uid(),
                    group: metadata.gid(),
                },
                candidate,
            )
        }

        #[cfg(test)]
        fn open_test_fixture_with_owner(
            anchor_path: &std::path::Path,
            candidate: u64,
            owner: u32,
            group: u32,
        ) -> Result<Self, LinuxReleaseEpochError> {
            let anchor = File::open(anchor_path)
                .map_err(|_| LinuxReleaseEpochError::AuthorityUnavailable)?;
            Self::open_from_anchor(
                anchor,
                AUTHORITY_COMPONENTS
                    .iter()
                    .map(|component| {
                        CString::new(component.to_bytes()).expect("static authority component")
                    })
                    .collect(),
                CString::new(AUTHORITY_FILE_NAME.to_bytes()).expect("static authority file name"),
                OwnerPolicy::Fixture { owner, group },
                candidate,
            )
        }

        #[cfg(test)]
        fn recheck_with_hook(
            &self,
            mut hook: impl FnMut(RecheckStep),
        ) -> Result<(), LinuxReleaseEpochError> {
            self.recheck_inner(|checkpoint| match checkpoint {
                RecheckCheckpoint::AfterRetainedRead => hook(RecheckStep::AfterRetainedRead),
                RecheckCheckpoint::AfterFirstRebind => hook(RecheckStep::AfterFirstRebind),
            })
        }
    }

    #[derive(Clone, Copy)]
    enum RecheckCheckpoint {
        AfterRetainedRead,
        AfterFirstRebind,
    }

    fn open_bound(
        anchor: &File,
        components: &[CString],
        file_name: &CStr,
        owner_policy: OwnerPolicy,
        phase: Phase,
    ) -> Result<(Vec<RetainedDirectory>, RetainedFloor, Vec<u8>), LinuxReleaseEpochError> {
        let mut directories = Vec::with_capacity(components.len() + 1);
        let mut current = anchor.try_clone().map_err(|_| phase.unavailable())?;
        let observed = observed_file(&current, phase)?;
        validate_ancestor_metadata(observed, owner_policy)
            .map_err(|error| phase.validation(error))?;
        directories.push(RetainedDirectory {
            descriptor: current.try_clone().map_err(|_| phase.unavailable())?,
            snapshot: DirectorySnapshot::from_observed(observed),
        });

        for component in components {
            current = openat2_directory(current.as_raw_fd(), component)
                .map_err(|_| phase.unavailable())?;
            let observed = observed_file(&current, phase)?;
            validate_ancestor_metadata(observed, owner_policy)
                .map_err(|error| phase.validation(error))?;
            directories.push(RetainedDirectory {
                descriptor: current.try_clone().map_err(|_| phase.unavailable())?,
                snapshot: DirectorySnapshot::from_observed(observed),
            });
        }

        let descriptor =
            openat2_floor(current.as_raw_fd(), file_name).map_err(|_| phase.unavailable())?;
        let observed = observed_file(&descriptor, phase)?;
        validate_floor_metadata(observed, owner_policy).map_err(|error| phase.validation(error))?;
        let snapshot = FloorSnapshot::from_observed(observed);
        let bytes = read_exact_floor(&descriptor, snapshot, phase)?;
        Ok((
            directories,
            RetainedFloor {
                descriptor,
                snapshot,
            },
            bytes,
        ))
    }

    fn observed_file(
        file: &File,
        phase: Phase,
    ) -> Result<ObservedMetadata, LinuxReleaseEpochError> {
        file.metadata()
            .map(|metadata| observed_metadata(&metadata))
            .map_err(|_| phase.unavailable())
    }

    fn observed_metadata(metadata: &Metadata) -> ObservedMetadata {
        let kind = if metadata.is_dir() {
            NodeKind::Directory
        } else if metadata.is_file() {
            NodeKind::RegularFile
        } else {
            NodeKind::Other
        };
        ObservedMetadata {
            kind,
            device: metadata.dev(),
            inode: metadata.ino(),
            owner: metadata.uid(),
            group: metadata.gid(),
            mode: metadata.mode(),
            links: metadata.nlink(),
            length: metadata.len(),
        }
    }

    fn read_exact_floor(
        file: &File,
        expected: FloorSnapshot,
        phase: Phase,
    ) -> Result<Vec<u8>, LinuxReleaseEpochError> {
        let before = observed_file(file, phase)?;
        if FloorSnapshot::from_observed(before) != expected {
            return Err(phase.validation(LinuxReleaseEpochError::AuthorityChanged));
        }
        let mut bytes = vec![0_u8; AUTHORITY_BYTES + 1];
        let mut filled = 0;
        while filled < bytes.len() {
            match file.read_at(&mut bytes[filled..], filled as u64) {
                Ok(0) => break,
                Ok(read) => filled += read,
                Err(error) if error.kind() == ErrorKind::Interrupted => continue,
                Err(_) => return Err(phase.unavailable()),
            }
        }
        bytes.truncate(filled);
        if bytes.len() != AUTHORITY_BYTES {
            return Err(phase.validation(LinuxReleaseEpochError::CorruptAuthority));
        }
        let after = observed_file(file, phase)?;
        if FloorSnapshot::from_observed(after) != expected {
            return Err(phase.validation(LinuxReleaseEpochError::AuthorityChanged));
        }
        Ok(bytes)
    }

    fn openat2_directory(parent: RawFd, name: &CStr) -> std::io::Result<File> {
        openat2(
            parent,
            name,
            O_RDONLY | O_CLOEXEC | O_DIRECTORY | O_NOFOLLOW,
        )
    }

    fn openat2_floor(parent: RawFd, name: &CStr) -> std::io::Result<File> {
        openat2(parent, name, O_RDONLY | O_NONBLOCK | O_CLOEXEC | O_NOFOLLOW)
    }

    fn openat2(parent: RawFd, name: &CStr, flags: u64) -> std::io::Result<File> {
        let how = OpenHow {
            flags,
            mode: 0,
            resolve: OPEN_RESOLVE,
        };
        // SAFETY: `name` and `how` remain valid for this synchronous syscall.
        let descriptor = unsafe {
            syscall(
                SYS_OPENAT2,
                parent,
                name.as_ptr(),
                &how as *const OpenHow,
                size_of::<OpenHow>(),
            )
        };
        if descriptor < 0 {
            Err(std::io::Error::last_os_error())
        } else {
            // SAFETY: a successful openat2 returns one newly owned descriptor.
            Ok(unsafe { File::from_raw_fd(descriptor as RawFd) })
        }
    }

    #[cfg(test)]
    mod tests {
        use std::fs;
        use std::io::Write;
        use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
        use std::os::unix::net::UnixListener;
        use std::path::{Path, PathBuf};
        use std::sync::{
            Arc, Barrier,
            atomic::{AtomicU64, Ordering},
        };

        use super::*;

        static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

        fn reserve_exclusive_fixture_directory(path: &Path) -> std::io::Result<()> {
            fs::create_dir(path)
        }

        struct OwnedFixtureRoot {
            path: PathBuf,
        }

        impl OwnedFixtureRoot {
            fn reserve() -> Self {
                loop {
                    let sequence = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
                    let candidate = std::env::temp_dir().join(format!(
                        "scribe-linux-release-epoch-{}-{sequence}",
                        std::process::id()
                    ));
                    match reserve_exclusive_fixture_directory(&candidate) {
                        Ok(()) => return Self { path: candidate },
                        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                        Err(error) => panic!("create isolated epoch fixture root: {error}"),
                    }
                }
            }

            fn path(&self) -> &Path {
                &self.path
            }
        }

        impl Drop for OwnedFixtureRoot {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.path);
            }
        }

        struct Fixture {
            _root: OwnedFixtureRoot,
            anchor: PathBuf,
            authority_dir: PathBuf,
            floor: PathBuf,
            epoch: u64,
        }

        impl Fixture {
            fn new(epoch: u64) -> Self {
                let root = OwnedFixtureRoot::reserve();
                fs::set_permissions(root.path(), fs::Permissions::from_mode(0o755)).unwrap();
                let anchor = root.path().join("anchor");
                let authority_dir = anchor.join("var/lib/scribe");
                fs::create_dir_all(&authority_dir).unwrap();
                for directory in [
                    root.path(),
                    &anchor,
                    &anchor.join("var"),
                    &anchor.join("var/lib"),
                    &authority_dir,
                ] {
                    fs::set_permissions(directory, fs::Permissions::from_mode(0o755)).unwrap();
                }
                let floor = authority_dir.join("gpu-release-security-epoch-v1");
                write_floor(&floor, epoch);
                Self {
                    _root: root,
                    anchor,
                    authority_dir,
                    floor,
                    epoch,
                }
            }

            fn open(&self) -> Result<LeaseInner, LinuxReleaseEpochError> {
                LeaseInner::open_test_fixture(&self.anchor, self.epoch)
            }

            fn recreate_authority_dir(&self) {
                fs::create_dir(&self.authority_dir).unwrap();
                fs::set_permissions(&self.authority_dir, fs::Permissions::from_mode(0o755))
                    .unwrap();
                write_floor(&self.floor, self.epoch);
            }
        }

        fn write_floor(path: &Path, epoch: u64) {
            fs::write(path, super::super::tests::canonical_bytes(epoch)).unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o644)).unwrap();
        }

        #[test]
        fn missing_authority_is_rejected() {
            let fixture = Fixture::new(7);
            fs::remove_file(&fixture.floor).unwrap();
            assert_eq!(
                fixture.open().err(),
                Some(LinuxReleaseEpochError::AuthorityUnavailable)
            );
        }

        #[test]
        fn fixture_reservation_rejects_directory_and_symlink_collisions_without_mutation() {
            let root = OwnedFixtureRoot::reserve();

            let existing = root.path().join("existing");
            reserve_exclusive_fixture_directory(&existing).unwrap();
            let existing_sentinel = existing.join("sentinel");
            fs::write(&existing_sentinel, b"existing-directory-sentinel").unwrap();
            assert_eq!(
                reserve_exclusive_fixture_directory(&existing)
                    .unwrap_err()
                    .kind(),
                std::io::ErrorKind::AlreadyExists
            );
            assert_eq!(
                fs::read(&existing_sentinel).unwrap(),
                b"existing-directory-sentinel"
            );

            let target = root.path().join("symlink-target");
            reserve_exclusive_fixture_directory(&target).unwrap();
            let target_sentinel = target.join("sentinel");
            fs::write(&target_sentinel, b"symlink-target-sentinel").unwrap();
            let collision = root.path().join("symlink-collision");
            symlink(&target, &collision).unwrap();
            assert_eq!(
                reserve_exclusive_fixture_directory(&collision)
                    .unwrap_err()
                    .kind(),
                std::io::ErrorKind::AlreadyExists
            );
            assert!(
                fs::symlink_metadata(&collision)
                    .unwrap()
                    .file_type()
                    .is_symlink()
            );
            assert_eq!(
                fs::read(&target_sentinel).unwrap(),
                b"symlink-target-sentinel"
            );
        }

        #[test]
        fn symlink_file_and_ancestor_are_rejected() {
            let fixture = Fixture::new(7);
            let real_floor = fixture.authority_dir.join("real-floor");
            fs::rename(&fixture.floor, &real_floor).unwrap();
            symlink(&real_floor, &fixture.floor).unwrap();
            assert!(fixture.open().is_err());

            let fixture = Fixture::new(7);
            let var = fixture.anchor.join("var");
            let real_var = fixture.anchor.join("real-var");
            fs::rename(&var, &real_var).unwrap();
            symlink(&real_var, &var).unwrap();
            assert!(fixture.open().is_err());
        }

        #[test]
        fn hardlinked_floor_is_rejected() {
            let fixture = Fixture::new(7);
            fs::hard_link(&fixture.floor, fixture.authority_dir.join("second-link")).unwrap();
            assert_eq!(
                fixture.open().err(),
                Some(LinuxReleaseEpochError::UnsafeAuthority)
            );
        }

        #[test]
        fn unsafe_directory_and_file_modes_are_rejected() {
            let fixture = Fixture::new(7);
            fs::set_permissions(&fixture.authority_dir, fs::Permissions::from_mode(0o777)).unwrap();
            assert_eq!(
                fixture.open().err(),
                Some(LinuxReleaseEpochError::UnsafeAuthority)
            );

            let fixture = Fixture::new(7);
            fs::set_permissions(&fixture.floor, fs::Permissions::from_mode(0o666)).unwrap();
            assert_eq!(
                fixture.open().err(),
                Some(LinuxReleaseEpochError::UnsafeAuthority)
            );
        }

        #[test]
        fn fixture_owner_policy_is_enforced_without_chown() {
            let fixture = Fixture::new(7);
            let metadata = fs::metadata(&fixture.anchor).unwrap();
            let wrong_owner = metadata.uid().wrapping_add(1);
            assert_eq!(
                LeaseInner::open_test_fixture_with_owner(
                    &fixture.anchor,
                    fixture.epoch,
                    wrong_owner,
                    metadata.gid(),
                )
                .err(),
                Some(LinuxReleaseEpochError::UnsafeAuthority)
            );
        }

        #[test]
        fn directory_fifo_and_socket_floor_nodes_are_rejected_without_blocking() {
            let fixture = Fixture::new(7);
            fs::remove_file(&fixture.floor).unwrap();
            fs::create_dir(&fixture.floor).unwrap();
            assert!(fixture.open().is_err());

            let fixture = Fixture::new(7);
            fs::remove_file(&fixture.floor).unwrap();
            let path = CString::new(fixture.floor.as_os_str().as_encoded_bytes()).unwrap();
            // SAFETY: `path` is a live NUL-terminated path and the fixture owns it.
            assert_eq!(unsafe { mkfifo(path.as_ptr(), 0o600) }, 0);
            assert!(fixture.open().is_err());

            let fixture = Fixture::new(7);
            fs::remove_file(&fixture.floor).unwrap();
            let _listener = UnixListener::bind(&fixture.floor).unwrap();
            assert!(fixture.open().is_err());
        }

        #[test]
        fn mutation_truncate_and_growth_are_rejected() {
            let fixture = Fixture::new(7);
            let lease = fixture.open().unwrap();
            write_floor(&fixture.floor, 8);
            assert_eq!(
                lease.recheck(),
                Err(LinuxReleaseEpochError::AuthorityChanged)
            );

            let fixture = Fixture::new(7);
            let lease = fixture.open().unwrap();
            fs::OpenOptions::new()
                .write(true)
                .open(&fixture.floor)
                .unwrap()
                .set_len(0)
                .unwrap();
            assert_eq!(
                lease.recheck(),
                Err(LinuxReleaseEpochError::AuthorityChanged)
            );

            let fixture = Fixture::new(7);
            let lease = fixture.open().unwrap();
            fs::OpenOptions::new()
                .append(true)
                .open(&fixture.floor)
                .unwrap()
                .write_all(b"x")
                .unwrap();
            assert_eq!(
                lease.recheck(),
                Err(LinuxReleaseEpochError::AuthorityChanged)
            );
        }

        #[test]
        fn same_content_inode_replacement_is_rejected() {
            let fixture = Fixture::new(7);
            let lease = fixture.open().unwrap();
            fs::rename(
                &fixture.floor,
                fixture.authority_dir.join("displaced-floor"),
            )
            .unwrap();
            write_floor(&fixture.floor, fixture.epoch);
            assert_eq!(
                lease.recheck(),
                Err(LinuxReleaseEpochError::AuthorityChanged)
            );
        }

        #[test]
        fn ancestor_replacement_is_rejected() {
            let fixture = Fixture::new(7);
            let lease = fixture.open().unwrap();
            let displaced = fixture.anchor.join("var/lib/displaced-scribe");
            fs::rename(&fixture.authority_dir, &displaced).unwrap();
            fixture.recreate_authority_dir();
            assert_eq!(
                lease.recheck(),
                Err(LinuxReleaseEpochError::AuthorityChanged)
            );
        }

        #[test]
        fn retained_descriptor_metadata_change_is_rejected() {
            let fixture = Fixture::new(7);
            let lease = fixture.open().unwrap();
            let displaced = fixture.authority_dir.join("displaced-floor");
            fs::rename(&fixture.floor, &displaced).unwrap();
            write_floor(&fixture.floor, fixture.epoch);
            fs::set_permissions(&displaced, fs::Permissions::from_mode(0o666)).unwrap();
            assert_eq!(
                lease.recheck(),
                Err(LinuxReleaseEpochError::AuthorityChanged)
            );
        }

        #[test]
        fn deterministic_change_after_retained_read_is_rejected() {
            let fixture = Fixture::new(7);
            let lease = fixture.open().unwrap();
            let mut changed = false;
            let result = lease.recheck_with_hook(|step| {
                if step == RecheckStep::AfterRetainedRead && !changed {
                    fs::rename(&fixture.floor, fixture.authority_dir.join("hook-displaced"))
                        .unwrap();
                    write_floor(&fixture.floor, fixture.epoch);
                    changed = true;
                }
            });
            assert_eq!(result, Err(LinuxReleaseEpochError::AuthorityChanged));
        }

        #[test]
        fn deterministic_change_after_first_rebind_is_rejected() {
            let fixture = Fixture::new(7);
            let lease = fixture.open().unwrap();
            let mut changed = false;
            let result = lease.recheck_with_hook(|step| {
                if step == RecheckStep::AfterFirstRebind && !changed {
                    let displaced = fixture.anchor.join("var/lib/hook-displaced-scribe");
                    fs::rename(&fixture.authority_dir, displaced).unwrap();
                    fixture.recreate_authority_dir();
                    changed = true;
                }
            });
            assert_eq!(result, Err(LinuxReleaseEpochError::AuthorityChanged));
        }

        #[test]
        fn concurrent_rechecks_use_independent_positional_reads() {
            const THREADS: usize = 8;
            const RECHECKS_PER_THREAD: usize = 64;

            let fixture = Fixture::new(7);
            let lease = Arc::new(fixture.open().unwrap());
            let barrier = Arc::new(Barrier::new(THREADS));
            let threads = (0..THREADS)
                .map(|_| {
                    let lease = Arc::clone(&lease);
                    let barrier = Arc::clone(&barrier);
                    std::thread::spawn(move || {
                        barrier.wait();
                        for _ in 0..RECHECKS_PER_THREAD {
                            lease.recheck()?;
                        }
                        Ok::<_, LinuxReleaseEpochError>(())
                    })
                })
                .collect::<Vec<_>>();

            for thread in threads {
                assert_eq!(thread.join().unwrap(), Ok(()));
            }

            write_floor(&fixture.floor, 8);
            assert_eq!(
                lease.recheck(),
                Err(LinuxReleaseEpochError::AuthorityChanged)
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(super) fn canonical_bytes(epoch: u64) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(AUTHORITY_BYTES);
        bytes.extend_from_slice(PAYLOAD_PREFIX);
        bytes.extend_from_slice(format!("{epoch:020}").as_bytes());
        bytes.push(b'\n');
        bytes
    }

    fn metadata(kind: NodeKind) -> ObservedMetadata {
        ObservedMetadata {
            kind,
            device: 1,
            inode: 2,
            owner: 0,
            group: 0,
            mode: 0o755,
            links: 1,
            length: AUTHORITY_BYTES as u64,
        }
    }

    #[test]
    fn canonical_floor_and_equal_candidate_are_accepted() {
        let bytes = canonical_bytes(42);
        assert_eq!(bytes.len(), AUTHORITY_BYTES);
        assert_eq!(parse_floor(&bytes), Ok(42));
        assert_eq!(require_committed_candidate(42, 42), Ok(()));
    }

    #[test]
    fn zero_floor_is_rejected() {
        assert_eq!(
            parse_floor(&canonical_bytes(0)),
            Err(LinuxReleaseEpochError::CorruptAuthority)
        );
    }

    #[test]
    fn maximum_floor_is_accepted() {
        assert_eq!(
            parse_floor(&canonical_bytes(MAX_RELEASE_SECURITY_EPOCH)),
            Ok(MAX_RELEASE_SECURITY_EPOCH)
        );
    }

    #[test]
    fn overflow_floor_is_rejected() {
        assert_eq!(
            parse_floor(&canonical_bytes(MAX_RELEASE_SECURITY_EPOCH + 1)),
            Err(LinuxReleaseEpochError::CorruptAuthority)
        );

        let mut bytes = PAYLOAD_PREFIX.to_vec();
        bytes.extend_from_slice(b"99999999999999999999\n");
        assert_eq!(
            parse_floor(&bytes),
            Err(LinuxReleaseEpochError::CorruptAuthority)
        );
    }

    #[test]
    fn malformed_digit_is_rejected() {
        let mut bytes = canonical_bytes(42);
        bytes[PAYLOAD_PREFIX.len() + 7] = b'x';
        assert_eq!(
            parse_floor(&bytes),
            Err(LinuxReleaseEpochError::CorruptAuthority)
        );
    }

    #[test]
    fn trailing_or_missing_bytes_are_rejected() {
        let mut non_lf_terminator = canonical_bytes(42);
        *non_lf_terminator.last_mut().unwrap() = b'\r';
        assert_eq!(
            parse_floor(&non_lf_terminator),
            Err(LinuxReleaseEpochError::CorruptAuthority)
        );

        let mut trailing = canonical_bytes(42);
        trailing.push(b'x');
        assert_eq!(
            parse_floor(&trailing),
            Err(LinuxReleaseEpochError::CorruptAuthority)
        );
        assert_eq!(
            parse_floor(&canonical_bytes(42)[..AUTHORITY_BYTES - 1]),
            Err(LinuxReleaseEpochError::CorruptAuthority)
        );
    }

    #[test]
    fn target_prefix_is_exact() {
        let mut wrong_target = canonical_bytes(42);
        let target = PAYLOAD_PREFIX
            .windows(b"x86_64".len())
            .position(|window| window == b"x86_64")
            .unwrap();
        wrong_target[target] = b'a';
        assert_eq!(
            parse_floor(&wrong_target),
            Err(LinuxReleaseEpochError::CorruptAuthority)
        );
    }

    #[test]
    fn candidate_below_floor_is_downgrade() {
        assert_eq!(
            require_committed_candidate(6, 7),
            Err(LinuxReleaseEpochError::Downgrade {
                candidate: 6,
                floor: 7,
            })
        );
    }

    #[test]
    fn candidate_above_floor_is_uncommitted() {
        assert_eq!(
            require_committed_candidate(8, 7),
            Err(LinuxReleaseEpochError::Uncommitted {
                candidate: 8,
                floor: 7,
            })
        );
    }

    #[test]
    fn candidate_bounds_are_enforced() {
        assert_eq!(
            require_committed_candidate(0, 1),
            Err(LinuxReleaseEpochError::InvalidCandidate)
        );
        assert_eq!(
            require_committed_candidate(MAX_RELEASE_SECURITY_EPOCH + 1, 1),
            Err(LinuxReleaseEpochError::InvalidCandidate)
        );
    }

    #[test]
    fn ancestor_owner_and_mode_rules_are_fail_closed() {
        assert_eq!(
            validate_ancestor_metadata(metadata(NodeKind::Directory), OwnerPolicy::Root),
            Ok(())
        );
        for changed in [
            ObservedMetadata {
                owner: 1,
                ..metadata(NodeKind::Directory)
            },
            ObservedMetadata {
                group: 1,
                ..metadata(NodeKind::Directory)
            },
            ObservedMetadata {
                mode: 0o775,
                ..metadata(NodeKind::Directory)
            },
            ObservedMetadata {
                mode: 0o757,
                ..metadata(NodeKind::Directory)
            },
            metadata(NodeKind::RegularFile),
        ] {
            assert_eq!(
                validate_ancestor_metadata(changed, OwnerPolicy::Root),
                Err(LinuxReleaseEpochError::UnsafeAuthority)
            );
        }
    }

    #[test]
    fn floor_type_link_owner_mode_and_size_rules_are_fail_closed() {
        assert_eq!(
            validate_floor_metadata(metadata(NodeKind::RegularFile), OwnerPolicy::Root),
            Ok(())
        );
        for changed in [
            metadata(NodeKind::Directory),
            metadata(NodeKind::Other),
            ObservedMetadata {
                links: 2,
                ..metadata(NodeKind::RegularFile)
            },
            ObservedMetadata {
                owner: 1,
                ..metadata(NodeKind::RegularFile)
            },
            ObservedMetadata {
                group: 1,
                ..metadata(NodeKind::RegularFile)
            },
            ObservedMetadata {
                mode: 0o664,
                ..metadata(NodeKind::RegularFile)
            },
            ObservedMetadata {
                mode: 0o646,
                ..metadata(NodeKind::RegularFile)
            },
        ] {
            assert_eq!(
                validate_floor_metadata(changed, OwnerPolicy::Root),
                Err(LinuxReleaseEpochError::UnsafeAuthority)
            );
        }
        for length in [AUTHORITY_BYTES as u64 - 1, AUTHORITY_BYTES as u64 + 1] {
            assert_eq!(
                validate_floor_metadata(
                    ObservedMetadata {
                        length,
                        ..metadata(NodeKind::RegularFile)
                    },
                    OwnerPolicy::Root,
                ),
                Err(LinuxReleaseEpochError::CorruptAuthority)
            );
        }
    }

    #[test]
    fn errors_are_path_free() {
        for error in [
            LinuxReleaseEpochError::UnsupportedPlatform,
            LinuxReleaseEpochError::InvalidCandidate,
            LinuxReleaseEpochError::AuthorityUnavailable,
            LinuxReleaseEpochError::UnsafeAuthority,
            LinuxReleaseEpochError::CorruptAuthority,
            LinuxReleaseEpochError::AuthorityChanged,
            LinuxReleaseEpochError::Downgrade {
                candidate: 1,
                floor: 2,
            },
            LinuxReleaseEpochError::Uncommitted {
                candidate: 2,
                floor: 1,
            },
        ] {
            assert!(!error.to_string().contains(AUTHORITY_PATH));
            assert!(!error.to_string().contains('/'));
        }
    }
}
