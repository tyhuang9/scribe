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

use super::power_scheme::{
    ActivePowerSchemeObservation, ActivePowerSchemeReader, WindowsActivePowerSchemeReader,
};
use super::telemetry::SamplingSession;
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
    PowerSchemeBaseline,
    PowerSchemePreflight,
    PowerSchemeBeforeLaunch,
    PowerSchemeBeforeDispatch,
    PowerSchemeAfterResult,
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
    PowerSchemeUnavailable,
    PowerSchemeChanged,
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
    active_power_scheme: ActivePowerSchemeObservation,
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
    power_scheme: RunPowerSchemeObservations,
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
    normalized_transcript_sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    failure: Option<FailureDescriptor>,
}

#[derive(Clone, Default, Serialize)]
struct RunPowerSchemeObservations {
    #[serde(skip_serializing_if = "Option::is_none")]
    before_launch: Option<ActivePowerSchemeObservation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    before_dispatch: Option<ActivePowerSchemeObservation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    after_result: Option<ActivePowerSchemeObservation>,
}

struct CampaignPowerContext<'a> {
    expected: CampaignPower,
    baseline_scheme: &'a str,
    reader: &'a dyn ActivePowerSchemeReader,
}

impl CampaignPowerContext<'_> {
    fn observe_stable_scheme(
        &self,
    ) -> (
        ActivePowerSchemeObservation,
        std::result::Result<(), FailureCategory>,
    ) {
        let observation = self.reader.observe_active_scheme();
        let result = require_stable_power_scheme(self.baseline_scheme, &observation);
        (observation, result)
    }

    fn power_now(&self) -> std::result::Result<PowerSource, FailureCategory> {
        require_power(self.expected, PowerSource::current())
    }
}

fn observe_then<T>(
    power: &CampaignPowerContext<'_>,
    observation_slot: &mut Option<ActivePowerSchemeObservation>,
    action: impl FnOnce() -> T,
) -> std::result::Result<T, FailureCategory> {
    let (observation, stable) = power.observe_stable_scheme();
    *observation_slot = Some(observation);
    stable.map(|()| action())
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
            power_scheme: RunPowerSchemeObservations::default(),
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
            normalized_transcript_sha256: None,
            failure: Some(failure),
        }
    }

    fn with_power_scheme(mut self, power_scheme: RunPowerSchemeObservations) -> Self {
        self.power_scheme = power_scheme;
        self
    }
}

struct ReportBuilder {
    report: CampaignReport,
    next_sequence: u8,
}

impl ReportBuilder {
    fn new(
        expected_power: CampaignPower,
        active_power_scheme: ActivePowerSchemeObservation,
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
                active_power_scheme,
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

fn require_stable_power_scheme(
    baseline: &str,
    observation: &ActivePowerSchemeObservation,
) -> std::result::Result<(), FailureCategory> {
    match observation.scheme_guid() {
        None => Err(FailureCategory::PowerSchemeUnavailable),
        Some(scheme_guid) if scheme_guid == baseline => Ok(()),
        Some(_) => Err(FailureCategory::PowerSchemeChanged),
    }
}

const fn power_scheme_preflight_failure(
    target: Target,
    category: FailureCategory,
) -> FailureDescriptor {
    FailureDescriptor {
        stage: FailureStage::PowerSchemePreflight,
        category,
        target: Some(target),
        phase: None,
        pair_index: None,
        order_in_pair: None,
    }
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
    let reader = WindowsActivePowerSchemeReader;
    run_with_power_scheme_reader(options, &reader)
}

fn run_with_power_scheme_reader(
    options: CommandOptions,
    reader: &dyn ActivePowerSchemeReader,
) -> Result<()> {
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
    // This baseline is deliberately read after the pinned inputs are valid,
    // but before any worker preflight can launch a process.
    let active_power_scheme = reader.observe_active_scheme();
    let mut builder = ReportBuilder::new(expected_power, active_power_scheme.clone(), inputs, None);
    let baseline_power_scheme = match active_power_scheme.scheme_guid() {
        Some(scheme_guid) => scheme_guid.to_owned(),
        None => {
            builder.fail(FailureDescriptor::global(
                FailureStage::PowerSchemeBaseline,
                FailureCategory::PowerSchemeUnavailable,
            ));
            return publish_campaign(builder.report, &options.output);
        }
    };
    let campaign_power = CampaignPowerContext {
        expected: expected_power,
        baseline_scheme: &baseline_power_scheme,
        reader,
    };
    let cpu_factory = CaptureObservationWorkerFactory::cpu();
    let gpu_factory = match CaptureObservationWorkerFactory::gpu_exact(
        &options.gpu_pack_id,
        &options.gpu_backend,
        &options.gpu_device,
    ) {
        Ok(factory) => factory,
        Err(_) => {
            builder.fail(FailureDescriptor::global(
                FailureStage::WorkerPreflight,
                FailureCategory::WorkerUnavailable,
            ));
            return publish_campaign(builder.report, &options.output);
        }
    };
    builder.report.gpu_identity = gpu_factory.gpu_identity().cloned();

    if let Err(category) = campaign_power.power_now() {
        builder.fail(FailureDescriptor::global(
            FailureStage::PowerPreflight,
            category,
        ));
        return publish_campaign(builder.report, &options.output);
    }

    if let Err(failure) =
        preflight_workers(&cpu_factory, &gpu_factory, &campaign_power, &mut builder)
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
            &campaign_power,
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
                &campaign_power,
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
    campaign_power: &CampaignPowerContext<'_>,
    builder: &mut ReportBuilder,
) -> std::result::Result<(), FailureDescriptor> {
    let mut cpu = None;
    let mut gpu = None;
    let mut cpu_lease = None;
    let mut gpu_lease = None;
    let operation = (|| {
        campaign_power.power_now().map_err(|category| {
            FailureDescriptor::global(FailureStage::PowerPreflight, category)
        })?;
        let mut cpu_launch_scheme = None;
        cpu = Some(Arc::new(
            observe_then(campaign_power, &mut cpu_launch_scheme, || {
                cpu_factory.spawn()
            })
            .map_err(|category| power_scheme_preflight_failure(Target::Cpu, category))?,
        ));
        let cpu_worker = cpu.as_ref().expect("CPU preflight worker was launched");
        cpu_lease = Some(
            cpu_worker
                .prepare_observation()
                .map_err(|_| FailureDescriptor {
                    stage: FailureStage::Handshake,
                    category: FailureCategory::WorkerUnavailable,
                    target: Some(Target::Cpu),
                    phase: None,
                    pair_index: None,
                    order_in_pair: None,
                })?,
        );
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
        campaign_power.power_now().map_err(|category| {
            FailureDescriptor::global(FailureStage::PowerPreflight, category)
        })?;
        let mut gpu_launch_scheme = None;
        gpu = Some(Arc::new(
            observe_then(campaign_power, &mut gpu_launch_scheme, || {
                gpu_factory.spawn()
            })
            .map_err(|category| power_scheme_preflight_failure(Target::Gpu, category))?,
        ));
        let gpu_worker = gpu.as_ref().expect("GPU preflight worker was launched");
        gpu_lease = Some(
            gpu_worker
                .prepare_observation()
                .map_err(|_| FailureDescriptor {
                    stage: FailureStage::Handshake,
                    category: FailureCategory::WorkerUnavailable,
                    target: Some(Target::Gpu),
                    phase: None,
                    pair_index: None,
                    order_in_pair: None,
                })?,
        );
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
        cpu_worker
            .negotiate_runtime_observation_retained(
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
        gpu_worker
            .negotiate_runtime_observation_retained(
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
        campaign_power.power_now().map_err(|category| {
            FailureDescriptor::global(FailureStage::PowerPreflight, category)
        })?;
        Ok(())
    })();
    if let Err(failure) = operation {
        builder.fail(failure);
    }
    let gpu_clean = gpu
        .as_ref()
        .is_none_or(|worker| cleanup_worker(worker, gpu_lease.as_ref()));
    let cpu_clean = cpu
        .as_ref()
        .is_none_or(|worker| cleanup_worker(worker, cpu_lease.as_ref()));
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
    campaign_power: &CampaignPowerContext<'_>,
    builder: &mut ReportBuilder,
) -> (CampaignRunRecord, bool) {
    let power_before = PowerSource::current();
    let started = Instant::now();
    let mut power_scheme = RunPowerSchemeObservations::default();
    let worker = match observe_then(campaign_power, &mut power_scheme.before_launch, || {
        require_power(campaign_power.expected, power_before).map(|_| Arc::new(factory.spawn()))
    }) {
        Err(category) => {
            let failure =
                FailureDescriptor::for_spec(spec, FailureStage::PowerSchemeBeforeLaunch, category);
            return (
                CampaignRunRecord::failed(
                    spec,
                    None,
                    started.elapsed(),
                    Some(power_before),
                    None,
                    failure,
                )
                .with_power_scheme(power_scheme),
                true,
            );
        }
        Ok(Err(category)) => {
            return (
                CampaignRunRecord::failed(
                    spec,
                    None,
                    started.elapsed(),
                    Some(power_before),
                    None,
                    FailureDescriptor::for_spec(spec, FailureStage::PowerEndpoint, category),
                )
                .with_power_scheme(power_scheme),
                true,
            );
        }
        Ok(Ok(worker)) => worker,
    };
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
            )
            .with_power_scheme(power_scheme);
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
            )
            .with_power_scheme(power_scheme);
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
        )
        .with_power_scheme(power_scheme);
        return (record, cleanup_worker(&worker, Some(&lease)));
    }
    let record = observe_attempt(
        spec,
        &worker,
        &lease,
        generation_ref,
        artifact,
        audio,
        campaign_power,
        started,
        Some(power_before),
        power_scheme,
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
    campaign_power: &CampaignPowerContext<'_>,
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
        let mut power_scheme = RunPowerSchemeObservations::default();
        let worker = match observe_then(campaign_power, &mut power_scheme.before_launch, || {
            require_power(campaign_power.expected, power_before).map(|_| Arc::new(factory.spawn()))
        }) {
            Err(category) => {
                let failure = FailureDescriptor::for_spec(
                    spec,
                    FailureStage::PowerSchemeBeforeLaunch,
                    category,
                );
                builder.report.runs.push(
                    CampaignRunRecord::failed(
                        spec,
                        None,
                        started.elapsed(),
                        Some(power_before),
                        None,
                        failure,
                    )
                    .with_power_scheme(power_scheme),
                );
                builder.fail(failure);
                break;
            }
            Ok(Err(category)) => {
                let failure =
                    FailureDescriptor::for_spec(spec, FailureStage::PowerEndpoint, category);
                builder.report.runs.push(
                    CampaignRunRecord::failed(
                        spec,
                        None,
                        started.elapsed(),
                        Some(power_before),
                        None,
                        failure,
                    )
                    .with_power_scheme(power_scheme),
                );
                builder.fail(failure);
                break;
            }
            Ok(Ok(worker)) => worker,
        };
        let lease = match worker.prepare_observation() {
            Ok(lease) => lease,
            Err(_) => {
                let failure = FailureDescriptor::for_spec(
                    spec,
                    FailureStage::Handshake,
                    FailureCategory::WorkerUnavailable,
                );
                builder.report.runs.push(
                    CampaignRunRecord::failed(
                        spec,
                        None,
                        started.elapsed(),
                        Some(power_before),
                        None,
                        failure,
                    )
                    .with_power_scheme(power_scheme),
                );
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
                builder.report.runs.push(
                    CampaignRunRecord::failed(
                        spec,
                        None,
                        started.elapsed(),
                        Some(power_before),
                        None,
                        failure,
                    )
                    .with_power_scheme(power_scheme),
                );
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
            builder.report.runs.push(
                CampaignRunRecord::failed(
                    spec,
                    Some(generation_ref),
                    started.elapsed(),
                    Some(power_before),
                    None,
                    failure,
                )
                .with_power_scheme(power_scheme),
            );
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
            builder.report.runs.push(
                CampaignRunRecord::failed(
                    spec,
                    Some(generation_ref),
                    started.elapsed(),
                    Some(power_before),
                    None,
                    failure,
                )
                .with_power_scheme(power_scheme),
            );
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
            campaign_power,
            started,
            Some(power_before),
            power_scheme,
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
                campaign_power,
                started,
                None,
                RunPowerSchemeObservations::default(),
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

fn settle_then_observe_after_result(
    result_at: Instant,
    correlated_success: bool,
    settled: impl FnOnce(Instant, bool) -> Result<()>,
    campaign_power: &CampaignPowerContext<'_>,
    power_scheme: &mut RunPowerSchemeObservations,
) -> (Result<()>, Option<std::result::Result<(), FailureCategory>>) {
    let lifecycle = settled(result_at, correlated_success);
    let post_result_scheme = correlated_success
        .then(|| observe_then(campaign_power, &mut power_scheme.after_result, || ()));
    (lifecycle, post_result_scheme)
}

fn completion_failure(
    lifecycle: &Result<()>,
    post_result_scheme: Option<&std::result::Result<(), FailureCategory>>,
) -> Option<(FailureStage, FailureCategory)> {
    if let Some(Err(category)) = post_result_scheme {
        Some((FailureStage::PowerSchemeAfterResult, *category))
    } else if lifecycle.is_err() {
        Some((
            FailureStage::WarmModelTtl,
            FailureCategory::InvariantViolation,
        ))
    } else {
        None
    }
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
    campaign_power: &CampaignPowerContext<'_>,
    started: Instant,
    initial_power: Option<PowerSource>,
    mut power_scheme: RunPowerSchemeObservations,
    settled: impl FnOnce(Instant, bool) -> Result<()>,
) -> CampaignRunRecord {
    let mut power_before = initial_power;
    let mut sampler = None;
    let execution = (|| {
        lease.require_current().map_err(|_| {
            (
                FailureStage::ObservationValidation,
                FailureCategory::IdentityMismatch,
            )
        })?;
        let before = PowerSource::current();
        power_before.get_or_insert(before);
        require_power(campaign_power.expected, before)
            .map_err(|category| (FailureStage::PowerEndpoint, category))?;
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
        // Sampling setup may touch native telemetry. Observe immediately
        // before dispatch so a scheme change during that setup is not hidden;
        // if setup fails, this boundary remains unvisited.
        observe_then(campaign_power, &mut power_scheme.before_dispatch, || {
            worker
                .transcribe_observed_retained(lease, artifact, spec.target.preference(), audio)
                .map_err(|_| (FailureStage::Inference, FailureCategory::ObservationFailed))
        })
        .map_err(|category| (FailureStage::PowerSchemeBeforeDispatch, category))?
    })();
    // The correlated response timestamp precedes sampler joining, endpoint
    // checks, validation, transcript hashing and report work. Notify the TTL
    // owner now, so opposite-target work cannot extend this idle lifetime.
    let result_at = execution
        .as_ref()
        .map_or_else(|_| Instant::now(), |result| result.result_at);
    let elapsed = result_at.saturating_duration_since(started);
    // This query intentionally follows both the exact correlated result time
    // and the TTL settlement. It is outside the measured request interval.
    let (lifecycle, post_result_scheme) = settle_then_observe_after_result(
        result_at,
        execution.is_ok(),
        settled,
        campaign_power,
        &mut power_scheme,
    );
    let power_after = sampler.as_ref().map(|_| PowerSource::current());
    let telemetry = sampler.map(SamplingSession::finish);
    let failed = |stage, category| {
        CampaignRunRecord::failed(
            spec,
            Some(generation_ref.clone()),
            elapsed,
            power_before,
            power_after,
            FailureDescriptor::for_spec(spec, stage, category),
        )
        .with_power_scheme(power_scheme.clone())
    };
    let observed = match execution {
        Ok(observed) => observed,
        Err((stage, category)) => return failed(stage, category),
    };
    if let Some((stage, category)) = completion_failure(&lifecycle, post_result_scheme.as_ref()) {
        return failed(stage, category);
    }
    let before = power_before.expect("successful request captured its initial power");
    let after = power_after.expect("successful request started its sampler");
    if let Err(category) = require_power(campaign_power.expected, after) {
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
        power_scheme,
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

fn validate_run_power_scheme(record: &CampaignRunRecord, baseline: &str) -> Result<()> {
    let observations = [
        (
            FailureStage::PowerSchemeBeforeLaunch,
            record.power_scheme.before_launch.as_ref(),
        ),
        (
            FailureStage::PowerSchemeBeforeDispatch,
            record.power_scheme.before_dispatch.as_ref(),
        ),
        (
            FailureStage::PowerSchemeAfterResult,
            record.power_scheme.after_result.as_ref(),
        ),
    ];
    if observations
        .iter()
        .filter_map(|(_, observation)| *observation)
        .any(|observation| !observation.is_valid())
        || (record.phase == Phase::Warm && record.power_scheme.before_launch.is_some())
        || (record.phase != Phase::Warm
            && (record.power_scheme.before_dispatch.is_some()
                || record.power_scheme.after_result.is_some())
            && record.power_scheme.before_launch.is_none())
        || (record.power_scheme.after_result.is_some()
            && record.power_scheme.before_dispatch.is_none())
    {
        bail!("campaign request contains inconsistent power-scheme observations")
    }

    for &(stage, observation) in &observations {
        let Some(observation) = observation else {
            continue;
        };
        let category = match observation.scheme_guid() {
            None => Some(FailureCategory::PowerSchemeUnavailable),
            Some(scheme_guid) if scheme_guid != baseline => {
                Some(FailureCategory::PowerSchemeChanged)
            }
            Some(_) => None,
        };
        if let Some(category) = category
            && (record.status != RecordStatus::Failed
                || record
                    .failure
                    .map(|failure| (failure.stage, failure.category))
                    != Some((stage, category)))
        {
            bail!("campaign request power-scheme failure does not match its boundary")
        }
    }

    if record.status == RecordStatus::Failed
        && let Some(failure) = record.failure
        && matches!(
            failure.stage,
            FailureStage::PowerSchemeBeforeLaunch
                | FailureStage::PowerSchemeBeforeDispatch
                | FailureStage::PowerSchemeAfterResult
        )
    {
        let observation = observations
            .iter()
            .find(|(stage, _)| *stage == failure.stage)
            .and_then(|(_, observation)| *observation)
            .ok_or_else(|| anyhow!("campaign request omitted its claimed power-scheme failure"))?;
        if require_stable_power_scheme(baseline, observation).err() != Some(failure.category) {
            bail!("campaign request claimed a power-scheme failure not present at its boundary")
        }
    }

    if record.status == RecordStatus::Succeeded
        && (record.power_scheme.before_dispatch.is_none()
            || record.power_scheme.after_result.is_none()
            || (record.phase != Phase::Warm && record.power_scheme.before_launch.is_none()))
    {
        bail!("campaign successful request omitted power-scheme observations")
    }
    Ok(())
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
    if !report.active_power_scheme.is_valid() {
        bail!("campaign report contains an invalid active power-scheme observation")
    }
    let baseline_power_scheme = match report.active_power_scheme.scheme_guid() {
        Some(scheme_guid) => scheme_guid,
        None => {
            if !report.incomplete
                || !report.runs.is_empty()
                || !report.captures.is_empty()
                || report.gpu_identity.is_some()
                || report
                    .failure
                    .map(|failure| (failure.stage, failure.category))
                    != Some((
                        FailureStage::PowerSchemeBaseline,
                        FailureCategory::PowerSchemeUnavailable,
                    ))
            {
                bail!("campaign unavailable power-scheme baseline has inconsistent state")
            }
            return Ok(());
        }
    };
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
        validate_run_power_scheme(record, baseline_power_scheme)?;
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
        if record.status == RecordStatus::Failed {
            if record.failure.is_none()
                || !report.incomplete
                || index + 1 != report.runs.len()
                || record.backend_ms.is_some()
                || record.model_load_ms.is_some()
                || record.warm_reused.is_some()
                || record.sampled_max_private_usage_bytes.is_some()
                || record.telemetry_sample_count.is_some()
                || record.video_memory.is_some()
                || record.raw_provider_memory.is_some()
                || record.memory_availability.is_some()
                || record.normalized_transcript_sha256.is_some()
            {
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
    use std::cell::{Cell, RefCell};
    use std::collections::VecDeque;
    use std::rc::Rc;

    use super::*;
    use crate::onnx_worker::{
        ProviderMemoryNotApplicableReason, ProviderMemoryObservation, WorkerMemoryAvailability,
    };

    fn builder() -> ReportBuilder {
        ReportBuilder::new(
            CampaignPower::Ac,
            observed_power_scheme(),
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

    fn observed_power_scheme() -> ActivePowerSchemeObservation {
        ActivePowerSchemeObservation::Observed {
            source: super::super::power_scheme::SOURCE,
            scheme_guid: "381b4222-f694-41f0-9685-ff5bb260df2e".to_owned(),
        }
    }

    fn changed_power_scheme() -> ActivePowerSchemeObservation {
        ActivePowerSchemeObservation::Observed {
            source: super::super::power_scheme::SOURCE,
            scheme_guid: "8c5e7fda-e8bf-4a96-9a85-a6e23a8c635c".to_owned(),
        }
    }

    fn unavailable_power_scheme() -> ActivePowerSchemeObservation {
        ActivePowerSchemeObservation::Unavailable {
            reason: "query_failed",
        }
    }

    struct ScriptedPowerSchemeReader {
        observations: RefCell<VecDeque<ActivePowerSchemeObservation>>,
        calls: Cell<usize>,
    }

    impl ScriptedPowerSchemeReader {
        fn new(observations: impl IntoIterator<Item = ActivePowerSchemeObservation>) -> Self {
            Self {
                observations: RefCell::new(observations.into_iter().collect()),
                calls: Cell::new(0),
            }
        }
    }

    impl ActivePowerSchemeReader for ScriptedPowerSchemeReader {
        fn observe_active_scheme(&self) -> ActivePowerSchemeObservation {
            self.calls.set(self.calls.get() + 1);
            self.observations
                .borrow_mut()
                .pop_front()
                .expect("campaign requested no more power-scheme observations than scripted")
        }
    }

    struct TracingPowerSchemeReader {
        observation: ActivePowerSchemeObservation,
        trace: Rc<RefCell<Vec<&'static str>>>,
    }

    impl ActivePowerSchemeReader for TracingPowerSchemeReader {
        fn observe_active_scheme(&self) -> ActivePowerSchemeObservation {
            self.trace.borrow_mut().push("after_result_query");
            self.observation.clone()
        }
    }

    fn successful_run_power_scheme(phase: Phase) -> RunPowerSchemeObservations {
        RunPowerSchemeObservations {
            before_launch: (phase != Phase::Warm).then(observed_power_scheme),
            before_dispatch: Some(observed_power_scheme()),
            after_result: Some(observed_power_scheme()),
        }
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
                power_scheme: successful_run_power_scheme(spec.phase),
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
                normalized_transcript_sha256: Some("d".repeat(64)),
                failure: None,
            });
        }
        report
    }

    fn failed_scheme_report(
        index: usize,
        stage: FailureStage,
        observation: ActivePowerSchemeObservation,
    ) -> CampaignReport {
        let mut report = complete_report();
        let spec = campaign_specs()[index];
        let generation_ref = report.runs[index].generation_ref.clone();
        report.runs.truncate(index);
        let mut power_scheme = RunPowerSchemeObservations::default();
        let category = require_stable_power_scheme(
            report
                .active_power_scheme
                .scheme_guid()
                .expect("complete report has an observed baseline"),
            &observation,
        )
        .expect_err("failed fixture observation differs from the baseline");
        match stage {
            FailureStage::PowerSchemeBeforeLaunch => {
                assert_ne!(spec.phase, Phase::Warm);
                power_scheme.before_launch = Some(observation);
            }
            FailureStage::PowerSchemeBeforeDispatch => {
                power_scheme.before_launch =
                    (spec.phase != Phase::Warm).then(observed_power_scheme);
                power_scheme.before_dispatch = Some(observation);
            }
            FailureStage::PowerSchemeAfterResult => {
                power_scheme.before_launch =
                    (spec.phase != Phase::Warm).then(observed_power_scheme);
                power_scheme.before_dispatch = Some(observed_power_scheme());
                power_scheme.after_result = Some(observation);
            }
            _ => panic!("test fixture requires a request power-scheme stage"),
        }
        let generation_ref = match stage {
            FailureStage::PowerSchemeBeforeLaunch => None,
            FailureStage::PowerSchemeBeforeDispatch | FailureStage::PowerSchemeAfterResult => {
                generation_ref
            }
            _ => unreachable!(),
        };
        let failure = FailureDescriptor::for_spec(spec, stage, category);
        report.runs.push(
            CampaignRunRecord::failed(
                spec,
                generation_ref,
                Duration::ZERO,
                Some(PowerSource::Ac),
                None,
                failure,
            )
            .with_power_scheme(power_scheme),
        );
        report.incomplete = true;
        report.failure = Some(failure);
        report
    }

    fn unavailable_baseline_report() -> CampaignReport {
        let mut report = builder().report;
        report.active_power_scheme = unavailable_power_scheme();
        report.gpu_identity = None;
        report.incomplete = true;
        report.failure = Some(FailureDescriptor::global(
            FailureStage::PowerSchemeBaseline,
            FailureCategory::PowerSchemeUnavailable,
        ));
        report
    }

    fn write_tiny_verified_inputs() -> (CommandOptions, std::path::PathBuf) {
        let mut nonce = [0_u8; 16];
        getrandom::fill(&mut nonce).expect("test nonce is available");
        let directory =
            std::env::temp_dir().join(format!("scribe-campaign-inputs-{}", hex(&nonce)));
        std::fs::create_dir(&directory).expect("test input directory is created");
        let model = directory.join("model.gguf");
        let wav = directory.join("audio.wav");
        let model_bytes = b"not-reached-model";
        let wav_bytes = b"not-reached-wav";
        std::fs::write(&model, model_bytes).expect("test model is written");
        std::fs::write(&wav, wav_bytes).expect("test WAV is written");
        let options = CommandOptions {
            model,
            model_sha256: format!("{:x}", Sha256::digest(model_bytes)),
            wav,
            wav_sha256: format!("{:x}", Sha256::digest(wav_bytes)),
            gpu_pack_id: "not-reached-pack".to_owned(),
            gpu_backend: "not-reached-backend".to_owned(),
            gpu_device: "not-reached-device".to_owned(),
            output: directory.join("report.json"),
            campaign_power: Some(CampaignPower::Ac),
        };
        (options, directory)
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
    fn production_boundary_recorder_uses_the_injected_reader_in_boundary_order() {
        let reader = ScriptedPowerSchemeReader::new([
            observed_power_scheme(),
            observed_power_scheme(),
            changed_power_scheme(),
        ]);
        let baseline = observed_power_scheme();
        let baseline_guid = baseline.scheme_guid().unwrap();
        let campaign_power = CampaignPowerContext {
            expected: CampaignPower::Ac,
            baseline_scheme: baseline_guid,
            reader: &reader,
        };
        let mut boundaries = RunPowerSchemeObservations::default();

        assert_eq!(
            observe_then(&campaign_power, &mut boundaries.before_launch, || ()),
            Ok(())
        );
        assert_eq!(
            observe_then(&campaign_power, &mut boundaries.before_dispatch, || ()),
            Ok(())
        );
        assert_eq!(
            observe_then(&campaign_power, &mut boundaries.after_result, || ()),
            Err(FailureCategory::PowerSchemeChanged)
        );
        assert_eq!(reader.calls.get(), 3);
        assert!(boundaries.before_launch.is_some());
        assert!(boundaries.before_dispatch.is_some());
        assert_eq!(boundaries.after_result, Some(changed_power_scheme()));
    }

    #[test]
    fn settlement_precedes_postresult_query_and_postresult_failure_wins() {
        let trace = Rc::new(RefCell::new(Vec::new()));
        let reader = TracingPowerSchemeReader {
            observation: changed_power_scheme(),
            trace: Rc::clone(&trace),
        };
        let baseline = observed_power_scheme();
        let campaign_power = CampaignPowerContext {
            expected: CampaignPower::Ac,
            baseline_scheme: baseline.scheme_guid().unwrap(),
            reader: &reader,
        };
        let mut observations = RunPowerSchemeObservations::default();
        let (lifecycle, post_result_scheme) = settle_then_observe_after_result(
            Instant::now(),
            true,
            |_, succeeded| {
                assert!(succeeded);
                trace.borrow_mut().push("settled");
                Err(anyhow!("simultaneous lifecycle failure"))
            },
            &campaign_power,
            &mut observations,
        );

        assert_eq!(
            trace.borrow().as_slice(),
            &["settled", "after_result_query"]
        );
        assert_eq!(observations.after_result, Some(changed_power_scheme()));
        assert_eq!(
            completion_failure(&lifecycle, post_result_scheme.as_ref()),
            Some((
                FailureStage::PowerSchemeAfterResult,
                FailureCategory::PowerSchemeChanged,
            ))
        );

        trace.borrow_mut().clear();
        let mut unsuccessful_observations = RunPowerSchemeObservations::default();
        let (lifecycle, post_result_scheme) = settle_then_observe_after_result(
            Instant::now(),
            false,
            |_, succeeded| {
                assert!(!succeeded);
                trace.borrow_mut().push("settled");
                Ok(())
            },
            &campaign_power,
            &mut unsuccessful_observations,
        );
        assert!(lifecycle.is_ok());
        assert!(post_result_scheme.is_none());
        assert!(unsuccessful_observations.after_result.is_none());
        assert_eq!(trace.borrow().as_slice(), &["settled"]);
    }

    #[test]
    fn unavailable_baseline_stops_the_real_campaign_before_worker_preflight() {
        let (options, directory) = write_tiny_verified_inputs();
        let output = options.output.clone();
        let reader = ScriptedPowerSchemeReader::new([unavailable_power_scheme()]);

        assert!(run_with_power_scheme_reader(options, &reader).is_err());
        assert_eq!(reader.calls.get(), 1);
        let report: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&output).expect("baseline report is published"))
                .expect("baseline report is JSON");
        assert!(
            report["captures"]
                .as_array()
                .expect("captures is an array")
                .is_empty()
        );
        assert!(report.get("gpu_identity").is_none());
        assert_eq!(
            report["failure"]["stage"],
            serde_json::Value::String("power_scheme_baseline".to_owned())
        );
        assert_eq!(
            report["failure"]["category"],
            serde_json::Value::String("power_scheme_unavailable".to_owned())
        );
        std::fs::remove_file(output).expect("test report is removed");
        std::fs::remove_file(directory.join("model.gguf")).expect("test model is removed");
        std::fs::remove_file(directory.join("audio.wav")).expect("test WAV is removed");
        std::fs::remove_dir(directory).expect("test input directory is removed");
    }

    #[test]
    fn run_cold_stops_before_cpu_worker_launch_when_the_scheme_is_unavailable() {
        let reader = ScriptedPowerSchemeReader::new([unavailable_power_scheme()]);
        let baseline = observed_power_scheme();
        let campaign_power = CampaignPowerContext {
            expected: CampaignPower::Ac,
            baseline_scheme: baseline
                .scheme_guid()
                .expect("fixture baseline is observed"),
            reader: &reader,
        };
        let artifact = RuntimeArtifact::Gguf(RuntimeModel {
            id: ModelId::new("campaign-test-artifact"),
            path: std::env::temp_dir().join("not-launched.gguf"),
            format: ArtifactFormat::Gguf,
            expected_size_bytes: 1,
            expected_sha256: "a".repeat(64),
        });
        let audio = PreparedAudio {
            samples: vec![0.0],
            sample_rate: 16_000,
            source_sample_rate: 16_000,
            source_channels: 1,
            source_frames: 1,
        };
        let mut report_builder = builder();
        let (record, cleanup_complete) = run_cold(
            campaign_specs()[0],
            &CaptureObservationWorkerFactory::cpu(),
            artifact,
            &audio,
            &campaign_power,
            &mut report_builder,
        );

        assert!(cleanup_complete);
        assert_eq!(reader.calls.get(), 1);
        assert_eq!(record.status, RecordStatus::Failed);
        assert_eq!(record.generation_ref, None);
        assert_eq!(
            record.failure,
            Some(FailureDescriptor::for_spec(
                campaign_specs()[0],
                FailureStage::PowerSchemeBeforeLaunch,
                FailureCategory::PowerSchemeUnavailable,
            ))
        );
        assert_eq!(
            record.power_scheme.before_launch,
            Some(unavailable_power_scheme())
        );
        assert!(record.power_scheme.before_dispatch.is_none());
        assert!(record.power_scheme.after_result.is_none());
    }

    #[test]
    fn preflight_scheme_failures_are_target_specific_and_prevent_further_launches() {
        for target in [Target::Cpu, Target::Gpu] {
            assert_eq!(
                power_scheme_preflight_failure(target, FailureCategory::PowerSchemeUnavailable),
                FailureDescriptor {
                    stage: FailureStage::PowerSchemePreflight,
                    category: FailureCategory::PowerSchemeUnavailable,
                    target: Some(target),
                    phase: None,
                    pair_index: None,
                    order_in_pair: None,
                }
            );
        }
        let reader =
            ScriptedPowerSchemeReader::new([observed_power_scheme(), unavailable_power_scheme()]);
        let baseline = observed_power_scheme();
        let campaign_power = CampaignPowerContext {
            expected: CampaignPower::Ac,
            baseline_scheme: baseline.scheme_guid().unwrap(),
            reader: &reader,
        };
        let mut cpu_launch_scheme = None;
        let mut gpu_launch_scheme = None;
        let cpu_launches = Cell::new(0);
        let gpu_launches = Cell::new(0);
        assert_eq!(
            observe_then(&campaign_power, &mut cpu_launch_scheme, || {
                cpu_launches.set(cpu_launches.get() + 1);
                Target::Cpu
            }),
            Ok(Target::Cpu)
        );
        assert_eq!(
            observe_then(&campaign_power, &mut gpu_launch_scheme, || {
                gpu_launches.set(gpu_launches.get() + 1);
                Target::Gpu
            }),
            Err(FailureCategory::PowerSchemeUnavailable)
        );
        assert_eq!(cpu_launches.get(), 1);
        assert_eq!(gpu_launches.get(), 0);
        assert_eq!(cpu_launch_scheme, Some(observed_power_scheme()));
        assert_eq!(gpu_launch_scheme, Some(unavailable_power_scheme()));
        assert_eq!(reader.calls.get(), 2);
    }

    #[test]
    fn validator_accepts_terminal_scheme_failure_prefixes() {
        for (index, stages) in [
            (
                0,
                [
                    FailureStage::PowerSchemeBeforeLaunch,
                    FailureStage::PowerSchemeBeforeDispatch,
                    FailureStage::PowerSchemeAfterResult,
                ],
            ),
            (
                10,
                [
                    FailureStage::PowerSchemeBeforeLaunch,
                    FailureStage::PowerSchemeBeforeDispatch,
                    FailureStage::PowerSchemeAfterResult,
                ],
            ),
            (
                12,
                [
                    FailureStage::PowerSchemeBeforeDispatch,
                    FailureStage::PowerSchemeAfterResult,
                    FailureStage::PowerSchemeAfterResult,
                ],
            ),
        ] {
            for stage in stages {
                let report = failed_scheme_report(index, stage, unavailable_power_scheme());
                assert_eq!(report.runs.len(), index + 1);
                assert!(validate_report(&report).is_ok());
            }
        }

        for (index, stage) in [
            (0, FailureStage::PowerSchemeAfterResult),
            (10, FailureStage::PowerSchemeBeforeDispatch),
            (12, FailureStage::PowerSchemeAfterResult),
        ] {
            let report = failed_scheme_report(index, stage, changed_power_scheme());
            assert_eq!(report.runs.len(), index + 1);
            assert!(validate_report(&report).is_ok());
        }
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
            value["active_power_scheme"]["scheme_guid"],
            "381b4222-f694-41f0-9685-ff5bb260df2e"
        );
        assert!(
            value["runs"][0]["power_scheme"]
                .get("before_launch")
                .is_some()
        );
        assert!(
            value["runs"][12]["power_scheme"]
                .get("before_launch")
                .is_none()
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
    fn validator_rejects_power_scheme_omissions_and_inconsistent_observations() {
        let mut missing_after_result = complete_report();
        missing_after_result.runs[0].power_scheme.after_result = None;
        assert!(validate_report(&missing_after_result).is_err());

        let mut warm_launch = complete_report();
        warm_launch.runs[12].power_scheme.before_launch = Some(observed_power_scheme());
        assert!(validate_report(&warm_launch).is_err());

        let mut changed_success = complete_report();
        changed_success.runs[0].power_scheme.after_result = Some(changed_power_scheme());
        assert!(validate_report(&changed_success).is_err());

        let unavailable_baseline = unavailable_baseline_report();
        assert!(validate_report(&unavailable_baseline).is_ok());

        let mut unavailable_baseline_capture = unavailable_baseline_report();
        capture(
            &mut unavailable_baseline_capture,
            Target::Cpu,
            CapturePurpose::Preflight,
        );
        assert!(validate_report(&unavailable_baseline_capture).is_err());

        let mut unavailable_baseline_gpu = unavailable_baseline_report();
        unavailable_baseline_gpu.gpu_identity = builder().report.gpu_identity;
        assert!(validate_report(&unavailable_baseline_gpu).is_err());

        let mut fabricated_failed_values = failed_scheme_report(
            0,
            FailureStage::PowerSchemeBeforeLaunch,
            unavailable_power_scheme(),
        );
        fabricated_failed_values.runs[0].backend_ms = Some(0);
        assert!(validate_report(&fabricated_failed_values).is_err());

        let mut omitted_claimed_failure = failed_scheme_report(
            0,
            FailureStage::PowerSchemeBeforeLaunch,
            unavailable_power_scheme(),
        );
        omitted_claimed_failure.runs[0].power_scheme.before_launch = None;
        assert!(validate_report(&omitted_claimed_failure).is_err());

        let mut stable_claimed_failure = failed_scheme_report(
            0,
            FailureStage::PowerSchemeBeforeLaunch,
            unavailable_power_scheme(),
        );
        stable_claimed_failure.runs[0].power_scheme.before_launch = Some(observed_power_scheme());
        assert!(validate_report(&stable_claimed_failure).is_err());

        let mut wrong_claimed_category = failed_scheme_report(
            0,
            FailureStage::PowerSchemeBeforeLaunch,
            changed_power_scheme(),
        );
        wrong_claimed_category.runs[0]
            .failure
            .as_mut()
            .expect("failed record has a failure")
            .category = FailureCategory::PowerSchemeUnavailable;
        wrong_claimed_category.failure = wrong_claimed_category.runs[0].failure;
        assert!(validate_report(&wrong_claimed_category).is_err());
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
