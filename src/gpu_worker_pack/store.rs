use std::collections::BTreeMap;
#[cfg(any(windows, test))]
use std::fs::OpenOptions;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};

#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::ffi::CStr;
#[cfg(unix)]
use std::ffi::CString;
#[cfg(unix)]
use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd};

use getrandom::fill;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use super::manifest::{
    EMBEDDED_MINIMUM_SECURITY_EPOCH, MAX_AGGREGATE_BYTES, MAX_FILES, MAX_MANIFEST_BYTES,
    MAX_SIGNATURE_BYTES, PackVerificationError, PackVerifier, PinnedPackRoot, StoreComponent,
    VerifiedCopyEntry, VerifiedPack, VerifiedPackLease, is_canonical_sha256,
};

const STATE_SCHEMA_VERSION: u16 = 1;
pub(super) const MAX_STATE_BYTES: u64 = 256 * 1024;
const STORE_LOCK_NAME: &str = ".worker-pack-store.lock";
pub(super) const DISCOVERY_EPOCH_LOCK_NAME: &str = ".worker-pack-discovery-epoch.lock";
const DISCOVERY_EPOCH_STATE_NAME: &str = "discovery-security-epochs.json";
#[cfg(windows)]
const PRIVATE_STATE_AUTHORITY_LOCK_NAME: &str = ".scribe-private-state.lock";

#[cfg(test)]
type StateReadHook = Box<dyn FnOnce(&Path)>;

#[cfg(test)]
thread_local! {
    static STATE_READ_HOOK: std::cell::RefCell<Option<StateReadHook>> =
        std::cell::RefCell::new(None);
}

#[cfg(test)]
pub(super) fn set_state_read_hook(hook: impl FnOnce(&Path) + 'static) {
    STATE_READ_HOOK.with(|slot| *slot.borrow_mut() = Some(Box::new(hook)));
}

#[cfg(test)]
fn run_state_read_hook(path: &Path) {
    STATE_READ_HOOK.with(|slot| {
        if let Some(hook) = slot.borrow_mut().take() {
            hook(path);
        }
    });
}

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
    root: AnchoredDirectory,
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

/// Persistent high-water authority for immutable bundled-catalog discovery.
/// This is separate from activation state but deliberately reuses the same
/// anchored lock and atomic durable-replace implementation.
pub(crate) struct DiscoveryEpochLedger {
    state_root: PathBuf,
}

struct AnchoredDirectory {
    path: PathBuf,
    chain: Vec<File>,
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
struct UnixDirectoryStream(*mut libc::DIR);

#[cfg(target_os = "macos")]
static MACOS_ANCHORED_DIRECTORY_SCAN_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(any(target_os = "linux", target_os = "macos"))]
impl UnixDirectoryStream {
    fn close(mut self) -> io::Result<()> {
        let stream = std::mem::replace(&mut self.0, std::ptr::null_mut());
        if unsafe { libc::closedir(stream) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
impl Drop for UnixDirectoryStream {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { libc::closedir(self.0) };
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct DirectoryIdentity {
    volume: u64,
    file: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AnchoredEntryKind {
    Directory,
    File,
}

impl AnchoredDirectory {
    fn open_or_create_root(path: &Path) -> Result<Self, PackStoreError> {
        open_absolute_directory_chain(path, true)
    }

    fn open_root(path: &Path) -> Result<Self, PackStoreError> {
        open_absolute_directory_chain(path, false)
    }

    fn open_or_create_child(&self, name: &str, delete_share: bool) -> Result<Self, PackStoreError> {
        validate_anchor_name(name)?;
        create_directory_at(self, name, false)?;
        self.open_child(name, delete_share)
    }

    fn create_new_child(&self, name: &str, delete_share: bool) -> Result<Self, PackStoreError> {
        validate_anchor_name(name)?;
        create_directory_at(self, name, true)?;
        self.open_child(name, delete_share)
    }

    fn open_child(&self, name: &str, delete_share: bool) -> Result<Self, PackStoreError> {
        validate_anchor_name(name)?;
        let path = self.path.join(name);
        let handle = open_directory_at(self, name, delete_share)?;
        let mut chain = self
            .chain
            .iter()
            .map(File::try_clone)
            .collect::<io::Result<Vec<_>>>()?;
        chain.push(handle);
        let anchored = Self { path, chain };
        anchored.recheck()?;
        Ok(anchored)
    }

    fn verifier_lease(&self) -> Result<PinnedPackRoot, PackStoreError> {
        let handles = self
            .chain
            .iter()
            .map(File::try_clone)
            .collect::<io::Result<Vec<_>>>()?;
        Ok(PinnedPackRoot::from_anchored_handles(
            self.path.clone(),
            handles,
        )?)
    }

    fn rebound(&self, path: PathBuf) -> Result<Self, PackStoreError> {
        let anchored = Self {
            path,
            chain: self
                .chain
                .iter()
                .map(File::try_clone)
                .collect::<io::Result<Vec<_>>>()?,
        };
        anchored.recheck()?;
        Ok(anchored)
    }

    fn recheck(&self) -> Result<(), PackStoreError> {
        let mut path = self.path.as_path();
        for (index, handle) in self.chain.iter().rev().enumerate() {
            if !same_anchored_directory(handle, path)? {
                return Err(PackStoreError::UnsafeFilesystemEntry(path.to_path_buf()));
            }
            if index + 1 < self.chain.len() {
                path = path
                    .parent()
                    .ok_or(PackStoreError::CorruptState("anchored directory chain"))?;
            }
        }
        Ok(())
    }

    fn leaf(&self) -> &File {
        self.chain.last().expect("anchored directory handle")
    }

    #[cfg(unix)]
    fn parent_leaf(&self) -> Result<&File, PackStoreError> {
        self.chain
            .iter()
            .rev()
            .nth(1)
            .ok_or(PackStoreError::CorruptState("anchored directory parent"))
    }

    fn identity(&self) -> Result<DirectoryIdentity, PackStoreError> {
        directory_identity(self.leaf())
    }
}

impl ExclusiveFileLock {
    fn state_name<'a>(&self, path: &'a Path) -> Result<&'a str, PackStoreError> {
        let parent = path
            .parent()
            .ok_or(PackStoreError::CorruptState("state parent"))?;
        if !same_anchored_directory(self.root.leaf(), parent)? {
            return Err(PackStoreError::CorruptState("state authority mismatch"));
        }
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or(PackStoreError::CorruptState("state file name"))?;
        validate_anchor_name(name)?;
        Ok(name)
    }

    pub(super) fn read<T>(&self, path: &Path) -> Result<T, PackStoreError>
    where
        T: for<'de> Deserialize<'de> + Serialize,
    {
        read_canonical_state_at(&self.root, self.state_name(path)?)
    }

    pub(super) fn write<T: Serialize>(&self, path: &Path, value: &T) -> Result<(), PackStoreError> {
        atomic_write_canonical_at(&self.root, self.state_name(path)?, value)
    }

    pub(super) fn exists(&self, path: &Path) -> Result<bool, PackStoreError> {
        state_file_exists_at(&self.root, self.state_name(path)?)
    }

    fn remove(&self, path: &Path) -> Result<(), PackStoreError> {
        remove_state_file_at(&self.root, self.state_name(path)?)
    }

    fn remove_temporary(&self, name: &str) -> Result<(), PackStoreError> {
        validate_anchor_name(name)?;
        if !name.starts_with('.') || !name.contains(".tmp-") {
            return Err(PackStoreError::UnsafeRecoveryTarget(
                self.root.path.join(name),
            ));
        }
        remove_state_file_at(&self.root, name)
    }

    fn root(&self) -> &AnchoredDirectory {
        &self.root
    }
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
        self.stage_and_install_inner(signed_source, |_| {}, |_| {}, |_| {})
    }

    fn stage_and_install_inner(
        &self,
        signed_source: &Path,
        after_source_verify: impl FnOnce(&Path),
        after_version_anchor: impl FnOnce(&Path),
        after_staging_anchor: impl FnOnce(&Path),
    ) -> Result<VerifiedPack, PackStoreError> {
        let source_root = AnchoredDirectory::open_root(signed_source)?;
        let initially_verified = self.verifier.verify_pinned(source_root.verifier_lease()?)?;
        after_source_verify(signed_source);
        initially_verified.recheck()?;
        let source_lease = self.verifier.verify_pinned(source_root.verifier_lease()?)?;
        require_same_pack(
            initially_verified.verified_pack(),
            source_lease.verified_pack(),
        )?;
        let source = source_lease.verified_pack().clone();
        let lock = self.acquire_lock()?;
        self.recover_pending_activation_with_lock(&lock)?;
        let epochs = self.load_epochs_with_lock(&lock)?;
        require_epoch_from(&source, &epochs)?;
        validate_descriptor_identity(&source)?;
        let packs = AnchoredDirectory::open_or_create_root(&self.packs_root)?;
        let _ = self.store_paths_for(&source)?;
        let pack_id = packs.open_or_create_child(source.pack_id.as_str(), false)?;
        let version = pack_id.open_or_create_child(source.pack_version.as_str(), false)?;
        after_version_anchor(&version.path);
        if let Ok(existing) = version.open_child(&source.pack_digest, false) {
            let installed = self.verify_anchored_identity(&source, &existing, false)?;
            require_same_pack(&source, installed.verified_pack())?;
            return Ok(installed.verified_pack().clone());
        }

        let staging_name = format!(".{}.staging-{}", source.pack_digest, random_suffix()?);
        let staging = version.create_new_child(&staging_name, true)?;
        after_staging_anchor(&staging.path);
        let prepared = (|| {
            copy_verified_inventory_anchored(&source_lease, &staging)?;
            let source_after_copy = self.verifier.verify_pinned(source_root.verifier_lease()?)?;
            require_same_pack(&source, source_after_copy.verified_pack())?;
            let staged = self.verifier.verify_pinned(staging.verifier_lease()?)?;
            require_same_pack(&source, staged.verified_pack())?;
            staged.recheck()?;
            drop(staged);
            make_payload_readonly_anchored(&staging)?;
            staging.recheck()?;
            staging.identity()
        })();
        let staging_identity = match prepared {
            Ok(identity) => identity,
            Err(error) => {
                let _ = remove_staging_tree_anchored(&staging);
                return Err(error);
            }
        };
        if staging.identity()? != staging_identity {
            return Err(PackStoreError::UnsafeFilesystemEntry(
                version.path.join(&staging_name),
            ));
        }
        let result = (|| {
            let installed_root =
                match durable_rename_new_anchored(&staging, &version, &source.pack_digest)? {
                    PublishOutcome::Published => {
                        staging.rebound(version.path.join(&source.pack_digest))?
                    }
                    PublishOutcome::DestinationExists => {
                        remove_staging_tree_anchored(&staging)?;
                        version.open_child(&source.pack_digest, false)?
                    }
                };
            let installed = self.verify_anchored_identity(&source, &installed_root, false)?;
            require_same_pack(&source, installed.verified_pack())?;
            Ok(installed.verified_pack().clone())
        })();
        if result.is_err() {
            let _ = remove_staging_tree_anchored(&staging);
        }
        result
    }

    pub(crate) fn activate(&self, descriptor: &VerifiedPack) -> Result<(), PackStoreError> {
        let lock = self.acquire_lock()?;
        self.recover_pending_activation_with_lock(&lock)?;
        self.activate_with_lock(&lock, descriptor, None)
    }

    fn activate_with_lock(
        &self,
        lock: &ExclusiveFileLock,
        descriptor: &VerifiedPack,
        #[cfg(test)] interrupt_after: Option<ActivationBoundary>,
        #[cfg(not(test))] _interrupt_after: Option<()>,
    ) -> Result<(), PackStoreError> {
        let verified = self.reverify_descriptor(descriptor)?;
        let prior_epochs = self.load_epochs_with_lock(lock)?;
        require_epoch_from(&verified, &prior_epochs)?;
        let prior_activation = self.load_activation_with_lock(lock)?;
        let mut next_epochs = prior_epochs.clone();
        let old_floor = next_epochs
            .epochs
            .get(verified.pack_id.as_str())
            .copied()
            .unwrap_or(EMBEDDED_MINIMUM_SECURITY_EPOCH);
        next_epochs.epochs.insert(
            verified.pack_id.as_str().to_owned(),
            old_floor.max(verified.security_epoch),
        );
        validate_epoch_state(&next_epochs)?;
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
        self.persist_pending(lock, &pending)?;
        #[cfg(test)]
        if interrupt_after == Some(ActivationBoundary::Journal) {
            return Err(PackStoreError::InjectedInterruption);
        }
        self.persist_epochs_with_lock(lock, &next_epochs)?;
        #[cfg(test)]
        if interrupt_after == Some(ActivationBoundary::Epochs) {
            return Err(PackStoreError::InjectedInterruption);
        }
        self.persist_activation(lock, &next_activation)?;
        #[cfg(test)]
        if interrupt_after == Some(ActivationBoundary::Activation) {
            return Err(PackStoreError::InjectedInterruption);
        }
        self.remove_pending(lock)?;
        Ok(())
    }

    pub(crate) fn rollback(&self) -> Result<VerifiedPack, PackStoreError> {
        let lock = self.acquire_lock()?;
        self.recover_pending_activation_with_lock(&lock)?;
        let state = self.load_activation_with_lock(&lock)?;
        let previous = state.previous.ok_or(PackStoreError::NoRollbackPack)?;
        let rollback = self.reverify_descriptor(&previous)?;
        let epochs = self.load_epochs_with_lock(&lock)?;
        require_epoch_from(&rollback, &epochs)?;
        let prior_current = state
            .current
            .as_ref()
            .and_then(|current| self.reverify_descriptor(current).ok())
            .filter(|current| require_epoch_from(current, &epochs).is_ok());
        self.persist_activation(
            &lock,
            &ActivationState {
                schema_version: STATE_SCHEMA_VERSION,
                current: Some(rollback.clone()),
                previous: prior_current,
            },
        )?;
        Ok(rollback)
    }

    /// Corrupt state or invalid packs project to no GPU pack and cannot affect
    /// the separately compiled CPU route.
    pub(crate) fn current_fail_closed(&self) -> Option<VerifiedPackLease> {
        let lock = self.acquire_lock().ok()?;
        self.recover_pending_activation_with_lock(&lock).ok()?;
        let descriptor = self.load_activation_with_lock(&lock).ok()?.current?;
        let epochs = self.load_epochs_with_lock(&lock).ok()?;
        self.reverify_lease_at(&descriptor)
            .ok()
            .filter(|pack| require_epoch_from(pack.verified_pack(), &epochs).is_ok())
    }

    /// Only uniquely named incomplete staging directories and state temporary
    /// files are removed. Final digest trees and state records are untouched.
    pub(crate) fn recover_interrupted_work(&self) -> Result<(), PackStoreError> {
        let lock = self.acquire_lock()?;
        self.recover_pending_activation_with_lock(&lock)?;
        self.recover_interrupted_work_locked(&lock)
    }

    fn recover_interrupted_work_locked(
        &self,
        lock: &ExclusiveFileLock,
    ) -> Result<(), PackStoreError> {
        if self.packs_root.exists() {
            let packs = AnchoredDirectory::open_root(&self.packs_root)?;
            for pack_id in anchored_child_names(&packs)? {
                let pack_id = packs.open_child(&pack_id, false)?;
                for version in anchored_child_names(&pack_id)? {
                    let version = pack_id.open_child(&version, false)?;
                    for name in anchored_child_names(&version)? {
                        if name.starts_with('.') && name.contains(".staging-") {
                            let staging = version.open_child(&name, true)?;
                            remove_staging_tree_anchored(&staging)?;
                        }
                    }
                }
            }
        }
        for name in anchored_child_names(lock.root())? {
            if name.starts_with('.') && name.contains(".tmp-") {
                lock.remove_temporary(&name)?;
            }
        }
        Ok(())
    }

    fn reverify_descriptor(
        &self,
        descriptor: &VerifiedPack,
    ) -> Result<VerifiedPack, PackStoreError> {
        Ok(self.reverify_lease_at(descriptor)?.verified_pack().clone())
    }

    fn reverify_lease_at(
        &self,
        descriptor: &VerifiedPack,
    ) -> Result<VerifiedPackLease, PackStoreError> {
        self.verify_installed_identity(descriptor, true)
    }

    fn verify_installed_identity(
        &self,
        descriptor: &VerifiedPack,
        require_descriptor_root: bool,
    ) -> Result<VerifiedPackLease, PackStoreError> {
        let (_, expected_root) = self.store_paths_for(descriptor)?;
        if require_descriptor_root && descriptor.root.as_os_str() != expected_root.as_os_str() {
            return Err(PackStoreError::DescriptorOutsideStore);
        }
        let packs = AnchoredDirectory::open_root(&self.packs_root)?;
        let pack_id = packs.open_child(descriptor.pack_id.as_str(), false)?;
        let version = pack_id.open_child(descriptor.pack_version.as_str(), false)?;
        let digest = version.open_child(&descriptor.pack_digest, false)?;
        let verified = self.verifier.verify_pinned(digest.verifier_lease()?)?;
        if require_descriptor_root {
            if verified.verified_pack() != descriptor {
                return Err(PackStoreError::DescriptorChanged);
            }
        } else {
            require_same_pack(descriptor, verified.verified_pack())?;
        }
        Ok(verified)
    }

    fn verify_anchored_identity(
        &self,
        descriptor: &VerifiedPack,
        directory: &AnchoredDirectory,
        require_descriptor_root: bool,
    ) -> Result<VerifiedPackLease, PackStoreError> {
        let (_, expected_root) = self.store_paths_for(descriptor)?;
        if directory.path != expected_root
            || (require_descriptor_root && descriptor.root != expected_root)
        {
            return Err(PackStoreError::DescriptorOutsideStore);
        }
        let verified = self.verifier.verify_pinned(directory.verifier_lease()?)?;
        if require_descriptor_root {
            if verified.verified_pack() != descriptor {
                return Err(PackStoreError::DescriptorChanged);
            }
        } else {
            require_same_pack(descriptor, verified.verified_pack())?;
        }
        Ok(verified)
    }

    fn store_paths_for(
        &self,
        descriptor: &VerifiedPack,
    ) -> Result<(PathBuf, PathBuf), PackStoreError> {
        validate_descriptor_identity(descriptor)?;
        if !self.packs_root.is_absolute() {
            return Err(PackStoreError::DescriptorOutsideStore);
        }

        let relative = PathBuf::from(descriptor.pack_id.as_str())
            .join(descriptor.pack_version.as_str())
            .join(&descriptor.pack_digest);
        let components = relative.components().collect::<Vec<_>>();
        if components.len() != 3
            || components
                .iter()
                .any(|component| !matches!(component, Component::Normal(_)))
        {
            return Err(PackStoreError::DescriptorOutsideStore);
        }
        let final_root = self.packs_root.join(&relative);
        let stripped = final_root
            .strip_prefix(&self.packs_root)
            .map_err(|_| PackStoreError::DescriptorOutsideStore)?;
        if stripped.components().count() != 3
            || final_root.ancestors().nth(3) != Some(self.packs_root.as_path())
        {
            return Err(PackStoreError::DescriptorOutsideStore);
        }
        let parent = final_root
            .parent()
            .ok_or(PackStoreError::DescriptorOutsideStore)?
            .to_path_buf();
        Ok((parent, final_root))
    }

    fn validate_activation_descriptors(
        &self,
        state: &ActivationState,
    ) -> Result<(), PackStoreError> {
        for descriptor in state.current.iter().chain(state.previous.iter()) {
            let (_, expected_root) = self.store_paths_for(descriptor)?;
            if descriptor.root.as_os_str() != expected_root.as_os_str() {
                return Err(PackStoreError::DescriptorOutsideStore);
            }
            if self.reverify_lease_at(descriptor)?.verified_pack() != descriptor {
                return Err(PackStoreError::DescriptorChanged);
            }
        }
        Ok(())
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

    fn load_activation_with_lock(
        &self,
        lock: &ExclusiveFileLock,
    ) -> Result<ActivationState, PackStoreError> {
        let path = self.activation_path();
        if !lock.exists(&path)? {
            return Ok(ActivationState::empty());
        }
        let state: ActivationState = lock.read(&path)?;
        if state.schema_version != STATE_SCHEMA_VERSION {
            return Err(PackStoreError::CorruptState("activation schema"));
        }
        self.validate_activation_descriptors(&state)?;
        Ok(state)
    }

    fn load_epochs_with_lock(
        &self,
        lock: &ExclusiveFileLock,
    ) -> Result<EpochState, PackStoreError> {
        let path = self.epoch_path();
        if !lock.exists(&path)? {
            return Ok(EpochState::empty());
        }
        let state: EpochState = lock.read(&path)?;
        validate_epoch_state(&state)?;
        Ok(state)
    }

    fn persist_activation(
        &self,
        lock: &ExclusiveFileLock,
        state: &ActivationState,
    ) -> Result<(), PackStoreError> {
        self.validate_activation_descriptors(state)?;
        lock.write(&self.activation_path(), state)
    }

    fn persist_epochs_with_lock(
        &self,
        lock: &ExclusiveFileLock,
        state: &EpochState,
    ) -> Result<(), PackStoreError> {
        validate_epoch_state(state)?;
        lock.write(&self.epoch_path(), state)
    }

    fn persist_pending(
        &self,
        lock: &ExclusiveFileLock,
        pending: &PendingActivation,
    ) -> Result<(), PackStoreError> {
        self.validate_pending_descriptors(pending)?;
        validate_pending_activation(pending)?;
        lock.write(&self.pending_path(), pending)
    }

    fn remove_pending(&self, lock: &ExclusiveFileLock) -> Result<(), PackStoreError> {
        lock.remove(&self.pending_path())
    }

    fn recover_pending_activation_with_lock(
        &self,
        lock: &ExclusiveFileLock,
    ) -> Result<(), PackStoreError> {
        let path = self.pending_path();
        if !lock.exists(&path)? {
            return Ok(());
        }
        let pending: PendingActivation = lock.read(&path)?;
        self.validate_pending_descriptors(&pending)?;
        validate_pending_activation(&pending)?;
        let target = self.reverify_descriptor(&pending.target)?;
        if target != pending.target {
            return Err(PackStoreError::DescriptorChanged);
        }
        let observed_epochs = self.load_epochs_with_lock(lock)?;
        if observed_epochs != pending.prior_epochs && observed_epochs != pending.next_epochs {
            return Err(PackStoreError::CorruptState(
                "pending activation epoch witness",
            ));
        }
        if let Ok(observed_activation) = self.load_activation_with_lock(lock)
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
        self.persist_epochs_with_lock(lock, &pending.next_epochs)?;
        self.persist_activation(lock, &pending.next_activation)?;
        self.remove_pending(lock)?;
        Ok(())
    }

    fn validate_pending_descriptors(
        &self,
        pending: &PendingActivation,
    ) -> Result<(), PackStoreError> {
        self.validate_activation_descriptors(&pending.prior_activation)?;
        self.validate_activation_descriptors(&pending.next_activation)?;
        let (_, expected_root) = self.store_paths_for(&pending.target)?;
        if pending.target.root.as_os_str() != expected_root.as_os_str() {
            return Err(PackStoreError::DescriptorOutsideStore);
        }
        if self.reverify_lease_at(&pending.target)?.verified_pack() != &pending.target {
            return Err(PackStoreError::DescriptorChanged);
        }
        Ok(())
    }

    #[cfg(test)]
    fn load_activation_strict(&self) -> Result<ActivationState, PackStoreError> {
        let lock = self.acquire_lock()?;
        self.load_activation_with_lock(&lock)
    }

    #[cfg(test)]
    fn load_epochs_strict(&self) -> Result<EpochState, PackStoreError> {
        let lock = self.acquire_lock()?;
        self.load_epochs_with_lock(&lock)
    }

    #[cfg(test)]
    fn persist_epochs(&self, state: &EpochState) -> Result<(), PackStoreError> {
        let lock = self.acquire_lock()?;
        self.persist_epochs_with_lock(&lock, state)
    }

    #[cfg(test)]
    fn activate_locked(
        &self,
        descriptor: &VerifiedPack,
        interrupt_after: Option<ActivationBoundary>,
    ) -> Result<(), PackStoreError> {
        let lock = self.acquire_lock()?;
        self.activate_with_lock(&lock, descriptor, interrupt_after)
    }

    #[cfg(test)]
    fn recover_pending_activation_locked(&self) -> Result<(), PackStoreError> {
        let lock = self.acquire_lock()?;
        self.recover_pending_activation_with_lock(&lock)
    }
}

impl DiscoveryEpochLedger {
    pub(crate) fn new(private_state_root: impl Into<PathBuf>) -> Self {
        Self {
            state_root: private_state_root.into(),
        }
    }

    pub(crate) fn admit(&self, packs: &[&VerifiedPack]) -> Result<(), PackStoreError> {
        if packs.is_empty() {
            return Ok(());
        }
        let state_root_preexisting = self.state_root.exists();
        let lock = exclusive_file_lock(&self.state_root.join(DISCOVERY_EPOCH_LOCK_NAME))?;
        let path = self.state_root.join(DISCOVERY_EPOCH_STATE_NAME);
        let state = if lock.exists(&path)? {
            let state: EpochState = lock.read(&path)?;
            validate_epoch_state(&state)?;
            state
        } else if state_root_preexisting {
            return Err(PackStoreError::CorruptState(
                "discovery security epoch state is missing",
            ));
        } else {
            EpochState::empty()
        };
        let mut requested = BTreeMap::<String, u64>::new();
        for pack in packs {
            let key = discovery_epoch_key(pack)?;
            requested
                .entry(key)
                .and_modify(|epoch| *epoch = (*epoch).max(pack.security_epoch))
                .or_insert(pack.security_epoch);
        }
        for pack in packs {
            let key = discovery_epoch_key(pack)?;
            let floor = state
                .epochs
                .get(&key)
                .copied()
                .unwrap_or(EMBEDDED_MINIMUM_SECURITY_EPOCH)
                .max(requested[&key])
                .max(EMBEDDED_MINIMUM_SECURITY_EPOCH);
            if pack.security_epoch < floor {
                return Err(PackStoreError::SecurityEpochDowngrade {
                    observed: pack.security_epoch,
                    floor,
                });
            }
        }
        let mut next = state.clone();
        for (key, epoch) in requested {
            let prior = next
                .epochs
                .get(&key)
                .copied()
                .unwrap_or(EMBEDDED_MINIMUM_SECURITY_EPOCH);
            next.epochs.insert(key, prior.max(epoch));
        }
        validate_epoch_state(&next)?;
        if next != state {
            lock.write(&path, &next)?;
        }
        Ok(())
    }

    #[cfg(test)]
    fn load_strict(&self) -> Result<EpochState, PackStoreError> {
        let lock = exclusive_file_lock(&self.state_root.join(DISCOVERY_EPOCH_LOCK_NAME))?;
        let path = self.state_root.join(DISCOVERY_EPOCH_STATE_NAME);
        let state = lock.read(&path)?;
        validate_epoch_state(&state)?;
        Ok(state)
    }
}

fn discovery_epoch_key(pack: &VerifiedPack) -> Result<String, PackStoreError> {
    let backend = match pack.backend {
        super::manifest::PackBackend::Cuda => "cuda",
        super::manifest::PackBackend::Vulkan => "vulkan",
        super::manifest::PackBackend::Metal => "metal",
    };
    let key = format!(
        "{}-{}-{backend}-{}",
        pack.target_os,
        pack.target_arch,
        pack.pack_id.as_str()
    );
    StoreComponent::new(key.clone())
        .map(|_| key)
        .ok_or(PackStoreError::CorruptState("discovery security epoch key"))
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
        .get(descriptor.pack_id.as_str())
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
    validate_epoch_state(&pending.prior_epochs)?;
    validate_epoch_state(&pending.next_epochs)?;
    let prior_floor = pending
        .prior_epochs
        .epochs
        .get(pending.target.pack_id.as_str())
        .copied()
        .unwrap_or(EMBEDDED_MINIMUM_SECURITY_EPOCH);
    let next_floor = pending
        .next_epochs
        .epochs
        .get(pending.target.pack_id.as_str())
        .copied()
        .unwrap_or(EMBEDDED_MINIMUM_SECURITY_EPOCH);
    let mut expected_epochs = pending.prior_epochs.clone();
    expected_epochs.epochs.insert(
        pending.target.pack_id.as_str().to_owned(),
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

fn validate_epoch_state(state: &EpochState) -> Result<(), PackStoreError> {
    if state.schema_version != STATE_SCHEMA_VERSION
        || state.epochs.len() > 256
        || state
            .epochs
            .values()
            .any(|epoch| *epoch < EMBEDDED_MINIMUM_SECURITY_EPOCH)
        || state
            .epochs
            .keys()
            .any(|pack_id| StoreComponent::new(pack_id.clone()).is_none())
    {
        return Err(PackStoreError::CorruptState("security epoch state"));
    }
    Ok(())
}

fn validate_descriptor_identity(descriptor: &VerifiedPack) -> Result<(), PackStoreError> {
    if !descriptor.pack_id.is_canonical()
        || !descriptor.pack_version.is_canonical()
        || !is_canonical_sha256(&descriptor.pack_digest)
    {
        return Err(PackStoreError::CorruptState(
            "worker pack descriptor identity",
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

fn copy_verified_inventory_anchored(
    source: &VerifiedPackLease,
    destination: &AnchoredDirectory,
) -> Result<(), PackStoreError> {
    const MAX_DIRECTORIES: usize = MAX_FILES * 12;
    let maximum_copy_bytes = MAX_AGGREGATE_BYTES
        .checked_add(MAX_MANIFEST_BYTES)
        .and_then(|value| value.checked_add(MAX_SIGNATURE_BYTES))
        .ok_or(PackStoreError::CorruptState("pack copy bounds"))?;
    if source.copy_entries().len() > MAX_FILES + 2 {
        return Err(PackStoreError::CorruptState("pack copy file count"));
    }
    let mut directories = std::collections::BTreeSet::new();
    let mut aggregate = 0_u64;
    for entry in source.copy_entries() {
        aggregate = aggregate
            .checked_add(entry.size_bytes)
            .ok_or(PackStoreError::CorruptState("pack copy aggregate"))?;
        if aggregate > maximum_copy_bytes {
            return Err(PackStoreError::CorruptState("pack copy aggregate"));
        }
        let mut parent = Path::new(&entry.path).parent();
        while let Some(path) = parent {
            if path.as_os_str().is_empty() {
                break;
            }
            directories.insert(path.to_path_buf());
            parent = path.parent();
        }
    }
    if directories.len() > MAX_DIRECTORIES {
        return Err(PackStoreError::CorruptState("pack copy directory count"));
    }

    source.recheck()?;
    destination.recheck()?;
    for entry in source.copy_entries() {
        let relative = Path::new(&entry.path);
        let mut components = relative.components().peekable();
        let mut parent = destination.rebound(destination.path.clone())?;
        while let Some(component) = components.next() {
            let Component::Normal(name) = component else {
                return Err(PackStoreError::CorruptState("pack copy path"));
            };
            let name = name
                .to_str()
                .ok_or(PackStoreError::CorruptState("pack copy path"))?;
            validate_anchor_name(name)?;
            if components.peek().is_none() {
                copy_verified_file_to_anchor(source, entry, &parent, name)?;
                break;
            }
            parent = parent.open_or_create_child(name, false)?;
        }
    }
    source.recheck()?;
    destination.recheck()?;
    Ok(())
}

fn copy_verified_file_to_anchor(
    source: &VerifiedPackLease,
    entry: &VerifiedCopyEntry,
    destination: &AnchoredDirectory,
    name: &str,
) -> Result<(), PackStoreError> {
    let mut input = source.open_copy_file(entry)?;
    let mut output = create_file_at(destination, name, false)?;
    let mut hasher = Sha256::new();
    let mut remaining = entry.size_bytes;
    let mut buffer = [0_u8; 64 * 1024];
    while remaining != 0 {
        let maximum = usize::try_from(remaining.min(buffer.len() as u64))
            .map_err(|_| PackStoreError::CorruptState("pack copy length"))?;
        let read = input.read(&mut buffer[..maximum])?;
        if read == 0 {
            return Err(PackStoreError::Verification(
                PackVerificationError::SizeMismatch(entry.path.clone()),
            ));
        }
        output.write_all(&buffer[..read])?;
        hasher.update(&buffer[..read]);
        remaining -= read as u64;
    }
    if input.read(&mut buffer[..1])? != 0 {
        return Err(PackStoreError::Verification(
            PackVerificationError::SizeMismatch(entry.path.clone()),
        ));
    }
    if format!("{:x}", hasher.finalize()) != entry.sha256 {
        return Err(PackStoreError::Verification(
            PackVerificationError::PayloadDigestMismatch(entry.path.clone()),
        ));
    }
    output.flush()?;
    output.sync_all()?;
    destination.recheck()?;
    Ok(())
}

#[cfg(unix)]
fn create_file_at(
    parent: &AnchoredDirectory,
    name: &str,
    delete_access: bool,
) -> Result<File, PackStoreError> {
    let name = CString::new(name).expect("validated anchor name");
    let flags = libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC;
    let fd = unsafe { libc::openat(parent.leaf().as_raw_fd(), name.as_ptr(), flags, 0o600) };
    let _ = delete_access;
    if fd < 0 {
        return Err(PackStoreError::Io(io::Error::last_os_error()));
    }
    Ok(unsafe { File::from_raw_fd(fd) })
}

#[cfg(windows)]
fn create_file_at(
    parent: &AnchoredDirectory,
    name: &str,
    delete_access: bool,
) -> Result<File, PackStoreError> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::{FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ};
    parent.recheck()?;
    let mut options = OpenOptions::new();
    options
        .write(true)
        .access_mode(0x4000_0000 | if delete_access { 0x0001_0000 } else { 0 })
        .share_mode(FILE_SHARE_READ)
        .create_new(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    Ok(options.open(parent.path.join(name))?)
}

#[cfg(not(any(unix, windows)))]
fn create_file_at(
    _parent: &AnchoredDirectory,
    _name: &str,
    _delete_access: bool,
) -> Result<File, PackStoreError> {
    Err(PackStoreError::UnsupportedAnchoredFilesystem)
}

fn make_payload_readonly_anchored(root: &AnchoredDirectory) -> Result<(), PackStoreError> {
    root.recheck()?;
    for (name, kind) in anchored_entries(root)? {
        match kind {
            AnchoredEntryKind::Directory => {
                let child = root.open_child(&name, false)?;
                make_payload_readonly_anchored(&child)?;
            }
            AnchoredEntryKind::File => {
                let file = open_regular_file_at(root, &name)?;
                let metadata = file.metadata()?;
                if !metadata.is_file()
                    || is_link_or_reparse(&metadata)
                    || !regular_file_has_single_link(&file, &metadata)?
                {
                    return Err(PackStoreError::UnsafeFilesystemEntry(root.path.join(&name)));
                }
                let mut permissions = metadata.permissions();
                permissions.set_readonly(true);
                file.set_permissions(permissions)?;
            }
        }
    }
    root.recheck()?;
    Ok(())
}

#[cfg(unix)]
fn open_regular_file_at(parent: &AnchoredDirectory, name: &str) -> Result<File, PackStoreError> {
    let name = CString::new(name).expect("validated anchor name");
    let fd = unsafe {
        libc::openat(
            parent.leaf().as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(PackStoreError::Io(io::Error::last_os_error()));
    }
    Ok(unsafe { File::from_raw_fd(fd) })
}

#[cfg(windows)]
fn open_regular_file_at(parent: &AnchoredDirectory, name: &str) -> Result<File, PackStoreError> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ, FILE_SHARE_WRITE,
    };
    parent.recheck()?;
    let mut options = OpenOptions::new();
    options
        .access_mode(0x8000_0000 | 0x0000_0100)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    Ok(options.open(parent.path.join(name))?)
}

#[cfg(not(any(unix, windows)))]
fn open_regular_file_at(_parent: &AnchoredDirectory, _name: &str) -> Result<File, PackStoreError> {
    Err(PackStoreError::UnsupportedAnchoredFilesystem)
}

#[cfg(unix)]
fn regular_file_has_single_link(
    _file: &File,
    metadata: &fs::Metadata,
) -> Result<bool, PackStoreError> {
    use std::os::unix::fs::MetadataExt;
    Ok(metadata.nlink() == 1)
}

#[cfg(windows)]
fn regular_file_has_single_link(
    file: &File,
    _metadata: &fs::Metadata,
) -> Result<bool, PackStoreError> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
    };
    let mut information = unsafe { std::mem::zeroed::<BY_HANDLE_FILE_INFORMATION>() };
    if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut information) } == 0 {
        return Err(PackStoreError::Io(io::Error::last_os_error()));
    }
    Ok(information.nNumberOfLinks == 1)
}

#[cfg(not(any(unix, windows)))]
fn regular_file_has_single_link(
    _file: &File,
    _metadata: &fs::Metadata,
) -> Result<bool, PackStoreError> {
    Err(PackStoreError::UnsupportedAnchoredFilesystem)
}

fn remove_staging_tree_anchored(staging: &AnchoredDirectory) -> Result<(), PackStoreError> {
    let name = staging
        .path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    if !name.starts_with('.') || !name.contains(".staging-") {
        return Err(PackStoreError::UnsafeRecoveryTarget(staging.path.clone()));
    }
    staging.recheck()?;
    remove_anchored_contents(staging)?;
    remove_anchored_directory(staging)
}

fn remove_anchored_contents(directory: &AnchoredDirectory) -> Result<(), PackStoreError> {
    for (name, kind) in anchored_entries(directory)? {
        match kind {
            AnchoredEntryKind::Directory => {
                let child = directory.open_child(&name, true)?;
                remove_anchored_contents(&child)?;
                remove_anchored_directory(&child)?;
            }
            AnchoredEntryKind::File => remove_anchored_file(directory, &name)?,
        }
    }
    Ok(())
}

fn validate_anchor_name(name: &str) -> Result<(), PackStoreError> {
    if name.is_empty()
        || name.len() > 160
        || matches!(name, "." | "..")
        || name.contains(['/', '\\', ':'])
        || name.ends_with(['.', ' '])
        || name.bytes().any(|byte| byte < 0x20)
        || is_reserved_windows_component(name)
    {
        return Err(PackStoreError::CorruptState("anchored entry name"));
    }
    Ok(())
}

#[cfg(unix)]
fn open_absolute_directory_chain(
    path: &Path,
    create_missing: bool,
) -> Result<AnchoredDirectory, PackStoreError> {
    if !path.is_absolute() {
        return Err(PackStoreError::CorruptState(
            "anchored root must be absolute",
        ));
    }
    let mut anchored = AnchoredDirectory {
        path: PathBuf::from("/"),
        chain: vec![open_directory_anchor(Path::new("/"), false)?],
    };
    for component in path.components() {
        match component {
            Component::RootDir => {}
            Component::Normal(name) => {
                let name = name
                    .to_str()
                    .ok_or(PackStoreError::CorruptState("anchored root component"))?;
                validate_root_component(name)?;
                anchored = match descend_root_component(&anchored, name) {
                    Ok(child) => child,
                    Err(PackStoreError::Io(error))
                        if create_missing && error.kind() == io::ErrorKind::NotFound =>
                    {
                        create_directory_at(&anchored, name, false)?;
                        descend_root_component(&anchored, name)?
                    }
                    Err(error) => return Err(error),
                };
            }
            _ => return Err(PackStoreError::CorruptState("anchored root component")),
        }
    }
    anchored.recheck()?;
    Ok(anchored)
}

#[cfg(windows)]
fn open_absolute_directory_chain(
    path: &Path,
    create_missing: bool,
) -> Result<AnchoredDirectory, PackStoreError> {
    use std::path::Prefix;

    if !path.is_absolute() {
        return Err(PackStoreError::CorruptState(
            "anchored root must be absolute",
        ));
    }
    let mut components = path.components();
    let Component::Prefix(prefix) = components
        .next()
        .ok_or(PackStoreError::CorruptState("anchored root prefix"))?
    else {
        return Err(PackStoreError::CorruptState("anchored root prefix"));
    };
    if !matches!(prefix.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_))
        || !matches!(components.next(), Some(Component::RootDir))
    {
        return Err(PackStoreError::CorruptState(
            "unsupported anchored root prefix",
        ));
    }
    let mut root_path = PathBuf::from(prefix.as_os_str());
    root_path.push("\\");
    let mut anchored = AnchoredDirectory {
        path: root_path.clone(),
        chain: vec![open_directory_anchor(&root_path, false)?],
    };
    for component in components {
        let Component::Normal(name) = component else {
            return Err(PackStoreError::CorruptState("anchored root component"));
        };
        let name = name
            .to_str()
            .ok_or(PackStoreError::CorruptState("anchored root component"))?;
        validate_root_component(name)?;
        anchored = match descend_root_component(&anchored, name) {
            Ok(child) => child,
            Err(PackStoreError::Io(error))
                if create_missing && error.kind() == io::ErrorKind::NotFound =>
            {
                create_directory_at(&anchored, name, false)?;
                descend_root_component(&anchored, name)?
            }
            Err(error) => return Err(error),
        };
    }
    anchored.recheck()?;
    Ok(anchored)
}

#[cfg(not(any(unix, windows)))]
fn open_absolute_directory_chain(
    _path: &Path,
    _create_missing: bool,
) -> Result<AnchoredDirectory, PackStoreError> {
    Err(PackStoreError::UnsupportedAnchoredFilesystem)
}

fn validate_root_component(name: &str) -> Result<(), PackStoreError> {
    if name.is_empty()
        || matches!(name, "." | "..")
        || name.contains(['/', '\\', ':'])
        || name.ends_with(['.', ' '])
        || name.bytes().any(|byte| byte < 0x20)
        || is_reserved_windows_component(name)
    {
        return Err(PackStoreError::CorruptState("anchored root component"));
    }
    Ok(())
}

fn is_reserved_windows_component(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or(name).to_ascii_uppercase();
    matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL" | "CLOCK$")
        || (stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && matches!(stem.as_bytes()[3], b'1'..=b'9'))
}

fn descend_root_component(
    parent: &AnchoredDirectory,
    name: &str,
) -> Result<AnchoredDirectory, PackStoreError> {
    validate_root_component(name)?;
    let handle = open_directory_at(parent, name, false)?;
    let mut chain = parent
        .chain
        .iter()
        .map(File::try_clone)
        .collect::<io::Result<Vec<_>>>()?;
    chain.push(handle);
    let anchored = AnchoredDirectory {
        path: parent.path.join(name),
        chain,
    };
    anchored.recheck()?;
    Ok(anchored)
}

#[cfg(unix)]
fn sync_anchored_directory(directory: &AnchoredDirectory) -> Result<(), PackStoreError> {
    directory.recheck()?;
    directory.leaf().sync_all()?;
    Ok(())
}

#[cfg(windows)]
fn sync_anchored_directory(directory: &AnchoredDirectory) -> Result<(), PackStoreError> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ, FILE_SHARE_WRITE,
    };
    directory.recheck()?;
    let mut options = OpenOptions::new();
    options
        .access_mode(0x8000_0000 | 0x0000_0002 | 0x0000_0004)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT);
    let file = options.open(&directory.path)?;
    if directory_identity(&file)? != directory.identity()? {
        return Err(PackStoreError::UnsafeFilesystemEntry(
            directory.path.clone(),
        ));
    }
    file.sync_all()?;
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn sync_anchored_directory(_directory: &AnchoredDirectory) -> Result<(), PackStoreError> {
    Err(PackStoreError::UnsupportedAnchoredFilesystem)
}

#[cfg(unix)]
fn open_directory_anchor(path: &Path, _delete_share: bool) -> Result<File, PackStoreError> {
    use std::os::unix::ffi::OsStrExt;
    let raw = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| PackStoreError::CorruptState("anchored directory path"))?;
    let fd = unsafe {
        libc::open(
            raw.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(PackStoreError::Io(io::Error::last_os_error()));
    }
    Ok(unsafe { File::from_raw_fd(fd) })
}

#[cfg(windows)]
fn open_directory_anchor(path: &Path, delete_share: bool) -> Result<File, PackStoreError> {
    use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
        FILE_SHARE_READ, FILE_SHARE_WRITE,
    };
    let mut options = OpenOptions::new();
    options
        .access_mode(0x8000_0000 | if delete_share { 0x0001_0000 } else { 0 })
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT);
    let file = options.open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_dir() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(PackStoreError::UnsafeFilesystemEntry(path.to_path_buf()));
    }
    Ok(file)
}

#[cfg(not(any(unix, windows)))]
fn open_directory_anchor(_path: &Path, _delete_share: bool) -> Result<File, PackStoreError> {
    Err(PackStoreError::UnsupportedAnchoredFilesystem)
}

#[cfg(unix)]
fn open_directory_at(
    parent: &AnchoredDirectory,
    name: &str,
    _delete_share: bool,
) -> Result<File, PackStoreError> {
    let name = CString::new(name).expect("validated anchor name");
    let fd = unsafe {
        libc::openat(
            parent.leaf().as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(PackStoreError::Io(io::Error::last_os_error()));
    }
    Ok(unsafe { File::from_raw_fd(fd) })
}

#[cfg(windows)]
fn open_directory_at(
    parent: &AnchoredDirectory,
    name: &str,
    delete_share: bool,
) -> Result<File, PackStoreError> {
    parent.recheck()?;
    let path = parent.path.join(name);
    let file = open_directory_anchor(&path, delete_share)?;
    super::manifest::reject_named_streams(&path)?;
    Ok(file)
}

#[cfg(not(any(unix, windows)))]
fn open_directory_at(
    _parent: &AnchoredDirectory,
    _name: &str,
    _delete_share: bool,
) -> Result<File, PackStoreError> {
    Err(PackStoreError::UnsupportedAnchoredFilesystem)
}

#[cfg(unix)]
fn create_directory_at(
    parent: &AnchoredDirectory,
    name: &str,
    require_new: bool,
) -> Result<(), PackStoreError> {
    let name = CString::new(name).expect("validated anchor name");
    if unsafe { libc::mkdirat(parent.leaf().as_raw_fd(), name.as_ptr(), 0o700) } == 0 {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    if !require_new && error.raw_os_error() == Some(libc::EEXIST) {
        return Ok(());
    }
    Err(PackStoreError::Io(error))
}

#[cfg(windows)]
fn create_directory_at(
    parent: &AnchoredDirectory,
    name: &str,
    require_new: bool,
) -> Result<(), PackStoreError> {
    parent.recheck()?;
    match fs::create_dir(parent.path.join(name)) {
        Ok(()) => Ok(()),
        Err(error) if !require_new && error.kind() == io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(PackStoreError::Io(error)),
    }
}

#[cfg(not(any(unix, windows)))]
fn create_directory_at(
    _parent: &AnchoredDirectory,
    _name: &str,
    _require_new: bool,
) -> Result<(), PackStoreError> {
    Err(PackStoreError::UnsupportedAnchoredFilesystem)
}

#[cfg(unix)]
fn same_anchored_directory(handle: &File, path: &Path) -> Result<bool, PackStoreError> {
    use std::os::unix::fs::MetadataExt;
    let held = handle.metadata()?;
    let observed = fs::symlink_metadata(path)?;
    Ok(observed.is_dir()
        && !observed.file_type().is_symlink()
        && held.dev() == observed.dev()
        && held.ino() == observed.ino())
}

#[cfg(unix)]
fn directory_identity(file: &File) -> Result<DirectoryIdentity, PackStoreError> {
    use std::os::unix::fs::MetadataExt;
    let metadata = file.metadata()?;
    Ok(DirectoryIdentity {
        volume: metadata.dev(),
        file: metadata.ino(),
    })
}

#[cfg(windows)]
fn directory_identity(file: &File) -> Result<DirectoryIdentity, PackStoreError> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
    };
    let mut value = unsafe { std::mem::zeroed::<BY_HANDLE_FILE_INFORMATION>() };
    if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut value) } == 0 {
        return Err(PackStoreError::Io(io::Error::last_os_error()));
    }
    Ok(DirectoryIdentity {
        volume: u64::from(value.dwVolumeSerialNumber),
        file: (u64::from(value.nFileIndexHigh) << 32) | u64::from(value.nFileIndexLow),
    })
}

#[cfg(not(any(unix, windows)))]
fn directory_identity(_file: &File) -> Result<DirectoryIdentity, PackStoreError> {
    Err(PackStoreError::UnsupportedAnchoredFilesystem)
}

#[cfg(unix)]
fn remove_anchored_file(parent: &AnchoredDirectory, name: &str) -> Result<(), PackStoreError> {
    use std::mem::MaybeUninit;
    let name = CString::new(name).expect("validated anchor name");
    let mut stat = MaybeUninit::<libc::stat>::uninit();
    if unsafe {
        libc::fstatat(
            parent.leaf().as_raw_fd(),
            name.as_ptr(),
            stat.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } != 0
    {
        return Err(PackStoreError::Io(io::Error::last_os_error()));
    }
    let stat = unsafe { stat.assume_init() };
    if stat.st_mode & libc::S_IFMT != libc::S_IFREG || stat.st_nlink != 1 {
        return Err(PackStoreError::UnsafeFilesystemEntry(
            parent
                .path
                .join(name.to_str().expect("validated anchor name")),
        ));
    }
    if unsafe { libc::unlinkat(parent.leaf().as_raw_fd(), name.as_ptr(), 0) } != 0 {
        return Err(PackStoreError::Io(io::Error::last_os_error()));
    }
    Ok(())
}

#[cfg(unix)]
fn remove_anchored_directory(directory: &AnchoredDirectory) -> Result<(), PackStoreError> {
    directory.recheck()?;
    let name = directory
        .path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or(PackStoreError::CorruptState("anchored directory name"))?;
    let name = CString::new(name).expect("validated anchor name");
    if unsafe {
        libc::unlinkat(
            directory.parent_leaf()?.as_raw_fd(),
            name.as_ptr(),
            libc::AT_REMOVEDIR,
        )
    } != 0
    {
        return Err(PackStoreError::Io(io::Error::last_os_error()));
    }
    Ok(())
}

#[cfg(windows)]
fn remove_anchored_file(parent: &AnchoredDirectory, name: &str) -> Result<(), PackStoreError> {
    use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ,
        FILE_SHARE_WRITE,
    };
    parent.recheck()?;
    let path = parent.path.join(name);
    let metadata = fs::symlink_metadata(&path)?;
    if !metadata.is_file() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(PackStoreError::UnsafeFilesystemEntry(path));
    }
    let mut options = OpenOptions::new();
    options
        .access_mode(0x0001_0000 | 0x0000_0080)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    let file = options.open(&path)?;
    let observed = file.metadata()?;
    if !observed.is_file() || observed.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(PackStoreError::UnsafeFilesystemEntry(path));
    }
    delete_by_handle(&file)
}

#[cfg(windows)]
fn remove_anchored_directory(directory: &AnchoredDirectory) -> Result<(), PackStoreError> {
    directory.recheck()?;
    delete_by_handle(directory.leaf())
}

#[cfg(windows)]
fn delete_by_handle(file: &File) -> Result<(), PackStoreError> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_DISPOSITION_FLAG_DELETE, FILE_DISPOSITION_FLAG_IGNORE_READONLY_ATTRIBUTE,
        FILE_DISPOSITION_INFO_EX, FileDispositionInfoEx, SetFileInformationByHandle,
    };
    let mut info = FILE_DISPOSITION_INFO_EX {
        Flags: FILE_DISPOSITION_FLAG_DELETE | FILE_DISPOSITION_FLAG_IGNORE_READONLY_ATTRIBUTE,
    };
    if unsafe {
        SetFileInformationByHandle(
            file.as_raw_handle(),
            FileDispositionInfoEx,
            (&raw mut info).cast(),
            u32::try_from(std::mem::size_of::<FILE_DISPOSITION_INFO_EX>())
                .expect("disposition structure size"),
        )
    } == 0
    {
        return Err(PackStoreError::Io(io::Error::last_os_error()));
    }
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn remove_anchored_file(_parent: &AnchoredDirectory, _name: &str) -> Result<(), PackStoreError> {
    Err(PackStoreError::UnsupportedAnchoredFilesystem)
}

#[cfg(not(any(unix, windows)))]
fn remove_anchored_directory(_directory: &AnchoredDirectory) -> Result<(), PackStoreError> {
    Err(PackStoreError::UnsupportedAnchoredFilesystem)
}

#[cfg(windows)]
fn same_anchored_directory(handle: &File, path: &Path) -> Result<bool, PackStoreError> {
    use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS,
        FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
        GetFileInformationByHandle,
    };
    let mut options = OpenOptions::new();
    options
        .access_mode(0x8000_0000)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT);
    let observed = match options.open(path) {
        Ok(file) => file,
        Err(_) => return Ok(false),
    };
    let metadata = observed.metadata()?;
    if !metadata.is_dir() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Ok(false);
    }
    let identity = |file: &File| -> Result<BY_HANDLE_FILE_INFORMATION, PackStoreError> {
        let mut value = unsafe { std::mem::zeroed::<BY_HANDLE_FILE_INFORMATION>() };
        if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut value) } == 0 {
            return Err(PackStoreError::Io(io::Error::last_os_error()));
        }
        Ok(value)
    };
    let held = identity(handle)?;
    let observed = identity(&observed)?;
    Ok(held.dwVolumeSerialNumber == observed.dwVolumeSerialNumber
        && held.nFileIndexHigh == observed.nFileIndexHigh
        && held.nFileIndexLow == observed.nFileIndexLow)
}

#[cfg(not(any(unix, windows)))]
fn same_anchored_directory(_handle: &File, _path: &Path) -> Result<bool, PackStoreError> {
    Err(PackStoreError::UnsupportedAnchoredFilesystem)
}

fn anchored_child_names(directory: &AnchoredDirectory) -> Result<Vec<String>, PackStoreError> {
    Ok(anchored_entries(directory)?
        .into_iter()
        .map(|(name, _)| name)
        .collect())
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn anchored_entries(
    directory: &AnchoredDirectory,
) -> Result<Vec<(String, AnchoredEntryKind)>, PackStoreError> {
    #[cfg(target_os = "macos")]
    let _scan_guard = MACOS_ANCHORED_DIRECTORY_SCAN_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    directory.recheck()?;

    #[cfg(target_os = "linux")]
    let descriptor = unsafe {
        libc::openat(
            directory.leaf().as_raw_fd(),
            c".".as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
        )
    };
    // Darwin's directory descriptor cannot be portably reopened through
    // `openat(dirfd, ".", ...)`. Duplicate the retained authority instead.
    // The duplicate shares its open-file-description offset, so the process-
    // wide lock above and the `rewinddir` below must cover the complete scan.
    #[cfg(target_os = "macos")]
    let descriptor = unsafe { libc::fcntl(directory.leaf().as_raw_fd(), libc::F_DUPFD_CLOEXEC, 0) };
    if descriptor < 0 {
        return Err(PackStoreError::Io(io::Error::last_os_error()));
    }
    let reopened = unsafe { File::from_raw_fd(descriptor) };
    if directory_identity(&reopened)? != directory.identity()? {
        return Err(PackStoreError::UnsafeFilesystemEntry(
            directory.path.clone(),
        ));
    }
    let descriptor = reopened.into_raw_fd();
    let stream = unsafe { libc::fdopendir(descriptor) };
    if stream.is_null() {
        let error = io::Error::last_os_error();
        unsafe { libc::close(descriptor) };
        return Err(PackStoreError::Io(error));
    }
    let stream = UnixDirectoryStream(stream);
    #[cfg(target_os = "macos")]
    unsafe {
        libc::rewinddir(stream.0);
    }

    let mut entries = Vec::new();
    loop {
        unsafe { *unix_errno_location() = 0 };
        let entry = unsafe { libc::readdir(stream.0) };
        if entry.is_null() {
            let error = unsafe { *unix_errno_location() };
            if error != 0 {
                return Err(PackStoreError::Io(io::Error::from_raw_os_error(error)));
            }
            break;
        }
        let raw_name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) };
        let name = raw_name
            .to_str()
            .map_err(|_| PackStoreError::CorruptState("anchored child name"))?;
        if matches!(name, "." | "..") {
            continue;
        }
        validate_anchor_name(name)?;
        let kind = anchored_entry_kind(directory, name)?;
        entries.push((name.to_owned(), kind));
    }
    stream.close()?;
    directory.recheck()?;
    Ok(entries)
}

#[cfg(target_os = "linux")]
unsafe fn unix_errno_location() -> *mut libc::c_int {
    unsafe { libc::__errno_location() }
}

#[cfg(target_os = "macos")]
unsafe fn unix_errno_location() -> *mut libc::c_int {
    unsafe { libc::__error() }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn anchored_entry_kind(
    directory: &AnchoredDirectory,
    name: &str,
) -> Result<AnchoredEntryKind, PackStoreError> {
    let raw_name = CString::new(name).expect("validated anchor name");
    let mut metadata = std::mem::MaybeUninit::<libc::stat>::uninit();
    if unsafe {
        libc::fstatat(
            directory.leaf().as_raw_fd(),
            raw_name.as_ptr(),
            metadata.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } != 0
    {
        return Err(PackStoreError::Io(io::Error::last_os_error()));
    }
    let mode = unsafe { metadata.assume_init() }.st_mode;
    match mode & libc::S_IFMT {
        libc::S_IFDIR => Ok(AnchoredEntryKind::Directory),
        libc::S_IFREG => Ok(AnchoredEntryKind::File),
        _ => Err(PackStoreError::UnsafeFilesystemEntry(
            directory.path.join(name),
        )),
    }
}

#[cfg(windows)]
fn anchored_entries(
    directory: &AnchoredDirectory,
) -> Result<Vec<(String, AnchoredEntryKind)>, PackStoreError> {
    directory.recheck()?;
    let mut entries = Vec::new();
    for entry in fs::read_dir(&directory.path)? {
        let entry = entry?;
        let name = entry
            .file_name()
            .to_str()
            .ok_or(PackStoreError::CorruptState("anchored child name"))?
            .to_owned();
        validate_anchor_name(&name)?;
        let metadata = fs::symlink_metadata(entry.path())?;
        if is_link_or_reparse(&metadata) {
            return Err(PackStoreError::UnsafeFilesystemEntry(entry.path()));
        }
        let kind = if metadata.is_dir() {
            AnchoredEntryKind::Directory
        } else if metadata.is_file() {
            AnchoredEntryKind::File
        } else {
            return Err(PackStoreError::UnsafeFilesystemEntry(entry.path()));
        };
        entries.push((name, kind));
    }
    directory.recheck()?;
    Ok(entries)
}

#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
fn anchored_entries(
    _directory: &AnchoredDirectory,
) -> Result<Vec<(String, AnchoredEntryKind)>, PackStoreError> {
    Err(PackStoreError::UnsupportedAnchoredFilesystem)
}

pub(super) fn read_canonical_state<T>(path: &Path) -> Result<T, PackStoreError>
where
    T: for<'de> Deserialize<'de> + Serialize,
{
    let parent = path
        .parent()
        .ok_or(PackStoreError::CorruptState("state parent"))?;
    let root = AnchoredDirectory::open_root(parent)?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or(PackStoreError::CorruptState("state file name"))?;
    read_canonical_state_at(&root, name)
}

fn read_canonical_state_at<T>(root: &AnchoredDirectory, name: &str) -> Result<T, PackStoreError>
where
    T: for<'de> Deserialize<'de> + Serialize,
{
    let mut file = open_state_file_at(root, name, false)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || is_link_or_reparse(&metadata) || metadata.len() > MAX_STATE_BYTES {
        return Err(PackStoreError::CorruptState("state file bounds"));
    }
    super::manifest::reject_named_streams(&root.path.join(name))?;
    #[cfg(test)]
    run_state_read_hook(&root.path.join(name));
    let bytes = super::manifest::read_capped(&mut file, MAX_STATE_BYTES)?;
    if bytes.len() as u64 > MAX_STATE_BYTES {
        return Err(PackStoreError::CorruptState("state file bounds"));
    }
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
    let root = AnchoredDirectory::open_or_create_root(parent)?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or(PackStoreError::CorruptState("state file name"))?;
    atomic_write_canonical_at(&root, name, value)
}

fn atomic_write_canonical_at<T: Serialize>(
    root: &AnchoredDirectory,
    name: &str,
    value: &T,
) -> Result<(), PackStoreError> {
    validate_anchor_name(name)?;
    let temporary_name = format!(".{name}.tmp-{}", random_suffix()?);
    let bytes = serde_json::to_vec(value)?;
    let mut file = create_file_at(root, &temporary_name, true)?;
    file.write_all(&bytes)?;
    file.flush()?;
    file.sync_all()?;
    let result = replace_state_file_at(root, &temporary_name, &file, name);
    if result.is_err() {
        let _ = remove_state_temp_at(root, &temporary_name, &file);
    }
    result
}

pub(super) fn exclusive_file_lock(path: &Path) -> Result<ExclusiveFileLock, PackStoreError> {
    let state_root = path
        .parent()
        .ok_or(PackStoreError::CorruptState("lock parent"))?;
    let requested_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or(PackStoreError::CorruptState("lock file name"))?;
    validate_anchor_name(requested_name)?;
    let authority_parent_path = state_root
        .parent()
        .ok_or(PackStoreError::CorruptState("state authority parent"))?;
    let state_root_name = state_root
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or(PackStoreError::CorruptState("state root name"))?;
    validate_anchor_name(state_root_name)?;
    let authority_parent = AnchoredDirectory::open_or_create_root(authority_parent_path)?;
    #[cfg(unix)]
    let file = authority_parent.leaf().try_clone()?;
    #[cfg(windows)]
    let file = {
        let file = open_or_create_lock_at(&authority_parent, PRIVATE_STATE_AUTHORITY_LOCK_NAME)?;
        let metadata = file.metadata()?;
        if !metadata.is_file() || is_link_or_reparse(&metadata) {
            return Err(PackStoreError::UnsafeFilesystemEntry(path.to_path_buf()));
        }
        super::manifest::reject_named_streams(
            &authority_parent
                .path
                .join(PRIVATE_STATE_AUTHORITY_LOCK_NAME),
        )?;
        file
    };
    #[cfg(not(any(unix, windows)))]
    let file = return Err(PackStoreError::UnsupportedFileLock);
    lock_file(&file)?;
    authority_parent.recheck()?;
    let root = authority_parent.open_or_create_child(state_root_name, false)?;
    root.recheck()?;
    Ok(ExclusiveFileLock { file, root })
}

#[cfg(unix)]
fn open_state_file_at(
    root: &AnchoredDirectory,
    name: &str,
    write: bool,
) -> Result<File, PackStoreError> {
    validate_anchor_name(name)?;
    let name = CString::new(name).expect("validated state name");
    let flags =
        if write { libc::O_RDWR } else { libc::O_RDONLY } | libc::O_NOFOLLOW | libc::O_CLOEXEC;
    let fd = unsafe { libc::openat(root.leaf().as_raw_fd(), name.as_ptr(), flags) };
    if fd < 0 {
        return Err(PackStoreError::Io(io::Error::last_os_error()));
    }
    Ok(unsafe { File::from_raw_fd(fd) })
}

#[cfg(windows)]
fn open_state_file_at(
    root: &AnchoredDirectory,
    name: &str,
    write: bool,
) -> Result<File, PackStoreError> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ, FILE_SHARE_WRITE,
    };
    validate_anchor_name(name)?;
    root.recheck()?;
    let mut options = OpenOptions::new();
    options
        .read(true)
        .write(write)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    Ok(options.open(root.path.join(name))?)
}

#[cfg(not(any(unix, windows)))]
fn open_state_file_at(
    _root: &AnchoredDirectory,
    _name: &str,
    _write: bool,
) -> Result<File, PackStoreError> {
    Err(PackStoreError::UnsupportedAnchoredFilesystem)
}

#[cfg(windows)]
fn open_or_create_lock_at(root: &AnchoredDirectory, name: &str) -> Result<File, PackStoreError> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ, FILE_SHARE_WRITE,
    };
    validate_anchor_name(name)?;
    root.recheck()?;
    let mut options = OpenOptions::new();
    options
        .read(true)
        .write(true)
        .create(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    Ok(options.open(root.path.join(name))?)
}

fn state_file_exists_at(root: &AnchoredDirectory, name: &str) -> Result<bool, PackStoreError> {
    match open_state_file_at(root, name, false) {
        Ok(file) => {
            let metadata = file.metadata()?;
            if !metadata.is_file() || is_link_or_reparse(&metadata) {
                return Err(PackStoreError::UnsafeFilesystemEntry(root.path.join(name)));
            }
            Ok(true)
        }
        Err(PackStoreError::Io(error)) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

#[cfg(unix)]
fn replace_state_file_at(
    root: &AnchoredDirectory,
    temporary_name: &str,
    _temporary: &File,
    destination_name: &str,
) -> Result<(), PackStoreError> {
    let temporary_name = CString::new(temporary_name).expect("validated temporary name");
    let destination_name = CString::new(destination_name).expect("validated state name");
    if unsafe {
        libc::renameat(
            root.leaf().as_raw_fd(),
            temporary_name.as_ptr(),
            root.leaf().as_raw_fd(),
            destination_name.as_ptr(),
        )
    } != 0
    {
        return Err(PackStoreError::Io(io::Error::last_os_error()));
    }
    sync_anchored_directory(root)?;
    Ok(())
}

#[cfg(windows)]
fn replace_state_file_at(
    root: &AnchoredDirectory,
    _temporary_name: &str,
    temporary: &File,
    destination_name: &str,
) -> Result<(), PackStoreError> {
    rename_file_handle_into(temporary, root, destination_name, true)?;
    sync_anchored_directory(root)?;
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn replace_state_file_at(
    _root: &AnchoredDirectory,
    _temporary_name: &str,
    _temporary: &File,
    _destination_name: &str,
) -> Result<(), PackStoreError> {
    Err(PackStoreError::UnsupportedAnchoredFilesystem)
}

#[cfg(unix)]
fn remove_state_temp_at(
    root: &AnchoredDirectory,
    name: &str,
    _file: &File,
) -> Result<(), PackStoreError> {
    remove_anchored_file(root, name)
}

#[cfg(windows)]
fn remove_state_temp_at(
    _root: &AnchoredDirectory,
    _name: &str,
    file: &File,
) -> Result<(), PackStoreError> {
    delete_by_handle(file)
}

#[cfg(not(any(unix, windows)))]
fn remove_state_temp_at(
    _root: &AnchoredDirectory,
    _name: &str,
    _file: &File,
) -> Result<(), PackStoreError> {
    Err(PackStoreError::UnsupportedAnchoredFilesystem)
}

fn remove_state_file_at(root: &AnchoredDirectory, name: &str) -> Result<(), PackStoreError> {
    if !state_file_exists_at(root, name)? {
        return Ok(());
    }
    #[cfg(unix)]
    {
        return remove_anchored_file(root, name);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ, FILE_SHARE_WRITE,
        };
        root.recheck()?;
        let mut options = OpenOptions::new();
        options
            .access_mode(0x0001_0000 | 0x0000_0080)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
        let file = options.open(root.path.join(name))?;
        delete_by_handle(&file)
    }
    #[cfg(not(any(unix, windows)))]
    Err(PackStoreError::UnsupportedAnchoredFilesystem)
}

fn random_suffix() -> Result<String, PackStoreError> {
    let mut bytes = [0_u8; 16];
    fill(&mut bytes).map_err(|error| PackStoreError::Random(error.to_string()))?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

#[cfg(windows)]
fn is_link_or_reparse(metadata: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    metadata.file_type().is_symlink()
        || metadata.file_attributes()
            & windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT
            != 0
}

#[cfg(target_os = "linux")]
fn durable_rename_new_anchored(
    source: &AnchoredDirectory,
    destination_parent: &AnchoredDirectory,
    destination_name: &str,
) -> Result<PublishOutcome, PackStoreError> {
    validate_anchor_name(destination_name)?;
    let source_name = source
        .path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or(PackStoreError::CorruptState("staging directory name"))?;
    let source_name = CString::new(source_name).expect("validated staging name");
    let destination_name = CString::new(destination_name).expect("validated digest name");
    let expected = source.identity()?;
    let result = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            destination_parent.leaf().as_raw_fd(),
            source_name.as_ptr(),
            destination_parent.leaf().as_raw_fd(),
            destination_name.as_ptr(),
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
    let published =
        destination_parent.open_child(destination_name.to_str().expect("ASCII digest"), false)?;
    if published.identity()? != expected {
        return Err(PackStoreError::UnsafeFilesystemEntry(published.path));
    }
    sync_anchored_directory(&published)?;
    sync_anchored_directory(destination_parent)?;
    Ok(PublishOutcome::Published)
}

#[cfg(target_os = "macos")]
fn durable_rename_new_anchored(
    source: &AnchoredDirectory,
    destination_parent: &AnchoredDirectory,
    destination_name: &str,
) -> Result<PublishOutcome, PackStoreError> {
    validate_anchor_name(destination_name)?;
    let source_name = source
        .path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or(PackStoreError::CorruptState("staging directory name"))?;
    let source_name = CString::new(source_name).expect("validated staging name");
    let destination_name = CString::new(destination_name).expect("validated digest name");
    let expected = source.identity()?;
    if directory_identity(source.parent_leaf()?)? != destination_parent.identity()? {
        return Err(PackStoreError::UnsafeFilesystemEntry(source.path.clone()));
    }
    let result = unsafe {
        libc::renameatx_np(
            destination_parent.leaf().as_raw_fd(),
            source_name.as_ptr(),
            destination_parent.leaf().as_raw_fd(),
            destination_name.as_ptr(),
            libc::RENAME_EXCL,
        )
    };
    if result != 0 {
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::EEXIST) {
            return Ok(PublishOutcome::DestinationExists);
        }
        if error.raw_os_error() == Some(libc::ENOTSUP) {
            return Err(PackStoreError::UnsupportedAtomicPublish);
        }
        return Err(PackStoreError::Io(error));
    }
    let destination_name = destination_name.to_str().expect("ASCII digest");
    let published = destination_parent.open_child(destination_name, false)?;
    if published.identity()? != expected {
        return Err(PackStoreError::UnsafeFilesystemEntry(published.path));
    }
    sync_anchored_directory(&published)?;
    sync_anchored_directory(destination_parent)?;
    Ok(PublishOutcome::Published)
}

#[cfg(windows)]
fn durable_rename_new_anchored(
    source: &AnchoredDirectory,
    destination_parent: &AnchoredDirectory,
    destination_name: &str,
) -> Result<PublishOutcome, PackStoreError> {
    rename_handle_into(source, destination_parent, destination_name, false)
}

#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
fn durable_rename_new_anchored(
    _source: &AnchoredDirectory,
    _destination_parent: &AnchoredDirectory,
    _destination_name: &str,
) -> Result<PublishOutcome, PackStoreError> {
    Err(PackStoreError::UnsupportedAtomicPublish)
}

#[cfg(windows)]
fn rename_handle_into(
    source: &AnchoredDirectory,
    destination_parent: &AnchoredDirectory,
    destination_name: &str,
    replace: bool,
) -> Result<PublishOutcome, PackStoreError> {
    source.recheck()?;
    rename_file_handle_into(source.leaf(), destination_parent, destination_name, replace)
}

#[cfg(windows)]
fn rename_file_handle_into(
    source: &File,
    destination_parent: &AnchoredDirectory,
    destination_name: &str,
    replace: bool,
) -> Result<PublishOutcome, PackStoreError> {
    use std::mem::size_of;
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::{ERROR_ALREADY_EXISTS, ERROR_FILE_EXISTS};
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_RENAME_INFO, FILE_RENAME_INFO_0, FileRenameInfo, SetFileInformationByHandle,
    };
    validate_anchor_name(destination_name)?;
    destination_parent.recheck()?;
    let destination = destination_parent.path.join(destination_name);
    let wide = destination.as_os_str().encode_wide().collect::<Vec<_>>();
    let bytes = size_of::<FILE_RENAME_INFO>() + wide.len() * size_of::<u16>();
    let words = bytes.div_ceil(size_of::<usize>());
    let mut buffer = vec![0_usize; words];
    let info = buffer.as_mut_ptr().cast::<FILE_RENAME_INFO>();
    unsafe {
        (*info).Anonymous = FILE_RENAME_INFO_0 {
            ReplaceIfExists: u8::from(replace),
        };
        (*info).RootDirectory = std::ptr::null_mut();
        (*info).FileNameLength = u32::try_from(wide.len() * size_of::<u16>())
            .map_err(|_| PackStoreError::CorruptState("rename target length"))?;
        std::ptr::copy_nonoverlapping(wide.as_ptr(), (*info).FileName.as_mut_ptr(), wide.len());
    }
    if unsafe {
        SetFileInformationByHandle(
            source.as_raw_handle(),
            FileRenameInfo,
            info.cast(),
            u32::try_from(bytes)
                .map_err(|_| PackStoreError::CorruptState("rename buffer length"))?,
        )
    } == 0
    {
        let error = io::Error::last_os_error();
        if matches!(
            error.raw_os_error().map(|value| value as u32),
            Some(ERROR_ALREADY_EXISTS) | Some(ERROR_FILE_EXISTS)
        ) {
            return Ok(PublishOutcome::DestinationExists);
        }
        return Err(PackStoreError::Io(error));
    }
    Ok(PublishOutcome::Published)
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

fn lock_file(file: &File) -> Result<(), PackStoreError> {
    // Native writes and directory flushes can exceed the old 80 ms window on
    // loaded or power-throttled systems. Keep contention bounded below the
    // two-second caller contract while allowing a normal in-flight mutation
    // to finish instead of surfacing a spurious persistence failure.
    const MAX_ATTEMPTS: usize = 100;
    const RETRY_DELAY: std::time::Duration = std::time::Duration::from_millis(5);

    for attempt in 0..MAX_ATTEMPTS {
        match try_lock_file(file) {
            Err(PackStoreError::LockContended) if attempt + 1 < MAX_ATTEMPTS => {
                std::thread::sleep(RETRY_DELAY);
            }
            result => return result,
        }
    }
    Err(PackStoreError::LockContended)
}

#[cfg(unix)]
fn try_lock_file(file: &File) -> Result<(), PackStoreError> {
    use std::os::fd::AsRawFd;
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
        Ok(())
    } else {
        let error = io::Error::last_os_error();
        if error
            .raw_os_error()
            .is_some_and(|code| code == libc::EAGAIN || code == libc::EWOULDBLOCK)
        {
            Err(PackStoreError::LockContended)
        } else {
            Err(PackStoreError::Io(error))
        }
    }
}

#[cfg(windows)]
fn try_lock_file(file: &File) -> Result<(), PackStoreError> {
    use std::mem::zeroed;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::ERROR_LOCK_VIOLATION;
    use windows_sys::Win32::Storage::FileSystem::{
        LOCKFILE_EXCLUSIVE_LOCK, LOCKFILE_FAIL_IMMEDIATELY, LockFileEx,
    };
    use windows_sys::Win32::System::IO::OVERLAPPED;
    let mut overlapped: OVERLAPPED = unsafe { zeroed() };
    if unsafe {
        LockFileEx(
            file.as_raw_handle(),
            LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY,
            0,
            1,
            0,
            &mut overlapped,
        )
    } != 0
    {
        Ok(())
    } else {
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(ERROR_LOCK_VIOLATION as i32) {
            Err(PackStoreError::LockContended)
        } else {
            Err(PackStoreError::Io(error))
        }
    }
}

#[cfg(not(any(unix, windows)))]
fn try_lock_file(_file: &File) -> Result<(), PackStoreError> {
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
    #[allow(
        dead_code,
        reason = "constructed only on platforms outside the supported Windows and Unix publish implementations"
    )]
    UnsupportedAtomicPublish,
    #[error("this platform has no supported OS-backed file lock")]
    #[allow(
        dead_code,
        reason = "constructed only on platforms outside the supported Windows and Unix lock implementations"
    )]
    UnsupportedFileLock,
    #[error("worker-pack state authority lock is contended")]
    LockContended,
    #[error("this platform has no supported anchored filesystem operations")]
    #[allow(
        dead_code,
        reason = "constructed only on platforms outside the supported Windows and Unix anchored filesystem implementations"
    )]
    UnsupportedAnchoredFilesystem,
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
    #[cfg(unix)]
    use std::cell::Cell;
    use std::sync::Arc;

    fn current_descriptor(store: &PackStore<'_>) -> Option<VerifiedPack> {
        store
            .current_fail_closed()
            .map(|lease| lease.verified_pack().clone())
    }

    fn discovery_descriptor(pack_id: &str, epoch: u64) -> VerifiedPack {
        let root = temp_root("discovery-epoch-descriptor");
        let source = root.join("source");
        fs::create_dir(&source).unwrap();
        let (_, mut descriptor) = fixture(&source);
        descriptor.pack_id = StoreComponent::new(pack_id).unwrap();
        descriptor.security_epoch = epoch;
        fs::remove_dir_all(root).unwrap();
        descriptor
    }

    #[test]
    fn discovery_epoch_ledger_persists_advances_and_rejects_restart_downgrade() {
        let root = temp_root("discovery-epoch-ledger");
        let state = root.join("state");
        let ledger = DiscoveryEpochLedger::new(&state);
        let epoch_one = discovery_descriptor("metal-main", 1);
        ledger.admit(&[&epoch_one]).unwrap();
        assert!(state.join(DISCOVERY_EPOCH_STATE_NAME).is_file());
        ledger.admit(&[&epoch_one]).unwrap();

        let epoch_three = discovery_descriptor("metal-main", 3);
        ledger.admit(&[&epoch_three]).unwrap();
        assert_eq!(
            ledger
                .load_strict()
                .unwrap()
                .epochs
                .get(&discovery_epoch_key(&epoch_three).unwrap()),
            Some(&3)
        );

        let restarted = DiscoveryEpochLedger::new(&state);
        let epoch_two = discovery_descriptor("metal-main", 2);
        assert!(matches!(
            restarted.admit(&[&epoch_two]),
            Err(PackStoreError::SecurityEpochDowngrade {
                observed: 2,
                floor: 3
            })
        ));

        let distinct = discovery_descriptor("metal-secondary", 1);
        restarted.admit(&[&distinct]).unwrap();
        assert_eq!(restarted.load_strict().unwrap().epochs.len(), 2);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn discovery_epoch_ledger_fails_closed_on_batch_rollback_corruption_and_bad_authority() {
        let root = temp_root("discovery-epoch-fail-closed");
        let state = root.join("state");
        let ledger = DiscoveryEpochLedger::new(&state);
        let high = discovery_descriptor("metal-main", 4);
        let low = discovery_descriptor("metal-main", 3);
        assert!(matches!(
            ledger.admit(&[&high, &low]),
            Err(PackStoreError::SecurityEpochDowngrade {
                observed: 3,
                floor: 4
            })
        ));
        assert!(!state.join(DISCOVERY_EPOCH_STATE_NAME).exists());

        let persisted_state = root.join("persisted-state");
        let persisted = DiscoveryEpochLedger::new(&persisted_state);
        persisted.admit(&[&high]).unwrap();
        fs::write(
            persisted_state.join(DISCOVERY_EPOCH_STATE_NAME),
            b"not-json",
        )
        .unwrap();
        assert!(persisted.admit(&[&high]).is_err());

        fs::remove_file(persisted_state.join(DISCOVERY_EPOCH_STATE_NAME)).unwrap();
        assert!(
            persisted.admit(&[&low]).is_err(),
            "a deleted high-water ledger must not reset the floor"
        );

        let authority_file = root.join("authority-file");
        fs::write(&authority_file, b"not-a-directory").unwrap();
        let unavailable = DiscoveryEpochLedger::new(authority_file.join("state"));
        assert!(unavailable.admit(&[&high]).is_err());

        let empty_root = root.join("empty-state");
        DiscoveryEpochLedger::new(&empty_root).admit(&[]).unwrap();
        assert!(
            !empty_root.exists(),
            "empty trust/discovery must not mutate state"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn discovery_epoch_ledger_lock_contention_is_bounded_and_does_not_mutate_state() {
        let root = temp_root("discovery-epoch-lock-contention");
        let state = root.join("state");
        let held = exclusive_file_lock(&state.join(DISCOVERY_EPOCH_LOCK_NAME)).unwrap();
        let pack = discovery_descriptor("metal-main", 2);
        let started = std::time::Instant::now();
        assert!(matches!(
            DiscoveryEpochLedger::new(&state).admit(&[&pack]),
            Err(PackStoreError::LockContended)
        ));
        assert!(
            started.elapsed() < std::time::Duration::from_secs(2),
            "discovery lock contention exceeded its conservative bound"
        );
        assert!(
            !state.join(DISCOVERY_EPOCH_STATE_NAME).exists(),
            "contended admission mutated epoch state"
        );
        drop(held);
        fs::remove_dir_all(root).unwrap();
    }
    use super::super::manifest::test_support::{base_manifest, fixture, temp_root, write_signed};
    use super::super::manifest::{Compatibility, PackBackend};

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
        manifest.pack_version = StoreComponent::new(version).unwrap();
        manifest.security_epoch = epoch;
        let trust = write_signed(&source, manifest);
        (source, trust)
    }

    fn replace_pack_id_with_directory_link(
        root: &Path,
        workers: &Path,
        descriptor: &VerifiedPack,
    ) -> (PathBuf, PathBuf) {
        let link = workers.join("packs").join(descriptor.pack_id.as_str());
        let external = root.join("external-pack-id");
        fs::rename(&link, &external).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&external, &link).unwrap();
        #[cfg(windows)]
        {
            let output = std::process::Command::new("cmd.exe")
                .args(["/d", "/c", "mklink", "/J"])
                .arg(&link)
                .arg(&external)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "junction fixture failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        (link, external)
    }

    fn restore_pack_id_directory(link: &Path, external: &Path) {
        #[cfg(unix)]
        fs::remove_file(link).unwrap();
        #[cfg(windows)]
        fs::remove_dir(link).unwrap();
        fs::rename(external, link).unwrap();
    }

    fn create_directory_link(link: &Path, target: &Path) {
        #[cfg(unix)]
        std::os::unix::fs::symlink(target, link).unwrap();
        #[cfg(windows)]
        {
            let output = std::process::Command::new("cmd.exe")
                .args(["/d", "/c", "mklink", "/J"])
                .arg(link)
                .arg(target)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "junction fixture failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }

    fn remove_directory_link(link: &Path) {
        #[cfg(unix)]
        fs::remove_file(link).unwrap();
        #[cfg(windows)]
        fs::remove_dir(link).unwrap();
    }

    #[cfg(unix)]
    fn rename_open_directory_or_reject(source: &Path, destination: &Path) -> bool {
        use std::os::unix::fs::MetadataExt;

        let before = fs::symlink_metadata(source).unwrap();
        assert!(before.is_dir() && !before.file_type().is_symlink());
        match fs::rename(source, destination) {
            Ok(()) => true,
            Err(error) if error.raw_os_error() == Some(libc::EPERM) => {
                let after = fs::symlink_metadata(source).unwrap();
                assert!(after.is_dir() && !after.file_type().is_symlink());
                assert_eq!((after.dev(), after.ino()), (before.dev(), before.ino()));
                assert!(!destination.exists());
                false
            }
            Err(error) => panic!("open-directory rename failed with an unexpected error: {error}"),
        }
    }

    #[test]
    fn anchored_directory_enumeration_does_not_depend_on_descriptor_paths() {
        let root = temp_root("descriptor-directory-enumeration");
        let authority = root.join("authority");
        fs::create_dir(&authority).unwrap();
        fs::create_dir(authority.join("child")).unwrap();
        fs::write(authority.join("payload.bin"), b"payload").unwrap();
        let anchored = AnchoredDirectory::open_root(&authority).unwrap();

        let mut entries = anchored_entries(&anchored).unwrap();
        entries.sort_by(|left, right| left.0.cmp(&right.0));
        assert_eq!(
            entries,
            vec![
                ("child".to_owned(), AnchoredEntryKind::Directory),
                ("payload.bin".to_owned(), AnchoredEntryKind::File),
            ]
        );

        let mut repeated = anchored_entries(&anchored).unwrap();
        repeated.sort_by(|left, right| left.0.cmp(&right.0));
        assert_eq!(repeated, entries);

        drop(anchored);
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn anchored_directory_enumeration_serializes_and_rewinds_shared_offsets() {
        const THREADS: usize = 8;
        const SCANS_PER_THREAD: usize = 32;

        let root = temp_root("descriptor-directory-concurrent-enumeration");
        let authority = root.join("authority");
        fs::create_dir(&authority).unwrap();
        fs::create_dir(authority.join("child")).unwrap();
        fs::write(authority.join("payload.bin"), b"payload").unwrap();
        let anchored = Arc::new(AnchoredDirectory::open_root(&authority).unwrap());
        let barrier = Arc::new(std::sync::Barrier::new(THREADS));
        let expected = vec![
            ("child".to_owned(), AnchoredEntryKind::Directory),
            ("payload.bin".to_owned(), AnchoredEntryKind::File),
        ];

        let threads = (0..THREADS)
            .map(|_| {
                let anchored = Arc::clone(&anchored);
                let barrier = Arc::clone(&barrier);
                let expected = expected.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    for _ in 0..SCANS_PER_THREAD {
                        let mut entries = anchored_entries(&anchored).unwrap();
                        entries.sort_by(|left, right| left.0.cmp(&right.0));
                        assert_eq!(entries, expected);
                    }
                })
            })
            .collect::<Vec<_>>();
        for thread in threads {
            thread.join().unwrap();
        }

        drop(anchored);
        fs::remove_dir_all(root).unwrap();
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
                .join(installed.pack_id.as_str())
                .join(installed.pack_version.as_str())
                .join(&installed.pack_digest)
        );
        store.activate(&installed).unwrap();
        assert_eq!(current_descriptor(&store), Some(installed.clone()));
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
        assert_eq!(current_descriptor(&store), Some(second.clone()));
        assert_eq!(store.rollback().unwrap(), first);
        assert_eq!(
            store
                .current_fail_closed()
                .unwrap()
                .verified_pack()
                .pack_version
                .as_str(),
            "1.2.3"
        );

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
        assert!(store.current_fail_closed().is_none());

        let staging = workers
            .join("packs")
            .join(installed.pack_id.as_str())
            .join(installed.pack_version.as_str())
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
        assert!(store.current_fail_closed().is_none());
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
        atomic_write_canonical(&store.activation_path(), &activation).unwrap();
        assert!(matches!(
            store.rollback(),
            Err(PackStoreError::DescriptorOutsideStore)
        ));
        assert!(store.current_fail_closed().is_none());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn persisted_activation_rejects_hostile_components_and_escape_paths() {
        let (root, workers, state, verifier, _) = store_fixture("hostile-activation-state");
        let store = PackStore::new(&workers, &state, &verifier);
        let installed = store.stage_and_install(&root.join("source")).unwrap();
        store.activate(&installed).unwrap();
        let baseline = store.load_activation_strict().unwrap();
        let hostile = [
            ".",
            "..",
            "a/b",
            "a\\b",
            "c:escape",
            "name:stream",
            "con",
            "con.txt",
            "nul.dll",
            "com1.sys",
            "lpt9.log",
            "trailing.",
            "trailing ",
        ];
        for value in hostile {
            for mutate_version in [false, true] {
                let mut corrupt = baseline.clone();
                let descriptor = corrupt.current.as_mut().unwrap();
                if mutate_version {
                    descriptor.pack_version = StoreComponent::test_unchecked(value);
                } else {
                    descriptor.pack_id = StoreComponent::test_unchecked(value);
                }
                atomic_write_canonical(&store.activation_path(), &corrupt).unwrap();
                assert!(
                    store.load_activation_strict().is_err(),
                    "accepted persisted component {value:?}"
                );
                assert!(store.current_fail_closed().is_none());
            }
        }

        let escaped = root.join("escaped-pack");
        fs::create_dir(&escaped).unwrap();
        fs::write(escaped.join("sentinel"), b"outside").unwrap();
        let mut corrupt = baseline.clone();
        corrupt.current.as_mut().unwrap().root = escaped.clone();
        atomic_write_canonical(&store.activation_path(), &corrupt).unwrap();
        assert!(matches!(
            store.load_activation_strict(),
            Err(PackStoreError::DescriptorOutsideStore)
        ));
        assert_eq!(fs::read(escaped.join("sentinel")).unwrap(), b"outside");

        atomic_write_canonical(&store.activation_path(), &baseline).unwrap();
        assert_eq!(current_descriptor(&store), Some(installed));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn persisted_pending_rejects_hostile_descriptors_before_recovery() {
        let (root, workers, state, verifier, _) = store_fixture("hostile-pending-state");
        let store = PackStore::new(&workers, &state, &verifier);
        let first = store.stage_and_install(&root.join("source")).unwrap();
        store.activate(&first).unwrap();
        let (next_source, _) = additional_source(&root, "2.0.0", 2);
        let next = store.stage_and_install(&next_source).unwrap();
        assert!(matches!(
            store.activate_locked(&next, Some(ActivationBoundary::Journal)),
            Err(PackStoreError::InjectedInterruption)
        ));
        let baseline: PendingActivation = read_canonical_state(&store.pending_path()).unwrap();
        for value in [
            ".",
            "..",
            "a/b",
            "a\\b",
            "c:escape",
            "con.txt",
            "nul",
            "lpt9.sys",
            "trailing.",
            "trailing ",
        ] {
            for mutate_version in [false, true] {
                let mut corrupt = baseline.clone();
                let target = if mutate_version {
                    &mut corrupt.target.pack_version
                } else {
                    &mut corrupt.target.pack_id
                };
                *target = StoreComponent::test_unchecked(value);
                corrupt.next_activation.current = Some(corrupt.target.clone());
                atomic_write_canonical(&store.pending_path(), &corrupt).unwrap();
                assert!(
                    store.recover_pending_activation_locked().is_err(),
                    "accepted pending component {value:?}"
                );
            }
        }

        let escaped = root.join("escaped-pending-pack");
        fs::create_dir(&escaped).unwrap();
        fs::write(escaped.join("sentinel"), b"outside").unwrap();
        let mut corrupt = baseline.clone();
        corrupt.target.root = escaped.clone();
        corrupt.next_activation.current = Some(corrupt.target.clone());
        atomic_write_canonical(&store.pending_path(), &corrupt).unwrap();
        assert!(matches!(
            store.recover_pending_activation_locked(),
            Err(PackStoreError::DescriptorOutsideStore)
        ));
        assert_eq!(fs::read(escaped.join("sentinel")).unwrap(), b"outside");

        atomic_write_canonical(&store.pending_path(), &baseline).unwrap();
        assert_eq!(current_descriptor(&store), Some(next));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn persisted_epoch_keys_are_canonical_store_components() {
        let (root, workers, state, verifier, _) = store_fixture("hostile-epoch-state");
        let store = PackStore::new(&workers, &state, &verifier);
        fs::create_dir_all(&state).unwrap();
        for hostile in [".", "..", "a/b", "c:escape", "con.txt", "trailing."] {
            let mut epochs = EpochState::empty();
            epochs.epochs.insert(hostile.to_owned(), 1);
            atomic_write_canonical(&store.epoch_path(), &epochs).unwrap();
            assert!(store.load_epochs_strict().is_err());
        }
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
            assert!(matches!(
                store.activate_locked(&next, Some(boundary)),
                Err(PackStoreError::InjectedInterruption)
            ));
            assert_eq!(current_descriptor(&store), Some(next.clone()));
            let activation = store.load_activation_strict().unwrap();
            assert_eq!(activation.previous, Some(first));
            assert!(!store.pending_path().exists());
            assert_eq!(
                store
                    .load_epochs_strict()
                    .unwrap()
                    .epochs
                    .get(next.pack_id.as_str()),
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
        assert!(store.current_fail_closed().is_none());
        assert!(store.activate(&first).is_err());
        assert_eq!(
            store
                .load_epochs_strict()
                .unwrap()
                .epochs
                .get(first.pack_id.as_str()),
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
        let (second_result, third_result) = std::thread::scope(|scope| {
            let left = PackStore::new(&workers, &state, &verifier);
            let right = PackStore::new(&workers, &state, &verifier);
            let second = second.clone();
            let third = third.clone();
            let left = scope.spawn(move || left.activate(&second));
            let right = scope.spawn(move || right.activate(&third));
            (left.join().unwrap(), right.join().unwrap())
        });
        let activation = store.load_activation_strict().unwrap();
        let current = activation.current.unwrap();
        let previous = activation.previous.unwrap();
        match (second_result, third_result) {
            (Ok(()), Ok(())) => assert!(
                (current == second && previous == third)
                    || (current == third && previous == second)
            ),
            (Ok(()), Err(PackStoreError::LockContended)) => {
                assert_eq!((current, previous), (second, first))
            }
            (Err(PackStoreError::LockContended), Ok(())) => {
                assert_eq!((current, previous), (third, first))
            }
            results => panic!("unexpected concurrent activation results: {results:?}"),
        }
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
        let higher_succeeded = std::thread::scope(|scope| {
            let left = PackStore::new(&workers, &state, &verifier);
            let right = PackStore::new(&workers, &state, &verifier);
            let epoch_two = epoch_two.clone();
            let epoch_three = epoch_three.clone();
            let lower = scope.spawn(move || left.activate(&epoch_two));
            let higher = scope.spawn(move || right.activate(&epoch_three));
            let lower = lower.join().unwrap();
            let higher = higher.join().unwrap();
            if let Err(error) = lower {
                assert!(matches!(
                    error,
                    PackStoreError::SecurityEpochDowngrade {
                        observed: 2,
                        floor: 3
                    } | PackStoreError::LockContended
                ));
            }
            match higher {
                Ok(()) => true,
                Err(PackStoreError::LockContended) => false,
                Err(error) => panic!("unexpected higher-epoch activation error: {error}"),
            }
        });
        if !higher_succeeded {
            store.activate(&epoch_three).unwrap();
        }
        let epochs = store.load_epochs_strict().unwrap();
        assert_eq!(epochs.epochs.get(epoch_three.pack_id.as_str()), Some(&3));
        assert_eq!(current_descriptor(&store), Some(epoch_three));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn linked_pack_id_ancestor_is_never_accepted_for_current_activation() {
        let (root, workers, state, verifier, _) = store_fixture("linked-current");
        let store = PackStore::new(&workers, &state, &verifier);
        let installed = store.stage_and_install(&root.join("source")).unwrap();
        store.activate(&installed).unwrap();
        let (link, external) = replace_pack_id_with_directory_link(&root, &workers, &installed);
        let sentinel = external.join("sentinel.txt");
        fs::write(&sentinel, b"must remain untouched").unwrap();

        assert!(store.current_fail_closed().is_none());
        assert_eq!(fs::read(&sentinel).unwrap(), b"must remain untouched");

        restore_pack_id_directory(&link, &external);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn linked_pack_id_ancestor_blocks_pending_activation_recovery() {
        let (root, workers, state, verifier, _) = store_fixture("linked-pending");
        let store = PackStore::new(&workers, &state, &verifier);
        let first = store.stage_and_install(&root.join("source")).unwrap();
        let (next_source, _) = additional_source(&root, "2.0.0", 2);
        let next = store.stage_and_install(&next_source).unwrap();
        store.activate(&first).unwrap();
        assert!(matches!(
            store.activate_locked(&next, Some(ActivationBoundary::Journal)),
            Err(PackStoreError::InjectedInterruption)
        ));
        let (link, external) = replace_pack_id_with_directory_link(&root, &workers, &next);
        let sentinel = external.join("sentinel.txt");
        fs::write(&sentinel, b"must remain untouched").unwrap();

        assert!(store.current_fail_closed().is_none());
        assert_eq!(fs::read(&sentinel).unwrap(), b"must remain untouched");

        fs::remove_file(&sentinel).unwrap();
        restore_pack_id_directory(&link, &external);
        store.recover_interrupted_work().unwrap();
        assert_eq!(current_descriptor(&store), Some(next));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn retained_lease_prevents_or_detects_ancestor_swap_before_launch() {
        let (root, workers, state, verifier, _) = store_fixture("leased-swap");
        let store = PackStore::new(&workers, &state, &verifier);
        let installed = store.stage_and_install(&root.join("source")).unwrap();
        store.activate(&installed).unwrap();
        let lease = store.current_fail_closed().unwrap();
        let pack_id = workers.join("packs").join(installed.pack_id.as_str());
        let moved = root.join("moved-pack-id");

        #[cfg(windows)]
        {
            assert!(fs::rename(&pack_id, &moved).is_err());
            verifier.launchable_worker(&lease).unwrap();
        }
        #[cfg(unix)]
        {
            if !rename_open_directory_or_reject(&pack_id, &moved) {
                verifier.launchable_worker(&lease).unwrap();
                drop(lease);
                fs::remove_dir_all(root).unwrap();
                return;
            }
            let decoy = root.join("decoy-pack-id");
            fs::create_dir(&decoy).unwrap();
            let sentinel = decoy.join("sentinel.txt");
            fs::write(&sentinel, b"must remain untouched").unwrap();
            std::os::unix::fs::symlink(&decoy, &pack_id).unwrap();
            assert!(matches!(
                verifier.launchable_worker(&lease),
                Err(PackVerificationError::PackStoreAncestorChanged(_))
            ));
            assert_eq!(fs::read(&sentinel).unwrap(), b"must remain untouched");
            fs::remove_file(&pack_id).unwrap();
            fs::rename(&moved, &pack_id).unwrap();
        }
        drop(lease);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn activation_cannot_overflow_the_bounded_epoch_map() {
        let (root, workers, state, verifier, _) = store_fixture("bounded-epochs");
        let store = PackStore::new(&workers, &state, &verifier);
        let installed = store.stage_and_install(&root.join("source")).unwrap();
        let mut epochs = EpochState::empty();
        for index in 0..256 {
            epochs.epochs.insert(format!("other-pack-{index}"), 1);
        }
        store.persist_epochs(&epochs).unwrap();
        assert!(matches!(
            store.activate(&installed),
            Err(PackStoreError::CorruptState("security epoch state"))
        ));
        assert_eq!(store.load_epochs_strict().unwrap(), epochs);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn anchored_stage_rejects_or_outlives_version_ancestor_swap() {
        let (root, workers, state, verifier, _) = store_fixture("stage-ancestor-swap");
        let store = PackStore::new(&workers, &state, &verifier);
        let external = root.join("external-stage-target");
        fs::create_dir(&external).unwrap();
        let sentinel = external.join("sentinel.txt");
        fs::write(&sentinel, b"must remain untouched").unwrap();
        let moved = root.join("moved-version");
        #[cfg(unix)]
        let swapped = Cell::new(false);

        let result = store.stage_and_install_inner(
            &root.join("source"),
            |_| {},
            |version| {
                #[cfg(windows)]
                assert!(fs::rename(version, &moved).is_err());
                #[cfg(unix)]
                {
                    if rename_open_directory_or_reject(version, &moved) {
                        std::os::unix::fs::symlink(&external, version).unwrap();
                        swapped.set(true);
                    }
                }
            },
            |_| {},
        );

        #[cfg(windows)]
        assert!(result.is_ok());
        #[cfg(unix)]
        assert_eq!(result.is_err(), swapped.get());
        assert_eq!(fs::read(&sentinel).unwrap(), b"must remain untouched");
        assert_eq!(fs::read_dir(&external).unwrap().count(), 1);
        #[cfg(unix)]
        if swapped.get() {
            let version = workers.join("packs").join("scribe-vulkan").join("1.2.3");
            fs::remove_file(version).unwrap();
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn anchored_staging_swap_never_redirects_copy_or_cleanup() {
        let (root, workers, state, verifier, _) = store_fixture("staging-root-swap");
        let store = PackStore::new(&workers, &state, &verifier);
        let external = root.join("external-staging-target");
        fs::create_dir(&external).unwrap();
        let sentinel = external.join("sentinel.txt");
        fs::write(&sentinel, b"must remain untouched").unwrap();
        let moved = root.join("moved-staging");
        #[cfg(unix)]
        let swapped = Cell::new(false);

        let result = store.stage_and_install_inner(
            &root.join("source"),
            |_| {},
            |_| {},
            |staging| {
                #[cfg(windows)]
                assert!(fs::rename(staging, &moved).is_err());
                #[cfg(unix)]
                {
                    if rename_open_directory_or_reject(staging, &moved) {
                        std::os::unix::fs::symlink(&external, staging).unwrap();
                        swapped.set(true);
                    }
                }
            },
        );

        #[cfg(windows)]
        assert!(result.is_ok());
        #[cfg(unix)]
        assert_eq!(result.is_err(), swapped.get());
        assert_eq!(fs::read(&sentinel).unwrap(), b"must remain untouched");
        assert_eq!(fs::read_dir(&external).unwrap().count(), 1);
        #[cfg(unix)]
        if swapped.get() {
            let version = workers.join("packs").join("scribe-vulkan").join("1.2.3");
            for entry in fs::read_dir(&version).unwrap() {
                let entry = entry.unwrap();
                if entry.file_type().unwrap().is_symlink() {
                    fs::remove_file(entry.path()).unwrap();
                }
            }
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn state_lock_and_mutation_share_one_anchored_parent() {
        let (root, workers, state, verifier, _) = store_fixture("state-ancestor-swap");
        let store = PackStore::new(&workers, &state, &verifier);
        let lock = store.acquire_lock().unwrap();
        let moved = root.join("moved-state");
        let second_lock_path = state.join(STORE_LOCK_NAME);
        let contention_started = std::time::Instant::now();
        assert!(matches!(
            exclusive_file_lock(&second_lock_path),
            Err(PackStoreError::LockContended)
        ));
        assert!(
            contention_started.elapsed() < std::time::Duration::from_secs(2),
            "state authority lock contention was not bounded"
        );
        #[cfg(unix)]
        let swapped = Cell::new(false);

        #[cfg(windows)]
        {
            assert!(fs::rename(&state, &moved).is_err());
            lock.write(&store.epoch_path(), &EpochState::empty())
                .unwrap();
            assert!(store.epoch_path().is_file());
        }
        #[cfg(unix)]
        {
            if rename_open_directory_or_reject(&state, &moved) {
                swapped.set(true);
                fs::create_dir(&state).unwrap();
                let sentinel = state.join("sentinel.txt");
                fs::write(&sentinel, b"must remain untouched").unwrap();
                assert!(
                    lock.write(&store.epoch_path(), &EpochState::empty())
                        .is_err()
                );
                assert_eq!(fs::read(&sentinel).unwrap(), b"must remain untouched");
                assert!(!state.join("security-epochs.json").exists());
            } else {
                lock.write(&store.epoch_path(), &EpochState::empty())
                    .unwrap();
                assert!(store.epoch_path().is_file());
            }
        }
        drop(lock);
        let second = exclusive_file_lock(&second_lock_path).unwrap();
        second
            .write(&state.join("second-instance.json"), &EpochState::empty())
            .unwrap();
        drop(second);
        #[cfg(unix)]
        if swapped.get() {
            assert_eq!(
                fs::read(state.join("sentinel.txt")).unwrap(),
                b"must remain untouched"
            );
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn preexisting_workers_root_ancestor_link_is_rejected_before_store_creation() {
        let (root, _workers, state, verifier, _) = store_fixture("prelinked-workers-root");
        let external = root.join("outside-workers");
        fs::create_dir(&external).unwrap();
        let sentinel = external.join("sentinel.txt");
        fs::write(&sentinel, b"outside remains unchanged").unwrap();
        let linked_ancestor = root.join("linked-workers-parent");
        create_directory_link(&linked_ancestor, &external);
        let store = PackStore::new(linked_ancestor.join("workers"), &state, &verifier);

        assert!(store.stage_and_install(&root.join("source")).is_err());
        assert_eq!(fs::read(&sentinel).unwrap(), b"outside remains unchanged");
        assert_eq!(fs::read_dir(&external).unwrap().count(), 1);

        remove_directory_link(&linked_ancestor);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn preexisting_state_parent_link_is_rejected_before_lock_or_state_creation() {
        let (root, workers, _state, verifier, _) = store_fixture("prelinked-state-root");
        let external = root.join("outside-state");
        fs::create_dir(&external).unwrap();
        let sentinel = external.join("sentinel.txt");
        fs::write(&sentinel, b"outside remains unchanged").unwrap();
        let linked_parent = root.join("linked-state-parent");
        create_directory_link(&linked_parent, &external);
        let store = PackStore::new(&workers, linked_parent.join("private-state"), &verifier);

        assert!(store.stage_and_install(&root.join("source")).is_err());
        assert_eq!(fs::read(&sentinel).unwrap(), b"outside remains unchanged");
        assert_eq!(fs::read_dir(&external).unwrap().count(), 1);

        remove_directory_link(&linked_parent);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn unexpected_large_source_file_after_verification_is_rejected_before_staging() {
        let (root, workers, state, verifier, _) = store_fixture("mutable-source-large-file");
        let store = PackStore::new(&workers, &state, &verifier);
        let source = root.join("source");
        let result = store.stage_and_install_inner(
            &source,
            |source| {
                let unexpected = OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(source.join("unexpected-large.bin"))
                    .unwrap();
                unexpected.set_len(128 * 1024 * 1024).unwrap();
            },
            |_| {},
            |_| {},
        );

        assert!(result.is_err());
        assert!(!workers.join("packs").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn source_root_swap_after_verification_never_redirects_copy() {
        let (root, workers, state, verifier, _) = store_fixture("mutable-source-swap");
        let store = PackStore::new(&workers, &state, &verifier);
        let source = root.join("source");
        let moved = root.join("moved-source");
        let external = root.join("outside-source");
        fs::create_dir(&external).unwrap();
        let sentinel = external.join("sentinel.txt");
        fs::write(&sentinel, b"outside remains unchanged").unwrap();
        #[cfg(unix)]
        let swapped = Cell::new(false);
        let result = store.stage_and_install_inner(
            &source,
            |source| {
                #[cfg(windows)]
                assert!(fs::rename(source, &moved).is_err());
                #[cfg(unix)]
                {
                    if rename_open_directory_or_reject(source, &moved) {
                        std::os::unix::fs::symlink(&external, source).unwrap();
                        swapped.set(true);
                    }
                }
            },
            |_| {},
            |_| {},
        );

        #[cfg(windows)]
        assert!(result.is_ok());
        #[cfg(unix)]
        assert_eq!(result.is_err(), swapped.get());
        assert_eq!(fs::read(&sentinel).unwrap(), b"outside remains unchanged");
        assert_eq!(fs::read_dir(&external).unwrap().count(), 1);
        #[cfg(unix)]
        if swapped.get() {
            fs::remove_file(&source).unwrap();
            fs::rename(&moved, &source).unwrap();
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn preexisting_source_ancestor_link_is_rejected_before_pack_reads() {
        let (root, workers, state, verifier, _) = store_fixture("prelinked-source-root");
        let external = root.join("outside-source-parent");
        fs::create_dir(&external).unwrap();
        let external_pack = external.join("pack");
        fs::rename(root.join("source"), &external_pack).unwrap();
        let sentinel = external.join("sentinel.txt");
        fs::write(&sentinel, b"outside remains unchanged").unwrap();
        let linked_parent = root.join("linked-source-parent");
        create_directory_link(&linked_parent, &external);
        let store = PackStore::new(&workers, &state, &verifier);

        assert!(
            store
                .stage_and_install(&linked_parent.join("pack"))
                .is_err()
        );
        assert_eq!(fs::read(&sentinel).unwrap(), b"outside remains unchanged");
        assert!(!workers.join("packs").exists());

        remove_directory_link(&linked_parent);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn concurrent_activation_epoch_and_pending_growth_fail_state_bounds() {
        for (label, state_name) in [
            ("grow-activation-state", "activation.json"),
            ("grow-epoch-state", "security-epochs.json"),
            ("grow-pending-state", "pending-activation.json"),
        ] {
            let (root, workers, state, verifier, _) = store_fixture(label);
            let store = PackStore::new(&workers, &state, &verifier);
            let installed = store.stage_and_install(&root.join("source")).unwrap();
            if state_name == "pending-activation.json" {
                assert!(matches!(
                    store.activate_locked(&installed, Some(ActivationBoundary::Journal)),
                    Err(PackStoreError::InjectedInterruption)
                ));
            } else {
                store.activate(&installed).unwrap();
            }
            let writer_start = Arc::new(std::sync::Barrier::new(2));
            let writer_finished = Arc::new(std::sync::Barrier::new(2));
            let writer_start_thread = Arc::clone(&writer_start);
            let writer_finished_thread = Arc::clone(&writer_finished);
            let state_path = state.join(state_name);
            let writer = std::thread::spawn(move || {
                writer_start_thread.wait();
                OpenOptions::new()
                    .write(true)
                    .open(state_path)
                    .unwrap()
                    .set_len(MAX_STATE_BYTES + 1)
                    .unwrap();
                writer_finished_thread.wait();
            });
            let expected_name = state_name.to_owned();
            let hook_start = Arc::clone(&writer_start);
            let hook_finished = Arc::clone(&writer_finished);
            set_state_read_hook(move |path| {
                assert_eq!(
                    path.file_name().and_then(|name| name.to_str()),
                    Some(expected_name.as_str())
                );
                hook_start.wait();
                hook_finished.wait();
            });
            let result = match state_name {
                "activation.json" => store.load_activation_strict().map(|_| ()),
                "security-epochs.json" => store.load_epochs_strict().map(|_| ()),
                "pending-activation.json" => store.recover_pending_activation_locked(),
                _ => unreachable!(),
            };
            writer.join().unwrap();
            assert!(matches!(
                result,
                Err(PackStoreError::CorruptState("state file bounds"))
            ));
            assert!(store.current_fail_closed().is_none());
            fs::remove_dir_all(root).unwrap();
        }
    }
}
