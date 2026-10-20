//! Darwin-only writer. The enclosing trusted installer supplies bootstrap trust.
//! No runtime-selected authority, permission repair, pruning, or root service.
use std::ffi::{CString, OsStr};
use std::fs::File;
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result, bail};

use crate::installer_fs::{
    checked_directory, checked_lock, has_errno, open_rw, owned_fd, require_mode, set_mode, unlink,
};
use crate::macos_worker_fs::{
    NodeIdentity, NodeKind, descriptor_stat, macos_directory_names, open_absolute_directory,
    openat_node, validate_native_node,
};
use crate::macos_worker_layout::{CPU_WORKER_NAME, embedded_cpu_digest, validate_cpu_size};
use crate::transaction::{Operations, PublishResult, copy_exact, verify_bytes};

mod bootstrap;

static STAGE_SEQUENCE: AtomicU64 = AtomicU64::new(0);
const LOCK_NAME: &str = ".installer.lock";

struct Anchor {
    name: String,
    file: File,
    identity: NodeIdentity,
}
struct Stage {
    name: String,
    file: File,
    identity: NodeIdentity,
    worker: Option<NodeIdentity>,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct InstalledIdentity {
    directory: NodeIdentity,
    worker: NodeIdentity,
}

pub(crate) struct NativeOperations {
    source: File,
    length: u64,
    digest: String,
    uid: libc::uid_t,
    anchors: Vec<Anchor>,
    lock: Option<(File, NodeIdentity)>,
    stage: Option<Stage>,
    unproven_stage: Option<String>,
    admitted: Option<InstalledIdentity>,
    published: bool,
    #[cfg(test)]
    fixture: bool,
}

impl NativeOperations {
    pub(crate) fn production(candidate: &Path) -> Result<Self> {
        // Must precede candidate opening and every protected-root mutation.
        if unsafe { libc::geteuid() } != 0 {
            bail!("CPU worker installation requires the trusted root installer");
        }
        let digest = embedded_cpu_digest()?.to_owned();
        if option_env!("SCRIBE_BUILD_REVISION").is_none() {
            bail!("installer has no compiled build identity");
        }
        if !candidate.is_absolute() {
            bail!("source worker must be an absolute candidate path");
        }
        let source = open_candidate(candidate)?;
        let length = candidate_size(&source)?;
        Ok(Self {
            source,
            length,
            digest,
            uid: 0,
            anchors: vec![],
            lock: None,
            stage: None,
            unproven_stage: None,
            admitted: None,
            published: false,
            #[cfg(test)]
            fixture: false,
        })
    }

    fn cpu(&self) -> Result<&File> {
        self.anchors
            .last()
            .map(|anchor| &anchor.file)
            .context("CPU root is not retained")
    }

    fn walk_protected_root(&mut self) -> Result<()> {
        #[cfg(test)]
        if self.fixture {
            return self.recheck_anchors();
        }
        let root = open_absolute_directory(Path::new("/"))?;
        let identity = checked_directory(&root, self.uid, None)?;
        self.anchors.push(Anchor {
            name: String::new(),
            file: root,
            identity,
        });
        for (name, may_create) in [
            ("Library", false),
            ("Application Support", false),
            ("Scribe", true),
            ("CPU", true),
        ] {
            let parent = self.cpu()?.as_raw_fd();
            let file = match openat_node(parent, OsStr::new(name), NodeKind::Directory) {
                Ok(file) => file,
                Err(error) if may_create && has_errno(&error, libc::ENOENT) => {
                    self.recheck_anchors()?;
                    bootstrap::ensure_fixed_ancestor(self.cpu()?, name, self.uid)?
                }
                Err(error) => return Err(error),
            };
            let identity = checked_directory(&file, self.uid, None)?;
            self.anchors.push(Anchor {
                name: name.to_owned(),
                file,
                identity,
            });
        }
        self.recheck_anchors()
    }

    fn recheck_anchors(&self) -> Result<()> {
        for (index, anchor) in self.anchors.iter().enumerate() {
            if checked_directory(&anchor.file, self.uid, None)? != anchor.identity {
                bail!("held installer ancestor changed identity");
            }
            let reopened = if index == 0 {
                #[cfg(test)]
                if self.fixture {
                    continue;
                }
                open_absolute_directory(Path::new("/"))?
            } else {
                openat_node(
                    self.anchors[index - 1].file.as_raw_fd(),
                    OsStr::new(&anchor.name),
                    NodeKind::Directory,
                )?
            };
            if checked_directory(&reopened, self.uid, None)? != anchor.identity {
                bail!("named installer ancestor changed identity");
            }
        }
        if let Some((lock, identity)) = &self.lock {
            let reopened = open_rw(self.cpu()?.as_raw_fd(), LOCK_NAME, false)?;
            if checked_lock(lock, self.uid)? != *identity
                || checked_lock(&reopened, self.uid)? != *identity
            {
                bail!("permanent installer lock identity changed");
            }
        }
        Ok(())
    }

    fn inspect(&self, directory: &File) -> Result<InstalledIdentity> {
        let directory_id = checked_directory(directory, self.uid, Some(0o755))?;
        if macos_directory_names(directory, 1)? != [CPU_WORKER_NAME] {
            bail!("CPU directory has an unexpected inventory");
        }
        let mut worker = openat_node(
            directory.as_raw_fd(),
            OsStr::new(CPU_WORKER_NAME),
            NodeKind::Regular,
        )?;
        let worker_id = validate_native_node(
            &worker,
            NodeKind::Regular,
            self.uid,
            Some(self.length),
            true,
        )?;
        require_mode(&worker, 0o755)?;
        verify_bytes(&mut worker, self.length, &self.digest)?;
        if validate_native_node(
            &worker,
            NodeKind::Regular,
            self.uid,
            Some(self.length),
            true,
        )? != worker_id
        {
            bail!("CPU worker identity changed during verification");
        }
        Ok(InstalledIdentity {
            directory: directory_id,
            worker: worker_id,
        })
    }

    fn cleanup_owned_stage(&mut self) -> Result<()> {
        let Some(stage) = &self.stage else {
            if self.unproven_stage.is_some() {
                bail!(
                    "newly created staging inode could not be proven; retaining unadmitted stage"
                );
            }
            return Ok(());
        };
        if self.published {
            bail!("published directory is not a cleanup target");
        }
        self.recheck_anchors()?;
        // Essential even with a retained FD: rename may have succeeded then
        // reported an error. Never delete through a now-final directory FD.
        let named = openat_node(
            self.cpu()?.as_raw_fd(),
            OsStr::new(&stage.name),
            NodeKind::Directory,
        )?;
        if validate_native_node(&named, NodeKind::Directory, self.uid, None, false)?
            != stage.identity
            || validate_native_node(&stage.file, NodeKind::Directory, self.uid, None, false)?
                != stage.identity
        {
            bail!("old stage name no longer identifies the owned stage");
        }
        let names = macos_directory_names(&stage.file, 1)?;
        if names == [CPU_WORKER_NAME] {
            let worker = openat_node(
                stage.file.as_raw_fd(),
                OsStr::new(CPU_WORKER_NAME),
                NodeKind::Regular,
            )?;
            let identity = validate_native_node(&worker, NodeKind::Regular, self.uid, None, false)?;
            if Some(identity) != stage.worker {
                bail!("staging worker is not proven invocation-owned");
            }
            unlink(stage.file.as_raw_fd(), CPU_WORKER_NAME, false)?;
        } else if !names.is_empty() {
            bail!("stage contains an unexpected entry; retaining it");
        }
        let named = openat_node(
            self.cpu()?.as_raw_fd(),
            OsStr::new(&stage.name),
            NodeKind::Directory,
        )?;
        if validate_native_node(&named, NodeKind::Directory, self.uid, None, false)?
            != stage.identity
        {
            bail!("stage name changed before directory cleanup");
        }
        unlink(self.cpu()?.as_raw_fd(), &stage.name, true)?;
        self.cpu()?.sync_all()?;
        self.stage = None;
        Ok(())
    }

    fn retain_created_stage(&mut self, name: String) -> Result<()> {
        // Record creation before open/stat/ACL checks can fail. Uncertain
        // ownership is reported, never treated as successful cleanup.
        self.unproven_stage = Some(name.clone());
        let file = openat_node(
            self.cpu()?.as_raw_fd(),
            OsStr::new(&name),
            NodeKind::Directory,
        )?;
        let identity = checked_directory(&file, self.uid, Some(0o700))?;
        self.stage = Some(Stage {
            name,
            file,
            identity,
            worker: None,
        });
        self.unproven_stage = None;
        Ok(())
    }
}

impl Operations for NativeOperations {
    fn prepare(&mut self) -> Result<()> {
        self.walk_protected_root()?;
        let lock = match open_rw(self.cpu()?.as_raw_fd(), LOCK_NAME, true) {
            Ok(file) => file,
            Err(error) if has_errno(&error, libc::EEXIST) => {
                open_rw(self.cpu()?.as_raw_fd(), LOCK_NAME, false)?
            }
            Err(error) => return Err(error),
        };
        let identity = checked_lock(&lock, self.uid)?;
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err(io::Error::last_os_error())
                .context("another CPU installation holds the permanent lock");
        }
        self.lock = Some((lock, identity));
        self.recheck_anchors()
    }

    fn existing(&mut self) -> Result<bool> {
        self.recheck_anchors()?;
        let directory = match openat_node(
            self.cpu()?.as_raw_fd(),
            OsStr::new(&self.digest),
            NodeKind::Directory,
        ) {
            Ok(file) => file,
            Err(error) if has_errno(&error, libc::ENOENT) => return Ok(false),
            Err(error) => return Err(error),
        };
        self.admitted = Some(self.inspect(&directory)?);
        Ok(true)
    }

    fn stage(&mut self) -> Result<()> {
        self.recheck_anchors()?;
        for _ in 0..32 {
            let name = format!(
                ".staging-{}-{}",
                std::process::id(),
                STAGE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
            );
            let name_c = CString::new(name.as_str())?;
            if unsafe { libc::mkdirat(self.cpu()?.as_raw_fd(), name_c.as_ptr(), 0o700) } != 0 {
                if io::Error::last_os_error().raw_os_error() == Some(libc::EEXIST) {
                    continue;
                }
                return Err(io::Error::last_os_error())
                    .context("could not create exclusive CPU stage");
            }
            return self.retain_created_stage(name);
        }
        bail!("exclusive CPU staging collision limit exceeded")
    }

    fn populate(&mut self) -> Result<()> {
        let stage = self.stage.as_mut().context("missing fresh CPU stage")?;
        let mut output = open_rw(stage.file.as_raw_fd(), CPU_WORKER_NAME, true)?;
        let identity = validate_native_node(&output, NodeKind::Regular, self.uid, Some(0), false)?;
        require_mode(&output, 0o600)?;
        stage.worker = Some(identity);
        if candidate_size(&self.source)? != self.length {
            bail!("candidate size changed before copying");
        }
        copy_exact(&mut self.source, &mut output, self.length)?;
        output.sync_all()?;
        set_mode(&output, 0o755)?;
        output.sync_all()?;
        drop(output); // No writable image handles may survive publication.
        let mut image = openat_node(
            stage.file.as_raw_fd(),
            OsStr::new(CPU_WORKER_NAME),
            NodeKind::Regular,
        )?;
        if validate_native_node(&image, NodeKind::Regular, self.uid, Some(self.length), true)?
            != identity
        {
            bail!("fresh worker inode changed before readback");
        }
        require_mode(&image, 0o755)?;
        verify_bytes(&mut image, self.length, &self.digest)
    }

    fn seal(&mut self) -> Result<()> {
        let stage = self.stage.as_ref().context("missing CPU stage")?;
        set_mode(&stage.file, 0o755)?;
        stage.file.sync_all()?;
        let admitted = self.inspect(&stage.file)?;
        if admitted.directory != stage.identity || Some(admitted.worker) != stage.worker {
            bail!("sealed CPU stage identity changed");
        }
        self.admitted = Some(admitted);
        self.recheck_anchors()
    }

    fn publish(&mut self) -> Result<PublishResult> {
        self.recheck_anchors()?;
        let stage = self.stage.as_ref().context("missing sealed CPU stage")?;
        let named = openat_node(
            self.cpu()?.as_raw_fd(),
            OsStr::new(&stage.name),
            NodeKind::Directory,
        )?;
        if self.inspect(&named)? != self.admitted.context("missing sealed CPU identity")? {
            bail!("named CPU stage changed before publication");
        }
        let source = CString::new(stage.name.as_str())?;
        let destination = CString::new(self.digest.as_str())?;
        let parent = self.cpu()?.as_raw_fd();
        if unsafe {
            libc::renameatx_np(
                parent,
                source.as_ptr(),
                parent,
                destination.as_ptr(),
                libc::RENAME_EXCL,
            )
        } == 0
        {
            self.published = true; // Set before any subsequent fallible operation.
            return Ok(PublishResult::New);
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::EEXIST) {
            return Ok(PublishResult::Collision);
        }
        Err(error).context("exclusive CPU publication failed; destination may exist")
    }

    fn cleanup(&mut self) -> Result<()> {
        self.cleanup_owned_stage()
    }

    fn confirm(&mut self) -> Result<()> {
        self.recheck_anchors()?;
        self.cpu()?.sync_all()?;
        let directory = openat_node(
            self.cpu()?.as_raw_fd(),
            OsStr::new(&self.digest),
            NodeKind::Directory,
        )?;
        if Some(self.inspect(&directory)?) != self.admitted {
            bail!("published CPU directory identity changed");
        }
        self.recheck_anchors()
    }
}

fn open_candidate(path: &Path) -> Result<File> {
    let path = CString::new(path.as_os_str().as_bytes())?;
    owned_fd(unsafe {
        libc::open(
            path.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
        )
    })
}
fn candidate_size(file: &File) -> Result<u64> {
    let stat = descriptor_stat(file.as_raw_fd())?;
    if stat.st_mode & libc::S_IFMT != libc::S_IFREG || stat.st_nlink != 1 {
        bail!("candidate must be a single-link regular file");
    }
    let size = u64::try_from(stat.st_size)?;
    validate_cpu_size(size)?;
    Ok(size)
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::transaction::{Outcome, install};
    use sha2::{Digest, Sha256};
    use std::io::Write;
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::path::PathBuf;

    struct Fixture {
        path: PathBuf,
        cpu: PathBuf,
        candidate: PathBuf,
        digest: String,
    }
    impl Fixture {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "scribe-cpu-writer-test-{}-{}",
                std::process::id(),
                STAGE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&path).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
            let cpu = path.join("CPU");
            std::fs::create_dir(&cpu).unwrap();
            std::fs::set_permissions(&cpu, std::fs::Permissions::from_mode(0o755)).unwrap();
            let candidate = path.join("candidate");
            File::create_new(&candidate)
                .unwrap()
                .write_all(b"fixture CPU image")
                .unwrap();
            Self {
                path,
                cpu,
                candidate,
                digest: format!("{:x}", Sha256::digest(b"fixture CPU image")),
            }
        }
        fn ops(&self) -> NativeOperations {
            let source = open_candidate(&self.candidate).unwrap();
            let length = candidate_size(&source).unwrap();
            let uid = unsafe { libc::geteuid() };
            let file = open_absolute_directory(&self.cpu).unwrap();
            let identity = checked_directory(&file, uid, Some(0o755)).unwrap();
            NativeOperations {
                source,
                length,
                digest: self.digest.clone(),
                uid,
                anchors: vec![Anchor {
                    name: String::new(),
                    file,
                    identity,
                }],
                lock: None,
                stage: None,
                unproven_stage: None,
                admitted: None,
                published: false,
                fixture: true,
            }
        }
        fn installed(&self) -> PathBuf {
            self.cpu.join(&self.digest)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            // Only this test's exclusively created private fixture; never /Library.
            std::fs::remove_dir_all(&self.path).unwrap();
        }
    }

    #[test]
    fn installer_native_fresh_copy_and_reuse_preserve_inode_and_previous_versions() {
        let fixture = Fixture::new();
        let previous = fixture.cpu.join("retained-previous-version");
        std::fs::create_dir(&previous).unwrap();
        assert_eq!(install(&mut fixture.ops()).unwrap(), Outcome::PublishedNew);
        let worker = fixture.installed().join(CPU_WORKER_NAME);
        let before = descriptor_stat(File::open(&worker).unwrap().as_raw_fd()).unwrap();
        assert_eq!(std::fs::read(&worker).unwrap(), b"fixture CPU image");
        assert_eq!(
            install(&mut fixture.ops()).unwrap(),
            Outcome::ReusedExisting
        );
        let after = descriptor_stat(File::open(&worker).unwrap().as_raw_fd()).unwrap();
        assert_eq!((before.st_dev, before.st_ino), (after.st_dev, after.st_ino));
        assert!(previous.is_dir());
        assert!(fixture.cpu.join(LOCK_NAME).is_file());
        assert_eq!(std::fs::read_dir(&fixture.cpu).unwrap().count(), 3);
    }
    #[test]
    fn installer_native_tampered_candidate_never_publishes() {
        let fixture = Fixture::new();
        let mut ops = fixture.ops();
        std::fs::write(&fixture.candidate, b"tampered CPU data").unwrap();
        assert!(install(&mut ops).is_err());
        assert!(!fixture.installed().exists());
        assert_eq!(std::fs::read_dir(&fixture.cpu).unwrap().count(), 1);
    }
    #[test]
    fn installer_native_existing_tamper_extra_entry_and_wrong_mode_are_not_repaired() {
        for mutation in ["bytes", "extra", "mode", "link"] {
            let fixture = Fixture::new();
            install(&mut fixture.ops()).unwrap();
            let worker = fixture.installed().join(CPU_WORKER_NAME);
            match mutation {
                "bytes" => std::fs::write(&worker, b"tampered CPU data").unwrap(),
                "extra" => {
                    File::create_new(fixture.installed().join("unexpected")).unwrap();
                }
                "mode" => std::fs::set_permissions(&worker, std::fs::Permissions::from_mode(0o777))
                    .unwrap(),
                "link" => std::fs::hard_link(&worker, fixture.path.join("alias")).unwrap(),
                _ => unreachable!(),
            }
            assert!(install(&mut fixture.ops()).is_err(), "{mutation}");
            assert!(fixture.installed().is_dir());
        }
    }
    #[test]
    fn installer_native_lock_contention_is_bounded_and_lock_inode_is_permanent() {
        let fixture = Fixture::new();
        let mut first = fixture.ops();
        first.prepare().unwrap();
        let id = first.lock.as_ref().unwrap().1;
        assert!(fixture.ops().prepare().is_err());
        drop(first);
        let mut retry = fixture.ops();
        retry.prepare().unwrap();
        assert_eq!(retry.lock.as_ref().unwrap().1, id);
    }
    #[test]
    fn installer_native_lock_symlink_fifo_hardlink_and_wrong_mode_fail_closed() {
        for kind in ["symlink", "fifo", "hardlink", "mode"] {
            let fixture = Fixture::new();
            let lock = fixture.cpu.join(LOCK_NAME);
            match kind {
                "symlink" => symlink(&fixture.candidate, &lock).unwrap(),
                "fifo" => {
                    let name = CString::new(lock.as_os_str().as_bytes()).unwrap();
                    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
                }
                "hardlink" => {
                    let backing = fixture.path.join("lock-backing");
                    File::create_new(&backing).unwrap();
                    std::fs::set_permissions(&backing, std::fs::Permissions::from_mode(0o600))
                        .unwrap();
                    std::fs::hard_link(&backing, &lock).unwrap();
                }
                "mode" => {
                    File::create_new(&lock).unwrap();
                    std::fs::set_permissions(&lock, std::fs::Permissions::from_mode(0o666))
                        .unwrap();
                }
                _ => unreachable!(),
            }
            assert!(fixture.ops().prepare().is_err(), "{kind}");
            assert!(!fixture.installed().exists());
        }
    }
    #[test]
    fn installer_native_destination_symlink_and_mutable_parent_are_rejected() {
        let fixture = Fixture::new();
        symlink(&fixture.path, fixture.installed()).unwrap();
        assert!(install(&mut fixture.ops()).is_err());
        std::fs::remove_file(fixture.installed()).unwrap();
        let mut ops = fixture.ops();
        std::fs::set_permissions(&fixture.cpu, std::fs::Permissions::from_mode(0o777)).unwrap();
        assert!(install(&mut ops).is_err());
        assert_eq!(
            std::fs::metadata(&fixture.cpu)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o777
        );
    }
    #[test]
    fn installer_native_exclusive_publication_never_replaces_collision() {
        let fixture = Fixture::new();
        let mut ops = fixture.ops();
        ops.prepare().unwrap();
        ops.stage().unwrap();
        ops.populate().unwrap();
        ops.seal().unwrap();
        std::fs::create_dir(fixture.installed()).unwrap();
        let before = descriptor_stat(File::open(fixture.installed()).unwrap().as_raw_fd()).unwrap();
        assert_eq!(ops.publish().unwrap(), PublishResult::Collision);
        assert!(ops.existing().is_err()); // Empty impostor is not reusable.
        ops.cleanup().unwrap();
        let after = descriptor_stat(File::open(fixture.installed()).unwrap().as_raw_fd()).unwrap();
        assert_eq!(before.st_ino, after.st_ino);
    }
    #[test]
    fn installer_native_ambiguous_rename_cannot_cleanup_through_final_descriptor() {
        let fixture = Fixture::new();
        let mut ops = fixture.ops();
        ops.prepare().unwrap();
        ops.stage().unwrap();
        ops.populate().unwrap();
        ops.seal().unwrap();
        assert_eq!(ops.publish().unwrap(), PublishResult::New);
        ops.published = false; // Model rename-applied-but-error-returned uncertainty.
        assert!(ops.cleanup().is_err());
        assert_eq!(
            std::fs::read(fixture.installed().join(CPU_WORKER_NAME)).unwrap(),
            b"fixture CPU image"
        );
        drop(ops);
        assert_eq!(
            install(&mut fixture.ops()).unwrap(),
            Outcome::ReusedExisting
        );
    }
    #[test]
    fn installer_native_unexpected_stage_entry_and_replaced_lock_are_retained() {
        let fixture = Fixture::new();
        let mut ops = fixture.ops();
        ops.prepare().unwrap();
        ops.stage().unwrap();
        ops.populate().unwrap();
        let stage_path = fixture.cpu.join(&ops.stage.as_ref().unwrap().name);
        File::create_new(stage_path.join("unexpected")).unwrap();
        assert!(ops.cleanup().is_err());
        assert!(stage_path.join(CPU_WORKER_NAME).is_file());
        std::fs::rename(fixture.cpu.join(LOCK_NAME), fixture.cpu.join("old-lock")).unwrap();
        File::create_new(fixture.cpu.join(LOCK_NAME)).unwrap();
        std::fs::set_permissions(
            fixture.cpu.join(LOCK_NAME),
            std::fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        assert!(ops.recheck_anchors().is_err());
    }
    #[test]
    fn installer_native_candidate_symlink_fifo_and_hardlink_are_not_payloads() {
        let fixture = Fixture::new();
        let link = fixture.path.join("symlink");
        symlink(&fixture.candidate, &link).unwrap();
        assert!(open_candidate(&link).is_err());
        let fifo = CString::new(fixture.path.join("fifo").as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
        assert!(
            candidate_size(&open_candidate(Path::new(OsStr::from_bytes(fifo.as_bytes()))).unwrap())
                .is_err()
        );
        std::fs::hard_link(&fixture.candidate, fixture.path.join("hardlink")).unwrap();
        assert!(candidate_size(&open_candidate(&fixture.candidate).unwrap()).is_err());
    }

    #[test]
    fn installer_native_failure_after_stage_creation_reports_retention_without_deleting() {
        let fixture = Fixture::new();
        let mut ops = fixture.ops();
        ops.prepare().unwrap();
        let name = ".staging-unproven-fixture";
        let path = fixture.cpu.join(name);
        std::fs::create_dir(&path).unwrap();
        // Exercise a real validation failure after creation and descriptor open.
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o777)).unwrap();
        assert!(ops.retain_created_stage(name.to_owned()).is_err());
        assert!(ops.stage.is_none());
        let error = ops.cleanup().unwrap_err();
        assert!(error.to_string().contains("retaining unadmitted stage"));
        assert!(path.is_dir());
        assert!(!fixture.installed().exists());
    }
}
