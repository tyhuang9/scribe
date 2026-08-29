use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::backend_policy::{BackendTarget, CandidateQuarantineProjection};

use super::store::{ExclusiveFileLock, PackStoreError, exclusive_file_lock};

const HEALTH_SCHEMA_VERSION: u16 = 3;
const MAX_RECORDS: usize = 128;
const MAX_TEXT_BYTES: usize = 256;
const RETRY_GRANT_SECONDS: u64 = 10 * 60;
const FIRST_QUARANTINE_SECONDS: u64 = 15 * 60;
const SECOND_QUARANTINE_SECONDS: u64 = 6 * 60 * 60;
const THIRD_QUARANTINE_SECONDS: u64 = 7 * 24 * 60 * 60;

pub(crate) trait Clock: Send + Sync {
    fn now_unix_seconds(&self) -> u64;
}

pub(crate) struct SystemClock;

impl Clock for SystemClock {
    fn now_unix_seconds(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
    }
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct HealthKey {
    pub(crate) pack_digest: String,
    pub(crate) runtime_abi: u16,
    pub(crate) os_arch: String,
    pub(crate) driver_version: String,
    pub(crate) stable_device_identity: String,
    pub(crate) model_digest: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct HealthWitnesses {
    pub(crate) app_build: String,
    pub(crate) device_set_digest: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum FailureCode {
    WorkerCrash,
    WorkerHang,
    ProviderInitialization,
    DriverFailure,
    DeviceLost,
    OutOfMemory,
    Protocol,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FailureObservation {
    WorkerCrash,
    WorkerHang,
    ProviderInitialization,
    DriverFailure,
    DeviceLost,
    OutOfMemory,
    Protocol,
    InvalidInput,
    ArtifactCorruption,
    ModelCorruption,
    DecodeContent,
    Cancellation,
    PartialOutput,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct HealthRecord {
    key: HealthKey,
    failure_code: FailureCode,
    /// Saturates at three because every later failure uses the same 7-day tier.
    failure_count: u8,
    last_failure_unix_seconds: u64,
    quarantined_until_unix_seconds: u64,
    successful_idle_probes: u8,
    retry_grant_expires_unix_seconds: Option<u64>,
    retry_grant_remaining: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct HealthEnvelope {
    schema_version: u16,
    witnesses: HealthWitnesses,
    recovery_mode: bool,
    records: Vec<HealthRecord>,
    recovery_probes: Vec<RecoveryProbe>,
    recovered_keys: Vec<HealthKey>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RecoveryProbe {
    key: HealthKey,
    successful_idle_probes: u8,
    last_probe_unix_seconds: u64,
}

impl HealthEnvelope {
    fn empty(witnesses: HealthWitnesses) -> Self {
        Self {
            schema_version: HEALTH_SCHEMA_VERSION,
            witnesses,
            recovery_mode: false,
            records: Vec::new(),
            recovery_probes: Vec::new(),
            recovered_keys: Vec::new(),
        }
    }

    fn recovering(witnesses: HealthWitnesses) -> Self {
        Self {
            recovery_mode: true,
            ..Self::empty(witnesses)
        }
    }
}

enum CacheLoad {
    Missing,
    Valid(HealthEnvelope),
    Invalid,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum HealthDecision {
    Available,
    RetryBypass,
    InvalidOrUnprobed,
    Quarantined { until_unix_seconds: u64 },
}

pub(crate) struct HealthCache<'a> {
    path: PathBuf,
    witnesses: HealthWitnesses,
    envelope: HealthEnvelope,
    invalid: bool,
    clock: &'a dyn Clock,
}

impl<'a> HealthCache<'a> {
    pub(crate) fn open(
        path: impl Into<PathBuf>,
        witnesses: HealthWitnesses,
        clock: &'a dyn Clock,
    ) -> Self {
        let path = path.into();
        let (envelope, invalid) = match load_cache(&path, &witnesses) {
            CacheLoad::Missing => (HealthEnvelope::empty(witnesses.clone()), false),
            CacheLoad::Valid(envelope) => (envelope, false),
            CacheLoad::Invalid => (HealthEnvelope::recovering(witnesses.clone()), true),
        };
        Self {
            path,
            witnesses,
            envelope,
            invalid,
            clock,
        }
    }

    pub(crate) fn decision(&self, key: &HealthKey) -> HealthDecision {
        match load_cache(&self.path, &self.witnesses) {
            CacheLoad::Missing => HealthDecision::Available,
            CacheLoad::Invalid => HealthDecision::InvalidOrUnprobed,
            CacheLoad::Valid(envelope) => {
                decision_from_envelope(&envelope, key, self.clock.now_unix_seconds())
            }
        }
    }

    pub(crate) fn record_observed_failure(
        &mut self,
        key: HealthKey,
        observation: FailureObservation,
    ) -> Result<Option<HealthDecision>, HealthCacheError> {
        let Some(code) = quarantine_eligible_code(observation) else {
            return Ok(None);
        };
        self.record_provider_failure(key, code).map(Some)
    }

    pub(crate) fn record_provider_failure(
        &mut self,
        key: HealthKey,
        code: FailureCode,
    ) -> Result<HealthDecision, HealthCacheError> {
        let lock = exclusive_file_lock(&self.lock_path())?;
        self.reload_locked(&lock);
        validate_key(&key)?;
        let now = self.clock.now_unix_seconds();
        self.envelope
            .recovery_probes
            .retain(|probe| probe.key != key);
        self.envelope
            .recovered_keys
            .retain(|recovered| recovered != &key);
        let record = match self.record_mut(&key) {
            Some(record) => {
                record.failure_count = record.failure_count.saturating_add(1).min(3);
                record.failure_code = code;
                record.last_failure_unix_seconds = now;
                record.successful_idle_probes = 0;
                record.retry_grant_expires_unix_seconds = None;
                record.retry_grant_remaining = false;
                record
            }
            None => {
                self.evict_if_full();
                self.envelope.records.push(HealthRecord {
                    key: key.clone(),
                    failure_code: code,
                    failure_count: 1,
                    last_failure_unix_seconds: now,
                    quarantined_until_unix_seconds: now,
                    successful_idle_probes: 0,
                    retry_grant_expires_unix_seconds: None,
                    retry_grant_remaining: false,
                });
                self.record_mut(&key).expect("record was inserted")
            }
        };
        let seconds = quarantine_seconds(record.failure_count);
        record.quarantined_until_unix_seconds = now.saturating_add(seconds);
        self.persist(&lock)?;
        Ok(decision_from_envelope(&self.envelope, &key, now))
    }

    /// Grants one immediate attempt for an exact quarantined key. The grant is
    /// bounded to ten minutes and does not alter failure count or history.
    pub(crate) fn grant_explicit_retry(
        &mut self,
        key: &HealthKey,
    ) -> Result<bool, HealthCacheError> {
        let lock = exclusive_file_lock(&self.lock_path())?;
        self.reload_locked(&lock);
        if self.invalid
            || (self.envelope.recovery_mode && !self.is_recovered(key))
            || self.recovery_probe(key).is_some()
        {
            return Ok(false);
        }
        let now = self.clock.now_unix_seconds();
        let Some(record) = self.record_mut(key) else {
            return Ok(false);
        };
        if now >= record.quarantined_until_unix_seconds {
            return Ok(false);
        }
        record.retry_grant_expires_unix_seconds = Some(now.saturating_add(RETRY_GRANT_SECONDS));
        record.retry_grant_remaining = true;
        self.persist(&lock)?;
        Ok(true)
    }

    /// Atomically consumes the one-shot retry. Repeated candidate enumeration
    /// may inspect a bypass without consuming it; only the launch coordinator
    /// should call this method.
    pub(crate) fn consume_explicit_retry(
        &mut self,
        key: &HealthKey,
    ) -> Result<bool, HealthCacheError> {
        let lock = exclusive_file_lock(&self.lock_path())?;
        self.reload_locked(&lock);
        if self.invalid
            || (self.envelope.recovery_mode && !self.is_recovered(key))
            || self.recovery_probe(key).is_some()
        {
            return Ok(false);
        }
        let now = self.clock.now_unix_seconds();
        let Some(record) = self.record_mut(key) else {
            return Ok(false);
        };
        let granted = record.retry_grant_remaining
            && record
                .retry_grant_expires_unix_seconds
                .is_some_and(|expires| now <= expires);
        if !granted {
            return Ok(false);
        }
        record.retry_grant_remaining = false;
        record.retry_grant_expires_unix_seconds = None;
        self.persist(&lock)?;
        Ok(true)
    }

    pub(crate) fn record_idle_probe_success(
        &mut self,
        key: &HealthKey,
    ) -> Result<bool, HealthCacheError> {
        let lock = exclusive_file_lock(&self.lock_path())?;
        self.reload_locked(&lock);
        validate_key(key)?;
        let now = self.clock.now_unix_seconds();
        if self.invalid {
            self.envelope = HealthEnvelope::recovering(self.witnesses.clone());
        }
        if self.envelope.recovery_mode {
            if self.is_recovered(key) {
                return Ok(true);
            }
            if let Some(record) = self.record_mut(key) {
                record.successful_idle_probes =
                    record.successful_idle_probes.saturating_add(1).min(2);
                if record.successful_idle_probes < 2 {
                    self.persist(&lock)?;
                    return Ok(false);
                }
                self.envelope.records.retain(|record| &record.key != key);
                self.envelope.recovered_keys.push(key.clone());
                self.persist(&lock)?;
                return Ok(true);
            }
            if let Some(probe) = self.recovery_probe_mut(key) {
                probe.successful_idle_probes =
                    probe.successful_idle_probes.saturating_add(1).min(2);
                probe.last_probe_unix_seconds = now;
                if probe.successful_idle_probes == 2 {
                    self.envelope
                        .recovery_probes
                        .retain(|probe| &probe.key != key);
                    self.envelope.recovered_keys.push(key.clone());
                    self.persist(&lock)?;
                    return Ok(true);
                }
                self.persist(&lock)?;
                return Ok(false);
            }
            self.evict_if_full();
            self.envelope.recovery_probes.push(RecoveryProbe {
                key: key.clone(),
                successful_idle_probes: 1,
                last_probe_unix_seconds: now,
            });
            self.persist(&lock)?;
            return Ok(false);
        }
        if let Some(probe) = self.recovery_probe_mut(key) {
            probe.successful_idle_probes = probe.successful_idle_probes.saturating_add(1).min(2);
            probe.last_probe_unix_seconds = now;
            if probe.successful_idle_probes == 2 {
                self.envelope
                    .recovery_probes
                    .retain(|probe| &probe.key != key);
                self.persist(&lock)?;
                return Ok(true);
            }
            self.persist(&lock)?;
            return Ok(false);
        }
        let Some(record) = self.record_mut(key) else {
            return Ok(false);
        };
        record.successful_idle_probes = record.successful_idle_probes.saturating_add(1).min(2);
        if record.successful_idle_probes == 2 {
            self.envelope.records.retain(|record| &record.key != key);
            self.persist(&lock)?;
            return Ok(true);
        }
        self.persist(&lock)?;
        Ok(false)
    }

    /// Builds a projection for one exact pack/runtime/platform/model context.
    /// Driver and device identity are matched again at the candidate boundary.
    pub(crate) fn quarantine_projection_for(&self, key: &HealthKey) -> HealthQuarantineProjection {
        let quarantined = matches!(
            self.decision(key),
            HealthDecision::Quarantined { .. } | HealthDecision::InvalidOrUnprobed
        )
        .then(|| {
            (
                key.driver_version.clone(),
                key.stable_device_identity.clone(),
            )
        });
        HealthQuarantineProjection { quarantined }
    }

    fn record_mut(&mut self, key: &HealthKey) -> Option<&mut HealthRecord> {
        self.envelope
            .records
            .iter_mut()
            .find(|record| &record.key == key)
    }

    fn recovery_probe(&self, key: &HealthKey) -> Option<&RecoveryProbe> {
        self.envelope
            .recovery_probes
            .iter()
            .find(|probe| &probe.key == key)
    }

    fn recovery_probe_mut(&mut self, key: &HealthKey) -> Option<&mut RecoveryProbe> {
        self.envelope
            .recovery_probes
            .iter_mut()
            .find(|probe| &probe.key == key)
    }

    fn is_recovered(&self, key: &HealthKey) -> bool {
        self.envelope
            .recovered_keys
            .iter()
            .any(|recovered| recovered == key)
    }

    fn evict_if_full(&mut self) {
        if self.envelope.records.len()
            + self.envelope.recovery_probes.len()
            + self.envelope.recovered_keys.len()
            < MAX_RECORDS
        {
            return;
        }
        if self.envelope.recovery_mode && !self.envelope.recovered_keys.is_empty() {
            self.envelope.recovered_keys.remove(0);
            return;
        }
        if let Some((index, _)) = self
            .envelope
            .recovery_probes
            .iter()
            .enumerate()
            .min_by_key(|(_, probe)| probe.last_probe_unix_seconds)
        {
            self.envelope.recovery_probes.remove(index);
            return;
        }
        if let Some((index, _)) = self
            .envelope
            .records
            .iter()
            .enumerate()
            .min_by_key(|(_, record)| record.last_failure_unix_seconds)
        {
            self.envelope.records.remove(index);
        }
    }

    fn persist(&mut self, lock: &ExclusiveFileLock) -> Result<(), HealthCacheError> {
        validate_envelope(&self.envelope)?;
        lock.write(&self.path, &self.envelope)?;
        self.invalid = false;
        Ok(())
    }

    fn reload_locked(&mut self, lock: &ExclusiveFileLock) {
        match load_cache_locked(lock, &self.path, &self.witnesses) {
            CacheLoad::Missing => {
                self.envelope = HealthEnvelope::empty(self.witnesses.clone());
                self.invalid = false;
            }
            CacheLoad::Valid(envelope) => {
                self.envelope = envelope;
                self.invalid = false;
            }
            CacheLoad::Invalid => {
                self.envelope = HealthEnvelope::recovering(self.witnesses.clone());
                self.invalid = true;
            }
        }
    }

    fn lock_path(&self) -> PathBuf {
        let name = self
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("gpu-health");
        self.path.with_file_name(format!(".{name}.lock"))
    }
}

pub(crate) struct HealthQuarantineProjection {
    quarantined: Option<(String, String)>,
}

impl CandidateQuarantineProjection for HealthQuarantineProjection {
    fn is_quarantined(&self, target: &BackendTarget) -> bool {
        let driver = target.driver_version.as_deref().unwrap_or("unknown");
        self.quarantined.as_ref()
            == Some(&(driver.to_owned(), target.device_id.as_str().to_owned()))
    }
}

fn load_cache(path: &Path, witnesses: &HealthWitnesses) -> CacheLoad {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("gpu-health");
    let lock_path = path.with_file_name(format!(".{name}.lock"));
    let Ok(lock) = exclusive_file_lock(&lock_path) else {
        return CacheLoad::Invalid;
    };
    load_cache_locked(&lock, path, witnesses)
}

fn load_cache_locked(
    lock: &ExclusiveFileLock,
    path: &Path,
    witnesses: &HealthWitnesses,
) -> CacheLoad {
    match lock.exists(path) {
        Ok(false) => return CacheLoad::Missing,
        Err(_) => return CacheLoad::Invalid,
        Ok(true) => {}
    }
    let Ok(envelope) = lock.read::<HealthEnvelope>(path) else {
        return CacheLoad::Invalid;
    };
    if envelope.witnesses != *witnesses || validate_envelope(&envelope).is_err() {
        return CacheLoad::Invalid;
    }
    CacheLoad::Valid(envelope)
}

fn decision_from_envelope(envelope: &HealthEnvelope, key: &HealthKey, now: u64) -> HealthDecision {
    if envelope.recovery_mode
        && !envelope
            .recovered_keys
            .iter()
            .any(|recovered| recovered == key)
    {
        return HealthDecision::InvalidOrUnprobed;
    }
    if envelope
        .recovery_probes
        .iter()
        .any(|probe| &probe.key == key)
    {
        return HealthDecision::InvalidOrUnprobed;
    }
    let Some(record) = envelope.records.iter().find(|record| &record.key == key) else {
        return HealthDecision::Available;
    };
    if record.retry_grant_remaining
        && record
            .retry_grant_expires_unix_seconds
            .is_some_and(|expires| now <= expires)
    {
        return HealthDecision::RetryBypass;
    }
    if now < record.quarantined_until_unix_seconds {
        HealthDecision::Quarantined {
            until_unix_seconds: record.quarantined_until_unix_seconds,
        }
    } else {
        HealthDecision::Available
    }
}

fn quarantine_eligible_code(observation: FailureObservation) -> Option<FailureCode> {
    match observation {
        FailureObservation::WorkerCrash => Some(FailureCode::WorkerCrash),
        FailureObservation::WorkerHang => Some(FailureCode::WorkerHang),
        FailureObservation::ProviderInitialization => Some(FailureCode::ProviderInitialization),
        FailureObservation::DriverFailure => Some(FailureCode::DriverFailure),
        FailureObservation::DeviceLost => Some(FailureCode::DeviceLost),
        FailureObservation::OutOfMemory => Some(FailureCode::OutOfMemory),
        FailureObservation::Protocol => Some(FailureCode::Protocol),
        FailureObservation::InvalidInput
        | FailureObservation::ArtifactCorruption
        | FailureObservation::ModelCorruption
        | FailureObservation::DecodeContent
        | FailureObservation::Cancellation
        | FailureObservation::PartialOutput => None,
    }
}

fn validate_envelope(envelope: &HealthEnvelope) -> Result<(), HealthCacheError> {
    if envelope.schema_version != HEALTH_SCHEMA_VERSION
        || envelope.records.len() + envelope.recovery_probes.len() + envelope.recovered_keys.len()
            > MAX_RECORDS
        || envelope.witnesses.app_build.is_empty()
        || envelope.witnesses.app_build.len() > MAX_TEXT_BYTES
        || !valid_sha256(&envelope.witnesses.device_set_digest)
    {
        return Err(HealthCacheError::InvalidCache);
    }
    if !envelope.recovery_mode
        && (!envelope.recovery_probes.is_empty() || !envelope.recovered_keys.is_empty())
    {
        return Err(HealthCacheError::InvalidCache);
    }
    let mut keys = BTreeSet::new();
    for record in &envelope.records {
        validate_key(&record.key)?;
        if !keys.insert(&record.key)
            || !(1..=3).contains(&record.failure_count)
            || record.successful_idle_probes > 1
            || record.quarantined_until_unix_seconds < record.last_failure_unix_seconds
            || record.retry_grant_remaining != record.retry_grant_expires_unix_seconds.is_some()
        {
            return Err(HealthCacheError::InvalidCache);
        }
    }
    for probe in &envelope.recovery_probes {
        validate_key(&probe.key)?;
        if !keys.insert(&probe.key) || probe.successful_idle_probes != 1 {
            return Err(HealthCacheError::InvalidCache);
        }
    }
    for recovered in &envelope.recovered_keys {
        validate_key(recovered)?;
        if !keys.insert(recovered) {
            return Err(HealthCacheError::InvalidCache);
        }
    }
    Ok(())
}

fn validate_key(key: &HealthKey) -> Result<(), HealthCacheError> {
    if !valid_sha256(&key.pack_digest)
        || key.runtime_abi == 0
        || !valid_sha256(&key.model_digest)
        || !valid_bounded_text(&key.os_arch)
        || !valid_bounded_text(&key.driver_version)
        || !valid_bounded_text(&key.stable_device_identity)
        || key.stable_device_identity != key.stable_device_identity.to_ascii_lowercase()
    {
        return Err(HealthCacheError::InvalidKey);
    }
    Ok(())
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn valid_bounded_text(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_TEXT_BYTES
        && value.bytes().all(|byte| (0x20..=0x7e).contains(&byte))
}

fn quarantine_seconds(failure_count: u8) -> u64 {
    match failure_count {
        1 => FIRST_QUARANTINE_SECONDS,
        2 => SECOND_QUARANTINE_SECONDS,
        _ => THIRD_QUARANTINE_SECONDS,
    }
}

#[derive(Debug, Error)]
pub(crate) enum HealthCacheError {
    #[error("GPU health key is invalid")]
    InvalidKey,
    #[error("GPU health cache is invalid")]
    InvalidCache,
    #[error("GPU health cache persistence failed: {0}")]
    Persistence(#[from] PackStoreError),
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{self, OpenOptions};
    use std::sync::atomic::{AtomicU64, Ordering};

    use crate::backend_policy::{
        BackendCandidate, BackendKind, CandidateAvailability, DeviceClass, DeviceIdentity,
        GpuVendor, ProviderIdentity, apply_quarantine_projection,
    };
    use crate::gpu_worker_pack::manifest::test_support::temp_root;

    struct ManualClock(AtomicU64);

    impl ManualClock {
        fn new(now: u64) -> Self {
            Self(AtomicU64::new(now))
        }

        fn advance(&self, seconds: u64) {
            self.0.fetch_add(seconds, Ordering::SeqCst);
        }
    }

    impl Clock for ManualClock {
        fn now_unix_seconds(&self) -> u64 {
            self.0.load(Ordering::SeqCst)
        }
    }

    fn witnesses(suffix: &str) -> HealthWitnesses {
        HealthWitnesses {
            app_build: format!("{}:{suffix}", crate::onnx_worker::DESKTOP_BUILD_ID),
            device_set_digest: "d".repeat(64),
        }
    }

    fn key(suffix: &str) -> HealthKey {
        HealthKey {
            pack_digest: "a".repeat(64),
            runtime_abi: 1,
            os_arch: "windows-x86_64".to_owned(),
            driver_version: format!("551.23-{suffix}"),
            stable_device_identity: "pci:0000:01:00.0".to_owned(),
            model_digest: "b".repeat(64),
        }
    }

    fn distinct_key(suffix: &str) -> HealthKey {
        HealthKey {
            pack_digest: "c".repeat(64),
            runtime_abi: 1,
            os_arch: "windows-x86_64".to_owned(),
            driver_version: format!("552.44-{suffix}"),
            stable_device_identity: "pci:0000:02:00.0".to_owned(),
            model_digest: "e".repeat(64),
        }
    }

    fn gpu(key: &HealthKey) -> BackendCandidate {
        BackendCandidate::available(BackendTarget {
            backend: BackendKind::Vulkan,
            provider_id: ProviderIdentity::new("fixture:vulkan"),
            driver_version: Some(key.driver_version.clone()),
            device_id: DeviceIdentity::new(&key.stable_device_identity),
            display_name: "Fixture GPU".to_owned(),
            vendor: GpuVendor::Nvidia,
            device_class: DeviceClass::DiscreteGpu,
            memory_total_bytes: 8 * 1024 * 1024 * 1024,
            memory_available_bytes: 6 * 1024 * 1024 * 1024,
            pack: None,
            process_index: Some(0),
        })
    }

    #[test]
    fn escalation_uses_exact_clock_controlled_15m_6h_7d_tiers() {
        let root = temp_root("health-tiers");
        let path = root.join("health.json");
        let clock = ManualClock::new(1_000_000);
        let mut cache = HealthCache::open(&path, witnesses("tiers"), &clock);
        let key = key("tiers");

        assert_eq!(
            cache
                .record_provider_failure(key.clone(), FailureCode::ProviderInitialization)
                .unwrap(),
            HealthDecision::Quarantined {
                until_unix_seconds: 1_000_000 + FIRST_QUARANTINE_SECONDS
            }
        );
        clock.advance(FIRST_QUARANTINE_SECONDS);
        assert_eq!(cache.decision(&key), HealthDecision::Available);
        assert_eq!(
            cache
                .record_provider_failure(key.clone(), FailureCode::Protocol)
                .unwrap(),
            HealthDecision::Quarantined {
                until_unix_seconds: 1_000_000
                    + FIRST_QUARANTINE_SECONDS
                    + SECOND_QUARANTINE_SECONDS
            }
        );
        clock.advance(SECOND_QUARANTINE_SECONDS);
        assert_eq!(
            cache
                .record_provider_failure(key.clone(), FailureCode::DeviceLost)
                .unwrap(),
            HealthDecision::Quarantined {
                until_unix_seconds: 1_000_000
                    + FIRST_QUARANTINE_SECONDS
                    + SECOND_QUARANTINE_SECONDS
                    + THIRD_QUARANTINE_SECONDS
            }
        );
        clock.advance(THIRD_QUARANTINE_SECONDS);
        let fourth = cache
            .record_provider_failure(key.clone(), FailureCode::OutOfMemory)
            .unwrap();
        assert_eq!(
            fourth,
            HealthDecision::Quarantined {
                until_unix_seconds: clock.now_unix_seconds() + THIRD_QUARANTINE_SECONDS
            }
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn explicit_retry_is_one_shot_and_failed_retry_retains_escalation_history() {
        let root = temp_root("health-retry");
        let path = root.join("health.json");
        let clock = ManualClock::new(2_000_000);
        let mut cache = HealthCache::open(&path, witnesses("retry"), &clock);
        let retry_key = key("retry");
        cache
            .record_provider_failure(retry_key.clone(), FailureCode::ProviderInitialization)
            .unwrap();
        assert!(cache.grant_explicit_retry(&retry_key).unwrap());
        assert_eq!(cache.decision(&retry_key), HealthDecision::RetryBypass);
        assert!(cache.consume_explicit_retry(&retry_key).unwrap());
        assert!(!cache.consume_explicit_retry(&retry_key).unwrap());
        assert!(matches!(
            cache.decision(&retry_key),
            HealthDecision::Quarantined { .. }
        ));
        let second = cache
            .record_provider_failure(retry_key.clone(), FailureCode::ProviderInitialization)
            .unwrap();
        assert_eq!(
            second,
            HealthDecision::Quarantined {
                until_unix_seconds: clock.now_unix_seconds() + SECOND_QUARANTINE_SECONDS
            }
        );

        let expiring = key("expiring-retry");
        cache
            .record_provider_failure(expiring.clone(), FailureCode::WorkerHang)
            .unwrap();
        assert!(cache.grant_explicit_retry(&expiring).unwrap());
        clock.advance(RETRY_GRANT_SECONDS + 1);
        assert!(matches!(
            cache.decision(&expiring),
            HealthDecision::Quarantined { .. }
        ));
        assert!(!cache.consume_explicit_retry(&expiring).unwrap());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn two_idle_probes_clear_history_but_a_failed_probe_escalates() {
        let root = temp_root("health-probes");
        let path = root.join("health.json");
        let clock = ManualClock::new(3_000_000);
        let mut cache = HealthCache::open(&path, witnesses("probe"), &clock);
        let key = key("probe");
        cache
            .record_provider_failure(key.clone(), FailureCode::WorkerCrash)
            .unwrap();
        assert!(!cache.record_idle_probe_success(&key).unwrap());
        cache
            .record_provider_failure(key.clone(), FailureCode::WorkerCrash)
            .unwrap();
        assert_eq!(cache.envelope.records[0].failure_count, 2);
        assert!(!cache.record_idle_probe_success(&key).unwrap());
        assert!(cache.record_idle_probe_success(&key).unwrap());
        assert_eq!(cache.decision(&key), HealthDecision::Available);
        cache
            .record_provider_failure(key.clone(), FailureCode::WorkerCrash)
            .unwrap();
        assert_eq!(cache.envelope.records[0].failure_count, 1);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn witness_key_changes_invalidate_without_affecting_cpu() {
        let root = temp_root("health-invalidation");
        let path = root.join("health.json");
        let clock = ManualClock::new(4_000_000);
        let original_key = key("original");
        let mut cache = HealthCache::open(&path, witnesses("app-a"), &clock);
        cache
            .record_provider_failure(original_key.clone(), FailureCode::Protocol)
            .unwrap();
        assert!(matches!(
            cache.decision(&original_key),
            HealthDecision::Quarantined { .. }
        ));
        assert_eq!(
            cache.decision(&key("new-driver")),
            HealthDecision::Available
        );
        for changed_key in [
            HealthKey {
                pack_digest: "c".repeat(64),
                ..original_key.clone()
            },
            HealthKey {
                stable_device_identity: "pci:0000:02:00.0".to_owned(),
                ..original_key.clone()
            },
            HealthKey {
                model_digest: "e".repeat(64),
                ..original_key.clone()
            },
        ] {
            assert_eq!(cache.decision(&changed_key), HealthDecision::Available);
        }

        let changed_app = HealthCache::open(&path, witnesses("app-b"), &clock);
        assert_eq!(
            changed_app.decision(&original_key),
            HealthDecision::InvalidOrUnprobed
        );
        let mut changed_device_set_witness = witnesses("app-a");
        changed_device_set_witness.device_set_digest = "f".repeat(64);
        let changed_device_set = HealthCache::open(&path, changed_device_set_witness, &clock);
        assert_eq!(
            changed_device_set.decision(&original_key),
            HealthDecision::InvalidOrUnprobed
        );
        let recovered_key = distinct_key("witness-recovered");
        let blocked_key = key("witness-blocked");
        let mut changed_app = HealthCache::open(&path, witnesses("app-b"), &clock);
        assert_eq!(
            changed_app.decision(&recovered_key),
            HealthDecision::InvalidOrUnprobed
        );
        assert!(
            !changed_app
                .record_idle_probe_success(&recovered_key)
                .unwrap()
        );
        assert!(
            changed_app
                .record_idle_probe_success(&recovered_key)
                .unwrap()
        );
        assert_eq!(
            changed_app.decision(&recovered_key),
            HealthDecision::Available
        );
        assert_eq!(
            changed_app.decision(&blocked_key),
            HealthDecision::InvalidOrUnprobed
        );
        drop(changed_app);

        let reopened = HealthCache::open(&path, witnesses("app-b"), &clock);
        assert_eq!(reopened.decision(&recovered_key), HealthDecision::Available);
        assert_eq!(
            reopened.decision(&blocked_key),
            HealthDecision::InvalidOrUnprobed
        );
        let reset_by_witness = HealthCache::open(&path, witnesses("app-c"), &clock);
        assert_eq!(
            reset_by_witness.decision(&recovered_key),
            HealthDecision::InvalidOrUnprobed
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn corrupt_cache_recovery_is_persisted_and_exact_key_fail_closed() {
        let root = temp_root("health-corrupt-exact-recovery");
        let path = root.join("health.json");
        let clock = ManualClock::new(4_500_000);
        let recovered_key = key("recovered-a");
        let blocked_key = distinct_key("blocked-b");
        fs::write(&path, b"{raw-error-and-path:C:\\private}").unwrap();
        let mut corrupt = HealthCache::open(&path, witnesses("corrupt"), &clock);
        assert_eq!(
            corrupt.decision(&recovered_key),
            HealthDecision::InvalidOrUnprobed
        );
        assert_eq!(
            corrupt.decision(&blocked_key),
            HealthDecision::InvalidOrUnprobed
        );

        let projection = corrupt.quarantine_projection_for(&blocked_key);
        let mut candidates = vec![
            gpu(&blocked_key),
            BackendCandidate::available(BackendTarget::cpu()),
        ];
        apply_quarantine_projection(&mut candidates, &projection);
        assert_eq!(
            candidates[0].availability,
            CandidateAvailability::Quarantined
        );
        assert_eq!(candidates[1].availability, CandidateAvailability::Available);
        assert!(!corrupt.record_idle_probe_success(&recovered_key).unwrap());
        assert_eq!(
            corrupt.decision(&recovered_key),
            HealthDecision::InvalidOrUnprobed
        );
        assert_eq!(
            corrupt.decision(&blocked_key),
            HealthDecision::InvalidOrUnprobed
        );

        let mut reopened = HealthCache::open(&path, witnesses("corrupt"), &clock);
        assert_eq!(
            reopened.decision(&recovered_key),
            HealthDecision::InvalidOrUnprobed
        );
        assert!(reopened.record_idle_probe_success(&recovered_key).unwrap());
        assert_eq!(reopened.decision(&recovered_key), HealthDecision::Available);
        assert_eq!(
            reopened.decision(&blocked_key),
            HealthDecision::InvalidOrUnprobed
        );
        drop(reopened);

        let mut reopened = HealthCache::open(&path, witnesses("corrupt"), &clock);
        assert_eq!(reopened.decision(&recovered_key), HealthDecision::Available);
        assert_eq!(
            reopened
                .record_provider_failure(recovered_key.clone(), FailureCode::WorkerCrash)
                .unwrap(),
            HealthDecision::InvalidOrUnprobed
        );
        assert!(!reopened.grant_explicit_retry(&recovered_key).unwrap());
        assert!(!reopened.consume_explicit_retry(&recovered_key).unwrap());
        assert_eq!(
            reopened.decision(&blocked_key),
            HealthDecision::InvalidOrUnprobed
        );

        assert!(!reopened.record_idle_probe_success(&recovered_key).unwrap());
        reopened
            .record_provider_failure(recovered_key.clone(), FailureCode::Protocol)
            .unwrap();
        assert!(!reopened.record_idle_probe_success(&recovered_key).unwrap());
        assert_eq!(
            reopened.decision(&recovered_key),
            HealthDecision::InvalidOrUnprobed
        );
        assert!(reopened.record_idle_probe_success(&recovered_key).unwrap());
        assert_eq!(reopened.decision(&recovered_key), HealthDecision::Available);
        assert_eq!(
            reopened.decision(&blocked_key),
            HealthDecision::InvalidOrUnprobed
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn concurrent_health_state_growth_is_bounded_and_cpu_remains_available() {
        let root = temp_root("health-concurrent-growth");
        let path = root.join("health.json");
        let clock = ManualClock::new(4_600_000);
        let context = key("growth");
        let expected_witnesses = witnesses("growth");
        let mut cache = HealthCache::open(&path, expected_witnesses.clone(), &clock);
        cache
            .record_provider_failure(context.clone(), FailureCode::WorkerCrash)
            .unwrap();
        drop(cache);

        let writer_start = std::sync::Arc::new(std::sync::Barrier::new(2));
        let writer_finished = std::sync::Arc::new(std::sync::Barrier::new(2));
        let writer_start_thread = std::sync::Arc::clone(&writer_start);
        let writer_finished_thread = std::sync::Arc::clone(&writer_finished);
        let writer_path = path.clone();
        let writer = std::thread::spawn(move || {
            writer_start_thread.wait();
            OpenOptions::new()
                .write(true)
                .open(writer_path)
                .unwrap()
                .set_len(crate::gpu_worker_pack::store::MAX_STATE_BYTES + 1)
                .unwrap();
            writer_finished_thread.wait();
        });
        crate::gpu_worker_pack::store::set_state_read_hook(move |_| {
            writer_start.wait();
            writer_finished.wait();
        });
        let cache = HealthCache::open(&path, expected_witnesses, &clock);
        writer.join().unwrap();
        assert_eq!(cache.decision(&context), HealthDecision::InvalidOrUnprobed);
        let projection = cache.quarantine_projection_for(&context);
        let mut candidates = vec![
            gpu(&context),
            BackendCandidate::available(BackendTarget::cpu()),
        ];
        apply_quarantine_projection(&mut candidates, &projection);
        assert_eq!(
            candidates[0].availability,
            CandidateAvailability::Quarantined
        );
        assert_eq!(candidates[1].availability, CandidateAvailability::Available);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn exact_health_projection_marks_only_matching_gpu_candidate() {
        let root = temp_root("health-projection");
        let path = root.join("health.json");
        let clock = ManualClock::new(5_000_000);
        let key = key("projection");
        let mut cache = HealthCache::open(&path, witnesses("projection"), &clock);
        cache
            .record_provider_failure(key.clone(), FailureCode::DeviceLost)
            .unwrap();
        let projection = cache.quarantine_projection_for(&key);
        let mut other = gpu(&key);
        other.target.driver_version = Some("different-driver".to_owned());
        let mut candidates = vec![
            gpu(&key),
            other,
            BackendCandidate::available(BackendTarget::cpu()),
        ];
        apply_quarantine_projection(&mut candidates, &projection);
        assert_eq!(
            candidates[0].availability,
            CandidateAvailability::Quarantined
        );
        assert_eq!(candidates[1].availability, CandidateAvailability::Available);
        assert_eq!(candidates[2].availability, CandidateAvailability::Available);

        assert!(cache.grant_explicit_retry(&key).unwrap());
        let retry_projection = cache.quarantine_projection_for(&key);
        apply_quarantine_projection(&mut candidates, &retry_projection);
        // Projection cannot restore another availability; fresh discovery sees
        // the bypass as available.
        let mut fresh = vec![gpu(&key)];
        apply_quarantine_projection(&mut fresh, &retry_projection);
        assert_eq!(fresh[0].availability, CandidateAvailability::Available);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn content_and_caller_failures_cannot_mutate_quarantine_history() {
        let root = temp_root("health-ineligible");
        let path = root.join("health.json");
        let clock = ManualClock::new(6_000_000);
        let mut cache = HealthCache::open(&path, witnesses("ineligible"), &clock);
        let key = key("ineligible");
        assert_eq!(cache.decision(&key), HealthDecision::Available);
        for observation in [
            FailureObservation::InvalidInput,
            FailureObservation::ArtifactCorruption,
            FailureObservation::ModelCorruption,
            FailureObservation::DecodeContent,
            FailureObservation::Cancellation,
            FailureObservation::PartialOutput,
        ] {
            assert_eq!(
                cache
                    .record_observed_failure(key.clone(), observation)
                    .unwrap(),
                None
            );
        }
        assert!(!path.exists());
        assert_eq!(cache.decision(&key), HealthDecision::Available);
        for observation in [
            FailureObservation::WorkerCrash,
            FailureObservation::WorkerHang,
            FailureObservation::ProviderInitialization,
            FailureObservation::DriverFailure,
            FailureObservation::DeviceLost,
            FailureObservation::OutOfMemory,
            FailureObservation::Protocol,
        ] {
            assert!(quarantine_eligible_code(observation).is_some());
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn two_instances_consume_exactly_one_retry_grant() {
        let root = temp_root("health-concurrent-retry");
        let path = root.join("health.json");
        let clock = ManualClock::new(7_000_000);
        let key = key("concurrent-retry");
        let mut setup = HealthCache::open(&path, witnesses("concurrent-retry"), &clock);
        setup
            .record_provider_failure(key.clone(), FailureCode::WorkerCrash)
            .unwrap();
        assert!(setup.grant_explicit_retry(&key).unwrap());
        let mut left = HealthCache::open(&path, witnesses("concurrent-retry"), &clock);
        let mut right = HealthCache::open(&path, witnesses("concurrent-retry"), &clock);
        let results = std::thread::scope(|scope| {
            let left_key = key.clone();
            let right_key = key.clone();
            let left = scope.spawn(move || left.consume_explicit_retry(&left_key).unwrap());
            let right = scope.spawn(move || right.consume_explicit_retry(&right_key).unwrap());
            [left.join().unwrap(), right.join().unwrap()]
        });
        assert_eq!(results.into_iter().filter(|consumed| *consumed).count(), 1);
        let observed = HealthCache::open(&path, witnesses("concurrent-retry"), &clock);
        assert!(matches!(
            observed.decision(&key),
            HealthDecision::Quarantined { .. }
        ));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn two_instances_do_not_lose_failure_or_probe_updates() {
        let root = temp_root("health-concurrent-updates");
        let path = root.join("health.json");
        let clock = ManualClock::new(8_000_000);
        let key = key("concurrent-updates");
        let mut left = HealthCache::open(&path, witnesses("concurrent-updates"), &clock);
        let mut right = HealthCache::open(&path, witnesses("concurrent-updates"), &clock);
        std::thread::scope(|scope| {
            let left_key = key.clone();
            let right_key = key.clone();
            scope.spawn(move || {
                left.record_provider_failure(left_key, FailureCode::WorkerCrash)
                    .unwrap()
            });
            scope.spawn(move || {
                right
                    .record_provider_failure(right_key, FailureCode::DriverFailure)
                    .unwrap()
            });
        });
        let observed = HealthCache::open(&path, witnesses("concurrent-updates"), &clock);
        assert_eq!(observed.envelope.records[0].failure_count, 2);

        let mut left = HealthCache::open(&path, witnesses("concurrent-updates"), &clock);
        let mut right = HealthCache::open(&path, witnesses("concurrent-updates"), &clock);
        let results = std::thread::scope(|scope| {
            let left_key = key.clone();
            let right_key = key.clone();
            let left = scope.spawn(move || left.record_idle_probe_success(&left_key).unwrap());
            let right = scope.spawn(move || right.record_idle_probe_success(&right_key).unwrap());
            [left.join().unwrap(), right.join().unwrap()]
        });
        assert_eq!(results.into_iter().filter(|cleared| *cleared).count(), 1);
        let observed = HealthCache::open(&path, witnesses("concurrent-updates"), &clock);
        assert_eq!(observed.decision(&key), HealthDecision::Available);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn quarantine_projection_preserves_non_available_candidate_states() {
        let root = temp_root("health-projection-composition");
        let path = root.join("health.json");
        let clock = ManualClock::new(9_000_000);
        let key = key("composition");
        let mut cache = HealthCache::open(&path, witnesses("composition"), &clock);
        cache
            .record_provider_failure(key.clone(), FailureCode::DeviceLost)
            .unwrap();
        let projection = cache.quarantine_projection_for(&key);
        let states = [
            CandidateAvailability::Available,
            CandidateAvailability::Unaddressable,
            CandidateAvailability::Incompatible,
            CandidateAvailability::Unhealthy,
            CandidateAvailability::Quarantined,
        ];
        let mut candidates = states
            .into_iter()
            .map(|availability| {
                let mut candidate = gpu(&key);
                candidate.availability = availability;
                candidate
            })
            .collect::<Vec<_>>();
        apply_quarantine_projection(&mut candidates, &projection);
        assert_eq!(
            candidates
                .iter()
                .map(|candidate| candidate.availability)
                .collect::<Vec<_>>(),
            vec![
                CandidateAvailability::Quarantined,
                CandidateAvailability::Unaddressable,
                CandidateAvailability::Incompatible,
                CandidateAvailability::Unhealthy,
                CandidateAvailability::Quarantined,
            ]
        );
        fs::remove_dir_all(root).unwrap();
    }
}
