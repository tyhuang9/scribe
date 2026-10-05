//! Bounded paired CPU/GPU observation campaign.
//!
//! This remains an unsigned observation artifact. It deliberately makes no
//! qualification, admission, evaluator, signature, memory-floor, thermal, or
//! background-load claim.

use std::io::Seek;
use std::path::Path;
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use serde::Serialize;
use sha2::{Digest, Sha256};

use super::telemetry::{ProcessAffinityObservation, SamplingSession, observe_process_affinity};
use super::{
    CampaignPower, CommandOptions, InputReport, MemoryAvailabilityReport, ProviderMemoryReport,
    UnavailableField, UnavailableReport, VideoMemoryReport, WorkerObservationMeasurements,
    build_worker_report, hex, publish_atomic_new, require_retained_digest,
    validate_handshake_capture, verify_input,
};
use crate::backend_policy::PowerSource;
use crate::model_catalog::ArtifactFormat;
use crate::onnx_worker::{
    CaptureObservationWorker, CaptureObservationWorkerFactory, GpuCaptureObservationIdentity,
    WorkerObservationLease,
};
use crate::prepared_audio::PreparedAudio;
use crate::runtime_artifact::{RuntimeArtifact, RuntimeModel};
use crate::runtime_contract::WARM_MODEL_TTL;
use crate::transcription::{AccelerationPreference, ModelId};

const KIND: &str = "windows_gpu_capture_campaign";
const SCHEMA_VERSION: u8 = 2;
const COLD_PAIRS: u8 = 5;
const WARM_PAIRS: u8 = 20;
const MAX_CAPTURES: usize = 14;
const MAX_RECORDS: usize = 52;
const MAX_CAMPAIGN_REPORT_BYTES: usize = 32 * 1024 * 1024;
const MAX_FRAME_BYTES: usize = 26 + 256 * 1024;
const MAX_REFERENCE_BYTES: usize = 96;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Target {
    Cpu,
    Gpu,
}

impl Target {
    const fn index(self) -> usize {
        match self {
            Self::Cpu => 0,
            Self::Gpu => 1,
        }
    }

    const fn preference(self) -> AccelerationPreference {
        match self {
            Self::Cpu => AccelerationPreference::Cpu,
            Self::Gpu => AccelerationPreference::Gpu,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Phase {
    Cold,
    Prime,
    Warm,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum CapturePurpose {
    Preflight,
    Cold,
    Prime,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum RecordStatus {
    Succeeded,
    Failed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum FailureStage {
    PowerPreflight,
    WorkerPreflight,
    AudioPreparation,
    Handshake,
    ObservationNegotiation,
    TelemetrySetup,
    Inference,
    ObservationValidation,
    PowerEndpoint,
    TelemetryFinish,
    WarmModelTtl,
    Cleanup,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum FailureCategory {
    PowerUnknown,
    PowerMismatch,
    PowerChanged,
    WorkerUnavailable,
    ObservationUnsupported,
    HandshakeInvalid,
    ObservationFailed,
    TelemetryUnavailable,
    IdentityMismatch,
    DiagnosticsMismatch,
    TtlExpired,
    CleanupFailed,
    InvariantViolation,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
struct FailureDescriptor {
    stage: FailureStage,
    category: FailureCategory,
    #[serde(skip_serializing_if = "Option::is_none")]
    target: Option<Target>,
    #[serde(skip_serializing_if = "Option::is_none")]
    phase: Option<Phase>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pair_index: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    order_in_pair: Option<u8>,
}

impl FailureDescriptor {
    const fn global(stage: FailureStage, category: FailureCategory) -> Self {
        Self {
            stage,
            category,
            target: None,
            phase: None,
            pair_index: None,
            order_in_pair: None,
        }
    }

    const fn for_spec(spec: RunSpec, stage: FailureStage, category: FailureCategory) -> Self {
        Self {
            stage,
            category,
            target: Some(spec.target),
            phase: Some(spec.phase),
            pair_index: spec.pair_index,
            order_in_pair: Some(spec.order_in_pair),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RunSpec {
    measured: bool,
    phase: Phase,
    pair_index: Option<u8>,
    order_in_pair: u8,
    target: Target,
}

#[derive(Serialize)]
struct CampaignReport {
    schema_version: u8,
    kind: &'static str,
    unsigned: bool,
    unqualified: bool,
    auto_eligible: bool,
    release_approved: bool,
    collector_build_revision: &'static str,
    expected_power: CampaignPower,
    inputs: InputReport,
    #[serde(skip_serializing_if = "Option::is_none")]
    gpu_identity: Option<GpuCaptureObservationIdentity>,
    incomplete: bool,
    cleanup_complete: bool,
    captures: Vec<HandshakeCapture>,
    runs: Vec<CampaignRunRecord>,
    #[serde(skip_serializing_if = "Option::is_none")]
    failure: Option<FailureDescriptor>,
    unavailable: UnavailableReport,
    environmental_controls: EnvironmentalControls,
}

#[derive(Serialize)]
struct EnvironmentalControls {
    background_load: UnavailableField,
    host_control: UnavailableField,
    affinity_control: UnavailableField,
    power_plan: UnavailableField,
}

#[derive(Clone, Serialize)]
struct HandshakeCapture {
    logical_sequence: u8,
    generation_ref: String,
    digest_sha256: String,
    target: Target,
    purpose: CapturePurpose,
    hello_frame_hex: String,
    ready_frame_hex: String,
}

#[derive(Serialize)]
struct CampaignRunRecord {
    measured: bool,
    phase: Phase,
    #[serde(skip_serializing_if = "Option::is_none")]
    pair_index: Option<u8>,
    order_in_pair: u8,
    target: Target,
    #[serde(skip_serializing_if = "Option::is_none")]
    generation_ref: Option<String>,
    status: RecordStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    power_source_before: Option<PowerSource>,
    #[serde(skip_serializing_if = "Option::is_none")]
    power_source_after: Option<PowerSource>,
    end_to_end_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    backend_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    model_load_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    warm_reused: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    sampled_max_private_usage_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    telemetry_sample_count: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    video_memory: Option<VideoMemoryReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    raw_provider_memory: Option<ProviderMemoryReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    memory_availability: Option<MemoryAvailabilityReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    worker_process_affinity: Option<ProcessAffinityPair>,
    #[serde(skip_serializing_if = "Option::is_none")]
    normalized_transcript_sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    failure: Option<FailureDescriptor>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct ProcessAffinityPair {
    before: Option<ProcessAffinityObservation>,
    after: Option<ProcessAffinityObservation>,
    changed: Option<bool>,
}

fn process_affinity_pair(
    before: Option<&ProcessAffinityObservation>,
    after: Option<&ProcessAffinityObservation>,
) -> Option<ProcessAffinityPair> {
    if before.is_none() && after.is_none() {
        return None;
    }
    let before = before.cloned();
    let after = after.cloned();
    let changed = match (before.as_ref(), after.as_ref()) {
        (
            Some(ProcessAffinityObservation::Available { .. }),
            Some(ProcessAffinityObservation::Available { .. }),
        ) => Some(before != after),
        _ => None,
    };
    Some(ProcessAffinityPair {
        before,
        after,
        changed,
    })
}

fn validate_process_affinity_pair(pair: &ProcessAffinityPair) -> Result<()> {
    if pair.before.is_none() {
        bail!("campaign affinity observation omitted its initial endpoint")
    }
    let validate_endpoint = |endpoint: &ProcessAffinityObservation| -> Result<()> {
        let ProcessAffinityObservation::Available {
            processor_group,
            process_mask_hex,
            system_mask_hex,
        } = endpoint
        else {
            return Ok(());
        };
        let width = std::mem::size_of::<usize>() * 2;
        let canonical = |mask: &str| {
            mask.len() == width
                && mask
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        };
        if *processor_group != 0 || !canonical(process_mask_hex) || !canonical(system_mask_hex) {
            bail!("campaign affinity observation is not canonical")
        }
        let process_mask = usize::from_str_radix(process_mask_hex, 16)?;
        let system_mask = usize::from_str_radix(system_mask_hex, 16)?;
        if process_mask == 0 || system_mask == 0 || process_mask & !system_mask != 0 {
            bail!("campaign affinity observation contains invalid masks")
        }
        Ok(())
    };
    if let Some(before) = &pair.before {
        validate_endpoint(before)?;
    }
    if let Some(after) = &pair.after {
        validate_endpoint(after)?;
    }
    let expected_changed = match (pair.before.as_ref(), pair.after.as_ref()) {
        (
            Some(ProcessAffinityObservation::Available { .. }),
            Some(ProcessAffinityObservation::Available { .. }),
        ) => Some(pair.before != pair.after),
        _ => None,
    };
    if pair.changed != expected_changed {
        bail!("campaign affinity change result is inconsistent")
    }
    Ok(())
}

impl CampaignRunRecord {
    fn failed(
        spec: RunSpec,
        generation_ref: Option<String>,
        elapsed: Duration,
        power_before: Option<PowerSource>,
        power_after: Option<PowerSource>,
        failure: FailureDescriptor,
    ) -> Self {
        Self {
            measured: spec.measured,
            phase: spec.phase,
            pair_index: spec.pair_index,
            order_in_pair: spec.order_in_pair,
            target: spec.target,
            generation_ref,
            status: RecordStatus::Failed,
            power_source_before: power_before,
            power_source_after: power_after,
            end_to_end_ms: duration_millis(elapsed),
            backend_ms: None,
            model_load_ms: None,
            warm_reused: None,
            sampled_max_private_usage_bytes: None,
            telemetry_sample_count: None,
            video_memory: None,
            raw_provider_memory: None,
            memory_availability: None,
            worker_process_affinity: None,
            normalized_transcript_sha256: None,
            failure: Some(failure),
        }
    }
}

struct ReportBuilder {
    report: CampaignReport,
    next_sequence: u8,
}

impl ReportBuilder {
    fn new(
        expected_power: CampaignPower,
        inputs: InputReport,
        gpu_identity: Option<GpuCaptureObservationIdentity>,
    ) -> Self {
        let unavailable = UnavailableField {
            status: "unavailable",
            reason: "not_observed",
        };
        Self {
            report: CampaignReport {
                schema_version: SCHEMA_VERSION,
                kind: KIND,
                unsigned: true,
                unqualified: true,
                auto_eligible: false,
                release_approved: false,
                collector_build_revision: env!("SCRIBE_BUILD_REVISION"),
                expected_power,
                inputs,
                gpu_identity,
                incomplete: false,
                cleanup_complete: true,
                captures: Vec::with_capacity(MAX_CAPTURES),
                runs: Vec::with_capacity(MAX_RECORDS),
                failure: None,
                unavailable: UnavailableReport {
                    inference_thread_count: UnavailableField {
                        status: "unavailable",
                        reason: "unsupported_by_pinned_runtime_api",
                    },
                    thermal_state: unavailable,
                },
                environmental_controls: EnvironmentalControls {
                    background_load: unavailable,
                    host_control: unavailable,
                    affinity_control: unavailable,
                    power_plan: unavailable,
                },
            },
            next_sequence: 0,
        }
    }

    fn fail(&mut self, failure: FailureDescriptor) {
        self.report.incomplete = true;
        if self.report.failure.is_none() {
            self.report.failure = Some(failure);
        }
    }

    fn cleanup_failed(&mut self, target: Option<Target>, phase: Option<Phase>) {
        self.report.cleanup_complete = false;
        let mut failure =
            FailureDescriptor::global(FailureStage::Cleanup, FailureCategory::CleanupFailed);
        failure.target = target;
        failure.phase = phase;
        self.fail(failure);
    }

    fn register_capture(
        &mut self,
        target: Target,
        purpose: CapturePurpose,
        lease: &WorkerObservationLease,
    ) -> Result<String> {
        validate_handshake_capture(lease)?;
        if self.report.captures.len() >= MAX_CAPTURES {
            bail!("campaign handshake capture bound exceeded")
        }
        let hello = lease.hello_frame();
        let ready = lease.ready_frame();
        if hello.len() > MAX_FRAME_BYTES || ready.len() > MAX_FRAME_BYTES {
            bail!("campaign handshake frame bound exceeded")
        }
        let digest_sha256 = handshake_digest(hello, ready);
        if self
            .report
            .captures
            .iter()
            .any(|capture| capture.digest_sha256 == digest_sha256)
        {
            bail!("campaign observed a repeated worker generation handshake")
        }
        self.next_sequence = self
            .next_sequence
            .checked_add(1)
            .ok_or_else(|| anyhow!("campaign generation sequence overflowed"))?;
        let generation_ref = format!(
            "generation-{:02}-{}",
            self.next_sequence,
            &digest_sha256[..24]
        );
        self.report.captures.push(HandshakeCapture {
            logical_sequence: self.next_sequence,
            generation_ref: generation_ref.clone(),
            digest_sha256,
            target,
            purpose,
            hello_frame_hex: hex(hello),
            ready_frame_hex: hex(ready),
        });
        Ok(generation_ref)
    }
}

fn handshake_digest(hello: &[u8], ready: &[u8]) -> String {
    let mut digest = Sha256::new();
    digest.update((hello.len() as u64).to_le_bytes());
    digest.update(hello);
    digest.update((ready.len() as u64).to_le_bytes());
    digest.update(ready);
    format!("{:x}", digest.finalize())
}

fn duration_millis(value: Duration) -> u64 {
    u64::try_from(value.as_millis()).unwrap_or(u64::MAX)
}

fn duration_nanos(value: Duration) -> u64 {
    u64::try_from(value.as_nanos()).unwrap_or(u64::MAX)
}

fn pair_order(pair_index: u8) -> [Target; 2] {
    if pair_index % 2 == 1 {
        [Target::Cpu, Target::Gpu]
    } else {
        [Target::Gpu, Target::Cpu]
    }
}

fn measured_specs(phase: Phase, pairs: u8) -> Vec<RunSpec> {
    let mut specs = Vec::with_capacity(usize::from(pairs) * 2);
    for pair_index in 1..=pairs {
        for (offset, target) in pair_order(pair_index).into_iter().enumerate() {
            specs.push(RunSpec {
                measured: true,
                phase,
                pair_index: Some(pair_index),
                order_in_pair: u8::try_from(offset + 1).expect("pair order is bounded"),
                target,
            });
        }
    }
    specs
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct SlotLifecycle {
    installed: bool,
    active: bool,
    expired: bool,
    deadline_ns: Option<u64>,
}

#[derive(Debug, Default)]
struct LifecycleState {
    slots: [SlotLifecycle; 2],
    active: Option<Target>,
    ttl_failure: Option<Target>,
}

impl LifecycleState {
    fn install(&mut self, target: Target, result_ns: u64, ttl_ns: u64) -> Result<()> {
        if self.slots[target.index()].installed {
            bail!("warm campaign target was primed more than once")
        }
        self.slots[target.index()] = SlotLifecycle {
            installed: true,
            active: false,
            expired: false,
            deadline_ns: Some(result_ns.saturating_add(ttl_ns)),
        };
        Ok(())
    }

    fn begin(&mut self, target: Target, now_ns: u64) -> Result<()> {
        self.expire_due(now_ns);
        if self.ttl_failure.is_some() {
            bail!("a retained warm worker exceeded its model TTL")
        }
        if self.active.is_some() {
            bail!("more than one campaign inference would be active")
        }
        let slot = &mut self.slots[target.index()];
        if !slot.installed || slot.expired {
            bail!("retained campaign worker is unavailable")
        }
        slot.active = true;
        slot.deadline_ns = None;
        self.active = Some(target);
        Ok(())
    }

    fn finish(
        &mut self,
        target: Target,
        result_ns: u64,
        succeeded: bool,
        ttl_ns: u64,
    ) -> Result<()> {
        if self.active != Some(target) || !self.slots[target.index()].active {
            bail!("campaign lifecycle finished a non-active target")
        }
        let slot = &mut self.slots[target.index()];
        slot.active = false;
        slot.expired = !succeeded;
        slot.deadline_ns = succeeded.then(|| result_ns.saturating_add(ttl_ns));
        self.active = None;
        Ok(())
    }

    fn expire_due(&mut self, now_ns: u64) -> Vec<Target> {
        let mut expired = Vec::new();
        for target in [Target::Cpu, Target::Gpu] {
            let slot = &mut self.slots[target.index()];
            if slot.installed
                && !slot.active
                && !slot.expired
                && slot.deadline_ns.is_some_and(|deadline| now_ns >= deadline)
            {
                slot.expired = true;
                slot.deadline_ns = None;
                self.ttl_failure.get_or_insert(target);
                expired.push(target);
            }
        }
        expired
    }

    fn next_deadline(&self) -> Option<u64> {
        self.slots
            .iter()
            .filter(|slot| slot.installed && !slot.active && !slot.expired)
            .filter_map(|slot| slot.deadline_ns)
            .min()
    }
}

struct RetainedWorker {
    worker: Arc<CaptureObservationWorker>,
    lease: WorkerObservationLease,
    generation_ref: String,
}

struct WarmSharedState {
    lifecycle: LifecycleState,
    workers: [Option<RetainedWorker>; 2],
    stop: bool,
    retirement_failed: bool,
}

struct WarmPool {
    origin: Instant,
    ttl_ns: u64,
    shared: Arc<(Mutex<WarmSharedState>, Condvar)>,
    watchdog: Option<JoinHandle<()>>,
}

impl WarmPool {
    fn start() -> Result<Self> {
        let origin = Instant::now();
        let ttl_ns = duration_nanos(WARM_MODEL_TTL);
        let shared = Arc::new((
            Mutex::new(WarmSharedState {
                lifecycle: LifecycleState::default(),
                workers: [None, None],
                stop: false,
                retirement_failed: false,
            }),
            Condvar::new(),
        ));
        let thread_shared = Arc::clone(&shared);
        let watchdog = std::thread::Builder::new()
            .name("scribe-capture-campaign-ttl".to_owned())
            .spawn(move || watchdog_loop(thread_shared, origin))
            .context("could not start the campaign warm-worker TTL watchdog")?;
        Ok(Self {
            origin,
            ttl_ns,
            shared,
            watchdog: Some(watchdog),
        })
    }

    fn tick(&self, at: Instant) -> u64 {
        duration_nanos(at.saturating_duration_since(self.origin))
    }

    fn install(
        &self,
        target: Target,
        worker: Arc<CaptureObservationWorker>,
        lease: WorkerObservationLease,
        generation_ref: String,
        result_at: Instant,
    ) -> Result<()> {
        let (lock, changed) = &*self.shared;
        let mut state = lock
            .lock()
            .map_err(|_| anyhow!("campaign warm-worker state lock was poisoned"))?;
        state
            .lifecycle
            .install(target, self.tick(result_at), self.ttl_ns)?;
        state.workers[target.index()] = Some(RetainedWorker {
            worker,
            lease,
            generation_ref,
        });
        changed.notify_all();
        Ok(())
    }

    fn begin(
        &self,
        target: Target,
        now: Instant,
    ) -> Result<(
        Arc<CaptureObservationWorker>,
        WorkerObservationLease,
        String,
    )> {
        self.retire_due(now)?;
        let (lock, changed) = &*self.shared;
        let mut state = lock
            .lock()
            .map_err(|_| anyhow!("campaign warm-worker state lock was poisoned"))?;
        if state.retirement_failed {
            bail!("campaign warm-worker retirement previously failed")
        }
        state.lifecycle.begin(target, self.tick(now))?;
        let retained = state.workers[target.index()]
            .as_ref()
            .ok_or_else(|| anyhow!("retained campaign worker is missing"))?;
        let result = (
            Arc::clone(&retained.worker),
            retained.lease.clone(),
            retained.generation_ref.clone(),
        );
        changed.notify_all();
        drop(state);
        if let Err(error) = result.1.require_current() {
            let _ = self.finish(target, Instant::now(), false);
            return Err(error.context("retained warm worker generation changed"));
        }
        Ok(result)
    }

    fn finish(&self, target: Target, result_at: Instant, succeeded: bool) -> Result<()> {
        let (lock, changed) = &*self.shared;
        let mut state = lock
            .lock()
            .map_err(|_| anyhow!("campaign warm-worker state lock was poisoned"))?;
        state
            .lifecycle
            .finish(target, self.tick(result_at), succeeded, self.ttl_ns)?;
        changed.notify_all();
        Ok(())
    }

    fn failure(&self) -> Result<Option<Target>> {
        let state = self
            .shared
            .0
            .lock()
            .map_err(|_| anyhow!("campaign warm-worker state lock was poisoned"))?;
        if state.retirement_failed {
            bail!("campaign warm-worker retirement failed")
        }
        Ok(state.lifecycle.ttl_failure)
    }

    fn retire_due(&self, now: Instant) -> Result<()> {
        let actions = {
            let mut state = self
                .shared
                .0
                .lock()
                .map_err(|_| anyhow!("campaign warm-worker state lock was poisoned"))?;
            let targets = state.lifecycle.expire_due(self.tick(now));
            targets
                .into_iter()
                .filter_map(|target| {
                    state.workers[target.index()]
                        .as_ref()
                        .map(|retained| (Arc::clone(&retained.worker), retained.lease.clone()))
                })
                .collect::<Vec<_>>()
        };
        for (worker, lease) in actions {
            if worker.retire_observation_generation(&lease).is_err()
                && let Ok(mut state) = self.shared.0.lock()
            {
                state.retirement_failed = true;
            }
        }
        self.shared.1.notify_all();
        Ok(())
    }

    fn stop_and_cleanup(mut self) -> [(Target, bool); 2] {
        {
            if let Ok(mut state) = self.shared.0.lock() {
                state.stop = true;
                self.shared.1.notify_all();
            }
        }
        let joined = self
            .watchdog
            .take()
            .is_some_and(|watchdog| watchdog.join().is_ok());
        let mut results = [(Target::Cpu, joined), (Target::Gpu, joined)];
        if let Ok(mut state) = self.shared.0.lock() {
            for target in [Target::Cpu, Target::Gpu] {
                if let Some(retained) = state.workers[target.index()].take()
                    && !cleanup_worker(&retained.worker, Some(&retained.lease))
                {
                    results[target.index()].1 = false;
                }
            }
            if state.retirement_failed {
                results[0].1 = false;
                results[1].1 = false;
            }
        } else {
            results[0].1 = false;
            results[1].1 = false;
        }
        results
    }
}

fn watchdog_loop(shared: Arc<(Mutex<WarmSharedState>, Condvar)>, origin: Instant) {
    loop {
        let actions = {
            let (lock, changed) = &*shared;
            let mut state = match lock.lock() {
                Ok(state) => state,
                Err(_) => return,
            };
            loop {
                if state.stop {
                    return;
                }
                let now_ns = duration_nanos(Instant::now().saturating_duration_since(origin));
                let expired = state.lifecycle.expire_due(now_ns);
                if !expired.is_empty() {
                    break expired
                        .into_iter()
                        .filter_map(|target| {
                            state.workers[target.index()].as_ref().map(|retained| {
                                (Arc::clone(&retained.worker), retained.lease.clone())
                            })
                        })
                        .collect::<Vec<_>>();
                }
                let wait = state
                    .lifecycle
                    .next_deadline()
                    .map(|deadline| Duration::from_nanos(deadline.saturating_sub(now_ns)))
                    .unwrap_or(WARM_MODEL_TTL);
                let waited = changed.wait_timeout(state, wait);
                match waited {
                    Ok((next, _)) => state = next,
                    Err(_) => return,
                }
            }
        };
        let mut failed = false;
        for (worker, lease) in actions {
            failed |= worker.retire_observation_generation(&lease).is_err();
        }
        if failed && let Ok(mut state) = shared.0.lock() {
            state.retirement_failed = true;
        }
        shared.1.notify_all();
    }
}

pub(super) fn run(options: CommandOptions) -> Result<()> {
    let expected_power = options
        .campaign_power
        .ok_or_else(|| anyhow!("campaign mode requires an expected power source"))?;
    let mut model = verify_input(
        &options.model,
        &options.model_sha256,
        super::MAX_MODEL_BYTES,
        "model",
    )?;
    let mut wav = verify_input(
        &options.wav,
        &options.wav_sha256,
        super::MAX_WAV_BYTES,
        "WAV",
    )?;
    require_retained_digest(&mut model)?;
    require_retained_digest(&mut wav)?;
    let artifact = RuntimeArtifact::Gguf(RuntimeModel {
        id: ModelId::new("windows-gpu-capture-campaign"),
        path: model.path.clone(),
        format: ArtifactFormat::Gguf,
        expected_size_bytes: model.size,
        expected_sha256: model.sha256.clone(),
    });
    let inputs = InputReport {
        model_sha256: model.sha256.clone(),
        wav_sha256: wav.sha256.clone(),
    };
    let cpu_factory = CaptureObservationWorkerFactory::cpu();
    let gpu_factory = match CaptureObservationWorkerFactory::gpu_exact(
        &options.gpu_pack_id,
        &options.gpu_backend,
        &options.gpu_device,
    ) {
        Ok(factory) => factory,
        Err(_) => {
            let mut builder = ReportBuilder::new(expected_power, inputs, None);
            builder.fail(FailureDescriptor::global(
                FailureStage::WorkerPreflight,
                FailureCategory::WorkerUnavailable,
            ));
            return publish_campaign(builder.report, &options.output);
        }
    };
    let mut builder =
        ReportBuilder::new(expected_power, inputs, gpu_factory.gpu_identity().cloned());

    if let Err(category) = power_now(expected_power) {
        builder.fail(FailureDescriptor::global(
            FailureStage::PowerPreflight,
            category,
        ));
        return publish_campaign(builder.report, &options.output);
    }

    if let Err(failure) =
        preflight_workers(&cpu_factory, &gpu_factory, expected_power, &mut builder)
    {
        builder.fail(failure);
        return publish_campaign(builder.report, &options.output);
    }

    // Preflight support has succeeded for both exact workers before the WAV is
    // decoded or any model-bearing command is sent.
    let audio = match wav
        .file
        .rewind()
        .map_err(anyhow::Error::new)
        .and_then(|()| PreparedAudio::from_wav_reader(&mut wav.file))
    {
        Ok(audio) => audio,
        Err(_) => {
            builder.fail(FailureDescriptor::global(
                FailureStage::AudioPreparation,
                FailureCategory::InvariantViolation,
            ));
            return publish_campaign(builder.report, &options.output);
        }
    };

    for spec in measured_specs(Phase::Cold, COLD_PAIRS) {
        if builder.report.incomplete {
            break;
        }
        let factory = match spec.target {
            Target::Cpu => &cpu_factory,
            Target::Gpu => &gpu_factory,
        };
        let (record, cleanup_ok) = run_cold(
            spec,
            factory,
            artifact.clone(),
            &audio,
            expected_power,
            &mut builder,
        );
        let failed = record.failure;
        builder.report.runs.push(record);
        if let Some(failure) = failed {
            builder.fail(failure);
        }
        if !cleanup_ok {
            builder.cleanup_failed(Some(spec.target), Some(Phase::Cold));
        }
    }

    if !builder.report.incomplete {
        match WarmPool::start() {
            Ok(pool) => run_retained_campaign(
                pool,
                &cpu_factory,
                &gpu_factory,
                artifact,
                &audio,
                expected_power,
                &mut builder,
            ),
            Err(_) => builder.fail(FailureDescriptor::global(
                FailureStage::WarmModelTtl,
                FailureCategory::WorkerUnavailable,
            )),
        }
    }

    publish_campaign(builder.report, &options.output)
}

fn preflight_workers(
    cpu_factory: &CaptureObservationWorkerFactory,
    gpu_factory: &CaptureObservationWorkerFactory,
    expected_power: CampaignPower,
    builder: &mut ReportBuilder,
) -> std::result::Result<(), FailureDescriptor> {
    let cpu = Arc::new(cpu_factory.spawn());
    let gpu = Arc::new(gpu_factory.spawn());
    let mut cpu_lease = None;
    let mut gpu_lease = None;
    let operation = (|| {
        power_now(expected_power).map_err(|category| {
            FailureDescriptor::global(FailureStage::PowerPreflight, category)
        })?;
        cpu_lease = Some(cpu.prepare_observation().map_err(|_| FailureDescriptor {
            stage: FailureStage::Handshake,
            category: FailureCategory::WorkerUnavailable,
            target: Some(Target::Cpu),
            phase: None,
            pair_index: None,
            order_in_pair: None,
        })?);
        builder
            .register_capture(
                Target::Cpu,
                CapturePurpose::Preflight,
                cpu_lease.as_ref().expect("preflight CPU lease retained"),
            )
            .map_err(|_| FailureDescriptor {
                stage: FailureStage::Handshake,
                category: FailureCategory::HandshakeInvalid,
                target: Some(Target::Cpu),
                phase: None,
                pair_index: None,
                order_in_pair: None,
            })?;
        gpu_lease = Some(gpu.prepare_observation().map_err(|_| FailureDescriptor {
            stage: FailureStage::Handshake,
            category: FailureCategory::WorkerUnavailable,
            target: Some(Target::Gpu),
            phase: None,
            pair_index: None,
            order_in_pair: None,
        })?);
        builder
            .register_capture(
                Target::Gpu,
                CapturePurpose::Preflight,
                gpu_lease.as_ref().expect("preflight GPU lease retained"),
            )
            .map_err(|_| FailureDescriptor {
                stage: FailureStage::Handshake,
                category: FailureCategory::HandshakeInvalid,
                target: Some(Target::Gpu),
                phase: None,
                pair_index: None,
                order_in_pair: None,
            })?;
        cpu.negotiate_runtime_observation_retained(
            cpu_lease.as_ref().expect("preflight CPU lease retained"),
        )
        .map_err(|_| FailureDescriptor {
            stage: FailureStage::ObservationNegotiation,
            category: FailureCategory::ObservationUnsupported,
            target: Some(Target::Cpu),
            phase: None,
            pair_index: None,
            order_in_pair: None,
        })?;
        gpu.negotiate_runtime_observation_retained(
            gpu_lease.as_ref().expect("preflight GPU lease retained"),
        )
        .map_err(|_| FailureDescriptor {
            stage: FailureStage::ObservationNegotiation,
            category: FailureCategory::ObservationUnsupported,
            target: Some(Target::Gpu),
            phase: None,
            pair_index: None,
            order_in_pair: None,
        })?;
        power_now(expected_power).map_err(|category| {
            FailureDescriptor::global(FailureStage::PowerPreflight, category)
        })?;
        Ok(())
    })();
    if let Err(failure) = operation {
        builder.fail(failure);
    }
    let gpu_clean = cleanup_worker(&gpu, gpu_lease.as_ref());
    let cpu_clean = cleanup_worker(&cpu, cpu_lease.as_ref());
    if !gpu_clean {
        builder.cleanup_failed(Some(Target::Gpu), None);
    }
    if !cpu_clean {
        builder.cleanup_failed(Some(Target::Cpu), None);
    }
    if builder.report.incomplete {
        return Err(builder.report.failure.unwrap_or(FailureDescriptor::global(
            FailureStage::Cleanup,
            FailureCategory::CleanupFailed,
        )));
    }
    operation
}

fn run_cold(
    spec: RunSpec,
    factory: &CaptureObservationWorkerFactory,
    artifact: RuntimeArtifact,
    audio: &PreparedAudio,
    expected_power: CampaignPower,
    builder: &mut ReportBuilder,
) -> (CampaignRunRecord, bool) {
    let power_before = PowerSource::current();
    let started = Instant::now();
    if let Err(category) = require_power(expected_power, power_before) {
        return (
            CampaignRunRecord::failed(
                spec,
                None,
                started.elapsed(),
                Some(power_before),
                None,
                FailureDescriptor::for_spec(spec, FailureStage::PowerEndpoint, category),
            ),
            true,
        );
    }
    let worker = Arc::new(factory.spawn());
    let lease = match worker.prepare_observation() {
        Ok(lease) => lease,
        Err(_) => {
            let failure = FailureDescriptor::for_spec(
                spec,
                FailureStage::Handshake,
                FailureCategory::WorkerUnavailable,
            );
            let record = CampaignRunRecord::failed(
                spec,
                None,
                started.elapsed(),
                Some(power_before),
                None,
                failure,
            );
            return (record, cleanup_worker(&worker, None));
        }
    };
    let generation_ref = match builder.register_capture(spec.target, CapturePurpose::Cold, &lease) {
        Ok(reference) => reference,
        Err(_) => {
            let failure = FailureDescriptor::for_spec(
                spec,
                FailureStage::Handshake,
                FailureCategory::HandshakeInvalid,
            );
            let record = CampaignRunRecord::failed(
                spec,
                None,
                started.elapsed(),
                Some(power_before),
                None,
                failure,
            );
            return (record, cleanup_worker(&worker, Some(&lease)));
        }
    };
    if worker
        .negotiate_runtime_observation_retained(&lease)
        .is_err()
    {
        let failure = FailureDescriptor::for_spec(
            spec,
            FailureStage::ObservationNegotiation,
            FailureCategory::ObservationUnsupported,
        );
        let record = CampaignRunRecord::failed(
            spec,
            Some(generation_ref),
            started.elapsed(),
            Some(power_before),
            None,
            failure,
        );
        return (record, cleanup_worker(&worker, Some(&lease)));
    }
    let record = observe_attempt(
        spec,
        &worker,
        &lease,
        generation_ref,
        artifact,
        audio,
        expected_power,
        started,
        Some(power_before),
        |_, _| Ok(()),
    );
    let clean = cleanup_worker(&worker, Some(&lease));
    (record, clean)
}

fn run_retained_campaign(
    pool: WarmPool,
    cpu_factory: &CaptureObservationWorkerFactory,
    gpu_factory: &CaptureObservationWorkerFactory,
    artifact: RuntimeArtifact,
    audio: &PreparedAudio,
    expected_power: CampaignPower,
    builder: &mut ReportBuilder,
) {
    for (order, target) in [Target::Cpu, Target::Gpu].into_iter().enumerate() {
        check_pool(&pool, builder, Phase::Prime);
        if builder.report.incomplete {
            break;
        }
        let spec = RunSpec {
            measured: false,
            phase: Phase::Prime,
            pair_index: None,
            order_in_pair: u8::try_from(order + 1).expect("prime order is bounded"),
            target,
        };
        let factory = match target {
            Target::Cpu => cpu_factory,
            Target::Gpu => gpu_factory,
        };
        let power_before = PowerSource::current();
        let started = Instant::now();
        if let Err(category) = require_power(expected_power, power_before) {
            let failure = FailureDescriptor::for_spec(spec, FailureStage::PowerEndpoint, category);
            builder.report.runs.push(CampaignRunRecord::failed(
                spec,
                None,
                started.elapsed(),
                Some(power_before),
                None,
                failure,
            ));
            builder.fail(failure);
            break;
        }
        let worker = Arc::new(factory.spawn());
        let lease = match worker.prepare_observation() {
            Ok(lease) => lease,
            Err(_) => {
                let failure = FailureDescriptor::for_spec(
                    spec,
                    FailureStage::Handshake,
                    FailureCategory::WorkerUnavailable,
                );
                builder.report.runs.push(CampaignRunRecord::failed(
                    spec,
                    None,
                    started.elapsed(),
                    Some(power_before),
                    None,
                    failure,
                ));
                builder.fail(failure);
                if !cleanup_worker(&worker, None) {
                    builder.cleanup_failed(Some(target), Some(Phase::Prime));
                }
                break;
            }
        };
        let generation_ref = match builder.register_capture(target, CapturePurpose::Prime, &lease) {
            Ok(reference) => reference,
            Err(_) => {
                let failure = FailureDescriptor::for_spec(
                    spec,
                    FailureStage::Handshake,
                    FailureCategory::HandshakeInvalid,
                );
                builder.report.runs.push(CampaignRunRecord::failed(
                    spec,
                    None,
                    started.elapsed(),
                    Some(power_before),
                    None,
                    failure,
                ));
                builder.fail(failure);
                if !cleanup_worker(&worker, Some(&lease)) {
                    builder.cleanup_failed(Some(target), Some(Phase::Prime));
                }
                break;
            }
        };
        if worker
            .negotiate_runtime_observation_retained(&lease)
            .is_err()
        {
            let failure = FailureDescriptor::for_spec(
                spec,
                FailureStage::ObservationNegotiation,
                FailureCategory::ObservationUnsupported,
            );
            builder.report.runs.push(CampaignRunRecord::failed(
                spec,
                Some(generation_ref),
                started.elapsed(),
                Some(power_before),
                None,
                failure,
            ));
            builder.fail(failure);
            if !cleanup_worker(&worker, Some(&lease)) {
                builder.cleanup_failed(Some(target), Some(Phase::Prime));
            }
            break;
        }
        check_pool(&pool, builder, Phase::Prime);
        if builder.report.incomplete {
            let failure = FailureDescriptor::for_spec(
                spec,
                FailureStage::WarmModelTtl,
                FailureCategory::TtlExpired,
            );
            builder.report.runs.push(CampaignRunRecord::failed(
                spec,
                Some(generation_ref),
                started.elapsed(),
                Some(power_before),
                None,
                failure,
            ));
            if !cleanup_worker(&worker, Some(&lease)) {
                builder.cleanup_failed(Some(target), Some(Phase::Prime));
            }
            break;
        }
        let install_worker = Arc::clone(&worker);
        let install_lease = lease.clone();
        let install_reference = generation_ref.clone();
        let record = observe_attempt(
            spec,
            &worker,
            &lease,
            generation_ref,
            artifact.clone(),
            audio,
            expected_power,
            started,
            Some(power_before),
            |result_at, succeeded| {
                if succeeded {
                    pool.install(
                        target,
                        install_worker,
                        install_lease,
                        install_reference,
                        result_at,
                    )
                } else {
                    Ok(())
                }
            },
        );
        let failure = record.failure;
        builder.report.runs.push(record);
        if let Some(failure) = failure {
            builder.fail(failure);
            if !cleanup_worker(&worker, Some(&lease)) {
                builder.cleanup_failed(Some(target), Some(Phase::Prime));
            }
            break;
        }
        check_pool(&pool, builder, Phase::Prime);
    }

    if !builder.report.incomplete {
        for spec in measured_specs(Phase::Warm, WARM_PAIRS) {
            if builder.report.incomplete {
                break;
            }
            let started = Instant::now();
            let (worker, lease, generation_ref) = match pool.begin(spec.target, started) {
                Ok(retained) => retained,
                Err(_) => {
                    let expired = pool.failure().ok().flatten().is_some();
                    let failure = FailureDescriptor::for_spec(
                        spec,
                        if expired {
                            FailureStage::WarmModelTtl
                        } else {
                            FailureStage::ObservationValidation
                        },
                        if expired {
                            FailureCategory::TtlExpired
                        } else {
                            FailureCategory::IdentityMismatch
                        },
                    );
                    builder.report.runs.push(CampaignRunRecord::failed(
                        spec,
                        None,
                        started.elapsed(),
                        None,
                        None,
                        failure,
                    ));
                    builder.fail(failure);
                    break;
                }
            };
            let record = observe_attempt(
                spec,
                &worker,
                &lease,
                generation_ref,
                artifact.clone(),
                audio,
                expected_power,
                started,
                None,
                |result_at, succeeded| pool.finish(spec.target, result_at, succeeded),
            );
            let failure = record.failure;
            builder.report.runs.push(record);
            if let Some(failure) = failure {
                builder.fail(failure);
            }
            check_pool(&pool, builder, Phase::Warm);
        }
    }

    for (target, clean) in pool.stop_and_cleanup() {
        if !clean {
            builder.cleanup_failed(Some(target), Some(Phase::Warm));
        }
    }
}

fn check_pool(pool: &WarmPool, builder: &mut ReportBuilder, phase: Phase) {
    let failure = pool
        .retire_due(Instant::now())
        .and_then(|()| pool.failure());
    match failure {
        Ok(Some(expired)) => {
            let mut failure =
                FailureDescriptor::global(FailureStage::WarmModelTtl, FailureCategory::TtlExpired);
            failure.target = Some(expired);
            failure.phase = Some(phase);
            builder.fail(failure);
        }
        Ok(None) => {}
        Err(_) => builder.cleanup_failed(None, Some(phase)),
    }
}

fn require_power(
    expected: CampaignPower,
    actual: PowerSource,
) -> std::result::Result<PowerSource, FailureCategory> {
    if actual == PowerSource::Unknown {
        Err(FailureCategory::PowerUnknown)
    } else if actual != expected.source() {
        Err(FailureCategory::PowerMismatch)
    } else {
        Ok(actual)
    }
}

fn power_now(expected: CampaignPower) -> std::result::Result<PowerSource, FailureCategory> {
    require_power(expected, PowerSource::current())
}

fn validate_diagnostics(
    phase: Phase,
    warm_reused: bool,
    model_load_ms: u128,
    backend_ms: u128,
) -> Result<(u64, u64)> {
    if warm_reused != (phase == Phase::Warm) || (warm_reused && model_load_ms != 0) {
        bail!("campaign model reuse does not match its measured phase")
    }
    Ok((u64::try_from(model_load_ms)?, u64::try_from(backend_ms)?))
}

fn terminal_attempt_failure(
    request_failure: Option<(FailureStage, FailureCategory)>,
    affinity_lease_stale: bool,
) -> Option<(FailureStage, FailureCategory)> {
    request_failure.or_else(|| {
        affinity_lease_stale.then_some((
            FailureStage::ObservationValidation,
            FailureCategory::IdentityMismatch,
        ))
    })
}

#[allow(
    clippy::too_many_arguments,
    reason = "one attempt carries its exact frozen worker lease, prepared inputs, timing boundary and result-time lifecycle callback"
)]
fn observe_attempt(
    spec: RunSpec,
    worker: &CaptureObservationWorker,
    lease: &WorkerObservationLease,
    generation_ref: String,
    artifact: RuntimeArtifact,
    audio: &PreparedAudio,
    expected_power: CampaignPower,
    started: Instant,
    initial_power: Option<PowerSource>,
    settled: impl FnOnce(Instant, bool) -> Result<()>,
) -> CampaignRunRecord {
    let mut power_before = initial_power;
    let mut sampler = None;
    let mut affinity_before = None;
    let execution = (|| {
        lease.require_current().map_err(|_| {
            (
                FailureStage::ObservationValidation,
                FailureCategory::IdentityMismatch,
            )
        })?;
        let before = PowerSource::current();
        power_before.get_or_insert(before);
        require_power(expected_power, before)
            .map_err(|category| (FailureStage::PowerEndpoint, category))?;
        affinity_before = Some(observe_process_affinity(lease).map_err(|_| {
            (
                FailureStage::ObservationValidation,
                FailureCategory::IdentityMismatch,
            )
        })?);
        sampler = Some(
            match spec.target {
                Target::Cpu => SamplingSession::cpu(lease.clone()),
                Target::Gpu => worker
                    .gpu_identity
                    .as_ref()
                    .ok_or_else(|| anyhow!("GPU observation has no frozen identity"))
                    .and_then(|identity| {
                        SamplingSession::gpu(lease.clone(), &identity.stable_device)
                    }),
            }
            .map_err(|_| {
                (
                    FailureStage::TelemetrySetup,
                    FailureCategory::TelemetryUnavailable,
                )
            })?,
        );
        worker
            .transcribe_observed_retained(lease, artifact, spec.target.preference(), audio)
            .map_err(|_| (FailureStage::Inference, FailureCategory::ObservationFailed))
    })();
    // The correlated response timestamp precedes sampler joining, endpoint
    // checks, validation, transcript hashing and report work. Notify the TTL
    // owner now, so opposite-target work cannot extend this idle lifetime.
    let result_at = execution
        .as_ref()
        .map_or_else(|_| Instant::now(), |result| result.result_at);
    let elapsed = result_at.saturating_duration_since(started);
    let lifecycle = settled(result_at, execution.is_ok());
    let affinity_after = affinity_before
        .as_ref()
        .map(|_| observe_process_affinity(lease));
    let power_after = sampler.as_ref().map(|_| PowerSource::current());
    let telemetry = sampler.map(SamplingSession::finish);
    let failed = |stage, category| {
        let mut record = CampaignRunRecord::failed(
            spec,
            Some(generation_ref.clone()),
            elapsed,
            power_before,
            power_after,
            FailureDescriptor::for_spec(spec, stage, category),
        );
        record.worker_process_affinity = process_affinity_pair(
            affinity_before.as_ref(),
            affinity_after
                .as_ref()
                .and_then(|result| result.as_ref().ok()),
        );
        record
    };
    let request_failure = execution.as_ref().err().copied();
    if let Some((stage, category)) = terminal_attempt_failure(
        request_failure,
        affinity_after
            .as_ref()
            .is_some_and(|result| result.is_err()),
    ) {
        return failed(stage, category);
    }
    let observed = execution.expect("attempt failure was handled above");
    if lifecycle.is_err() {
        return failed(
            FailureStage::WarmModelTtl,
            FailureCategory::InvariantViolation,
        );
    }
    let before = power_before.expect("successful request captured its initial power");
    let after = power_after.expect("successful request started its sampler");
    if let Err(category) = require_power(expected_power, after) {
        let category = if after != PowerSource::Unknown && after != before {
            FailureCategory::PowerChanged
        } else {
            category
        };
        return failed(FailureStage::PowerEndpoint, category);
    }
    let telemetry = match telemetry {
        Some(Ok(telemetry)) if telemetry.sample_count > 0 => telemetry,
        _ => {
            return failed(
                FailureStage::TelemetryFinish,
                FailureCategory::TelemetryUnavailable,
            );
        }
    };
    if lease.require_current().is_err() {
        return failed(
            FailureStage::ObservationValidation,
            FailureCategory::IdentityMismatch,
        );
    }
    let warm_reused = observed.execution.diagnostics.warm_reused;
    let (model_load_ms, backend_ms) = match validate_diagnostics(
        spec.phase,
        warm_reused,
        observed.execution.diagnostics.model_load_duration_ms,
        observed.execution.processing_duration_ms,
    ) {
        Ok(values) => values,
        Err(_) => {
            return failed(
                FailureStage::ObservationValidation,
                FailureCategory::DiagnosticsMismatch,
            );
        }
    };
    let worker_report = build_worker_report(
        lease,
        observed.execution,
        WorkerObservationMeasurements {
            provider_memory: ProviderMemoryReport {
                before: observed.before,
                after: observed.after,
            },
            memory_availability: MemoryAvailabilityReport {
                before: observed.availability_before,
                after: observed.availability_after,
            },
            telemetry,
            power_source_before: before,
            power_source_after: after,
            elapsed_ms: elapsed.as_millis(),
        },
        spec.target == Target::Gpu,
        worker.gpu_identity.as_ref(),
    );
    let report = match worker_report {
        Ok(observed) => observed.report,
        Err(_) => {
            return failed(
                FailureStage::ObservationValidation,
                FailureCategory::ObservationFailed,
            );
        }
    };
    CampaignRunRecord {
        measured: spec.measured,
        phase: spec.phase,
        pair_index: spec.pair_index,
        order_in_pair: spec.order_in_pair,
        target: spec.target,
        generation_ref: Some(generation_ref),
        status: RecordStatus::Succeeded,
        power_source_before: Some(before),
        power_source_after: Some(after),
        end_to_end_ms: report.elapsed_ms,
        backend_ms: Some(backend_ms),
        model_load_ms: Some(model_load_ms),
        warm_reused: Some(warm_reused),
        sampled_max_private_usage_bytes: Some(report.sampled_max_private_usage_bytes),
        telemetry_sample_count: Some(report.telemetry_sample_count),
        video_memory: Some(report.video_memory),
        raw_provider_memory: Some(report.provider_memory),
        memory_availability: Some(report.memory_availability),
        worker_process_affinity: process_affinity_pair(
            affinity_before.as_ref(),
            affinity_after
                .as_ref()
                .and_then(|result| result.as_ref().ok()),
        ),
        normalized_transcript_sha256: Some(report.normalized_transcript_sha256),
        failure: None,
    }
}

fn cleanup_worker(
    worker: &CaptureObservationWorker,
    lease: Option<&WorkerObservationLease>,
) -> bool {
    // Always attempt forced exact-generation retirement after graceful shutdown,
    // including when shutdown fails. Neither path may acquire a new generation.
    let graceful = worker.shutdown().is_ok();
    let retired = lease.is_none_or(|lease| worker.retire_observation_generation(lease).is_ok());
    graceful && retired
}

fn campaign_specs() -> Vec<RunSpec> {
    let mut specs = measured_specs(Phase::Cold, COLD_PAIRS);
    specs.extend(
        [Target::Cpu, Target::Gpu]
            .into_iter()
            .enumerate()
            .map(|(order, target)| RunSpec {
                measured: false,
                phase: Phase::Prime,
                pair_index: None,
                order_in_pair: (order + 1) as u8,
                target,
            }),
    );
    specs.extend(measured_specs(Phase::Warm, WARM_PAIRS));
    specs
}

fn validate_report(report: &CampaignReport) -> Result<()> {
    if report.captures.len() > MAX_CAPTURES || report.runs.len() > MAX_RECORDS {
        bail!("campaign report exceeds its record bounds")
    }
    if report.incomplete != report.failure.is_some()
        || (!report.cleanup_complete && !report.incomplete)
    {
        bail!("campaign report has inconsistent failure state")
    }
    let expected_captures = [
        (Target::Cpu, CapturePurpose::Preflight),
        (Target::Gpu, CapturePurpose::Preflight),
    ]
    .into_iter()
    .chain(
        measured_specs(Phase::Cold, COLD_PAIRS)
            .into_iter()
            .map(|spec| (spec.target, CapturePurpose::Cold)),
    )
    .chain([
        (Target::Cpu, CapturePurpose::Prime),
        (Target::Gpu, CapturePurpose::Prime),
    ])
    .collect::<Vec<_>>();
    for (index, capture) in report.captures.iter().enumerate() {
        if usize::from(capture.logical_sequence) != index + 1
            || (capture.target, capture.purpose) != expected_captures[index]
            || capture.generation_ref.len() > MAX_REFERENCE_BYTES
            || capture.generation_ref
                != format!(
                    "generation-{:02}-{}",
                    index + 1,
                    capture.digest_sha256.get(..24).unwrap_or("")
                )
            || super::canonical_sha256(&capture.digest_sha256).is_err()
            || [&capture.hello_frame_hex, &capture.ready_frame_hex]
                .into_iter()
                .any(|frame| {
                    frame.len() < 52
                        || frame.len() > MAX_FRAME_BYTES * 2
                        || frame.len() % 2 != 0
                        || !frame.starts_with("53434946")
                        || !frame
                            .bytes()
                            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                })
            || report.captures[..index]
                .iter()
                .any(|previous| previous.digest_sha256 == capture.digest_sha256)
        {
            bail!("campaign report contains invalid generation captures")
        }
    }
    let specs = campaign_specs();
    let mut primed = [None, None];
    for (index, record) in report.runs.iter().enumerate() {
        let spec = specs[index];
        if (
            record.measured,
            record.phase,
            record.pair_index,
            record.order_in_pair,
            record.target,
        ) != (
            spec.measured,
            spec.phase,
            spec.pair_index,
            spec.order_in_pair,
            spec.target,
        ) {
            bail!("campaign request order is not a prefix of the fixed sequence")
        }
        let capture = record
            .generation_ref
            .as_ref()
            .map(|reference| {
                report
                    .captures
                    .iter()
                    .find(|capture| &capture.generation_ref == reference)
                    .ok_or_else(|| anyhow!("campaign request references an absent capture"))
            })
            .transpose()?;
        if let Some(capture) = capture {
            let purpose = if record.phase == Phase::Cold {
                CapturePurpose::Cold
            } else {
                CapturePurpose::Prime
            };
            if capture.target != record.target || capture.purpose != purpose {
                bail!("campaign request references a different worker target or phase")
            }
            match record.phase {
                Phase::Prime => {
                    primed[record.target.index()] = Some(capture.generation_ref.as_str())
                }
                Phase::Warm
                    if primed[record.target.index()] != Some(capture.generation_ref.as_str()) =>
                {
                    bail!("campaign warm request replaced its primed generation")
                }
                Phase::Cold
                    if report.runs[..index]
                        .iter()
                        .any(|previous| previous.generation_ref == record.generation_ref) =>
                {
                    bail!("campaign cold requests reused a generation")
                }
                _ => {}
            }
        }
        if record
            .worker_process_affinity
            .as_ref()
            .is_some_and(|pair| validate_process_affinity_pair(pair).is_err())
        {
            bail!("campaign request contains invalid affinity observations")
        }
        if record.status == RecordStatus::Failed {
            if record.failure.is_none() || !report.incomplete || index + 1 != report.runs.len() {
                bail!("campaign failed request is missing its terminal failure")
            }
            continue;
        }
        if capture.is_none()
            || record.failure.is_some()
            || record.power_source_before != Some(report.expected_power.source())
            || record.power_source_after != Some(report.expected_power.source())
            || record.backend_ms.is_none()
            || record.model_load_ms.is_none()
            || record.warm_reused != Some(record.phase == Phase::Warm)
            || (record.phase == Phase::Warm && record.model_load_ms != Some(0))
            || record.sampled_max_private_usage_bytes.is_none()
            || record.telemetry_sample_count.is_none_or(|count| count == 0)
            || record.video_memory.is_none()
            || record.raw_provider_memory.is_none()
            || record.memory_availability.is_none()
            || record
                .worker_process_affinity
                .as_ref()
                .is_none_or(|pair| pair.before.is_none() || pair.after.is_none())
            || record
                .normalized_transcript_sha256
                .as_ref()
                .is_none_or(|digest| super::canonical_sha256(digest).is_err())
        {
            bail!("campaign successful request omitted verified observations")
        }
    }
    if !report.incomplete
        && (report.captures.len() != MAX_CAPTURES
            || report.runs.len() != MAX_RECORDS
            || report.gpu_identity.is_none())
    {
        bail!("campaign success omitted required generations or requests")
    }
    Ok(())
}

fn serialize_report(report: &CampaignReport) -> Result<Vec<u8>> {
    validate_report(report)?;
    let mut bytes =
        serde_json::to_vec(report).context("could not serialize the campaign report")?;
    bytes.push(b'\n');
    if bytes.len() > MAX_CAMPAIGN_REPORT_BYTES {
        bail!("campaign report exceeds its byte bound")
    }
    Ok(bytes)
}

fn publish_campaign(report: CampaignReport, output: &Path) -> Result<()> {
    let bytes = serialize_report(&report)?;
    publish_atomic_new(output, &bytes)?;
    if report.incomplete {
        bail!("campaign incomplete; categorized diagnostic report published after cleanup")
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::onnx_worker::{
        ProviderMemoryNotApplicableReason, ProviderMemoryObservation, WorkerMemoryAvailability,
    };

    fn builder() -> ReportBuilder {
        ReportBuilder::new(
            CampaignPower::Ac,
            InputReport {
                model_sha256: "a".repeat(64),
                wav_sha256: "b".repeat(64),
            },
            Some(GpuCaptureObservationIdentity {
                backend: "vulkan".into(),
                provider: "fixture-provider".into(),
                stable_device: "native:luid:0102030405060708".into(),
                driver: "fixture-driver".into(),
                device_class: "integrated_gpu".into(),
                vendor: "intel".into(),
                memory_total_bytes: 1024,
                pack_id: "fixture-pack".into(),
                pack_version: "1".into(),
                pack_sha256: "c".repeat(64),
                pack_security_epoch: 1,
                runtime_abi: 1,
            }),
        )
    }

    fn capture(report: &mut CampaignReport, target: Target, purpose: CapturePurpose) -> String {
        let sequence = report.captures.len() + 1;
        let mut hello = vec![0_u8; 26];
        hello[..4].copy_from_slice(b"SCIF");
        hello[25] = sequence as u8;
        let mut ready = hello.clone();
        ready[24] = 1;
        let digest_sha256 = handshake_digest(&hello, &ready);
        let generation_ref = format!("generation-{sequence:02}-{}", &digest_sha256[..24]);
        report.captures.push(HandshakeCapture {
            logical_sequence: sequence as u8,
            generation_ref: generation_ref.clone(),
            digest_sha256,
            target,
            purpose,
            hello_frame_hex: hex(&hello),
            ready_frame_hex: hex(&ready),
        });
        generation_ref
    }

    fn complete_report() -> CampaignReport {
        let mut report = builder().report;
        capture(&mut report, Target::Cpu, CapturePurpose::Preflight);
        capture(&mut report, Target::Gpu, CapturePurpose::Preflight);
        let mut prime_refs = [String::new(), String::new()];
        for spec in campaign_specs() {
            let reference = match spec.phase {
                Phase::Cold => capture(&mut report, spec.target, CapturePurpose::Cold),
                Phase::Prime => {
                    let reference = capture(&mut report, spec.target, CapturePurpose::Prime);
                    prime_refs[spec.target.index()] = reference.clone();
                    reference
                }
                Phase::Warm => prime_refs[spec.target.index()].clone(),
            };
            let (provider, availability, video_memory) = match spec.target {
                Target::Cpu => (
                    ProviderMemoryObservation::NotApplicable {
                        reason: ProviderMemoryNotApplicableReason::CpuProvider,
                    },
                    WorkerMemoryAvailability::NotApplicable {
                        reason: ProviderMemoryNotApplicableReason::CpuProvider,
                    },
                    VideoMemoryReport::NotApplicable,
                ),
                Target::Gpu => {
                    let segment = super::super::telemetry::SegmentSummary {
                        sampled_max_current_usage_bytes: 1,
                        sampled_max_current_reservation_bytes: 0,
                        sampled_min_budget_bytes: 1024,
                        sampled_min_available_for_reservation_bytes: 512,
                    };
                    (
                        ProviderMemoryObservation::Unavailable {
                            reason: crate::onnx_worker::ProviderMemoryUnavailableReason::ProviderQueryFailed,
                        },
                        WorkerMemoryAvailability::Unavailable {
                            reason: crate::onnx_worker::WorkerMemoryUnavailableReason::ProviderQueryFailed,
                        },
                        VideoMemoryReport::Available { local: segment.clone(), non_local: segment },
                    )
                }
            };
            report.runs.push(CampaignRunRecord {
                measured: spec.measured,
                phase: spec.phase,
                pair_index: spec.pair_index,
                order_in_pair: spec.order_in_pair,
                target: spec.target,
                generation_ref: Some(reference),
                status: RecordStatus::Succeeded,
                power_source_before: Some(PowerSource::Ac),
                power_source_after: Some(PowerSource::Ac),
                end_to_end_ms: 0,
                backend_ms: Some(0),
                model_load_ms: Some(0),
                warm_reused: Some(spec.phase == Phase::Warm),
                sampled_max_private_usage_bytes: Some(1),
                telemetry_sample_count: Some(1),
                video_memory: Some(video_memory),
                raw_provider_memory: Some(ProviderMemoryReport {
                    before: provider.clone(),
                    after: provider,
                }),
                memory_availability: Some(MemoryAvailabilityReport {
                    before: availability.clone(),
                    after: availability,
                }),
                worker_process_affinity: process_affinity_pair(
                    Some(&ProcessAffinityObservation::Available {
                        processor_group: 0,
                        process_mask_hex: format!(
                            "{:0width$x}",
                            0xf,
                            width = std::mem::size_of::<usize>() * 2
                        ),
                        system_mask_hex: format!(
                            "{:0width$x}",
                            0xff,
                            width = std::mem::size_of::<usize>() * 2
                        ),
                    }),
                    Some(&ProcessAffinityObservation::Available {
                        processor_group: 0,
                        process_mask_hex: format!(
                            "{:0width$x}",
                            0xf,
                            width = std::mem::size_of::<usize>() * 2
                        ),
                        system_mask_hex: format!(
                            "{:0width$x}",
                            0xff,
                            width = std::mem::size_of::<usize>() * 2
                        ),
                    }),
                ),
                normalized_transcript_sha256: Some("d".repeat(64)),
                failure: None,
            });
        }
        report
    }

    #[test]
    fn sequence_has_five_cold_twenty_warm_pairs_and_two_distinct_primes() {
        let specs = campaign_specs();
        assert_eq!(specs.len(), 52);
        assert_eq!(specs.iter().filter(|spec| spec.measured).count(), 50);
        for target in [Target::Cpu, Target::Gpu] {
            assert_eq!(
                specs
                    .iter()
                    .filter(|spec| spec.target == target && spec.phase == Phase::Cold)
                    .count(),
                5
            );
            assert_eq!(
                specs
                    .iter()
                    .filter(|spec| spec.target == target && spec.phase == Phase::Prime)
                    .count(),
                1
            );
            assert_eq!(
                specs
                    .iter()
                    .filter(|spec| spec.target == target && spec.phase == Phase::Warm)
                    .count(),
                20
            );
        }
        for (offset, pairs) in [(0, 5), (12, 20)] {
            for pair in 0..pairs {
                let first = if pair % 2 == 0 {
                    Target::Cpu
                } else {
                    Target::Gpu
                };
                assert_eq!(specs[offset + pair * 2].target, first);
                assert_ne!(specs[offset + pair * 2 + 1].target, first);
                assert_eq!(specs[offset + pair * 2].pair_index, Some(pair as u8 + 1));
                assert_eq!(specs[offset + pair * 2 + 1].order_in_pair, 2);
            }
        }
        assert!(!specs[10].measured && !specs[11].measured);
    }

    #[test]
    fn idle_ttl_starts_at_the_result_not_finalization_or_opposite_prime() {
        assert_eq!(WARM_MODEL_TTL, Duration::from_secs(300));
        let mut lifecycle = LifecycleState::default();
        lifecycle.install(Target::Cpu, 10, 300_000).unwrap();
        assert_eq!(lifecycle.next_deadline(), Some(300_010));
        assert!(lifecycle.expire_due(300_009).is_empty());
        // GPU is still being primed: CPU must expire independently.
        assert_eq!(lifecycle.expire_due(300_010), vec![Target::Cpu]);
        lifecycle.install(Target::Gpu, 400_000, 300_000).unwrap();
        assert!(lifecycle.begin(Target::Gpu, 400_001).is_err());
        assert!(lifecycle.install(Target::Cpu, 400_001, 300_000).is_err());
        assert!(lifecycle.expire_due(400_001).is_empty());
        assert_eq!(lifecycle.ttl_failure, Some(Target::Cpu));
    }

    #[test]
    fn opposite_idle_expiry_does_not_abort_the_active_request() {
        let mut lifecycle = LifecycleState::default();
        lifecycle.install(Target::Cpu, 0, 100).unwrap();
        lifecycle.install(Target::Gpu, 20, 100).unwrap();
        lifecycle.begin(Target::Gpu, 30).unwrap();
        assert!(lifecycle.begin(Target::Cpu, 31).is_err());
        assert_eq!(lifecycle.expire_due(500), vec![Target::Cpu]);
        assert!(lifecycle.slots[Target::Gpu.index()].active);
        lifecycle.finish(Target::Gpu, 600, true, 100).unwrap();
        assert_eq!(lifecycle.slots[Target::Gpu.index()].deadline_ns, Some(700));
        assert!(lifecycle.begin(Target::Gpu, 601).is_err());
    }

    #[test]
    fn lifecycle_rejects_expired_start_unprimed_overlap_and_duplicate_completion() {
        let mut lifecycle = LifecycleState::default();
        assert!(lifecycle.begin(Target::Cpu, 0).is_err());
        lifecycle.install(Target::Cpu, 10, 100).unwrap();
        lifecycle.install(Target::Gpu, 20, 100).unwrap();
        lifecycle.begin(Target::Cpu, 30).unwrap();
        assert!(lifecycle.begin(Target::Gpu, 31).is_err());
        assert!(lifecycle.finish(Target::Gpu, 40, true, 100).is_err());
        lifecycle.finish(Target::Cpu, 40, false, 100).unwrap();
        assert!(lifecycle.finish(Target::Cpu, 40, true, 100).is_err());
        assert_eq!(lifecycle.slots[0].deadline_ns, None);
        assert!(lifecycle.begin(Target::Gpu, 120).is_err());
        assert_eq!(lifecycle.ttl_failure, Some(Target::Gpu));
    }

    #[test]
    fn watchdog_runs_while_the_opposite_request_is_active() {
        let mut lifecycle = LifecycleState::default();
        lifecycle.install(Target::Cpu, 0, 1).unwrap();
        lifecycle.install(Target::Gpu, 0, 1).unwrap();
        lifecycle.begin(Target::Gpu, 0).unwrap();
        let shared = Arc::new((
            Mutex::new(WarmSharedState {
                lifecycle,
                workers: [None, None],
                stop: false,
                retirement_failed: false,
            }),
            Condvar::new(),
        ));
        let thread_shared = Arc::clone(&shared);
        let origin = Instant::now() - Duration::from_secs(1);
        let watchdog = std::thread::spawn(move || watchdog_loop(thread_shared, origin));
        let state = shared.0.lock().unwrap();
        let (mut state, wait) = shared
            .1
            .wait_timeout_while(state, Duration::from_secs(2), |state| {
                state.lifecycle.ttl_failure.is_none()
            })
            .unwrap();
        let expired = state.lifecycle.ttl_failure;
        let active = state.lifecycle.active;
        state.stop = true;
        shared.1.notify_all();
        drop(state);
        watchdog.join().unwrap();
        assert!(!wait.timed_out());
        assert_eq!(expired, Some(Target::Cpu));
        assert_eq!(active, Some(Target::Gpu));
    }

    #[test]
    fn power_is_exact_and_unknown_never_counts_as_a_measurement() {
        assert_eq!(
            require_power(CampaignPower::Ac, PowerSource::Ac),
            Ok(PowerSource::Ac)
        );
        assert_eq!(
            require_power(CampaignPower::Battery, PowerSource::Battery),
            Ok(PowerSource::Battery)
        );
        assert_eq!(
            require_power(CampaignPower::Ac, PowerSource::Unknown),
            Err(FailureCategory::PowerUnknown)
        );
        assert_eq!(
            require_power(CampaignPower::Ac, PowerSource::Battery),
            Err(FailureCategory::PowerMismatch)
        );
        assert_eq!(
            require_power(CampaignPower::Battery, PowerSource::Ac),
            Err(FailureCategory::PowerMismatch)
        );
    }

    #[test]
    fn diagnostics_preserve_real_zero_and_reject_reload_or_integer_overflow() {
        assert_eq!(
            validate_diagnostics(Phase::Cold, false, 0, 0).unwrap(),
            (0, 0)
        );
        assert_eq!(
            validate_diagnostics(Phase::Prime, false, 17, 5).unwrap(),
            (17, 5)
        );
        assert_eq!(
            validate_diagnostics(Phase::Warm, true, 0, 0).unwrap(),
            (0, 0)
        );
        assert!(validate_diagnostics(Phase::Warm, false, 0, 1).is_err());
        assert!(validate_diagnostics(Phase::Warm, true, 1, 1).is_err());
        assert!(validate_diagnostics(Phase::Cold, true, 0, 1).is_err());
        assert!(validate_diagnostics(Phase::Prime, true, 0, 1).is_err());
        assert!(validate_diagnostics(Phase::Cold, false, u128::MAX, 0).is_err());
        assert!(validate_diagnostics(Phase::Cold, false, 0, u128::MAX).is_err());
    }

    #[test]
    fn request_failure_precedes_post_query_stale_identity() {
        let primary = (FailureStage::Inference, FailureCategory::ObservationFailed);
        assert_eq!(terminal_attempt_failure(Some(primary), true), Some(primary));
        assert_eq!(
            terminal_attempt_failure(None, true),
            Some((
                FailureStage::ObservationValidation,
                FailureCategory::IdentityMismatch
            ))
        );
        assert_eq!(terminal_attempt_failure(None, false), None);
    }

    #[test]
    fn complete_report_is_bounded_unsigned_and_keeps_environment_unknown() {
        let report = complete_report();
        let bytes = serialize_report(&report).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value["kind"], KIND);
        assert_eq!(value["schema_version"], 2);
        assert_eq!(value["unsigned"], true);
        assert_eq!(value["unqualified"], true);
        assert_eq!(value["auto_eligible"], false);
        assert_eq!(value["release_approved"], false);
        assert_eq!(value["captures"].as_array().unwrap().len(), 14);
        assert_eq!(value["runs"].as_array().unwrap().len(), 52);
        assert_eq!(value["runs"][0]["end_to_end_ms"], 0);
        assert_eq!(value["runs"][0]["backend_ms"], 0);
        assert_eq!(
            value["runs"][0]["worker_process_affinity"]["changed"],
            false
        );
        for field in [
            "background_load",
            "host_control",
            "affinity_control",
            "power_plan",
        ] {
            assert_eq!(
                value["environmental_controls"][field]["status"],
                "unavailable"
            );
        }
        assert_eq!(
            value["unavailable"]["thermal_state"]["reason"],
            "not_observed"
        );
        assert!(bytes.len() < MAX_CAMPAIGN_REPORT_BYTES);
    }

    #[test]
    fn campaign_affinity_reports_changes_and_keeps_unavailable_endpoints_unknown() {
        let mask = |value| format!("{value:0width$x}", width = std::mem::size_of::<usize>() * 2);
        let first = ProcessAffinityObservation::Available {
            processor_group: 0,
            process_mask_hex: mask(0xf),
            system_mask_hex: mask(0xff),
        };
        let second = ProcessAffinityObservation::Available {
            processor_group: 0,
            process_mask_hex: mask(0x3),
            system_mask_hex: mask(0xff),
        };
        assert_eq!(
            process_affinity_pair(Some(&first), Some(&second))
                .expect("complete pair")
                .changed,
            Some(true)
        );
        let unavailable = ProcessAffinityObservation::Unavailable {
            reason:
                crate::windows_gpu_capture::telemetry::ProcessAffinityUnavailableReason::AffinityMaskQueryFailed,
        };
        assert_eq!(
            process_affinity_pair(Some(&first), Some(&unavailable))
                .expect("complete pair")
                .changed,
            None
        );

        let before_only = process_affinity_pair(Some(&first), None).expect("known prefix");
        let mut partial = builder();
        let spec = campaign_specs()[0];
        let failure = FailureDescriptor::for_spec(
            spec,
            FailureStage::ObservationValidation,
            FailureCategory::IdentityMismatch,
        );
        partial.fail(failure);
        let mut failed = CampaignRunRecord::failed(
            spec,
            None,
            Duration::ZERO,
            Some(PowerSource::Ac),
            None,
            failure,
        );
        failed.worker_process_affinity = Some(before_only);
        partial.report.runs.push(failed);
        let serialized: serde_json::Value =
            serde_json::from_slice(&serialize_report(&partial.report).unwrap()).unwrap();
        let serialized = &serialized["runs"][0]["worker_process_affinity"];
        assert_eq!(serialized["before"]["status"], "available");
        assert!(serialized["after"].is_null());
        assert!(serialized["changed"].is_null());

        let mut invalid = complete_report();
        invalid.runs[0]
            .worker_process_affinity
            .as_mut()
            .expect("fixture affinity")
            .changed = Some(true);
        assert!(validate_report(&invalid).is_err());

        let mut fabricated_success = complete_report();
        fabricated_success.runs[0]
            .worker_process_affinity
            .as_mut()
            .expect("fixture affinity")
            .after = None;
        assert!(validate_report(&fabricated_success).is_err());
    }

    #[test]
    fn campaign_affinity_report_contains_no_process_identity_or_paths() {
        let bytes = serialize_report(&complete_report()).unwrap();
        let json = String::from_utf8(bytes).unwrap();
        for forbidden in [
            "\"process_id\"",
            "\"pid\"",
            "\"process_handle\"",
            "\"model_path\"",
            "\"wav_path\"",
        ] {
            assert!(!json.contains(forbidden));
        }
    }

    #[test]
    fn campaign_report_rejects_malformed_or_fabricated_affinity_facts() {
        enum Mutation {
            Uppercase,
            NonHex,
            WrongWidth,
            ZeroProcess,
            ZeroSystem,
            ProcessOutsideSystem,
            NonzeroGroup,
            MissingBefore,
            MissingAfter,
            UnavailableWithChanged,
        }

        for mutation in [
            Mutation::Uppercase,
            Mutation::NonHex,
            Mutation::WrongWidth,
            Mutation::ZeroProcess,
            Mutation::ZeroSystem,
            Mutation::ProcessOutsideSystem,
            Mutation::NonzeroGroup,
            Mutation::MissingBefore,
            Mutation::MissingAfter,
            Mutation::UnavailableWithChanged,
        ] {
            let mut report = complete_report();
            let pair = report.runs[0]
                .worker_process_affinity
                .as_mut()
                .expect("fixture affinity");
            match mutation {
                Mutation::MissingBefore => pair.before = None,
                Mutation::MissingAfter => pair.after = None,
                Mutation::UnavailableWithChanged => {
                    pair.before = Some(ProcessAffinityObservation::Unavailable {
                        reason: crate::windows_gpu_capture::telemetry::ProcessAffinityUnavailableReason::AffinityMaskQueryFailed,
                    });
                    pair.changed = Some(false);
                }
                mutation => {
                    let Some(ProcessAffinityObservation::Available {
                        processor_group,
                        process_mask_hex,
                        system_mask_hex,
                    }) = pair.before.as_mut()
                    else {
                        panic!("fixture affinity endpoint must be available")
                    };
                    match mutation {
                        Mutation::Uppercase => {
                            *process_mask_hex = process_mask_hex.to_ascii_uppercase()
                        }
                        Mutation::NonHex => process_mask_hex.replace_range(..1, "g"),
                        Mutation::WrongWidth => {
                            process_mask_hex.pop();
                        }
                        Mutation::ZeroProcess => {
                            *process_mask_hex = "0".repeat(std::mem::size_of::<usize>() * 2)
                        }
                        Mutation::ZeroSystem => {
                            *system_mask_hex = "0".repeat(std::mem::size_of::<usize>() * 2)
                        }
                        Mutation::ProcessOutsideSystem => {
                            let width = std::mem::size_of::<usize>() * 2;
                            *process_mask_hex = format!("{:0width$x}", 4);
                            *system_mask_hex = format!("{:0width$x}", 3);
                        }
                        Mutation::NonzeroGroup => *processor_group = 1,
                        Mutation::MissingBefore
                        | Mutation::MissingAfter
                        | Mutation::UnavailableWithChanged => unreachable!(),
                    }
                }
            }
            assert!(validate_report(&report).is_err());
        }
    }

    #[test]
    fn report_rejects_missing_duplicate_replaced_and_wrong_target_generations() {
        let mut missing = complete_report();
        missing.captures.pop();
        assert!(validate_report(&missing).is_err());
        let mut reused = complete_report();
        reused.runs[2].generation_ref = reused.runs[0].generation_ref.clone();
        assert!(validate_report(&reused).is_err());
        let mut replaced = complete_report();
        replaced.runs[12].generation_ref = replaced.runs[0].generation_ref.clone();
        assert!(validate_report(&replaced).is_err());
        let mut wrong = complete_report();
        wrong.runs[12].generation_ref = wrong.runs[11].generation_ref.clone();
        assert!(validate_report(&wrong).is_err());
        let mut duplicate = complete_report();
        duplicate.captures[1] = duplicate.captures[0].clone();
        assert!(validate_report(&duplicate).is_err());
    }

    #[test]
    fn report_rejects_unattempted_padding_reordered_runs_and_oversized_captures() {
        let mut short = complete_report();
        short.runs.pop();
        assert!(validate_report(&short).is_err());
        let mut reordered = complete_report();
        reordered.runs.swap(0, 1);
        assert!(validate_report(&reordered).is_err());
        let mut oversized = complete_report();
        oversized.captures[0].hello_frame_hex = "0".repeat(MAX_FRAME_BYTES * 2 + 2);
        assert!(serialize_report(&oversized).is_err());
        let mut extra = complete_report();
        extra.captures.push(extra.captures[0].clone());
        assert!(validate_report(&extra).is_err());
    }

    #[test]
    fn failed_prefix_has_typed_failure_and_no_fabricated_success_values() {
        let mut builder = builder();
        let spec = campaign_specs()[0];
        let failure = FailureDescriptor::for_spec(
            spec,
            FailureStage::Inference,
            FailureCategory::ObservationFailed,
        );
        builder.fail(failure);
        builder.report.runs.push(CampaignRunRecord::failed(
            spec,
            None,
            Duration::ZERO,
            Some(PowerSource::Ac),
            None,
            failure,
        ));
        builder.cleanup_failed(Some(Target::Cpu), Some(Phase::Cold));
        builder.cleanup_failed(Some(Target::Gpu), Some(Phase::Cold));
        assert_eq!(builder.report.failure, Some(failure));
        assert!(!builder.report.cleanup_complete);
        let value: serde_json::Value =
            serde_json::from_slice(&serialize_report(&builder.report).unwrap()).unwrap();
        assert_eq!(value["runs"].as_array().unwrap().len(), 1);
        assert_eq!(
            value["runs"][0]["failure"]["category"],
            "observation_failed"
        );
        assert!(value["runs"][0].get("backend_ms").is_none());
        assert!(
            value["runs"][0]
                .get("normalized_transcript_sha256")
                .is_none()
        );
        assert!(
            value["runs"][0]
                .get("sampled_max_private_usage_bytes")
                .is_none()
        );
        assert!(value["runs"][0].get("native_error").is_none());
    }

    #[test]
    fn incomplete_publication_returns_failure_and_never_replaces_an_existing_report() {
        let mut nonce = [0_u8; 16];
        getrandom::fill(&mut nonce).unwrap();
        let directory =
            std::env::temp_dir().join(format!("scribe-campaign-report-{}", hex(&nonce)));
        std::fs::create_dir(&directory).unwrap();
        let output = directory.join("report.json");
        let mut builder = builder();
        builder.fail(FailureDescriptor::global(
            FailureStage::WorkerPreflight,
            FailureCategory::WorkerUnavailable,
        ));
        assert!(publish_campaign(builder.report, &output).is_err());
        let original = std::fs::read(&output).unwrap();
        assert!(publish_campaign(complete_report(), &output).is_err());
        assert_eq!(std::fs::read(&output).unwrap(), original);
        let value: serde_json::Value = serde_json::from_slice(&original).unwrap();
        assert_eq!(value["incomplete"], true);
        assert_eq!(value["cleanup_complete"], true);
        assert!(value["runs"].as_array().unwrap().is_empty());
        std::fs::remove_file(&output).unwrap();
        std::fs::remove_dir(&directory).unwrap();
    }
}
