//! Real Darwin operations, exclusively in private unprivileged fixtures.
use super::*;
use crate::installer_transaction::{Outcome, run_transaction};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};

struct Fixture {
    source: crate::admission::tests::Fixture,
    owner: PathBuf,
    store: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let source = crate::admission::tests::Fixture::new(|_| {});
        let owner = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "scribe-metal-copy-test-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&owner).unwrap();
        std::fs::set_permissions(&owner, std::fs::Permissions::from_mode(0o700)).unwrap();
        let store = owner.join("WorkerPacks");
        std::fs::create_dir(&store).unwrap();
        std::fs::set_permissions(&store, std::fs::Permissions::from_mode(0o755)).unwrap();
        Self {
            source,
            owner,
            store,
        }
    }
    fn ops(&self) -> NativeOperations<'_> {
        self.ops_with_source(CopySource::Leased(Box::new(self.source.admit().unwrap())))
    }
    fn ops_with_source<'bytes>(&self, source: CopySource<'bytes>) -> NativeOperations<'bytes> {
        let uid = unsafe { libc::geteuid() };
        let file = open_absolute_directory(&self.store).unwrap();
        let identity = checked_directory(&file, uid, Some(0o755)).unwrap();
        NativeOperations {
            source,
            anchors: vec![Anchor {
                name: String::new(),
                file,
                identity,
            }],
            store_index: Some(0),
            lock: None,
            stage: None,
            unproven_stage: None,
            admitted: None,
            published: false,
            fixture: true,
            fixture_root: Some(self.store.clone()),
            fixture_uid: uid,
            copy_hook: None,
        }
    }
    fn installed(&self) -> PathBuf {
        let source = self.source.admit().unwrap();
        source
            .destination_components()
            .into_iter()
            .fold(self.store.clone(), |path, name| path.join(name))
    }
    fn version(&self) -> PathBuf {
        self.installed().parent().unwrap().to_path_buf()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        // This invocation's exclusively created fixture only; never /Library.
        std::fs::remove_dir_all(&self.owner).unwrap();
    }
}
fn sealed(ops: &mut NativeOperations) {
    ops.prepare().unwrap();
    assert!(!ops.existing().unwrap());
    ops.stage().unwrap();
    ops.populate().unwrap();
    ops.seal().unwrap();
}
fn inode(path: &Path) -> (u64, u64) {
    let stat = std::fs::metadata(path).unwrap();
    (stat.dev(), stat.ino())
}

#[test]
fn metal_native_embedded_copy_reuses_exact_bytes_without_source_files() {
    let fixture = Fixture::new();
    let bytes = fixture.source.embedded_bytes();
    let installed = fixture.installed();
    let mut ops = fixture.ops_with_source(fixture.source.embedded(&bytes).unwrap());
    for slot in FileSlot::ALL {
        std::fs::remove_file(fixture.source.root().join(slot.path())).unwrap();
    }
    assert_eq!(
        run_transaction(&mut ops, "Metal").unwrap(),
        Outcome::PublishedNew
    );
    drop(ops);
    let before = inode(&installed);
    for slot in FileSlot::ALL {
        let path = installed.join(slot.path());
        assert_eq!(std::fs::read(&path).unwrap(), bytes[slot.index()]);
        assert_eq!(
            std::fs::metadata(path).unwrap().permissions().mode() & 0o7777,
            slot.mode()
        );
    }
    assert_eq!(
        run_transaction(
            &mut fixture.ops_with_source(fixture.source.embedded(&bytes).unwrap()),
            "Metal"
        )
        .unwrap(),
        Outcome::ReusedExisting
    );
    assert_eq!(before, inode(&installed));
}

#[test]
fn metal_native_embedded_corruption_and_partial_population_never_publish() {
    for slot in FileSlot::ALL {
        for corrupt in [false, true] {
            let fixture = Fixture::new();
            let bytes = fixture.source.embedded_bytes();
            let mut ops = fixture.ops_with_source(fixture.source.embedded(&bytes).unwrap());
            let fired = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let observed = fired.clone();
            ops.copy_hook = Some(Box::new(move |point, current, path| {
                if current == slot {
                    if corrupt && point == CopyPoint::AfterWrite {
                        observed.store(true, Ordering::SeqCst);
                        let mut bytes = std::fs::read(path)?;
                        bytes[0] ^= 1;
                        std::fs::write(path, bytes)?;
                    } else if !corrupt && point == CopyPoint::BeforeRead {
                        observed.store(true, Ordering::SeqCst);
                        bail!("injected embedded copy failure");
                    }
                }
                Ok(())
            }));
            assert!(run_transaction(&mut ops, "Metal").is_err());
            assert!(
                fired.load(Ordering::SeqCst),
                "fault hook was not exercised: {slot:?}/{corrupt}"
            );
            assert!(!fixture.installed().exists());
            assert!(
                std::fs::read_dir(fixture.version())
                    .unwrap()
                    .next()
                    .is_none()
            );
        }
    }
}

#[test]
fn metal_native_embedded_collision_verifies_winner_without_replacement() {
    for identical in [false, true] {
        let fixture = Fixture::new();
        let bytes = fixture.source.embedded_bytes();
        let mut ops = fixture.ops_with_source(fixture.source.embedded(&bytes).unwrap());
        sealed(&mut ops);
        if identical {
            let other = Fixture::new();
            run_transaction(&mut other.ops(), "Metal").unwrap();
            std::fs::rename(other.installed(), fixture.installed()).unwrap();
        } else {
            std::fs::create_dir(fixture.installed()).unwrap();
        }
        let before = inode(&fixture.installed());
        assert_eq!(ops.publish().unwrap(), PublishResult::Collision);
        if identical {
            assert!(ops.existing().unwrap());
        } else {
            assert!(ops.existing().is_err());
        }
        ops.cleanup().unwrap();
        if identical {
            ops.confirm().unwrap();
        }
        assert_eq!(before, inode(&fixture.installed()));
    }
}

#[test]
fn metal_native_embedded_uncertain_publication_and_confirmation_retain_final() {
    for ambiguous in [false, true] {
        let fixture = Fixture::new();
        let bytes = fixture.source.embedded_bytes();
        let mut ops = fixture.ops_with_source(fixture.source.embedded(&bytes).unwrap());
        sealed(&mut ops);
        assert_eq!(ops.publish().unwrap(), PublishResult::New);
        let before = inode(&fixture.installed());
        if ambiguous {
            ops.published = false;
        } else {
            std::fs::set_permissions(fixture.version(), std::fs::Permissions::from_mode(0o777))
                .unwrap();
            assert!(ops.confirm().is_err());
        }
        assert!(ops.cleanup().is_err());
        assert_eq!(before, inode(&fixture.installed()));
        if !ambiguous {
            std::fs::set_permissions(fixture.version(), std::fs::Permissions::from_mode(0o755))
                .unwrap();
        }
        drop(ops);
        assert_eq!(
            run_transaction(
                &mut fixture.ops_with_source(fixture.source.embedded(&bytes).unwrap()),
                "Metal"
            )
            .unwrap(),
            Outcome::ReusedExisting
        );
        assert_eq!(before, inode(&fixture.installed()));
    }
}

#[test]
fn metal_native_copy_reuse_preserves_identity_modes_and_old_versions() {
    let fixture = Fixture::new();
    assert_eq!(
        run_transaction(&mut fixture.ops(), "Metal").unwrap(),
        Outcome::PublishedNew
    );
    let installed = fixture.installed();
    let before = inode(&installed);
    let older = fixture.version().join("retained-older-version");
    std::fs::create_dir(&older).unwrap();
    for slot in FileSlot::ALL {
        let path = installed.join(slot.path());
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o7777,
            slot.mode()
        );
        assert_eq!(
            std::fs::read(path).unwrap(),
            std::fs::read(fixture.source.root().join(slot.path())).unwrap()
        );
    }
    assert_eq!(
        run_transaction(&mut fixture.ops(), "Metal").unwrap(),
        Outcome::ReusedExisting
    );
    assert_eq!(before, inode(&installed));
    assert!(older.is_dir() && fixture.store.join(LOCK_NAME).is_file());
    assert_eq!(std::fs::read_dir(fixture.version()).unwrap().count(), 2);
}

#[test]
fn metal_native_changed_source_after_open_never_publishes() {
    let fixture = Fixture::new();
    let mut ops = fixture.ops();
    let source_worker = fixture.source.root().join(FileSlot::Worker.path());
    ops.copy_hook = Some(Box::new(move |point, slot, _| {
        if point == CopyPoint::BeforeRead && slot == FileSlot::Worker {
            std::fs::write(&source_worker, b"forged worker")?;
        }
        Ok(())
    }));
    let destination = fixture.installed();
    assert!(run_transaction(&mut ops, "Metal").is_err());
    assert!(!destination.exists());
    assert!(
        std::fs::read_dir(destination.parent().unwrap())
            .unwrap()
            .next()
            .is_none()
    );
}

#[test]
fn metal_native_independent_readback_rejects_copied_corruption() {
    let fixture = Fixture::new();
    let mut ops = fixture.ops();
    ops.copy_hook = Some(Box::new(|point, slot, path| {
        if point == CopyPoint::AfterWrite && slot == FileSlot::Worker {
            std::fs::write(path, b"forged worker")?;
        }
        Ok(())
    }));
    assert!(run_transaction(&mut ops, "Metal").is_err());
    assert!(!fixture.installed().exists());
    assert!(
        std::fs::read_dir(fixture.version())
            .unwrap()
            .next()
            .is_none()
    );
}

#[test]
fn metal_native_partial_population_cleans_only_proven_created_nodes() {
    for fail_slot in FileSlot::ALL {
        let fixture = Fixture::new();
        let mut ops = fixture.ops();
        ops.copy_hook = Some(Box::new(move |point, slot, _| {
            if point == CopyPoint::BeforeRead && slot == fail_slot {
                bail!("injected partial population failure");
            }
            Ok(())
        }));
        assert!(run_transaction(&mut ops, "Metal").is_err());
        assert!(!fixture.installed().exists());
        assert!(
            std::fs::read_dir(fixture.version())
                .unwrap()
                .next()
                .is_none()
        );
    }
}

#[test]
fn metal_native_lock_is_permanent_and_contention_is_nonblocking() {
    let fixture = Fixture::new();
    let mut first = fixture.ops();
    first.prepare().unwrap();
    let before = first.lock.as_ref().unwrap().1;
    assert!(fixture.ops().prepare().is_err());
    drop(first);
    let mut retry = fixture.ops();
    retry.prepare().unwrap();
    assert_eq!(retry.lock.as_ref().unwrap().1, before);
}

#[test]
fn metal_native_invalid_lock_nodes_fail_without_repair() {
    for kind in ["symlink", "fifo", "hardlink", "mode"] {
        let fixture = Fixture::new();
        let lock = fixture.store.join(LOCK_NAME);
        match kind {
            "symlink" => symlink(&fixture.owner, &lock).unwrap(),
            "fifo" => {
                let name = CString::new(lock.as_os_str().as_bytes()).unwrap();
                assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
            }
            "hardlink" => {
                let original = fixture.owner.join("lock-alias");
                File::create_new(&original).unwrap();
                std::fs::set_permissions(&original, std::fs::Permissions::from_mode(0o600))
                    .unwrap();
                std::fs::hard_link(original, &lock).unwrap();
            }
            "mode" => {
                File::create_new(&lock).unwrap();
                std::fs::set_permissions(&lock, std::fs::Permissions::from_mode(0o666)).unwrap();
            }
            _ => unreachable!(),
        }
        let before = std::fs::symlink_metadata(&lock).unwrap();
        assert!(fixture.ops().prepare().is_err(), "{kind}");
        let after = std::fs::symlink_metadata(&lock).unwrap();
        assert_eq!(
            (before.dev(), before.ino(), before.mode()),
            (after.dev(), after.ino(), after.mode())
        );
        assert!(!fixture.installed().exists());
    }
}

#[test]
fn metal_native_existing_tamper_extra_links_modes_and_envelope_are_not_repaired() {
    for kind in [
        "worker", "metadata", "extra", "nested", "hardlink", "mode", "envelope",
    ] {
        let fixture = Fixture::new();
        run_transaction(&mut fixture.ops(), "Metal").unwrap();
        let installed = fixture.installed();
        match kind {
            "worker" => {
                std::fs::write(installed.join(FileSlot::Worker.path()), b"forged worker").unwrap()
            }
            "metadata" => std::fs::write(installed.join(FileSlot::Manifest.path()), b"{}").unwrap(),
            "extra" => {
                File::create_new(installed.join("unexpected")).unwrap();
            }
            "nested" => {
                File::create_new(installed.join("worker/unexpected")).unwrap();
            }
            "hardlink" => std::fs::hard_link(
                installed.join(FileSlot::Worker.path()),
                fixture.owner.join("alias"),
            )
            .unwrap(),
            "mode" => std::fs::set_permissions(
                installed.join(FileSlot::Worker.path()),
                std::fs::Permissions::from_mode(0o777),
            )
            .unwrap(),
            "envelope" => {
                let path = installed.join(FileSlot::Signature.path());
                let bytes = std::fs::read_to_string(&path)
                    .unwrap()
                    .replace("fixture-ed25519-v1", "fixture-ed25519-v2");
                std::fs::write(path, bytes).unwrap();
            }
            _ => unreachable!(),
        }
        let before = inode(&installed);
        assert!(
            run_transaction(&mut fixture.ops(), "Metal").is_err(),
            "{kind}"
        );
        assert_eq!(inode(&installed), before);
    }
}

#[test]
fn metal_native_unsafe_ancestors_and_destination_aliases_fail_closed() {
    for kind in ["id-mode", "id-symlink", "digest-symlink"] {
        let fixture = Fixture::new();
        let mut ops = fixture.ops();
        ops.prepare().unwrap();
        let id = fixture.version().parent().unwrap().to_path_buf();
        let destination = fixture.installed();
        drop(ops);
        match kind {
            "id-mode" => {
                std::fs::set_permissions(id, std::fs::Permissions::from_mode(0o777)).unwrap()
            }
            "id-symlink" => {
                std::fs::rename(&id, fixture.owner.join("retained-id")).unwrap();
                symlink(fixture.owner.join("retained-id"), id).unwrap();
            }
            "digest-symlink" => symlink(&fixture.owner, destination).unwrap(),
            _ => unreachable!(),
        }
        assert!(
            run_transaction(&mut fixture.ops(), "Metal").is_err(),
            "{kind}"
        );
    }
}

#[test]
fn metal_native_no_replace_collision_verifies_instead_of_overwriting() {
    let fixture = Fixture::new();
    let mut ops = fixture.ops();
    sealed(&mut ops);
    let installed = fixture.installed();
    std::fs::create_dir(&installed).unwrap();
    let before = inode(&installed);
    assert_eq!(ops.publish().unwrap(), PublishResult::Collision);
    assert!(ops.existing().is_err());
    ops.cleanup().unwrap();
    assert_eq!(inode(&installed), before);
    assert_eq!(std::fs::read_dir(&installed).unwrap().count(), 0);
}

#[test]
fn metal_native_identical_collision_is_independently_verified_and_reused() {
    let fixture = Fixture::new();
    let other = Fixture::new();
    let mut ops = fixture.ops();
    sealed(&mut ops);
    run_transaction(&mut other.ops(), "Metal").unwrap();
    std::fs::rename(other.installed(), fixture.installed()).unwrap();
    let before = inode(&fixture.installed());
    assert_eq!(ops.publish().unwrap(), PublishResult::Collision);
    assert!(ops.existing().unwrap());
    ops.cleanup().unwrap();
    ops.confirm().unwrap();
    assert_eq!(inode(&fixture.installed()), before);
    assert_eq!(std::fs::read_dir(fixture.version()).unwrap().count(), 1);
}

#[test]
fn metal_native_interrupted_private_stages_are_not_adopted_or_swept() {
    for phase in 0..3 {
        let fixture = Fixture::new();
        let mut abandoned = fixture.ops();
        abandoned.prepare().unwrap();
        abandoned.stage().unwrap();
        if phase > 0 {
            abandoned.populate().unwrap();
        }
        if phase > 1 {
            abandoned.seal().unwrap();
        }
        let stage = fixture
            .version()
            .join(&abandoned.stage.as_ref().unwrap().name);
        let before = inode(&stage);
        drop(abandoned);
        assert_eq!(
            run_transaction(&mut fixture.ops(), "Metal").unwrap(),
            Outcome::PublishedNew
        );
        assert!(stage.is_dir());
        assert_eq!(inode(&stage), before);
        assert_ne!(inode(&fixture.installed()), before);
    }
}

#[test]
fn metal_native_real_mutation_acls_reject_ancestry_and_cleanup_before_unlink() {
    for child_acl in [false, true] {
        let fixture = Fixture::new();
        let mut ops = fixture.ops();
        sealed(&mut ops);
        let stage = fixture.version().join(&ops.stage.as_ref().unwrap().name);
        let path = if child_acl {
            stage.join("worker")
        } else {
            fixture.version()
        };
        // The same unprivileged private-fixture ACL mechanism used by CPU
        // component authoring tests; no shell, elevation, or production root.
        assert!(
            std::process::Command::new("/bin/chmod")
                .args(["+a", "everyone allow delete_child,add_subdirectory"])
                .arg(&path)
                .status()
                .unwrap()
                .success()
        );
        let rejected = ops.cleanup().is_err();
        let untouched = stage.join(FileSlot::Manifest.path()).is_file();
        assert!(
            std::process::Command::new("/bin/chmod")
                .arg("-N")
                .arg(&path)
                .status()
                .unwrap()
                .success()
        );
        assert!(rejected && untouched);
        ops.cleanup().unwrap();
    }
}

#[test]
fn metal_native_ambiguous_rename_retains_final_and_retry_reuses_it() {
    let fixture = Fixture::new();
    let mut ops = fixture.ops();
    sealed(&mut ops);
    assert_eq!(ops.publish().unwrap(), PublishResult::New);
    ops.published = false; // Model an applied rename with an error return.
    assert!(ops.cleanup().is_err());
    let before = inode(&fixture.installed());
    drop(ops);
    assert_eq!(
        run_transaction(&mut fixture.ops(), "Metal").unwrap(),
        Outcome::ReusedExisting
    );
    assert_eq!(inode(&fixture.installed()), before);
}

#[test]
fn metal_native_failed_confirmation_retains_final_for_verified_retry() {
    let fixture = Fixture::new();
    let mut ops = fixture.ops();
    sealed(&mut ops);
    ops.publish().unwrap();
    let version = fixture.version();
    let before = inode(&fixture.installed());
    std::fs::set_permissions(&version, std::fs::Permissions::from_mode(0o777)).unwrap();
    assert!(ops.confirm().is_err() && ops.cleanup().is_err());
    assert_eq!(inode(&fixture.installed()), before);
    std::fs::set_permissions(&version, std::fs::Permissions::from_mode(0o755)).unwrap();
    drop(ops);
    assert_eq!(
        run_transaction(&mut fixture.ops(), "Metal").unwrap(),
        Outcome::ReusedExisting
    );
}

#[test]
fn metal_native_cleanup_rejects_any_unknown_or_replaced_child_before_unlink() {
    for kind in [
        "root-extra",
        "nested-extra",
        "worker-replaced",
        "file-replaced",
        "lock-replaced",
        "unproven",
    ] {
        let fixture = Fixture::new();
        let mut ops = fixture.ops();
        sealed(&mut ops);
        let stage = ops.stage.as_ref().unwrap();
        let path = fixture.version().join(&stage.name);
        match kind {
            "root-extra" => {
                File::create_new(path.join("unknown")).unwrap();
            }
            "nested-extra" => {
                File::create_new(path.join("worker/unknown")).unwrap();
            }
            "worker-replaced" => {
                std::fs::rename(path.join("worker"), fixture.owner.join("retained-worker"))
                    .unwrap();
                std::fs::create_dir(path.join("worker")).unwrap();
            }
            "file-replaced" => {
                std::fs::rename(
                    path.join(FileSlot::Worker.path()),
                    fixture.owner.join("retained-image"),
                )
                .unwrap();
                File::create_new(path.join(FileSlot::Worker.path())).unwrap();
            }
            "lock-replaced" => {
                std::fs::rename(
                    fixture.store.join(LOCK_NAME),
                    fixture.store.join("retained-lock"),
                )
                .unwrap();
                File::create_new(fixture.store.join(LOCK_NAME)).unwrap();
                std::fs::set_permissions(
                    fixture.store.join(LOCK_NAME),
                    std::fs::Permissions::from_mode(0o600),
                )
                .unwrap();
            }
            "unproven" => ops.stage.as_mut().unwrap().unproven_child = true,
            _ => unreachable!(),
        }
        assert!(ops.cleanup().is_err(), "{kind}");
        assert!(
            path.join(FileSlot::Manifest.path()).is_file(),
            "no unlink before complete proof: {kind}"
        );
    }
}

#[test]
fn metal_native_cleanup_rejects_replaced_stage_and_unknown_created_inode() {
    let fixture = Fixture::new();
    let mut ops = fixture.ops();
    ops.prepare().unwrap();
    ops.stage().unwrap();
    let stage = fixture.version().join(&ops.stage.as_ref().unwrap().name);
    let retained = fixture.owner.join("retained-stage");
    std::fs::rename(&stage, &retained).unwrap();
    std::fs::create_dir(&stage).unwrap();
    std::fs::set_permissions(&stage, std::fs::Permissions::from_mode(0o700)).unwrap();
    File::create_new(stage.join("replacement")).unwrap();
    assert!(ops.cleanup().is_err());
    assert!(retained.is_dir() && stage.join("replacement").is_file());
    drop(ops);
    let mut uncertain = fixture.ops();
    uncertain.prepare().unwrap();
    let name = ".metal-unproven-fixture";
    let path = fixture.version().join(name);
    std::fs::create_dir(&path).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o777)).unwrap();
    assert!(uncertain.retain_stage(name.to_owned()).is_err());
    assert!(uncertain.cleanup().is_err() && path.is_dir());
}
