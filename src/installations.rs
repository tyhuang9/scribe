//! Manifest-driven download and activation primitives.
//!
//! This module owns artifact bytes and filesystem transactions only. Runtime
//! selection and smoke testing stay behind `TranscriptionService` and
//! `RuntimeRouter`.

use std::collections::{HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use url::Url;

use crate::disk_space::{self, CanonicalTargetIdentity, DiskSpacePreflight};

const BUFFER_BYTES: usize = 64 * 1024;
const PROGRESS_INTERVAL: Duration = Duration::from_millis(200);
const MAX_DOWNLOAD_REDIRECTS: usize = 5;
const MAX_REMOVAL_DISCOVERY_DEPTH: usize = 12;
const MAX_REMOVAL_DISCOVERY_ENTRIES: usize = 8_192;

#[cfg(test)]
static FILE_HASH_COUNTS: std::sync::OnceLock<std::sync::Mutex<HashMap<PathBuf, usize>>> =
    std::sync::OnceLock::new();
#[cfg(test)]
static FILE_HASH_CANCELLATIONS: std::sync::OnceLock<
    std::sync::Mutex<HashMap<PathBuf, (u64, InstallCancellation)>>,
> = std::sync::OnceLock::new();

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum InstallStage {
    Downloading,
    Verifying,
    Extracting,
    HealthChecking,
    Activating,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DownloadFaultKind {
    TimedOut,
    Stalled,
    PrematureEof,
    ConnectionReset,
    ConnectionFailed,
    Dns,
    HttpStatus(u16),
}

impl std::fmt::Display for DownloadFaultKind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TimedOut => formatter.write_str("network timeout"),
            Self::Stalled => formatter.write_str("download stalled"),
            Self::PrematureEof => formatter.write_str("response ended early"),
            Self::ConnectionReset => formatter.write_str("connection interrupted"),
            Self::ConnectionFailed => formatter.write_str("connection failed"),
            Self::Dns => formatter.write_str("DNS lookup failed"),
            Self::HttpStatus(status) => write!(formatter, "HTTP {status}"),
        }
    }
}

impl DownloadFaultKind {
    pub(crate) fn diagnostic_label(self) -> &'static str {
        match self {
            Self::TimedOut => "network_timeout",
            Self::Stalled => "download_stalled",
            Self::PrematureEof => "premature_eof",
            Self::ConnectionReset => "connection_interrupted",
            Self::ConnectionFailed => "connection_failed",
            Self::Dns => "dns_lookup_failed",
            Self::HttpStatus(408) => "http_408",
            Self::HttpStatus(429) => "http_429",
            Self::HttpStatus(500) => "http_500",
            Self::HttpStatus(502) => "http_502",
            Self::HttpStatus(503) => "http_503",
            Self::HttpStatus(504) => "http_504",
            Self::HttpStatus(_) => "http_retryable_status",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DownloadActivity {
    Active,
    Stalled,
    RetryScheduled {
        attempt: u8,
        max_attempts: u8,
        delay_ms: u64,
        cause: DownloadFaultKind,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct DownloadRetryPolicy {
    read_poll_interval: Duration,
    stalled_after: Duration,
    reconnect_after: Duration,
    retry_backoffs: [Duration; 3],
    cancellation_poll_interval: Duration,
}

impl DownloadRetryPolicy {
    const STANDARD: Self = Self {
        read_poll_interval: Duration::from_millis(750),
        stalled_after: Duration::from_secs(3),
        reconnect_after: Duration::from_secs(15),
        retry_backoffs: [
            Duration::from_secs(1),
            Duration::from_secs(2),
            Duration::from_secs(4),
        ],
        cancellation_poll_interval: Duration::from_millis(100),
    };

    fn max_retries(self) -> u8 {
        self.retry_backoffs.len() as u8
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct InstallProgress {
    pub(crate) stage: InstallStage,
    pub(crate) completed_bytes: u64,
    pub(crate) total_bytes: u64,
    pub(crate) bytes_per_second: Option<u64>,
    pub(crate) download_activity: Option<DownloadActivity>,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct InstallCancellation {
    state: Arc<AtomicU8>,
}

const INSTALL_ACTIVE: u8 = 0;
const INSTALL_CANCELLED: u8 = 1;
const INSTALL_ACTIVATION_COMMITTED: u8 = 2;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ActivationCommitError {
    Cancelled,
    AlreadyCommitted,
}

/// Immutable facts observed while hashing a user-selected local artifact.
/// The canonical path is retained so callers can reject a source that changes
/// identity before it becomes an imported model record.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct FileFingerprint {
    pub(crate) canonical_path: PathBuf,
    pub(crate) size_bytes: u64,
    pub(crate) sha256: String,
}

impl InstallCancellation {
    pub(crate) fn cancel(&self) {
        let _ = self.state.compare_exchange(
            INSTALL_ACTIVE,
            INSTALL_CANCELLED,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        self.state.load(Ordering::Acquire) == INSTALL_CANCELLED
    }

    pub(crate) fn same_handle(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.state, &other.state)
    }

    /// Linearization point between cancellation and the first activation
    /// mutation. Whichever transition wins is terminal for this install.
    pub(crate) fn try_commit_activation(&self) -> Result<(), ActivationCommitError> {
        match self.state.compare_exchange(
            INSTALL_ACTIVE,
            INSTALL_ACTIVATION_COMMITTED,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => Ok(()),
            Err(INSTALL_CANCELLED) => Err(ActivationCommitError::Cancelled),
            Err(INSTALL_ACTIVATION_COMMITTED) => Err(ActivationCommitError::AlreadyCommitted),
            Err(_) => unreachable!("install cancellation state is invalid"),
        }
    }

    #[cfg(test)]
    pub(crate) fn activation_is_committed(&self) -> bool {
        self.state.load(Ordering::Acquire) == INSTALL_ACTIVATION_COMMITTED
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum InstallError {
    #[error("installation cancelled; resumable partial retained at {partial_path}")]
    Cancelled {
        partial_path: PathBuf,
        downloaded_bytes: u64,
    },
    #[error(
        "download failed after {retry_count} ranged retries ({category}); {retained_bytes} bytes retained"
    )]
    DownloadFailed {
        category: DownloadFaultKind,
        retry_count: u8,
        retained_bytes: u64,
        partial_path: PathBuf,
    },
    #[error("{0}")]
    Failed(String),
    #[error("recovery required: {0}")]
    RecoveryRequired(String),
}

impl InstallError {
    #[cfg(test)]
    pub(crate) fn is_cancelled(&self) -> bool {
        matches!(self, Self::Cancelled { .. })
    }

    pub(crate) fn requires_recovery(&self) -> bool {
        matches!(self, Self::RecoveryRequired(_))
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PinnedArtifact {
    pub(crate) id: String,
    pub(crate) url: String,
    pub(crate) size_bytes: u64,
    pub(crate) sha256: String,
    pub(crate) destination: PathBuf,
}

#[derive(Debug)]
pub(crate) struct PinnedArtifactInspectionPlan {
    artifact: PinnedArtifact,
    target_identity: CanonicalTargetIdentity,
    state: PinnedArtifactInspectionState,
}

#[derive(Debug)]
enum PinnedArtifactInspectionState {
    VerifiedDestination(OpenedFileIdentity),
    RequiresDownload(u64),
}

impl PinnedArtifactInspectionPlan {
    pub(crate) fn required_download_bytes(&self) -> u64 {
        match self.state {
            PinnedArtifactInspectionState::VerifiedDestination(_) => 0,
            PinnedArtifactInspectionState::RequiresDownload(bytes) => bytes,
        }
    }
}

#[cfg(unix)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct OpenedFileIdentity {
    device: u64,
    inode: u64,
}

#[cfg(windows)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct OpenedFileIdentity {
    volume_serial: u32,
    file_index_high: u32,
    file_index_low: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DownloadedArtifact {
    pub(crate) id: String,
    pub(crate) path: PathBuf,
    pub(crate) destination: PathBuf,
    pub(crate) size_bytes: u64,
    pub(crate) sha256: String,
    pub(crate) target_identity: CanonicalTargetIdentity,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RetainedPartial {
    pub(crate) bytes: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RuntimeFileSpec {
    pub(crate) archive_path: PathBuf,
    pub(crate) install_path: PathBuf,
    pub(crate) size_bytes: u64,
    pub(crate) sha256: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RuntimeArchiveSpec {
    pub(crate) package_id: String,
    pub(crate) artifact: PinnedArtifact,
    pub(crate) manifest_json: String,
    pub(crate) files: Vec<RuntimeFileSpec>,
}

/// One already downloaded, exact file copied into a freshly assembled bundle.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct BundleAssemblyFile {
    pub(crate) source_path: PathBuf,
    pub(crate) install_path: PathBuf,
    pub(crate) size_bytes: u64,
    pub(crate) sha256: String,
}

/// One deterministic metadata file generated while assembling a bundle.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct GeneratedBundleFile {
    pub(crate) install_path: PathBuf,
    pub(crate) bytes: Vec<u8>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ActivationPhase {
    Prepared,
    RuntimeActivated,
    ModelActivated,
    ConfigPersisted,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct ActivationJournalDocument {
    schema_version: u16,
    phase: ActivationPhase,
    model_target: PathBuf,
    model_had_previous: bool,
    #[serde(default)]
    manifest_target: Option<PathBuf>,
    #[serde(default)]
    manifest_had_previous: bool,
    runtime_target: Option<PathBuf>,
    runtime_had_previous: bool,
    #[serde(default)]
    retain_runtime_as_previous: bool,
    #[serde(default)]
    prior_config_fingerprint: Option<String>,
    #[serde(default)]
    expected_config_fingerprint: Option<String>,
}

#[derive(Debug)]
pub(crate) struct ActivationJournal {
    path: PathBuf,
    document: ActivationJournalDocument,
}

impl ActivationJournal {
    pub(crate) fn begin(
        path: PathBuf,
        model_target: PathBuf,
        runtime_target: Option<PathBuf>,
        retain_runtime_as_previous: bool,
        prior_config_fingerprint: String,
    ) -> Result<Self, InstallError> {
        if path.exists() {
            return Err(InstallError::RecoveryRequired(format!(
                "refusing to overwrite unresolved activation journal {}",
                path.display()
            )));
        }
        validate_sha256(&prior_config_fingerprint)?;
        let journal = Self {
            path,
            document: ActivationJournalDocument {
                schema_version: 2,
                phase: ActivationPhase::Prepared,
                model_had_previous: model_target.exists(),
                manifest_target: None,
                manifest_had_previous: false,
                runtime_had_previous: runtime_target.as_ref().is_some_and(|path| path.exists()),
                model_target,
                runtime_target,
                retain_runtime_as_previous,
                prior_config_fingerprint: Some(prior_config_fingerprint),
                expected_config_fingerprint: None,
            },
        };
        journal.persist()?;
        Ok(journal)
    }

    pub(crate) fn mark(&mut self, phase: ActivationPhase) -> Result<(), InstallError> {
        let legal = matches!(
            (self.document.phase, phase),
            (ActivationPhase::Prepared, ActivationPhase::RuntimeActivated)
                | (ActivationPhase::Prepared, ActivationPhase::ModelActivated)
                | (
                    ActivationPhase::RuntimeActivated,
                    ActivationPhase::ModelActivated
                )
                | (
                    ActivationPhase::ModelActivated,
                    ActivationPhase::ConfigPersisted
                )
        );
        if !legal {
            return Err(failed(format!(
                "illegal activation journal transition from {:?} to {phase:?}",
                self.document.phase
            )));
        }
        if phase == ActivationPhase::ConfigPersisted
            && self.document.expected_config_fingerprint.is_none()
        {
            return Err(failed(
                "settings cannot be marked persisted without a prepared fingerprint",
            ));
        }
        self.document.phase = phase;
        self.persist()
    }

    pub(crate) fn prepare_config_commit(
        &mut self,
        expected_config_fingerprint: String,
    ) -> Result<(), InstallError> {
        if self.document.phase != ActivationPhase::ModelActivated {
            return Err(failed(
                "settings commit can be prepared only after model activation",
            ));
        }
        validate_sha256(&expected_config_fingerprint)?;
        self.document.expected_config_fingerprint = Some(expected_config_fingerprint);
        self.persist()
    }

    /// Associates an auxiliary, model-scoped file replacement with this
    /// transaction before it is activated. The journal can then either commit
    /// or restore it after an interrupted settings write.
    pub(crate) fn record_manifest_target(&mut self, target: PathBuf) -> Result<(), InstallError> {
        if self.document.phase != ActivationPhase::ModelActivated {
            return Err(failed(
                "an installed-model manifest can be recorded only after model activation",
            ));
        }
        if self.document.manifest_target.is_some() {
            return Err(failed(
                "an installed-model manifest is already associated with this activation journal",
            ));
        }
        self.document.manifest_had_previous = target.exists();
        self.document.manifest_target = Some(target);
        self.persist()
    }

    pub(crate) fn clear(self) -> Result<(), InstallError> {
        remove_path_if_exists(&self.path)
    }

    fn persist(&self) -> Result<(), InstallError> {
        let bytes = serde_json::to_vec_pretty(&self.document)
            .map_err(|error| failed(format!("failed to serialize activation journal: {error}")))?;
        crate::config::settings::atomic_write_bytes(&self.path, &bytes)
            .map_err(|error| failed(format!("failed to persist activation journal: {error:#}")))
    }
}

pub(crate) fn reconcile_activation_journal(
    path: &Path,
    allowed_model_targets: &[PathBuf],
    allowed_manifest_targets: &[PathBuf],
    allowed_runtime_targets: &[PathBuf],
    durable_config_fingerprint: Option<&str>,
) -> Result<bool, InstallError> {
    if !path.exists() {
        return Ok(false);
    }
    let bytes = fs::read(path)
        .map_err(|error| failed(format!("failed to read {}: {error}", path.display())))?;
    let document: ActivationJournalDocument = serde_json::from_slice(&bytes).map_err(|error| {
        failed(format!(
            "invalid activation journal {}: {error}",
            path.display()
        ))
    })?;
    if document.schema_version != 2 {
        return Err(failed("unsupported activation journal schema"));
    }
    if let Some(fingerprint) = document.prior_config_fingerprint.as_deref() {
        validate_sha256(fingerprint)?;
    }
    if let Some(fingerprint) = document.expected_config_fingerprint.as_deref() {
        validate_sha256(fingerprint)?;
    }
    validate_reconciliation_target(&document.model_target, allowed_model_targets, "model")?;
    if let Some(manifest_target) = document.manifest_target.as_ref() {
        validate_reconciliation_target(
            manifest_target,
            allowed_manifest_targets,
            "installed-model manifest",
        )?;
    }
    if let Some(runtime_target) = document.runtime_target.as_ref() {
        validate_reconciliation_target(runtime_target, allowed_runtime_targets, "runtime")?;
    }
    let prior_config_is_durable = document
        .prior_config_fingerprint
        .as_deref()
        .zip(durable_config_fingerprint)
        .is_some_and(|(prior, actual)| prior.eq_ignore_ascii_case(actual));
    let new_config_is_durable = document
        .expected_config_fingerprint
        .as_deref()
        .zip(durable_config_fingerprint)
        .is_some_and(|(expected, actual)| expected.eq_ignore_ascii_case(actual));
    if new_config_is_durable {
        finalize_file_replacement(&document.model_target)?;
        if let Some(manifest) = document.manifest_target.as_ref() {
            finalize_file_replacement(manifest)?;
        }
        if let Some(runtime) = document.runtime_target.as_ref() {
            finalize_directory_replacement(runtime, document.retain_runtime_as_previous)?;
        }
    } else if prior_config_is_durable {
        if let Some(manifest) = document.manifest_target.as_ref() {
            restore_file_replacement(manifest, document.manifest_had_previous)?;
        }
        restore_file_replacement(&document.model_target, document.model_had_previous)?;
        if let Some(runtime) = document.runtime_target.as_ref() {
            restore_directory_replacement(runtime, document.runtime_had_previous)?;
        }
    } else {
        return Err(InstallError::RecoveryRequired(format!(
            "durable artifact settings match neither the pre-install nor expected post-install fingerprint for {}; refusing to mutate artifacts",
            path.display()
        )));
    }
    remove_path_if_exists(path)?;
    Ok(true)
}

fn validate_reconciliation_target(
    target: &Path,
    allowed: &[PathBuf],
    label: &str,
) -> Result<(), InstallError> {
    let target = canonicalize_missing(target)?;
    let permitted = allowed.iter().any(|candidate| {
        canonicalize_missing(candidate)
            .map(|candidate| candidate == target)
            .unwrap_or(false)
    });
    if permitted {
        Ok(())
    } else {
        Err(failed(format!(
            "activation journal {label} target is outside the catalog-managed roots: {}",
            target.display()
        )))
    }
}

#[derive(Debug)]
pub(crate) struct StagedRuntime {
    pub(crate) root: PathBuf,
    pub(crate) target_root: PathBuf,
}

impl Drop for StagedRuntime {
    fn drop(&mut self) {
        let _ = remove_path_if_exists(&self.root);
    }
}

impl StagedRuntime {
    pub(crate) fn activate(mut self) -> Result<DirectoryReplacement, InstallError> {
        let replacement = activate_directory(&self.root, &self.target_root)?;
        self.root = PathBuf::new();
        Ok(replacement)
    }
}

#[derive(Debug)]
pub(crate) struct FileReplacement {
    destination: PathBuf,
    state: FileReplacementState,
}

#[derive(Debug)]
enum FileReplacementState {
    Unchanged,
    Created,
    Replaced { rollback_path: PathBuf },
}

impl FileReplacement {
    pub(crate) fn destination(&self) -> &Path {
        &self.destination
    }

    pub(crate) fn commit(self) -> Result<(), InstallError> {
        if let FileReplacementState::Replaced { rollback_path } = self.state {
            remove_path_if_exists(&rollback_path)?;
        }
        Ok(())
    }

    pub(crate) fn rollback(self) -> Result<(), InstallError> {
        match self.state {
            FileReplacementState::Unchanged => return Ok(()),
            FileReplacementState::Created => {
                remove_path_if_exists(&self.destination)?;
            }
            FileReplacementState::Replaced { rollback_path } => {
                remove_path_if_exists(&self.destination)?;
                durable_rename(&rollback_path, &self.destination).map_err(|error| {
                    failed(format!(
                        "failed to restore previous model {}: {error}",
                        self.destination.display()
                    ))
                })?;
            }
        }
        sync_parent(&self.destination).map_err(|error| {
            InstallError::RecoveryRequired(format!(
                "rolled back model {}, but the change was not made durable: {error}",
                self.destination.display()
            ))
        })?;
        Ok(())
    }
}

#[derive(Debug)]
pub(crate) struct DirectoryReplacement {
    target_root: PathBuf,
    rollback_path: Option<PathBuf>,
}

/// Recoverable removal of one exact catalog-managed artifact. The stable
/// tombstone survives a crash so startup can either restore it when settings
/// still reference the artifact or finish deletion after settings commit.
#[derive(Debug)]
pub(crate) struct ManagedRemoval {
    target: PathBuf,
    tombstone: Option<PathBuf>,
    journal_path: Option<PathBuf>,
    journal: Option<RemovalJournalDocument>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct RemovalJournalDocument {
    schema_version: u16,
    target: PathBuf,
    prior_config_fingerprint: String,
    expected_config_fingerprint: Option<String>,
}

impl ManagedRemoval {
    /// Stages deletion of one exact regular file without following links or
    /// crossing a reparse-point ancestor. This is used for installer-owned
    /// executable siblings, whose removal authority must never be inferred
    /// from a user-configured model path.
    pub(crate) fn stage_exact_regular_file(
        target: &Path,
        allowed_target: &Path,
        prior_config_fingerprint: String,
    ) -> Result<Self, InstallError> {
        let allowed_target = allowed_target.to_path_buf();
        validate_reconciliation_target(
            target,
            std::slice::from_ref(&allowed_target),
            "regular-file removal",
        )?;
        reject_link_or_reparse_ancestors(target)?;
        let metadata = fs::symlink_metadata(target).map_err(|error| {
            failed(format!(
                "failed to inspect exact removal target {}: {error}",
                target.display()
            ))
        })?;
        if !metadata.file_type().is_file() || runtime_metadata_is_link_or_reparse(&metadata) {
            return Err(failed(format!(
                "exact removal target is not a regular non-link file: {}",
                target.display()
            )));
        }
        Self::stage(
            target,
            std::slice::from_ref(&allowed_target),
            prior_config_fingerprint,
        )
    }

    pub(crate) fn stage(
        target: &Path,
        allowed_targets: &[PathBuf],
        prior_config_fingerprint: String,
    ) -> Result<Self, InstallError> {
        validate_reconciliation_target(target, allowed_targets, "removal")?;
        validate_sha256(&prior_config_fingerprint)?;
        let tombstone = removal_tombstone_path(target)?;
        let journal_path = removal_journal_path(target)?;
        if tombstone.exists() || journal_path.exists() {
            return Err(InstallError::RecoveryRequired(format!(
                "unresolved artifact removal exists at {} or {}",
                tombstone.display(),
                journal_path.display()
            )));
        }
        if !target.exists() {
            return Ok(Self {
                target: target.to_path_buf(),
                tombstone: None,
                journal_path: None,
                journal: None,
            });
        }
        let journal = RemovalJournalDocument {
            schema_version: 1,
            target: target.to_path_buf(),
            prior_config_fingerprint,
            expected_config_fingerprint: None,
        };
        persist_removal_journal(&journal_path, &journal)?;
        durable_rename(target, &tombstone).map_err(|error| {
            let clear_error = remove_path_if_exists(&journal_path).err();
            if let Some(clear_error) = clear_error {
                InstallError::RecoveryRequired(format!(
                    "failed to stage artifact removal {}: {error}; the prepared removal journal also could not be cleared: {clear_error}",
                    target.display()
                ))
            } else {
                failed(format!(
                    "failed to stage artifact removal {}: {error}",
                    target.display()
                ))
            }
        })?;
        if let Err(error) = sync_parent(target) {
            if let Err(restore) = durable_rename(&tombstone, target) {
                return Err(InstallError::RecoveryRequired(format!(
                    "artifact removal staging was not durable: {error}; restoring {} also failed: {restore}",
                    target.display()
                )));
            }
            return Err(InstallError::RecoveryRequired(format!(
                "artifact removal staging for {} could not be confirmed durable even though the logical restore succeeded: {error}",
                target.display()
            )));
        }
        Ok(Self {
            target: target.to_path_buf(),
            tombstone: Some(tombstone),
            journal_path: Some(journal_path),
            journal: Some(journal),
        })
    }

    pub(crate) fn prepare_config_commit(
        &mut self,
        expected_config_fingerprint: String,
    ) -> Result<(), InstallError> {
        let (Some(journal_path), Some(journal)) =
            (self.journal_path.as_ref(), self.journal.as_mut())
        else {
            return Ok(());
        };
        validate_sha256(&expected_config_fingerprint)?;
        journal.expected_config_fingerprint = Some(expected_config_fingerprint);
        persist_removal_journal(journal_path, journal)
    }

    pub(crate) fn removed_files(&self) -> bool {
        self.tombstone.is_some()
    }

    pub(crate) fn commit(mut self) -> Result<(), InstallError> {
        if let Some(tombstone) = self.tombstone.take()
            && let Err(error) = remove_path_if_exists(&tombstone)
        {
            return Err(InstallError::RecoveryRequired(format!(
                "settings committed but artifact removal cleanup failed at {}: {error}",
                tombstone.display()
            )));
        }
        sync_parent(&self.target).map_err(|error| {
            InstallError::RecoveryRequired(format!(
                "artifact removal completed but directory sync failed for {}: {error}",
                self.target.display()
            ))
        })?;
        if let Some(journal_path) = self.journal_path.take() {
            remove_path_if_exists(&journal_path).map_err(|error| {
                InstallError::RecoveryRequired(format!(
                    "artifact removal committed, but its journal could not be cleared: {error}"
                ))
            })?;
        }
        Ok(())
    }

    pub(crate) fn rollback(mut self) -> Result<(), InstallError> {
        let Some(tombstone) = self.tombstone.take() else {
            return Ok(());
        };
        if self.target.exists() {
            return Err(InstallError::RecoveryRequired(format!(
                "cannot restore {} because a replacement already exists",
                self.target.display()
            )));
        }
        durable_rename(&tombstone, &self.target).map_err(|error| {
            InstallError::RecoveryRequired(format!(
                "failed to restore staged removal {}: {error}",
                self.target.display()
            ))
        })?;
        sync_parent(&self.target).map_err(|error| {
            InstallError::RecoveryRequired(format!(
                "restored staged removal {} but could not sync it: {error}",
                self.target.display()
            ))
        })?;
        if let Some(journal_path) = self.journal_path.take() {
            remove_path_if_exists(&journal_path).map_err(|error| {
                InstallError::RecoveryRequired(format!(
                    "artifact removal rolled back, but its journal could not be cleared: {error}"
                ))
            })?;
        }
        Ok(())
    }
}

/// Removes one exact regular file after the same no-link validation used by
/// transactional bundled-model deletion. This is safe for startup cleanup
/// because the persisted exclusion is already the source of truth: either
/// side of an interrupted remove continues to keep the artifact out of use.
pub(crate) fn remove_exact_regular_file(
    target: &Path,
    allowed_target: &Path,
) -> Result<bool, InstallError> {
    let allowed_target = allowed_target.to_path_buf();
    validate_reconciliation_target(
        target,
        std::slice::from_ref(&allowed_target),
        "regular-file cleanup",
    )?;
    reject_link_or_reparse_ancestors(target)?;
    let metadata = match fs::symlink_metadata(target) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(failed(format!(
                "failed to inspect exact cleanup target {}: {error}",
                target.display()
            )));
        }
    };
    if !metadata.file_type().is_file() || runtime_metadata_is_link_or_reparse(&metadata) {
        return Err(failed(format!(
            "exact cleanup target is not a regular non-link file: {}",
            target.display()
        )));
    }
    fs::remove_file(target).map_err(|error| {
        failed(format!(
            "failed to remove exact cleanup target {}: {error}",
            target.display()
        ))
    })?;
    sync_parent(target).map_err(|error| {
        InstallError::RecoveryRequired(format!(
            "removed {}, but its parent directory could not be made durable: {error}",
            target.display()
        ))
    })?;
    Ok(true)
}

pub(crate) fn reconcile_managed_removal(
    target: &Path,
    allowed_targets: &[PathBuf],
    durable_config_fingerprint: &str,
) -> Result<bool, InstallError> {
    validate_reconciliation_target(target, allowed_targets, "removal")?;
    validate_sha256(durable_config_fingerprint)?;
    let tombstone = removal_tombstone_path(target)?;
    let journal_path = removal_journal_path(target)?;
    if !tombstone.exists() && !journal_path.exists() {
        return Ok(false);
    }
    if !journal_path.exists() {
        return Err(InstallError::RecoveryRequired(format!(
            "artifact removal tombstone {} has no durable settings witness",
            tombstone.display()
        )));
    }
    let bytes = fs::read(&journal_path).map_err(|error| {
        failed(format!(
            "failed to read removal journal {}: {error}",
            journal_path.display()
        ))
    })?;
    let journal: RemovalJournalDocument = serde_json::from_slice(&bytes).map_err(|error| {
        failed(format!(
            "invalid removal journal {}: {error}",
            journal_path.display()
        ))
    })?;
    if journal.schema_version != 1
        || canonicalize_missing(&journal.target)? != canonicalize_missing(target)?
    {
        return Err(InstallError::RecoveryRequired(format!(
            "removal journal {} does not describe the exact catalog target {}",
            journal_path.display(),
            target.display()
        )));
    }
    validate_sha256(&journal.prior_config_fingerprint)?;
    if let Some(expected) = journal.expected_config_fingerprint.as_deref() {
        validate_sha256(expected)?;
    }
    let durable_is_prior = journal
        .prior_config_fingerprint
        .eq_ignore_ascii_case(durable_config_fingerprint);
    let durable_is_expected = journal
        .expected_config_fingerprint
        .as_deref()
        .is_some_and(|expected| expected.eq_ignore_ascii_case(durable_config_fingerprint));
    if !durable_is_prior && !durable_is_expected {
        return Err(InstallError::RecoveryRequired(format!(
            "durable artifact settings match neither side of removal transaction {}; refusing to mutate artifacts",
            target.display()
        )));
    }
    if durable_is_prior {
        if !tombstone.exists() && target.exists() {
            remove_path_if_exists(&journal_path)?;
            return Ok(true);
        }
        if target.exists() {
            return Err(InstallError::RecoveryRequired(format!(
                "both active artifact and removal tombstone exist for {}",
                target.display()
            )));
        }
        durable_rename(&tombstone, target).map_err(|error| {
            InstallError::RecoveryRequired(format!(
                "failed to restore interrupted artifact removal {}: {error}",
                target.display()
            ))
        })?;
    } else {
        if target.exists() {
            return Err(InstallError::RecoveryRequired(format!(
                "durable settings removed {}, but the active artifact still exists",
                target.display()
            )));
        }
        remove_path_if_exists(&tombstone)?;
    }
    sync_parent(target).map_err(|error| {
        InstallError::RecoveryRequired(format!(
            "artifact removal reconciliation changed {} but directory sync failed: {error}",
            target.display()
        ))
    })?;
    remove_path_if_exists(&journal_path)?;
    Ok(true)
}

/// Finds durable removal journals below Scribe-owned storage roots so a
/// transaction remains recoverable after its model/runtime record has already
/// been removed from the persisted settings. Traversal is bounded and never
/// follows symbolic links or Windows reparse points.
pub(crate) fn discover_managed_removal_targets(
    roots: &[PathBuf],
) -> Result<Vec<PathBuf>, InstallError> {
    let mut discovered = Vec::new();
    let mut inspected_entries = 0_usize;

    for root in roots {
        if !root.exists() {
            continue;
        }
        let root_metadata = fs::symlink_metadata(root).map_err(|error| {
            InstallError::RecoveryRequired(format!(
                "could not inspect managed removal root {}: {error}",
                root.display()
            ))
        })?;
        if !root_metadata.is_dir() || runtime_metadata_is_link_or_reparse(&root_metadata) {
            return Err(InstallError::RecoveryRequired(format!(
                "managed removal root is not a regular directory or is a symbolic link/reparse point: {}",
                root.display()
            )));
        }
        let canonical_root = fs::canonicalize(root).map_err(|error| {
            InstallError::RecoveryRequired(format!(
                "could not canonicalize managed removal root {}: {error}",
                root.display()
            ))
        })?;
        let mut pending = vec![(canonical_root.clone(), 0_usize)];

        while let Some((directory, depth)) = pending.pop() {
            let entries = fs::read_dir(&directory).map_err(|error| {
                InstallError::RecoveryRequired(format!(
                    "could not enumerate managed removal directory {}: {error}",
                    directory.display()
                ))
            })?;
            for entry in entries {
                let entry = entry.map_err(|error| {
                    InstallError::RecoveryRequired(format!(
                        "could not enumerate a managed removal entry below {}: {error}",
                        directory.display()
                    ))
                })?;
                inspected_entries = inspected_entries.saturating_add(1);
                if inspected_entries > MAX_REMOVAL_DISCOVERY_ENTRIES {
                    return Err(InstallError::RecoveryRequired(format!(
                        "managed removal discovery exceeded {MAX_REMOVAL_DISCOVERY_ENTRIES} entries"
                    )));
                }
                let path = entry.path();
                let metadata = fs::symlink_metadata(&path).map_err(|error| {
                    InstallError::RecoveryRequired(format!(
                        "could not inspect managed removal entry {}: {error}",
                        path.display()
                    ))
                })?;
                if runtime_metadata_is_link_or_reparse(&metadata) {
                    return Err(InstallError::RecoveryRequired(format!(
                        "managed removal discovery encountered a symbolic link/reparse point: {}",
                        path.display()
                    )));
                }
                if metadata.is_dir() {
                    if depth >= MAX_REMOVAL_DISCOVERY_DEPTH {
                        return Err(InstallError::RecoveryRequired(format!(
                            "managed removal discovery exceeded depth {MAX_REMOVAL_DISCOVERY_DEPTH} at {}",
                            path.display()
                        )));
                    }
                    pending.push((path, depth + 1));
                    continue;
                }
                if !metadata.is_file()
                    || !path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .is_some_and(|name| name.ends_with(".removal-journal.json"))
                {
                    continue;
                }

                let bytes = fs::read(&path).map_err(|error| {
                    InstallError::RecoveryRequired(format!(
                        "could not read managed removal journal {}: {error}",
                        path.display()
                    ))
                })?;
                let journal: RemovalJournalDocument =
                    serde_json::from_slice(&bytes).map_err(|error| {
                        InstallError::RecoveryRequired(format!(
                            "invalid managed removal journal {}: {error}",
                            path.display()
                        ))
                    })?;
                if journal.schema_version != 1 {
                    return Err(InstallError::RecoveryRequired(format!(
                        "unsupported managed removal journal schema at {}",
                        path.display()
                    )));
                }
                let target = canonicalize_missing(&journal.target)?;
                if !target.starts_with(&canonical_root) || target == canonical_root {
                    return Err(InstallError::RecoveryRequired(format!(
                        "managed removal journal {} references a target outside its storage root",
                        path.display()
                    )));
                }
                let expected_journal =
                    canonicalize_missing(&removal_journal_path(&journal.target)?)?;
                let actual_journal = canonicalize_missing(&path)?;
                if expected_journal != actual_journal {
                    return Err(InstallError::RecoveryRequired(format!(
                        "managed removal journal path does not match its target: {}",
                        path.display()
                    )));
                }
                discovered.push(target);
            }
        }
    }

    discovered.sort();
    discovered.dedup();
    Ok(discovered)
}

fn persist_removal_journal(
    path: &Path,
    journal: &RemovalJournalDocument,
) -> Result<(), InstallError> {
    let bytes = serde_json::to_vec_pretty(journal)
        .map_err(|error| failed(format!("failed to serialize removal journal: {error}")))?;
    crate::config::settings::atomic_write_bytes(path, &bytes)
        .map_err(|error| failed(format!("failed to persist removal journal: {error:#}")))
}

impl DirectoryReplacement {
    pub(crate) fn previous_root(&self) -> PathBuf {
        previous_runtime_root(&self.target_root)
    }

    /// Retains exactly one previous known-good runtime after the new package
    /// and its settings record are durably committed.
    pub(crate) fn commit_with_previous_policy(
        mut self,
        retain_replaced_as_previous: bool,
    ) -> Result<(), InstallError> {
        if !retain_replaced_as_previous {
            if let Some(rollback) = self.rollback_path.take() {
                remove_path_if_exists(&rollback)?;
            }
            return Ok(());
        }
        let Some(rollback) = self.rollback_path.take() else {
            return Ok(());
        };
        let previous = self.previous_root();
        remove_path_if_exists(&previous)?;
        durable_rename(&rollback, &previous).map_err(|error| {
            failed(format!(
                "failed to retain previous runtime {}: {error}",
                previous.display()
            ))
        })?;
        sync_parent(&previous).map_err(|error| {
            InstallError::RecoveryRequired(format!(
                "retained previous runtime {}, but its directory entry was not made durable: {error}",
                previous.display()
            ))
        })?;
        Ok(())
    }

    pub(crate) fn rollback(mut self) -> Result<(), InstallError> {
        remove_path_if_exists(&self.target_root)?;
        if let Some(rollback) = self.rollback_path.take() {
            durable_rename(&rollback, &self.target_root).map_err(|error| {
                failed(format!(
                    "failed to restore previous runtime {}: {error}",
                    self.target_root.display()
                ))
            })?;
            sync_parent(&self.target_root).map_err(|error| {
                InstallError::RecoveryRequired(format!(
                    "restored runtime {}, but the rename was not made durable: {error}",
                    self.target_root.display()
                ))
            })?;
        }
        Ok(())
    }
}

#[cfg(test)]
pub(crate) fn download_pinned_artifact(
    artifact: &PinnedArtifact,
    cancellation: &InstallCancellation,
    progress: &dyn Fn(InstallProgress),
) -> Result<DownloadedArtifact, InstallError> {
    if cancellation.is_cancelled() {
        return Err(cancelled_before_artifact_inspection(artifact)?);
    }
    let expected_target_identity = disk_space::canonical_target_identity(&artifact.destination)
        .map_err(InstallError::Failed)?;
    download_pinned_artifact_for_target(
        artifact,
        &expected_target_identity,
        None,
        cancellation,
        progress,
    )
}

pub(crate) fn download_pinned_artifact_for_target(
    artifact: &PinnedArtifact,
    expected_target_identity: &CanonicalTargetIdentity,
    inspection: Option<PinnedArtifactInspectionPlan>,
    cancellation: &InstallCancellation,
    progress: &dyn Fn(InstallProgress),
) -> Result<DownloadedArtifact, InstallError> {
    if let Some(inspection) = inspection {
        validate_artifact_inspection(artifact, expected_target_identity, &inspection)?;
        match inspection.state {
            PinnedArtifactInspectionState::VerifiedDestination(expected_file_identity) => {
                if cancellation.is_cancelled() {
                    return Err(cancelled_before_artifact_inspection(artifact)?);
                }
                revalidate_artifact_target(&artifact.destination, expected_target_identity)?;
                let (file, metadata) = open_regular_file_no_follow(&artifact.destination)?;
                if metadata.len() != artifact.size_bytes
                    || opened_file_identity(&file)? != expected_file_identity
                {
                    return Err(failed(format!(
                        "verified artifact changed after inspection: {}",
                        artifact.destination.display()
                    )));
                }
                return Ok(downloaded_candidate(
                    artifact,
                    artifact.destination.clone(),
                    expected_target_identity,
                ));
            }
            PinnedArtifactInspectionState::RequiresDownload(_) => {
                return download_pinned_artifact_with_target(
                    &UreqHttpSource,
                    artifact,
                    expected_target_identity,
                    false,
                    cancellation,
                    progress,
                );
            }
        }
    }
    download_pinned_artifact_with_target(
        &UreqHttpSource,
        artifact,
        expected_target_identity,
        true,
        cancellation,
        progress,
    )
}

/// Reports the conservative free-space budget for a managed artifact without
/// creating directories, touching partials, or contacting the network.
pub(crate) fn pinned_artifact_disk_space_preflight(
    artifact: &PinnedArtifact,
) -> Result<DiskSpacePreflight, InstallError> {
    validate_artifact_spec(artifact)?;
    let partial = partial_path(&artifact.destination)?;
    let partial_bytes = partial_file_metadata(&partial)?.map_or(0, |metadata| metadata.len());
    let additional_bytes = additional_download_bytes(artifact.size_bytes, partial_bytes)?;
    disk_space::preflight_download_destination(&artifact.destination, additional_bytes)
        .map_err(InstallError::Failed)
}

/// Inspects retained resumable bytes for a validated artifact.
pub(crate) fn pinned_artifact_retained_partial(
    artifact: &PinnedArtifact,
) -> Result<Option<RetainedPartial>, InstallError> {
    validate_artifact_spec(artifact)?;
    let partial = partial_path(&artifact.destination)?;
    Ok(
        partial_file_metadata(&partial)?.map(|metadata| RetainedPartial {
            bytes: metadata.len(),
        }),
    )
}

/// Computes the bytes that a new request must reserve before touching the
/// network. Exact completed destinations need no download capacity. Only a
/// regular, strictly short partial is treated as resumable; full, corrupt, or
/// oversized partials conservatively reserve a complete replacement.
pub(crate) fn inspect_pinned_artifact_for_target(
    artifact: &PinnedArtifact,
    expected_target_identity: &CanonicalTargetIdentity,
    cancellation: &InstallCancellation,
) -> Result<PinnedArtifactInspectionPlan, InstallError> {
    if cancellation.is_cancelled() {
        return Err(cancelled_before_artifact_inspection(artifact)?);
    }
    validate_artifact_spec(artifact)?;
    revalidate_artifact_target(&artifact.destination, expected_target_identity)?;
    if artifact_destination_is_regular(&artifact.destination)? {
        let (mut file, metadata) = open_regular_file_no_follow(&artifact.destination)?;
        let identity = opened_file_identity(&file)?;
        match verify_opened_file_cancellable(
            &mut file,
            &metadata,
            &artifact.destination,
            artifact.size_bytes,
            &artifact.sha256,
            cancellation,
        ) {
            Ok(()) => {
                return Ok(PinnedArtifactInspectionPlan {
                    artifact: artifact.clone(),
                    target_identity: expected_target_identity.clone(),
                    state: PinnedArtifactInspectionState::VerifiedDestination(identity),
                });
            }
            Err(error @ InstallError::Cancelled { .. }) => return Err(error),
            Err(_) => {}
        }
    }
    if cancellation.is_cancelled() {
        return Err(cancelled_before_artifact_inspection(artifact)?);
    }
    let partial = partial_path(&artifact.destination)?;
    let required = match partial_file_metadata(&partial)? {
        Some(metadata) if metadata.len() < artifact.size_bytes => {
            artifact.size_bytes - metadata.len()
        }
        Some(_) | None => artifact.size_bytes,
    };
    Ok(PinnedArtifactInspectionPlan {
        artifact: artifact.clone(),
        target_identity: expected_target_identity.clone(),
        state: PinnedArtifactInspectionState::RequiresDownload(required),
    })
}

#[cfg(test)]
fn pinned_artifact_required_download_bytes(artifact: &PinnedArtifact) -> Result<u64, InstallError> {
    let identity = disk_space::canonical_target_identity(&artifact.destination)
        .map_err(InstallError::Failed)?;
    Ok(
        inspect_pinned_artifact_for_target(artifact, &identity, &InstallCancellation::default())?
            .required_download_bytes(),
    )
}

fn validate_artifact_inspection(
    artifact: &PinnedArtifact,
    expected_target_identity: &CanonicalTargetIdentity,
    inspection: &PinnedArtifactInspectionPlan,
) -> Result<(), InstallError> {
    if inspection.artifact != *artifact || inspection.target_identity != *expected_target_identity {
        return Err(failed(
            "pinned artifact inspection does not match the requested artifact and target",
        ));
    }
    Ok(())
}

/// Removes only the resumable sidecar derived from a validated artifact.
/// The activated destination is deliberately never removed.
pub(crate) fn discard_pinned_artifact_partial(
    artifact: &PinnedArtifact,
) -> Result<bool, InstallError> {
    validate_artifact_spec(artifact)?;
    let partial = partial_path(&artifact.destination)?;
    if !partial_file_exists(&partial)? {
        return Ok(false);
    }
    fs::remove_file(&partial).map_err(|error| {
        failed(format!(
            "failed to discard resumable partial {}: {error}",
            partial.display()
        ))
    })?;
    sync_parent(&partial).map_err(|error| {
        InstallError::RecoveryRequired(format!(
            "discarded resumable partial {}, but could not confirm the directory update was durable: {error}",
            partial.display()
        ))
    })?;
    Ok(true)
}

fn partial_file_exists(partial: &Path) -> Result<bool, InstallError> {
    Ok(partial_file_metadata(partial)?.is_some())
}

fn partial_file_metadata(partial: &Path) -> Result<Option<fs::Metadata>, InstallError> {
    match fs::symlink_metadata(partial) {
        Ok(metadata)
            if metadata.file_type().is_file()
                && !runtime_metadata_is_link_or_reparse(&metadata) =>
        {
            Ok(Some(metadata))
        }
        Ok(_) => Err(failed(format!(
            "resumable partial {} is not a regular file",
            partial.display()
        ))),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(failed(format!(
            "failed to inspect resumable partial {}: {error}",
            partial.display()
        ))),
    }
}

fn cancelled_before_artifact_inspection(
    artifact: &PinnedArtifact,
) -> Result<InstallError, InstallError> {
    Ok(InstallError::Cancelled {
        partial_path: partial_path(&artifact.destination)?,
        downloaded_bytes: 0,
    })
}

fn artifact_destination_is_regular(destination: &Path) -> Result<bool, InstallError> {
    match fs::symlink_metadata(destination) {
        Ok(metadata) if metadata.is_file() && !runtime_metadata_is_link_or_reparse(&metadata) => {
            Ok(true)
        }
        Ok(_) => Err(failed(format!(
            "managed artifact destination is not a regular non-link file: {}",
            destination.display()
        ))),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(failed(format!(
            "failed to inspect managed artifact destination {}: {error}",
            destination.display()
        ))),
    }
}

fn revalidate_artifact_target(
    destination: &Path,
    expected_target_identity: &CanonicalTargetIdentity,
) -> Result<(), InstallError> {
    let parent = destination
        .parent()
        .ok_or_else(|| failed(format!("{} has no parent directory", destination.display())))?;
    for ancestor in parent.ancestors() {
        if ancestor.as_os_str().is_empty() {
            continue;
        }
        match fs::symlink_metadata(ancestor) {
            Ok(metadata)
                if metadata.is_dir() && !runtime_metadata_is_link_or_reparse(&metadata) => {}
            Ok(_) => {
                return Err(failed(format!(
                    "managed artifact path crosses a symbolic link, reparse point, or non-directory ancestor: {}",
                    ancestor.display()
                )));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(failed(format!(
                    "failed to revalidate managed artifact ancestor {}: {error}",
                    ancestor.display()
                )));
            }
        }
    }
    let actual =
        disk_space::canonical_target_identity(destination).map_err(InstallError::Failed)?;
    if &actual != expected_target_identity {
        return Err(failed(format!(
            "managed artifact target identity changed before filesystem mutation: {}",
            destination.display()
        )));
    }
    let _ = artifact_destination_is_regular(destination)?;
    Ok(())
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

fn validate_opened_regular_file(file: &File, path: &Path) -> Result<fs::Metadata, InstallError> {
    let metadata = file.metadata().map_err(|error| {
        failed(format!(
            "failed to inspect open file {}: {error}",
            path.display()
        ))
    })?;
    if !metadata.is_file() || runtime_metadata_is_link_or_reparse(&metadata) {
        return Err(failed(format!(
            "managed artifact file is not a regular non-link file: {}",
            path.display()
        )));
    }
    Ok(metadata)
}

fn open_regular_file_no_follow(path: &Path) -> Result<(File, fs::Metadata), InstallError> {
    let mut options = OpenOptions::new();
    options.read(true);
    configure_no_follow(&mut options);
    let file = options
        .open(path)
        .map_err(|error| failed(format!("failed to open {}: {error}", path.display())))?;
    let metadata = validate_opened_regular_file(&file, path)?;
    Ok((file, metadata))
}

#[cfg(unix)]
fn opened_file_identity(file: &File) -> Result<OpenedFileIdentity, InstallError> {
    use std::os::unix::fs::MetadataExt;

    let metadata = file
        .metadata()
        .map_err(|error| failed(format!("failed to inspect open artifact: {error}")))?;
    Ok(OpenedFileIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

#[cfg(windows)]
fn opened_file_identity(file: &File) -> Result<OpenedFileIdentity, InstallError> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
    };

    // SAFETY: the all-zero representation is valid for this plain C output
    // structure, and the OS initializes every field on success.
    let mut information = unsafe { std::mem::zeroed::<BY_HANDLE_FILE_INFORMATION>() };
    // SAFETY: `file` owns a valid handle for the duration of this call and
    // `information` points to writable storage of the required type.
    let succeeded = unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut information) };
    if succeeded == 0 {
        return Err(failed(format!(
            "failed to identify open artifact: {}",
            io::Error::last_os_error()
        )));
    }
    Ok(OpenedFileIdentity {
        volume_serial: information.dwVolumeSerialNumber,
        file_index_high: information.nFileIndexHigh,
        file_index_low: information.nFileIndexLow,
    })
}

fn additional_download_bytes(
    artifact_size_bytes: u64,
    partial_bytes: u64,
) -> Result<u64, InstallError> {
    // An oversized partial will be quarantined before a later retry. Reserve
    // a complete replacement artifact rather than reporting a misleading
    // zero-byte need in the browse UI.
    if partial_bytes > artifact_size_bytes {
        Ok(artifact_size_bytes)
    } else {
        Ok(artifact_size_bytes - partial_bytes)
    }
}

fn require_pinned_artifact_disk_space(
    artifact: &PinnedArtifact,
    partial_bytes: u64,
) -> Result<(), InstallError> {
    let additional_bytes = additional_download_bytes(artifact.size_bytes, partial_bytes)?;
    let preflight =
        disk_space::preflight_download_destination(&artifact.destination, additional_bytes)
            .map_err(InstallError::Failed)?;
    if preflight.has_sufficient_space() {
        return Ok(());
    }
    Err(failed(format!(
        "insufficient free space on {}: {} bytes are available but {} bytes are required, including Scribe's {}-byte safety headroom",
        preflight.volume,
        preflight.available_bytes,
        preflight.required_bytes,
        disk_space::SAFETY_HEADROOM_BYTES,
    )))
}

type DownloadReaderSpawner =
    fn(Box<dyn Read + Send + Sync>) -> Result<PendingDownloadReader, InstallError>;

#[cfg(test)]
fn download_pinned_artifact_with(
    source: &dyn HttpSource,
    artifact: &PinnedArtifact,
    cancellation: &InstallCancellation,
    progress: &dyn Fn(InstallProgress),
) -> Result<DownloadedArtifact, InstallError> {
    if cancellation.is_cancelled() {
        return Err(cancelled_before_artifact_inspection(artifact)?);
    }
    let expected_target_identity = disk_space::canonical_target_identity(&artifact.destination)
        .map_err(InstallError::Failed)?;
    download_pinned_artifact_with_target(
        source,
        artifact,
        &expected_target_identity,
        true,
        cancellation,
        progress,
    )
}

#[cfg(test)]
fn download_pinned_artifact_with_policy(
    source: &dyn HttpSource,
    artifact: &PinnedArtifact,
    cancellation: &InstallCancellation,
    progress: &dyn Fn(InstallProgress),
    policy: DownloadRetryPolicy,
) -> Result<DownloadedArtifact, InstallError> {
    if cancellation.is_cancelled() {
        return Err(cancelled_before_artifact_inspection(artifact)?);
    }
    let expected_target_identity = disk_space::canonical_target_identity(&artifact.destination)
        .map_err(InstallError::Failed)?;
    download_pinned_artifact_with_target_and_policy(
        source,
        artifact,
        &expected_target_identity,
        true,
        cancellation,
        progress,
        policy,
    )
}

#[cfg(test)]
fn download_pinned_artifact_with_policy_and_reader_spawner(
    source: &dyn HttpSource,
    artifact: &PinnedArtifact,
    cancellation: &InstallCancellation,
    progress: &dyn Fn(InstallProgress),
    policy: DownloadRetryPolicy,
    reader_spawner: DownloadReaderSpawner,
) -> Result<DownloadedArtifact, InstallError> {
    if cancellation.is_cancelled() {
        return Err(cancelled_before_artifact_inspection(artifact)?);
    }
    let expected_target_identity = disk_space::canonical_target_identity(&artifact.destination)
        .map_err(InstallError::Failed)?;
    download_pinned_artifact_with_target_and_policy_and_reader_spawner(
        source,
        artifact,
        &expected_target_identity,
        true,
        cancellation,
        progress,
        policy,
        reader_spawner,
    )
}

fn download_pinned_artifact_with_target(
    source: &dyn HttpSource,
    artifact: &PinnedArtifact,
    expected_target_identity: &CanonicalTargetIdentity,
    verify_existing_destination: bool,
    cancellation: &InstallCancellation,
    progress: &dyn Fn(InstallProgress),
) -> Result<DownloadedArtifact, InstallError> {
    download_pinned_artifact_with_target_and_policy(
        source,
        artifact,
        expected_target_identity,
        verify_existing_destination,
        cancellation,
        progress,
        DownloadRetryPolicy::STANDARD,
    )
}

fn download_pinned_artifact_with_target_and_policy(
    source: &dyn HttpSource,
    artifact: &PinnedArtifact,
    expected_target_identity: &CanonicalTargetIdentity,
    verify_existing_destination: bool,
    cancellation: &InstallCancellation,
    progress: &dyn Fn(InstallProgress),
    policy: DownloadRetryPolicy,
) -> Result<DownloadedArtifact, InstallError> {
    download_pinned_artifact_with_target_and_policy_and_reader_spawner(
        source,
        artifact,
        expected_target_identity,
        verify_existing_destination,
        cancellation,
        progress,
        policy,
        spawn_download_reader,
    )
}

#[allow(clippy::too_many_arguments)]
fn download_pinned_artifact_with_target_and_policy_and_reader_spawner(
    source: &dyn HttpSource,
    artifact: &PinnedArtifact,
    expected_target_identity: &CanonicalTargetIdentity,
    verify_existing_destination: bool,
    cancellation: &InstallCancellation,
    progress: &dyn Fn(InstallProgress),
    policy: DownloadRetryPolicy,
    reader_spawner: DownloadReaderSpawner,
) -> Result<DownloadedArtifact, InstallError> {
    validate_artifact_spec(artifact)?;
    let partial = partial_path(&artifact.destination)?;
    if cancellation.is_cancelled() {
        return Err(InstallError::Cancelled {
            partial_path: partial,
            downloaded_bytes: 0,
        });
    }
    revalidate_artifact_target(&artifact.destination, expected_target_identity)?;
    let partial_exists = partial_file_exists(&partial)?;
    if verify_existing_destination
        && artifact_destination_is_regular(&artifact.destination)?
        && verify_file(&artifact.destination, artifact.size_bytes, &artifact.sha256).is_ok()
    {
        return Ok(DownloadedArtifact {
            id: artifact.id.clone(),
            path: artifact.destination.clone(),
            destination: artifact.destination.clone(),
            size_bytes: artifact.size_bytes,
            sha256: artifact.sha256.clone(),
            target_identity: expected_target_identity.clone(),
        });
        // A mismatched destination may be the currently active artifact from
        // an older manifest. It remains untouched until activation has a
        // verified replacement and a rollback handle.
    }

    if let Some(parent) = artifact.destination.parent() {
        revalidate_artifact_target(&artifact.destination, expected_target_identity)?;
        fs::create_dir_all(parent)
            .map_err(|error| failed(format!("failed to create {}: {error}", parent.display())))?;
        revalidate_artifact_target(&artifact.destination, expected_target_identity)?;
    }
    let mut offset = if partial_exists {
        partial_file_metadata(&partial)?
            .ok_or_else(|| failed("resumable partial vanished during validated inspection"))?
            .len()
    } else {
        0
    };
    if offset > artifact.size_bytes {
        let quarantined = quarantine_partial(
            &partial,
            "oversized",
            &artifact.destination,
            expected_target_identity,
        )?;
        return Err(failed(format!(
            "resumable partial for {} exceeds the pinned size; quarantined at {}",
            artifact.id,
            quarantined.display()
        )));
    }
    if cancellation.is_cancelled() {
        return Err(InstallError::Cancelled {
            partial_path: partial,
            downloaded_bytes: offset,
        });
    }

    if offset == artifact.size_bytes && offset > 0 {
        match verify_file_cancellable(
            &partial,
            artifact.size_bytes,
            &artifact.sha256,
            cancellation,
        ) {
            Ok(()) => {
                return Ok(downloaded_candidate(
                    artifact,
                    partial,
                    expected_target_identity,
                ));
            }
            Err(error) => {
                if matches!(error, InstallError::Cancelled { .. }) {
                    return Err(error);
                }
                let quarantined = quarantine_partial(
                    &partial,
                    "invalid",
                    &artifact.destination,
                    expected_target_identity,
                )?;
                offset = 0;
                eprintln!(
                    "Scribe quarantined checksum-invalid partial {} at {} before a clean retry: {error}",
                    artifact.id,
                    quarantined.display()
                );
            }
        }
    }

    require_pinned_artifact_disk_space(artifact, offset)?;

    let mut retry_count = 0_u8;
    let mut downloaded = offset;
    loop {
        if cancellation.is_cancelled() {
            sync_partial_for_pause(&partial)?;
            return Err(InstallError::Cancelled {
                partial_path: partial,
                downloaded_bytes: downloaded,
            });
        }

        revalidate_artifact_target(&artifact.destination, expected_target_identity)?;
        if let Some((partial_file, actual_offset)) = reopen_partial_no_follow(&partial)? {
            partial_file.sync_all().map_err(|error| {
                failed(format!("failed to preserve {}: {error}", partial.display()))
            })?;
            downloaded = actual_offset;
        } else {
            downloaded = 0;
        }
        if downloaded > artifact.size_bytes {
            return Err(failed(
                "resumable partial exceeded the pinned artifact size",
            ));
        }
        let request_offset = downloaded;
        let response = match source.get(
            &artifact.url,
            (request_offset > 0).then_some(request_offset),
            policy.read_poll_interval,
        ) {
            Ok(response) => response,
            Err(HttpSourceError::Fatal(error)) => {
                sync_partial_for_pause(&partial)?;
                return Err(error);
            }
            Err(HttpSourceError::Retryable(cause)) => {
                schedule_download_retry(
                    progress,
                    cancellation,
                    policy,
                    &partial,
                    artifact.size_bytes,
                    downloaded,
                    &mut retry_count,
                    cause,
                )?;
                continue;
            }
        };
        if let Some(cause) = retryable_http_status(response.status) {
            drop(response);
            schedule_download_retry(
                progress,
                cancellation,
                policy,
                &partial,
                artifact.size_bytes,
                downloaded,
                &mut retry_count,
                cause,
            )?;
            continue;
        }
        let disposition =
            match validate_download_response(&response, request_offset, artifact.size_bytes) {
                Ok(disposition) => disposition,
                Err(error) => {
                    sync_partial_for_pause(&partial)?;
                    return Err(error);
                }
            };
        revalidate_artifact_target(&artifact.destination, expected_target_identity)?;
        if disposition == ResponseDisposition::CompletePartial {
            verify_file_cancellable(
                &partial,
                artifact.size_bytes,
                &artifact.sha256,
                cancellation,
            )?;
            return Ok(downloaded_candidate(
                artifact,
                partial,
                expected_target_identity,
            ));
        }

        // The reader thread is created while the verified response remains
        // gated. A server that ignored Range must not cost the caller its
        // retained bytes merely because the local reader thread cannot start.
        let pending_reader = reader_spawner(response.reader)?;
        if cancellation.is_cancelled() {
            sync_partial_for_pause(&partial)?;
            return Err(InstallError::Cancelled {
                partial_path: partial,
                downloaded_bytes: downloaded,
            });
        }
        let append = disposition == ResponseDisposition::Append;
        let mut options = OpenOptions::new();
        options.create(true).write(true);
        configure_no_follow(&mut options);
        if append {
            options.append(true);
        }
        revalidate_artifact_target(&artifact.destination, expected_target_identity)?;
        let mut file = options
            .open(&partial)
            .map_err(|error| failed(format!("failed to open {}: {error}", partial.display())))?;
        let opened_metadata = validate_opened_regular_file(&file, &partial)?;
        if append && opened_metadata.len() != request_offset {
            return Err(failed(
                "resumable partial changed while its ranged response was in flight",
            ));
        }
        if !append {
            file.set_len(0).map_err(|error| {
                failed(format!("failed to reset {}: {error}", partial.display()))
            })?;
            downloaded = 0;
        }

        let started_at = Instant::now();
        let mut last_progress = started_at;
        let mut last_byte_at = started_at;
        let mut stalled = false;
        emit_progress_activity(
            progress,
            InstallStage::Downloading,
            downloaded,
            artifact.size_bytes,
            None,
            DownloadActivity::Active,
        );
        let attempt_start_bytes = downloaded;
        let reader_events = pending_reader.start();
        let retry_cause = loop {
            if cancellation.is_cancelled() {
                file.sync_all().map_err(|error| {
                    failed(format!("failed to preserve {}: {error}", partial.display()))
                })?;
                return Err(InstallError::Cancelled {
                    partial_path: partial,
                    downloaded_bytes: downloaded,
                });
            }
            let inactive_for = last_byte_at.elapsed();
            if inactive_for >= policy.reconnect_after {
                break Some(DownloadFaultKind::Stalled);
            }
            if !stalled && inactive_for >= policy.stalled_after {
                stalled = true;
                emit_progress_activity(
                    progress,
                    InstallStage::Downloading,
                    downloaded,
                    artifact.size_bytes,
                    None,
                    DownloadActivity::Stalled,
                );
            }
            let bytes = match reader_events.recv_timeout(
                policy
                    .cancellation_poll_interval
                    .min(Duration::from_millis(100)),
            ) {
                Ok(Ok(bytes)) => bytes,
                Ok(Err(error))
                    if matches!(
                        error.kind(),
                        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
                    ) =>
                {
                    continue;
                }
                Ok(Err(error)) => {
                    if let Some(cause) = retryable_read_fault(&error) {
                        break Some(cause);
                    }
                    file.sync_all().map_err(|sync_error| {
                        failed(format!(
                            "failed to preserve {}: {sync_error}",
                            partial.display()
                        ))
                    })?;
                    return Err(failed(
                        "download response could not be read; resumable partial was preserved",
                    ));
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    break Some(DownloadFaultKind::ConnectionReset);
                }
            };
            if bytes.is_empty() {
                if downloaded == artifact.size_bytes {
                    break None;
                }
                break Some(DownloadFaultKind::PrematureEof);
            }
            if stalled {
                stalled = false;
                emit_progress_activity(
                    progress,
                    InstallStage::Downloading,
                    downloaded,
                    artifact.size_bytes,
                    None,
                    DownloadActivity::Active,
                );
            }
            last_byte_at = Instant::now();
            downloaded = downloaded.saturating_add(bytes.len() as u64);
            if downloaded > artifact.size_bytes {
                file.sync_all().map_err(|error| {
                    failed(format!("failed to preserve {}: {error}", partial.display()))
                })?;
                return Err(failed(format!(
                    "download for {} exceeded the pinned size of {} bytes; partial retained at {}",
                    artifact.id,
                    artifact.size_bytes,
                    partial.display()
                )));
            }
            if let Err(error) = file.write_all(&bytes) {
                file.sync_all().map_err(|sync_error| {
                    failed(format!(
                        "failed to preserve {} after a write fault: {sync_error}",
                        partial.display()
                    ))
                })?;
                return Err(failed(format!(
                    "failed to write {}: {error}",
                    partial.display()
                )));
            }
            let now = Instant::now();
            if now.duration_since(last_progress) >= PROGRESS_INTERVAL {
                last_progress = now;
                emit_progress(
                    progress,
                    InstallStage::Downloading,
                    downloaded,
                    artifact.size_bytes,
                    started_at,
                    now,
                    attempt_start_bytes,
                );
            }
        };

        file.sync_all()
            .map_err(|error| failed(format!("failed to finish {}: {error}", partial.display())))?;
        drop(file);
        if let Some(retry_cause) = retry_cause {
            schedule_download_retry(
                progress,
                cancellation,
                policy,
                &partial,
                artifact.size_bytes,
                downloaded,
                &mut retry_count,
                retry_cause,
            )?;
            continue;
        }
        emit_progress(
            progress,
            InstallStage::Verifying,
            downloaded,
            artifact.size_bytes,
            started_at,
            Instant::now(),
            attempt_start_bytes,
        );
        revalidate_artifact_target(&artifact.destination, expected_target_identity)?;
        verify_file_cancellable(
            &partial,
            artifact.size_bytes,
            &artifact.sha256,
            cancellation,
        )?;
        return Ok(downloaded_candidate(
            artifact,
            partial,
            expected_target_identity,
        ));
    }
}

struct PendingDownloadReader {
    state: Arc<AtomicU8>,
    thread: Option<std::thread::Thread>,
    receiver: Option<std::sync::mpsc::Receiver<io::Result<Vec<u8>>>>,
}

impl PendingDownloadReader {
    fn start(mut self) -> std::sync::mpsc::Receiver<io::Result<Vec<u8>>> {
        let receiver = self
            .receiver
            .take()
            .expect("pending download reader always owns its receiver");
        self.state.store(DOWNLOAD_READER_STARTED, Ordering::Release);
        self.thread
            .take()
            .expect("pending download reader always owns its thread")
            .unpark();
        receiver
    }
}

impl Drop for PendingDownloadReader {
    fn drop(&mut self) {
        let Some(thread) = self.thread.take() else {
            return;
        };
        self.state.store(DOWNLOAD_READER_ABORTED, Ordering::Release);
        thread.unpark();
    }
}

const DOWNLOAD_READER_WAITING: u8 = 0;
const DOWNLOAD_READER_STARTED: u8 = 1;
const DOWNLOAD_READER_ABORTED: u8 = 2;

fn spawn_download_reader(
    mut reader: Box<dyn Read + Send + Sync>,
) -> Result<PendingDownloadReader, InstallError> {
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    let state = Arc::new(AtomicU8::new(DOWNLOAD_READER_WAITING));
    let reader_state = Arc::clone(&state);
    let handle = std::thread::Builder::new()
        .name("scribe-download-reader".to_owned())
        .spawn(move || {
            loop {
                match reader_state.load(Ordering::Acquire) {
                    DOWNLOAD_READER_WAITING => std::thread::park(),
                    DOWNLOAD_READER_STARTED => break,
                    DOWNLOAD_READER_ABORTED => return,
                    _ => unreachable!("download reader gate state is invalid"),
                }
            }
            loop {
                let mut buffer = vec![0_u8; BUFFER_BYTES];
                let event = reader.read(&mut buffer).map(|count| {
                    buffer.truncate(count);
                    buffer
                });
                let terminal = match &event {
                    Ok(bytes) => bytes.is_empty(),
                    Err(error) => !matches!(
                        error.kind(),
                        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
                    ),
                };
                if sender.send(event).is_err() || terminal {
                    break;
                }
            }
        })
        .map_err(|_| failed("failed to start the download response reader"))?;
    let thread = handle.thread().clone();
    drop(handle);
    Ok(PendingDownloadReader {
        state,
        thread: Some(thread),
        receiver: Some(receiver),
    })
}

fn reopen_partial_no_follow(partial: &Path) -> Result<Option<(File, u64)>, InstallError> {
    if !partial_file_exists(partial)? {
        return Ok(None);
    }
    let mut options = OpenOptions::new();
    options.read(true).write(true);
    configure_no_follow(&mut options);
    let file = options
        .open(partial)
        .map_err(|error| failed(format!("failed to reopen {}: {error}", partial.display())))?;
    let metadata = validate_opened_regular_file(&file, partial)?;
    let length = metadata.len();
    Ok(Some((file, length)))
}

fn sync_partial_for_pause(partial: &Path) -> Result<u64, InstallError> {
    let Some((file, length)) = reopen_partial_no_follow(partial)? else {
        return Ok(0);
    };
    file.sync_all()
        .map_err(|error| failed(format!("failed to preserve {}: {error}", partial.display())))?;
    Ok(length)
}

fn retryable_read_fault(error: &io::Error) -> Option<DownloadFaultKind> {
    match error.kind() {
        io::ErrorKind::UnexpectedEof => Some(DownloadFaultKind::PrematureEof),
        io::ErrorKind::ConnectionReset
        | io::ErrorKind::ConnectionAborted
        | io::ErrorKind::BrokenPipe => Some(DownloadFaultKind::ConnectionReset),
        _ => None,
    }
}

#[allow(clippy::too_many_arguments)]
fn schedule_download_retry(
    progress: &dyn Fn(InstallProgress),
    cancellation: &InstallCancellation,
    policy: DownloadRetryPolicy,
    partial: &Path,
    total_bytes: u64,
    retained_bytes: u64,
    retry_count: &mut u8,
    cause: DownloadFaultKind,
) -> Result<(), InstallError> {
    let actual_retained_bytes = sync_partial_for_pause(partial)?;
    debug_assert_eq!(retained_bytes, actual_retained_bytes);
    let retained_bytes = actual_retained_bytes;
    if *retry_count >= policy.max_retries() {
        return Err(InstallError::DownloadFailed {
            category: cause,
            retry_count: *retry_count,
            retained_bytes,
            partial_path: partial.to_path_buf(),
        });
    }
    let attempt = retry_count.saturating_add(1);
    let delay = policy.retry_backoffs[*retry_count as usize];
    emit_progress_activity(
        progress,
        InstallStage::Downloading,
        retained_bytes,
        total_bytes,
        None,
        DownloadActivity::RetryScheduled {
            attempt,
            max_attempts: policy.max_retries(),
            delay_ms: delay.as_millis().min(u128::from(u64::MAX)) as u64,
            cause,
        },
    );
    sleep_cancellable(
        delay,
        policy.cancellation_poll_interval,
        cancellation,
        partial,
        retained_bytes,
    )?;
    *retry_count = attempt;
    Ok(())
}

fn sleep_cancellable(
    delay: Duration,
    poll_interval: Duration,
    cancellation: &InstallCancellation,
    partial: &Path,
    retained_bytes: u64,
) -> Result<(), InstallError> {
    let started_at = Instant::now();
    loop {
        if cancellation.is_cancelled() {
            sync_partial_for_pause(partial)?;
            return Err(InstallError::Cancelled {
                partial_path: partial.to_path_buf(),
                downloaded_bytes: retained_bytes,
            });
        }
        let remaining = delay.saturating_sub(started_at.elapsed());
        if remaining.is_zero() {
            return Ok(());
        }
        std::thread::sleep(remaining.min(poll_interval).min(Duration::from_millis(100)));
    }
}

fn retryable_http_status(status: u16) -> Option<DownloadFaultKind> {
    matches!(status, 408 | 429 | 500 | 502 | 503 | 504)
        .then_some(DownloadFaultKind::HttpStatus(status))
}

fn downloaded_candidate(
    artifact: &PinnedArtifact,
    path: PathBuf,
    target_identity: &CanonicalTargetIdentity,
) -> DownloadedArtifact {
    DownloadedArtifact {
        id: artifact.id.clone(),
        path,
        destination: artifact.destination.clone(),
        size_bytes: artifact.size_bytes,
        sha256: artifact.sha256.clone(),
        target_identity: target_identity.clone(),
    }
}

impl DownloadedArtifact {
    pub(crate) fn activate(self) -> Result<FileReplacement, InstallError> {
        revalidate_artifact_target(&self.destination, &self.target_identity)?;
        if self.path == self.destination {
            return Ok(FileReplacement {
                destination: self.destination,
                state: FileReplacementState::Unchanged,
            });
        }
        verify_file(&self.path, self.size_bytes, &self.sha256)?;
        let rollback = file_rollback_path(&self.destination)?;
        revalidate_artifact_target(&self.destination, &self.target_identity)?;
        remove_path_if_exists(&rollback)?;
        let previous = if artifact_destination_is_regular(&self.destination)? {
            revalidate_artifact_target(&self.destination, &self.target_identity)?;
            durable_rename(&self.destination, &rollback).map_err(|error| {
                failed(format!(
                    "failed to preserve existing model {}: {error}",
                    self.destination.display()
                ))
            })?;
            sync_parent(&self.destination).map_err(|error| {
                InstallError::RecoveryRequired(format!(
                    "preserved existing model {}, but its rollback rename was not made durable: {error}",
                    self.destination.display()
                ))
            })?;
            Some(rollback)
        } else {
            None
        };
        revalidate_artifact_target(&self.destination, &self.target_identity)?;
        partial_file_metadata(&self.path)?
            .ok_or_else(|| failed("verified artifact partial changed before activation"))?;
        if let Err(error) = durable_rename(&self.path, &self.destination) {
            if let Some(rollback) = previous.as_ref()
                && let Err(restore) = durable_rename(rollback, &self.destination)
            {
                return Err(InstallError::RecoveryRequired(format!(
                    "failed to activate model {}: {error}; restoring the prior model also failed: {restore}",
                    self.destination.display()
                )));
            }
            return Err(failed(format!(
                "failed to activate model {}: {error}",
                self.destination.display()
            )));
        }
        if let Err(error) = sync_parent(&self.destination) {
            if let Err(remove) = remove_path_if_exists(&self.destination) {
                return Err(InstallError::RecoveryRequired(format!(
                    "model activation durability failed: {error}; removing the new model also failed: {remove}"
                )));
            }
            if let Some(rollback) = previous.as_ref()
                && let Err(restore) = durable_rename(rollback, &self.destination)
            {
                return Err(InstallError::RecoveryRequired(format!(
                    "model activation durability failed: {error}; restoring the prior model also failed: {restore}"
                )));
            }
            return Err(InstallError::RecoveryRequired(format!(
                "model activation durability remained ambiguous after logical rollback: {error}"
            )));
        }
        Ok(FileReplacement {
            destination: self.destination,
            state: match previous {
                Some(rollback_path) => FileReplacementState::Replaced { rollback_path },
                None => FileReplacementState::Created,
            },
        })
    }
}

pub(crate) fn stage_runtime_archive_for_target(
    spec: &RuntimeArchiveSpec,
    target_root: &Path,
    entrypoint_relative: &Path,
    expected_archive_target_identity: &CanonicalTargetIdentity,
    cancellation: &InstallCancellation,
    progress: &dyn Fn(InstallProgress),
) -> Result<StagedRuntime, InstallError> {
    let archive = download_pinned_artifact_for_target(
        &spec.artifact,
        expected_archive_target_identity,
        None,
        cancellation,
        progress,
    )?;
    let stage_root = transaction_path(target_root, "installing")?;
    remove_path_if_exists(&stage_root)?;
    fs::create_dir_all(&stage_root).map_err(|error| {
        failed(format!(
            "failed to create {}: {error}",
            stage_root.display()
        ))
    })?;
    let preparation = (|| {
        extract_runtime_archive(
            &archive.path,
            &stage_root,
            &spec.files,
            cancellation,
            progress,
        )?;
        verify_runtime_tree(&stage_root, &spec.files)?;
        let entrypoint = stage_root.join(entrypoint_relative);
        if !entrypoint.is_file() {
            return Err(failed(format!(
                "staged runtime has no entrypoint at {}",
                entrypoint.display()
            )));
        }
        Ok(())
    })();
    if let Err(error) = preparation {
        let _ = remove_path_if_exists(&stage_root);
        return Err(error);
    }
    Ok(StagedRuntime {
        root: stage_root,
        target_root: target_root.to_path_buf(),
    })
}

/// Assembles exact, individually verified files into a fresh same-volume
/// staging directory. A prior crash can leave only the reserved staging path;
/// the next explicit installation safely replaces that path before doing any
/// assembly. Installed and retained-previous directories are never touched.
pub(crate) fn stage_file_bundle_for_target(
    files: &[BundleAssemblyFile],
    generated_files: &[GeneratedBundleFile],
    target_root: &Path,
    cancellation: &InstallCancellation,
    progress: &dyn Fn(InstallProgress),
) -> Result<StagedRuntime, InstallError> {
    if files.is_empty() {
        return Err(failed("file bundle has no downloaded files"));
    }
    let stage_root = stable_staging_path(target_root)?;
    validate_non_overlapping_paths(&stage_root, target_root)?;
    reject_link_or_reparse_ancestors(target_root)?;
    remove_path_if_exists(&stage_root)?;
    fs::create_dir(&stage_root).map_err(|error| {
        failed(format!(
            "failed to create fresh bundle staging directory {}: {error}",
            stage_root.display()
        ))
    })?;
    let preparation = (|| {
        let total_bytes = files.iter().try_fold(0_u64, |total, file| {
            total
                .checked_add(file.size_bytes)
                .ok_or_else(|| failed("bundle assembly size overflow"))
        })?;
        let mut completed_bytes = 0_u64;
        for file in files {
            if cancellation.is_cancelled() {
                return Err(InstallError::Cancelled {
                    partial_path: file.source_path.clone(),
                    downloaded_bytes: completed_bytes,
                });
            }
            validate_relative_path(&file.install_path)?;
            validate_sha256(&file.sha256)?;
            copy_regular_file_to_stage(
                &file.source_path,
                &stage_root,
                &file.install_path,
                file.size_bytes,
                &file.sha256,
                cancellation,
            )?;
            completed_bytes = completed_bytes
                .checked_add(file.size_bytes)
                .ok_or_else(|| failed("bundle assembly size overflow"))?;
            progress(InstallProgress {
                stage: InstallStage::Extracting,
                completed_bytes,
                total_bytes,
                bytes_per_second: None,
                download_activity: None,
            });
        }
        for file in generated_files {
            validate_relative_path(&file.install_path)?;
            write_new_bundle_file(&stage_root, &file.install_path, &file.bytes)?;
        }
        let mut exact_files = files
            .iter()
            .map(|file| RuntimeFileSpec {
                archive_path: file.install_path.clone(),
                install_path: file.install_path.clone(),
                size_bytes: file.size_bytes,
                sha256: file.sha256.clone(),
            })
            .collect::<Vec<_>>();
        exact_files.extend(generated_files.iter().map(|file| RuntimeFileSpec {
            archive_path: file.install_path.clone(),
            install_path: file.install_path.clone(),
            size_bytes: file.bytes.len() as u64,
            sha256: format!("{:x}", Sha256::digest(&file.bytes)),
        }));
        verify_runtime_tree_cancellable(&stage_root, &exact_files, cancellation)
    })();
    if let Err(error) = preparation {
        let _ = remove_path_if_exists(&stage_root);
        return Err(error);
    }
    Ok(StagedRuntime {
        root: stage_root,
        target_root: target_root.to_path_buf(),
    })
}

fn stable_staging_path(target_root: &Path) -> Result<PathBuf, InstallError> {
    let name = target_root
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| failed(format!("{} has no safe filename", target_root.display())))?;
    Ok(target_root.with_file_name(format!(".{name}.installing")))
}

pub(crate) fn directory_activation_rollback_root(target_root: &Path) -> PathBuf {
    directory_rollback_path(target_root)
}

pub(crate) fn path_entry_exists_no_follow(path: &Path) -> Result<bool, InstallError> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(failed(format!(
            "failed to inspect filesystem entry {}: {error}",
            path.display()
        ))),
    }
}

pub(crate) fn discard_file_bundle_staging(target_root: &Path) -> Result<bool, InstallError> {
    let staging = stable_staging_path(target_root)?;
    if !path_entry_exists_no_follow(&staging)? {
        return Ok(false);
    }
    remove_path_if_exists(&staging)?;
    Ok(true)
}

pub(crate) fn restore_interrupted_directory_replacement(
    target_root: &Path,
) -> Result<(), InstallError> {
    let rollback = directory_rollback_path(target_root);
    if path_entry_exists_no_follow(target_root)? {
        return Err(InstallError::RecoveryRequired(format!(
            "cannot restore interrupted directory replacement because target still exists: {}",
            target_root.display()
        )));
    }
    if !path_entry_exists_no_follow(&rollback)? {
        return Err(InstallError::RecoveryRequired(format!(
            "cannot restore interrupted directory replacement without rollback: {}",
            rollback.display()
        )));
    }
    restore_directory_replacement(target_root, true)
}

pub(crate) fn retain_interrupted_directory_replacement(
    target_root: &Path,
) -> Result<(), InstallError> {
    if !path_entry_exists_no_follow(target_root)? {
        return Err(InstallError::RecoveryRequired(format!(
            "cannot retain an interrupted directory replacement without an active target: {}",
            target_root.display()
        )));
    }
    let rollback = directory_rollback_path(target_root);
    if !path_entry_exists_no_follow(&rollback)? {
        return Err(InstallError::RecoveryRequired(format!(
            "cannot retain an interrupted directory replacement without rollback: {}",
            rollback.display()
        )));
    }
    finalize_directory_replacement(target_root, true)
}

fn copy_regular_file_to_stage(
    source: &Path,
    stage_root: &Path,
    relative: &Path,
    expected_size: u64,
    expected_sha256: &str,
    cancellation: &InstallCancellation,
) -> Result<(), InstallError> {
    validate_sha256(expected_sha256)?;
    ensure_no_symlink_components(stage_root, relative)?;
    let output = stage_root.join(relative);
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| failed(format!("failed to create {}: {error}", parent.display())))?;
        ensure_no_symlink_components(stage_root, relative)?;
    }
    let (mut input, metadata) = open_regular_file_no_follow(source)?;
    if metadata.len() != expected_size {
        return Err(failed(format!(
            "bundle source changed size before assembly: {}",
            source.display()
        )));
    }
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    configure_no_follow(&mut options);
    let mut destination = options
        .open(&output)
        .map_err(|error| failed(format!("failed to create {}: {error}", output.display())))?;
    validate_opened_regular_file(&destination, &output)?;
    let mut copied = 0_u64;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; BUFFER_BYTES];
    loop {
        if cancellation.is_cancelled() {
            return Err(InstallError::Cancelled {
                partial_path: source.to_path_buf(),
                downloaded_bytes: copied,
            });
        }
        let count = input
            .read(&mut buffer)
            .map_err(|error| failed(format!("failed to read {}: {error}", source.display())))?;
        if count == 0 {
            break;
        }
        copied = copied
            .checked_add(count as u64)
            .ok_or_else(|| failed("bundle copy size overflow"))?;
        if copied > expected_size {
            return Err(failed(format!(
                "bundle source exceeded its pinned size: {}",
                source.display()
            )));
        }
        hasher.update(&buffer[..count]);
        destination
            .write_all(&buffer[..count])
            .map_err(|error| failed(format!("failed to write {}: {error}", output.display())))?;
    }
    if copied != expected_size {
        return Err(failed(format!(
            "bundle source length changed during assembly: {}",
            source.display()
        )));
    }
    let actual_sha256 = format!("{:x}", hasher.finalize());
    if !actual_sha256.eq_ignore_ascii_case(expected_sha256) {
        return Err(failed(format!(
            "bundle source checksum mismatch while copying {}: expected {expected_sha256}, got {actual_sha256}",
            source.display()
        )));
    }
    destination
        .sync_all()
        .map_err(|error| failed(format!("failed to sync {}: {error}", output.display())))
}

fn write_new_bundle_file(
    stage_root: &Path,
    relative: &Path,
    bytes: &[u8],
) -> Result<(), InstallError> {
    ensure_no_symlink_components(stage_root, relative)?;
    let output = stage_root.join(relative);
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| failed(format!("failed to create {}: {error}", parent.display())))?;
        ensure_no_symlink_components(stage_root, relative)?;
    }
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    configure_no_follow(&mut options);
    let mut file = options
        .open(&output)
        .map_err(|error| failed(format!("failed to create {}: {error}", output.display())))?;
    validate_opened_regular_file(&file, &output)?;
    file.write_all(bytes)
        .map_err(|error| failed(format!("failed to write {}: {error}", output.display())))?;
    file.sync_all()
        .map_err(|error| failed(format!("failed to sync {}: {error}", output.display())))
}

fn extract_runtime_archive(
    archive_path: &Path,
    stage_root: &Path,
    files: &[RuntimeFileSpec],
    cancellation: &InstallCancellation,
    progress: &dyn Fn(InstallProgress),
) -> Result<(), InstallError> {
    let file = File::open(archive_path).map_err(|error| {
        failed(format!(
            "failed to open {}: {error}",
            archive_path.display()
        ))
    })?;
    let mut archive = zip::ZipArchive::new(file)
        .map_err(|error| failed(format!("invalid runtime ZIP: {error}")))?;
    if archive
        .has_overlapping_files()
        .map_err(|error| failed(format!("failed to validate runtime ZIP layout: {error}")))?
    {
        return Err(failed("runtime ZIP contains overlapping entries"));
    }
    let expected = files
        .iter()
        .map(|file| (file.archive_path.clone(), file))
        .collect::<HashMap<_, _>>();
    if archive.len() > 256 {
        return Err(failed("runtime ZIP exceeds the 256-entry safety limit"));
    }
    let total = files.iter().try_fold(0_u64, |total, file| {
        total
            .checked_add(file.size_bytes)
            .ok_or_else(|| failed("runtime manifest expanded size overflow"))
    })?;
    let mut extracted = 0_u64;
    let mut found = HashSet::new();
    for index in 0..archive.len() {
        if cancellation.is_cancelled() {
            return Err(InstallError::Cancelled {
                partial_path: archive_path.to_path_buf(),
                downloaded_bytes: fs::metadata(archive_path)
                    .map(|metadata| metadata.len())
                    .unwrap_or(0),
            });
        }
        let mut entry = archive
            .by_index(index)
            .map_err(|error| failed(format!("failed to read runtime ZIP entry: {error}")))?;
        let enclosed = entry
            .enclosed_name()
            .ok_or_else(|| failed(format!("unsafe runtime ZIP path: {}", entry.name())))?;
        validate_relative_path(&enclosed)?;
        if entry.unix_mode().is_some_and(|mode| {
            let file_type = mode & 0o170000;
            file_type != 0 && file_type != 0o100000 && file_type != 0o040000
        }) {
            return Err(failed(format!(
                "runtime ZIP entry {} is not a regular file or directory",
                entry.name()
            )));
        }
        if entry.is_dir() {
            continue;
        }
        let Some(file_spec) = expected.get(&enclosed) else {
            continue;
        };
        if !found.insert(enclosed.clone()) {
            return Err(failed(format!(
                "runtime ZIP contains duplicate entry {}",
                enclosed.display()
            )));
        }
        if entry.size() != file_spec.size_bytes {
            return Err(failed(format!(
                "runtime ZIP entry {} size mismatch: expected {}, got {}",
                enclosed.display(),
                file_spec.size_bytes,
                entry.size()
            )));
        }
        validate_relative_path(&file_spec.install_path)?;
        let output = stage_root.join(&file_spec.install_path);
        ensure_no_symlink_components(stage_root, &file_spec.install_path)?;
        if let Some(parent) = output.parent() {
            fs::create_dir_all(parent).map_err(|error| {
                failed(format!("failed to create {}: {error}", parent.display()))
            })?;
        }
        let mut destination = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&output)
            .map_err(|error| failed(format!("failed to create {}: {error}", output.display())))?;
        let mut copied = 0_u64;
        let mut buffer = [0_u8; BUFFER_BYTES];
        loop {
            if cancellation.is_cancelled() {
                return Err(InstallError::Cancelled {
                    partial_path: archive_path.to_path_buf(),
                    downloaded_bytes: copied,
                });
            }
            let count = entry.read(&mut buffer).map_err(|error| {
                failed(format!("failed to extract {}: {error}", enclosed.display()))
            })?;
            if count == 0 {
                break;
            }
            copied = copied
                .checked_add(count as u64)
                .ok_or_else(|| failed("runtime extraction size overflow"))?;
            if copied > file_spec.size_bytes {
                return Err(failed(format!(
                    "runtime ZIP entry {} exceeded its pinned size",
                    enclosed.display()
                )));
            }
            destination.write_all(&buffer[..count]).map_err(|error| {
                failed(format!("failed to write {}: {error}", output.display()))
            })?;
        }
        if copied != file_spec.size_bytes {
            return Err(failed(format!(
                "runtime ZIP entry {} extracted length mismatch",
                enclosed.display()
            )));
        }
        destination
            .sync_all()
            .map_err(|error| failed(format!("failed to finish {}: {error}", output.display())))?;
        extracted = extracted.saturating_add(copied);
        progress(InstallProgress {
            stage: InstallStage::Extracting,
            completed_bytes: extracted,
            total_bytes: total,
            bytes_per_second: None,
            download_activity: None,
        });
    }
    for file in files {
        if !found.contains(&file.archive_path) {
            return Err(failed(format!(
                "runtime ZIP is missing {}",
                file.archive_path.display()
            )));
        }
    }
    Ok(())
}

pub(crate) fn verify_runtime_tree(
    root: &Path,
    files: &[RuntimeFileSpec],
) -> Result<(), InstallError> {
    verify_runtime_tree_cancellable(root, files, &InstallCancellation::default())
}

pub(crate) fn verify_regular_directory_root(root: &Path) -> Result<(), InstallError> {
    reject_link_or_reparse_ancestors(root)?;
    let root_metadata = fs::symlink_metadata(root)
        .map_err(|error| failed(format!("failed to inspect {}: {error}", root.display())))?;
    if runtime_metadata_is_link_or_reparse(&root_metadata) || !root_metadata.is_dir() {
        return Err(failed(format!(
            "runtime package root is not a regular directory or is a symbolic link/reparse point: {}",
            root.display()
        )));
    }
    Ok(())
}

pub(crate) fn verify_runtime_tree_cancellable(
    root: &Path,
    files: &[RuntimeFileSpec],
    cancellation: &InstallCancellation,
) -> Result<(), InstallError> {
    verify_regular_directory_root(root)?;
    let mut allowed_files = HashSet::new();
    let mut allowed_directories = HashSet::new();
    for file in files {
        validate_relative_path(&file.install_path)?;
        if !allowed_files.insert(file.install_path.clone()) {
            return Err(failed(format!(
                "runtime manifest repeats {}",
                file.install_path.display()
            )));
        }
        let mut parent = file.install_path.parent();
        while let Some(directory) = parent {
            if directory.as_os_str().is_empty() {
                break;
            }
            allowed_directories.insert(directory.to_path_buf());
            parent = directory.parent();
        }
        ensure_no_symlink_components(root, &file.install_path)?;
        verify_file_cancellable(
            &root.join(&file.install_path),
            file.size_bytes,
            &file.sha256,
            cancellation,
        )?;
    }
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        if cancellation.is_cancelled() {
            return Err(InstallError::Cancelled {
                partial_path: root.to_path_buf(),
                downloaded_bytes: 0,
            });
        }
        for entry in fs::read_dir(&directory).map_err(|error| {
            failed(format!(
                "failed to enumerate {}: {error}",
                directory.display()
            ))
        })? {
            if cancellation.is_cancelled() {
                return Err(InstallError::Cancelled {
                    partial_path: root.to_path_buf(),
                    downloaded_bytes: 0,
                });
            }
            let entry = entry.map_err(|error| {
                failed(format!(
                    "failed to enumerate {}: {error}",
                    directory.display()
                ))
            })?;
            let path = entry.path();
            let relative = path.strip_prefix(root).map_err(|_| {
                failed(format!(
                    "runtime entry escaped package root: {}",
                    path.display()
                ))
            })?;
            let metadata = fs::symlink_metadata(&path).map_err(|error| {
                failed(format!("failed to inspect {}: {error}", path.display()))
            })?;
            if runtime_metadata_is_link_or_reparse(&metadata) {
                return Err(failed(format!(
                    "runtime tree contains a symbolic link or reparse point: {}",
                    path.display()
                )));
            }
            if metadata.is_dir() {
                if !allowed_directories.contains(relative) {
                    return Err(failed(format!(
                        "runtime tree contains unallowlisted directory {}",
                        relative.display()
                    )));
                }
                pending.push(path);
            } else if metadata.is_file() {
                if !allowed_files.contains(relative) {
                    return Err(failed(format!(
                        "runtime tree contains unallowlisted file {}",
                        relative.display()
                    )));
                }
            } else {
                return Err(failed(format!(
                    "runtime tree contains an unsupported entry {}",
                    relative.display()
                )));
            }
        }
    }
    Ok(())
}

pub(crate) fn activate_directory(
    stage_root: &Path,
    target_root: &Path,
) -> Result<DirectoryReplacement, InstallError> {
    validate_non_overlapping_paths(stage_root, target_root)?;
    let stage_metadata = fs::symlink_metadata(stage_root).map_err(|error| {
        failed(format!(
            "failed to inspect staged directory {}: {error}",
            stage_root.display()
        ))
    })?;
    if !stage_metadata.is_dir() || runtime_metadata_is_link_or_reparse(&stage_metadata) {
        return Err(failed(format!(
            "staged bundle is not a regular non-link directory: {}",
            stage_root.display()
        )));
    }
    reject_link_or_reparse_ancestors(target_root)?;
    let parent = target_root
        .parent()
        .ok_or_else(|| failed(format!("{} has no parent", target_root.display())))?;
    fs::create_dir_all(parent)
        .map_err(|error| failed(format!("failed to create {}: {error}", parent.display())))?;
    let rollback = directory_rollback_path(target_root);
    remove_path_if_exists(&rollback)?;
    let target_exists = match fs::symlink_metadata(target_root) {
        Ok(metadata) if metadata.is_dir() && !runtime_metadata_is_link_or_reparse(&metadata) => {
            true
        }
        Ok(_) => {
            return Err(failed(format!(
                "directory activation target is a link, reparse point, or non-directory: {}",
                target_root.display()
            )));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => false,
        Err(error) => {
            return Err(failed(format!(
                "failed to inspect directory activation target {}: {error}",
                target_root.display()
            )));
        }
    };
    let previous = if target_exists {
        durable_rename(target_root, &rollback).map_err(|error| {
            failed(format!(
                "failed to preserve existing runtime {}: {error}",
                target_root.display()
            ))
        })?;
        sync_parent(target_root).map_err(|error| {
            InstallError::RecoveryRequired(format!(
                "preserved existing runtime {}, but its rollback rename was not made durable: {error}",
                target_root.display()
            ))
        })?;
        Some(rollback)
    } else {
        None
    };
    if let Err(error) = durable_rename(stage_root, target_root) {
        if let Some(rollback) = previous.as_ref()
            && let Err(restore) = durable_rename(rollback, target_root)
        {
            return Err(InstallError::RecoveryRequired(format!(
                "failed to activate staged runtime {}: {error}; restoring the prior runtime also failed: {restore}",
                stage_root.display()
            )));
        }
        return Err(failed(format!(
            "failed to activate staged runtime {}: {error}",
            stage_root.display()
        )));
    }
    if let Err(error) = sync_parent(target_root) {
        if let Err(remove) = remove_path_if_exists(target_root) {
            return Err(InstallError::RecoveryRequired(format!(
                "runtime activation durability failed: {error}; removing the new runtime also failed: {remove}"
            )));
        }
        if let Some(rollback) = previous.as_ref()
            && let Err(restore) = durable_rename(rollback, target_root)
        {
            return Err(InstallError::RecoveryRequired(format!(
                "runtime activation durability failed: {error}; restoring the prior runtime also failed: {restore}"
            )));
        }
        return Err(InstallError::RecoveryRequired(format!(
            "runtime activation durability remained ambiguous after logical rollback: {error}"
        )));
    }
    Ok(DirectoryReplacement {
        target_root: target_root.to_path_buf(),
        rollback_path: previous,
    })
}

pub(crate) fn rollback_to_previous_runtime(target_root: &Path) -> Result<bool, InstallError> {
    let previous = previous_runtime_root(target_root);
    if !previous.exists() {
        return Ok(false);
    }
    let failed_root = transaction_path(target_root, "failed")?;
    remove_path_if_exists(&failed_root)?;
    if target_root.exists() {
        durable_rename(target_root, &failed_root).map_err(|error| {
            failed(format!(
                "failed to quarantine unhealthy runtime {}: {error}",
                target_root.display()
            ))
        })?;
    }
    if let Err(error) = durable_rename(&previous, target_root) {
        if failed_root.exists() {
            let _ = durable_rename(&failed_root, target_root);
        }
        return Err(failed(format!(
            "failed to restore previous runtime {}: {error}",
            target_root.display()
        )));
    }
    let _ = remove_path_if_exists(&failed_root);
    sync_parent(target_root)?;
    Ok(true)
}

pub(crate) fn verify_file(
    path: &Path,
    expected_size: u64,
    expected_sha256: &str,
) -> Result<(), InstallError> {
    verify_file_cancellable(
        path,
        expected_size,
        expected_sha256,
        &InstallCancellation::default(),
    )
}

/// Reads a small regular metadata file without following a final symbolic
/// link or Windows reparse point. Large model artifacts must use streaming
/// verification instead.
pub(crate) fn read_regular_file_no_follow(
    path: &Path,
    maximum_bytes: u64,
) -> Result<Vec<u8>, InstallError> {
    let (mut file, metadata) = open_regular_file_no_follow(path)?;
    if metadata.len() > maximum_bytes {
        return Err(failed(format!(
            "metadata file {} exceeds the {}-byte safety limit",
            path.display(),
            maximum_bytes
        )));
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.read_to_end(&mut bytes)
        .map_err(|error| failed(format!("failed to read {}: {error}", path.display())))?;
    if bytes.len() as u64 != metadata.len() {
        return Err(failed(format!(
            "metadata file changed while it was read: {}",
            path.display()
        )));
    }
    Ok(bytes)
}

/// Streams a regular, non-link file through SHA-256 without buffering its
/// contents. This is intentionally separate from `verify_file`: local import
/// has no upstream expected digest, so its result is an observed fingerprint,
/// never a trusted checksum claim.
pub(crate) fn fingerprint_file_cancellable(
    path: &Path,
    cancellation: &InstallCancellation,
) -> Result<FileFingerprint, InstallError> {
    let source_metadata = fs::symlink_metadata(path)
        .map_err(|error| failed(format!("failed to inspect {}: {error}", path.display())))?;
    if !source_metadata.file_type().is_file() || source_metadata.file_type().is_symlink() {
        return Err(failed(format!(
            "local import must be a regular file, not a link or directory: {}",
            path.display()
        )));
    }
    #[cfg(windows)]
    if has_reparse_point(&source_metadata) {
        return Err(failed(format!(
            "local import cannot use a Windows reparse-point file: {}",
            path.display()
        )));
    }
    let canonical_path = fs::canonicalize(path).map_err(|error| {
        failed(format!(
            "failed to canonicalize local import {}: {error}",
            path.display()
        ))
    })?;
    let size_bytes = source_metadata.len();
    if size_bytes == 0 {
        return Err(failed(format!(
            "local import is empty: {}",
            canonical_path.display()
        )));
    }
    let mut file = File::open(&canonical_path).map_err(|error| {
        failed(format!(
            "failed to open local import {}: {error}",
            canonical_path.display()
        ))
    })?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; BUFFER_BYTES];
    loop {
        if cancellation.is_cancelled() {
            return Err(InstallError::Cancelled {
                partial_path: canonical_path,
                downloaded_bytes: size_bytes,
            });
        }
        let count = file.read(&mut buffer).map_err(|error| {
            failed(format!(
                "failed to hash local import {}: {error}",
                canonical_path.display()
            ))
        })?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    let final_path = fs::canonicalize(path).map_err(|error| {
        failed(format!(
            "local import changed while hashing {}: {error}",
            path.display()
        ))
    })?;
    let final_metadata = fs::metadata(&final_path).map_err(|error| {
        failed(format!(
            "failed to inspect local import after hashing {}: {error}",
            final_path.display()
        ))
    })?;
    if final_path != canonical_path
        || !final_metadata.is_file()
        || final_metadata.len() != size_bytes
    {
        return Err(failed(format!(
            "local import changed while hashing: {}",
            path.display()
        )));
    }
    Ok(FileFingerprint {
        canonical_path,
        size_bytes,
        sha256: format!("{:x}", hasher.finalize()),
    })
}

#[cfg(windows)]
fn has_reparse_point(metadata: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;

    metadata.file_attributes() & 0x400 != 0
}

pub(crate) fn verify_file_cancellable(
    path: &Path,
    expected_size: u64,
    expected_sha256: &str,
    cancellation: &InstallCancellation,
) -> Result<(), InstallError> {
    validate_sha256(expected_sha256)?;
    if cancellation.is_cancelled() {
        return Err(InstallError::Cancelled {
            partial_path: path.to_path_buf(),
            downloaded_bytes: 0,
        });
    }
    let (mut file, metadata) = open_regular_file_no_follow(path)?;
    verify_opened_file_cancellable(
        &mut file,
        &metadata,
        path,
        expected_size,
        expected_sha256,
        cancellation,
    )
}

fn verify_opened_file_cancellable(
    file: &mut File,
    metadata: &fs::Metadata,
    path: &Path,
    expected_size: u64,
    expected_sha256: &str,
    cancellation: &InstallCancellation,
) -> Result<(), InstallError> {
    validate_sha256(expected_sha256)?;
    if cancellation.is_cancelled() {
        return Err(InstallError::Cancelled {
            partial_path: path.to_path_buf(),
            downloaded_bytes: 0,
        });
    }
    if !metadata.is_file() || metadata.len() != expected_size {
        return Err(failed(format!(
            "artifact size mismatch for {}: expected {expected_size} bytes, got {}",
            path.display(),
            metadata.len()
        )));
    }
    #[cfg(test)]
    {
        let mut counts = FILE_HASH_COUNTS
            .get_or_init(Default::default)
            .lock()
            .expect("file hash count lock poisoned");
        *counts.entry(path.to_path_buf()).or_default() += 1;
    }
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; BUFFER_BYTES];
    #[cfg(test)]
    let mut hashed_bytes = 0_u64;
    loop {
        if cancellation.is_cancelled() {
            return Err(InstallError::Cancelled {
                partial_path: path.to_path_buf(),
                downloaded_bytes: metadata.len(),
            });
        }
        let count = file
            .read(&mut buffer)
            .map_err(|error| failed(format!("failed to hash {}: {error}", path.display())))?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
        #[cfg(test)]
        {
            hashed_bytes = hashed_bytes.saturating_add(count as u64);
            cancel_file_hash_at_test_threshold(path, hashed_bytes);
        }
    }
    let actual = format!("{:x}", hasher.finalize());
    if !actual.eq_ignore_ascii_case(expected_sha256) {
        return Err(failed(format!(
            "artifact checksum mismatch for {}: expected {expected_sha256}, got {actual}",
            path.display()
        )));
    }
    Ok(())
}

#[cfg(test)]
pub(crate) fn reset_file_hash_count(path: &Path) {
    FILE_HASH_COUNTS
        .get_or_init(Default::default)
        .lock()
        .expect("file hash count lock poisoned")
        .remove(path);
}

#[cfg(test)]
pub(crate) fn file_hash_count(path: &Path) -> usize {
    FILE_HASH_COUNTS
        .get_or_init(Default::default)
        .lock()
        .expect("file hash count lock poisoned")
        .get(path)
        .copied()
        .unwrap_or_default()
}

#[cfg(test)]
fn cancel_file_hash_at_test_threshold(path: &Path, hashed_bytes: u64) {
    let cancellation = {
        let mut hooks = FILE_HASH_CANCELLATIONS
            .get_or_init(Default::default)
            .lock()
            .expect("file hash cancellation lock poisoned");
        if hooks
            .get(path)
            .is_some_and(|(threshold, _)| hashed_bytes >= *threshold)
        {
            hooks.remove(path).map(|(_, cancellation)| cancellation)
        } else {
            None
        }
    };
    if let Some(cancellation) = cancellation {
        cancellation.cancel();
    }
}

#[cfg(test)]
pub(crate) fn cancel_file_hash_after(
    path: &Path,
    threshold: u64,
    cancellation: InstallCancellation,
) {
    FILE_HASH_CANCELLATIONS
        .get_or_init(Default::default)
        .lock()
        .expect("file hash cancellation lock poisoned")
        .insert(path.to_path_buf(), (threshold, cancellation));
}

fn ensure_no_symlink_components(root: &Path, relative: &Path) -> Result<(), InstallError> {
    let mut current = root.to_path_buf();
    for component in relative.components() {
        let Component::Normal(component) = component else {
            return Err(failed("unsafe artifact path component"));
        };
        current.push(component);
        if current.exists() {
            let metadata = fs::symlink_metadata(&current).map_err(|error| {
                failed(format!("failed to inspect {}: {error}", current.display()))
            })?;
            if runtime_metadata_is_link_or_reparse(&metadata) {
                return Err(failed(format!(
                    "artifact path crosses a symbolic link or reparse point: {}",
                    current.display()
                )));
            }
        }
    }
    Ok(())
}

#[cfg(windows)]
fn runtime_metadata_is_link_or_reparse(metadata: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    metadata.file_type().is_symlink()
        || metadata.file_attributes()
            & windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT
            != 0
}

#[cfg(not(windows))]
fn runtime_metadata_is_link_or_reparse(metadata: &fs::Metadata) -> bool {
    metadata.file_type().is_symlink()
}

fn validate_artifact_spec(artifact: &PinnedArtifact) -> Result<(), InstallError> {
    let secure_url = artifact.url.starts_with("https://")
        || cfg!(test)
            && (artifact.url.starts_with("http://127.0.0.1:")
                || artifact.url.starts_with("http://[::1]:"));
    if artifact.id.trim().is_empty()
        || artifact.size_bytes == 0
        || !secure_url
        || artifact.destination.as_os_str().is_empty()
    {
        return Err(failed(format!(
            "artifact {} has an invalid pinned manifest",
            artifact.id
        )));
    }
    validate_sha256(&artifact.sha256)
}

fn validate_sha256(value: &str) -> Result<(), InstallError> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        Err(failed(
            "pinned SHA-256 must contain exactly 64 hexadecimal characters",
        ))
    } else {
        Ok(())
    }
}

fn validate_relative_path(path: &Path) -> Result<(), InstallError> {
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(failed(format!(
            "unsafe relative artifact path: {}",
            path.display()
        )));
    }
    Ok(())
}

fn validate_non_overlapping_paths(left: &Path, right: &Path) -> Result<(), InstallError> {
    let left = canonicalize_missing(left)?;
    let right = canonicalize_missing(right)?;
    if left == right || left.starts_with(&right) || right.starts_with(&left) {
        Err(failed(format!(
            "staging path {} overlaps activation target {}",
            left.display(),
            right.display()
        )))
    } else {
        Ok(())
    }
}

fn canonicalize_missing(path: &Path) -> Result<PathBuf, InstallError> {
    let mut missing = Vec::new();
    let mut current = path;
    loop {
        match current.canonicalize() {
            Ok(mut resolved) => {
                for component in missing.iter().rev() {
                    resolved.push(component);
                }
                return Ok(resolved);
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let name = current.file_name().ok_or_else(|| {
                    failed(format!("failed to resolve {}: {error}", path.display()))
                })?;
                missing.push(name.to_os_string());
                current = current.parent().ok_or_else(|| {
                    failed(format!("failed to resolve {}: {error}", path.display()))
                })?;
            }
            Err(error) => {
                return Err(failed(format!(
                    "failed to resolve {}: {error}",
                    path.display()
                )));
            }
        }
    }
}

fn partial_path(destination: &Path) -> Result<PathBuf, InstallError> {
    let name = destination
        .file_name()
        .ok_or_else(|| failed(format!("{} has no filename", destination.display())))?;
    let mut partial_name = name.to_os_string();
    partial_name.push(".partial");
    Ok(destination.with_file_name(partial_name))
}

fn quarantine_partial(
    partial: &Path,
    reason: &str,
    destination: &Path,
    expected_target_identity: &CanonicalTargetIdentity,
) -> Result<PathBuf, InstallError> {
    let name = partial
        .file_name()
        .ok_or_else(|| failed(format!("{} has no filename", partial.display())))?;
    let mut quarantined_name = name.to_os_string();
    quarantined_name.push(format!(".{reason}"));
    let quarantined = partial.with_file_name(quarantined_name);
    revalidate_artifact_target(destination, expected_target_identity)?;
    partial_file_metadata(partial)?
        .ok_or_else(|| failed("resumable partial vanished before quarantine"))?;
    remove_path_if_exists(&quarantined)?;
    revalidate_artifact_target(destination, expected_target_identity)?;
    partial_file_metadata(partial)?
        .ok_or_else(|| failed("resumable partial changed before quarantine"))?;
    durable_rename(partial, &quarantined).map_err(|error| {
        failed(format!(
            "failed to quarantine invalid partial {}: {error}",
            partial.display()
        ))
    })?;
    Ok(quarantined)
}

pub(crate) fn previous_runtime_root(target_root: &Path) -> PathBuf {
    let name = target_root
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("runtime");
    target_root.with_file_name(format!("{name}.previous"))
}

pub(crate) fn remove_previous_runtime_if_exists(target_root: &Path) -> Result<bool, InstallError> {
    let previous = previous_runtime_root(target_root);
    if !previous.exists() {
        return Ok(false);
    }
    remove_path_if_exists(&previous)?;
    Ok(true)
}

pub(crate) fn reconcile_orphaned_previous_runtime(
    target_root: &Path,
    runtime_is_configured: bool,
) -> Result<bool, InstallError> {
    if runtime_is_configured {
        return Ok(false);
    }
    remove_previous_runtime_if_exists(target_root)
}

fn directory_rollback_path(target_root: &Path) -> PathBuf {
    let name = target_root
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("runtime");
    target_root.with_file_name(format!(".{name}.rollback"))
}

fn file_rollback_path(destination: &Path) -> Result<PathBuf, InstallError> {
    let name = destination
        .file_name()
        .ok_or_else(|| failed(format!("{} has no filename", destination.display())))?;
    let mut rollback = name.to_os_string();
    rollback.push(".rollback");
    Ok(destination.with_file_name(rollback))
}

fn removal_tombstone_path(target: &Path) -> Result<PathBuf, InstallError> {
    let name = target
        .file_name()
        .ok_or_else(|| failed(format!("{} has no filename", target.display())))?;
    let mut tombstone = name.to_os_string();
    tombstone.push(".removing");
    Ok(target.with_file_name(tombstone))
}

fn removal_journal_path(target: &Path) -> Result<PathBuf, InstallError> {
    let name = target
        .file_name()
        .ok_or_else(|| failed(format!("{} has no filename", target.display())))?;
    let mut journal = name.to_os_string();
    journal.push(".removal-journal.json");
    Ok(target.with_file_name(journal))
}

fn restore_file_replacement(target: &Path, had_previous: bool) -> Result<(), InstallError> {
    let rollback = file_rollback_path(target)?;
    if rollback.exists() {
        remove_path_if_exists(target)?;
        durable_rename(&rollback, target).map_err(|error| {
            failed(format!(
                "failed to reconcile model rollback {}: {error}",
                target.display()
            ))
        })?;
        sync_parent(target).map_err(|error| {
            InstallError::RecoveryRequired(format!(
                "reconciled model rollback {}, but the rename was not made durable: {error}",
                target.display()
            ))
        })?;
    } else if !had_previous {
        remove_path_if_exists(target)?;
    }
    Ok(())
}

fn restore_directory_replacement(target: &Path, had_previous: bool) -> Result<(), InstallError> {
    let rollback = directory_rollback_path(target);
    if rollback.exists() {
        remove_path_if_exists(target)?;
        durable_rename(&rollback, target).map_err(|error| {
            failed(format!(
                "failed to reconcile runtime rollback {}: {error}",
                target.display()
            ))
        })?;
        sync_parent(target).map_err(|error| {
            InstallError::RecoveryRequired(format!(
                "reconciled runtime rollback {}, but the rename was not made durable: {error}",
                target.display()
            ))
        })?;
    } else if !had_previous {
        remove_path_if_exists(target)?;
    }
    Ok(())
}

fn finalize_file_replacement(target: &Path) -> Result<(), InstallError> {
    remove_path_if_exists(&file_rollback_path(target)?)
}

fn finalize_directory_replacement(
    target: &Path,
    retain_replaced_as_previous: bool,
) -> Result<(), InstallError> {
    let rollback = directory_rollback_path(target);
    if !rollback.exists() {
        return Ok(());
    }
    if !retain_replaced_as_previous {
        return remove_path_if_exists(&rollback);
    }
    let previous = previous_runtime_root(target);
    remove_path_if_exists(&previous)?;
    durable_rename(&rollback, &previous).map_err(|error| {
        failed(format!(
            "failed to retain previous runtime {}: {error}",
            previous.display()
        ))
    })?;
    sync_parent(&previous).map_err(|error| {
        InstallError::RecoveryRequired(format!(
            "retained previous runtime {}, but its directory entry was not made durable: {error}",
            previous.display()
        ))
    })
}

fn transaction_path(target: &Path, phase: &str) -> Result<PathBuf, InstallError> {
    let name = target
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| failed(format!("{} has no safe filename", target.display())))?;
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    Ok(target.with_file_name(format!(".{name}.{phase}-{}-{nonce}", std::process::id())))
}

fn remove_path_if_exists(path: &Path) -> Result<(), InstallError> {
    if path.as_os_str().is_empty() {
        return Ok(());
    }
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(failed(format!(
                "failed to inspect {}: {error}",
                path.display()
            )));
        }
    };
    let result = if runtime_metadata_is_link_or_reparse(&metadata) && metadata.is_dir() {
        fs::remove_dir(path)
    } else if metadata.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    };
    result.map_err(|error| failed(format!("failed to remove {}: {error}", path.display())))?;
    sync_parent(path).map_err(|error| {
        InstallError::RecoveryRequired(format!(
            "removed {}, but its parent directory could not be made durable: {error}",
            path.display()
        ))
    })
}

fn reject_link_or_reparse_ancestors(path: &Path) -> Result<(), InstallError> {
    let mut current = path.parent();
    while let Some(ancestor) = current {
        if ancestor.as_os_str().is_empty() {
            break;
        }
        match fs::symlink_metadata(ancestor) {
            Ok(metadata)
                if metadata.is_dir() && !runtime_metadata_is_link_or_reparse(&metadata) => {}
            Ok(_) => {
                return Err(failed(format!(
                    "bundle target crosses a symbolic link, reparse point, or non-directory ancestor: {}",
                    ancestor.display()
                )));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(failed(format!(
                    "failed to inspect bundle target ancestor {}: {error}",
                    ancestor.display()
                )));
            }
        }
        current = ancestor.parent();
    }
    Ok(())
}

#[cfg(windows)]
fn durable_rename(source: &Path, destination: &Path) -> Result<(), InstallError> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{MOVEFILE_WRITE_THROUGH, MoveFileExW};

    let source_wide = source
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let destination_wide = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let succeeded = unsafe {
        MoveFileExW(
            source_wide.as_ptr(),
            destination_wide.as_ptr(),
            MOVEFILE_WRITE_THROUGH,
        )
    };
    if succeeded == 0 {
        Err(failed(format!(
            "failed write-through rename from {} to {}: {}",
            source.display(),
            destination.display(),
            io::Error::last_os_error()
        )))
    } else {
        Ok(())
    }
}

#[cfg(not(windows))]
fn durable_rename(source: &Path, destination: &Path) -> Result<(), InstallError> {
    fs::rename(source, destination).map_err(|error| {
        failed(format!(
            "failed rename from {} to {}: {error}",
            source.display(),
            destination.display()
        ))
    })
}

#[cfg(unix)]
fn sync_parent(path: &Path) -> Result<(), InstallError> {
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    let directory = File::open(parent)
        .map_err(|error| failed(format!("failed to open {}: {error}", parent.display())))?;
    directory
        .sync_all()
        .map_err(|error| failed(format!("failed to sync {}: {error}", parent.display())))
}

#[cfg(not(unix))]
fn sync_parent(_path: &Path) -> Result<(), InstallError> {
    // Namespace mutations use `durable_rename`, which requests
    // MOVEFILE_WRITE_THROUGH on Windows. This helper remains for Unix removal
    // durability and for call sites that already performed that rename.
    Ok(())
}

fn emit_progress(
    progress: &dyn Fn(InstallProgress),
    stage: InstallStage,
    completed_bytes: u64,
    total_bytes: u64,
    started_at: Instant,
    measured_at: Instant,
    request_start_bytes: u64,
) {
    let elapsed = measured_at.duration_since(started_at).as_secs_f64();
    let transferred = completed_bytes.saturating_sub(request_start_bytes);
    progress(InstallProgress {
        stage,
        completed_bytes,
        total_bytes,
        bytes_per_second: if transferred == 0 || elapsed <= 0.0 {
            None
        } else {
            Some((transferred as f64 / elapsed).round() as u64)
        },
        download_activity: None,
    });
}

fn emit_progress_activity(
    progress: &dyn Fn(InstallProgress),
    stage: InstallStage,
    completed_bytes: u64,
    total_bytes: u64,
    bytes_per_second: Option<u64>,
    download_activity: DownloadActivity,
) {
    progress(InstallProgress {
        stage,
        completed_bytes,
        total_bytes,
        bytes_per_second,
        download_activity: Some(download_activity),
    });
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ResponseDisposition {
    Restart,
    Append,
    CompletePartial,
}

fn validate_download_response(
    response: &HttpResponse,
    offset: u64,
    expected_total: u64,
) -> Result<ResponseDisposition, InstallError> {
    let content_length = strict_header_u64(&response.content_lengths, "Content-Length")?;
    let content_range = strict_header(&response.content_ranges, "Content-Range")?;
    if strict_header(&response.content_encodings, "Content-Encoding")?
        .is_some_and(|value| !value.eq_ignore_ascii_case("identity"))
    {
        return Err(failed(
            "artifact response used a transformed Content-Encoding instead of identity",
        ));
    }
    match (offset, response.status) {
        (0, 200) => {
            validate_content_length(content_length, expected_total)?;
            Ok(ResponseDisposition::Restart)
        }
        (0, 206) => Err(failed("server returned 206 without a Range request")),
        (_, 206) => {
            let range = content_range
                .and_then(parse_content_range)
                .ok_or_else(|| failed("resume response has no valid Content-Range"))?;
            if range.start != offset
                || range.total != expected_total
                || range.start > range.end
                || range.end >= range.total
                || range.end != expected_total.saturating_sub(1)
            {
                return Err(failed(
                    "resume response Content-Range does not match the partial",
                ));
            }
            let response_bytes = range
                .end
                .checked_sub(range.start)
                .and_then(|value| value.checked_add(1))
                .ok_or_else(|| failed("resume response Content-Range overflow"))?;
            validate_content_length(content_length, response_bytes)?;
            Ok(ResponseDisposition::Append)
        }
        (_, 200) => {
            // The server ignored Range. Restart from the verified beginning;
            // never append a full response to an existing partial.
            validate_content_length(content_length, expected_total)?;
            Ok(ResponseDisposition::Restart)
        }
        (_, 416) if offset == expected_total => Ok(ResponseDisposition::CompletePartial),
        (_, status) => Err(failed(format!(
            "download returned HTTP {status}; resumable partial was preserved"
        ))),
    }
}

fn strict_header<'a>(values: &'a [String], name: &str) -> Result<Option<&'a str>, InstallError> {
    match values {
        [] => Ok(None),
        [value] => Ok(Some(value.as_str())),
        _ => Err(failed(format!("response has duplicate {name} headers"))),
    }
}

fn strict_header_u64(values: &[String], name: &str) -> Result<Option<u64>, InstallError> {
    strict_header(values, name)?
        .map(|value| {
            value
                .parse::<u64>()
                .map_err(|_| failed(format!("response has malformed {name}")))
        })
        .transpose()
}

fn validate_content_length(actual: Option<u64>, expected: u64) -> Result<(), InstallError> {
    if actual.is_some_and(|actual| actual != expected) {
        Err(failed(format!(
            "response Content-Length mismatch: expected {expected}, got {}",
            actual.unwrap_or_default()
        )))
    } else {
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ContentRange {
    start: u64,
    end: u64,
    total: u64,
}

fn parse_content_range(value: &str) -> Option<ContentRange> {
    let value = value.strip_prefix("bytes ")?;
    let (range, total) = value.split_once('/')?;
    let (start, end) = range.split_once('-')?;
    Some(ContentRange {
        start: start.parse().ok()?,
        end: end.parse().ok()?,
        total: total.parse().ok()?,
    })
}

struct HttpResponse {
    status: u16,
    content_lengths: Vec<String>,
    content_ranges: Vec<String>,
    content_encodings: Vec<String>,
    reader: Box<dyn Read + Send + Sync>,
}

#[derive(Debug)]
enum HttpSourceError {
    Retryable(DownloadFaultKind),
    Fatal(InstallError),
}

impl std::fmt::Display for HttpSourceError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Retryable(cause) => cause.fmt(formatter),
            Self::Fatal(error) => error.fmt(formatter),
        }
    }
}

trait HttpSource {
    fn get(
        &self,
        url: &str,
        range_start: Option<u64>,
        read_poll_interval: Duration,
    ) -> Result<HttpResponse, HttpSourceError>;
}

struct UreqHttpSource;

impl HttpSource for UreqHttpSource {
    fn get(
        &self,
        url: &str,
        range_start: Option<u64>,
        read_poll_interval: Duration,
    ) -> Result<HttpResponse, HttpSourceError> {
        let policy = redirect_policy_for_initial_url(url).map_err(HttpSourceError::Fatal)?;
        self.get_with_redirect_policy(url, range_start, policy, read_poll_interval)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RedirectPolicy {
    Standard,
    HuggingFace,
}

impl UreqHttpSource {
    fn get_with_redirect_policy(
        &self,
        url: &str,
        range_start: Option<u64>,
        policy: RedirectPolicy,
        read_poll_interval: Duration,
    ) -> Result<HttpResponse, HttpSourceError> {
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(10))
            .timeout_read(read_poll_interval)
            .redirects(0)
            .https_only(!cfg!(test))
            .build();
        let mut current = Url::parse(url)
            .map_err(|_| HttpSourceError::Fatal(failed("invalid pinned download URL")))?;
        for redirects in 0..=MAX_DOWNLOAD_REDIRECTS {
            let mut request = agent.get(current.as_str());
            request = request.set("Accept-Encoding", "identity");
            if let Some(start) = range_start {
                request = request.set("Range", &format!("bytes={start}-"));
            }
            let response = match request.call() {
                Ok(response) => response,
                Err(ureq::Error::Status(_, response)) => response,
                Err(ureq::Error::Transport(error)) => {
                    return Err(classify_ureq_transport(&error));
                }
            };
            let status = response.status();
            if is_redirect_status(status) {
                if redirects == MAX_DOWNLOAD_REDIRECTS {
                    return Err(HttpSourceError::Fatal(failed(format!(
                        "download exceeded {MAX_DOWNLOAD_REDIRECTS} validated redirects"
                    ))));
                }
                let location =
                    strict_ureq_header(&response, "Location").map_err(HttpSourceError::Fatal)?;
                let location = location.ok_or_else(|| {
                    HttpSourceError::Fatal(failed(
                        "download redirect response has no Location header",
                    ))
                })?;
                current = validated_redirect_url(&current, location, policy)
                    .map_err(HttpSourceError::Fatal)?;
                continue;
            }
            let content_lengths = response
                .all("Content-Length")
                .into_iter()
                .map(str::to_owned)
                .collect();
            let content_ranges = response
                .all("Content-Range")
                .into_iter()
                .map(str::to_owned)
                .collect();
            let content_encodings = response
                .all("Content-Encoding")
                .into_iter()
                .map(str::to_owned)
                .collect();
            return Ok(HttpResponse {
                status,
                content_lengths,
                content_ranges,
                content_encodings,
                reader: response.into_reader(),
            });
        }
        Err(HttpSourceError::Fatal(failed(
            "download redirect handling ended unexpectedly",
        )))
    }
}

fn classify_ureq_transport(error: &ureq::Transport) -> HttpSourceError {
    use std::error::Error as _;

    let retryable = match error.kind() {
        ureq::ErrorKind::Dns => Some(DownloadFaultKind::Dns),
        ureq::ErrorKind::ConnectionFailed | ureq::ErrorKind::ProxyConnect => {
            Some(DownloadFaultKind::ConnectionFailed)
        }
        ureq::ErrorKind::Io => error
            .source()
            .and_then(|source| source.downcast_ref::<io::Error>())
            .and_then(|source| match source.kind() {
                io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock => {
                    Some(DownloadFaultKind::TimedOut)
                }
                io::ErrorKind::ConnectionReset
                | io::ErrorKind::ConnectionAborted
                | io::ErrorKind::BrokenPipe => Some(DownloadFaultKind::ConnectionReset),
                _ => None,
            }),
        _ => None,
    };
    retryable.map_or_else(
        || {
            HttpSourceError::Fatal(failed(
                "download request was rejected before a trusted response was received",
            ))
        },
        HttpSourceError::Retryable,
    )
}

fn redirect_policy_for_initial_url(url: &str) -> Result<RedirectPolicy, InstallError> {
    let url =
        Url::parse(url).map_err(|error| failed(format!("invalid pinned download URL: {error}")))?;
    Ok(if is_hugging_face_host(url.host_str()) {
        validate_hugging_face_download_url(&url)?;
        RedirectPolicy::HuggingFace
    } else {
        RedirectPolicy::Standard
    })
}

fn is_redirect_status(status: u16) -> bool {
    matches!(status, 301 | 302 | 303 | 307 | 308)
}

fn strict_ureq_header<'a>(
    response: &'a ureq::Response,
    name: &str,
) -> Result<Option<&'a str>, InstallError> {
    let values = response.all(name);
    match values.len() {
        0 => Ok(None),
        1 => Ok(values.first().copied()),
        _ => Err(failed(format!(
            "download response has duplicate {name} headers"
        ))),
    }
}

fn validated_redirect_url(
    current: &Url,
    location: &str,
    policy: RedirectPolicy,
) -> Result<Url, InstallError> {
    let next = current
        .join(location)
        .map_err(|error| failed(format!("download redirect Location is invalid: {error}")))?;
    if next.scheme() != "https" || !next.username().is_empty() || next.password().is_some() {
        return Err(failed(
            "download redirect must use a credential-free HTTPS URL",
        ));
    }
    if policy == RedirectPolicy::HuggingFace {
        validate_hugging_face_download_url(&next)?;
    }
    Ok(next)
}

fn validate_hugging_face_download_url(url: &Url) -> Result<(), InstallError> {
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port_or_known_default() != Some(443)
        || !is_hugging_face_host(url.host_str())
    {
        return Err(failed(
            "trusted Hugging Face downloads may redirect only to approved HTTPS hosts",
        ));
    }
    Ok(())
}

fn is_hugging_face_host(host: Option<&str>) -> bool {
    host.is_some_and(|host| {
        let host = host.to_ascii_lowercase();
        host == "huggingface.co"
            || host.ends_with(".huggingface.co")
            || host == "hf.co"
            || host.ends_with(".hf.co")
    })
}

fn failed(message: impl Into<String>) -> InstallError {
    InstallError::Failed(message.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::io::Cursor;
    use std::net::TcpListener;
    use std::sync::{Mutex, mpsc};
    use std::thread;

    #[derive(Clone)]
    struct FakeReply {
        status: u16,
        content_range: Option<String>,
        bytes: Vec<u8>,
    }

    struct FakeHttp {
        reply: FakeReply,
        requested_ranges: Mutex<Vec<Option<u64>>>,
    }

    impl HttpSource for FakeHttp {
        fn get(
            &self,
            _url: &str,
            range_start: Option<u64>,
            _read_poll_interval: Duration,
        ) -> Result<HttpResponse, HttpSourceError> {
            self.requested_ranges.lock().unwrap().push(range_start);
            Ok(HttpResponse {
                status: self.reply.status,
                content_lengths: vec![self.reply.bytes.len().to_string()],
                content_ranges: self.reply.content_range.clone().into_iter().collect(),
                content_encodings: Vec::new(),
                reader: Box::new(Cursor::new(self.reply.bytes.clone())),
            })
        }
    }

    struct HookHttp {
        reply: FakeReply,
        hook: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    }

    enum ReadAction {
        Bytes(Vec<u8>),
        Error(io::ErrorKind),
        ErrorAfter(Duration, io::ErrorKind),
        Eof,
    }

    struct ScriptedReader {
        actions: VecDeque<ReadAction>,
    }

    impl Read for ScriptedReader {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            match self.actions.pop_front().unwrap_or(ReadAction::Eof) {
                ReadAction::Bytes(bytes) => {
                    assert!(bytes.len() <= buffer.len());
                    buffer[..bytes.len()].copy_from_slice(&bytes);
                    Ok(bytes.len())
                }
                ReadAction::Error(kind) => Err(io::Error::from(kind)),
                ReadAction::ErrorAfter(delay, kind) => {
                    thread::sleep(delay);
                    Err(io::Error::from(kind))
                }
                ReadAction::Eof => Ok(0),
            }
        }
    }

    enum ScriptedAttempt {
        Response {
            status: u16,
            content_length: Option<u64>,
            content_range: Option<String>,
            actions: Vec<ReadAction>,
        },
        Retryable(DownloadFaultKind),
        Fatal(&'static str),
    }

    struct ScriptedHttp {
        attempts: Mutex<VecDeque<ScriptedAttempt>>,
        requested_ranges: Mutex<Vec<Option<u64>>>,
    }

    impl ScriptedHttp {
        fn new(attempts: impl IntoIterator<Item = ScriptedAttempt>) -> Self {
            Self {
                attempts: Mutex::new(attempts.into_iter().collect()),
                requested_ranges: Mutex::new(Vec::new()),
            }
        }
    }

    impl HttpSource for ScriptedHttp {
        fn get(
            &self,
            _url: &str,
            range_start: Option<u64>,
            _read_poll_interval: Duration,
        ) -> Result<HttpResponse, HttpSourceError> {
            self.requested_ranges.lock().unwrap().push(range_start);
            match self
                .attempts
                .lock()
                .unwrap()
                .pop_front()
                .expect("scripted HTTP attempt")
            {
                ScriptedAttempt::Response {
                    status,
                    content_length,
                    content_range,
                    actions,
                } => Ok(HttpResponse {
                    status,
                    content_lengths: content_length
                        .map(|length| length.to_string())
                        .into_iter()
                        .collect(),
                    content_ranges: content_range.into_iter().collect(),
                    content_encodings: Vec::new(),
                    reader: Box::new(ScriptedReader {
                        actions: actions.into(),
                    }),
                }),
                ScriptedAttempt::Retryable(cause) => Err(HttpSourceError::Retryable(cause)),
                ScriptedAttempt::Fatal(message) => Err(HttpSourceError::Fatal(failed(message))),
            }
        }
    }

    fn fast_retry_policy() -> DownloadRetryPolicy {
        DownloadRetryPolicy {
            read_poll_interval: Duration::from_millis(1),
            stalled_after: Duration::from_millis(1),
            reconnect_after: Duration::from_millis(20),
            retry_backoffs: [Duration::ZERO; 3],
            cancellation_poll_interval: Duration::from_millis(1),
        }
    }

    fn fail_download_reader_spawn(
        reader: Box<dyn Read + Send + Sync>,
    ) -> Result<PendingDownloadReader, InstallError> {
        drop(reader);
        Err(failed("injected download reader spawn failure"))
    }

    #[test]
    fn standard_retry_policy_matches_the_bounded_transport_contract() {
        let policy = DownloadRetryPolicy::STANDARD;
        assert_eq!(policy.read_poll_interval, Duration::from_millis(750));
        assert_eq!(policy.stalled_after, Duration::from_secs(3));
        assert_eq!(policy.reconnect_after, Duration::from_secs(15));
        assert_eq!(
            policy.retry_backoffs,
            [
                Duration::from_secs(1),
                Duration::from_secs(2),
                Duration::from_secs(4),
            ]
        );
        assert_eq!(
            policy.cancellation_poll_interval,
            Duration::from_millis(100)
        );
        assert_eq!(policy.max_retries(), 3);
    }

    #[test]
    fn stalled_download_emits_transition_only_activity_and_recovers_with_a_range() {
        let root = unique_root("stalled-transition");
        fs::create_dir_all(&root).unwrap();
        let bytes = b"complete artifact";
        let prefix = 5;
        let spec = artifact(&root, bytes);
        let mut first_actions = vec![ReadAction::Bytes(bytes[..prefix].to_vec())];
        first_actions.extend([
            ReadAction::ErrorAfter(Duration::from_millis(2), io::ErrorKind::TimedOut),
            ReadAction::ErrorAfter(Duration::from_millis(2), io::ErrorKind::WouldBlock),
            ReadAction::ErrorAfter(Duration::from_millis(2), io::ErrorKind::TimedOut),
        ]);
        let source = ScriptedHttp::new([
            ScriptedAttempt::Response {
                status: 200,
                content_length: Some(bytes.len() as u64),
                content_range: None,
                actions: first_actions,
            },
            ScriptedAttempt::Response {
                status: 206,
                content_length: Some((bytes.len() - prefix) as u64),
                content_range: Some(format!(
                    "bytes {prefix}-{}/{}",
                    bytes.len() - 1,
                    bytes.len()
                )),
                actions: vec![ReadAction::Bytes(bytes[prefix..].to_vec()), ReadAction::Eof],
            },
        ]);
        let updates = Mutex::new(Vec::new());
        let mut policy = fast_retry_policy();
        policy.reconnect_after = Duration::from_millis(5);

        let candidate = download_pinned_artifact_with_policy(
            &source,
            &spec,
            &InstallCancellation::default(),
            &|update| updates.lock().unwrap().push(update),
            policy,
        )
        .unwrap();

        let activities = updates
            .lock()
            .unwrap()
            .iter()
            .filter_map(|update| update.download_activity)
            .collect::<Vec<_>>();
        assert_eq!(activities[0], DownloadActivity::Active);
        assert_eq!(activities[1], DownloadActivity::Stalled);
        assert!(matches!(
            activities[2],
            DownloadActivity::RetryScheduled {
                attempt: 1,
                max_attempts: 3,
                cause: DownloadFaultKind::Stalled,
                ..
            }
        ));
        assert_eq!(activities[3], DownloadActivity::Active);
        assert_eq!(activities.len(), 4);
        assert_eq!(
            source.requested_ranges.lock().unwrap().as_slice(),
            &[None, Some(prefix as u64)]
        );
        assert_eq!(fs::read(candidate.path).unwrap(), bytes);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn ranged_retry_exhaustion_returns_sanitized_typed_failure() {
        let root = unique_root("retry-exhaustion-username-marker-alice");
        fs::create_dir_all(&root).unwrap();
        let bytes = b"complete artifact";
        let prefix = 4;
        let spec = artifact(&root, bytes);
        let source = ScriptedHttp::new([
            ScriptedAttempt::Response {
                status: 200,
                content_length: Some(bytes.len() as u64),
                content_range: None,
                actions: vec![
                    ReadAction::Bytes(bytes[..prefix].to_vec()),
                    ReadAction::Error(io::ErrorKind::ConnectionReset),
                ],
            },
            ScriptedAttempt::Retryable(DownloadFaultKind::ConnectionFailed),
            ScriptedAttempt::Retryable(DownloadFaultKind::ConnectionFailed),
            ScriptedAttempt::Retryable(DownloadFaultKind::ConnectionFailed),
        ]);

        let error = download_pinned_artifact_with_policy(
            &source,
            &spec,
            &InstallCancellation::default(),
            &|_| {},
            fast_retry_policy(),
        )
        .unwrap_err();

        assert!(matches!(
            error,
            InstallError::DownloadFailed {
                category: DownloadFaultKind::ConnectionFailed,
                retry_count: 3,
                retained_bytes,
                partial_path: ref retained_path,
            } if retained_bytes == prefix as u64
                && retained_path == &partial_path(&spec.destination).unwrap()
        ));
        let message = error.to_string();
        assert_eq!(
            message,
            "download failed after 3 ranged retries (connection failed); 4 bytes retained"
        );
        assert!(message.contains("connection failed"));
        assert!(!message.contains(&spec.url));
        assert!(!message.contains('?'));
        let retained_path = partial_path(&spec.destination).unwrap();
        assert!(!message.contains(retained_path.to_string_lossy().as_ref()));
        assert!(!message.contains("username-marker-alice"));
        assert_eq!(
            source.requested_ranges.lock().unwrap().as_slice(),
            &[None, Some(4), Some(4), Some(4)]
        );
        assert_eq!(
            DownloadFaultKind::ConnectionFailed.diagnostic_label(),
            "connection_failed"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn ignored_range_preserves_existing_partial_when_reader_spawn_fails() {
        let root = unique_root("ignored-range-reader-spawn-failure");
        fs::create_dir_all(&root).unwrap();
        let bytes = b"complete artifact";
        let retained = &bytes[..7];
        let spec = artifact(&root, bytes);
        let partial = partial_path(&spec.destination).unwrap();
        fs::write(&partial, retained).unwrap();
        let source = ScriptedHttp::new([ScriptedAttempt::Response {
            status: 200,
            content_length: Some(bytes.len() as u64),
            content_range: None,
            actions: vec![ReadAction::Bytes(bytes.to_vec()), ReadAction::Eof],
        }]);

        let error = download_pinned_artifact_with_policy_and_reader_spawner(
            &source,
            &spec,
            &InstallCancellation::default(),
            &|_| {},
            fast_retry_policy(),
            fail_download_reader_spawn,
        )
        .unwrap_err();

        assert_eq!(error.to_string(), "injected download reader spawn failure");
        assert_eq!(fs::read(&partial).unwrap(), retained);
        assert_eq!(
            source.requested_ranges.lock().unwrap().as_slice(),
            &[Some(retained.len() as u64)]
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn retryable_http_statuses_reconnect_and_complete() {
        for status in [408, 429, 500, 502, 503, 504] {
            let root = unique_root(&format!("retry-status-{status}"));
            fs::create_dir_all(&root).unwrap();
            let bytes = b"complete artifact";
            let spec = artifact(&root, bytes);
            let source = ScriptedHttp::new([
                ScriptedAttempt::Response {
                    status,
                    content_length: None,
                    content_range: None,
                    actions: vec![ReadAction::Eof],
                },
                ScriptedAttempt::Response {
                    status: 200,
                    content_length: Some(bytes.len() as u64),
                    content_range: None,
                    actions: vec![ReadAction::Bytes(bytes.to_vec()), ReadAction::Eof],
                },
            ]);
            let updates = Mutex::new(Vec::new());

            let candidate = download_pinned_artifact_with_policy(
                &source,
                &spec,
                &InstallCancellation::default(),
                &|update| updates.lock().unwrap().push(update),
                fast_retry_policy(),
            )
            .unwrap();

            assert!(updates.lock().unwrap().iter().any(|update| matches!(
                update.download_activity,
                Some(DownloadActivity::RetryScheduled {
                    cause: DownloadFaultKind::HttpStatus(actual),
                    ..
                }) if actual == status
            )));
            assert_eq!(fs::read(candidate.path).unwrap(), bytes);
            fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn premature_eof_retries_from_the_exact_retained_offset() {
        let root = unique_root("premature-eof");
        fs::create_dir_all(&root).unwrap();
        let bytes = b"complete artifact";
        let prefix = 6;
        let spec = artifact(&root, bytes);
        let source = ScriptedHttp::new([
            ScriptedAttempt::Response {
                status: 200,
                content_length: Some(bytes.len() as u64),
                content_range: None,
                actions: vec![ReadAction::Bytes(bytes[..prefix].to_vec()), ReadAction::Eof],
            },
            ScriptedAttempt::Response {
                status: 206,
                content_length: Some((bytes.len() - prefix) as u64),
                content_range: Some(format!(
                    "bytes {prefix}-{}/{}",
                    bytes.len() - 1,
                    bytes.len()
                )),
                actions: vec![ReadAction::Bytes(bytes[prefix..].to_vec()), ReadAction::Eof],
            },
        ]);

        let candidate = download_pinned_artifact_with_policy(
            &source,
            &spec,
            &InstallCancellation::default(),
            &|_| {},
            fast_retry_policy(),
        )
        .unwrap();

        assert_eq!(
            source.requested_ranges.lock().unwrap().as_slice(),
            &[None, Some(prefix as u64)]
        );
        assert_eq!(fs::read(candidate.path).unwrap(), bytes);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn retryable_read_interruptions_resume_without_losing_bytes() {
        for kind in [
            io::ErrorKind::ConnectionReset,
            io::ErrorKind::ConnectionAborted,
            io::ErrorKind::BrokenPipe,
        ] {
            let root = unique_root(&format!("retry-read-{kind:?}"));
            fs::create_dir_all(&root).unwrap();
            let bytes = b"complete artifact";
            let prefix = 3;
            let spec = artifact(&root, bytes);
            let source = ScriptedHttp::new([
                ScriptedAttempt::Response {
                    status: 200,
                    content_length: Some(bytes.len() as u64),
                    content_range: None,
                    actions: vec![
                        ReadAction::Bytes(bytes[..prefix].to_vec()),
                        ReadAction::Error(kind),
                    ],
                },
                ScriptedAttempt::Response {
                    status: 206,
                    content_length: Some((bytes.len() - prefix) as u64),
                    content_range: Some(format!(
                        "bytes {prefix}-{}/{}",
                        bytes.len() - 1,
                        bytes.len()
                    )),
                    actions: vec![ReadAction::Bytes(bytes[prefix..].to_vec()), ReadAction::Eof],
                },
            ]);

            let candidate = download_pinned_artifact_with_policy(
                &source,
                &spec,
                &InstallCancellation::default(),
                &|_| {},
                fast_retry_policy(),
            )
            .unwrap();

            assert_eq!(
                source.requested_ranges.lock().unwrap().as_slice(),
                &[None, Some(prefix as u64)]
            );
            assert_eq!(fs::read(candidate.path).unwrap(), bytes);
            fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn nonretryable_status_and_request_validation_faults_fail_once() {
        let root = unique_root("fatal-no-retry");
        fs::create_dir_all(&root).unwrap();
        let bytes = b"complete artifact";
        let spec = artifact(&root, bytes);
        let status_source = ScriptedHttp::new([ScriptedAttempt::Response {
            status: 404,
            content_length: None,
            content_range: None,
            actions: vec![ReadAction::Eof],
        }]);

        let error = download_pinned_artifact_with_policy(
            &status_source,
            &spec,
            &InstallCancellation::default(),
            &|_| {},
            fast_retry_policy(),
        )
        .unwrap_err();
        assert!(matches!(error, InstallError::Failed(_)));
        assert_eq!(status_source.requested_ranges.lock().unwrap().len(), 1);

        let fatal_source = ScriptedHttp::new([ScriptedAttempt::Fatal("request rejected")]);
        let error = download_pinned_artifact_with_policy(
            &fatal_source,
            &spec,
            &InstallCancellation::default(),
            &|_| {},
            fast_retry_policy(),
        )
        .unwrap_err();
        assert_eq!(error.to_string(), "request rejected");
        assert_eq!(fatal_source.requested_ranges.lock().unwrap().len(), 1);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn cancellation_interrupts_retry_backoff_without_another_request() {
        let root = unique_root("cancel-backoff");
        fs::create_dir_all(&root).unwrap();
        let spec = artifact(&root, b"complete artifact");
        let source = ScriptedHttp::new([ScriptedAttempt::Retryable(DownloadFaultKind::Dns)]);
        let cancellation = InstallCancellation::default();
        let mut policy = fast_retry_policy();
        policy.retry_backoffs = [Duration::from_secs(1); 3];
        policy.cancellation_poll_interval = Duration::from_millis(5);
        let started_at = Instant::now();

        let error = download_pinned_artifact_with_policy(
            &source,
            &spec,
            &cancellation,
            &|update| {
                if matches!(
                    update.download_activity,
                    Some(DownloadActivity::RetryScheduled { .. })
                ) {
                    cancellation.cancel();
                }
            },
            policy,
        )
        .unwrap_err();

        assert!(error.is_cancelled());
        assert!(started_at.elapsed() < Duration::from_millis(100));
        assert_eq!(source.requested_ranges.lock().unwrap().len(), 1);
        fs::remove_dir_all(root).unwrap();
    }

    impl HttpSource for HookHttp {
        fn get(
            &self,
            _url: &str,
            _range_start: Option<u64>,
            _read_poll_interval: Duration,
        ) -> Result<HttpResponse, HttpSourceError> {
            if let Some(hook) = self.hook.lock().unwrap().take() {
                hook();
            }
            Ok(HttpResponse {
                status: self.reply.status,
                content_lengths: vec![self.reply.bytes.len().to_string()],
                content_ranges: self.reply.content_range.clone().into_iter().collect(),
                content_encodings: Vec::new(),
                reader: Box::new(Cursor::new(self.reply.bytes.clone())),
            })
        }
    }

    fn unique_root(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "scribe-installation-{name}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    #[test]
    fn disk_space_preflight_accounts_for_resumable_partial_bytes() {
        assert_eq!(additional_download_bytes(100, 0).unwrap(), 100);
        assert_eq!(additional_download_bytes(100, 40).unwrap(), 60);
        assert_eq!(additional_download_bytes(100, 100).unwrap(), 0);
        assert_eq!(additional_download_bytes(100, 101).unwrap(), 100);
    }

    #[test]
    fn bundle_download_accounting_distinguishes_cache_partial_and_replacement() {
        let root = unique_root("bundle-accounting");
        fs::create_dir_all(&root).unwrap();
        let bytes = b"complete artifact";
        let spec = artifact(&root, bytes);

        assert_eq!(
            pinned_artifact_required_download_bytes(&spec).unwrap(),
            bytes.len() as u64
        );
        fs::write(partial_path(&spec.destination).unwrap(), &bytes[..5]).unwrap();
        assert_eq!(
            pinned_artifact_required_download_bytes(&spec).unwrap(),
            (bytes.len() - 5) as u64
        );
        fs::write(partial_path(&spec.destination).unwrap(), bytes).unwrap();
        assert_eq!(
            pinned_artifact_required_download_bytes(&spec).unwrap(),
            bytes.len() as u64,
            "a full partial reserves a clean replacement"
        );
        fs::write(
            partial_path(&spec.destination).unwrap(),
            b"oversized invalid partial",
        )
        .unwrap();
        assert_eq!(
            pinned_artifact_required_download_bytes(&spec).unwrap(),
            bytes.len() as u64
        );
        fs::write(&spec.destination, bytes).unwrap();
        assert_eq!(pinned_artifact_required_download_bytes(&spec).unwrap(), 0);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn verified_destination_inspection_is_consumed_without_a_second_hash() {
        let root = unique_root("verified-cache-single-hash");
        fs::create_dir_all(&root).unwrap();
        let bytes = b"complete artifact";
        let spec = artifact(&root, bytes);
        fs::write(&spec.destination, bytes).unwrap();
        let identity = disk_space::canonical_target_identity(&spec.destination).unwrap();
        reset_file_hash_count(&spec.destination);

        let inspection =
            inspect_pinned_artifact_for_target(&spec, &identity, &InstallCancellation::default())
                .unwrap();
        assert_eq!(inspection.required_download_bytes(), 0);
        assert_eq!(file_hash_count(&spec.destination), 1);

        let downloaded = download_pinned_artifact_for_target(
            &spec,
            &identity,
            Some(inspection),
            &InstallCancellation::default(),
            &|_| {},
        )
        .unwrap();
        assert_eq!(downloaded.path, spec.destination);
        assert_eq!(file_hash_count(&downloaded.path), 1);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn verified_destination_replacement_after_inspection_fails_closed() {
        let root = unique_root("verified-cache-replaced");
        fs::create_dir_all(&root).unwrap();
        let bytes = b"complete artifact";
        let spec = artifact(&root, bytes);
        fs::write(&spec.destination, bytes).unwrap();
        let identity = disk_space::canonical_target_identity(&spec.destination).unwrap();
        reset_file_hash_count(&spec.destination);
        let inspection =
            inspect_pinned_artifact_for_target(&spec, &identity, &InstallCancellation::default())
                .unwrap();
        fs::rename(&spec.destination, root.join("original.bin")).unwrap();
        fs::write(&spec.destination, bytes).unwrap();

        let error = download_pinned_artifact_for_target(
            &spec,
            &identity,
            Some(inspection),
            &InstallCancellation::default(),
            &|_| {},
        )
        .unwrap_err();

        assert!(error.to_string().contains("changed after inspection"));
        assert_eq!(file_hash_count(&spec.destination), 1);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn pinned_inspection_honors_pre_cancel_without_touching_artifact_paths() {
        let root = unique_root("inspection-pre-cancel");
        fs::create_dir_all(&root).unwrap();
        let spec = artifact(&root, b"complete artifact");
        let identity = disk_space::canonical_target_identity(&spec.destination).unwrap();
        let cancellation = InstallCancellation::default();
        cancellation.cancel();

        let error =
            inspect_pinned_artifact_for_target(&spec, &identity, &cancellation).unwrap_err();

        assert!(error.is_cancelled());
        assert!(!spec.destination.exists());
        assert!(!partial_path(&spec.destination).unwrap().exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn pinned_inspection_cancellation_interrupts_cache_hash_without_mutation() {
        let root = unique_root("inspection-hash-cancel");
        fs::create_dir_all(&root).unwrap();
        let bytes = vec![0x5a; BUFFER_BYTES * 3];
        let spec = artifact(&root, &bytes);
        fs::write(&spec.destination, &bytes).unwrap();
        let identity = disk_space::canonical_target_identity(&spec.destination).unwrap();
        let cancellation = InstallCancellation::default();
        cancel_file_hash_after(&spec.destination, BUFFER_BYTES as u64, cancellation.clone());

        let error =
            inspect_pinned_artifact_for_target(&spec, &identity, &cancellation).unwrap_err();

        assert!(error.is_cancelled());
        assert_eq!(fs::read(&spec.destination).unwrap(), bytes);
        assert!(!partial_path(&spec.destination).unwrap().exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn same_inode_cache_mutation_fails_during_hashing_copy_without_activation() {
        let root = unique_root("verified-cache-same-inode-mutation");
        fs::create_dir_all(&root).unwrap();
        let expected = b"complete artifact";
        let mutated = b"mutated artifact!";
        assert_eq!(expected.len(), mutated.len());
        let spec = artifact(&root, expected);
        fs::write(&spec.destination, expected).unwrap();
        let identity = disk_space::canonical_target_identity(&spec.destination).unwrap();
        reset_file_hash_count(&spec.destination);
        let inspection =
            inspect_pinned_artifact_for_target(&spec, &identity, &InstallCancellation::default())
                .unwrap();
        let mut same_file = OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(&spec.destination)
            .unwrap();
        same_file.write_all(mutated).unwrap();
        same_file.sync_all().unwrap();
        drop(same_file);

        let downloaded = download_pinned_artifact_for_target(
            &spec,
            &identity,
            Some(inspection),
            &InstallCancellation::default(),
            &|_| {},
        )
        .unwrap();
        assert_eq!(file_hash_count(&spec.destination), 1);
        let target = root.join("assembled");
        let error = stage_file_bundle_for_target(
            &[BundleAssemblyFile {
                source_path: downloaded.path,
                install_path: PathBuf::from("model.bin"),
                size_bytes: spec.size_bytes,
                sha256: spec.sha256,
            }],
            &[],
            &target,
            &InstallCancellation::default(),
            &|_| {},
        )
        .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("checksum mismatch while copying")
        );
        assert!(!target.exists());
        assert!(!stable_staging_path(&target).unwrap().exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn cancellation_precedes_any_download_filesystem_inspection_or_mutation() {
        let root = unique_root("cancel-before-filesystem");
        let bytes = b"complete artifact";
        let spec = artifact(&root, bytes);
        let cancellation = InstallCancellation::default();
        cancellation.cancel();
        let source = FakeHttp {
            reply: FakeReply {
                status: 200,
                content_range: None,
                bytes: bytes.to_vec(),
            },
            requested_ranges: Mutex::new(Vec::new()),
        };

        let error =
            download_pinned_artifact_with(&source, &spec, &cancellation, &|_| {}).unwrap_err();

        assert!(error.is_cancelled());
        assert!(!root.exists());
        assert!(source.requested_ranges.lock().unwrap().is_empty());
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn ancestor_symlink_swap_after_http_admission_is_rejected_before_open() {
        let root = unique_root("ancestor-swap");
        let managed = root.join("managed");
        let moved = root.join("managed-original");
        let external = root.join("external");
        fs::create_dir_all(&managed).unwrap();
        fs::create_dir_all(&external).unwrap();
        let bytes = b"complete artifact";
        let spec = artifact(&managed, bytes);
        let source = HookHttp {
            reply: FakeReply {
                status: 200,
                content_range: None,
                bytes: bytes.to_vec(),
            },
            hook: Mutex::new(Some(Box::new({
                let managed = managed.clone();
                let moved = moved.clone();
                let external = external.clone();
                move || {
                    fs::rename(&managed, &moved).unwrap();
                    #[cfg(unix)]
                    std::os::unix::fs::symlink(&external, &managed).unwrap();
                    #[cfg(windows)]
                    if std::os::windows::fs::symlink_dir(&external, &managed).is_err() {
                        fs::rename(&moved, &managed).unwrap();
                    }
                }
            }))),
        };

        let result =
            download_pinned_artifact_with(&source, &spec, &InstallCancellation::default(), &|_| {});

        #[cfg(windows)]
        if managed.is_dir()
            && !fs::symlink_metadata(&managed)
                .unwrap()
                .file_type()
                .is_symlink()
        {
            fs::remove_dir_all(root).unwrap();
            return;
        }
        let error = result.unwrap_err().to_string();
        assert!(error.contains("symbolic link") || error.contains("target identity changed"));
        assert!(!external.join("fixture.bin.partial").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn partial_symlink_swap_after_response_is_opened_without_following() {
        let root = unique_root("partial-swap");
        fs::create_dir_all(&root).unwrap();
        let bytes = b"complete artifact";
        let spec = artifact(&root, bytes);
        let partial = partial_path(&spec.destination).unwrap();
        let external = root.join("external.bin");
        fs::write(&partial, []).unwrap();
        fs::write(&external, b"do not overwrite").unwrap();
        let source = HookHttp {
            reply: FakeReply {
                status: 200,
                content_range: None,
                bytes: bytes.to_vec(),
            },
            hook: Mutex::new(Some(Box::new({
                let partial = partial.clone();
                let external = external.clone();
                move || {
                    fs::remove_file(&partial).unwrap();
                    #[cfg(unix)]
                    std::os::unix::fs::symlink(&external, &partial).unwrap();
                    #[cfg(windows)]
                    if std::os::windows::fs::symlink_file(&external, &partial).is_err() {
                        fs::write(&partial, []).unwrap();
                    }
                }
            }))),
        };

        let result =
            download_pinned_artifact_with(&source, &spec, &InstallCancellation::default(), &|_| {});

        #[cfg(windows)]
        if fs::symlink_metadata(&partial)
            .unwrap()
            .file_type()
            .is_file()
            && !runtime_metadata_is_link_or_reparse(&fs::symlink_metadata(&partial).unwrap())
        {
            fs::remove_dir_all(root).unwrap();
            return;
        }
        assert!(result.is_err());
        assert_eq!(fs::read(&external).unwrap(), b"do not overwrite");
        fs::remove_dir_all(root).unwrap();
    }

    fn artifact(root: &Path, bytes: &[u8]) -> PinnedArtifact {
        PinnedArtifact {
            id: "fixture".to_owned(),
            url: "https://example.invalid/fixture".to_owned(),
            size_bytes: bytes.len() as u64,
            sha256: format!("{:x}", Sha256::digest(bytes)),
            destination: root.join("fixture.bin"),
        }
    }

    fn serve_once(response: Vec<u8>) -> (String, mpsc::Receiver<String>, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (request_tx, request_rx) = mpsc::channel();
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut request = Vec::new();
            let mut buffer = [0_u8; 1024];
            while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                let count = stream.read(&mut buffer).unwrap();
                if count == 0 {
                    break;
                }
                request.extend_from_slice(&buffer[..count]);
            }
            request_tx
                .send(String::from_utf8_lossy(&request).into_owned())
                .unwrap();
            stream.write_all(&response).unwrap();
        });
        (format!("http://{address}/artifact"), request_rx, handle)
    }

    fn http_response(status: &str, headers: &[(&str, String)], body: &[u8]) -> Vec<u8> {
        let mut response = format!("HTTP/1.1 {status}\r\nConnection: close\r\n");
        for (name, value) in headers {
            response.push_str(name);
            response.push_str(": ");
            response.push_str(value);
            response.push_str("\r\n");
        }
        response.push_str("\r\n");
        let mut bytes = response.into_bytes();
        bytes.extend_from_slice(body);
        bytes
    }

    #[test]
    fn trusted_huggingface_redirect_rejects_a_disallowed_host_from_a_local_server() {
        let response = http_response(
            "302 Found",
            &[("Location", "https://example.invalid/model.gguf".to_owned())],
            &[],
        );
        let (url, request, server) = serve_once(response);

        let error = match UreqHttpSource.get_with_redirect_policy(
            &url,
            None,
            RedirectPolicy::HuggingFace,
            DownloadRetryPolicy::STANDARD.read_poll_interval,
        ) {
            Ok(_) => panic!("disallowed redirect must not return a download response"),
            Err(error) => error,
        };

        assert!(error.to_string().contains("approved HTTPS hosts"));
        assert!(request.recv_timeout(Duration::from_secs(2)).is_ok());
        server.join().unwrap();
    }

    #[test]
    fn trusted_huggingface_redirect_accepts_documented_hub_host_suffixes() {
        let current = Url::parse(
            "https://huggingface.co/handy-computer/example/resolve/0123456789abcdef0123456789abcdef01234567/model.gguf",
        )
        .unwrap();
        for location in [
            "https://cdn-lfs-us-1.hf.co/model.gguf",
            "https://cas-server.xethub-eu.hf.co/reconstruction",
            "https://us.aws.cdn.hf.co/model.gguf",
        ] {
            assert_eq!(
                validated_redirect_url(&current, location, RedirectPolicy::HuggingFace)
                    .unwrap()
                    .host_str(),
                Url::parse(location).unwrap().host_str()
            );
        }
    }

    #[test]
    fn trusted_huggingface_redirect_rejects_insecure_or_credentialed_targets() {
        let current = Url::parse(
            "https://huggingface.co/handy-computer/example/resolve/0123456789abcdef0123456789abcdef01234567/model.gguf",
        )
        .unwrap();
        for location in [
            "http://cdn-lfs-us-1.hf.co/model.gguf",
            "https://user@cdn-lfs-us-1.hf.co/model.gguf",
            "https://cdn-lfs-us-1.hf.co:444/model.gguf",
            "https://hf.co.example.invalid/model.gguf",
        ] {
            assert!(
                validated_redirect_url(&current, location, RedirectPolicy::HuggingFace).is_err(),
                "redirect should be rejected: {location}"
            );
        }
    }

    fn write_zip(path: &Path, entries: &[(&str, &[u8])]) {
        let file = File::create(path).unwrap();
        let mut archive = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        for (name, bytes) in entries {
            archive.start_file(*name, options).unwrap();
            archive.write_all(bytes).unwrap();
        }
        archive.finish().unwrap();
    }

    #[test]
    fn valid_range_resume_appends_only_the_requested_suffix() {
        let root = unique_root("resume");
        fs::create_dir_all(&root).unwrap();
        let bytes = b"complete artifact";
        let spec = artifact(&root, bytes);
        let partial = partial_path(&spec.destination).unwrap();
        fs::write(&partial, &bytes[..8]).unwrap();
        let source = FakeHttp {
            reply: FakeReply {
                status: 206,
                content_range: Some(format!("bytes 8-{}/{}", bytes.len() - 1, bytes.len())),
                bytes: bytes[8..].to_vec(),
            },
            requested_ranges: Mutex::new(Vec::new()),
        };

        let candidate =
            download_pinned_artifact_with(&source, &spec, &InstallCancellation::default(), &|_| {})
                .unwrap();

        assert_eq!(
            source.requested_ranges.lock().unwrap().as_slice(),
            &[Some(8)]
        );
        assert_eq!(fs::read(candidate.path).unwrap(), bytes);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn reused_verified_model_rollback_is_a_noop() {
        let root = unique_root("reused-model-rollback");
        fs::create_dir_all(&root).unwrap();
        let destination = root.join("model.bin");
        let bytes = b"existing verified model";
        fs::write(&destination, bytes).unwrap();
        let target_identity = disk_space::canonical_target_identity(&destination).unwrap();
        let replacement = DownloadedArtifact {
            id: "fixture".to_owned(),
            path: destination.clone(),
            destination: destination.clone(),
            size_bytes: bytes.len() as u64,
            sha256: format!("{:x}", Sha256::digest(bytes)),
            target_identity,
        }
        .activate()
        .unwrap();

        replacement.rollback().unwrap();

        assert_eq!(fs::read(&destination).unwrap(), bytes);
        assert!(!file_rollback_path(&destination).unwrap().exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn reused_verified_model_commit_is_a_noop() {
        let root = unique_root("reused-model-commit");
        fs::create_dir_all(&root).unwrap();
        let destination = root.join("model.bin");
        let bytes = b"existing verified model";
        fs::write(&destination, bytes).unwrap();
        let target_identity = disk_space::canonical_target_identity(&destination).unwrap();
        let replacement = DownloadedArtifact {
            id: "fixture".to_owned(),
            path: destination.clone(),
            destination: destination.clone(),
            size_bytes: bytes.len() as u64,
            sha256: format!("{:x}", Sha256::digest(bytes)),
            target_identity,
        }
        .activate()
        .unwrap();

        replacement.commit().unwrap();

        assert_eq!(fs::read(&destination).unwrap(), bytes);
        assert!(!file_rollback_path(&destination).unwrap().exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn ignored_range_restarts_instead_of_appending_duplicate_bytes() {
        let root = unique_root("ignored-range");
        fs::create_dir_all(&root).unwrap();
        let bytes = b"complete artifact";
        let spec = artifact(&root, bytes);
        fs::write(partial_path(&spec.destination).unwrap(), &bytes[..5]).unwrap();
        let source = FakeHttp {
            reply: FakeReply {
                status: 200,
                content_range: None,
                bytes: bytes.to_vec(),
            },
            requested_ranges: Mutex::new(Vec::new()),
        };

        let candidate =
            download_pinned_artifact_with(&source, &spec, &InstallCancellation::default(), &|_| {})
                .unwrap();

        assert_eq!(fs::read(candidate.path).unwrap(), bytes);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn real_http_resume_sends_range_and_identity_headers() {
        let root = unique_root("real-http-resume");
        fs::create_dir_all(&root).unwrap();
        let bytes = b"complete artifact";
        let offset = 8;
        let response = http_response(
            "206 Partial Content",
            &[
                ("Content-Length", (bytes.len() - offset).to_string()),
                (
                    "Content-Range",
                    format!("bytes {offset}-{}/{}", bytes.len() - 1, bytes.len()),
                ),
            ],
            &bytes[offset..],
        );
        let (url, request, server) = serve_once(response);
        let mut spec = artifact(&root, bytes);
        spec.url = url;
        fs::write(partial_path(&spec.destination).unwrap(), &bytes[..offset]).unwrap();

        let candidate =
            download_pinned_artifact(&spec, &InstallCancellation::default(), &|_| {}).unwrap();
        let request = request.recv_timeout(Duration::from_secs(2)).unwrap();
        server.join().unwrap();

        let request = request.to_ascii_lowercase();
        assert!(request.contains(&format!("range: bytes={offset}-")));
        assert!(request.contains("accept-encoding: identity"));
        assert_eq!(fs::read(candidate.path).unwrap(), bytes);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn real_http_ignored_range_restarts_cleanly() {
        let root = unique_root("real-http-restart");
        fs::create_dir_all(&root).unwrap();
        let bytes = b"complete artifact";
        let response = http_response(
            "200 OK",
            &[("Content-Length", bytes.len().to_string())],
            bytes,
        );
        let (url, request, server) = serve_once(response);
        let mut spec = artifact(&root, bytes);
        spec.url = url;
        fs::write(partial_path(&spec.destination).unwrap(), &bytes[..5]).unwrap();

        let candidate =
            download_pinned_artifact(&spec, &InstallCancellation::default(), &|_| {}).unwrap();
        let request = request.recv_timeout(Duration::from_secs(2)).unwrap();
        server.join().unwrap();

        assert!(request.to_ascii_lowercase().contains("range: bytes=5-"));
        assert_eq!(fs::read(candidate.path).unwrap(), bytes);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn real_http_duplicate_content_range_is_rejected_without_touching_partial() {
        let root = unique_root("real-http-duplicate-range");
        fs::create_dir_all(&root).unwrap();
        let bytes = b"complete artifact";
        let offset = 5;
        let range = format!("bytes {offset}-{}/{}", bytes.len() - 1, bytes.len());
        let response = http_response(
            "206 Partial Content",
            &[
                ("Content-Length", (bytes.len() - offset).to_string()),
                ("Content-Range", range.clone()),
                ("Content-Range", range),
            ],
            &bytes[offset..],
        );
        let (url, _request, server) = serve_once(response);
        let mut spec = artifact(&root, bytes);
        spec.url = url;
        let partial = partial_path(&spec.destination).unwrap();
        fs::write(&partial, &bytes[..offset]).unwrap();

        let error =
            download_pinned_artifact(&spec, &InstallCancellation::default(), &|_| {}).unwrap_err();
        server.join().unwrap();

        assert!(error.to_string().contains("duplicate Content-Range"));
        assert_eq!(fs::read(partial).unwrap(), &bytes[..offset]);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn real_http_malformed_content_range_is_rejected_without_touching_partial() {
        let root = unique_root("real-http-malformed-range");
        fs::create_dir_all(&root).unwrap();
        let bytes = b"complete artifact";
        let offset = 5;
        let response = http_response(
            "206 Partial Content",
            &[
                ("Content-Length", (bytes.len() - offset).to_string()),
                ("Content-Range", "bytes nope".to_owned()),
            ],
            &bytes[offset..],
        );
        let (url, _request, server) = serve_once(response);
        let mut spec = artifact(&root, bytes);
        spec.url = url;
        let partial = partial_path(&spec.destination).unwrap();
        fs::write(&partial, &bytes[..offset]).unwrap();

        let error =
            download_pinned_artifact(&spec, &InstallCancellation::default(), &|_| {}).unwrap_err();
        server.join().unwrap();

        assert!(error.to_string().contains("valid Content-Range"));
        assert_eq!(fs::read(partial).unwrap(), &bytes[..offset]);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn stalled_http_body_acknowledges_cancellation_after_read_timeout() {
        let root = unique_root("real-http-stall-cancel");
        fs::create_dir_all(&root).unwrap();
        let bytes = b"complete artifact";
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 1024];
            let _ = stream.read(&mut request);
            stream
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        bytes.len()
                    )
                    .as_bytes(),
                )
                .unwrap();
            thread::sleep(Duration::from_millis(900));
        });
        let mut spec = artifact(&root, bytes);
        spec.url = format!("http://{address}/artifact");
        let cancellation = InstallCancellation::default();
        let cancellation_trigger = cancellation.clone();
        let cancel = thread::spawn(move || {
            thread::sleep(Duration::from_millis(50));
            cancellation_trigger.cancel();
        });
        let started = Instant::now();

        let error = download_pinned_artifact(&spec, &cancellation, &|_| {}).unwrap_err();

        cancel.join().unwrap();
        server.join().unwrap();
        assert!(error.is_cancelled());
        assert!(started.elapsed() < Duration::from_secs(2));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn complete_partial_accepts_only_exact_416_semantics() {
        let complete = HttpResponse {
            status: 416,
            content_lengths: Vec::new(),
            content_ranges: Vec::new(),
            content_encodings: Vec::new(),
            reader: Box::new(Cursor::new(Vec::<u8>::new())),
        };
        assert_eq!(
            validate_download_response(&complete, 10, 10).unwrap(),
            ResponseDisposition::CompletePartial
        );
        assert!(validate_download_response(&complete, 9, 10).is_err());
    }

    #[test]
    fn checksum_invalid_complete_partial_is_quarantined_before_clean_retry() {
        let root = unique_root("invalid-complete-partial");
        fs::create_dir_all(&root).unwrap();
        let bytes = b"complete artifact";
        let spec = artifact(&root, bytes);
        let partial = partial_path(&spec.destination).unwrap();
        fs::write(&partial, vec![b'x'; bytes.len()]).unwrap();
        let source = FakeHttp {
            reply: FakeReply {
                status: 200,
                content_range: None,
                bytes: bytes.to_vec(),
            },
            requested_ranges: Mutex::new(Vec::new()),
        };

        let candidate =
            download_pinned_artifact_with(&source, &spec, &InstallCancellation::default(), &|_| {})
                .unwrap();

        assert_eq!(fs::read(candidate.path).unwrap(), bytes);
        assert_eq!(
            fs::read(root.join("fixture.bin.partial.invalid")).unwrap(),
            vec![b'x'; bytes.len()]
        );
        assert_eq!(source.requested_ranges.lock().unwrap().as_slice(), &[None]);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn resumed_progress_rate_counts_only_newly_transferred_bytes() {
        let root = unique_root("resume-progress-rate");
        fs::create_dir_all(&root).unwrap();
        let bytes = b"complete artifact";
        let offset = 8;
        let spec = artifact(&root, bytes);
        fs::write(partial_path(&spec.destination).unwrap(), &bytes[..offset]).unwrap();
        let source = FakeHttp {
            reply: FakeReply {
                status: 206,
                content_range: Some(format!(
                    "bytes {offset}-{}/{}",
                    bytes.len() - 1,
                    bytes.len()
                )),
                bytes: bytes[offset..].to_vec(),
            },
            requested_ranges: Mutex::new(Vec::new()),
        };
        let updates = Mutex::new(Vec::new());

        download_pinned_artifact_with(&source, &spec, &InstallCancellation::default(), &|update| {
            updates.lock().unwrap().push(update)
        })
        .unwrap();

        let updates = updates.lock().unwrap();
        assert_eq!(updates[0].completed_bytes, offset as u64);
        assert_eq!(updates[0].bytes_per_second, None);
        let verification = updates
            .iter()
            .find(|update| update.stage == InstallStage::Verifying)
            .unwrap();
        assert_eq!(verification.completed_bytes, bytes.len() as u64);
        assert!(verification.bytes_per_second.is_some());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn mismatched_content_range_preserves_the_partial() {
        let root = unique_root("bad-range");
        fs::create_dir_all(&root).unwrap();
        let bytes = b"complete artifact";
        let spec = artifact(&root, bytes);
        let partial = partial_path(&spec.destination).unwrap();
        fs::write(&partial, &bytes[..5]).unwrap();
        let source = FakeHttp {
            reply: FakeReply {
                status: 206,
                content_range: Some(format!("bytes 4-{}/{}", bytes.len() - 1, bytes.len())),
                bytes: bytes[5..].to_vec(),
            },
            requested_ranges: Mutex::new(Vec::new()),
        };

        let error =
            download_pinned_artifact_with(&source, &spec, &InstallCancellation::default(), &|_| {})
                .unwrap_err();

        assert!(error.to_string().contains("Content-Range"));
        assert_eq!(fs::read(partial).unwrap(), &bytes[..5]);
        fs::remove_dir_all(root).unwrap();
    }

    struct CancellingReader {
        bytes: Cursor<Vec<u8>>,
        cancellation: InstallCancellation,
        reads: usize,
    }

    impl Read for CancellingReader {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            let limit = buffer.len().min(4);
            let count = self.bytes.read(&mut buffer[..limit])?;
            self.reads += 1;
            if self.reads == 1 {
                self.cancellation.cancel();
            }
            Ok(count)
        }
    }

    struct CancellingHttp {
        bytes: Vec<u8>,
        cancellation: InstallCancellation,
    }

    impl HttpSource for CancellingHttp {
        fn get(
            &self,
            _url: &str,
            _range_start: Option<u64>,
            _read_poll_interval: Duration,
        ) -> Result<HttpResponse, HttpSourceError> {
            Ok(HttpResponse {
                status: 200,
                content_lengths: vec![self.bytes.len().to_string()],
                content_ranges: Vec::new(),
                content_encodings: Vec::new(),
                reader: Box::new(CancellingReader {
                    bytes: Cursor::new(self.bytes.clone()),
                    cancellation: self.cancellation.clone(),
                    reads: 0,
                }),
            })
        }
    }

    #[test]
    fn cancellation_retains_a_resumable_partial() {
        let root = unique_root("cancel");
        fs::create_dir_all(&root).unwrap();
        let bytes = b"complete artifact";
        let spec = artifact(&root, bytes);
        let cancellation = InstallCancellation::default();
        let error = download_pinned_artifact_with(
            &CancellingHttp {
                bytes: bytes.to_vec(),
                cancellation: cancellation.clone(),
            },
            &spec,
            &cancellation,
            &|_| {},
        )
        .unwrap_err();

        assert!(error.is_cancelled());
        let partial = partial_path(&spec.destination).unwrap();
        assert_eq!(fs::read(partial).unwrap(), &bytes[..4]);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn partial_cleanup_is_idempotent_and_preserves_the_destination() {
        let root = unique_root("discard-partial");
        fs::create_dir_all(&root).unwrap();
        let spec = artifact(&root, b"complete artifact");
        let partial = partial_path(&spec.destination).unwrap();
        fs::write(&spec.destination, b"complete artifact").unwrap();
        fs::write(&partial, b"resumable bytes").unwrap();

        assert_eq!(
            pinned_artifact_retained_partial(&spec).unwrap(),
            Some(RetainedPartial { bytes: 15 })
        );
        assert!(discard_pinned_artifact_partial(&spec).unwrap());
        assert_eq!(pinned_artifact_retained_partial(&spec).unwrap(), None);
        assert!(!discard_pinned_artifact_partial(&spec).unwrap());
        assert_eq!(fs::read(&spec.destination).unwrap(), b"complete artifact");

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn discarded_partial_restarts_from_zero_bytes() {
        let root = unique_root("discard-partial-restart");
        fs::create_dir_all(&root).unwrap();
        let bytes = b"complete artifact";
        let spec = artifact(&root, bytes);
        fs::write(partial_path(&spec.destination).unwrap(), &bytes[..8]).unwrap();
        let source = FakeHttp {
            reply: FakeReply {
                status: 200,
                content_range: None,
                bytes: bytes.to_vec(),
            },
            requested_ranges: Mutex::new(Vec::new()),
        };

        assert!(discard_pinned_artifact_partial(&spec).unwrap());
        let candidate =
            download_pinned_artifact_with(&source, &spec, &InstallCancellation::default(), &|_| {})
                .unwrap();

        assert_eq!(source.requested_ranges.lock().unwrap().as_slice(), &[None]);
        assert_eq!(fs::read(candidate.path).unwrap(), bytes);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn partial_cleanup_rejects_a_directory_without_mutation() {
        let root = unique_root("discard-partial-directory");
        fs::create_dir_all(&root).unwrap();
        let spec = artifact(&root, b"complete artifact");
        let partial = partial_path(&spec.destination).unwrap();
        fs::write(&spec.destination, b"activated artifact").unwrap();
        fs::create_dir(&partial).unwrap();
        let source = FakeHttp {
            reply: FakeReply {
                status: 200,
                content_range: None,
                bytes: b"complete artifact".to_vec(),
            },
            requested_ranges: Mutex::new(Vec::new()),
        };

        let download_error =
            download_pinned_artifact_with(&source, &spec, &InstallCancellation::default(), &|_| {})
                .unwrap_err();
        let preflight_error = pinned_artifact_disk_space_preflight(&spec).unwrap_err();

        let error = discard_pinned_artifact_partial(&spec).unwrap_err();

        assert!(download_error.to_string().contains("not a regular file"));
        assert!(preflight_error.to_string().contains("not a regular file"));
        assert!(source.requested_ranges.lock().unwrap().is_empty());
        assert!(error.to_string().contains("not a regular file"));
        assert!(partial.is_dir());
        assert_eq!(fs::read(&spec.destination).unwrap(), b"activated artifact");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn partial_cleanup_rejects_a_symbolic_link_without_mutation() {
        let root = unique_root("discard-partial-link");
        fs::create_dir_all(&root).unwrap();
        let spec = artifact(&root, b"complete artifact");
        let partial = partial_path(&spec.destination).unwrap();
        let target = root.join("link-target");
        fs::write(&spec.destination, b"complete artifact").unwrap();
        fs::write(&target, b"retained target").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&target, &partial).unwrap();
        #[cfg(windows)]
        if std::os::windows::fs::symlink_file(&target, &partial).is_err() {
            fs::remove_dir_all(root).unwrap();
            return;
        }
        let source = FakeHttp {
            reply: FakeReply {
                status: 200,
                content_range: None,
                bytes: b"complete artifact".to_vec(),
            },
            requested_ranges: Mutex::new(Vec::new()),
        };

        let download_error =
            download_pinned_artifact_with(&source, &spec, &InstallCancellation::default(), &|_| {})
                .unwrap_err();
        let preflight_error = pinned_artifact_disk_space_preflight(&spec).unwrap_err();

        let error = discard_pinned_artifact_partial(&spec).unwrap_err();

        assert!(download_error.to_string().contains("not a regular file"));
        assert!(preflight_error.to_string().contains("not a regular file"));
        assert!(source.requested_ranges.lock().unwrap().is_empty());
        assert!(error.to_string().contains("not a regular file"));
        assert!(fs::symlink_metadata(&partial).is_ok());
        assert_eq!(fs::read(target).unwrap(), b"retained target");
        assert_eq!(fs::read(&spec.destination).unwrap(), b"complete artifact");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn partial_cleanup_validates_the_typed_artifact_before_inspection() {
        let root = unique_root("discard-partial-invalid-spec");
        fs::create_dir_all(&root).unwrap();
        let mut spec = artifact(&root, b"complete artifact");
        let partial = partial_path(&spec.destination).unwrap();
        fs::write(&partial, b"retained bytes").unwrap();
        spec.sha256 = "invalid".to_owned();

        assert!(discard_pinned_artifact_partial(&spec).is_err());
        assert_eq!(fs::read(partial).unwrap(), b"retained bytes");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn oversized_partial_is_rejected_and_quarantined_for_retry() {
        let root = unique_root("oversized-partial");
        fs::create_dir_all(&root).unwrap();
        let bytes = b"small";
        let spec = artifact(&root, bytes);
        let partial = partial_path(&spec.destination).unwrap();
        fs::write(&partial, b"too many bytes").unwrap();
        let source = FakeHttp {
            reply: FakeReply {
                status: 200,
                content_range: None,
                bytes: bytes.to_vec(),
            },
            requested_ranges: Mutex::new(Vec::new()),
        };

        let error =
            download_pinned_artifact_with(&source, &spec, &InstallCancellation::default(), &|_| {})
                .unwrap_err();

        assert!(error.to_string().contains("exceeds"));
        let quarantined = partial.with_file_name("fixture.bin.partial.oversized");
        assert!(!partial.exists());
        assert_eq!(fs::read(quarantined).unwrap(), b"too many bytes");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn exactly_one_previous_runtime_is_retained_and_restorable() {
        let root = unique_root("previous");
        let target = root.join("runtime");
        let stage_one = root.join("stage-one");
        let stage_two = root.join("stage-two");
        fs::create_dir_all(&target).unwrap();
        fs::write(target.join("version"), b"zero").unwrap();
        fs::create_dir_all(&stage_one).unwrap();
        fs::write(stage_one.join("version"), b"one").unwrap();

        activate_directory(&stage_one, &target)
            .unwrap()
            .commit_with_previous_policy(true)
            .unwrap();
        assert_eq!(fs::read(target.join("version")).unwrap(), b"one");
        assert_eq!(
            fs::read(previous_runtime_root(&target).join("version")).unwrap(),
            b"zero"
        );

        fs::create_dir_all(&stage_two).unwrap();
        fs::write(stage_two.join("version"), b"two").unwrap();
        activate_directory(&stage_two, &target)
            .unwrap()
            .commit_with_previous_policy(true)
            .unwrap();
        assert_eq!(fs::read(target.join("version")).unwrap(), b"two");
        assert_eq!(
            fs::read(previous_runtime_root(&target).join("version")).unwrap(),
            b"one"
        );
        assert!(rollback_to_previous_runtime(&target).unwrap());
        assert_eq!(fs::read(target.join("version")).unwrap(), b"one");
        assert!(!previous_runtime_root(&target).exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn activation_journal_refuses_to_overwrite_unresolved_state() {
        let root = unique_root("journal-singleton");
        fs::create_dir_all(&root).unwrap();
        let path = root.join("activation-journal.json");
        fs::write(&path, b"unresolved").unwrap();

        let error = ActivationJournal::begin(
            path.clone(),
            root.join("model.bin"),
            Some(root.join("runtime")),
            true,
            format!("{:x}", Sha256::digest(b"prior-config")),
        )
        .unwrap_err();

        assert!(error.requires_recovery());
        assert_eq!(fs::read(path).unwrap(), b"unresolved");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn reconciliation_rejects_out_of_catalog_targets_without_mutation() {
        let root = unique_root("journal-target-escape");
        fs::create_dir_all(&root).unwrap();
        let journal_path = root.join("activation-journal.json");
        let outside = root.join("outside-model.bin");
        fs::write(&outside, b"sentinel").unwrap();
        let document = ActivationJournalDocument {
            schema_version: 2,
            phase: ActivationPhase::Prepared,
            model_target: outside.clone(),
            model_had_previous: false,
            manifest_target: None,
            manifest_had_previous: false,
            runtime_target: None,
            runtime_had_previous: false,
            retain_runtime_as_previous: false,
            prior_config_fingerprint: Some(format!("{:x}", Sha256::digest(b"prior-config"))),
            expected_config_fingerprint: None,
        };
        fs::write(&journal_path, serde_json::to_vec(&document).unwrap()).unwrap();

        let error = reconcile_activation_journal(
            &journal_path,
            &[root.join("allowed-model.bin")],
            &[],
            &[],
            None,
        )
        .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("outside the catalog-managed roots")
        );
        assert_eq!(fs::read(outside).unwrap(), b"sentinel");
        assert!(journal_path.exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn repair_reconciliation_preserves_known_good_previous_runtime() {
        let root = unique_root("journal-repair-policy");
        let journal_path = root.join("activation-journal.json");
        let model = root.join("model.bin");
        let runtime = root.join("runtime");
        let rollback = directory_rollback_path(&runtime);
        let previous = previous_runtime_root(&runtime);
        fs::create_dir_all(&runtime).unwrap();
        fs::create_dir_all(&rollback).unwrap();
        fs::create_dir_all(&previous).unwrap();
        fs::write(&model, b"new-model").unwrap();
        fs::write(runtime.join("version"), b"repaired").unwrap();
        fs::write(rollback.join("version"), b"unhealthy").unwrap();
        fs::write(previous.join("version"), b"known-good").unwrap();
        let durable_new = format!("{:x}", Sha256::digest(b"new-config"));
        let document = ActivationJournalDocument {
            schema_version: 2,
            phase: ActivationPhase::ConfigPersisted,
            model_target: model.clone(),
            model_had_previous: false,
            manifest_target: None,
            manifest_had_previous: false,
            runtime_target: Some(runtime.clone()),
            runtime_had_previous: true,
            retain_runtime_as_previous: false,
            prior_config_fingerprint: Some(format!("{:x}", Sha256::digest(b"old-config"))),
            expected_config_fingerprint: Some(durable_new.clone()),
        };
        fs::write(&journal_path, serde_json::to_vec(&document).unwrap()).unwrap();

        assert!(
            reconcile_activation_journal(
                &journal_path,
                std::slice::from_ref(&model),
                &[],
                std::slice::from_ref(&runtime),
                Some(&durable_new),
            )
            .unwrap()
        );

        assert!(!rollback.exists());
        assert_eq!(fs::read(previous.join("version")).unwrap(), b"known-good");
        assert!(!journal_path.exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn update_reconciliation_promotes_replaced_runtime_to_previous() {
        let root = unique_root("journal-update-policy");
        let journal_path = root.join("activation-journal.json");
        let model = root.join("model.bin");
        let runtime = root.join("runtime");
        let rollback = directory_rollback_path(&runtime);
        let previous = previous_runtime_root(&runtime);
        fs::create_dir_all(&runtime).unwrap();
        fs::create_dir_all(&rollback).unwrap();
        fs::create_dir_all(&previous).unwrap();
        fs::write(&model, b"new-model").unwrap();
        fs::write(runtime.join("version"), b"new").unwrap();
        fs::write(rollback.join("version"), b"replaced-good").unwrap();
        fs::write(previous.join("version"), b"older-good").unwrap();
        let durable_new = format!("{:x}", Sha256::digest(b"new-config"));
        let document = ActivationJournalDocument {
            schema_version: 2,
            phase: ActivationPhase::ConfigPersisted,
            model_target: model.clone(),
            model_had_previous: false,
            manifest_target: None,
            manifest_had_previous: false,
            runtime_target: Some(runtime.clone()),
            runtime_had_previous: true,
            retain_runtime_as_previous: true,
            prior_config_fingerprint: Some(format!("{:x}", Sha256::digest(b"old-config"))),
            expected_config_fingerprint: Some(durable_new.clone()),
        };
        fs::write(&journal_path, serde_json::to_vec(&document).unwrap()).unwrap();

        reconcile_activation_journal(
            &journal_path,
            std::slice::from_ref(&model),
            &[],
            std::slice::from_ref(&runtime),
            Some(&durable_new),
        )
        .unwrap();

        assert!(!rollback.exists());
        assert_eq!(
            fs::read(previous.join("version")).unwrap(),
            b"replaced-good"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn orphaned_previous_runtime_is_preserved_only_while_runtime_is_configured() {
        let root = unique_root("orphaned-previous");
        let target = root.join("transcribe-cpp");
        let previous = previous_runtime_root(&target);
        fs::create_dir_all(&previous).unwrap();
        fs::write(previous.join("version"), b"known-good").unwrap();

        assert!(!reconcile_orphaned_previous_runtime(&target, true).unwrap());
        assert!(previous.exists());
        assert!(reconcile_orphaned_previous_runtime(&target, false).unwrap());
        assert!(!previous.exists());
        assert!(!reconcile_orphaned_previous_runtime(&target, false).unwrap());

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn model_activated_journal_rolls_back_when_old_config_is_durable() {
        let root = unique_root("journal-old-config");
        fs::create_dir_all(&root).unwrap();
        let journal_path = root.join("activation-journal.json");
        let model = root.join("model.bin");
        let model_rollback = file_rollback_path(&model).unwrap();
        fs::write(&model, b"new-model").unwrap();
        fs::write(&model_rollback, b"old-model").unwrap();
        let expected_new = format!("{:x}", Sha256::digest(b"new-config"));
        let durable_old = format!("{:x}", Sha256::digest(b"old-config"));
        let document = ActivationJournalDocument {
            schema_version: 2,
            phase: ActivationPhase::ModelActivated,
            model_target: model.clone(),
            model_had_previous: true,
            manifest_target: None,
            manifest_had_previous: false,
            runtime_target: None,
            runtime_had_previous: false,
            retain_runtime_as_previous: false,
            prior_config_fingerprint: Some(durable_old.clone()),
            expected_config_fingerprint: Some(expected_new),
        };
        fs::write(&journal_path, serde_json::to_vec(&document).unwrap()).unwrap();

        reconcile_activation_journal(
            &journal_path,
            std::slice::from_ref(&model),
            &[],
            &[],
            Some(&durable_old),
        )
        .unwrap();

        assert_eq!(fs::read(model).unwrap(), b"old-model");
        assert!(!journal_path.exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn model_activation_recovery_rolls_back_its_installed_manifest() {
        let root = unique_root("journal-manifest-old-config");
        fs::create_dir_all(&root).unwrap();
        let journal_path = root.join("activation-journal.json");
        let model = root.join("model.gguf");
        let manifest = root.join("model.gguf.install-manifest.json");
        let model_rollback = file_rollback_path(&model).unwrap();
        let manifest_rollback = file_rollback_path(&manifest).unwrap();
        fs::write(&model, b"new-model").unwrap();
        fs::write(&model_rollback, b"old-model").unwrap();
        fs::write(&manifest, b"new-manifest").unwrap();
        fs::write(&manifest_rollback, b"old-manifest").unwrap();
        let durable_old = format!("{:x}", Sha256::digest(b"old-config"));
        let document = ActivationJournalDocument {
            schema_version: 2,
            phase: ActivationPhase::ModelActivated,
            model_target: model.clone(),
            model_had_previous: true,
            manifest_target: Some(manifest.clone()),
            manifest_had_previous: true,
            runtime_target: None,
            runtime_had_previous: false,
            retain_runtime_as_previous: false,
            prior_config_fingerprint: Some(durable_old.clone()),
            expected_config_fingerprint: Some(format!("{:x}", Sha256::digest(b"new-config"))),
        };
        fs::write(&journal_path, serde_json::to_vec(&document).unwrap()).unwrap();

        reconcile_activation_journal(
            &journal_path,
            std::slice::from_ref(&model),
            std::slice::from_ref(&manifest),
            &[],
            Some(&durable_old),
        )
        .unwrap();

        assert_eq!(fs::read(&model).unwrap(), b"old-model");
        assert_eq!(fs::read(&manifest).unwrap(), b"old-manifest");
        assert!(!journal_path.exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn model_activated_journal_finalizes_when_new_config_is_durable() {
        let root = unique_root("journal-new-config");
        fs::create_dir_all(&root).unwrap();
        let journal_path = root.join("activation-journal.json");
        let model = root.join("model.bin");
        let model_rollback = file_rollback_path(&model).unwrap();
        fs::write(&model, b"new-model").unwrap();
        fs::write(&model_rollback, b"old-model").unwrap();
        let durable_new = format!("{:x}", Sha256::digest(b"new-config"));
        let document = ActivationJournalDocument {
            schema_version: 2,
            phase: ActivationPhase::ModelActivated,
            model_target: model.clone(),
            model_had_previous: true,
            manifest_target: None,
            manifest_had_previous: false,
            runtime_target: None,
            runtime_had_previous: false,
            retain_runtime_as_previous: false,
            prior_config_fingerprint: Some(format!("{:x}", Sha256::digest(b"old-config"))),
            expected_config_fingerprint: Some(durable_new.clone()),
        };
        fs::write(&journal_path, serde_json::to_vec(&document).unwrap()).unwrap();

        reconcile_activation_journal(
            &journal_path,
            std::slice::from_ref(&model),
            &[],
            &[],
            Some(&durable_new),
        )
        .unwrap();

        assert_eq!(fs::read(model).unwrap(), b"new-model");
        assert!(!model_rollback.exists());
        assert!(!journal_path.exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn activation_recovery_mismatch_requires_operator_without_mutation() {
        let root = unique_root("journal-config-mismatch");
        fs::create_dir_all(&root).unwrap();
        let journal_path = root.join("activation-journal.json");
        let model = root.join("model.bin");
        let model_rollback = file_rollback_path(&model).unwrap();
        fs::write(&model, b"new-model").unwrap();
        fs::write(&model_rollback, b"old-model").unwrap();
        let document = ActivationJournalDocument {
            schema_version: 2,
            phase: ActivationPhase::ModelActivated,
            model_target: model.clone(),
            model_had_previous: true,
            manifest_target: None,
            manifest_had_previous: false,
            runtime_target: None,
            runtime_had_previous: false,
            retain_runtime_as_previous: false,
            prior_config_fingerprint: Some(format!("{:x}", Sha256::digest(b"prior"))),
            expected_config_fingerprint: Some(format!("{:x}", Sha256::digest(b"expected"))),
        };
        fs::write(&journal_path, serde_json::to_vec(&document).unwrap()).unwrap();
        let unrelated = format!("{:x}", Sha256::digest(b"neither"));

        let error = reconcile_activation_journal(
            &journal_path,
            std::slice::from_ref(&model),
            &[],
            &[],
            Some(&unrelated),
        )
        .unwrap_err();

        assert!(error.requires_recovery());
        assert_eq!(fs::read(&model).unwrap(), b"new-model");
        assert_eq!(fs::read(&model_rollback).unwrap(), b"old-model");
        assert!(journal_path.exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn managed_removal_rolls_back_when_settings_persistence_fails() {
        let root = unique_root("removal-rollback");
        fs::create_dir_all(&root).unwrap();
        let target = root.join("model.bin");
        fs::write(&target, b"model").unwrap();

        let removal = ManagedRemoval::stage(
            &target,
            std::slice::from_ref(&target),
            format!("{:x}", Sha256::digest(b"prior-config")),
        )
        .unwrap();
        assert!(!target.exists());
        removal.rollback().unwrap();

        assert_eq!(fs::read(&target).unwrap(), b"model");
        assert!(!removal_tombstone_path(&target).unwrap().exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn exact_regular_file_removal_rejects_directories_and_wrong_targets() {
        let root = unique_root("exact-removal-safety");
        fs::create_dir_all(&root).unwrap();
        let target = root.join("included.gguf");
        let other = root.join("configured.gguf");
        fs::write(&target, b"included").unwrap();
        fs::write(&other, b"external").unwrap();
        let fingerprint = format!("{:x}", Sha256::digest(b"config"));

        let wrong_target =
            ManagedRemoval::stage_exact_regular_file(&other, &target, fingerprint.clone())
                .unwrap_err();
        assert!(wrong_target.to_string().contains("outside"));
        assert_eq!(fs::read(&other).unwrap(), b"external");

        let directory = root.join("directory.gguf");
        fs::create_dir_all(&directory).unwrap();
        let directory_error =
            ManagedRemoval::stage_exact_regular_file(&directory, &directory, fingerprint)
                .unwrap_err();
        assert!(
            directory_error
                .to_string()
                .contains("regular non-link file")
        );
        assert!(directory.is_dir());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn exact_regular_file_cleanup_is_idempotent() {
        let root = unique_root("exact-removal-cleanup");
        fs::create_dir_all(&root).unwrap();
        let target = root.join("included.gguf");
        fs::write(&target, b"included").unwrap();

        assert!(remove_exact_regular_file(&target, &target).unwrap());
        assert!(!target.exists());
        assert!(!remove_exact_regular_file(&target, &target).unwrap());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn interrupted_removal_restores_or_finishes_from_durable_settings_state() {
        let restore_root = unique_root("removal-reconcile-restore");
        fs::create_dir_all(&restore_root).unwrap();
        let restore_target = restore_root.join("runtime");
        fs::create_dir_all(&restore_target).unwrap();
        fs::write(restore_target.join("version"), b"one").unwrap();
        let prior = format!("{:x}", Sha256::digest(b"prior-config"));
        let restore = ManagedRemoval::stage(
            &restore_target,
            std::slice::from_ref(&restore_target),
            prior.clone(),
        )
        .unwrap();
        assert!(restore.removed_files());
        std::mem::forget(restore);
        assert!(
            reconcile_managed_removal(
                &restore_target,
                std::slice::from_ref(&restore_target),
                &prior,
            )
            .unwrap()
        );
        assert_eq!(fs::read(restore_target.join("version")).unwrap(), b"one");

        let finish_root = unique_root("removal-reconcile-finish");
        fs::create_dir_all(&finish_root).unwrap();
        let finish_target = finish_root.join("model.bin");
        fs::write(&finish_target, b"model").unwrap();
        let prior = format!("{:x}", Sha256::digest(b"prior-config"));
        let expected = format!("{:x}", Sha256::digest(b"expected-config"));
        let mut finish =
            ManagedRemoval::stage(&finish_target, std::slice::from_ref(&finish_target), prior)
                .unwrap();
        finish.prepare_config_commit(expected.clone()).unwrap();
        std::mem::forget(finish);
        assert!(
            reconcile_managed_removal(
                &finish_target,
                std::slice::from_ref(&finish_target),
                &expected,
            )
            .unwrap()
        );
        assert!(!finish_target.exists());
        assert!(!removal_tombstone_path(&finish_target).unwrap().exists());

        fs::remove_dir_all(restore_root).unwrap();
        fs::remove_dir_all(finish_root).unwrap();
    }

    #[test]
    fn removal_discovery_recovers_a_target_absent_from_post_commit_settings() {
        let root = unique_root("removal-discovery-post-commit");
        let target = root
            .join("huggingface")
            .join("handy-computer")
            .join("fixture")
            .join("revision")
            .join("artifact");
        fs::create_dir_all(&target).unwrap();
        fs::write(target.join("model.gguf"), b"model").unwrap();
        let prior = format!("{:x}", Sha256::digest(b"prior-config"));
        let expected = format!("{:x}", Sha256::digest(b"expected-config"));
        let mut removal =
            ManagedRemoval::stage(&target, std::slice::from_ref(&target), prior).unwrap();
        removal.prepare_config_commit(expected.clone()).unwrap();
        std::mem::forget(removal);

        let discovered = discover_managed_removal_targets(std::slice::from_ref(&root)).unwrap();
        assert_eq!(discovered, vec![canonicalize_missing(&target).unwrap()]);
        assert!(
            reconcile_managed_removal(&target, &discovered, &expected).unwrap(),
            "the durable post-removal fingerprint should finish cleanup"
        );
        assert!(!target.exists());
        assert!(!removal_tombstone_path(&target).unwrap().exists());
        assert!(!removal_journal_path(&target).unwrap().exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn removal_discovery_rejects_a_journal_target_outside_managed_storage() {
        let root = unique_root("removal-discovery-escape");
        let outside = unique_root("removal-discovery-outside").join("model.gguf");
        fs::create_dir_all(&root).unwrap();
        let journal_path = root.join("model.gguf.removal-journal.json");
        let journal = RemovalJournalDocument {
            schema_version: 1,
            target: outside,
            prior_config_fingerprint: format!("{:x}", Sha256::digest(b"prior-config")),
            expected_config_fingerprint: None,
        };
        fs::write(&journal_path, serde_json::to_vec_pretty(&journal).unwrap()).unwrap();

        let error = discover_managed_removal_targets(std::slice::from_ref(&root)).unwrap_err();
        assert!(error.requires_recovery());
        assert!(error.to_string().contains("outside its storage root"));
        assert!(journal_path.exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn removal_recovery_mismatch_preserves_tombstone_and_journal() {
        let root = unique_root("removal-reconcile-mismatch");
        fs::create_dir_all(&root).unwrap();
        let target = root.join("model.bin");
        fs::write(&target, b"model").unwrap();
        let prior = format!("{:x}", Sha256::digest(b"prior-config"));
        let expected = format!("{:x}", Sha256::digest(b"expected-config"));
        let unrelated = format!("{:x}", Sha256::digest(b"unrelated-config"));
        let mut removal =
            ManagedRemoval::stage(&target, std::slice::from_ref(&target), prior).unwrap();
        removal.prepare_config_commit(expected).unwrap();
        std::mem::forget(removal);

        let error = reconcile_managed_removal(&target, std::slice::from_ref(&target), &unrelated)
            .unwrap_err();

        assert!(error.requires_recovery());
        assert!(!target.exists());
        assert!(removal_tombstone_path(&target).unwrap().exists());
        assert!(removal_journal_path(&target).unwrap().exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn unsafe_archive_paths_are_rejected_before_extraction() {
        for path in [
            Path::new("../escape"),
            Path::new("/absolute"),
            Path::new("a/../b"),
        ] {
            assert!(
                validate_relative_path(path).is_err(),
                "accepted {}",
                path.display()
            );
        }
        assert!(validate_relative_path(Path::new("Release/whisper.dll")).is_ok());
    }

    #[test]
    fn actual_zip_traversal_is_rejected_without_writing_outside_stage() {
        let root = unique_root("zip-traversal");
        let stage = root.join("stage");
        fs::create_dir_all(&stage).unwrap();
        let archive = root.join("runtime.zip");
        write_zip(
            &archive,
            &[("../escape.dll", b"escape"), ("Release/allowed.dll", b"ok")],
        );
        let files = [RuntimeFileSpec {
            archive_path: PathBuf::from("Release/allowed.dll"),
            install_path: PathBuf::from("bin/allowed.dll"),
            size_bytes: 2,
            sha256: format!("{:x}", Sha256::digest(b"ok")),
        }];

        let error = extract_runtime_archive(
            &archive,
            &stage,
            &files,
            &InstallCancellation::default(),
            &|_| {},
        )
        .unwrap_err();

        assert!(error.to_string().contains("unsafe runtime ZIP path"));
        assert!(!root.join("escape.dll").exists());
        assert!(!stage.join("bin/allowed.dll").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn local_file_fingerprint_is_canonical_and_exact() {
        let root = unique_root("local-fingerprint");
        fs::create_dir_all(&root).unwrap();
        let model = root.join("imported.gguf");
        fs::write(&model, b"local fixture").unwrap();

        let fingerprint =
            fingerprint_file_cancellable(&model, &InstallCancellation::default()).unwrap();

        assert_eq!(
            fingerprint.canonical_path,
            fs::canonicalize(&model).unwrap()
        );
        assert_eq!(fingerprint.size_bytes, b"local fixture".len() as u64);
        assert_eq!(
            fingerprint.sha256,
            format!("{:x}", Sha256::digest(b"local fixture"))
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn runtime_tree_rejects_a_symlinked_package_root() {
        use std::os::unix::fs::symlink;

        let root = unique_root("runtime-root-link");
        let external = root.join("external");
        let linked = root.join("linked-runtime");
        fs::create_dir_all(external.join("bin")).unwrap();
        fs::write(external.join("bin").join("runtime.dll"), b"runtime").unwrap();
        symlink(&external, &linked).unwrap();
        let files = [RuntimeFileSpec {
            archive_path: PathBuf::from("Release/runtime.dll"),
            install_path: PathBuf::from("bin/runtime.dll"),
            size_bytes: 7,
            sha256: format!("{:x}", Sha256::digest(b"runtime")),
        }];

        let error = verify_runtime_tree(&linked, &files).unwrap_err();

        assert!(error.to_string().contains("symbolic link/reparse point"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn runtime_tree_rejects_a_linked_or_reparse_ancestor() {
        let root = unique_root("runtime-ancestor-link");
        let external = root.join("external");
        let linked_parent = root.join("linked-parent");
        let package = external.join("runtime");
        fs::create_dir_all(&package).unwrap();
        fs::write(package.join("runtime.dll"), b"runtime").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&external, &linked_parent).unwrap();
        #[cfg(windows)]
        if std::os::windows::fs::symlink_dir(&external, &linked_parent).is_err() {
            fs::remove_dir_all(root).unwrap();
            return;
        }
        let files = [RuntimeFileSpec {
            archive_path: PathBuf::from("runtime.dll"),
            install_path: PathBuf::from("runtime.dll"),
            size_bytes: 7,
            sha256: format!("{:x}", Sha256::digest(b"runtime")),
        }];

        let error = verify_runtime_tree(&linked_parent.join("runtime"), &files).unwrap_err();

        assert!(error.to_string().contains("symbolic link, reparse point"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn runtime_extraction_requires_every_allowlisted_file_and_honors_cancel() {
        let missing_root = unique_root("zip-missing");
        let missing_stage = missing_root.join("stage");
        fs::create_dir_all(&missing_stage).unwrap();
        let missing_archive = missing_root.join("runtime.zip");
        write_zip(&missing_archive, &[("Release/other.dll", b"other")]);
        let files = [RuntimeFileSpec {
            archive_path: PathBuf::from("Release/required.dll"),
            install_path: PathBuf::from("bin/required.dll"),
            size_bytes: 8,
            sha256: format!("{:x}", Sha256::digest(b"required")),
        }];
        let error = extract_runtime_archive(
            &missing_archive,
            &missing_stage,
            &files,
            &InstallCancellation::default(),
            &|_| {},
        )
        .unwrap_err();
        assert!(error.to_string().contains("is missing"));

        let cancelled_root = unique_root("zip-cancelled");
        let cancelled_stage = cancelled_root.join("stage");
        fs::create_dir_all(&cancelled_stage).unwrap();
        let cancelled_archive = cancelled_root.join("runtime.zip");
        write_zip(&cancelled_archive, &[("Release/required.dll", b"required")]);
        let cancellation = InstallCancellation::default();
        cancellation.cancel();
        let error = extract_runtime_archive(
            &cancelled_archive,
            &cancelled_stage,
            &files,
            &cancellation,
            &|_| {},
        )
        .unwrap_err();
        assert!(error.is_cancelled());
        assert!(!cancelled_stage.join("bin/required.dll").exists());

        fs::remove_dir_all(missing_root).unwrap();
        fs::remove_dir_all(cancelled_root).unwrap();
    }

    #[test]
    fn runtime_tree_rejects_extra_files_and_directories() {
        let root = unique_root("runtime-tree-extras");
        fs::create_dir_all(root.join("bin")).unwrap();
        fs::write(root.join("bin").join("allowed.dll"), b"allowed").unwrap();
        let files = [RuntimeFileSpec {
            archive_path: PathBuf::from("Release/allowed.dll"),
            install_path: PathBuf::from("bin/allowed.dll"),
            size_bytes: 7,
            sha256: format!("{:x}", Sha256::digest(b"allowed")),
        }];
        verify_runtime_tree(&root, &files).unwrap();

        fs::write(root.join("bin").join("extra.dll"), b"extra").unwrap();
        let error = verify_runtime_tree(&root, &files).unwrap_err();
        assert!(error.to_string().contains("unallowlisted file"));
        fs::remove_file(root.join("bin").join("extra.dll")).unwrap();

        fs::create_dir(root.join("unexpected")).unwrap();
        let error = verify_runtime_tree(&root, &files).unwrap_err();
        assert!(error.to_string().contains("unallowlisted directory"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn content_range_parser_is_strict() {
        assert_eq!(
            parse_content_range("bytes 4-9/10"),
            Some(ContentRange {
                start: 4,
                end: 9,
                total: 10
            })
        );
        for invalid in ["4-9/10", "bytes */10", "bytes 4-9/*", "bytes 4/10"] {
            assert_eq!(parse_content_range(invalid), None);
        }
    }

    #[test]
    fn cancellation_and_activation_commit_have_one_terminal_winner() {
        use std::sync::Barrier;
        use std::thread;

        for _ in 0..128 {
            let cancellation = InstallCancellation::default();
            let cancel_handle = cancellation.clone();
            let activation_handle = cancellation.clone();
            let barrier = Arc::new(Barrier::new(3));
            let cancel_barrier = Arc::clone(&barrier);
            let activate_barrier = Arc::clone(&barrier);
            let cancel = thread::spawn(move || {
                cancel_barrier.wait();
                cancel_handle.cancel();
            });
            let activate = thread::spawn(move || {
                activate_barrier.wait();
                activation_handle.try_commit_activation()
            });
            barrier.wait();
            cancel.join().unwrap();
            let activation = activate.join().unwrap();

            match activation {
                Ok(()) => {
                    assert!(cancellation.activation_is_committed());
                    assert!(!cancellation.is_cancelled());
                    cancellation.cancel();
                    assert!(cancellation.activation_is_committed());
                    assert!(!cancellation.is_cancelled());
                }
                Err(ActivationCommitError::Cancelled) => {
                    assert!(cancellation.is_cancelled());
                    assert!(!cancellation.activation_is_committed());
                }
                Err(ActivationCommitError::AlreadyCommitted) => {
                    panic!("only one activation contender exists")
                }
            }
        }
    }

    #[test]
    fn activation_commit_makes_late_cancellation_a_defined_no_op() {
        let cancellation = InstallCancellation::default();
        cancellation.try_commit_activation().unwrap();
        cancellation.cancel();

        assert!(cancellation.activation_is_committed());
        assert!(!cancellation.is_cancelled());
        assert_eq!(
            cancellation.try_commit_activation(),
            Err(ActivationCommitError::AlreadyCommitted)
        );
    }
}
