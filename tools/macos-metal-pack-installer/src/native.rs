//! Root installer-owned, copy-only Metal transaction. Never a runtime service.
use anyhow::{Context, Result, bail};
use std::ffi::{CString, OsStr};
use std::fs::File;
use std::io;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::admission::{CopySource, FileSlot};
use crate::installer_bootstrap::ensure_admitted_ancestor;
use crate::installer_fs::{
    checked_directory, checked_lock, has_errno, open_rw, require_mode, set_mode, unlink,
};
use crate::installer_transaction::{Operations, PublishResult, copy_exact, verify_bytes};
use crate::macos_worker_fs::{
    NodeIdentity, NodeKind, macos_directory_names, open_absolute_directory, openat_node,
    validate_native_node,
};
use crate::manifest::PinnedPackRoot;

static SEQUENCE: AtomicU64 = AtomicU64::new(0);
const LOCK_NAME: &str = ".installer.lock";
#[cfg(test)]
#[derive(Clone, Copy, Eq, PartialEq)]
enum CopyPoint {
    BeforeRead,
    AfterWrite,
}
#[cfg(test)]
type CopyHook = Box<dyn FnMut(CopyPoint, FileSlot, &Path) -> Result<()>>;

struct Anchor {
    name: String,
    file: File,
    identity: NodeIdentity,
}
struct OwnedDirectory {
    file: File,
    identity: NodeIdentity,
}
struct Stage {
    name: String,
    root: OwnedDirectory,
    worker: Option<OwnedDirectory>,
    files: [Option<NodeIdentity>; 3],
    unproven_child: bool,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct TreeIdentity {
    root: NodeIdentity,
    worker: NodeIdentity,
    files: [NodeIdentity; 3],
}

pub(crate) struct NativeOperations<'bytes> {
    source: CopySource<'bytes>,
    anchors: Vec<Anchor>,
    store_index: Option<usize>,
    lock: Option<(File, NodeIdentity)>,
    stage: Option<Stage>,
    unproven_stage: Option<String>,
    admitted: Option<TreeIdentity>,
    published: bool,
    #[cfg(test)]
    fixture: bool,
    #[cfg(test)]
    fixture_root: Option<PathBuf>,
    #[cfg(test)]
    fixture_uid: libc::uid_t,
    #[cfg(test)]
    copy_hook: Option<CopyHook>,
}

impl<'bytes> NativeOperations<'bytes> {
    #[cfg(not(feature = "installer-package-entry"))]
    pub(crate) fn production(candidate: &Path) -> Result<Self> {
        // Must precede candidate access and every protected-root mutation.
        crate::require_installer_root(unsafe { libc::geteuid() })?;
        let source = CopySource::Leased(Box::new(crate::admission::admit_source(candidate)?));
        Ok(Self::new(source))
    }
    #[cfg(feature = "installer-package-entry")]
    pub(crate) fn production_embedded() -> Result<Self> {
        crate::require_installer_root(unsafe { libc::geteuid() })?;
        let source = CopySource::Embedded(Box::new(crate::package_entry::compiled_source()?));
        Ok(Self::new(source))
    }
    fn new(source: CopySource<'bytes>) -> Self {
        Self {
            source,
            anchors: vec![],
            store_index: None,
            lock: None,
            stage: None,
            unproven_stage: None,
            admitted: None,
            published: false,
            #[cfg(test)]
            fixture: false,
            #[cfg(test)]
            fixture_root: None,
            #[cfg(test)]
            fixture_uid: 0,
            #[cfg(test)]
            copy_hook: None,
        }
    }

    fn uid(&self) -> libc::uid_t {
        #[cfg(test)]
        if self.fixture {
            return self.fixture_uid;
        }
        0
    }
    fn parent(&self) -> Result<&File> {
        self.anchors
            .last()
            .map(|anchor| &anchor.file)
            .context("Metal parent is not retained")
    }
    fn store(&self) -> Result<&File> {
        self.store_index
            .and_then(|index| self.anchors.get(index))
            .map(|anchor| &anchor.file)
            .context("WorkerPacks root is not retained")
    }
    fn parent_path(&self) -> PathBuf {
        #[cfg(test)]
        let mut path = self
            .fixture_root
            .clone()
            .unwrap_or_else(|| PathBuf::from("/"));
        #[cfg(not(test))]
        let mut path = PathBuf::from("/");
        for anchor in self.anchors.iter().skip(1) {
            path.push(&anchor.name);
        }
        path
    }
    fn digest(&self) -> &str {
        self.source.destination_components()[2]
    }

    fn recheck_anchors(&self) -> Result<()> {
        for (index, anchor) in self.anchors.iter().enumerate() {
            if checked_directory(&anchor.file, self.uid(), None)? != anchor.identity {
                bail!("held Metal installer ancestor changed identity");
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
            if checked_directory(&reopened, self.uid(), None)? != anchor.identity {
                bail!("named Metal installer ancestor changed identity");
            }
        }
        if let Some((lock, identity)) = &self.lock {
            let named = open_rw(self.store()?.as_raw_fd(), LOCK_NAME, false)?;
            if checked_lock(lock, self.uid())? != *identity
                || checked_lock(&named, self.uid())? != *identity
            {
                bail!("permanent Metal installer lock identity changed");
            }
        }
        Ok(())
    }

    fn append_ancestor(&mut self, name: &str, may_create: bool) -> Result<()> {
        self.recheck_anchors()?;
        let file = match openat_node(
            self.parent()?.as_raw_fd(),
            OsStr::new(name),
            NodeKind::Directory,
        ) {
            Ok(file) => file,
            Err(error) if may_create && has_errno(&error, libc::ENOENT) => {
                ensure_admitted_ancestor(self.parent()?, name, self.uid())?
            }
            Err(error) => return Err(error),
        };
        let identity = checked_directory(&file, self.uid(), None)?;
        self.anchors.push(Anchor {
            name: name.to_owned(),
            file,
            identity,
        });
        self.recheck_anchors()
    }

    fn prepare_store(&mut self) -> Result<()> {
        #[cfg(test)]
        if self.fixture {
            return self.recheck_anchors();
        }
        let root = open_absolute_directory(Path::new("/"))?;
        let identity = checked_directory(&root, 0, None)?;
        self.anchors.push(Anchor {
            name: String::new(),
            file: root,
            identity,
        });
        for (name, create) in [
            ("Library", false),
            ("Application Support", false),
            ("Scribe", true),
            ("WorkerPacks", true),
        ] {
            self.append_ancestor(name, create)?;
        }
        self.store_index = Some(self.anchors.len() - 1);
        Ok(())
    }

    fn inspect_nodes(&self, root: &File) -> Result<TreeIdentity> {
        let root_id = checked_directory(root, self.uid(), Some(0o755))?;
        if macos_directory_names(root, 3)? != ["pack-manifest.json", "pack-manifest.sig", "worker"]
        {
            bail!("Metal root has an unexpected inventory");
        }
        let worker = openat_node(root.as_raw_fd(), OsStr::new("worker"), NodeKind::Directory)?;
        let worker_id = checked_directory(&worker, self.uid(), Some(0o755))?;
        if macos_directory_names(&worker, 1)? != ["scribe-inference-worker"] {
            bail!("Metal worker directory has an unexpected inventory");
        }
        let mut files = Vec::with_capacity(3);
        for slot in FileSlot::ALL {
            let parent = if slot == FileSlot::Worker {
                &worker
            } else {
                root
            };
            let file = openat_node(
                parent.as_raw_fd(),
                OsStr::new(slot.basename()),
                NodeKind::Regular,
            )?;
            let expected = self.source.expected(slot)?;
            let identity = validate_native_node(
                &file,
                NodeKind::Regular,
                self.uid(),
                Some(expected.size_bytes),
                slot == FileSlot::Worker,
            )?;
            require_mode(&file, slot.mode())?;
            files.push(identity);
        }
        Ok(TreeIdentity {
            root: root_id,
            worker: worker_id,
            files: files.try_into().expect("three closed file slots"),
        })
    }

    fn inspect(&self, root: &File, path: PathBuf) -> Result<TreeIdentity> {
        self.recheck_anchors()?;
        let identity = self.inspect_nodes(root)?;
        let mut handles = self
            .anchors
            .iter()
            .map(|anchor| anchor.file.try_clone())
            .collect::<io::Result<Vec<_>>>()?;
        handles.push(root.try_clone()?);
        let pinned = PinnedPackRoot::from_anchored_handles(path, handles)?;
        self.source.verify_materialized(pinned)?;
        if self.inspect_nodes(root)? != identity {
            bail!("Metal tree changed during independent verification");
        }
        self.recheck_anchors()?;
        Ok(identity)
    }

    fn retain_stage(&mut self, name: String) -> Result<()> {
        self.unproven_stage = Some(name.clone());
        let file = openat_node(
            self.parent()?.as_raw_fd(),
            OsStr::new(&name),
            NodeKind::Directory,
        )?;
        let identity = checked_directory(&file, self.uid(), Some(0o700))?;
        self.stage = Some(Stage {
            name,
            root: OwnedDirectory { file, identity },
            worker: None,
            files: [None; 3],
            unproven_child: false,
        });
        self.unproven_stage = None;
        let stage = self
            .stage
            .as_mut()
            .context("missing newly retained Metal stage")?;
        stage.unproven_child = true; // Before any child creation or fallible proof.
        if unsafe { libc::mkdirat(stage.root.file.as_raw_fd(), c"worker".as_ptr(), 0o700) } != 0 {
            return Err(io::Error::last_os_error())
                .context("could not create exclusive worker directory");
        }
        let file = openat_node(
            stage.root.file.as_raw_fd(),
            OsStr::new("worker"),
            NodeKind::Directory,
        )?;
        let identity = checked_directory(&file, self.uid(), Some(0o700))?;
        let stage = self.stage.as_mut().unwrap();
        stage.worker = Some(OwnedDirectory { file, identity });
        stage.unproven_child = false;
        Ok(())
    }

    fn prove_cleanup_tree(&self, stage: &Stage) -> Result<()> {
        if self.published {
            bail!("published Metal tree is not a cleanup target");
        }
        self.recheck_anchors()?;
        // This proof MUST precede every unlink through a descriptor: an applied
        // rename may have returned an error and turned that descriptor final.
        let named = openat_node(
            self.parent()?.as_raw_fd(),
            OsStr::new(&stage.name),
            NodeKind::Directory,
        )?;
        for file in [&named, &stage.root.file] {
            if validate_native_node(file, NodeKind::Directory, self.uid(), None, false)?
                != stage.root.identity
            {
                bail!("original Metal stage name no longer identifies the owned inode");
            }
        }
        if stage.unproven_child {
            bail!("Metal stage contains an unproven created child; retaining it");
        }
        let mut root_names = Vec::new();
        for slot in [FileSlot::Manifest, FileSlot::Signature] {
            if stage.files[slot.index()].is_some() {
                root_names.push(slot.basename().to_owned());
            }
        }
        if stage.worker.is_some() {
            root_names.push("worker".to_owned());
        }
        root_names.sort();
        if macos_directory_names(&stage.root.file, 3)? != root_names {
            bail!("Metal stage has unknown or missing children; retaining it");
        }
        if let Some(worker) = &stage.worker {
            let named = openat_node(
                stage.root.file.as_raw_fd(),
                OsStr::new("worker"),
                NodeKind::Directory,
            )?;
            for file in [&named, &worker.file] {
                if validate_native_node(file, NodeKind::Directory, self.uid(), None, false)?
                    != worker.identity
                {
                    bail!("owned worker directory changed before cleanup");
                }
            }
            let expected = if stage.files[FileSlot::Worker.index()].is_some() {
                vec!["scribe-inference-worker".to_owned()]
            } else {
                vec![]
            };
            if macos_directory_names(&worker.file, 1)? != expected {
                bail!("worker stage contains unknown entries; retaining it");
            }
        } else if stage.files[FileSlot::Worker.index()].is_some() {
            bail!("owned worker file lost its directory");
        }
        for slot in FileSlot::ALL {
            if let Some(expected) = stage.files[slot.index()] {
                let parent = stage_parent(stage, slot)?;
                let file = openat_node(
                    parent.as_raw_fd(),
                    OsStr::new(slot.basename()),
                    NodeKind::Regular,
                )?;
                if validate_native_node(&file, NodeKind::Regular, self.uid(), None, false)?
                    != expected
                {
                    bail!("owned staging file changed before cleanup");
                }
            }
        }
        Ok(())
    }
}

fn stage_parent(stage: &Stage, slot: FileSlot) -> Result<&File> {
    if slot == FileSlot::Worker {
        stage
            .worker
            .as_ref()
            .map(|worker| &worker.file)
            .context("missing owned worker directory")
    } else {
        Ok(&stage.root.file)
    }
}

impl Operations for NativeOperations<'_> {
    fn prepare(&mut self) -> Result<()> {
        self.source.reverify()?; // Before creating even a fixed protected ancestor.
        self.prepare_store()?;
        let lock = match open_rw(self.store()?.as_raw_fd(), LOCK_NAME, true) {
            Ok(lock) => lock,
            Err(error) if has_errno(&error, libc::EEXIST) => {
                open_rw(self.store()?.as_raw_fd(), LOCK_NAME, false)?
            }
            Err(error) => return Err(error),
        };
        let identity = checked_lock(&lock, self.uid())?;
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err(io::Error::last_os_error())
                .context("another Metal installation holds the permanent lock");
        }
        self.lock = Some((lock, identity));
        self.recheck_anchors()?;
        self.lock.as_ref().unwrap().0.sync_all()?;
        self.store()?.sync_all()?;
        // The CLI supplies neither component. Both come only from the permit.
        let [id, version, _] = self.source.destination_components();
        let id = id.to_owned();
        let version = version.to_owned();
        self.append_ancestor(&id, true)?;
        self.append_ancestor(&version, true)?;
        Ok(())
    }

    fn existing(&mut self) -> Result<bool> {
        self.recheck_anchors()?;
        let file = match openat_node(
            self.parent()?.as_raw_fd(),
            OsStr::new(self.digest()),
            NodeKind::Directory,
        ) {
            Ok(file) => file,
            Err(error) if has_errno(&error, libc::ENOENT) => return Ok(false),
            Err(error) => return Err(error),
        };
        self.admitted = Some(self.inspect(&file, self.parent_path().join(self.digest()))?);
        Ok(true)
    }

    fn stage(&mut self) -> Result<()> {
        self.recheck_anchors()?;
        for _ in 0..32 {
            let name = format!(
                ".metal-stage-{}-{}",
                std::process::id(),
                SEQUENCE.fetch_add(1, Ordering::Relaxed)
            );
            let name_c = CString::new(name.as_str())?;
            if unsafe { libc::mkdirat(self.parent()?.as_raw_fd(), name_c.as_ptr(), 0o700) } != 0 {
                let error = io::Error::last_os_error();
                if error.raw_os_error() == Some(libc::EEXIST) {
                    continue;
                }
                return Err(error).context("could not create exclusive Metal stage");
            }
            return self.retain_stage(name);
        }
        bail!("exclusive Metal staging collision limit exceeded")
    }

    fn populate(&mut self) -> Result<()> {
        self.source.reverify()?;
        let uid = self.uid();
        #[cfg(test)]
        let stage_path = self
            .parent_path()
            .join(&self.stage.as_ref().context("missing Metal stage")?.name);
        for slot in FileSlot::ALL {
            self.recheck_anchors()?;
            let (expected, mut input) = self.source.open_for_copy(slot)?;
            let stage = self.stage.as_mut().context("missing Metal stage")?;
            stage.unproven_child = true;
            let mut output = open_rw(
                stage_parent(stage, slot)?.as_raw_fd(),
                slot.basename(),
                true,
            )?;
            let identity = validate_native_node(&output, NodeKind::Regular, uid, Some(0), false)?;
            require_mode(&output, 0o600)?;
            stage.files[slot.index()] = Some(identity);
            stage.unproven_child = false;
            #[cfg(test)]
            if let Some(hook) = self.copy_hook.as_mut() {
                hook(CopyPoint::BeforeRead, slot, &stage_path.join(slot.path()))?;
            }
            copy_exact(&mut input, &mut output, expected.size_bytes, slot.maximum())?;
            output.sync_all()?;
            set_mode(&output, slot.mode() as libc::mode_t)?;
            output.sync_all()?;
            drop(output); // No writable image or metadata handles survive publication.
            #[cfg(test)]
            if let Some(hook) = self.copy_hook.as_mut() {
                hook(CopyPoint::AfterWrite, slot, &stage_path.join(slot.path()))?;
            }
            let mut copied = openat_node(
                stage_parent(stage, slot)?.as_raw_fd(),
                OsStr::new(slot.basename()),
                NodeKind::Regular,
            )?;
            if validate_native_node(
                &copied,
                NodeKind::Regular,
                uid,
                Some(expected.size_bytes),
                slot == FileSlot::Worker,
            )? != identity
            {
                bail!("copied file identity changed before readback");
            }
            require_mode(&copied, slot.mode())?;
            verify_bytes(
                &mut copied,
                expected.size_bytes,
                &expected.sha256,
                slot.maximum(),
            )?;
            if validate_native_node(
                &copied,
                NodeKind::Regular,
                uid,
                Some(expected.size_bytes),
                slot == FileSlot::Worker,
            )? != identity
            {
                bail!("copied file changed during readback");
            }
        }
        self.source.reverify()
    }

    fn seal(&mut self) -> Result<()> {
        let stage = self.stage.as_ref().context("missing Metal stage")?;
        let worker = stage
            .worker
            .as_ref()
            .context("missing owned worker directory")?;
        set_mode(&worker.file, 0o755)?;
        worker.file.sync_all()?;
        set_mode(&stage.root.file, 0o755)?;
        stage.root.file.sync_all()?;
        let identity = self.inspect(&stage.root.file, self.parent_path().join(&stage.name))?;
        if identity.root != stage.root.identity
            || identity.worker != worker.identity
            || FileSlot::ALL
                .iter()
                .any(|slot| stage.files[slot.index()] != Some(identity.files[slot.index()]))
        {
            bail!("sealed Metal stage differs from invocation-owned inodes");
        }
        self.admitted = Some(identity);
        Ok(())
    }

    fn publish(&mut self) -> Result<PublishResult> {
        self.recheck_anchors()?;
        let stage = self.stage.as_ref().context("missing sealed Metal stage")?;
        let named = openat_node(
            self.parent()?.as_raw_fd(),
            OsStr::new(&stage.name),
            NodeKind::Directory,
        )?;
        if Some(self.inspect(&named, self.parent_path().join(&stage.name))?) != self.admitted {
            bail!("named Metal stage changed before publication");
        }
        let from = CString::new(stage.name.as_str())?;
        let to = CString::new(self.digest())?;
        let parent = self.parent()?.as_raw_fd();
        if unsafe {
            libc::renameatx_np(
                parent,
                from.as_ptr(),
                parent,
                to.as_ptr(),
                libc::RENAME_EXCL,
            )
        } == 0
        {
            self.published = true; // Before every fallible confirmation operation.
            return Ok(PublishResult::New);
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::EEXIST) {
            return Ok(PublishResult::Collision);
        }
        Err(error).context("exclusive Metal publication failed; destination may exist")
    }

    fn cleanup(&mut self) -> Result<()> {
        let Some(stage) = self.stage.as_ref() else {
            if self.unproven_stage.is_some() {
                bail!("new Metal stage ownership is unproven; retaining it");
            }
            return Ok(());
        };
        self.prove_cleanup_tree(stage)?; // Complete proof before the first unlink.
        for slot in FileSlot::ALL {
            if stage.files[slot.index()].is_some() {
                unlink(
                    stage_parent(stage, slot)?.as_raw_fd(),
                    slot.basename(),
                    false,
                )?;
            }
        }
        if stage.worker.is_some() {
            unlink(stage.root.file.as_raw_fd(), "worker", true)?;
        }
        // Re-prove the old basename after child removal, never a digest name.
        let named = openat_node(
            self.parent()?.as_raw_fd(),
            OsStr::new(&stage.name),
            NodeKind::Directory,
        )?;
        if validate_native_node(&named, NodeKind::Directory, self.uid(), None, false)?
            != stage.root.identity
        {
            bail!("Metal stage name changed before final owned-stage cleanup");
        }
        unlink(self.parent()?.as_raw_fd(), &stage.name, true)?;
        self.parent()?.sync_all()?;
        self.stage = None;
        Ok(())
    }

    fn confirm(&mut self) -> Result<()> {
        self.recheck_anchors()?;
        self.parent()?.sync_all()?;
        let file = openat_node(
            self.parent()?.as_raw_fd(),
            OsStr::new(self.digest()),
            NodeKind::Directory,
        )?;
        if Some(self.inspect(&file, self.parent_path().join(self.digest()))?) != self.admitted {
            bail!("published Metal tree identity changed during confirmation");
        }
        self.recheck_anchors()
    }
}

#[cfg(test)]
mod tests;
