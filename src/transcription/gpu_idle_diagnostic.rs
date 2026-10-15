//! Opt-in, supervised Windows validation of the real five-minute model idle
//! lifetime. This is deliberately a development diagnostic, never a release
//! qualification or Auto-admission path.

#[cfg(test)]
use std::collections::VecDeque;
use std::ffi::OsString;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::backend_policy::{BackendKind, BackendTarget};
use crate::model_catalog::ArtifactFormat;
use crate::onnx_worker::{ActiveGpuModelObservation, WorkerModelResidency};
use crate::prepared_audio::{PREPARED_SAMPLE_RATE, PreparedAudio};
use crate::runtime_artifact::{RuntimeArtifact, RuntimeModel};
use crate::runtime_contract::{RuntimeExecution, WARM_MODEL_TTL};
use crate::windows_gpu_capture::{
    MAX_MODEL_BYTES, canonical_sha256, require_retained_digest, verify_input,
};
use crate::worker_contracts::{AccelerationPreference, ComputeDevice, TranscriptionOptions};

use super::{ModelId, RuntimeWorker};

const COMMAND_FLAG: &str = "--scribe-windows-gpu-idle-diagnostic";
const ACK_LOCAL_SECURITY_RECORDS: &str = "--ack-local-security-records";
const ACK_LOCAL_EXECUTION_COPY: &str = "--ack-local-execution-copies";
const MAX_TEXT_BYTES: usize = 256;
const IDLE_SETTLEMENT: Duration = Duration::from_secs(15);
const IDLE_POLL_INTERVAL: Duration = Duration::from_millis(250);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_REPORT_BYTES: usize = 16 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
struct CommandOptions {
    model_id: String,
    model: PathBuf,
    model_sha256: String,
    model_size: u64,
    expected_backend: String,
    expected_pack_id: String,
    expected_stable_device: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Failure {
    Arguments,
    Input,
    Audio,
    InitialColdRequest,
    ImmediateWarmRequest,
    PostIdleColdRequest,
    InitialObservation,
    WarmObservation,
    PostIdleObservation,
    CpuOrFallback,
    WarmExpectation,
    RouteIdentity,
    TranscriptDrift,
    EarlyUnload,
    LostGeneration,
    IdleTimeout,
    Cleanup,
    OperationAndCleanup,
    Report,
}

impl Failure {
    fn label(self) -> &'static str {
        match self {
            Self::Arguments => "arguments",
            Self::Input => "input",
            Self::Audio => "audio",
            Self::InitialColdRequest => "initial_cold_request",
            Self::ImmediateWarmRequest => "immediate_warm_request",
            Self::PostIdleColdRequest => "post_idle_cold_request",
            Self::InitialObservation => "initial_observation",
            Self::WarmObservation => "warm_observation",
            Self::PostIdleObservation => "post_idle_observation",
            Self::CpuOrFallback => "cpu_or_fallback",
            Self::WarmExpectation => "warm_expectation",
            Self::RouteIdentity => "route_identity",
            Self::TranscriptDrift => "transcript_drift",
            Self::EarlyUnload => "early_unload",
            Self::LostGeneration => "lost_generation",
            Self::IdleTimeout => "idle_timeout",
            Self::Cleanup => "cleanup",
            Self::OperationAndCleanup => "operation_and_cleanup",
            Self::Report => "report",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ExpectedGpuTarget {
    backend: String,
    pack_id: String,
    stable_device: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct DiagnosticRoute {
    backend: String,
    provider: String,
    stable_device: String,
    driver: Option<String>,
    pack_id: String,
    pack_version: String,
    pack_sha256: String,
    pack_security_epoch: u64,
    runtime_abi: u16,
}

impl DiagnosticRoute {
    fn from_target(target: &BackendTarget) -> Result<Self, Failure> {
        let pack = target.pack.as_ref().ok_or(Failure::RouteIdentity)?;
        let backend = match target.backend {
            BackendKind::Cuda => "cuda",
            BackendKind::Vulkan => "vulkan",
            _ => return Err(Failure::CpuOrFallback),
        };
        Ok(Self {
            backend: backend.to_owned(),
            provider: target.provider_id.as_str().to_owned(),
            stable_device: target.device_id.as_str().to_owned(),
            driver: target.driver_version.clone(),
            pack_id: pack.pack_id.clone(),
            pack_version: pack.pack_version.clone(),
            pack_sha256: pack.pack_digest.clone(),
            pack_security_epoch: pack.security_epoch,
            runtime_abi: pack.runtime_abi,
        })
    }

    fn matches_expected(&self, expected: &ExpectedGpuTarget) -> bool {
        self.backend == expected.backend
            && self.pack_id == expected.pack_id
            && self.stable_device == expected.stable_device
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ObservedRoute {
    route: DiagnosticRoute,
    generation: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct DiagnosticExecution {
    warm_reused: bool,
    model_load_ms: u128,
    transcript_sha256: String,
    explicit_gpu_without_fallback: bool,
    route: DiagnosticRoute,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ModelResidency {
    Loaded,
    Unloaded,
}

trait DiagnosticExecutor {
    fn transcribe(&mut self) -> Result<DiagnosticExecution, Failure>;
    fn observe_loaded(&mut self) -> Result<ObservedRoute, Failure>;
    fn model_residency(&mut self) -> Result<ModelResidency, Failure>;
    fn shutdown(&mut self) -> Result<(), Failure>;
}

trait DiagnosticClock {
    fn elapsed(&self) -> Duration;
    fn sleep(&mut self, duration: Duration);
}

struct WallClock {
    started: Instant,
}

impl WallClock {
    fn new() -> Self {
        Self {
            started: Instant::now(),
        }
    }
}

impl DiagnosticClock for WallClock {
    fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }

    fn sleep(&mut self, duration: Duration) {
        std::thread::sleep(duration);
    }
}

struct ProcessExecutor {
    worker: RuntimeWorker,
    artifact: RuntimeArtifact,
    audio: Arc<PreparedAudio>,
    observation: Option<ActiveGpuModelObservation>,
}

impl ProcessExecutor {
    fn new(artifact: RuntimeArtifact, audio: Arc<PreparedAudio>) -> Self {
        Self {
            worker: RuntimeWorker::new_process(),
            artifact,
            audio,
            observation: None,
        }
    }

    fn execution(execution: RuntimeExecution) -> Result<DiagnosticExecution, Failure> {
        let acceleration = &execution.diagnostics.resolved_acceleration;
        let target = acceleration
            .selection
            .as_ref()
            .map(|selection| &selection.target)
            .ok_or(Failure::CpuOrFallback)?;
        let explicit_gpu_without_fallback = acceleration.requested == AccelerationPreference::Gpu
            && matches!(acceleration.resolved, ComputeDevice::Gpu { .. })
            && acceleration
                .selection
                .as_ref()
                .is_some_and(|selection| selection.fallback_history.is_empty());
        Ok(DiagnosticExecution {
            warm_reused: execution.diagnostics.warm_reused,
            model_load_ms: execution.diagnostics.model_load_duration_ms,
            transcript_sha256: format!(
                "{:x}",
                Sha256::digest(execution.transcript.text.as_bytes())
            ),
            explicit_gpu_without_fallback,
            route: DiagnosticRoute::from_target(target)?,
        })
    }
}

impl DiagnosticExecutor for ProcessExecutor {
    fn transcribe(&mut self) -> Result<DiagnosticExecution, Failure> {
        let execution = self
            .worker
            .transcribe(
                self.artifact.clone(),
                AccelerationPreference::Gpu,
                Arc::clone(&self.audio),
                TranscriptionOptions::default(),
                self.worker.cancellation_snapshot(),
            )
            .map_err(|_| Failure::InitialColdRequest)?;
        Self::execution(execution)
    }

    fn observe_loaded(&mut self) -> Result<ObservedRoute, Failure> {
        let observation = self
            .worker
            .current_active_gpu_model_observation(&self.artifact)
            .map_err(|_| Failure::InitialObservation)?;
        let route = DiagnosticRoute::from_target(&observation.target)?;
        let generation = observation.generation();
        self.observation = Some(observation);
        Ok(ObservedRoute { route, generation })
    }

    fn model_residency(&mut self) -> Result<ModelResidency, Failure> {
        let observation = self.observation.as_ref().ok_or(Failure::LostGeneration)?;
        match observation
            .model_residency()
            .map_err(|_| Failure::LostGeneration)?
        {
            WorkerModelResidency::Loaded => Ok(ModelResidency::Loaded),
            WorkerModelResidency::Unloaded => Ok(ModelResidency::Unloaded),
            WorkerModelResidency::NotObserved => Err(Failure::LostGeneration),
        }
    }

    fn shutdown(&mut self) -> Result<(), Failure> {
        self.worker
            .shutdown_and_join(SHUTDOWN_TIMEOUT)
            .then_some(())
            .ok_or(Failure::Cleanup)
    }
}

#[derive(Debug, Eq, PartialEq, Serialize)]
struct PhaseReport {
    warm_reused: bool,
    model_load_ms: u128,
}

#[derive(Debug, Eq, PartialEq, Serialize)]
struct IdleDiagnosticReport {
    schema_version: u8,
    kind: &'static str,
    unsigned: bool,
    unqualified: bool,
    auto_eligible: bool,
    release_approved: bool,
    model_sha256: String,
    gpu: DiagnosticRoute,
    initial_cold: PhaseReport,
    immediate_warm: PhaseReport,
    post_idle_cold: PhaseReport,
    transcript_sha256: String,
    warm_model_ttl_ms: u64,
    automatic_unload_observed_after_warm_request_ms: u64,
    automatic_unload_observed: bool,
}

fn validate_phase(execution: &DiagnosticExecution, must_be_warm: bool) -> Result<(), Failure> {
    if !execution.explicit_gpu_without_fallback {
        return Err(Failure::CpuOrFallback);
    }
    if execution.warm_reused != must_be_warm || (must_be_warm && execution.model_load_ms != 0) {
        return Err(Failure::WarmExpectation);
    }
    Ok(())
}

fn require_expected_route(
    observed: &ObservedRoute,
    expected: &ExpectedGpuTarget,
) -> Result<(), Failure> {
    observed
        .route
        .matches_expected(expected)
        .then_some(())
        .ok_or(Failure::RouteIdentity)
}

fn require_expected_execution_route(
    execution: &DiagnosticExecution,
    expected: &ExpectedGpuTarget,
) -> Result<(), Failure> {
    execution
        .route
        .matches_expected(expected)
        .then_some(())
        .ok_or(Failure::RouteIdentity)
}

fn duration_millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

fn wait_for_automatic_unload<E: DiagnosticExecutor, C: DiagnosticClock>(
    executor: &mut E,
    clock: &mut C,
    earliest_expiry: Duration,
    settlement_deadline: Duration,
) -> Result<Duration, Failure> {
    loop {
        // The time before the linearizable lease read is the conservative
        // proof boundary for early unload. Record a second timestamp after the
        // read for the receipt's acknowledged-Unload observation interval.
        let sampled_at = clock.elapsed();
        let residency = executor.model_residency()?;
        let observed_at = clock.elapsed();
        match residency {
            ModelResidency::Unloaded if sampled_at < earliest_expiry => {
                return Err(Failure::EarlyUnload);
            }
            // The settlement deadline is inclusive. A delayed thread resume
            // must not turn an automatic Unload that was observed after the
            // bounded window into a passing TTL receipt.
            ModelResidency::Unloaded if observed_at > settlement_deadline => {
                return Err(Failure::IdleTimeout);
            }
            ModelResidency::Unloaded => return Ok(observed_at),
            ModelResidency::Loaded if observed_at >= settlement_deadline => {
                return Err(Failure::IdleTimeout);
            }
            ModelResidency::Loaded => {
                let remaining = settlement_deadline.saturating_sub(observed_at);
                clock.sleep(IDLE_POLL_INTERVAL.min(remaining));
            }
        }
    }
}

fn run_sequence<E: DiagnosticExecutor, C: DiagnosticClock>(
    executor: &mut E,
    clock: &mut C,
    expected: &ExpectedGpuTarget,
    model_sha256: String,
) -> Result<IdleDiagnosticReport, Failure> {
    let initial_cold = executor
        .transcribe()
        .map_err(|_| Failure::InitialColdRequest)?;
    validate_phase(&initial_cold, false)?;
    require_expected_execution_route(&initial_cold, expected)?;
    let initial_route = executor
        .observe_loaded()
        .map_err(|_| Failure::InitialObservation)?;
    require_expected_route(&initial_route, expected)?;
    if initial_cold.route != initial_route.route {
        return Err(Failure::RouteIdentity);
    }

    // This is deliberately before the immediate request. The worker's timer
    // begins only after the response, so this is the earliest legal expiry and
    // catches any disappearance that cannot be attributed to normal delivery.
    let immediate_warm_started = clock.elapsed();
    let earliest_expiry = immediate_warm_started
        .checked_add(WARM_MODEL_TTL)
        .ok_or(Failure::IdleTimeout)?;
    let immediate_warm = executor
        .transcribe()
        .map_err(|_| Failure::ImmediateWarmRequest)?;
    let reply_expiry = clock
        .elapsed()
        .checked_add(WARM_MODEL_TTL)
        .ok_or(Failure::IdleTimeout)?;
    let settlement_deadline = reply_expiry
        .checked_add(IDLE_SETTLEMENT)
        .ok_or(Failure::IdleTimeout)?;
    validate_phase(&immediate_warm, true)?;
    require_expected_execution_route(&immediate_warm, expected)?;
    let warm_route = executor
        .observe_loaded()
        .map_err(|_| Failure::WarmObservation)?;
    require_expected_route(&warm_route, expected)?;
    if immediate_warm.route != initial_route.route
        || immediate_warm.route != warm_route.route
        || warm_route.route != initial_route.route
        || warm_route.generation != initial_route.generation
    {
        return Err(Failure::RouteIdentity);
    }
    if immediate_warm.transcript_sha256 != initial_cold.transcript_sha256 {
        return Err(Failure::TranscriptDrift);
    }

    // The only work during this interval is a read-only, lease-bound state
    // observation. No RuntimeCommand is sent, so the dispatch loop remains in
    // its ordinary `recv_timeout(WARM_MODEL_TTL)` wait.
    let automatic_unload_observed_at =
        wait_for_automatic_unload(executor, clock, earliest_expiry, settlement_deadline)?;

    let post_idle_cold = executor
        .transcribe()
        .map_err(|_| Failure::PostIdleColdRequest)?;
    validate_phase(&post_idle_cold, false)?;
    require_expected_execution_route(&post_idle_cold, expected)?;
    let post_idle_route = executor
        .observe_loaded()
        .map_err(|_| Failure::PostIdleObservation)?;
    require_expected_route(&post_idle_route, expected)?;
    if post_idle_cold.route != initial_route.route
        || post_idle_cold.route != post_idle_route.route
        || post_idle_route.route != initial_route.route
        || post_idle_route.generation != initial_route.generation
    {
        return Err(Failure::RouteIdentity);
    }
    if post_idle_cold.transcript_sha256 != initial_cold.transcript_sha256 {
        return Err(Failure::TranscriptDrift);
    }

    Ok(IdleDiagnosticReport {
        schema_version: 1,
        kind: "windows_gpu_idle_diagnostic",
        unsigned: true,
        unqualified: true,
        auto_eligible: false,
        release_approved: false,
        model_sha256,
        gpu: initial_cold.route,
        initial_cold: PhaseReport {
            warm_reused: initial_cold.warm_reused,
            model_load_ms: initial_cold.model_load_ms,
        },
        immediate_warm: PhaseReport {
            warm_reused: immediate_warm.warm_reused,
            model_load_ms: immediate_warm.model_load_ms,
        },
        post_idle_cold: PhaseReport {
            warm_reused: post_idle_cold.warm_reused,
            model_load_ms: post_idle_cold.model_load_ms,
        },
        transcript_sha256: initial_cold.transcript_sha256,
        warm_model_ttl_ms: duration_millis(WARM_MODEL_TTL),
        automatic_unload_observed_after_warm_request_ms: duration_millis(
            automatic_unload_observed_at.saturating_sub(immediate_warm_started),
        ),
        automatic_unload_observed: true,
    })
}

fn run_with_cleanup<E: DiagnosticExecutor, C: DiagnosticClock>(
    executor: &mut E,
    clock: &mut C,
    expected: &ExpectedGpuTarget,
    model_sha256: String,
) -> Result<IdleDiagnosticReport, Failure> {
    let operation = run_sequence(executor, clock, expected, model_sha256);
    let cleanup = executor.shutdown();
    match (operation, cleanup) {
        (Ok(report), Ok(())) => Ok(report),
        (Ok(_), Err(_)) => Err(Failure::Cleanup),
        (operation @ Err(_), Ok(())) => operation,
        (Err(_), Err(_)) => Err(Failure::OperationAndCleanup),
    }
}

pub(crate) fn maybe_run_windows_gpu_idle_diagnostic() -> Option<i32> {
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    if !args.iter().any(|arg| arg == COMMAND_FLAG) {
        return None;
    }
    match run_local_command(&args) {
        Ok(report) => match serde_json::to_vec(&report) {
            Ok(bytes)
                if bytes.len() <= MAX_REPORT_BYTES
                    && std::io::stdout().lock().write_all(&bytes).is_ok() =>
            {
                Some(0)
            }
            _ => {
                eprintln!(
                    "Windows GPU idle diagnostic failed: {}",
                    Failure::Report.label()
                );
                Some(1)
            }
        },
        Err(failure) => {
            eprintln!("Windows GPU idle diagnostic failed: {}", failure.label());
            Some(1)
        }
    }
}

fn run_local_command(args: &[OsString]) -> Result<IdleDiagnosticReport, Failure> {
    // Parsing is intentionally complete before any model handle, provider, or
    // process is opened. Bad arguments are therefore inert on a developer PC.
    let options = parse_command(args)?;
    let mut model = verify_input(
        &options.model,
        &options.model_sha256,
        MAX_MODEL_BYTES,
        "idle diagnostic model",
    )
    .map_err(|_| Failure::Input)?;
    if model.size != options.model_size {
        return Err(Failure::Input);
    }
    require_retained_digest(&mut model).map_err(|_| Failure::Input)?;
    let artifact = RuntimeArtifact::Gguf(RuntimeModel {
        id: ModelId::new(options.model_id),
        path: model.path.clone(),
        format: ArtifactFormat::Gguf,
        expected_size_bytes: model.size,
        expected_sha256: model.sha256.clone(),
    });
    let audio = Arc::new(
        PreparedAudio::from_captured_mono(
            vec![0.0; PREPARED_SAMPLE_RATE as usize],
            PREPARED_SAMPLE_RATE,
            1,
            PREPARED_SAMPLE_RATE as usize,
        )
        .map_err(|_| Failure::Audio)?,
    );
    let expected = ExpectedGpuTarget {
        backend: options.expected_backend,
        pack_id: options.expected_pack_id,
        stable_device: options.expected_stable_device,
    };
    let mut executor = ProcessExecutor::new(artifact, audio);
    let mut clock = WallClock::new();
    run_with_cleanup(&mut executor, &mut clock, &expected, model.sha256)
}

fn parse_command(args: &[OsString]) -> Result<CommandOptions, Failure> {
    let mut model_id = None;
    let mut model = None;
    let mut model_sha256 = None;
    let mut model_size = None;
    let mut expected_backend = None;
    let mut expected_pack_id = None;
    let mut expected_stable_device = None;
    let mut acknowledged_security_records = false;
    let mut acknowledged_execution_copy = false;
    let mut saw_command = false;
    let mut index = 0;
    while index < args.len() {
        let name = args[index].to_str().ok_or(Failure::Arguments)?;
        match name {
            COMMAND_FLAG => {
                if saw_command {
                    return Err(Failure::Arguments);
                }
                saw_command = true;
                index += 1;
            }
            ACK_LOCAL_SECURITY_RECORDS => {
                if acknowledged_security_records {
                    return Err(Failure::Arguments);
                }
                acknowledged_security_records = true;
                index += 1;
            }
            ACK_LOCAL_EXECUTION_COPY => {
                if acknowledged_execution_copy {
                    return Err(Failure::Arguments);
                }
                acknowledged_execution_copy = true;
                index += 1;
            }
            "--model-id" => take_once(&mut model_id, args, &mut index)?,
            "--model" => take_once(&mut model, args, &mut index)?,
            "--model-sha256" => take_once(&mut model_sha256, args, &mut index)?,
            "--model-size" => take_once(&mut model_size, args, &mut index)?,
            "--gpu-backend" => take_once(&mut expected_backend, args, &mut index)?,
            "--gpu-pack-id" => take_once(&mut expected_pack_id, args, &mut index)?,
            "--gpu-device" => take_once(&mut expected_stable_device, args, &mut index)?,
            _ => return Err(Failure::Arguments),
        }
    }
    if !saw_command || !acknowledged_security_records || !acknowledged_execution_copy {
        return Err(Failure::Arguments);
    }
    let text = |value: Option<OsString>| canonical_text(value).ok_or(Failure::Arguments);
    let backend = text(expected_backend)?;
    if !matches!(backend.as_str(), "cuda" | "vulkan") {
        return Err(Failure::Arguments);
    }
    let model_size = text(model_size)?;
    let model_size = canonical_positive_decimal(&model_size).ok_or(Failure::Arguments)?;
    let model_sha256 = canonical_sha256(&text(model_sha256)?).map_err(|_| Failure::Arguments)?;
    let model = PathBuf::from(model.ok_or(Failure::Arguments)?);
    if !model.is_absolute() {
        return Err(Failure::Arguments);
    }
    Ok(CommandOptions {
        model_id: text(model_id)?,
        model,
        model_sha256,
        model_size,
        expected_backend: backend,
        expected_pack_id: text(expected_pack_id)?,
        expected_stable_device: text(expected_stable_device)?,
    })
}

fn take_once(
    slot: &mut Option<OsString>,
    args: &[OsString],
    index: &mut usize,
) -> Result<(), Failure> {
    if slot.is_some() {
        return Err(Failure::Arguments);
    }
    *index += 1;
    *slot = Some(args.get(*index).cloned().ok_or(Failure::Arguments)?);
    *index += 1;
    Ok(())
}

fn canonical_text(value: Option<OsString>) -> Option<String> {
    let value = value?.into_string().ok()?;
    (!value.is_empty()
        && value.len() <= MAX_TEXT_BYTES
        && value == value.trim()
        && !value.chars().any(char::is_control))
    .then_some(value)
}

fn canonical_positive_decimal(value: &str) -> Option<u64> {
    (!value.starts_with('0') && value.bytes().all(|byte| byte.is_ascii_digit()))
        .then(|| value.parse().ok())
        .flatten()
        .filter(|value| *value > 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};

    #[derive(Default)]
    struct FakeClock {
        elapsed: Duration,
        sleeps: Vec<Duration>,
    }

    impl DiagnosticClock for FakeClock {
        fn elapsed(&self) -> Duration {
            self.elapsed
        }

        fn sleep(&mut self, duration: Duration) {
            self.elapsed += duration;
            self.sleeps.push(duration);
        }
    }

    struct ScriptedClock {
        elapsed_values: RefCell<VecDeque<Duration>>,
        last: Cell<Duration>,
    }

    impl ScriptedClock {
        fn new(elapsed_values: impl IntoIterator<Item = Duration>) -> Self {
            Self {
                elapsed_values: RefCell::new(elapsed_values.into_iter().collect()),
                last: Cell::new(Duration::ZERO),
            }
        }
    }

    impl DiagnosticClock for ScriptedClock {
        fn elapsed(&self) -> Duration {
            if let Some(next) = self.elapsed_values.borrow_mut().pop_front() {
                self.last.set(next);
            }
            self.last.get()
        }

        fn sleep(&mut self, duration: Duration) {
            self.last.set(self.last.get().saturating_add(duration));
        }
    }

    struct FakeExecutor {
        executions: VecDeque<Result<DiagnosticExecution, Failure>>,
        observations: VecDeque<Result<ObservedRoute, Failure>>,
        residency: VecDeque<Result<ModelResidency, Failure>>,
        shutdown: Result<(), Failure>,
        calls: Vec<&'static str>,
    }

    impl DiagnosticExecutor for FakeExecutor {
        fn transcribe(&mut self) -> Result<DiagnosticExecution, Failure> {
            self.calls.push("transcribe");
            self.executions
                .pop_front()
                .unwrap_or(Err(Failure::InitialColdRequest))
        }

        fn observe_loaded(&mut self) -> Result<ObservedRoute, Failure> {
            self.calls.push("observe_loaded");
            self.observations
                .pop_front()
                .unwrap_or(Err(Failure::LostGeneration))
        }

        fn model_residency(&mut self) -> Result<ModelResidency, Failure> {
            self.calls.push("model_residency");
            self.residency
                .pop_front()
                .unwrap_or(Ok(ModelResidency::Loaded))
        }

        fn shutdown(&mut self) -> Result<(), Failure> {
            self.calls.push("shutdown");
            self.shutdown
        }
    }

    fn route() -> DiagnosticRoute {
        DiagnosticRoute {
            backend: "cuda".to_owned(),
            provider: "transcribe-cpp:cuda".to_owned(),
            stable_device: "native:luid:0102030405060708".to_owned(),
            driver: Some("windows-display:1".to_owned()),
            pack_id: "scribe-cuda-windows-x64".to_owned(),
            pack_version: "p1".to_owned(),
            pack_sha256: "a".repeat(64),
            pack_security_epoch: 1,
            runtime_abi: 1,
        }
    }

    fn expected() -> ExpectedGpuTarget {
        ExpectedGpuTarget {
            backend: "cuda".to_owned(),
            pack_id: "scribe-cuda-windows-x64".to_owned(),
            stable_device: "native:luid:0102030405060708".to_owned(),
        }
    }

    fn execution(warm_reused: bool) -> DiagnosticExecution {
        DiagnosticExecution {
            warm_reused,
            model_load_ms: if warm_reused { 0 } else { 12 },
            transcript_sha256: "b".repeat(64),
            explicit_gpu_without_fallback: true,
            route: route(),
        }
    }

    fn observed(generation: u64) -> ObservedRoute {
        ObservedRoute {
            route: route(),
            generation,
        }
    }

    fn successful_executor() -> FakeExecutor {
        let polls = (WARM_MODEL_TTL + IDLE_SETTLEMENT)
            .as_millis()
            .div_ceil(IDLE_POLL_INTERVAL.as_millis()) as usize;
        FakeExecutor {
            executions: VecDeque::from(vec![
                Ok(execution(false)),
                Ok(execution(true)),
                Ok(execution(false)),
            ]),
            observations: VecDeque::from(vec![Ok(observed(7)), Ok(observed(7)), Ok(observed(7))]),
            residency: std::iter::repeat_n(Ok(ModelResidency::Loaded), polls)
                .chain(std::iter::once(Ok(ModelResidency::Unloaded)))
                .collect(),
            shutdown: Ok(()),
            calls: Vec::new(),
        }
    }

    fn command_args() -> Vec<OsString> {
        vec![
            COMMAND_FLAG.into(),
            "--model-id".into(),
            "whisper-base.en-q8".into(),
            "--model".into(),
            r"C:\\fixtures\\model.gguf".into(),
            "--model-sha256".into(),
            "a".repeat(64).into(),
            "--model-size".into(),
            "1234".into(),
            "--gpu-backend".into(),
            "cuda".into(),
            "--gpu-pack-id".into(),
            "scribe-cuda-windows-x64".into(),
            "--gpu-device".into(),
            "native:luid:0102030405060708".into(),
            ACK_LOCAL_SECURITY_RECORDS.into(),
            ACK_LOCAL_EXECUTION_COPY.into(),
        ]
    }

    #[test]
    fn idle_diagnostic_cli_is_exact_and_requires_both_acknowledgements() {
        assert!(parse_command(&command_args()).is_ok());
        for removed in [
            COMMAND_FLAG,
            ACK_LOCAL_SECURITY_RECORDS,
            ACK_LOCAL_EXECUTION_COPY,
        ] {
            let mut args = command_args();
            let index = args.iter().position(|arg| arg == removed).unwrap();
            args.remove(index);
            assert!(parse_command(&args).is_err(), "accepted missing {removed}");
        }
        let mut duplicate = command_args();
        duplicate.push(ACK_LOCAL_EXECUTION_COPY.into());
        assert!(parse_command(&duplicate).is_err());
        let mut unexpected = command_args();
        unexpected.extend(["--idle-ttl".into(), "1".into()]);
        assert!(parse_command(&unexpected).is_err());
        let mut bad_backend = command_args();
        let value = bad_backend
            .iter()
            .position(|arg| arg == "--gpu-backend")
            .unwrap()
            + 1;
        bad_backend[value] = "auto".into();
        assert!(parse_command(&bad_backend).is_err());
        let mut mixed_command = command_args();
        mixed_command.push("--scribe-windows-gpu-pack-probe".into());
        assert!(parse_command(&mixed_command).is_err());
    }

    #[test]
    fn idle_diagnostic_sequence_waits_the_full_ttl_without_sleeping_in_tests() {
        let mut executor = successful_executor();
        let mut clock = FakeClock::default();
        let report =
            run_with_cleanup(&mut executor, &mut clock, &expected(), "a".repeat(64)).unwrap();
        assert_eq!(clock.elapsed, WARM_MODEL_TTL + IDLE_SETTLEMENT);
        assert!(
            clock
                .sleeps
                .iter()
                .all(|sleep| *sleep <= IDLE_POLL_INTERVAL)
        );
        assert_eq!(report.immediate_warm.model_load_ms, 0);
        assert_eq!(report.warm_model_ttl_ms, 300_000);
        assert_eq!(
            report.automatic_unload_observed_after_warm_request_ms,
            315_000
        );
        assert!(report.automatic_unload_observed);
        assert_eq!(
            executor
                .calls
                .iter()
                .filter(|call| **call == "transcribe")
                .count(),
            3
        );
        assert_eq!(executor.calls.last(), Some(&"shutdown"));
    }

    #[test]
    fn idle_diagnostic_rejects_early_unload_and_lost_generation() {
        let mut early = successful_executor();
        early.residency = VecDeque::from(vec![Ok(ModelResidency::Unloaded)]);
        assert_eq!(
            run_with_cleanup(
                &mut early,
                &mut FakeClock::default(),
                &expected(),
                "a".repeat(64)
            ),
            Err(Failure::EarlyUnload)
        );

        let mut lost = successful_executor();
        lost.residency = VecDeque::from(vec![Err(Failure::LostGeneration)]);
        assert_eq!(
            run_with_cleanup(
                &mut lost,
                &mut FakeClock::default(),
                &expected(),
                "a".repeat(64)
            ),
            Err(Failure::LostGeneration)
        );
    }

    #[test]
    fn idle_diagnostic_rejects_unload_after_the_inclusive_settlement_deadline() {
        let deadline = WARM_MODEL_TTL + IDLE_SETTLEMENT;

        let mut jump_past_deadline = successful_executor();
        jump_past_deadline.residency = VecDeque::from(vec![Ok(ModelResidency::Unloaded)]);
        let mut late_clock = ScriptedClock::new([deadline + Duration::from_millis(1)]);
        assert_eq!(
            wait_for_automatic_unload(
                &mut jump_past_deadline,
                &mut late_clock,
                WARM_MODEL_TTL,
                deadline,
            ),
            Err(Failure::IdleTimeout)
        );

        let mut boundary = successful_executor();
        boundary.residency = VecDeque::from(vec![Ok(ModelResidency::Unloaded)]);
        let mut boundary_clock = ScriptedClock::new([WARM_MODEL_TTL, deadline]);
        assert_eq!(
            wait_for_automatic_unload(&mut boundary, &mut boundary_clock, WARM_MODEL_TTL, deadline,),
            Ok(deadline)
        );

        let mut observation_consumed_window = successful_executor();
        observation_consumed_window.residency = VecDeque::from(vec![Ok(ModelResidency::Unloaded)]);
        let mut consuming_clock =
            ScriptedClock::new([WARM_MODEL_TTL, deadline + Duration::from_millis(1)]);
        assert_eq!(
            wait_for_automatic_unload(
                &mut observation_consumed_window,
                &mut consuming_clock,
                WARM_MODEL_TTL,
                deadline,
            ),
            Err(Failure::IdleTimeout)
        );
    }

    #[test]
    fn idle_diagnostic_rejects_cpu_fallback_warm_and_identity_drift() {
        let mut cpu = successful_executor();
        cpu.executions[0] = Ok(DiagnosticExecution {
            explicit_gpu_without_fallback: false,
            ..execution(false)
        });
        assert_eq!(
            run_with_cleanup(
                &mut cpu,
                &mut FakeClock::default(),
                &expected(),
                "a".repeat(64)
            ),
            Err(Failure::CpuOrFallback)
        );

        let mut warm = successful_executor();
        warm.executions[1] = Ok(DiagnosticExecution {
            model_load_ms: 1,
            ..execution(true)
        });
        assert_eq!(
            run_with_cleanup(
                &mut warm,
                &mut FakeClock::default(),
                &expected(),
                "a".repeat(64)
            ),
            Err(Failure::WarmExpectation)
        );

        let mut identity = successful_executor();
        identity.observations[1] = Ok(ObservedRoute {
            generation: 8,
            ..observed(8)
        });
        assert_eq!(
            run_with_cleanup(
                &mut identity,
                &mut FakeClock::default(),
                &expected(),
                "a".repeat(64)
            ),
            Err(Failure::RouteIdentity)
        );

        let mut replacement = successful_executor();
        replacement.observations[2] = Ok(ObservedRoute {
            generation: 8,
            ..observed(8)
        });
        assert_eq!(
            run_with_cleanup(
                &mut replacement,
                &mut FakeClock::default(),
                &expected(),
                "a".repeat(64),
            ),
            Err(Failure::RouteIdentity)
        );
    }

    #[test]
    fn idle_diagnostic_rejects_actual_execution_identity_drift_from_observation() {
        let mut execution_identity = successful_executor();
        let mut actual_route = route();
        actual_route.driver = Some("windows-display:changed".to_owned());
        execution_identity.executions[1] = Ok(DiagnosticExecution {
            route: actual_route,
            ..execution(true)
        });
        assert_eq!(
            run_with_cleanup(
                &mut execution_identity,
                &mut FakeClock::default(),
                &expected(),
                "a".repeat(64),
            ),
            Err(Failure::RouteIdentity)
        );
    }

    #[test]
    fn idle_diagnostic_rejects_warm_or_cold_in_the_wrong_phase() {
        let mut initial_warm = successful_executor();
        initial_warm.executions[0] = Ok(execution(true));
        assert_eq!(
            run_with_cleanup(
                &mut initial_warm,
                &mut FakeClock::default(),
                &expected(),
                "a".repeat(64),
            ),
            Err(Failure::WarmExpectation)
        );

        let mut immediate_cold = successful_executor();
        immediate_cold.executions[1] = Ok(execution(false));
        assert_eq!(
            run_with_cleanup(
                &mut immediate_cold,
                &mut FakeClock::default(),
                &expected(),
                "a".repeat(64),
            ),
            Err(Failure::WarmExpectation)
        );

        let mut post_idle_warm = successful_executor();
        post_idle_warm.executions[2] = Ok(execution(true));
        assert_eq!(
            run_with_cleanup(
                &mut post_idle_warm,
                &mut FakeClock::default(),
                &expected(),
                "a".repeat(64),
            ),
            Err(Failure::WarmExpectation)
        );
    }

    #[test]
    fn idle_diagnostic_rejects_digest_drift_timeout_and_cleanup_failure() {
        let mut drift = successful_executor();
        drift.executions[2] = Ok(DiagnosticExecution {
            transcript_sha256: "c".repeat(64),
            ..execution(false)
        });
        assert_eq!(
            run_with_cleanup(
                &mut drift,
                &mut FakeClock::default(),
                &expected(),
                "a".repeat(64)
            ),
            Err(Failure::TranscriptDrift)
        );

        let mut timeout = successful_executor();
        timeout.residency.clear();
        assert_eq!(
            run_with_cleanup(
                &mut timeout,
                &mut FakeClock::default(),
                &expected(),
                "a".repeat(64)
            ),
            Err(Failure::IdleTimeout)
        );

        let mut cleanup = successful_executor();
        cleanup.shutdown = Err(Failure::Cleanup);
        assert_eq!(
            run_with_cleanup(
                &mut cleanup,
                &mut FakeClock::default(),
                &expected(),
                "a".repeat(64)
            ),
            Err(Failure::Cleanup)
        );

        let mut both = successful_executor();
        both.executions[0] = Err(Failure::InitialColdRequest);
        both.shutdown = Err(Failure::Cleanup);
        assert_eq!(
            run_with_cleanup(
                &mut both,
                &mut FakeClock::default(),
                &expected(),
                "a".repeat(64)
            ),
            Err(Failure::OperationAndCleanup)
        );
    }
}
