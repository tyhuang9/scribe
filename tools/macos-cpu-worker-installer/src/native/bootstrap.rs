//! Publish missing fixed ancestors only after private staging is fully sealed.
//! Reuses the image transaction's ordering and conservative failure accounting.
use super::*;
use crate::transaction::install;

struct DirectoryStage {
    name: String,
    file: File,
    identity: NodeIdentity,
}

struct Bootstrap<'a> {
    parent: &'a File,
    parent_identity: NodeIdentity,
    final_name: &'a str,
    uid: libc::uid_t,
    stage: Option<DirectoryStage>,
    unproven_stage: Option<String>,
    admitted: Option<NodeIdentity>,
    published: bool,
}

pub(super) fn ensure_fixed_ancestor(parent: &File, name: &str, uid: libc::uid_t) -> Result<File> {
    let mut operation = Bootstrap::new(parent, name, uid)?;
    install(&mut operation).context("fixed CPU ancestor transaction failed")?;
    let file = openat_node(parent.as_raw_fd(), OsStr::new(name), NodeKind::Directory)?;
    if Some(checked_directory(&file, uid, None)?) != operation.admitted {
        bail!("fixed ancestor changed after transaction confirmation");
    }
    Ok(file)
}

impl<'a> Bootstrap<'a> {
    fn new(parent: &'a File, final_name: &'a str, uid: libc::uid_t) -> Result<Self> {
        if !matches!(final_name, "Scribe" | "CPU") {
            bail!("only fixed Scribe/CPU ancestors may be created");
        }
        Ok(Self {
            parent,
            parent_identity: checked_directory(parent, uid, None)?,
            final_name,
            uid,
            stage: None,
            unproven_stage: None,
            admitted: None,
            published: false,
        })
    }

    fn recheck_parent(&self) -> Result<()> {
        if checked_directory(self.parent, self.uid, None)? != self.parent_identity {
            bail!("held bootstrap parent changed identity");
        }
        Ok(())
    }

    fn retain_created_stage(&mut self, name: String) -> Result<()> {
        self.unproven_stage = Some(name.clone());
        let file = openat_node(
            self.parent.as_raw_fd(),
            OsStr::new(&name),
            NodeKind::Directory,
        )?;
        let identity = checked_directory(&file, self.uid, Some(0o700))?;
        self.stage = Some(DirectoryStage {
            name,
            file,
            identity,
        });
        self.unproven_stage = None;
        Ok(())
    }

    fn check_empty_stage(&self, mode: u32) -> Result<&DirectoryStage> {
        let stage = self
            .stage
            .as_ref()
            .context("missing proven ancestor stage")?;
        if checked_directory(&stage.file, self.uid, Some(mode))? != stage.identity {
            bail!("held ancestor stage changed identity");
        }
        if !macos_directory_names(&stage.file, 0)?.is_empty() {
            bail!("ancestor stage is not empty");
        }
        Ok(stage)
    }
}

impl Operations for Bootstrap<'_> {
    fn prepare(&mut self) -> Result<()> {
        self.recheck_parent()
    }

    fn existing(&mut self) -> Result<bool> {
        self.recheck_parent()?;
        let file = match openat_node(
            self.parent.as_raw_fd(),
            OsStr::new(self.final_name),
            NodeKind::Directory,
        ) {
            Ok(file) => file,
            Err(error) if has_errno(&error, libc::ENOENT) => return Ok(false),
            Err(error) => return Err(error),
        };
        // Existing ancestors may contain a legitimate installer's descendants.
        // They are inspected, never adopted, normalized, emptied or replaced.
        self.admitted = Some(checked_directory(&file, self.uid, None)?);
        Ok(true)
    }

    fn stage(&mut self) -> Result<()> {
        self.recheck_parent()?;
        for _ in 0..32 {
            let name = format!(
                ".bootstrap-{}-{}",
                std::process::id(),
                STAGE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
            );
            let name_c = CString::new(name.as_str())?;
            if unsafe { libc::mkdirat(self.parent.as_raw_fd(), name_c.as_ptr(), 0o700) } != 0 {
                let error = io::Error::last_os_error();
                if error.raw_os_error() == Some(libc::EEXIST) {
                    continue;
                }
                return Err(error).context("could not create exclusive ancestor stage");
            }
            return self.retain_created_stage(name);
        }
        bail!("exclusive ancestor staging collision limit exceeded")
    }

    fn populate(&mut self) -> Result<()> {
        self.check_empty_stage(0o700)?;
        Ok(())
    }

    fn seal(&mut self) -> Result<()> {
        let stage = self.check_empty_stage(0o700)?;
        set_mode(&stage.file, 0o755)?;
        stage.file.sync_all()?;
        self.admitted = Some(self.check_empty_stage(0o755)?.identity);
        self.recheck_parent()
    }

    fn publish(&mut self) -> Result<PublishResult> {
        self.recheck_parent()?;
        let stage = self.check_empty_stage(0o755)?;
        let named = openat_node(
            self.parent.as_raw_fd(),
            OsStr::new(&stage.name),
            NodeKind::Directory,
        )?;
        if checked_directory(&named, self.uid, Some(0o755))? != stage.identity
            || Some(stage.identity) != self.admitted
        {
            bail!("named ancestor stage changed before publication");
        }
        let source = CString::new(stage.name.as_str())?;
        let destination = CString::new(self.final_name)?;
        let parent = self.parent.as_raw_fd();
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
            self.published = true; // Before sync/reopen or any other fallible work.
            return Ok(PublishResult::New);
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::EEXIST) {
            return Ok(PublishResult::Collision);
        }
        Err(error).context("exclusive ancestor publication failed; final name may exist")
    }

    fn cleanup(&mut self) -> Result<()> {
        if self.published {
            bail!("published ancestor is not a cleanup target");
        }
        let Some(stage) = &self.stage else {
            if self.unproven_stage.is_some() {
                bail!("unproven ancestor stage retained");
            }
            return Ok(());
        };
        self.recheck_parent()?;
        // A rename can take effect then report error. Old-name proof MUST
        // precede any operation through the potentially final descriptor.
        let named = openat_node(
            self.parent.as_raw_fd(),
            OsStr::new(&stage.name),
            NodeKind::Directory,
        )?;
        if validate_native_node(&named, NodeKind::Directory, self.uid, None, false)?
            != stage.identity
            || validate_native_node(&stage.file, NodeKind::Directory, self.uid, None, false)?
                != stage.identity
        {
            bail!("original ancestor stage name changed; retaining it");
        }
        if !macos_directory_names(&stage.file, 0)?.is_empty() {
            bail!("ancestor stage gained an entry; retaining it");
        }
        unlink(self.parent.as_raw_fd(), &stage.name, true)?;
        self.parent.sync_all()?;
        self.stage = None;
        Ok(())
    }

    fn confirm(&mut self) -> Result<()> {
        self.recheck_parent()?;
        self.parent.sync_all()?;
        let file = openat_node(
            self.parent.as_raw_fd(),
            OsStr::new(self.final_name),
            NodeKind::Directory,
        )?;
        // Once published, another legitimate installer can add descendants.
        // Only stage inventory is empty; final policy and identity stay bound.
        if Some(checked_directory(&file, self.uid, None)?) != self.admitted {
            bail!("fixed ancestor changed during confirmation");
        }
        self.recheck_parent()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::path::PathBuf;

    struct Fixture {
        path: PathBuf,
        parent: File,
        uid: libc::uid_t,
    }
    impl Fixture {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "scribe-cpu-bootstrap-test-{}-{}",
                std::process::id(),
                STAGE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&path).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
            std::fs::create_dir(path.join("parent")).unwrap();
            std::fs::set_permissions(path.join("parent"), std::fs::Permissions::from_mode(0o755))
                .unwrap();
            let parent = open_absolute_directory(&path.join("parent")).unwrap();
            Self {
                path,
                parent,
                uid: unsafe { libc::geteuid() },
            }
        }
        fn child(&self, name: &str) -> PathBuf {
            self.path.join("parent").join(name)
        }
        fn operation(&self, name: &'static str) -> Bootstrap<'_> {
            Bootstrap::new(&self.parent, name, self.uid).unwrap()
        }
        fn identity(&self, name: &str) -> NodeIdentity {
            let file = openat_node(
                self.parent.as_raw_fd(),
                OsStr::new(name),
                NodeKind::Directory,
            )
            .unwrap();
            checked_directory(&file, self.uid, None).unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            // Exclusively created private test root; no production root entry.
            std::fs::remove_dir_all(&self.path).unwrap();
        }
    }

    fn sealed_stage(operation: &mut Bootstrap<'_>) {
        operation.prepare().unwrap();
        assert!(!operation.existing().unwrap());
        operation.stage().unwrap();
        operation.populate().unwrap();
        operation.seal().unwrap();
    }

    #[test]
    fn bootstrap_native_publishes_and_reuses_fixed_ancestors_preserving_descendants() {
        let fixture = Fixture::new();
        for name in ["Scribe", "CPU"] {
            let first = ensure_fixed_ancestor(&fixture.parent, name, fixture.uid).unwrap();
            require_mode(&first, 0o755).unwrap();
            let identity = checked_directory(&first, fixture.uid, None).unwrap();
            File::create_new(fixture.child(name).join("retained-descendant")).unwrap();
            let second = ensure_fixed_ancestor(&fixture.parent, name, fixture.uid).unwrap();
            assert_eq!(
                checked_directory(&second, fixture.uid, None).unwrap(),
                identity
            );
            assert!(fixture.child(name).join("retained-descendant").is_file());
        }
        assert_eq!(
            macos_directory_names(&fixture.parent, 2).unwrap(),
            ["CPU", "Scribe"]
        );
    }

    #[test]
    fn bootstrap_native_interruption_before_and_after_seal_leaves_no_final_and_retry_succeeds() {
        for sealed in [false, true] {
            let fixture = Fixture::new();
            let mut abandoned = fixture.operation("Scribe");
            abandoned.prepare().unwrap();
            abandoned.stage().unwrap();
            abandoned.populate().unwrap();
            if sealed {
                abandoned.seal().unwrap();
            }
            let stage = abandoned.stage.as_ref().unwrap();
            let stage_path = fixture.child(&stage.name);
            let stage_identity = stage.identity;
            require_mode(&stage.file, if sealed { 0o755 } else { 0o700 }).unwrap();
            assert!(!fixture.child("Scribe").exists());
            drop(abandoned); // Simulate a process exit without stage cleanup.
            let ready = ensure_fixed_ancestor(&fixture.parent, "Scribe", fixture.uid).unwrap();
            require_mode(&ready, 0o755).unwrap();
            assert_ne!(
                checked_directory(&ready, fixture.uid, None).unwrap(),
                stage_identity
            );
            assert!(stage_path.is_dir()); // Never sweep/adopt a previous invocation.
        }
    }

    #[test]
    fn bootstrap_native_unsafe_existing_and_nonfixed_names_are_never_repaired() {
        for kind in ["private", "mutable", "symlink", "regular", "fifo"] {
            let fixture = Fixture::new();
            let final_path = fixture.child("CPU");
            match kind {
                "private" | "mutable" => {
                    std::fs::create_dir(&final_path).unwrap();
                    std::fs::set_permissions(
                        &final_path,
                        std::fs::Permissions::from_mode(if kind == "private" {
                            0o700
                        } else {
                            0o777
                        }),
                    )
                    .unwrap();
                }
                "symlink" => symlink(&fixture.path, &final_path).unwrap(),
                "regular" => {
                    File::create_new(&final_path).unwrap();
                }
                "fifo" => {
                    let path = CString::new(final_path.as_os_str().as_bytes()).unwrap();
                    assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
                }
                _ => unreachable!(),
            }
            let before = std::fs::symlink_metadata(&final_path).unwrap();
            assert!(
                ensure_fixed_ancestor(&fixture.parent, "CPU", fixture.uid).is_err(),
                "{kind}"
            );
            let after = std::fs::symlink_metadata(&final_path).unwrap();
            use std::os::unix::fs::MetadataExt;
            assert_eq!(
                (before.dev(), before.ino(), before.mode()),
                (after.dev(), after.ino(), after.mode())
            );
            assert_eq!(macos_directory_names(&fixture.parent, 1).unwrap(), ["CPU"]);
        }
        let fixture = Fixture::new();
        for invalid in ["", "..", "../CPU", "Scribe/CPU", "Scribe:CPU", "other"] {
            assert!(ensure_fixed_ancestor(&fixture.parent, invalid, fixture.uid).is_err());
        }
        assert!(
            macos_directory_names(&fixture.parent, 0)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn bootstrap_native_concurrent_collision_preserves_existing_children_and_identity() {
        let fixture = Fixture::new();
        let mut first = fixture.operation("CPU");
        let mut second = fixture.operation("CPU");
        sealed_stage(&mut first);
        sealed_stage(&mut second);
        assert_eq!(first.publish().unwrap(), PublishResult::New);
        let identity = fixture.identity("CPU");
        File::create_new(fixture.child("CPU").join("legitimate-descendant")).unwrap();
        first.confirm().unwrap(); // Final ancestors need not remain empty.
        assert_eq!(second.publish().unwrap(), PublishResult::Collision);
        assert!(second.existing().unwrap());
        second.cleanup().unwrap();
        second.confirm().unwrap();
        assert_eq!(fixture.identity("CPU"), identity);
        assert_eq!(second.admitted, Some(identity));
        assert!(fixture.child("CPU").join("legitimate-descendant").is_file());
        assert_eq!(macos_directory_names(&fixture.parent, 1).unwrap(), ["CPU"]);
    }

    #[test]
    fn bootstrap_native_ambiguous_rename_cannot_cleanup_through_final_descriptor() {
        let fixture = Fixture::new();
        let mut operation = fixture.operation("Scribe");
        sealed_stage(&mut operation);
        assert_eq!(operation.publish().unwrap(), PublishResult::New);
        operation.published = false; // Model applied rename with an error return.
        let identity = fixture.identity("Scribe");
        File::create_new(fixture.child("Scribe").join("retained-descendant")).unwrap();
        assert!(operation.cleanup().is_err());
        drop(operation);
        let ready = ensure_fixed_ancestor(&fixture.parent, "Scribe", fixture.uid).unwrap();
        assert_eq!(
            checked_directory(&ready, fixture.uid, None).unwrap(),
            identity
        );
        assert!(
            fixture
                .child("Scribe")
                .join("retained-descendant")
                .is_file()
        );
    }

    #[test]
    fn bootstrap_native_unproven_and_nonempty_stages_are_retained_and_reported() {
        let fixture = Fixture::new();
        let mut unproven = fixture.operation("CPU");
        let name = ".bootstrap-unproven-fixture";
        std::fs::create_dir(fixture.child(name)).unwrap();
        std::fs::set_permissions(fixture.child(name), std::fs::Permissions::from_mode(0o777))
            .unwrap();
        assert!(unproven.retain_created_stage(name.to_owned()).is_err());
        assert!(unproven.stage.is_none());
        assert!(
            unproven
                .cleanup()
                .unwrap_err()
                .to_string()
                .contains("unproven ancestor stage retained")
        );
        assert!(fixture.child(name).is_dir());

        let mut unexpected = fixture.operation("Scribe");
        unexpected.prepare().unwrap();
        unexpected.stage().unwrap();
        let stage_path = fixture.child(&unexpected.stage.as_ref().unwrap().name);
        File::create_new(stage_path.join("unexpected")).unwrap();
        assert!(unexpected.populate().is_err());
        assert!(unexpected.cleanup().is_err());
        assert!(stage_path.join("unexpected").is_file());
        assert!(!fixture.child("Scribe").exists());
    }

    #[test]
    fn bootstrap_native_post_publication_confirmation_failure_retains_final_for_retry() {
        let fixture = Fixture::new();
        let mut operation = fixture.operation("CPU");
        sealed_stage(&mut operation);
        assert_eq!(operation.publish().unwrap(), PublishResult::New);
        let identity = fixture.identity("CPU");
        // Test-only private parent mutation makes real confirmation fail.
        std::fs::set_permissions(fixture.child(""), std::fs::Permissions::from_mode(0o777))
            .unwrap();
        assert!(operation.confirm().is_err());
        assert!(operation.cleanup().is_err());
        assert!(fixture.child("CPU").is_dir());
        std::fs::set_permissions(fixture.child(""), std::fs::Permissions::from_mode(0o755))
            .unwrap();
        drop(operation);
        let ready = ensure_fixed_ancestor(&fixture.parent, "CPU", fixture.uid).unwrap();
        assert_eq!(
            checked_directory(&ready, fixture.uid, None).unwrap(),
            identity
        );
    }

    #[test]
    fn bootstrap_native_replaced_stage_name_never_deletes_the_replacement() {
        let fixture = Fixture::new();
        let mut operation = fixture.operation("CPU");
        operation.prepare().unwrap();
        operation.stage().unwrap();
        let stage_path = fixture.child(&operation.stage.as_ref().unwrap().name);
        let retained = fixture.child("retained-original");
        std::fs::rename(&stage_path, &retained).unwrap();
        std::fs::create_dir(&stage_path).unwrap();
        std::fs::set_permissions(&stage_path, std::fs::Permissions::from_mode(0o700)).unwrap();
        File::create_new(stage_path.join("replacement-entry")).unwrap();
        assert!(operation.cleanup().is_err());
        assert!(retained.is_dir() && stage_path.join("replacement-entry").is_file());
        assert!(!fixture.child("CPU").exists());
    }
}
