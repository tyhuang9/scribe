use std::collections::BTreeMap;
#[cfg(not(windows))]
use std::fs::File;
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use getrandom::fill;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::manifest::{
    EMBEDDED_MINIMUM_SECURITY_EPOCH, PackVerificationError, PackVerifier, VerifiedPack,
};

const STATE_SCHEMA_VERSION: u16 = 1;
const MAX_STATE_BYTES: u64 = 256 * 1024;

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ActivationState {
    schema_version: u16,
    pub(crate) current: Option<VerifiedPack>,
    pub(crate) previous: Option<VerifiedPack>,
}

impl ActivationState {
    fn empty() -> Self {
        Self {
            schema_version: STATE_SCHEMA_VERSION,
            current: None,
            previous: None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct EpochState {
    schema_version: u16,
    epochs: BTreeMap<String, u64>,
}

impl EpochState {
    fn empty() -> Self {
        Self {
            schema_version: STATE_SCHEMA_VERSION,
            epochs: BTreeMap::new(),
        }
    }
}

pub(crate) struct PackStore<'a> {
    packs_root: PathBuf,
    state_root: PathBuf,
    verifier: &'a PackVerifier<'a>,
}

impl<'a> PackStore<'a> {
    pub(crate) fn new(
        workers_root: impl Into<PathBuf>,
        private_state_root: impl Into<PathBuf>,
        verifier: &'a PackVerifier<'a>,
    ) -> Self {
        Self {
            packs_root: workers_root.into().join("packs"),
            state_root: private_state_root.into(),
            verifier,
        }
    }

    pub(crate) fn stage_and_install(
        &self,
        signed_source: &Path,
    ) -> Result<VerifiedPack, PackStoreError> {
        let source = self.verifier.verify(signed_source)?;
        self.require_epoch(&source)?;
        let parent = self
            .packs_root
            .join(&source.pack_id)
            .join(&source.pack_version);
        ensure_regular_directories(&parent)?;
        let final_root = parent.join(&source.pack_digest);
        if final_root.exists() {
            let installed = self.verifier.verify(&final_root)?;
            require_same_pack(&source, &installed)?;
            return Ok(installed);
        }

        let staging = parent.join(format!(
            ".{}.staging-{}",
            source.pack_digest,
            random_suffix()?
        ));
        create_new_private_directory(&staging)?;
        let result = (|| {
            copy_verified_tree(signed_source, &staging)?;
            let staged = self.verifier.verify(&staging)?;
            require_same_pack(&source, &staged)?;
            make_payload_readonly(&staging)?;
            durable_rename_new(&staging, &final_root)?;
            let installed = self.verifier.verify(&final_root)?;
            require_same_pack(&source, &installed)?;
            Ok(installed)
        })();
        if result.is_err() {
            let _ = remove_staging_tree(&staging);
        }
        result
    }

    pub(crate) fn activate(&self, descriptor: &VerifiedPack) -> Result<(), PackStoreError> {
        let verified = self.reverify_descriptor(descriptor)?;
        self.require_epoch(&verified)?;
        let mut epochs = self.load_epochs_strict()?;
        let old_floor = epochs
            .epochs
            .get(&verified.pack_id)
            .copied()
            .unwrap_or(EMBEDDED_MINIMUM_SECURITY_EPOCH);
        epochs.epochs.insert(
            verified.pack_id.clone(),
            old_floor.max(verified.security_epoch),
        );
        self.persist_epochs(&epochs)?;

        let old = self.load_activation_fail_closed();
        self.persist_activation(&ActivationState {
            schema_version: STATE_SCHEMA_VERSION,
            current: Some(verified),
            previous: old.current,
        })
    }

    pub(crate) fn rollback(&self) -> Result<VerifiedPack, PackStoreError> {
        let state = self.load_activation_strict()?;
        let previous = state.previous.ok_or(PackStoreError::NoRollbackPack)?;
        let rollback = self.reverify_descriptor(&previous)?;
        self.require_epoch(&rollback)?;
        let prior_current = state
            .current
            .as_ref()
            .and_then(|current| self.reverify_descriptor(current).ok())
            .filter(|current| self.require_epoch(current).is_ok());
        self.persist_activation(&ActivationState {
            schema_version: STATE_SCHEMA_VERSION,
            current: Some(rollback.clone()),
            previous: prior_current,
        })?;
        Ok(rollback)
    }

    /// Corrupt state or invalid packs project to no GPU pack and cannot affect
    /// the separately compiled CPU route.
    pub(crate) fn current_fail_closed(&self) -> Option<VerifiedPack> {
        let descriptor = self.load_activation_fail_closed().current?;
        self.reverify_descriptor(&descriptor)
            .ok()
            .filter(|pack| self.require_epoch(pack).is_ok())
    }

    /// Only uniquely named incomplete staging directories and state temporary
    /// files are removed. Final digest trees and state records are untouched.
    pub(crate) fn recover_interrupted_work(&self) -> Result<(), PackStoreError> {
        if self.packs_root.exists() {
            for pack_id in read_regular_directory(&self.packs_root)? {
                for version in read_regular_directory(&pack_id)? {
                    for entry in read_regular_directory(&version)? {
                        let name = entry
                            .file_name()
                            .and_then(|name| name.to_str())
                            .unwrap_or("");
                        if name.starts_with('.') && name.contains(".staging-") {
                            remove_staging_tree(&entry)?;
                        }
                    }
                }
            }
        }
        if self.state_root.exists() {
            for entry in read_regular_directory(&self.state_root)? {
                let name = entry
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("");
                if name.starts_with('.') && name.contains(".tmp-") {
                    let metadata = fs::symlink_metadata(&entry)?;
                    if metadata.is_file() && !is_link_or_reparse(&metadata) {
                        fs::remove_file(entry)?;
                    }
                }
            }
        }
        Ok(())
    }

    fn reverify_descriptor(
        &self,
        descriptor: &VerifiedPack,
    ) -> Result<VerifiedPack, PackStoreError> {
        let expected_root = self
            .packs_root
            .join(&descriptor.pack_id)
            .join(&descriptor.pack_version)
            .join(&descriptor.pack_digest);
        if descriptor.root != expected_root {
            return Err(PackStoreError::DescriptorOutsideStore);
        }
        let verified = self.verifier.verify(&expected_root)?;
        if &verified != descriptor {
            return Err(PackStoreError::DescriptorChanged);
        }
        Ok(verified)
    }

    fn require_epoch(&self, descriptor: &VerifiedPack) -> Result<(), PackStoreError> {
        let epochs = self.load_epochs_strict()?;
        let floor = epochs
            .epochs
            .get(&descriptor.pack_id)
            .copied()
            .unwrap_or(EMBEDDED_MINIMUM_SECURITY_EPOCH)
            .max(EMBEDDED_MINIMUM_SECURITY_EPOCH);
        if descriptor.security_epoch < floor {
            return Err(PackStoreError::SecurityEpochDowngrade {
                observed: descriptor.security_epoch,
                floor,
            });
        }
        Ok(())
    }

    fn activation_path(&self) -> PathBuf {
        self.state_root.join("activation.json")
    }

    fn epoch_path(&self) -> PathBuf {
        self.state_root.join("security-epochs.json")
    }

    fn load_activation_fail_closed(&self) -> ActivationState {
        self.load_activation_strict()
            .unwrap_or_else(|_| ActivationState::empty())
    }

    fn load_activation_strict(&self) -> Result<ActivationState, PackStoreError> {
        let path = self.activation_path();
        if !path.exists() {
            return Ok(ActivationState::empty());
        }
        let state: ActivationState = read_canonical_state(&path)?;
        if state.schema_version != STATE_SCHEMA_VERSION {
            return Err(PackStoreError::CorruptState("activation schema"));
        }
        Ok(state)
    }

    fn load_epochs_strict(&self) -> Result<EpochState, PackStoreError> {
        let path = self.epoch_path();
        if !path.exists() {
            return Ok(EpochState::empty());
        }
        let state: EpochState = read_canonical_state(&path)?;
        if state.schema_version != STATE_SCHEMA_VERSION
            || state.epochs.len() > 256
            || state
                .epochs
                .values()
                .any(|epoch| *epoch < EMBEDDED_MINIMUM_SECURITY_EPOCH)
        {
            return Err(PackStoreError::CorruptState("security epoch state"));
        }
        Ok(state)
    }

    fn persist_activation(&self, state: &ActivationState) -> Result<(), PackStoreError> {
        atomic_write_canonical(&self.activation_path(), state)
    }

    fn persist_epochs(&self, state: &EpochState) -> Result<(), PackStoreError> {
        atomic_write_canonical(&self.epoch_path(), state)
    }
}

fn require_same_pack(
    expected: &VerifiedPack,
    observed: &VerifiedPack,
) -> Result<(), PackStoreError> {
    if expected.pack_id != observed.pack_id
        || expected.pack_version != observed.pack_version
        || expected.pack_digest != observed.pack_digest
        || expected.security_epoch != observed.security_epoch
        || expected.runtime_abi_version != observed.runtime_abi_version
        || expected.backend != observed.backend
        || expected.provider != observed.provider
        || expected.target_os != observed.target_os
        || expected.target_arch != observed.target_arch
        || expected.worker_relative_path != observed.worker_relative_path
    {
        return Err(PackStoreError::DescriptorChanged);
    }
    Ok(())
}

fn copy_verified_tree(source: &Path, destination: &Path) -> Result<(), PackStoreError> {
    let mut pending = vec![(source.to_path_buf(), destination.to_path_buf())];
    while let Some((source_dir, destination_dir)) = pending.pop() {
        for entry in fs::read_dir(&source_dir)? {
            let entry = entry?;
            let source_path = entry.path();
            let destination_path = destination_dir.join(entry.file_name());
            let metadata = fs::symlink_metadata(&source_path)?;
            if is_link_or_reparse(&metadata) {
                return Err(PackStoreError::UnsafeFilesystemEntry(source_path));
            }
            if metadata.is_dir() {
                create_new_private_directory(&destination_path)?;
                pending.push((source_path, destination_path));
            } else if metadata.is_file() {
                copy_regular_no_follow(&source_path, &destination_path)?;
            } else {
                return Err(PackStoreError::UnsafeFilesystemEntry(source_path));
            }
        }
    }
    Ok(())
}

fn copy_regular_no_follow(source: &Path, destination: &Path) -> Result<(), PackStoreError> {
    let mut source_options = OpenOptions::new();
    source_options.read(true);
    configure_no_follow(&mut source_options);
    let mut input = source_options.open(source)?;
    let metadata = input.metadata()?;
    if !metadata.is_file() || is_link_or_reparse(&metadata) {
        return Err(PackStoreError::UnsafeFilesystemEntry(source.to_path_buf()));
    }

    let mut destination_options = OpenOptions::new();
    destination_options.write(true).create_new(true);
    configure_private_create(&mut destination_options);
    configure_no_follow(&mut destination_options);
    let mut output = destination_options.open(destination)?;
    io::copy(&mut input, &mut output)?;
    output.flush()?;
    output.sync_all()?;
    Ok(())
}

fn make_payload_readonly(root: &Path) -> Result<(), PackStoreError> {
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path)?;
            if metadata.is_dir() {
                pending.push(path);
            } else if metadata.is_file() {
                let mut permissions = metadata.permissions();
                permissions.set_readonly(true);
                fs::set_permissions(path, permissions)?;
            }
        }
    }
    Ok(())
}

fn remove_staging_tree(path: &Path) -> Result<(), PackStoreError> {
    if !path.exists() {
        return Ok(());
    }
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    if !name.starts_with('.') || !name.contains(".staging-") {
        return Err(PackStoreError::UnsafeRecoveryTarget(path.to_path_buf()));
    }
    let mut directories = vec![path.to_path_buf()];
    let mut ordered = Vec::new();
    while let Some(directory) = directories.pop() {
        ordered.push(directory.clone());
        for entry in fs::read_dir(&directory)? {
            let entry = entry?;
            let child = entry.path();
            let metadata = fs::symlink_metadata(&child)?;
            if is_link_or_reparse(&metadata) {
                return Err(PackStoreError::UnsafeFilesystemEntry(child));
            }
            if metadata.is_dir() {
                directories.push(child);
            } else if metadata.is_file() {
                let mut permissions = metadata.permissions();
                permissions.set_readonly(false);
                fs::set_permissions(&child, permissions)?;
                fs::remove_file(child)?;
            }
        }
    }
    for directory in ordered.into_iter().rev() {
        fs::remove_dir(directory)?;
    }
    Ok(())
}

fn ensure_regular_directories(path: &Path) -> Result<(), PackStoreError> {
    let existing = path
        .ancestors()
        .find(|ancestor| ancestor.exists())
        .ok_or(PackStoreError::CorruptState("directory root"))?;
    for ancestor in existing.ancestors() {
        let metadata = fs::symlink_metadata(ancestor)?;
        if !metadata.is_dir() || is_link_or_reparse(&metadata) {
            return Err(PackStoreError::UnsafeFilesystemEntry(
                ancestor.to_path_buf(),
            ));
        }
    }
    let missing = path
        .ancestors()
        .take_while(|ancestor| *ancestor != existing)
        .collect::<Vec<_>>();
    for directory in missing.into_iter().rev() {
        create_new_private_directory(directory)?;
        let metadata = fs::symlink_metadata(directory)?;
        if !metadata.is_dir() || is_link_or_reparse(&metadata) {
            return Err(PackStoreError::UnsafeFilesystemEntry(
                directory.to_path_buf(),
            ));
        }
    }
    Ok(())
}

fn create_new_private_directory(path: &Path) -> Result<(), PackStoreError> {
    fs::create_dir(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn read_regular_directory(path: &Path) -> Result<Vec<PathBuf>, PackStoreError> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir() || is_link_or_reparse(&metadata) {
        return Err(PackStoreError::UnsafeFilesystemEntry(path.to_path_buf()));
    }
    fs::read_dir(path)?
        .map(|entry| entry.map(|entry| entry.path()).map_err(PackStoreError::Io))
        .collect()
}

fn read_canonical_state<T>(path: &Path) -> Result<T, PackStoreError>
where
    T: for<'de> Deserialize<'de> + Serialize,
{
    let mut options = OpenOptions::new();
    options.read(true);
    configure_no_follow(&mut options);
    let mut file = options.open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || is_link_or_reparse(&metadata) || metadata.len() > MAX_STATE_BYTES {
        return Err(PackStoreError::CorruptState("state file bounds"));
    }
    super::manifest::reject_named_streams(path)?;
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.read_to_end(&mut bytes)?;
    let value = serde_json::from_slice::<T>(&bytes)?;
    if serde_json::to_vec(&value)? != bytes {
        return Err(PackStoreError::CorruptState("noncanonical state"));
    }
    Ok(value)
}

fn atomic_write_canonical<T: Serialize>(path: &Path, value: &T) -> Result<(), PackStoreError> {
    let parent = path
        .parent()
        .ok_or(PackStoreError::CorruptState("state parent"))?;
    ensure_regular_directories(parent)?;
    let temporary = parent.join(format!(
        ".{}.tmp-{}",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("state"),
        random_suffix()?
    ));
    let bytes = serde_json::to_vec(value)?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    configure_private_create(&mut options);
    configure_no_follow(&mut options);
    let mut file = options.open(&temporary)?;
    file.write_all(&bytes)?;
    file.flush()?;
    file.sync_all()?;
    drop(file);
    let result = durable_replace(&temporary, path);
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn random_suffix() -> Result<String, PackStoreError> {
    let mut bytes = [0_u8; 16];
    fill(&mut bytes).map_err(|error| PackStoreError::Random(error.to_string()))?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

#[cfg(unix)]
fn configure_no_follow(options: &mut OpenOptions) {
    use std::os::unix::fs::OpenOptionsExt;
    options.custom_flags(libc::O_NOFOLLOW);
}

#[cfg(windows)]
fn configure_no_follow(options: &mut OpenOptions) {
    use std::os::windows::fs::OpenOptionsExt;
    options.custom_flags(windows_sys::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT);
}

#[cfg(not(any(unix, windows)))]
fn configure_no_follow(_options: &mut OpenOptions) {}

#[cfg(unix)]
fn configure_private_create(options: &mut OpenOptions) {
    use std::os::unix::fs::OpenOptionsExt;
    options.mode(0o600);
}

#[cfg(not(unix))]
fn configure_private_create(_options: &mut OpenOptions) {}

#[cfg(windows)]
fn is_link_or_reparse(metadata: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    metadata.file_type().is_symlink()
        || metadata.file_attributes()
            & windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT
            != 0
}

#[cfg(not(windows))]
fn is_link_or_reparse(metadata: &fs::Metadata) -> bool {
    metadata.file_type().is_symlink()
}

#[cfg(windows)]
fn durable_rename_new(source: &Path, destination: &Path) -> Result<(), PackStoreError> {
    move_file(source, destination, false)
}

#[cfg(not(windows))]
fn durable_rename_new(source: &Path, destination: &Path) -> Result<(), PackStoreError> {
    fs::rename(source, destination)?;
    sync_parent(destination)?;
    Ok(())
}

#[cfg(windows)]
fn durable_replace(source: &Path, destination: &Path) -> Result<(), PackStoreError> {
    move_file(source, destination, true)
}

#[cfg(not(windows))]
fn durable_replace(source: &Path, destination: &Path) -> Result<(), PackStoreError> {
    fs::rename(source, destination)?;
    sync_parent(destination)?;
    Ok(())
}

#[cfg(windows)]
fn move_file(source: &Path, destination: &Path, replace: bool) -> Result<(), PackStoreError> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };
    let source = source
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let destination = destination
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let flags = MOVEFILE_WRITE_THROUGH
        | if replace {
            MOVEFILE_REPLACE_EXISTING
        } else {
            0
        };
    if unsafe { MoveFileExW(source.as_ptr(), destination.as_ptr(), flags) } == 0 {
        return Err(PackStoreError::Io(io::Error::last_os_error()));
    }
    Ok(())
}

#[cfg(not(windows))]
fn sync_parent(path: &Path) -> Result<(), PackStoreError> {
    let parent = path
        .parent()
        .ok_or(PackStoreError::CorruptState("rename parent"))?;
    File::open(parent)?.sync_all()?;
    Ok(())
}

#[derive(Debug, Error)]
pub(crate) enum PackStoreError {
    #[error("worker-pack store I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("worker-pack verification failed: {0}")]
    Verification(#[from] PackVerificationError),
    #[error("worker-pack store JSON failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("worker-pack store randomness failed: {0}")]
    Random(String),
    #[error("worker-pack descriptor changed")]
    DescriptorChanged,
    #[error("worker-pack descriptor points outside the immutable store")]
    DescriptorOutsideStore,
    #[error("worker-pack security epoch {observed} is below high-water floor {floor}")]
    SecurityEpochDowngrade { observed: u64, floor: u64 },
    #[error("worker-pack activation state is corrupt: {0}")]
    CorruptState(&'static str),
    #[error("no verified previous worker pack is available for rollback")]
    NoRollbackPack,
    #[error("worker-pack store contains an unsafe filesystem entry: {0}")]
    UnsafeFilesystemEntry(PathBuf),
    #[error("refused unsafe worker-pack recovery target: {0}")]
    UnsafeRecoveryTarget(PathBuf),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gpu_worker_pack::manifest::test_support::{
        base_manifest, fixture, temp_root, write_signed,
    };
    use crate::gpu_worker_pack::manifest::{Compatibility, PackBackend};

    fn store_fixture(
        label: &str,
    ) -> (
        PathBuf,
        PathBuf,
        PathBuf,
        PackVerifier<'static>,
        VerifiedPack,
    ) {
        let root = temp_root(label);
        let source = root.join("source");
        fs::create_dir(&source).unwrap();
        let (verifier, source_descriptor) = fixture(&source);
        let workers = root.join("workers");
        let state = root.join("private-state");
        (root, workers, state, verifier, source_descriptor)
    }

    fn additional_source(
        root: &Path,
        version: &str,
        epoch: u64,
    ) -> (
        PathBuf,
        &'static super::super::manifest::test_support::FixtureTrustRoot,
    ) {
        let source = root.join(format!("source-{version}-{epoch}"));
        fs::create_dir(&source).unwrap();
        let mut manifest = base_manifest();
        manifest.pack_version = version.to_owned();
        manifest.security_epoch = epoch;
        let trust = write_signed(&source, manifest);
        (source, trust)
    }

    #[test]
    fn stages_into_digest_layout_and_activates_only_reverified_pack() {
        let (root, workers, state, verifier, _) = store_fixture("store-install");
        let store = PackStore::new(&workers, &state, &verifier);
        let installed = store.stage_and_install(&root.join("source")).unwrap();
        assert_eq!(
            installed.root,
            workers
                .join("packs")
                .join(&installed.pack_id)
                .join(&installed.pack_version)
                .join(&installed.pack_digest)
        );
        store.activate(&installed).unwrap();
        assert_eq!(store.current_fail_closed(), Some(installed.clone()));
        assert_eq!(
            store.stage_and_install(&root.join("source")).unwrap(),
            installed
        );
        assert!(
            fs::metadata(installed.worker_path())
                .unwrap()
                .permissions()
                .readonly()
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rollback_reverifies_previous_and_respects_epoch_high_water() {
        let (root, workers, state, verifier, _) = store_fixture("store-rollback");
        let store = PackStore::new(&workers, &state, &verifier);
        let first = store.stage_and_install(&root.join("source")).unwrap();
        let (second_source, _) = additional_source(&root, "2.0.0", 1);
        let second = store.stage_and_install(&second_source).unwrap();
        store.activate(&first).unwrap();
        store.activate(&second).unwrap();
        assert_eq!(store.current_fail_closed(), Some(second.clone()));
        assert_eq!(store.rollback().unwrap(), first);
        assert_eq!(store.current_fail_closed().unwrap().pack_version, "1.2.3");

        let (epoch_two_source, trust) = additional_source(&root, "3.0.0", 2);
        let verifier_two = PackVerifier::new(
            trust,
            Compatibility {
                app_build: crate::onnx_worker::DESKTOP_BUILD_ID,
                worker_build: crate::onnx_worker::INFERENCE_WORKER_BUILD_ID,
                target_os: std::env::consts::OS,
                target_arch: std::env::consts::ARCH,
                allowed_backends: &[PackBackend::Vulkan],
            },
        );
        let epoch_store = PackStore::new(&workers, &state, &verifier_two);
        let epoch_two = epoch_store.stage_and_install(&epoch_two_source).unwrap();
        epoch_store.activate(&epoch_two).unwrap();
        assert!(matches!(
            epoch_store.rollback(),
            Err(PackStoreError::SecurityEpochDowngrade {
                observed: 1,
                floor: 2
            })
        ));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn corrupt_state_and_interrupted_work_fail_closed_without_losing_cpu_safety() {
        let (root, workers, state, verifier, _) = store_fixture("store-recovery");
        let store = PackStore::new(&workers, &state, &verifier);
        let installed = store.stage_and_install(&root.join("source")).unwrap();
        store.activate(&installed).unwrap();
        fs::write(store.activation_path(), b"{corrupt").unwrap();
        assert_eq!(store.current_fail_closed(), None);

        let staging = workers
            .join("packs")
            .join(&installed.pack_id)
            .join(&installed.pack_version)
            .join(format!(".{}.staging-interrupted", installed.pack_digest));
        fs::create_dir(&staging).unwrap();
        fs::write(staging.join("partial"), b"partial").unwrap();
        fs::create_dir_all(&state).unwrap();
        let temporary = state.join(".activation.json.tmp-interrupted");
        fs::write(&temporary, b"partial").unwrap();
        store.recover_interrupted_work().unwrap();
        assert!(!staging.exists());
        assert!(!temporary.exists());
        assert!(installed.root.exists());

        fs::write(store.epoch_path(), b"not-json").unwrap();
        assert_eq!(store.current_fail_closed(), None);
        assert!(matches!(
            store.activate(&installed),
            Err(PackStoreError::Json(_))
        ));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn corrupt_previous_descriptor_cannot_be_rolled_back() {
        let (root, workers, state, verifier, _) = store_fixture("store-corrupt-previous");
        let store = PackStore::new(&workers, &state, &verifier);
        let first = store.stage_and_install(&root.join("source")).unwrap();
        let (second_source, _) = additional_source(&root, "2.0.0", 1);
        let second = store.stage_and_install(&second_source).unwrap();
        store.activate(&first).unwrap();
        store.activate(&second).unwrap();
        let mut activation = store.load_activation_strict().unwrap();
        activation.previous.as_mut().unwrap().root = root.join("outside-store");
        store.persist_activation(&activation).unwrap();
        assert!(matches!(
            store.rollback(),
            Err(PackStoreError::DescriptorOutsideStore)
        ));
        assert_eq!(store.current_fail_closed(), Some(second));
        fs::remove_dir_all(root).unwrap();
    }
}
