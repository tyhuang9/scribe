use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
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
const STORE_LOCK_NAME: &str = ".worker-pack-store.lock";

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

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PendingActivation {
    schema_version: u16,
    target: VerifiedPack,
    prior_activation: ActivationState,
    next_activation: ActivationState,
    prior_epochs: EpochState,
    next_epochs: EpochState,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PublishOutcome {
    Published,
    DestinationExists,
}

pub(super) struct ExclusiveFileLock {
    file: File,
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
        let _lock = self.acquire_lock()?;
        self.recover_pending_activation_locked()?;
        let epochs = self.load_epochs_strict()?;
        require_epoch_from(&source, &epochs)?;
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
            if durable_rename_new(&staging, &final_root)? == PublishOutcome::DestinationExists {
                remove_staging_tree(&staging)?;
            }
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
        let _lock = self.acquire_lock()?;
        self.recover_pending_activation_locked()?;
        self.activate_locked(descriptor, None)
    }

    fn activate_locked(
        &self,
        descriptor: &VerifiedPack,
        #[cfg(test)] interrupt_after: Option<ActivationBoundary>,
        #[cfg(not(test))] _interrupt_after: Option<()>,
    ) -> Result<(), PackStoreError> {
        let verified = self.reverify_descriptor(descriptor)?;
        let prior_epochs = self.load_epochs_strict()?;
        require_epoch_from(&verified, &prior_epochs)?;
        let prior_activation = self.load_activation_strict()?;
        let mut next_epochs = prior_epochs.clone();
        let old_floor = next_epochs
            .epochs
            .get(&verified.pack_id)
            .copied()
            .unwrap_or(EMBEDDED_MINIMUM_SECURITY_EPOCH);
        next_epochs.epochs.insert(
            verified.pack_id.clone(),
            old_floor.max(verified.security_epoch),
        );
        let next_activation = ActivationState {
            schema_version: STATE_SCHEMA_VERSION,
            current: Some(verified.clone()),
            previous: prior_activation.current.clone(),
        };
        let pending = PendingActivation {
            schema_version: STATE_SCHEMA_VERSION,
            target: verified,
            prior_activation,
            next_activation: next_activation.clone(),
            prior_epochs,
            next_epochs: next_epochs.clone(),
        };
        self.persist_pending(&pending)?;
        #[cfg(test)]
        if interrupt_after == Some(ActivationBoundary::Journal) {
            return Err(PackStoreError::InjectedInterruption);
        }
        self.persist_epochs(&next_epochs)?;
        #[cfg(test)]
        if interrupt_after == Some(ActivationBoundary::Epochs) {
            return Err(PackStoreError::InjectedInterruption);
        }
        self.persist_activation(&next_activation)?;
        #[cfg(test)]
        if interrupt_after == Some(ActivationBoundary::Activation) {
            return Err(PackStoreError::InjectedInterruption);
        }
        self.remove_pending()?;
        Ok(())
    }

    pub(crate) fn rollback(&self) -> Result<VerifiedPack, PackStoreError> {
        let _lock = self.acquire_lock()?;
        self.recover_pending_activation_locked()?;
        let state = self.load_activation_strict()?;
        let previous = state.previous.ok_or(PackStoreError::NoRollbackPack)?;
        let rollback = self.reverify_descriptor(&previous)?;
        let epochs = self.load_epochs_strict()?;
        require_epoch_from(&rollback, &epochs)?;
        let prior_current = state
            .current
            .as_ref()
            .and_then(|current| self.reverify_descriptor(current).ok())
            .filter(|current| require_epoch_from(current, &epochs).is_ok());
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
        let _lock = self.acquire_lock().ok()?;
        self.recover_pending_activation_locked().ok()?;
        let descriptor = self.load_activation_strict().ok()?.current?;
        let epochs = self.load_epochs_strict().ok()?;
        self.reverify_descriptor(&descriptor)
            .ok()
            .filter(|pack| require_epoch_from(pack, &epochs).is_ok())
    }

    /// Only uniquely named incomplete staging directories and state temporary
    /// files are removed. Final digest trees and state records are untouched.
    pub(crate) fn recover_interrupted_work(&self) -> Result<(), PackStoreError> {
        let _lock = self.acquire_lock()?;
        self.recover_pending_activation_locked()?;
        self.recover_interrupted_work_locked()
    }

    fn recover_interrupted_work_locked(&self) -> Result<(), PackStoreError> {
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

    fn acquire_lock(&self) -> Result<ExclusiveFileLock, PackStoreError> {
        exclusive_file_lock(&self.state_root.join(STORE_LOCK_NAME))
    }

    fn activation_path(&self) -> PathBuf {
        self.state_root.join("activation.json")
    }

    fn epoch_path(&self) -> PathBuf {
        self.state_root.join("security-epochs.json")
    }

    fn pending_path(&self) -> PathBuf {
        self.state_root.join("pending-activation.json")
    }

    fn load_activation_strict(&self) -> Result<ActivationState, PackStoreError> {
        let path = self.activation_path();
        if !entry_exists(&path)? {
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
        if !entry_exists(&path)? {
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

    fn persist_pending(&self, pending: &PendingActivation) -> Result<(), PackStoreError> {
        atomic_write_canonical(&self.pending_path(), pending)
    }

    fn remove_pending(&self) -> Result<(), PackStoreError> {
        remove_regular_state_file(&self.pending_path())
    }

    fn recover_pending_activation_locked(&self) -> Result<(), PackStoreError> {
        let path = self.pending_path();
        if !entry_exists(&path)? {
            return Ok(());
        }
        let pending: PendingActivation = read_canonical_state(&path)?;
        validate_pending_activation(&pending)?;
        let target = self.reverify_descriptor(&pending.target)?;
        if target != pending.target {
            return Err(PackStoreError::DescriptorChanged);
        }
        let observed_epochs = self.load_epochs_strict()?;
        if observed_epochs != pending.prior_epochs && observed_epochs != pending.next_epochs {
            return Err(PackStoreError::CorruptState(
                "pending activation epoch witness",
            ));
        }
        if let Ok(observed_activation) = self.load_activation_strict()
            && observed_activation != pending.prior_activation
            && observed_activation != pending.next_activation
        {
            return Err(PackStoreError::CorruptState(
                "pending activation state witness",
            ));
        }
        // The journal is durable before the floor is raised. A valid pending
        // target is therefore always completed; this never lowers an observed
        // security epoch and repairs a torn/corrupt activation pointer.
        self.persist_epochs(&pending.next_epochs)?;
        self.persist_activation(&pending.next_activation)?;
        self.remove_pending()?;
        Ok(())
    }
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ActivationBoundary {
    Journal,
    Epochs,
    Activation,
}

fn require_epoch_from(
    descriptor: &VerifiedPack,
    epochs: &EpochState,
) -> Result<(), PackStoreError> {
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

fn validate_pending_activation(pending: &PendingActivation) -> Result<(), PackStoreError> {
    if pending.schema_version != STATE_SCHEMA_VERSION
        || pending.prior_activation.schema_version != STATE_SCHEMA_VERSION
        || pending.next_activation.schema_version != STATE_SCHEMA_VERSION
        || pending.prior_epochs.schema_version != STATE_SCHEMA_VERSION
        || pending.next_epochs.schema_version != STATE_SCHEMA_VERSION
        || pending.next_activation.current.as_ref() != Some(&pending.target)
        || pending.next_activation.previous != pending.prior_activation.current
    {
        return Err(PackStoreError::CorruptState(
            "pending activation transaction",
        ));
    }
    let prior_floor = pending
        .prior_epochs
        .epochs
        .get(&pending.target.pack_id)
        .copied()
        .unwrap_or(EMBEDDED_MINIMUM_SECURITY_EPOCH);
    let next_floor = pending
        .next_epochs
        .epochs
        .get(&pending.target.pack_id)
        .copied()
        .unwrap_or(EMBEDDED_MINIMUM_SECURITY_EPOCH);
    let mut expected_epochs = pending.prior_epochs.clone();
    expected_epochs.epochs.insert(
        pending.target.pack_id.clone(),
        prior_floor.max(pending.target.security_epoch),
    );
    if next_floor != prior_floor.max(pending.target.security_epoch)
        || pending.next_epochs != expected_epochs
    {
        return Err(PackStoreError::CorruptState(
            "pending activation epoch transition",
        ));
    }
    Ok(())
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
                #[cfg(windows)]
                clear_windows_readonly(&child, metadata.permissions())?;
                fs::remove_file(child)?;
            }
        }
    }
    for directory in ordered.into_iter().rev() {
        fs::remove_dir(directory)?;
    }
    Ok(())
}

#[cfg(windows)]
#[allow(clippy::permissions_set_readonly_false)]
fn clear_windows_readonly(
    path: &Path,
    mut permissions: fs::Permissions,
) -> Result<(), PackStoreError> {
    // Windows refuses removal of FILE_ATTRIBUTE_READONLY payloads. This code
    // does not compile on Unix, where clearing readonly could broaden mode bits.
    permissions.set_readonly(false);
    fs::set_permissions(path, permissions)?;
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

pub(super) fn read_canonical_state<T>(path: &Path) -> Result<T, PackStoreError>
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

pub(super) fn atomic_write_canonical<T: Serialize>(
    path: &Path,
    value: &T,
) -> Result<(), PackStoreError> {
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

pub(super) fn exclusive_file_lock(path: &Path) -> Result<ExclusiveFileLock, PackStoreError> {
    let parent = path
        .parent()
        .ok_or(PackStoreError::CorruptState("lock parent"))?;
    ensure_regular_directories(parent)?;
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true);
    configure_private_create(&mut options);
    configure_no_follow(&mut options);
    let file = options.open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || is_link_or_reparse(&metadata) {
        return Err(PackStoreError::UnsafeFilesystemEntry(path.to_path_buf()));
    }
    super::manifest::reject_named_streams(path)?;
    lock_file(&file)?;
    Ok(ExclusiveFileLock { file })
}

fn remove_regular_state_file(path: &Path) -> Result<(), PackStoreError> {
    if !entry_exists(path)? {
        return Ok(());
    }
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_file() || is_link_or_reparse(&metadata) {
        return Err(PackStoreError::UnsafeFilesystemEntry(path.to_path_buf()));
    }
    super::manifest::reject_named_streams(path)?;
    fs::remove_file(path)?;
    sync_parent_if_supported(path)?;
    Ok(())
}

fn entry_exists(path: &Path) -> Result<bool, PackStoreError> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(PackStoreError::Io(error)),
    }
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
fn durable_rename_new(source: &Path, destination: &Path) -> Result<PublishOutcome, PackStoreError> {
    match move_file(source, destination, false) {
        Ok(()) => Ok(PublishOutcome::Published),
        Err(_error) if destination.symlink_metadata().is_ok() => {
            Ok(PublishOutcome::DestinationExists)
        }
        Err(error) => Err(PackStoreError::Io(error)),
    }
}

#[cfg(target_os = "linux")]
fn durable_rename_new(source: &Path, destination: &Path) -> Result<PublishOutcome, PackStoreError> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let source = CString::new(source.as_os_str().as_bytes())
        .map_err(|_| PackStoreError::CorruptState("publish source path"))?;
    let destination_raw = CString::new(destination.as_os_str().as_bytes())
        .map_err(|_| PackStoreError::CorruptState("publish destination path"))?;
    let result = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            libc::AT_FDCWD,
            source.as_ptr(),
            libc::AT_FDCWD,
            destination_raw.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if result != 0 {
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::EEXIST) {
            return Ok(PublishOutcome::DestinationExists);
        }
        if error.raw_os_error() == Some(libc::ENOSYS) {
            return Err(PackStoreError::UnsupportedAtomicPublish);
        }
        return Err(PackStoreError::Io(error));
    }
    sync_parent(destination)?;
    Ok(PublishOutcome::Published)
}

#[cfg(target_os = "macos")]
fn durable_rename_new(source: &Path, destination: &Path) -> Result<PublishOutcome, PackStoreError> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let source = CString::new(source.as_os_str().as_bytes())
        .map_err(|_| PackStoreError::CorruptState("publish source path"))?;
    let destination_raw = CString::new(destination.as_os_str().as_bytes())
        .map_err(|_| PackStoreError::CorruptState("publish destination path"))?;
    if unsafe { libc::renamex_np(source.as_ptr(), destination_raw.as_ptr(), libc::RENAME_EXCL) }
        != 0
    {
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::EEXIST) {
            return Ok(PublishOutcome::DestinationExists);
        }
        return Err(PackStoreError::Io(error));
    }
    sync_parent(destination)?;
    Ok(PublishOutcome::Published)
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
fn durable_rename_new(
    _source: &Path,
    _destination: &Path,
) -> Result<PublishOutcome, PackStoreError> {
    Err(PackStoreError::UnsupportedAtomicPublish)
}

#[cfg(not(any(unix, windows)))]
fn durable_rename_new(
    _source: &Path,
    _destination: &Path,
) -> Result<PublishOutcome, PackStoreError> {
    Err(PackStoreError::UnsupportedAtomicPublish)
}

#[cfg(windows)]
fn durable_replace(source: &Path, destination: &Path) -> Result<(), PackStoreError> {
    move_file(source, destination, true).map_err(PackStoreError::Io)
}

#[cfg(not(windows))]
fn durable_replace(source: &Path, destination: &Path) -> Result<(), PackStoreError> {
    fs::rename(source, destination)?;
    sync_parent(destination)?;
    Ok(())
}

#[cfg(windows)]
fn move_file(source: &Path, destination: &Path, replace: bool) -> io::Result<()> {
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
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(unix)]
fn lock_file(file: &File) -> Result<(), PackStoreError> {
    use std::os::fd::AsRawFd;
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } == 0 {
        Ok(())
    } else {
        Err(PackStoreError::Io(io::Error::last_os_error()))
    }
}

#[cfg(windows)]
fn lock_file(file: &File) -> Result<(), PackStoreError> {
    use std::mem::zeroed;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{LOCKFILE_EXCLUSIVE_LOCK, LockFileEx};
    use windows_sys::Win32::System::IO::OVERLAPPED;
    let mut overlapped: OVERLAPPED = unsafe { zeroed() };
    if unsafe {
        LockFileEx(
            file.as_raw_handle(),
            LOCKFILE_EXCLUSIVE_LOCK,
            0,
            1,
            0,
            &mut overlapped,
        )
    } != 0
    {
        Ok(())
    } else {
        Err(PackStoreError::Io(io::Error::last_os_error()))
    }
}

#[cfg(not(any(unix, windows)))]
fn lock_file(_file: &File) -> Result<(), PackStoreError> {
    Err(PackStoreError::UnsupportedFileLock)
}

#[cfg(unix)]
fn unlock_file(file: &File) {
    use std::os::fd::AsRawFd;
    let _ = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) };
}

#[cfg(windows)]
fn unlock_file(file: &File) {
    use std::mem::zeroed;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::UnlockFileEx;
    use windows_sys::Win32::System::IO::OVERLAPPED;
    let mut overlapped: OVERLAPPED = unsafe { zeroed() };
    let _ = unsafe { UnlockFileEx(file.as_raw_handle(), 0, 1, 0, &mut overlapped) };
}

#[cfg(not(any(unix, windows)))]
fn unlock_file(_file: &File) {}

impl Drop for ExclusiveFileLock {
    fn drop(&mut self) {
        unlock_file(&self.file);
    }
}

fn sync_parent_if_supported(path: &Path) -> Result<(), PackStoreError> {
    #[cfg(not(windows))]
    {
        sync_parent(path)?;
    }
    #[cfg(windows)]
    let _ = path;
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
    #[error("this platform has no supported atomic no-replace directory publication primitive")]
    UnsupportedAtomicPublish,
    #[error("this platform has no supported OS-backed file lock")]
    UnsupportedFileLock,
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
    #[cfg(test)]
    #[error("injected activation interruption")]
    InjectedInterruption,
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

    #[test]
    fn atomic_publish_never_replaces_an_existing_digest_directory() {
        let root = temp_root("store-no-replace");
        let source = root.join("source-directory");
        let destination = root.join("digest-directory");
        fs::create_dir(&source).unwrap();
        fs::create_dir(&destination).unwrap();
        fs::write(source.join("new"), b"new").unwrap();
        fs::write(destination.join("immutable"), b"original").unwrap();
        assert_eq!(
            durable_rename_new(&source, &destination).unwrap(),
            PublishOutcome::DestinationExists
        );
        assert_eq!(
            fs::read(destination.join("immutable")).unwrap(),
            b"original"
        );
        assert!(source.join("new").is_file());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn activation_journal_recovers_every_persistence_boundary() {
        for boundary in [
            ActivationBoundary::Journal,
            ActivationBoundary::Epochs,
            ActivationBoundary::Activation,
        ] {
            let nonce = random_suffix().unwrap();
            let (root, workers, state, verifier, _) =
                store_fixture(&format!("ab-{boundary:?}-{}", &nonce[..8]));
            let store = PackStore::new(&workers, &state, &verifier);
            let first = store.stage_and_install(&root.join("source")).unwrap();
            store.activate(&first).unwrap();
            let (next_source, _) = additional_source(&root, "2.0.0", 2);
            let next = store.stage_and_install(&next_source).unwrap();
            {
                let _lock = store.acquire_lock().unwrap();
                assert!(matches!(
                    store.activate_locked(&next, Some(boundary)),
                    Err(PackStoreError::InjectedInterruption)
                ));
            }
            assert_eq!(store.current_fail_closed(), Some(next.clone()));
            let activation = store.load_activation_strict().unwrap();
            assert_eq!(activation.previous, Some(first));
            assert!(!store.pending_path().exists());
            assert_eq!(
                store
                    .load_epochs_strict()
                    .unwrap()
                    .epochs
                    .get(&next.pack_id),
                Some(&2)
            );
            fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn corrupt_pending_activation_fails_closed_without_lowering_epoch() {
        let (root, workers, state, verifier, _) = store_fixture("corrupt-pending");
        let store = PackStore::new(&workers, &state, &verifier);
        let first = store.stage_and_install(&root.join("source")).unwrap();
        store.activate(&first).unwrap();
        fs::write(store.pending_path(), b"{corrupt").unwrap();
        assert_eq!(store.current_fail_closed(), None);
        assert!(store.activate(&first).is_err());
        assert_eq!(
            store
                .load_epochs_strict()
                .unwrap()
                .epochs
                .get(&first.pack_id),
            Some(&1)
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn concurrent_activations_preserve_the_immediate_predecessor() {
        let (root, workers, state, verifier, _) = store_fixture("concurrent-activation");
        let store = PackStore::new(&workers, &state, &verifier);
        let first = store.stage_and_install(&root.join("source")).unwrap();
        let (second_source, _) = additional_source(&root, "2.0.0", 1);
        let (third_source, _) = additional_source(&root, "3.0.0", 1);
        let second = store.stage_and_install(&second_source).unwrap();
        let third = store.stage_and_install(&third_source).unwrap();
        store.activate(&first).unwrap();
        std::thread::scope(|scope| {
            let left = PackStore::new(&workers, &state, &verifier);
            let right = PackStore::new(&workers, &state, &verifier);
            let second = second.clone();
            let third = third.clone();
            scope.spawn(move || left.activate(&second).unwrap());
            scope.spawn(move || right.activate(&third).unwrap());
        });
        let activation = store.load_activation_strict().unwrap();
        let current = activation.current.unwrap();
        let previous = activation.previous.unwrap();
        assert!(
            (current == second && previous == third) || (current == third && previous == second)
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn concurrent_epoch_raises_never_lose_the_highest_floor() {
        let (root, workers, state, verifier, _) = store_fixture("concurrent-epochs");
        let store = PackStore::new(&workers, &state, &verifier);
        let first = store.stage_and_install(&root.join("source")).unwrap();
        let (epoch_two_source, _) = additional_source(&root, "2.0.0", 2);
        let (epoch_three_source, _) = additional_source(&root, "3.0.0", 3);
        let epoch_two = store.stage_and_install(&epoch_two_source).unwrap();
        let epoch_three = store.stage_and_install(&epoch_three_source).unwrap();
        store.activate(&first).unwrap();
        std::thread::scope(|scope| {
            let left = PackStore::new(&workers, &state, &verifier);
            let right = PackStore::new(&workers, &state, &verifier);
            let epoch_two = epoch_two.clone();
            let epoch_three = epoch_three.clone();
            let lower = scope.spawn(move || left.activate(&epoch_two));
            let higher = scope.spawn(move || right.activate(&epoch_three));
            let lower = lower.join().unwrap();
            higher.join().unwrap().unwrap();
            if let Err(error) = lower {
                assert!(matches!(
                    error,
                    PackStoreError::SecurityEpochDowngrade {
                        observed: 2,
                        floor: 3
                    }
                ));
            }
        });
        let epochs = store.load_epochs_strict().unwrap();
        assert_eq!(epochs.epochs.get(&epoch_three.pack_id), Some(&3));
        assert_eq!(store.current_fail_closed(), Some(epoch_three));
        fs::remove_dir_all(root).unwrap();
    }
}
