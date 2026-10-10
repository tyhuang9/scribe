//! Opt-in, unsigned Windows GPU capture observation.
//!
//! This collector deliberately stops before qualification: it runs one CPU
//! and one exact verified-GPU request, retains only validated Hello/Ready
//! bytes plus bounded native telemetry, and publishes an unqualified report.

mod campaign;
mod telemetry;

use std::ffi::{OsStr, OsString};
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result, anyhow, bail};
use serde::Serialize;
use sha2::{Digest, Sha256};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_ATTRIBUTE_REPARSE_POINT, FILE_SHARE_READ, MoveFileExW,
};

use crate::backend_policy::PowerSource;
use crate::model_catalog::ArtifactFormat;
use crate::onnx_worker::{
    CaptureFrozenInputs, CaptureObservationWorker, CapturePackPin, CaptureWorkerPin,
    GpuCaptureObservationIdentity, ProviderMemoryNotApplicableReason, ProviderMemoryObservation,
    WorkerMemoryAvailability, WorkerObservationLease, validate_capture_worker_memory,
};
use crate::prepared_audio::PreparedAudio;
use crate::runtime_artifact::{RuntimeArtifact, RuntimeModel};
use crate::runtime_contract::RuntimeExecution;
use crate::transcription::{AccelerationPreference, ModelId};
use telemetry::{SamplingSession, TelemetrySummary, VideoMemorySummary};

const COMMAND_FLAG: &str = "--scribe-windows-gpu-capture-observation";
const MAX_MODEL_BYTES: u64 = 8 * 1024 * 1024 * 1024;
const MAX_WAV_BYTES: u64 = 256 * 1024 * 1024;
const MAX_REPORT_BYTES: usize = 1024 * 1024;
const MAX_SELECTOR_BYTES: usize = 256;

#[derive(Debug)]
struct CommandOptions {
    model: PathBuf,
    model_sha256: String,
    wav: PathBuf,
    wav_sha256: String,
    gpu_pack_id: String,
    gpu_backend: String,
    gpu_device: String,
    output: PathBuf,
    campaign_power: Option<CampaignPower>,
    frozen_inputs: Option<Arc<CaptureFrozenInputs>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum CampaignPower {
    Ac,
    Battery,
}

impl CampaignPower {
    fn source(self) -> PowerSource {
        match self {
            Self::Ac => PowerSource::Ac,
            Self::Battery => PowerSource::Battery,
        }
    }
}

struct VerifiedInput {
    path: PathBuf,
    file: File,
    size: u64,
    sha256: String,
}

#[derive(Serialize)]
struct CaptureReport {
    schema_version: u8,
    kind: &'static str,
    unsigned: bool,
    unqualified: bool,
    auto_eligible: bool,
    release_approved: bool,
    collector_build_revision: &'static str,
    inputs: InputReport,
    gpu_identity: GpuCaptureObservationIdentity,
    cpu: WorkerReport,
    gpu: WorkerReport,
    transcript_parity: bool,
    unavailable: UnavailableReport,
}

#[derive(Serialize)]
struct InputReport {
    model_sha256: String,
    wav_sha256: String,
}

#[derive(Serialize)]
struct WorkerReport {
    hello_frame_hex: String,
    ready_frame_hex: String,
    power_source_before: PowerSource,
    power_source_after: PowerSource,
    elapsed_ms: u64,
    sampled_max_private_usage_bytes: u64,
    telemetry_sample_count: u64,
    video_memory: VideoMemoryReport,
    provider_memory: ProviderMemoryReport,
    memory_availability: MemoryAvailabilityReport,
    normalized_transcript_sha256: String,
}

#[derive(Serialize)]
struct ProviderMemoryReport {
    before: ProviderMemoryObservation,
    after: ProviderMemoryObservation,
}

#[derive(Serialize)]
struct MemoryAvailabilityReport {
    before: WorkerMemoryAvailability,
    after: WorkerMemoryAvailability,
}

#[derive(Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum VideoMemoryReport {
    Available {
        local: telemetry::SegmentSummary,
        non_local: telemetry::SegmentSummary,
    },
    NotApplicable,
}

#[derive(Serialize)]
struct UnavailableReport {
    inference_thread_count: UnavailableField,
    thermal_state: UnavailableField,
}

#[derive(Clone, Copy, Serialize)]
struct UnavailableField {
    status: &'static str,
    reason: &'static str,
}

struct ObservedWorker {
    report: WorkerReport,
    normalized_transcript_sha256: String,
}

struct WorkerObservationMeasurements {
    provider_memory: ProviderMemoryReport,
    memory_availability: MemoryAvailabilityReport,
    telemetry: TelemetrySummary,
    power_source_before: PowerSource,
    power_source_after: PowerSource,
    elapsed_ms: u128,
}

pub(crate) fn maybe_run_local_command() -> Option<i32> {
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    if !args.iter().any(|arg| arg == COMMAND_FLAG) {
        return None;
    }
    match run_local_command(&args) {
        Ok(()) => Some(0),
        Err(error) => {
            eprintln!("Windows GPU capture observation failed: {error:#}");
            Some(1)
        }
    }
}

fn run_local_command(args: &[OsString]) -> Result<()> {
    let options = parse_command(args)?;
    if options.campaign_power.is_some() {
        return campaign::run(options);
    }
    run_single_capture(options)
}

fn run_single_capture(options: CommandOptions) -> Result<()> {
    let mut model = verify_input(
        &options.model,
        &options.model_sha256,
        MAX_MODEL_BYTES,
        "model",
    )?;
    let mut wav = verify_input(&options.wav, &options.wav_sha256, MAX_WAV_BYTES, "WAV")?;
    // Re-hash from the retained handles before launching either worker. The
    // read-only, non-delete-sharing handles then remain live through cleanup.
    require_retained_digest(&mut model)?;
    require_retained_digest(&mut wav)?;
    let artifact = RuntimeArtifact::Gguf(RuntimeModel {
        id: ModelId::new("windows-gpu-capture-observation"),
        path: model.path.clone(),
        format: ArtifactFormat::Gguf,
        expected_size_bytes: model.size,
        expected_sha256: model.sha256.clone(),
    });

    // Authenticate and retain both exact worker generations before decoding
    // the WAV or issuing a model command. Empty production pack trust therefore
    // fails before the CPU request or costly model/audio execution.
    let mut cpu_worker: Option<CaptureObservationWorker> = None;
    let mut gpu_worker: Option<CaptureObservationWorker> = None;
    let mut cpu_lease = None;
    let mut gpu_lease = None;
    let preflight = single_preflight(options.frozen_inputs.is_some(), |step| {
        match step {
            SinglePreflightStep::CpuHello => {
                let worker =
                    cpu_worker.insert(CaptureObservationWorker::cpu(options.frozen_inputs.clone()));
                cpu_lease = Some(
                    worker
                        .prepare_observation()
                        .context("could not preflight the bundled CPU worker")?,
                );
            }
            SinglePreflightStep::GpuAdmission => {
                // With frozen inputs this admits BOTH identities before any
                // provider probe. Without them the legacy CPU-first order stays.
                gpu_worker = Some(
                    CaptureObservationWorker::gpu(
                        &options.gpu_pack_id,
                        &options.gpu_backend,
                        &options.gpu_device,
                        options.frozen_inputs.clone(),
                    )
                    .context("could not preflight the verified GPU binding")?,
                );
            }
            SinglePreflightStep::GpuHello => {
                gpu_lease = Some(
                    gpu_worker
                        .as_ref()
                        .expect("GPU admission precedes Hello")
                        .prepare_observation()
                        .context("could not preflight the selected GPU worker")?,
                );
            }
        }
        Ok(())
    });
    if let Err(error) = preflight {
        let gpu_cleanup = gpu_worker
            .as_ref()
            .map_or(Ok(()), |worker| worker.shutdown());
        let cpu_cleanup = cpu_worker
            .as_ref()
            .map_or(Ok(()), |worker| worker.shutdown());
        return combine_operation_cleanup(Err(error), gpu_cleanup.and(cpu_cleanup));
    }
    let cpu_worker = cpu_worker.expect("successful preflight retains CPU worker");
    let gpu_worker = gpu_worker.expect("successful preflight retains GPU worker");
    let cpu_lease = cpu_lease.expect("successful preflight retains CPU lease");
    let gpu_lease = gpu_lease.expect("successful preflight retains GPU lease");
    if let Err(error) = cpu_worker
        .negotiate_runtime_observation()
        .context("bundled CPU worker does not support runtime memory observation")
        .and_then(|()| {
            gpu_worker
                .negotiate_runtime_observation()
                .context("selected GPU worker does not support runtime memory observation")
        })
    {
        let cleanup = gpu_worker.shutdown().and(cpu_worker.shutdown());
        return combine_operation_cleanup(Err(error), cleanup);
    }
    wav.file.seek(SeekFrom::Start(0))?;
    let audio = match PreparedAudio::from_wav_reader(&mut wav.file)
        .context("hash-pinned WAV input could not be prepared")
    {
        Ok(audio) => audio,
        Err(error) => {
            let cleanup = gpu_worker.shutdown().and(cpu_worker.shutdown());
            return combine_operation_cleanup(Err(error), cleanup);
        }
    };
    let cpu = match observe_and_shutdown(
        cpu_worker,
        cpu_lease,
        artifact.clone(),
        AccelerationPreference::Cpu,
        &audio,
        false,
    ) {
        Ok(observation) => observation,
        Err(error) => {
            return combine_operation_cleanup(Err(error), gpu_worker.shutdown());
        }
    };
    let gpu_identity = gpu_worker
        .gpu_identity
        .clone()
        .ok_or_else(|| anyhow!("GPU observation worker omitted its verified identity"))?;
    let gpu = observe_and_shutdown(
        gpu_worker,
        gpu_lease,
        artifact,
        AccelerationPreference::Gpu,
        &audio,
        true,
    )?;
    if cpu.report.power_source_before != gpu.report.power_source_before {
        bail!("CPU and GPU observations crossed a power-source boundary")
    }
    let transcript_parity = cpu.normalized_transcript_sha256 == gpu.normalized_transcript_sha256;
    let unavailable = UnavailableField {
        status: "unavailable",
        reason: "not_observed",
    };
    let report = CaptureReport {
        schema_version: 3,
        kind: "windows_gpu_capture_observation",
        unsigned: true,
        unqualified: true,
        auto_eligible: false,
        release_approved: false,
        collector_build_revision: env!("SCRIBE_BUILD_REVISION"),
        inputs: InputReport {
            model_sha256: model.sha256,
            wav_sha256: wav.sha256,
        },
        gpu_identity,
        cpu: cpu.report,
        gpu: gpu.report,
        transcript_parity,
        unavailable: UnavailableReport {
            inference_thread_count: UnavailableField {
                status: "unavailable",
                reason: "unsupported_by_pinned_runtime_api",
            },
            thermal_state: unavailable,
        },
    };
    let mut bytes = serde_json::to_vec(&report).context("serialize capture observation report")?;
    bytes.push(b'\n');
    if bytes.len() > MAX_REPORT_BYTES {
        bail!("capture observation report exceeds the {MAX_REPORT_BYTES}-byte limit")
    }
    publish_atomic_new(&options.output, &bytes)
}

fn observe_and_shutdown(
    worker: CaptureObservationWorker,
    lease: WorkerObservationLease,
    artifact: RuntimeArtifact,
    preference: AccelerationPreference,
    audio: &PreparedAudio,
    gpu: bool,
) -> Result<ObservedWorker> {
    let operation = observe_worker(&worker, lease, artifact, preference, audio, gpu);
    let cleanup = worker.shutdown();
    combine_operation_cleanup(operation, cleanup)
}

fn combine_operation_cleanup<T>(operation: Result<T>, cleanup: Result<()>) -> Result<T> {
    match (operation, cleanup) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(error)) => {
            Err(error.context("worker cleanup failed before report publication"))
        }
        (Err(operation), Err(cleanup)) => Err(operation.context(format!(
            "worker cleanup also failed before report publication: {cleanup:#}"
        ))),
    }
}

fn observe_worker(
    worker: &CaptureObservationWorker,
    lease: WorkerObservationLease,
    artifact: RuntimeArtifact,
    preference: AccelerationPreference,
    audio: &PreparedAudio,
    gpu: bool,
) -> Result<ObservedWorker> {
    validate_handshake_capture(&lease)?;
    let power_source_before = PowerSource::current();
    if power_source_before == PowerSource::Unknown {
        bail!("power source was unknown before the worker request")
    }
    let started = Instant::now();
    let sampler = if gpu {
        let stable = worker
            .gpu_identity
            .as_ref()
            .ok_or_else(|| anyhow!("GPU telemetry requires a verified stable device"))?
            .stable_device
            .as_str();
        SamplingSession::gpu(lease.clone(), stable)?
    } else {
        SamplingSession::cpu(lease.clone())?
    };
    let execution = worker.transcribe_observed(artifact, preference, audio);
    let elapsed = started.elapsed().as_millis();
    let telemetry = sampler.finish();
    let power_source_after = PowerSource::current();
    let execution = execution?;
    let telemetry = telemetry?;
    lease.require_current()?;
    require_stable_power(power_source_before, power_source_after)?;
    build_worker_report(
        &lease,
        execution.execution,
        WorkerObservationMeasurements {
            provider_memory: ProviderMemoryReport {
                before: execution.before,
                after: execution.after,
            },
            memory_availability: MemoryAvailabilityReport {
                before: execution.availability_before,
                after: execution.availability_after,
            },
            telemetry,
            power_source_before,
            power_source_after,
            elapsed_ms: elapsed,
        },
        gpu,
        worker.gpu_identity.as_ref(),
    )
}

fn require_stable_power(before: PowerSource, after: PowerSource) -> Result<()> {
    if before == PowerSource::Unknown || after == PowerSource::Unknown || after != before {
        bail!("power source was unknown or changed during the worker request")
    }
    Ok(())
}

fn validate_handshake_capture(lease: &WorkerObservationLease) -> Result<()> {
    for (label, frame) in [
        ("Hello", lease.hello_frame()),
        ("Ready", lease.ready_frame()),
    ] {
        if frame.len() < 26 || frame.len() > 26 + 256 * 1024 || &frame[..4] != b"SCIF" {
            bail!("validated {label} capture is outside the raw SCIF bounds")
        }
    }
    Ok(())
}

fn build_worker_report(
    lease: &WorkerObservationLease,
    execution: RuntimeExecution,
    measurements: WorkerObservationMeasurements,
    gpu: bool,
    expected_gpu: Option<&GpuCaptureObservationIdentity>,
) -> Result<ObservedWorker> {
    let WorkerObservationMeasurements {
        provider_memory,
        memory_availability,
        telemetry,
        power_source_before,
        power_source_after,
        elapsed_ms,
    } = measurements;
    let normalized = normalize_transcript(&execution.transcript.text);
    let normalized_transcript_sha256 = format!("{:x}", Sha256::digest(normalized.as_bytes()));
    let video_memory = match (gpu, telemetry.video_memory) {
        (true, Some(VideoMemorySummary { local, non_local })) => {
            VideoMemoryReport::Available { local, non_local }
        }
        (true, None) => bail!("GPU observation omitted required local/non-local memory telemetry"),
        (false, None) => VideoMemoryReport::NotApplicable,
        (false, Some(_)) => bail!("CPU observation unexpectedly reported GPU video memory"),
    };
    validate_provider_memory_report(gpu, expected_gpu, &provider_memory)?;
    for availability in [&memory_availability.before, &memory_availability.after] {
        validate_capture_worker_memory(
            availability,
            if gpu {
                AccelerationPreference::Gpu
            } else {
                AccelerationPreference::Cpu
            },
            expected_gpu,
        )?;
    }
    let report = WorkerReport {
        hello_frame_hex: hex(lease.hello_frame()),
        ready_frame_hex: hex(lease.ready_frame()),
        power_source_before,
        power_source_after,
        elapsed_ms: u64::try_from(elapsed_ms)
            .map_err(|_| anyhow!("worker request duration exceeded the report integer range"))?,
        sampled_max_private_usage_bytes: telemetry.sampled_max_private_usage_bytes,
        telemetry_sample_count: telemetry.sample_count,
        video_memory,
        provider_memory,
        memory_availability,
        normalized_transcript_sha256: normalized_transcript_sha256.clone(),
    };
    Ok(ObservedWorker {
        report,
        normalized_transcript_sha256,
    })
}

fn validate_provider_memory_report(
    gpu: bool,
    expected_gpu: Option<&GpuCaptureObservationIdentity>,
    report: &ProviderMemoryReport,
) -> Result<()> {
    for observation in [&report.before, &report.after] {
        match (gpu, observation) {
            (
                false,
                ProviderMemoryObservation::NotApplicable {
                    reason: ProviderMemoryNotApplicableReason::CpuProvider,
                },
            )
            | (true, ProviderMemoryObservation::Unavailable { .. }) => {}
            (
                true,
                ProviderMemoryObservation::Available {
                    backend,
                    provider_id,
                    stable_device,
                    memory_total_bytes,
                    ..
                },
            ) => {
                let expected = expected_gpu.ok_or_else(|| {
                    anyhow!("GPU provider memory validation omitted the authenticated identity")
                })?;
                if backend != &expected.backend
                    || provider_id != &expected.provider
                    || stable_device != &expected.stable_device
                    || memory_total_bytes != &expected.memory_total_bytes
                {
                    bail!("provider memory observation does not match the authenticated GPU")
                }
            }
            _ => bail!("provider memory observation does not match the worker provider"),
        }
    }
    Ok(())
}

fn normalize_transcript(value: &str) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SinglePreflightStep {
    CpuHello,
    GpuAdmission,
    GpuHello,
}

fn single_preflight(
    frozen: bool,
    mut execute: impl FnMut(SinglePreflightStep) -> Result<()>,
) -> Result<()> {
    use SinglePreflightStep::{CpuHello, GpuAdmission, GpuHello};
    let steps = if frozen {
        [GpuAdmission, CpuHello, GpuHello]
    } else {
        [CpuHello, GpuAdmission, GpuHello]
    };
    for step in steps {
        execute(step)?;
    }
    Ok(())
}

fn parse_command(args: &[OsString]) -> Result<CommandOptions> {
    let mut model = None;
    let mut model_sha256 = None;
    let mut wav = None;
    let mut wav_sha256 = None;
    let mut gpu_pack_id = None;
    let mut gpu_backend = None;
    let mut gpu_device = None;
    let mut output = None;
    let mut campaign_power = None;
    let pin_flags = [
        "--cpu-worker-build-id",
        "--cpu-worker-sha256",
        "--cpu-worker-protocol",
        "--cpu-worker-abi",
        "--gpu-worker-build-id",
        "--gpu-worker-sha256",
        "--gpu-worker-protocol",
        "--gpu-worker-abi",
        "--gpu-pack-version",
        "--gpu-pack-sha256",
        "--gpu-pack-security-epoch",
    ];
    let mut pin_values: [Option<OsString>; 11] = Default::default();
    let mut saw_command = false;
    let mut index = 0;
    while index < args.len() {
        let name = args[index]
            .to_str()
            .ok_or_else(|| anyhow!("capture observation arguments must be Unicode"))?;
        if name == "--campaign-power" {
            if campaign_power.is_some() {
                bail!("--campaign-power may be specified only once")
            }
            index += 1;
            let value = args
                .get(index)
                .and_then(|value| value.to_str())
                .ok_or_else(|| anyhow!("--campaign-power requires a Unicode value"))?;
            campaign_power = Some(match value {
                "ac" => CampaignPower::Ac,
                "battery" => CampaignPower::Battery,
                _ => bail!("--campaign-power must be ac or battery"),
            });
            index += 1;
            continue;
        }
        let slot = match name {
            COMMAND_FLAG => {
                if saw_command {
                    bail!("{COMMAND_FLAG} may be specified only once")
                }
                saw_command = true;
                index += 1;
                continue;
            }
            "--model" => &mut model,
            "--model-sha256" => &mut model_sha256,
            "--wav" => &mut wav,
            "--wav-sha256" => &mut wav_sha256,
            "--gpu-pack-id" => &mut gpu_pack_id,
            "--gpu-backend" => &mut gpu_backend,
            "--gpu-device" => &mut gpu_device,
            "--output" => &mut output,
            _ => match pin_flags.iter().position(|flag| *flag == name) {
                Some(position) => &mut pin_values[position],
                None => bail!("unknown Windows GPU capture observation argument: {name}"),
            },
        };
        if slot.is_some() {
            bail!("{name} may be specified only once")
        }
        index += 1;
        let value = args
            .get(index)
            .ok_or_else(|| anyhow!("{name} requires a value"))?
            .clone();
        *slot = Some(value);
        index += 1;
    }
    if !saw_command {
        bail!("missing {COMMAND_FLAG}")
    }
    let required = |value: Option<OsString>, name: &str| {
        value.ok_or_else(|| anyhow!("{name} is required for capture observation"))
    };
    let canonical_text = |value: Option<OsString>, name: &str| -> Result<String> {
        let value = required(value, name)?;
        let value = value
            .to_str()
            .ok_or_else(|| anyhow!("{name} must be Unicode"))?;
        if value.is_empty()
            || value.len() > MAX_SELECTOR_BYTES
            || value != value.trim()
            || value.chars().any(char::is_control)
        {
            bail!("{name} is empty, oversized, or noncanonical")
        }
        Ok(value.to_owned())
    };
    let model_sha256 = canonical_sha256(&canonical_text(model_sha256, "--model-sha256")?)?;
    let wav_sha256 = canonical_sha256(&canonical_text(wav_sha256, "--wav-sha256")?)?;
    let gpu_backend = canonical_text(gpu_backend, "--gpu-backend")?;
    if !matches!(gpu_backend.as_str(), "cuda" | "vulkan") {
        bail!("--gpu-backend must be cuda or vulkan")
    }
    let gpu_pack_id = canonical_text(gpu_pack_id, "--gpu-pack-id")?;
    let frozen_inputs = if pin_values.iter().all(Option::is_none) {
        None
    } else {
        let mut values = Vec::with_capacity(pin_values.len());
        for (value, flag) in pin_values.into_iter().zip(pin_flags) {
            values.push(canonical_text(value, flag)?);
        }
        let number = |index: usize| -> Result<u64> {
            let value = &values[index];
            if value.starts_with('0') || !value.bytes().all(|byte| byte.is_ascii_digit()) {
                bail!("capture frozen numeric constraints must be positive canonical decimal");
            }
            value
                .parse()
                .context("capture frozen numeric constraint is out of range")
        };
        let worker = |offset: usize| -> Result<CaptureWorkerPin> {
            Ok(CaptureWorkerPin {
                worker_build_id: values[offset].clone(),
                worker_sha256: values[offset + 1].clone(),
                protocol_version: u8::try_from(number(offset + 2)?)?,
                runtime_abi: u16::try_from(number(offset + 3)?)?,
            })
        };
        let cpu_baseline = worker(0)?;
        let gpu_worker = worker(4)?;
        let frozen = CaptureFrozenInputs {
            cpu_baseline,
            pack: CapturePackPin {
                pack_id: gpu_pack_id.clone(),
                pack_version: values[8].clone(),
                pack_digest: values[9].clone(),
                security_epoch: number(10)?,
                runtime_abi: gpu_worker.runtime_abi,
            },
            gpu_worker,
        };
        frozen.validate()?;
        Some(Arc::new(frozen))
    };
    Ok(CommandOptions {
        model: PathBuf::from(required(model, "--model")?),
        model_sha256,
        wav: PathBuf::from(required(wav, "--wav")?),
        wav_sha256,
        gpu_pack_id,
        gpu_backend,
        gpu_device: canonical_text(gpu_device, "--gpu-device")?,
        output: PathBuf::from(required(output, "--output")?),
        campaign_power,
        frozen_inputs,
    })
}

fn canonical_sha256(value: &str) -> Result<String> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        bail!("SHA-256 values must be exactly 64 lowercase hexadecimal characters")
    }
    Ok(value.to_owned())
}

fn verify_input(path: &Path, expected: &str, max_bytes: u64, label: &str) -> Result<VerifiedInput> {
    if !path.is_absolute() {
        bail!("{label} input must use an absolute path")
    }
    reject_reparse_components(path)?;
    let metadata =
        std::fs::symlink_metadata(path).with_context(|| format!("{label} input does not exist"))?;
    if !metadata.is_file()
        || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
        || metadata.len() == 0
        || metadata.len() > max_bytes
    {
        bail!("{label} input is not a bounded regular non-reparse file")
    }
    let mut options = OpenOptions::new();
    options.read(true).share_mode(FILE_SHARE_READ);
    let mut file = options
        .open(path)
        .with_context(|| format!("could not retain the {label} input"))?;
    let sha256 = hash_file(&mut file)?;
    if sha256 != expected {
        bail!("{label} input does not match its pinned SHA-256")
    }
    Ok(VerifiedInput {
        path: path.to_owned(),
        file,
        size: metadata.len(),
        sha256,
    })
}

fn require_retained_digest(input: &mut VerifiedInput) -> Result<()> {
    if hash_file(&mut input.file)? != input.sha256 {
        bail!("retained capture input changed before worker launch")
    }
    Ok(())
}

fn hash_file(file: &mut File) -> Result<String> {
    file.seek(SeekFrom::Start(0))?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    file.seek(SeekFrom::Start(0))?;
    Ok(format!("{:x}", digest.finalize()))
}

fn reject_reparse_components(path: &Path) -> Result<()> {
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component.as_os_str());
        if matches!(component, Component::Prefix(_) | Component::RootDir) {
            continue;
        }
        if std::fs::symlink_metadata(&current)
            .is_ok_and(|metadata| metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0)
        {
            bail!("capture observation paths may not traverse reparse points")
        }
    }
    Ok(())
}

fn publish_atomic_new(output: &Path, bytes: &[u8]) -> Result<()> {
    publish_atomic_new_with(output, bytes, |_, _| Ok(()))
}

fn publish_atomic_new_with(
    output: &Path,
    bytes: &[u8],
    before_publish: impl FnOnce(&Path, &Path) -> Result<()>,
) -> Result<()> {
    if !output.is_absolute() {
        bail!("capture observation output must use an absolute path")
    }
    let parent = output
        .parent()
        .ok_or_else(|| anyhow!("capture observation output has no parent directory"))?;
    reject_reparse_components(parent)?;
    let parent_metadata = std::fs::symlink_metadata(parent)
        .context("capture observation output parent does not exist")?;
    if !parent_metadata.is_dir()
        || parent_metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    {
        bail!("capture observation output parent must be a physical directory")
    }
    if std::fs::symlink_metadata(output).is_ok() {
        bail!("capture observation output already exists")
    }
    let name = output
        .file_name()
        .and_then(OsStr::to_str)
        .filter(|name| !name.is_empty() && name.len() <= 200 && !name.contains(':'))
        .ok_or_else(|| anyhow!("capture observation output filename is invalid"))?;
    let mut nonce = [0_u8; 16];
    getrandom::fill(&mut nonce)
        .map_err(|error| anyhow!("could not create an output staging nonce: {error}"))?;
    let pending = parent.join(format!(".{name}.{}.pending", hex(&nonce)));
    let staged = (|| -> Result<()> {
        let mut options = OpenOptions::new();
        options
            .write(true)
            .create_new(true)
            .share_mode(FILE_SHARE_READ);
        let mut file = options
            .open(&pending)
            .context("could not create the private pending observation output")?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        if std::fs::symlink_metadata(output).is_ok() {
            bail!("capture observation output appeared before publication")
        }
        // The test seam runs only after the final advisory check, immediately
        // before the atomic OS operation, so it proves the native no-replace
        // primitive rather than either earlier existence check.
        before_publish(&pending, output)?;
        let pending_wide = pending
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect::<Vec<_>>();
        let output_wide = output
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect::<Vec<_>>();
        // SAFETY: both buffers are terminated UTF-16 paths and remain live
        // for the synchronous call. Flags are deliberately zero: unlike
        // std::fs::rename on Windows, MoveFileExW without
        // MOVEFILE_REPLACE_EXISTING atomically fails if the destination wins
        // the publication race.
        if unsafe { MoveFileExW(pending_wide.as_ptr(), output_wide.as_ptr(), 0) } == 0 {
            return Err(std::io::Error::last_os_error())
                .context("could not atomically publish the new capture observation output");
        }
        Ok(())
    })();
    if staged.is_err() {
        let _ = std::fs::remove_file(&pending);
    }
    staged
}

fn hex(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        output.push(ALPHABET[(byte >> 4) as usize] as char);
        output.push(ALPHABET[(byte & 0xf) as usize] as char);
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    fn command_args() -> Vec<OsString> {
        vec![
            COMMAND_FLAG.into(),
            "--model".into(),
            r"C:\fixtures\known.gguf".into(),
            "--model-sha256".into(),
            "a".repeat(64).into(),
            "--wav".into(),
            r"C:\fixtures\known.wav".into(),
            "--wav-sha256".into(),
            "b".repeat(64).into(),
            "--gpu-pack-id".into(),
            "scribe-vulkan-windows-x64".into(),
            "--gpu-backend".into(),
            "vulkan".into(),
            "--gpu-device".into(),
            "native:luid:0102030405060708".into(),
            "--output".into(),
            r"C:\capture\observation.json".into(),
        ]
    }

    #[test]
    fn capture_observation_cli_is_exact_and_bounded() {
        let parsed = parse_command(&command_args()).unwrap();
        assert_eq!(parsed.campaign_power, None);
        assert_eq!(parsed.gpu_backend, "vulkan");
        assert_eq!(parsed.gpu_device, "native:luid:0102030405060708");
        let mut duplicate = command_args();
        duplicate.extend(["--gpu-backend".into(), "cuda".into()]);
        assert!(parse_command(&duplicate).is_err());
        let mut unknown = command_args();
        unknown.extend(["--trust-root".into(), r"C:\fixture-key".into()]);
        assert!(parse_command(&unknown).is_err());
        let mut uppercase = command_args();
        let digest = uppercase
            .iter()
            .position(|value| value == "--wav-sha256")
            .unwrap()
            + 1;
        uppercase[digest] = "B".repeat(64).into();
        assert!(parse_command(&uppercase).is_err());
    }

    #[test]
    fn capture_observation_single_preflight_preserves_unpinned_order_and_stops_at_first_failure() {
        use SinglePreflightStep::{CpuHello, GpuAdmission, GpuHello};
        for (frozen, expected) in [
            (false, [CpuHello, GpuAdmission, GpuHello]),
            (true, [GpuAdmission, CpuHello, GpuHello]),
        ] {
            let mut seen = Vec::new();
            single_preflight(frozen, |step| {
                seen.push(step);
                Ok(())
            })
            .unwrap();
            assert_eq!(seen, expected);
            for failed in 0..expected.len() {
                let mut seen = Vec::new();
                let error = single_preflight(frozen, |step| {
                    seen.push(step);
                    if step == expected[failed] {
                        bail!("fixture preflight failure")
                    }
                    Ok(())
                })
                .unwrap_err();
                assert_eq!(error.to_string(), "fixture preflight failure");
                assert_eq!(seen, expected[..=failed]);
            }
        }
    }

    fn frozen_command_args() -> Vec<OsString> {
        let mut args = command_args();
        for (flag, value) in [
            (
                "--cpu-worker-build-id",
                crate::onnx_worker::INFERENCE_WORKER_BUILD_ID.to_owned(),
            ),
            ("--cpu-worker-sha256", "c".repeat(64)),
            ("--cpu-worker-protocol", "5".to_owned()),
            ("--cpu-worker-abi", "1".to_owned()),
            (
                "--gpu-worker-build-id",
                crate::onnx_worker::INFERENCE_WORKER_BUILD_ID.to_owned(),
            ),
            ("--gpu-worker-sha256", "d".repeat(64)),
            ("--gpu-worker-protocol", "5".to_owned()),
            ("--gpu-worker-abi", "1".to_owned()),
            ("--gpu-pack-version", "p1-fixture".to_owned()),
            ("--gpu-pack-sha256", "e".repeat(64)),
            ("--gpu-pack-security-epoch", "1".to_owned()),
        ] {
            args.extend([flag.into(), value.into()]);
        }
        args
    }

    #[test]
    fn capture_observation_frozen_cli_is_optional_complete_and_exact() {
        assert!(
            parse_command(&command_args())
                .unwrap()
                .frozen_inputs
                .is_none()
        );
        let args = frozen_command_args();
        let pins = parse_command(&args).unwrap().frozen_inputs.unwrap();
        assert_eq!(pins.cpu_baseline.worker_sha256, "c".repeat(64));
        assert_eq!(pins.gpu_worker.worker_sha256, "d".repeat(64));
        assert_eq!(pins.pack.pack_id, "scribe-vulkan-windows-x64");
        assert_eq!(pins.pack.pack_digest, "e".repeat(64));
        for index in (command_args().len()..args.len()).step_by(2) {
            let mut partial = args.clone();
            partial.drain(index..index + 2);
            assert!(
                parse_command(&partial).is_err(),
                "accepted partial pins at {index}"
            );
            let mut duplicate = args.clone();
            duplicate.extend_from_slice(&args[index..index + 2]);
            assert!(
                parse_command(&duplicate).is_err(),
                "accepted duplicate pins at {index}"
            );
        }
    }

    #[test]
    fn capture_observation_frozen_cli_rejects_noncanonical_or_incompatible_values_before_io() {
        for (flag, invalid) in [
            ("--cpu-worker-build-id", "short".to_owned()),
            ("--gpu-worker-build-id", "has space build".to_owned()),
            ("--gpu-worker-build-id", "x".repeat(193)),
            ("--cpu-worker-sha256", "0".repeat(64)),
            ("--gpu-worker-sha256", "A".repeat(64)),
            ("--gpu-pack-sha256", "a".repeat(63)),
            ("--cpu-worker-protocol", "4".to_owned()),
            ("--gpu-worker-protocol", "05".to_owned()),
            ("--cpu-worker-abi", "2".to_owned()),
            ("--gpu-worker-abi", "+1".to_owned()),
            ("--gpu-pack-version", "../pack".to_owned()),
            ("--gpu-pack-version", "CON".to_owned()),
            ("--gpu-pack-version", "con.log".to_owned()),
            ("--gpu-pack-security-epoch", "0".to_owned()),
            ("--gpu-pack-security-epoch", "01".to_owned()),
            (
                "--gpu-pack-security-epoch",
                "18446744073709551616".to_owned(),
            ),
        ] {
            let mut args = frozen_command_args();
            let index = args.iter().position(|value| value == flag).unwrap();
            args[index + 1] = invalid.into();
            let error = run_local_command(&args).unwrap_err();
            assert!(
                !error.to_string().contains("input does not exist"),
                "{flag} reached file IO: {error:#}"
            );
        }
    }

    #[test]
    fn capture_campaign_cli_requires_one_exact_optional_power() {
        for (text, expected) in [
            ("ac", CampaignPower::Ac),
            ("battery", CampaignPower::Battery),
        ] {
            let mut args = command_args();
            args.extend(["--campaign-power".into(), text.into()]);
            assert_eq!(parse_command(&args).unwrap().campaign_power, Some(expected));
            args.extend(["--campaign-power".into(), text.into()]);
            assert!(parse_command(&args).is_err());
        }
        for invalid in [
            "", "AC", "Battery", "unknown", "auto", " ac", "battery ", "--output",
        ] {
            let mut args = command_args();
            args.extend(["--campaign-power".into(), invalid.into()]);
            assert!(parse_command(&args).is_err());
        }
        let mut missing = command_args();
        missing.push("--campaign-power".into());
        assert!(parse_command(&missing).is_err());
    }

    #[test]
    fn capture_observation_report_is_unsigned_private_and_bounded() {
        let unavailable = UnavailableField {
            status: "unavailable",
            reason: "not_observed",
        };
        let worker =
            |transcript: &str, video_memory, provider_memory, memory_availability| WorkerReport {
                hello_frame_hex: hex(b"SCIF-hello"),
                ready_frame_hex: hex(b"SCIF-ready"),
                power_source_before: PowerSource::Ac,
                power_source_after: PowerSource::Ac,
                elapsed_ms: 10,
                sampled_max_private_usage_bytes: 20,
                telemetry_sample_count: 2,
                video_memory,
                provider_memory,
                memory_availability,
                normalized_transcript_sha256: format!(
                    "{:x}",
                    Sha256::digest(transcript.as_bytes())
                ),
            };
        let report = CaptureReport {
            schema_version: 3,
            kind: "windows_gpu_capture_observation",
            unsigned: true,
            unqualified: true,
            auto_eligible: false,
            release_approved: false,
            collector_build_revision: "fixture",
            inputs: InputReport {
                model_sha256: "a".repeat(64),
                wav_sha256: "b".repeat(64),
            },
            gpu_identity: GpuCaptureObservationIdentity {
                backend: "vulkan".to_owned(),
                provider: "fixture-provider".to_owned(),
                stable_device: "native:luid:0102030405060708".to_owned(),
                driver: "fixture-driver".to_owned(),
                device_class: "integrated_gpu".to_owned(),
                vendor: "intel".to_owned(),
                memory_total_bytes: 1024,
                pack_id: "fixture-pack".to_owned(),
                pack_version: "1".to_owned(),
                pack_sha256: "c".repeat(64),
                pack_security_epoch: 1,
                runtime_abi: 1,
            },
            cpu: worker(
                "private transcript text",
                VideoMemoryReport::NotApplicable,
                ProviderMemoryReport {
                    before: ProviderMemoryObservation::NotApplicable {
                        reason: ProviderMemoryNotApplicableReason::CpuProvider,
                    },
                    after: ProviderMemoryObservation::NotApplicable {
                        reason: ProviderMemoryNotApplicableReason::CpuProvider,
                    },
                },
                MemoryAvailabilityReport {
                    before: WorkerMemoryAvailability::NotApplicable {
                        reason: ProviderMemoryNotApplicableReason::CpuProvider,
                    },
                    after: WorkerMemoryAvailability::NotApplicable {
                        reason: ProviderMemoryNotApplicableReason::CpuProvider,
                    },
                },
            ),
            gpu: worker(
                "private transcript text",
                VideoMemoryReport::Available {
                    local: telemetry::SegmentSummary {
                        sampled_max_current_usage_bytes: 11,
                        sampled_max_current_reservation_bytes: 1,
                        sampled_min_budget_bytes: 100,
                        sampled_min_available_for_reservation_bytes: 80,
                    },
                    non_local: telemetry::SegmentSummary {
                        sampled_max_current_usage_bytes: 12,
                        sampled_max_current_reservation_bytes: 2,
                        sampled_min_budget_bytes: 200,
                        sampled_min_available_for_reservation_bytes: 160,
                    },
                },
                ProviderMemoryReport {
                    before: ProviderMemoryObservation::Available {
                        backend: "vulkan".to_owned(),
                        provider_id: "fixture-provider".to_owned(),
                        stable_device: "native:luid:0102030405060708".to_owned(),
                        memory_total_bytes: 1024,
                        provider_reported_memory_free_bytes: 0,
                        value_semantics:
                            crate::onnx_worker::ProviderMemoryValueSemantics::NativeBackendDefined,
                        admission_validity:
                            crate::onnx_worker::ProviderMemoryAdmissionValidity::Unestablished,
                    },
                    after: ProviderMemoryObservation::Unavailable {
                        reason:
                            crate::onnx_worker::ProviderMemoryUnavailableReason::ProviderQueryFailed,
                    },
                },
                MemoryAvailabilityReport {
                    before: WorkerMemoryAvailability::Observed {
                        backend: "vulkan".to_owned(),
                        provider_id: "fixture-provider".to_owned(),
                        stable_device: "native:luid:0102030405060708".to_owned(),
                        memory_total_bytes: 1024,
                        available_memory_bytes: 0,
                        source: crate::onnx_worker::WorkerMemoryAvailabilitySource::VulkanMemoryBudget {
                            heap_selection: crate::onnx_worker::VulkanMemoryHeapSelection::AllHeapsIntegrated,
                            heaps: vec![crate::onnx_worker::VulkanMemoryHeapObservation {
                                heap_index: 0,
                                size_bytes: 1024,
                                flags: 0,
                                budget_bytes: 1024,
                                usage_bytes: 2048,
                            }],
                        },
                    },
                    after: WorkerMemoryAvailability::Unavailable {
                        reason: crate::onnx_worker::WorkerMemoryUnavailableReason::ProviderQueryFailed,
                    },
                },
            ),
            transcript_parity: true,
            unavailable: UnavailableReport {
                inference_thread_count: UnavailableField {
                    status: "unavailable",
                    reason: "unsupported_by_pinned_runtime_api",
                },
                thermal_state: unavailable,
            },
        };
        let bytes = serde_json::to_vec(&report).unwrap();
        let text = String::from_utf8(bytes).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(parsed["schema_version"], 3);
        assert_eq!(
            parsed["gpu"]["memory_availability"]["before"]["source"]["method"],
            "vulkan_memory_budget"
        );
        assert_eq!(
            parsed["gpu"]["memory_availability"]["before"]["available_memory_bytes"],
            0
        );
        assert_eq!(
            parsed["unavailable"]["inference_thread_count"]["status"],
            "unavailable"
        );
        assert_eq!(
            parsed["unavailable"]["inference_thread_count"]["reason"],
            "unsupported_by_pinned_runtime_api"
        );
        assert!(text.contains("\"unsigned\":true"));
        assert!(text.contains("\"unqualified\":true"));
        assert!(text.contains("\"auto_eligible\":false"));
        assert!(text.contains("\"release_approved\":false"));
        assert!(text.contains("\"local\""));
        assert!(text.contains("\"non_local\""));
        assert!(text.contains("\"status\":\"not_applicable\""));
        assert!(!text.contains("provider_free_memory_bytes"));
        assert!(!text.contains("private transcript text"));
        assert!(!text.contains("known.gguf"));
        assert!(!text.contains("known.wav"));
        assert!(text.len() < MAX_REPORT_BYTES);
    }

    #[test]
    fn capture_observation_normalization_hashes_without_retaining_transcript() {
        assert_eq!(normalize_transcript("  Hello\nWORLD  "), "hello world");
        assert_eq!(normalize_transcript("hello world"), "hello world");
    }

    #[test]
    fn capture_observation_provider_memory_is_bound_and_zero_is_preserved() {
        let identity = GpuCaptureObservationIdentity {
            backend: "vulkan".to_owned(),
            provider: "fixture-provider".to_owned(),
            stable_device: "native:pci:0000:01:00.0".to_owned(),
            driver: "fixture-driver".to_owned(),
            device_class: "integrated_gpu".to_owned(),
            vendor: "intel".to_owned(),
            memory_total_bytes: 8192,
            pack_id: "fixture-pack".to_owned(),
            pack_version: "1".to_owned(),
            pack_sha256: "c".repeat(64),
            pack_security_epoch: 1,
            runtime_abi: 1,
        };
        let available = ProviderMemoryObservation::Available {
            backend: identity.backend.clone(),
            provider_id: identity.provider.clone(),
            stable_device: identity.stable_device.clone(),
            memory_total_bytes: identity.memory_total_bytes,
            provider_reported_memory_free_bytes: 0,
            value_semantics: crate::onnx_worker::ProviderMemoryValueSemantics::NativeBackendDefined,
            admission_validity: crate::onnx_worker::ProviderMemoryAdmissionValidity::Unestablished,
        };
        let report = ProviderMemoryReport {
            before: available.clone(),
            after: available,
        };
        validate_provider_memory_report(true, Some(&identity), &report).unwrap();

        let mut wrong_total = report;
        if let ProviderMemoryObservation::Available {
            memory_total_bytes, ..
        } = &mut wrong_total.before
        {
            *memory_total_bytes += 1;
        }
        assert!(validate_provider_memory_report(true, Some(&identity), &wrong_total).is_err());
    }

    #[test]
    fn capture_observation_memory_availability_is_bound_without_equating_vulkan_capacity() {
        let identity = GpuCaptureObservationIdentity {
            backend: "vulkan".to_owned(),
            provider: "fixture-provider".to_owned(),
            stable_device: "native:luid:0102030405060708".to_owned(),
            driver: "fixture-driver".to_owned(),
            device_class: "integrated_gpu".to_owned(),
            vendor: "intel".to_owned(),
            memory_total_bytes: 8192,
            pack_id: "fixture-pack".to_owned(),
            pack_version: "1".to_owned(),
            pack_sha256: "c".repeat(64),
            pack_security_epoch: 1,
            runtime_abi: 1,
        };
        let observed = WorkerMemoryAvailability::Observed {
            backend: identity.backend.clone(),
            provider_id: identity.provider.clone(),
            stable_device: identity.stable_device.clone(),
            memory_total_bytes: 12_288,
            available_memory_bytes: 0,
            source: crate::onnx_worker::WorkerMemoryAvailabilitySource::VulkanMemoryBudget {
                heap_selection: crate::onnx_worker::VulkanMemoryHeapSelection::AllHeapsIntegrated,
                heaps: vec![crate::onnx_worker::VulkanMemoryHeapObservation {
                    heap_index: 0,
                    size_bytes: 12_288,
                    flags: 0,
                    budget_bytes: 10_000,
                    usage_bytes: 11_000,
                }],
            },
        };
        validate_capture_worker_memory(&observed, AccelerationPreference::Gpu, Some(&identity))
            .unwrap();

        for flags in [0b10, 0b11] {
            let mut singleton_heap = observed.clone();
            if let WorkerMemoryAvailability::Observed {
                source:
                    crate::onnx_worker::WorkerMemoryAvailabilitySource::VulkanMemoryBudget {
                        heaps, ..
                    },
                ..
            } = &mut singleton_heap
            {
                heaps[0].flags = flags;
            }
            validate_capture_worker_memory(
                &singleton_heap,
                AccelerationPreference::Gpu,
                Some(&identity),
            )
            .unwrap();
        }

        let mut wrong_device = observed.clone();
        if let WorkerMemoryAvailability::Observed { stable_device, .. } = &mut wrong_device {
            *stable_device = "native:luid:ffffffffffffffff".to_owned();
        }
        assert!(
            validate_capture_worker_memory(
                &wrong_device,
                AccelerationPreference::Gpu,
                Some(&identity),
            )
            .is_err()
        );

        let mut wrong_scope = observed;
        if let WorkerMemoryAvailability::Observed {
            source:
                crate::onnx_worker::WorkerMemoryAvailabilitySource::VulkanMemoryBudget {
                    heap_selection,
                    ..
                },
            ..
        } = &mut wrong_scope
        {
            *heap_selection = crate::onnx_worker::VulkanMemoryHeapSelection::DeviceLocalHeaps;
        }
        assert!(
            validate_capture_worker_memory(
                &wrong_scope,
                AccelerationPreference::Gpu,
                Some(&identity),
            )
            .is_err()
        );
        assert!(
            validate_capture_worker_memory(
                &WorkerMemoryAvailability::Unavailable {
                    reason: crate::onnx_worker::WorkerMemoryUnavailableReason::ProviderQueryFailed,
                },
                AccelerationPreference::Gpu,
                Some(&identity),
            )
            .is_ok()
        );
    }

    #[test]
    fn capture_observation_power_transition_and_unknown_are_rejected() {
        assert!(require_stable_power(PowerSource::Ac, PowerSource::Ac).is_ok());
        assert!(require_stable_power(PowerSource::Battery, PowerSource::Battery).is_ok());
        assert!(require_stable_power(PowerSource::Ac, PowerSource::Battery).is_err());
        assert!(require_stable_power(PowerSource::Battery, PowerSource::Ac).is_err());
        assert!(require_stable_power(PowerSource::Unknown, PowerSource::Unknown).is_err());
        assert!(require_stable_power(PowerSource::Ac, PowerSource::Unknown).is_err());
    }

    #[test]
    fn capture_observation_publication_is_atomic_new_and_cleans_pending_name() {
        let mut nonce = [0_u8; 16];
        getrandom::fill(&mut nonce).unwrap();
        let root = std::env::temp_dir().join(format!("scribe-capture-publish-{}", hex(&nonce)));
        std::fs::create_dir(&root).unwrap();
        let output = root.join("observation.json");
        publish_atomic_new(&output, b"{\"unsigned\":true}\n").unwrap();
        assert_eq!(std::fs::read(&output).unwrap(), b"{\"unsigned\":true}\n");
        assert!(publish_atomic_new(&output, b"replacement\n").is_err());
        assert_eq!(std::fs::read(&output).unwrap(), b"{\"unsigned\":true}\n");
        let entries = std::fs::read_dir(&root)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>();
        assert_eq!(entries, vec![OsString::from("observation.json")]);
        std::fs::remove_file(output).unwrap();
        std::fs::remove_dir(root).unwrap();
    }

    #[test]
    fn capture_observation_publication_loses_destination_race_without_replacement() {
        let mut nonce = [0_u8; 16];
        getrandom::fill(&mut nonce).unwrap();
        let root = std::env::temp_dir().join(format!("scribe-capture-race-{}", hex(&nonce)));
        std::fs::create_dir(&root).unwrap();
        let output = root.join("observation.json");
        let result = publish_atomic_new_with(&output, b"new report\n", |pending, output| {
            assert!(pending.exists());
            std::fs::write(output, b"race winner\n")?;
            Ok(())
        });
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("atomically publish")
        );
        assert_eq!(std::fs::read(&output).unwrap(), b"race winner\n");
        let entries = std::fs::read_dir(&root)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>();
        assert_eq!(entries, vec![OsString::from("observation.json")]);
        std::fs::remove_file(output).unwrap();
        std::fs::remove_dir(root).unwrap();
    }
}
