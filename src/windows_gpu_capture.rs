//! Opt-in, unsigned Windows GPU capture observation.
//!
//! This collector deliberately stops before qualification: it runs one CPU
//! and one exact verified-GPU request, retains only validated Hello/Ready
//! bytes plus bounded native telemetry, and publishes an unqualified report.

mod telemetry;

use std::ffi::{OsStr, OsString};
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Component, Path, PathBuf};
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
    CaptureObservationWorker, GpuCaptureObservationIdentity, WorkerObservationLease,
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
    normalized_transcript_sha256: String,
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
    provider_free_memory_bytes: UnavailableField,
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
    let cpu_worker = CaptureObservationWorker::cpu();
    let cpu_lease = match cpu_worker.prepare_observation() {
        Ok(lease) => lease,
        Err(error) => {
            let _ = cpu_worker.shutdown();
            return Err(error.context("could not preflight the bundled CPU worker"));
        }
    };
    let gpu_worker = match CaptureObservationWorker::gpu(
        &options.gpu_pack_id,
        &options.gpu_backend,
        &options.gpu_device,
    ) {
        Ok(worker) => worker,
        Err(error) => {
            let cleanup = cpu_worker.shutdown();
            return combine_operation_cleanup(
                Err(error.context("could not preflight the verified GPU binding")),
                cleanup,
            );
        }
    };
    let gpu_lease = match gpu_worker.prepare_observation() {
        Ok(lease) => lease,
        Err(error) => {
            let gpu_cleanup = gpu_worker.shutdown();
            let cpu_cleanup = cpu_worker.shutdown();
            let cleanup = gpu_cleanup.and(cpu_cleanup);
            return combine_operation_cleanup(
                Err(error.context("could not preflight the selected GPU worker")),
                cleanup,
            );
        }
    };
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
        schema_version: 1,
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
            provider_free_memory_bytes: unavailable,
            inference_thread_count: unavailable,
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
    let execution = worker.transcribe(artifact, preference, audio);
    let elapsed = started.elapsed().as_millis();
    let telemetry = sampler.finish();
    let power_source_after = PowerSource::current();
    let execution = execution?;
    let telemetry = telemetry?;
    lease.require_current()?;
    require_stable_power(power_source_before, power_source_after)?;
    build_worker_report(
        &lease,
        execution,
        telemetry,
        power_source_before,
        power_source_after,
        elapsed,
        gpu,
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
    telemetry: TelemetrySummary,
    power_source_before: PowerSource,
    power_source_after: PowerSource,
    elapsed_ms: u128,
    gpu: bool,
) -> Result<ObservedWorker> {
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
        normalized_transcript_sha256: normalized_transcript_sha256.clone(),
    };
    Ok(ObservedWorker {
        report,
        normalized_transcript_sha256,
    })
}

fn normalize_transcript(value: &str) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
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
    let mut saw_command = false;
    let mut index = 0;
    while index < args.len() {
        let name = args[index]
            .to_str()
            .ok_or_else(|| anyhow!("capture observation arguments must be Unicode"))?;
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
            _ => bail!("unknown Windows GPU capture observation argument: {name}"),
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
    Ok(CommandOptions {
        model: PathBuf::from(required(model, "--model")?),
        model_sha256,
        wav: PathBuf::from(required(wav, "--wav")?),
        wav_sha256,
        gpu_pack_id: canonical_text(gpu_pack_id, "--gpu-pack-id")?,
        gpu_backend,
        gpu_device: canonical_text(gpu_device, "--gpu-device")?,
        output: PathBuf::from(required(output, "--output")?),
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
    fn capture_observation_report_is_unsigned_private_and_bounded() {
        let unavailable = UnavailableField {
            status: "unavailable",
            reason: "not_observed",
        };
        let worker = |transcript: &str, video_memory| WorkerReport {
            hello_frame_hex: hex(b"SCIF-hello"),
            ready_frame_hex: hex(b"SCIF-ready"),
            power_source_before: PowerSource::Ac,
            power_source_after: PowerSource::Ac,
            elapsed_ms: 10,
            sampled_max_private_usage_bytes: 20,
            telemetry_sample_count: 2,
            video_memory,
            normalized_transcript_sha256: format!("{:x}", Sha256::digest(transcript.as_bytes())),
        };
        let report = CaptureReport {
            schema_version: 1,
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
            cpu: worker("private transcript text", VideoMemoryReport::NotApplicable),
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
            ),
            transcript_parity: true,
            unavailable: UnavailableReport {
                provider_free_memory_bytes: unavailable,
                inference_thread_count: unavailable,
                thermal_state: unavailable,
            },
        };
        let bytes = serde_json::to_vec(&report).unwrap();
        let text = String::from_utf8(bytes).unwrap();
        assert!(text.contains("\"unsigned\":true"));
        assert!(text.contains("\"unqualified\":true"));
        assert!(text.contains("\"auto_eligible\":false"));
        assert!(text.contains("\"release_approved\":false"));
        assert!(text.contains("\"local\""));
        assert!(text.contains("\"non_local\""));
        assert!(text.contains("\"status\":\"not_applicable\""));
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
