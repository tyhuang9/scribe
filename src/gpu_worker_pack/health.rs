use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::backend_policy::{BackendTarget, CandidateQuarantineProjection};

use super::store::{PackStoreError, atomic_write_canonical, read_canonical_state};

const HEALTH_SCHEMA_VERSION: u16 = 1;
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
    WorkerStart,
    Handshake,
    RuntimeLoad,
    DeviceLost,
    OutOfMemory,
    Decode,
    Timeout,
    Protocol,
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
    records: Vec<HealthRecord>,
}

impl HealthEnvelope {
    fn empty(witnesses: HealthWitnesses) -> Self {
        Self {
            schema_version: HEALTH_SCHEMA_VERSION,
            witnesses,
            records: Vec::new(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum HealthDecision {
    Available,
    RetryBypass,
    Quarantined { until_unix_seconds: u64 },
}

pub(crate) struct HealthCache<'a> {
    path: PathBuf,
    envelope: HealthEnvelope,
    clock: &'a dyn Clock,
}

impl<'a> HealthCache<'a> {
    pub(crate) fn open(
        path: impl Into<PathBuf>,
        witnesses: HealthWitnesses,
        clock: &'a dyn Clock,
    ) -> Self {
        let path = path.into();
        let envelope =
            load_fail_closed(&path, &witnesses).unwrap_or_else(|| HealthEnvelope::empty(witnesses));
        Self {
            path,
            envelope,
            clock,
        }
    }

    pub(crate) fn decision(&self, key: &HealthKey) -> HealthDecision {
        let now = self.clock.now_unix_seconds();
        let Some(record) = self.record(key) else {
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

    pub(crate) fn record_failure(
        &mut self,
        key: HealthKey,
        code: FailureCode,
    ) -> Result<HealthDecision, HealthCacheError> {
        validate_key(&key)?;
        let now = self.clock.now_unix_seconds();
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
        let decision = HealthDecision::Quarantined {
            until_unix_seconds: record.quarantined_until_unix_seconds,
        };
        self.persist()?;
        Ok(decision)
    }

    /// Grants one immediate attempt for an exact quarantined key. The grant is
    /// bounded to ten minutes and does not alter failure count or history.
    pub(crate) fn grant_explicit_retry(
        &mut self,
        key: &HealthKey,
    ) -> Result<bool, HealthCacheError> {
        let now = self.clock.now_unix_seconds();
        let Some(record) = self.record_mut(key) else {
            return Ok(false);
        };
        if now >= record.quarantined_until_unix_seconds {
            return Ok(false);
        }
        record.retry_grant_expires_unix_seconds = Some(now.saturating_add(RETRY_GRANT_SECONDS));
        record.retry_grant_remaining = true;
        self.persist()?;
        Ok(true)
    }

    /// Atomically consumes the one-shot retry. Repeated candidate enumeration
    /// may inspect a bypass without consuming it; only the launch coordinator
    /// should call this method.
    pub(crate) fn consume_explicit_retry(
        &mut self,
        key: &HealthKey,
    ) -> Result<bool, HealthCacheError> {
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
        self.persist()?;
        Ok(true)
    }

    pub(crate) fn record_idle_probe_success(
        &mut self,
        key: &HealthKey,
    ) -> Result<bool, HealthCacheError> {
        let Some(record) = self.record_mut(key) else {
            return Ok(false);
        };
        record.successful_idle_probes = record.successful_idle_probes.saturating_add(1).min(2);
        if record.successful_idle_probes == 2 {
            self.envelope.records.retain(|record| &record.key != key);
            self.persist()?;
            return Ok(true);
        }
        self.persist()?;
        Ok(false)
    }

    /// Builds a projection for one exact pack/runtime/platform/model context.
    /// Driver and device identity are matched again at the candidate boundary.
    pub(crate) fn quarantine_projection_for(&self, key: &HealthKey) -> HealthQuarantineProjection {
        let quarantined =
            matches!(self.decision(key), HealthDecision::Quarantined { .. }).then(|| {
                (
                    key.driver_version.clone(),
                    key.stable_device_identity.clone(),
                )
            });
        HealthQuarantineProjection { quarantined }
    }

    fn record(&self, key: &HealthKey) -> Option<&HealthRecord> {
        self.envelope
            .records
            .iter()
            .find(|record| &record.key == key)
    }

    fn record_mut(&mut self, key: &HealthKey) -> Option<&mut HealthRecord> {
        self.envelope
            .records
            .iter_mut()
            .find(|record| &record.key == key)
    }

    fn evict_if_full(&mut self) {
        if self.envelope.records.len() < MAX_RECORDS {
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

    fn persist(&self) -> Result<(), HealthCacheError> {
        validate_envelope(&self.envelope)?;
        atomic_write_canonical(&self.path, &self.envelope)?;
        Ok(())
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

fn load_fail_closed(path: &Path, witnesses: &HealthWitnesses) -> Option<HealthEnvelope> {
    if !path.exists() {
        return Some(HealthEnvelope::empty(witnesses.clone()));
    }
    let envelope = read_canonical_state::<HealthEnvelope>(path).ok()?;
    if envelope.witnesses != *witnesses || validate_envelope(&envelope).is_err() {
        return None;
    }
    Some(envelope)
}

fn validate_envelope(envelope: &HealthEnvelope) -> Result<(), HealthCacheError> {
    if envelope.schema_version != HEALTH_SCHEMA_VERSION
        || envelope.records.len() > MAX_RECORDS
        || envelope.witnesses.app_build.is_empty()
        || envelope.witnesses.app_build.len() > MAX_TEXT_BYTES
        || !valid_sha256(&envelope.witnesses.device_set_digest)
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
    use std::fs;
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
                .record_failure(key.clone(), FailureCode::WorkerStart)
                .unwrap(),
            HealthDecision::Quarantined {
                until_unix_seconds: 1_000_000 + FIRST_QUARANTINE_SECONDS
            }
        );
        clock.advance(FIRST_QUARANTINE_SECONDS);
        assert_eq!(cache.decision(&key), HealthDecision::Available);
        assert_eq!(
            cache
                .record_failure(key.clone(), FailureCode::Handshake)
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
                .record_failure(key.clone(), FailureCode::DeviceLost)
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
            .record_failure(key.clone(), FailureCode::OutOfMemory)
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
            .record_failure(retry_key.clone(), FailureCode::RuntimeLoad)
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
            .record_failure(retry_key.clone(), FailureCode::RuntimeLoad)
            .unwrap();
        assert_eq!(
            second,
            HealthDecision::Quarantined {
                until_unix_seconds: clock.now_unix_seconds() + SECOND_QUARANTINE_SECONDS
            }
        );

        let expiring = key("expiring-retry");
        cache
            .record_failure(expiring.clone(), FailureCode::Timeout)
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
            .record_failure(key.clone(), FailureCode::Decode)
            .unwrap();
        assert!(!cache.record_idle_probe_success(&key).unwrap());
        cache
            .record_failure(key.clone(), FailureCode::Decode)
            .unwrap();
        assert_eq!(cache.envelope.records[0].failure_count, 2);
        assert!(!cache.record_idle_probe_success(&key).unwrap());
        assert!(cache.record_idle_probe_success(&key).unwrap());
        assert_eq!(cache.decision(&key), HealthDecision::Available);
        cache
            .record_failure(key.clone(), FailureCode::Decode)
            .unwrap();
        assert_eq!(cache.envelope.records[0].failure_count, 1);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn witness_key_and_corrupt_cache_changes_invalidate_without_affecting_cpu() {
        let root = temp_root("health-invalidation");
        let path = root.join("health.json");
        let clock = ManualClock::new(4_000_000);
        let original_key = key("original");
        let mut cache = HealthCache::open(&path, witnesses("app-a"), &clock);
        cache
            .record_failure(original_key.clone(), FailureCode::Protocol)
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
            HealthDecision::Available
        );
        let mut changed_device_set_witness = witnesses("app-a");
        changed_device_set_witness.device_set_digest = "f".repeat(64);
        let changed_device_set = HealthCache::open(&path, changed_device_set_witness, &clock);
        assert_eq!(
            changed_device_set.decision(&original_key),
            HealthDecision::Available
        );
        fs::write(&path, b"{raw-error-and-path:C:\\private}").unwrap();
        let corrupt = HealthCache::open(&path, witnesses("app-a"), &clock);
        assert_eq!(corrupt.decision(&original_key), HealthDecision::Available);

        let projection = corrupt.quarantine_projection_for(&original_key);
        let mut candidates = vec![
            gpu(&original_key),
            BackendCandidate::available(BackendTarget::cpu()),
        ];
        apply_quarantine_projection(&mut candidates, &projection);
        assert_eq!(candidates[0].availability, CandidateAvailability::Available);
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
            .record_failure(key.clone(), FailureCode::DeviceLost)
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
}
