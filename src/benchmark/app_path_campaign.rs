//! Opt-in benchmark of Scribe's ordinary application transcription path.
//!
//! This is deliberately a single explicit CPU or GPU lane. It produces
//! unsigned, unqualified acquisition data and cannot authorize Auto or a
//! release. Audio preparation and input hashing happen before the measured
//! `application_transcription_latency_ms` interval.

use std::ffi::OsString;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt as _;
#[cfg(windows)]
use std::os::windows::fs::OpenOptionsExt as _;

use anyhow::{Context, Result, anyhow, bail, ensure};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::config::AppConfig;
use crate::prepared_audio::PreparedAudio;
use crate::transcription::{
    AccelerationPreference, ComputeDevice, ModelId, RequestId, ResolvedAcceleration, SessionId,
    TranscriptionRequest, TranscriptionService,
};

const COMMAND_FLAG: &str = "--benchmark-campaign";
const COLD_RUNS: u8 = 5;
const WARM_RUNS: u8 = 20;
const MAX_WAV_BYTES: u64 = 256 * 1024 * 1024;
const MAX_SOURCE_SAMPLES: u64 = 32_000_000;
const MIN_SOURCE_RATE: u32 = 8_000;
const MAX_SOURCE_RATE: u32 = 192_000;
const MAX_SOURCE_CHANNELS: u16 = 8;
const MAX_MODEL_BYTES: u64 = 8 * 1024 * 1024 * 1024;
const MAX_REPORT_BYTES: usize = 1024 * 1024;
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Lane {
    Cpu,
    Gpu,
}

impl Lane {
    fn preference(self) -> AccelerationPreference {
        match self {
            Self::Cpu => AccelerationPreference::Cpu,
            Self::Gpu => AccelerationPreference::Gpu,
        }
    }
}

#[derive(Debug)]
struct CommandOptions {
    fixture: PathBuf,
    fixture_sha256: String,
    model_id: String,
    model_path: PathBuf,
    model_sha256: String,
    lane: Lane,
    output: PathBuf,
}

struct VerifiedInput {
    path: PathBuf,
    file: File,
    size_bytes: u64,
    sha256: String,
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
    recorded_at_unix_seconds: u64,
    metric: MetricDefinition,
    inputs: InputReport,
    lane: Lane,
    identity: RuntimeIdentity,
    runs: Vec<RunRecord>,
}

#[derive(Serialize)]
struct MetricDefinition {
    name: &'static str,
    starts_at: &'static str,
    ends_at: &'static str,
    excludes: [&'static str; 3],
}

#[derive(Serialize)]
struct InputReport {
    model_id: String,
    model_sha256: String,
    model_size_bytes: u64,
    fixture_sha256: String,
    fixture_size_bytes: u64,
    fixture_duration_ms: u128,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct RuntimeIdentity {
    backend: String,
    provider_id: String,
    stable_device_id: String,
    driver_version: Option<String>,
    pack: Option<PackIdentity>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct PackIdentity {
    pack_id: String,
    pack_version: String,
    pack_digest: String,
    security_epoch: u64,
    runtime_abi: u16,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Phase {
    Cold,
    Prime,
    Warm,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RunSpec {
    phase: Phase,
    index: u8,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct RunRecord {
    phase: Phase,
    index: u8,
    measured: bool,
    application_transcription_latency_ms: u64,
    warm_model_reused: bool,
    normalized_transcript_sha256: String,
}

#[derive(Clone)]
struct RunObservation {
    latency_ms: u64,
    warm_model_reused: bool,
    normalized_transcript_sha256: String,
    requested: AccelerationPreference,
    resolved_device: ComputeDevice,
    identity: RuntimeIdentity,
}

trait CampaignExecutor {
    type Service;

    fn create_service(&mut self) -> Result<Self::Service>;
    fn execute(&mut self, service: &Self::Service, spec: RunSpec) -> Result<RunObservation>;
    fn shutdown(&mut self, service: &Self::Service) -> bool;
}

#[derive(Debug)]
struct CampaignResult {
    identity: RuntimeIdentity,
    runs: Vec<RunRecord>,
}

struct ApplicationExecutor {
    config: AppConfig,
    audio: Arc<PreparedAudio>,
    model_id: ModelId,
    next_request_id: u64,
    _retained_model: File,
    _retained_fixture: File,
}

pub(super) fn run_local_command(args: &[OsString]) -> i32 {
    match run(args) {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("benchmark campaign failed: {error:#}");
            1
        }
    }
}

fn run(args: &[OsString]) -> Result<()> {
    let options = parse_command(args)?;
    let model_id = ModelId::new(options.model_id.clone());
    let artifact =
        crate::model_catalog::runtime_artifact_manifest_for_path(&model_id, &options.model_path)
            .ok_or_else(|| {
                anyhow!("campaign model ID and filename are not a known GGUF artifact")
            })?;
    if artifact.sha256 != options.model_sha256 {
        bail!("campaign model pin does not match the trusted catalog")
    }

    let mut model = verify_input(
        &options.model_path,
        &options.model_sha256,
        MAX_MODEL_BYTES,
        Some(artifact.size_bytes),
        "model",
    )?;
    let mut fixture = verify_input(
        &options.fixture,
        &options.fixture_sha256,
        MAX_WAV_BYTES,
        None,
        "fixture",
    )?;
    require_retained_digest(&mut model, "model")?;
    require_retained_digest(&mut fixture, "fixture")?;
    let audio = prepare_campaign_audio(&mut fixture.file, fixture.size_bytes)?;
    require_retained_digest(&mut fixture, "fixture")?;

    let mut config = AppConfig::default();
    config.general.selected_default_model = options.model_id.clone();
    config
        .general
        .model_paths
        .insert(options.model_id.clone(), model.path.clone());
    config.performance.acceleration_preference = options.lane.preference();

    let mut executor = ApplicationExecutor {
        config,
        audio: Arc::new(audio),
        model_id,
        next_request_id: 1,
        _retained_model: model.file,
        _retained_fixture: fixture.file,
    };
    let result = run_campaign(&mut executor, options.lane)?;
    let report = CampaignReport {
        schema_version: 1,
        kind: "scribe_application_transcription_campaign",
        unsigned: true,
        unqualified: true,
        auto_eligible: false,
        release_approved: false,
        collector_build_revision: env!("SCRIBE_BUILD_REVISION"),
        recorded_at_unix_seconds: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
        metric: MetricDefinition {
            name: "application_transcription_latency_ms",
            starts_at: "immediately before begin_transcription_task",
            ends_at: "successful TranscriptionOutcome",
            excludes: [
                "fixture hashing and preparation",
                "model hashing and validation",
                "report serialization and transcript digest hashing",
            ],
        },
        inputs: InputReport {
            model_id: options.model_id,
            model_sha256: model.sha256,
            model_size_bytes: model.size_bytes,
            fixture_sha256: fixture.sha256,
            fixture_size_bytes: fixture.size_bytes,
            fixture_duration_ms: executor.audio.duration_ms(),
        },
        lane: options.lane,
        identity: result.identity,
        runs: result.runs,
    };
    write_report_new(&options.output, &report)
}

fn prepare_campaign_audio<R: Read + Seek>(
    reader: &mut R,
    size_bytes: u64,
) -> Result<PreparedAudio> {
    validate_campaign_wav(reader, size_bytes)?;
    PreparedAudio::from_wav_reader(reader)
        .context("campaign fixture is not a supported non-empty WAV")
}

/// Bound both decoding and rate conversion before PreparedAudio reserves sample buffers.
/// The encoded-file cap alone cannot constrain a forged sample count or a very low rate.
fn validate_campaign_wav<R: Read + Seek>(reader: &mut R, size_bytes: u64) -> Result<()> {
    reader
        .rewind()
        .context("campaign fixture could not rewind")?;
    let wav = hound::WavReader::new(&mut *reader)
        .map_err(|_| anyhow!("campaign fixture has an invalid WAV header"))?;
    let spec = wav.spec();
    let source_samples = u64::from(wav.len());
    let data_offset = wav
        .into_inner()
        .stream_position()
        .context("campaign fixture data position is unavailable")?;
    ensure!(
        (1..=MAX_SOURCE_CHANNELS).contains(&spec.channels),
        "campaign fixture channel count is outside 1..=8"
    );
    ensure!(
        (MIN_SOURCE_RATE..=MAX_SOURCE_RATE).contains(&spec.sample_rate),
        "campaign fixture sample rate is outside 8000..=192000 Hz"
    );
    ensure!(
        match spec.sample_format {
            hound::SampleFormat::Int => matches!(spec.bits_per_sample, 8 | 16 | 24 | 32),
            hound::SampleFormat::Float => spec.bits_per_sample == 32,
        },
        "campaign fixture has an unsupported sample format"
    );
    ensure!(
        (1..=MAX_SOURCE_SAMPLES).contains(&source_samples),
        "campaign fixture exceeds the decoded sample bound or is empty"
    );
    let channels = u64::from(spec.channels);
    ensure!(
        source_samples.is_multiple_of(channels),
        "campaign fixture has incomplete channel frames"
    );
    let frames = source_samples / channels;
    let max_seconds = u64::from(crate::config::MAX_RECORDING_SECONDS);
    ensure!(
        frames <= u64::from(spec.sample_rate) * max_seconds,
        "campaign fixture exceeds the recording duration bound"
    );
    let prepared_samples = frames
        .checked_mul(u64::from(crate::prepared_audio::PREPARED_SAMPLE_RATE))
        .and_then(|scaled| scaled.checked_add(u64::from(spec.sample_rate / 2)))
        .map(|rounded| rounded / u64::from(spec.sample_rate))
        .ok_or_else(|| anyhow!("campaign fixture prepared length overflowed"))?;
    ensure!(
        prepared_samples <= u64::from(crate::prepared_audio::PREPARED_SAMPLE_RATE) * max_seconds,
        "campaign fixture exceeds the prepared sample bound"
    );
    // Extensible PCM can use padded sample containers. This is a necessary lower
    // bound, not a replacement for the decoder's exact format/data validation.
    let minimum_payload = source_samples
        .checked_mul(u64::from(spec.bits_per_sample.div_ceil(8)))
        .ok_or_else(|| anyhow!("campaign fixture payload length overflowed"))?;
    ensure!(
        size_bytes
            .checked_sub(data_offset)
            .is_some_and(|remaining| remaining >= minimum_payload),
        "campaign fixture declares more sample data than its retained file contains"
    );
    reader
        .rewind()
        .context("campaign fixture could not rewind")?;
    Ok(())
}

impl CampaignExecutor for ApplicationExecutor {
    type Service = TranscriptionService;

    fn create_service(&mut self) -> Result<Self::Service> {
        Ok(TranscriptionService::new(self.config.clone()))
    }

    fn execute(&mut self, service: &Self::Service, _spec: RunSpec) -> Result<RunObservation> {
        let request_id = self.next_request_id;
        self.next_request_id = self
            .next_request_id
            .checked_add(1)
            .ok_or_else(|| anyhow!("campaign request counter was exhausted"))?;
        let request = TranscriptionRequest::new(
            SessionId(1),
            RequestId(request_id),
            Arc::clone(&self.audio),
            self.model_id.clone(),
        );
        let started = Instant::now();
        let task = service
            .begin_transcription_task()
            .map_err(|_| anyhow!("application transcription dispatch failed"))?;
        let outcome = service
            .transcribe_task(request, task)
            .map_err(|_| anyhow!("application transcription request failed"))?;
        let elapsed = started.elapsed().as_millis();
        let latency_ms = u64::try_from(elapsed)
            .map_err(|_| anyhow!("application transcription latency exceeded report bounds"))?;
        let resolved = outcome
            .resolved_acceleration
            .as_ref()
            .ok_or_else(|| anyhow!("application transcription omitted acceleration identity"))?;
        let identity = runtime_identity(resolved)?;
        let normalized_transcript_sha256 = normalized_transcript_sha256(&outcome.transcript.text);
        Ok(RunObservation {
            latency_ms,
            warm_model_reused: outcome.warm_model_reused,
            normalized_transcript_sha256,
            requested: resolved.requested,
            resolved_device: resolved.resolved.clone(),
            identity,
        })
    }

    fn shutdown(&mut self, service: &Self::Service) -> bool {
        service.shutdown_runtime_and_wait(SHUTDOWN_TIMEOUT)
    }
}

fn run_campaign<E: CampaignExecutor>(executor: &mut E, lane: Lane) -> Result<CampaignResult> {
    let mut state = ValidationState::default();
    let mut runs = Vec::with_capacity(usize::from(COLD_RUNS + WARM_RUNS) + 1);
    for index in 1..=COLD_RUNS {
        let spec = RunSpec {
            phase: Phase::Cold,
            index,
        };
        let observation = run_on_fresh_service(executor, spec)?;
        runs.push(validate_observation(&mut state, lane, spec, observation)?);
    }

    let service = executor
        .create_service()
        .map_err(|_| anyhow!("campaign could not create its warm application service"))?;
    let warm_result = (|| {
        let prime = RunSpec {
            phase: Phase::Prime,
            index: 1,
        };
        let prime_observation = executor.execute(&service, prime)?;
        runs.push(validate_observation(
            &mut state,
            lane,
            prime,
            prime_observation,
        )?);
        for index in 1..=WARM_RUNS {
            let spec = RunSpec {
                phase: Phase::Warm,
                index,
            };
            let observation = executor.execute(&service, spec)?;
            runs.push(validate_observation(&mut state, lane, spec, observation)?);
        }
        Ok(())
    })();
    let shutdown = executor.shutdown(&service);
    combine_operation_and_shutdown(warm_result, shutdown)?;

    let identity = state
        .identity
        .ok_or_else(|| anyhow!("campaign completed without a runtime identity"))?;
    Ok(CampaignResult { identity, runs })
}

fn run_on_fresh_service<E: CampaignExecutor>(
    executor: &mut E,
    spec: RunSpec,
) -> Result<RunObservation> {
    let service = executor
        .create_service()
        .map_err(|_| anyhow!("campaign could not create a cold application service"))?;
    let operation = executor.execute(&service, spec);
    let shutdown = executor.shutdown(&service);
    combine_operation_and_shutdown(operation, shutdown)
}

fn combine_operation_and_shutdown<T>(operation: Result<T>, shutdown: bool) -> Result<T> {
    match (operation, shutdown) {
        (Ok(value), true) => Ok(value),
        (Err(error), true) => Err(error),
        (Ok(_), false) => bail!("campaign service cleanup exceeded its bounded deadline"),
        (Err(_), false) => {
            bail!("campaign request failed and service cleanup exceeded its bounded deadline")
        }
    }
}

#[derive(Default)]
struct ValidationState {
    identity: Option<RuntimeIdentity>,
    transcript_sha256: Option<String>,
}

fn validate_observation(
    state: &mut ValidationState,
    lane: Lane,
    spec: RunSpec,
    observation: RunObservation,
) -> Result<RunRecord> {
    if observation.requested != lane.preference() {
        bail!("application transcription did not preserve the explicit acceleration request")
    }
    match (lane, &observation.resolved_device) {
        (Lane::Cpu, ComputeDevice::Cpu) | (Lane::Gpu, ComputeDevice::Gpu { .. }) => {}
        (Lane::Gpu, ComputeDevice::Cpu) => {
            bail!("strict GPU campaign resolved to CPU; fallback is not permitted")
        }
        (Lane::Cpu, ComputeDevice::Gpu { .. }) => {
            bail!("strict CPU campaign resolved to a GPU")
        }
    }
    validate_identity(lane, &observation.identity)?;
    if state
        .identity
        .as_ref()
        .is_some_and(|expected| expected != &observation.identity)
    {
        bail!("application transcription runtime identity drifted within the lane")
    }
    if state.identity.is_none() {
        state.identity = Some(observation.identity);
    }
    if state
        .transcript_sha256
        .as_ref()
        .is_some_and(|expected| expected != &observation.normalized_transcript_sha256)
    {
        bail!("normalized transcript digest drifted within the lane")
    }
    if state.transcript_sha256.is_none() {
        state.transcript_sha256 = Some(observation.normalized_transcript_sha256.clone());
    }
    let expected_warm = spec.phase == Phase::Warm;
    if observation.warm_model_reused != expected_warm {
        bail!("application transcription warm-state diagnostics did not match the fixed schedule")
    }
    Ok(RunRecord {
        phase: spec.phase,
        index: spec.index,
        measured: spec.phase != Phase::Prime,
        application_transcription_latency_ms: observation.latency_ms,
        warm_model_reused: observation.warm_model_reused,
        normalized_transcript_sha256: observation.normalized_transcript_sha256,
    })
}

fn validate_identity(lane: Lane, identity: &RuntimeIdentity) -> Result<()> {
    if identity.backend.is_empty()
        || identity.provider_id.is_empty()
        || identity.stable_device_id.is_empty()
    {
        bail!("application transcription returned an incomplete stable runtime identity")
    }
    match lane {
        Lane::Cpu if identity.backend != "CPU" || identity.pack.is_some() => {
            bail!("CPU campaign returned an invalid CPU runtime identity")
        }
        Lane::Gpu if identity.backend == "CPU" || identity.pack.is_none() => {
            bail!("GPU campaign returned an incomplete verified-pack runtime identity")
        }
        _ => {}
    }
    if let Some(pack) = &identity.pack
        && (pack.pack_id.is_empty()
            || pack.pack_version.is_empty()
            || !is_sha256(&pack.pack_digest)
            || pack.runtime_abi == 0)
    {
        bail!("GPU campaign returned an invalid verified-pack runtime identity")
    }
    Ok(())
}

fn runtime_identity(resolved: &ResolvedAcceleration) -> Result<RuntimeIdentity> {
    let selection = resolved
        .selection
        .as_ref()
        .ok_or_else(|| anyhow!("application transcription omitted stable backend selection"))?;
    let target = &selection.target;
    Ok(RuntimeIdentity {
        backend: target.kind_label().to_owned(),
        provider_id: target.provider_id.as_str().to_owned(),
        stable_device_id: target.device_id.as_str().to_owned(),
        driver_version: target.driver_version.clone(),
        pack: target.pack.as_ref().map(|pack| PackIdentity {
            pack_id: pack.pack_id.clone(),
            pack_version: pack.pack_version.clone(),
            pack_digest: pack.pack_digest.clone(),
            security_epoch: pack.security_epoch,
            runtime_abi: pack.runtime_abi,
        }),
    })
}

fn normalized_transcript_sha256(value: &str) -> String {
    let normalized = value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    format!("{:x}", Sha256::digest(normalized.as_bytes()))
}

fn parse_command(args: &[OsString]) -> Result<CommandOptions> {
    let mut fixture = None;
    let mut fixture_sha256 = None;
    let mut model_id = None;
    let mut model_path = None;
    let mut model_sha256 = None;
    let mut lane = None;
    let mut output = None;
    let mut saw_command = false;
    let mut index = 0;
    while index < args.len() {
        let flag = args[index]
            .to_str()
            .ok_or_else(|| anyhow!("benchmark campaign arguments must be Unicode"))?;
        match flag {
            COMMAND_FLAG => {
                if saw_command {
                    bail!("--benchmark-campaign may be provided only once")
                }
                saw_command = true;
                fixture = Some(next_value(args, &mut index, COMMAND_FLAG)?);
            }
            "--fixture-sha256" => set_once(
                &mut fixture_sha256,
                canonical_sha256(&next_value(args, &mut index, flag)?)?,
                flag,
            )?,
            "--model" => set_once(
                &mut model_id,
                canonical_model_id(&next_value(args, &mut index, flag)?)?,
                flag,
            )?,
            "--model-path" => set_once(
                &mut model_path,
                PathBuf::from(next_value(args, &mut index, flag)?),
                flag,
            )?,
            "--model-sha256" => set_once(
                &mut model_sha256,
                canonical_sha256(&next_value(args, &mut index, flag)?)?,
                flag,
            )?,
            "--acceleration" => {
                let value = next_value(args, &mut index, flag)?;
                let parsed = match value.as_str() {
                    "cpu" => Lane::Cpu,
                    "gpu" => Lane::Gpu,
                    _ => bail!("--acceleration must be exactly cpu or gpu"),
                };
                set_once(&mut lane, parsed, flag)?;
            }
            "--output" => set_once(
                &mut output,
                PathBuf::from(next_value(args, &mut index, flag)?),
                flag,
            )?,
            _ => bail!("unknown benchmark campaign argument"),
        }
        index += 1;
    }
    if !saw_command {
        bail!("--benchmark-campaign is required")
    }
    Ok(CommandOptions {
        fixture: PathBuf::from(required(fixture, "--benchmark-campaign")?),
        fixture_sha256: required(fixture_sha256, "--fixture-sha256")?,
        model_id: required(model_id, "--model")?,
        model_path: required(model_path, "--model-path")?,
        model_sha256: required(model_sha256, "--model-sha256")?,
        lane: required(lane, "--acceleration")?,
        output: required(output, "--output")?,
    })
}

fn next_value(args: &[OsString], index: &mut usize, flag: &str) -> Result<String> {
    *index += 1;
    let value = args
        .get(*index)
        .ok_or_else(|| anyhow!("{flag} requires a value"))?
        .to_str()
        .ok_or_else(|| anyhow!("benchmark campaign arguments must be Unicode"))?;
    if value.starts_with("--") || value.is_empty() {
        bail!("{flag} requires a value")
    }
    Ok(value.to_owned())
}

fn set_once<T>(slot: &mut Option<T>, value: T, flag: &str) -> Result<()> {
    if slot.replace(value).is_some() {
        bail!("{flag} may be provided only once")
    }
    Ok(())
}

fn required<T>(value: Option<T>, flag: &str) -> Result<T> {
    value.ok_or_else(|| anyhow!("{flag} is required"))
}

fn canonical_sha256(value: &str) -> Result<String> {
    if !is_sha256(value) {
        bail!("SHA-256 pins must be exactly 64 lowercase hexadecimal characters")
    }
    Ok(value.to_owned())
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn canonical_model_id(value: &str) -> Result<String> {
    if value.len() > 128
        || value.is_empty()
        || !value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'-')
        })
    {
        bail!("--model must be a canonical built-in model ID")
    }
    Ok(value.to_owned())
}

fn verify_input(
    path: &Path,
    expected_sha256: &str,
    max_bytes: u64,
    exact_size: Option<u64>,
    label: &str,
) -> Result<VerifiedInput> {
    if !path.is_absolute() {
        bail!("{label} input must use an absolute path")
    }
    let metadata =
        std::fs::symlink_metadata(path).with_context(|| format!("{label} input is unavailable"))?;
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.len() == 0
        || metadata.len() > max_bytes
        || exact_size.is_some_and(|expected| metadata.len() != expected)
    {
        bail!("{label} input is not the expected bounded regular file")
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    options.share_mode(windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ);
    let mut file = options
        .open(path)
        .with_context(|| format!("could not retain the {label} input"))?;
    let sha256 = hash_file(&mut file, metadata.len())
        .with_context(|| format!("could not hash the {label} input"))?;
    if sha256 != expected_sha256 {
        bail!("{label} input does not match its pinned SHA-256")
    }
    Ok(VerifiedInput {
        path: path.to_owned(),
        file,
        size_bytes: metadata.len(),
        sha256,
    })
}

fn require_retained_digest(input: &mut VerifiedInput, label: &str) -> Result<()> {
    if hash_file(&mut input.file, input.size_bytes)? != input.sha256 {
        bail!("retained {label} input changed before campaign execution")
    }
    Ok(())
}

fn hash_file(file: &mut File, expected_size: u64) -> Result<String> {
    file.seek(SeekFrom::Start(0))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    let mut total = 0_u64;
    // Retaining a read handle does not prohibit writes on every platform.
    // Admit exactly the previously bounded length, never an unbounded stream.
    let mut bounded = file.take(expected_size + 1);
    loop {
        let count = bounded.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        total += count as u64;
        hasher.update(&buffer[..count]);
    }
    if total != expected_size {
        bail!("input size changed while hashing")
    }
    file.seek(SeekFrom::Start(0))?;
    Ok(format!("{:x}", hasher.finalize()))
}

fn write_report_new(path: &Path, report: &CampaignReport) -> Result<()> {
    let mut bytes = serde_json::to_vec_pretty(report)
        .context("could not serialize benchmark campaign metadata")?;
    bytes.push(b'\n');
    if bytes.len() > MAX_REPORT_BYTES {
        bail!("benchmark campaign metadata exceeds its size limit")
    }
    write_report_bytes_new(path, &bytes)
}

fn write_report_bytes_new(path: &Path, bytes: &[u8]) -> Result<()> {
    write_report_bytes_new_with(path, bytes, |file, bytes| {
        file.write_all(bytes)?;
        file.sync_all()
    })
}

fn write_report_bytes_new_with(
    path: &Path,
    bytes: &[u8],
    write: impl FnOnce(&mut File, &[u8]) -> std::io::Result<()>,
) -> Result<()> {
    if !path.is_absolute() {
        bail!("benchmark campaign output must use an absolute path")
    }
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    #[cfg(windows)]
    options.share_mode(0);
    let mut file = options
        .open(path)
        .context("refusing to overwrite benchmark campaign output or create it safely")?;
    let result = write(&mut file, bytes).context("could not finish benchmark campaign output");
    if result.is_err() {
        drop(file);
        let _ = std::fs::remove_file(path);
    }
    result
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::fs;
    use std::rc::Rc;

    use super::*;

    const HASH: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    fn pcm_header(rate: u32, channels: u16, frames: u32) -> Vec<u8> {
        let payload = frames.checked_mul(u32::from(channels)).unwrap() * 2;
        let mut header = Vec::new();
        header.extend_from_slice(b"RIFF");
        header.extend_from_slice(&(payload + 36).to_le_bytes());
        header.extend_from_slice(b"WAVEfmt ");
        header.extend_from_slice(&16_u32.to_le_bytes());
        header.extend_from_slice(&1_u16.to_le_bytes());
        header.extend_from_slice(&channels.to_le_bytes());
        header.extend_from_slice(&rate.to_le_bytes());
        header.extend_from_slice(&(rate * u32::from(channels) * 2).to_le_bytes());
        header.extend_from_slice(&(channels * 2).to_le_bytes());
        header.extend_from_slice(&16_u16.to_le_bytes());
        header.extend_from_slice(b"data");
        header.extend_from_slice(&payload.to_le_bytes());
        header
    }

    #[test]
    fn wav_preflight_rejects_resampling_amplification_before_reading_samples() {
        for rate in [0, 1, MIN_SOURCE_RATE - 1, MAX_SOURCE_RATE + 1] {
            let mut input = std::io::Cursor::new(pcm_header(rate, 1, 100));
            let error = prepare_campaign_audio(&mut input, 244).unwrap_err();
            assert!(error.to_string().contains("sample rate"));
            assert_eq!(input.position(), 44);
        }
    }

    #[test]
    fn wav_preflight_bounds_declared_decode_and_duration_without_allocating_payloads() {
        let max_seconds = crate::config::MAX_RECORDING_SECONDS;
        let cases = [
            (192_000, 8, 4_000_001, "decoded sample bound"),
            (16_000, 1, 16_000 * max_seconds + 1, "duration bound"),
            (8_000, 1, 8_000 * max_seconds + 1, "duration bound"),
            (16_000, 1, 0, "empty"),
            (16_000, 9, 1, "channel count"),
        ];
        for (rate, channels, frames, reason) in cases {
            let mut input = std::io::Cursor::new(pcm_header(rate, channels, frames));
            let error = validate_campaign_wav(&mut input, MAX_WAV_BYTES).unwrap_err();
            assert!(error.to_string().contains(reason), "{error}");
            assert_eq!(input.position(), 44);
        }
    }

    #[test]
    fn wav_preflight_accepts_exact_header_bounds_and_rewinds_without_decoding() {
        // Header-only tests explicitly supply a hypothetical retained length;
        // they exercise admission arithmetic, not a claim that truncated WAVs decode.
        let max_seconds = crate::config::MAX_RECORDING_SECONDS;
        for (rate, channels, frames) in [
            (8_000, 1, 8_000 * max_seconds),
            (16_000, 1, 16_000 * max_seconds),
            (44_100, 1, 44_100 * max_seconds),
            (48_000, 1, 48_000 * max_seconds),
            (192_000, 1, 32_000_000),
            (192_000, 8, 4_000_000),
        ] {
            let header = pcm_header(rate, channels, frames);
            let size = 44 + u64::from(frames) * u64::from(channels) * 2;
            let mut input = std::io::Cursor::new(header);
            validate_campaign_wav(&mut input, size).unwrap();
            assert_eq!(input.position(), 0);
        }
    }

    #[test]
    fn wav_preflight_rejects_forged_payload_lengths_before_decoding() {
        for size in [0, 43, 44, 243] {
            let mut input = std::io::Cursor::new(pcm_header(16_000, 1, 100));
            let error = prepare_campaign_audio(&mut input, size).unwrap_err();
            assert!(error.to_string().contains("retained file contains"));
            assert_eq!(input.position(), 44);
        }
        let mut huge = pcm_header(16_000, 1, 100);
        huge[40..44].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(prepare_campaign_audio(&mut std::io::Cursor::new(huge), 244).is_err());
    }

    #[test]
    fn wav_preflight_preserves_normal_rates_and_channel_preparation() {
        for rate in [8_000, 16_000, 44_100, 48_000, 192_000] {
            for channels in 1..=MAX_SOURCE_CHANNELS {
                let mut bytes = pcm_header(rate, channels, 48);
                bytes.resize(44 + usize::from(channels) * 48 * 2, 0);
                let size = bytes.len() as u64;
                let prepared =
                    prepare_campaign_audio(&mut std::io::Cursor::new(bytes), size).unwrap();
                assert_eq!(prepared.source_frames, 48);
                assert_eq!(prepared.source_sample_rate, rate);
                assert_eq!(prepared.source_channels, channels);
                assert_eq!(
                    prepared.samples.len(),
                    ((48 * 16_000 + rate / 2) / rate) as usize
                );
            }
        }
    }

    #[test]
    fn wav_preflight_rejects_malformed_headers_and_decoder_still_checks_data() {
        for bytes in [b"not a wav".to_vec(), pcm_header(16_000, 0, 1)] {
            let size = bytes.len() as u64;
            assert!(prepare_campaign_audio(&mut std::io::Cursor::new(bytes), size).is_err());
        }
        // Even if a caller lies about retained length, the decoder rejects missing data.
        let mut input = std::io::Cursor::new(pcm_header(16_000, 1, 100));
        let error = prepare_campaign_audio(&mut input, 244).unwrap_err();
        assert!(error.to_string().contains("supported non-empty WAV"));
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum ServiceEvent {
        Created(usize),
        CreateFailed(usize),
        Executed(usize, RunSpec),
        Shutdown(usize),
        Dropped(usize),
    }

    struct FakeService {
        id: usize,
        events: Rc<RefCell<Vec<ServiceEvent>>>,
    }

    impl Drop for FakeService {
        fn drop(&mut self) {
            self.events
                .borrow_mut()
                .push(ServiceEvent::Dropped(self.id));
        }
    }

    #[derive(Debug)]
    struct SyntheticCancellation;

    impl std::fmt::Display for SyntheticCancellation {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("synthetic cancellation")
        }
    }

    impl std::error::Error for SyntheticCancellation {}

    struct FakeExecutor {
        lane: Lane,
        identity: RuntimeIdentity,
        transcript_sha256: String,
        creation_attempts: usize,
        created: usize,
        shutdowns: usize,
        calls: Vec<RunSpec>,
        events: Rc<RefCell<Vec<ServiceEvent>>>,
        fail_at_creation: Option<usize>,
        fail_at_call: Option<usize>,
        cancel_at_call: Option<usize>,
        shutdown_results: VecDeque<bool>,
        override_observations: VecDeque<RunObservation>,
    }

    impl FakeExecutor {
        fn new(lane: Lane) -> Self {
            Self {
                lane,
                identity: identity(lane),
                transcript_sha256: normalized_transcript_sha256("Hello, WORLD"),
                creation_attempts: 0,
                created: 0,
                shutdowns: 0,
                calls: Vec::new(),
                events: Rc::new(RefCell::new(Vec::new())),
                fail_at_creation: None,
                fail_at_call: None,
                cancel_at_call: None,
                shutdown_results: VecDeque::new(),
                override_observations: VecDeque::new(),
            }
        }

        fn observation(&self, spec: RunSpec) -> RunObservation {
            RunObservation {
                latency_ms: u64::from(spec.index),
                warm_model_reused: spec.phase == Phase::Warm,
                normalized_transcript_sha256: self.transcript_sha256.clone(),
                requested: self.lane.preference(),
                resolved_device: match self.lane {
                    Lane::Cpu => ComputeDevice::Cpu,
                    Lane::Gpu => ComputeDevice::Gpu {
                        name: "not retained in the report".to_owned(),
                    },
                },
                identity: self.identity.clone(),
            }
        }
    }

    impl CampaignExecutor for FakeExecutor {
        type Service = FakeService;

        fn create_service(&mut self) -> Result<Self::Service> {
            self.creation_attempts += 1;
            let id = self.creation_attempts;
            if self.fail_at_creation == Some(id) {
                self.events
                    .borrow_mut()
                    .push(ServiceEvent::CreateFailed(id));
                bail!("synthetic factory failure")
            }
            self.created += 1;
            self.events.borrow_mut().push(ServiceEvent::Created(id));
            Ok(FakeService {
                id,
                events: Rc::clone(&self.events),
            })
        }

        fn execute(&mut self, service: &Self::Service, spec: RunSpec) -> Result<RunObservation> {
            self.calls.push(spec);
            self.events
                .borrow_mut()
                .push(ServiceEvent::Executed(service.id, spec));
            if self.cancel_at_call == Some(self.calls.len()) {
                return Err(SyntheticCancellation.into());
            }
            if self.fail_at_call == Some(self.calls.len()) {
                bail!("synthetic native failure")
            }
            if let Some(observation) = self.override_observations.pop_front() {
                return Ok(observation);
            }
            Ok(self.observation(spec))
        }

        fn shutdown(&mut self, service: &Self::Service) -> bool {
            self.shutdowns += 1;
            self.events
                .borrow_mut()
                .push(ServiceEvent::Shutdown(service.id));
            self.shutdown_results.pop_front().unwrap_or(true)
        }
    }

    fn identity(lane: Lane) -> RuntimeIdentity {
        match lane {
            Lane::Cpu => RuntimeIdentity {
                backend: "CPU".to_owned(),
                provider_id: "transcribe-cpp:cpu".to_owned(),
                stable_device_id: "cpu:system".to_owned(),
                driver_version: None,
                pack: None,
            },
            Lane::Gpu => RuntimeIdentity {
                backend: "CUDA".to_owned(),
                provider_id: "cuda:fixture".to_owned(),
                stable_device_id: "native:0000:01:00.0".to_owned(),
                driver_version: Some("fixture-driver".to_owned()),
                pack: Some(PackIdentity {
                    pack_id: "cuda-fixture".to_owned(),
                    pack_version: "fixture-version".to_owned(),
                    pack_digest: HASH.to_owned(),
                    security_epoch: 1,
                    runtime_abi: 1,
                }),
            },
        }
    }

    fn valid_args() -> Vec<OsString> {
        vec![
            COMMAND_FLAG.into(),
            "C:\\fixture.wav".into(),
            "--fixture-sha256".into(),
            HASH.into(),
            "--model".into(),
            "whisper_cpp_base_en".into(),
            "--model-path".into(),
            "C:\\whisper-base.en-Q8_0.gguf".into(),
            "--model-sha256".into(),
            HASH.into(),
            "--acceleration".into(),
            "cpu".into(),
            "--output".into(),
            "C:\\report.json".into(),
        ]
    }

    fn temp_path(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "scribe-app-campaign-{label}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    #[test]
    fn fixed_schedule_runs_exactly_five_cold_one_prime_and_twenty_warm() {
        let mut executor = FakeExecutor::new(Lane::Cpu);
        let result = run_campaign(&mut executor, Lane::Cpu).unwrap();

        assert_eq!(executor.created, 6);
        assert_eq!(executor.shutdowns, 6);
        assert_eq!(executor.calls.len(), 26);
        assert_eq!(result.runs.len(), 26);
        assert_eq!(
            result
                .runs
                .iter()
                .filter(|run| run.phase == Phase::Cold)
                .count(),
            5
        );
        assert_eq!(
            result
                .runs
                .iter()
                .filter(|run| run.phase == Phase::Prime)
                .count(),
            1
        );
        assert_eq!(
            result
                .runs
                .iter()
                .filter(|run| run.phase == Phase::Warm)
                .count(),
            20
        );
        assert_eq!(result.runs.iter().filter(|run| run.measured).count(), 25);
    }

    #[test]
    fn cold_services_are_released_before_next_creation_and_warm_requests_keep_one_service() {
        let mut executor = FakeExecutor::new(Lane::Cpu);

        run_campaign(&mut executor, Lane::Cpu).unwrap();

        let mut expected = Vec::new();
        for index in 1..=COLD_RUNS {
            let id = usize::from(index);
            expected.extend([
                ServiceEvent::Created(id),
                ServiceEvent::Executed(
                    id,
                    RunSpec {
                        phase: Phase::Cold,
                        index,
                    },
                ),
                ServiceEvent::Shutdown(id),
                ServiceEvent::Dropped(id),
            ]);
        }
        let warm_id = usize::from(COLD_RUNS) + 1;
        expected.extend([
            ServiceEvent::Created(warm_id),
            ServiceEvent::Executed(
                warm_id,
                RunSpec {
                    phase: Phase::Prime,
                    index: 1,
                },
            ),
        ]);
        for index in 1..=WARM_RUNS {
            expected.push(ServiceEvent::Executed(
                warm_id,
                RunSpec {
                    phase: Phase::Warm,
                    index,
                },
            ));
        }
        expected.extend([
            ServiceEvent::Shutdown(warm_id),
            ServiceEvent::Dropped(warm_id),
        ]);
        assert_eq!(*executor.events.borrow(), expected);
    }

    #[test]
    fn factory_failure_aborts_before_execution_without_retrying_creation() {
        for failed_creation in [1, 3, usize::from(COLD_RUNS) + 1] {
            let mut executor = FakeExecutor::new(Lane::Cpu);
            executor.fail_at_creation = Some(failed_creation);

            let error = run_campaign(&mut executor, Lane::Cpu).unwrap_err();

            assert!(error.to_string().contains("create"));
            assert_eq!(executor.creation_attempts, failed_creation);
            assert_eq!(executor.created, failed_creation - 1);
            assert_eq!(executor.calls.len(), failed_creation - 1);
            assert_eq!(executor.shutdowns, failed_creation - 1);
            assert_eq!(
                executor.events.borrow().last(),
                Some(&ServiceEvent::CreateFailed(failed_creation))
            );
        }
    }

    #[test]
    fn schedule_requires_cold_and_prime_not_reused_and_every_warm_reused() {
        for bad_spec in [
            RunSpec {
                phase: Phase::Cold,
                index: 1,
            },
            RunSpec {
                phase: Phase::Prime,
                index: 1,
            },
            RunSpec {
                phase: Phase::Warm,
                index: 1,
            },
        ] {
            let mut executor = FakeExecutor::new(Lane::Cpu);
            let mut bad = executor.observation(bad_spec);
            bad.warm_model_reused = !bad.warm_model_reused;
            let call_index = match bad_spec.phase {
                Phase::Cold => 0,
                Phase::Prime => usize::from(COLD_RUNS),
                Phase::Warm => usize::from(COLD_RUNS) + 1,
            };
            let filler = executor.observation(RunSpec {
                phase: Phase::Cold,
                index: 1,
            });
            executor.override_observations = (0..call_index)
                .map(|_| filler.clone())
                .chain(std::iter::once(bad))
                .collect();
            let error = run_campaign(&mut executor, Lane::Cpu).unwrap_err();
            assert!(error.to_string().contains("warm-state"));
        }
    }

    #[test]
    fn strict_gpu_rejects_cpu_fallback_and_auto_requests() {
        let spec = RunSpec {
            phase: Phase::Cold,
            index: 1,
        };
        let mut state = ValidationState::default();
        let mut cpu_fallback = FakeExecutor::new(Lane::Gpu).observation(spec);
        cpu_fallback.resolved_device = ComputeDevice::Cpu;
        assert!(
            validate_observation(&mut state, Lane::Gpu, spec, cpu_fallback)
                .unwrap_err()
                .to_string()
                .contains("fallback")
        );

        let mut auto = FakeExecutor::new(Lane::Gpu).observation(spec);
        auto.requested = AccelerationPreference::Auto;
        assert!(
            validate_observation(&mut ValidationState::default(), Lane::Gpu, spec, auto)
                .unwrap_err()
                .to_string()
                .contains("explicit")
        );
    }

    #[test]
    fn stable_identity_and_transcript_are_required_for_the_whole_lane() {
        let mut executor = FakeExecutor::new(Lane::Gpu);
        let first = executor.observation(RunSpec {
            phase: Phase::Cold,
            index: 1,
        });
        let mut drift = executor.observation(RunSpec {
            phase: Phase::Cold,
            index: 2,
        });
        drift.identity.stable_device_id = "native:other".to_owned();
        executor.override_observations = VecDeque::from([first, drift]);
        assert!(
            run_campaign(&mut executor, Lane::Gpu)
                .unwrap_err()
                .to_string()
                .contains("identity drifted")
        );

        let mut executor = FakeExecutor::new(Lane::Cpu);
        let first = executor.observation(RunSpec {
            phase: Phase::Cold,
            index: 1,
        });
        let mut drift = executor.observation(RunSpec {
            phase: Phase::Cold,
            index: 2,
        });
        drift.normalized_transcript_sha256 = normalized_transcript_sha256("different");
        executor.override_observations = VecDeque::from([first, drift]);
        assert!(
            run_campaign(&mut executor, Lane::Cpu)
                .unwrap_err()
                .to_string()
                .contains("transcript digest drifted")
        );
    }

    #[test]
    fn normalized_transcript_hash_is_case_and_whitespace_insensitive_but_keeps_punctuation() {
        assert_eq!(
            normalized_transcript_sha256("  Hello,\nWORLD  "),
            normalized_transcript_sha256("hello, world")
        );
        assert_ne!(
            normalized_transcript_sha256("hello, world"),
            normalized_transcript_sha256("hello world")
        );
    }

    #[test]
    fn request_failure_aborts_without_replay_or_reprime_and_still_shuts_down() {
        for failed_call in [3, usize::from(COLD_RUNS) + 1, 12] {
            let mut executor = FakeExecutor::new(Lane::Cpu);
            executor.fail_at_call = Some(failed_call);
            assert!(run_campaign(&mut executor, Lane::Cpu).is_err());
            assert_eq!(executor.calls.len(), failed_call);
            let expected_services = if failed_call <= usize::from(COLD_RUNS) {
                failed_call
            } else {
                usize::from(COLD_RUNS) + 1
            };
            assert_eq!(executor.created, expected_services);
            assert_eq!(executor.shutdowns, expected_services);
            assert_eq!(
                executor
                    .calls
                    .iter()
                    .filter(|spec| spec.phase == Phase::Prime)
                    .count(),
                usize::from(failed_call > usize::from(COLD_RUNS))
            );
        }
    }

    #[test]
    fn prime_and_warm_cancellation_abort_without_replay_and_release_the_active_service() {
        for cancelled_call in [usize::from(COLD_RUNS) + 1, usize::from(COLD_RUNS) + 2, 12] {
            let mut executor = FakeExecutor::new(Lane::Cpu);
            executor.cancel_at_call = Some(cancelled_call);

            let error = run_campaign(&mut executor, Lane::Cpu).unwrap_err();

            assert!(error.is::<SyntheticCancellation>());
            assert_eq!(executor.calls.len(), cancelled_call);
            let warm_id = usize::from(COLD_RUNS) + 1;
            assert_eq!(executor.creation_attempts, warm_id);
            assert_eq!(executor.created, warm_id);
            assert_eq!(executor.shutdowns, warm_id);
            assert_eq!(
                executor
                    .calls
                    .iter()
                    .filter(|spec| spec.phase == Phase::Prime)
                    .count(),
                1
            );
            assert!(executor.events.borrow().ends_with(&[
                ServiceEvent::Shutdown(warm_id),
                ServiceEvent::Dropped(warm_id),
            ]));
        }
    }

    #[test]
    fn successful_and_failed_shutdown_are_both_observed() {
        let mut success = FakeExecutor::new(Lane::Cpu);
        assert!(run_campaign(&mut success, Lane::Cpu).is_ok());
        assert_eq!(success.shutdowns, 6);

        let mut failure = FakeExecutor::new(Lane::Cpu);
        failure.shutdown_results.push_back(false);
        let error = run_campaign(&mut failure, Lane::Cpu).unwrap_err();
        assert!(error.to_string().contains("cleanup"));
        assert_eq!(failure.calls.len(), 1);
        assert_eq!(failure.shutdowns, 1);
    }

    #[test]
    fn warm_shutdown_failure_rejects_an_otherwise_successful_campaign() {
        let mut executor = FakeExecutor::new(Lane::Cpu);
        executor.shutdown_results = (0..COLD_RUNS)
            .map(|_| true)
            .chain(std::iter::once(false))
            .collect();

        let error = run_campaign(&mut executor, Lane::Cpu).unwrap_err();

        assert!(error.to_string().contains("cleanup"));
        assert_eq!(executor.calls.len(), usize::from(COLD_RUNS + WARM_RUNS) + 1);
        let warm_id = usize::from(COLD_RUNS) + 1;
        assert_eq!(executor.creation_attempts, warm_id);
        assert_eq!(executor.shutdowns, warm_id);
        assert!(executor.events.borrow().ends_with(&[
            ServiceEvent::Shutdown(warm_id),
            ServiceEvent::Dropped(warm_id),
        ]));
    }

    #[test]
    fn request_and_shutdown_failure_abort_cold_prime_and_warm_without_retrying() {
        for failed_call in [1, usize::from(COLD_RUNS) + 1, 12] {
            let mut executor = FakeExecutor::new(Lane::Cpu);
            executor.fail_at_call = Some(failed_call);
            let services = failed_call.min(usize::from(COLD_RUNS) + 1);
            executor.shutdown_results = (0..services - 1)
                .map(|_| true)
                .chain(std::iter::once(false))
                .collect();

            let error = run_campaign(&mut executor, Lane::Cpu).unwrap_err();

            assert!(
                error
                    .to_string()
                    .contains("request failed and service cleanup")
            );
            assert_eq!(executor.calls.len(), failed_call);
            assert_eq!(executor.creation_attempts, services);
            assert_eq!(executor.shutdowns, services);
            assert!(executor.events.borrow().ends_with(&[
                ServiceEvent::Shutdown(services),
                ServiceEvent::Dropped(services),
            ]));
        }
    }

    #[test]
    fn parser_accepts_only_complete_explicit_cpu_or_gpu_commands() {
        let cpu = parse_command(&valid_args()).unwrap();
        assert_eq!(cpu.lane, Lane::Cpu);
        assert_eq!(cpu.model_id, "whisper_cpp_base_en");

        let mut gpu_args = valid_args();
        let acceleration = gpu_args
            .iter()
            .position(|arg| arg == "cpu")
            .expect("cpu value");
        gpu_args[acceleration] = "gpu".into();
        assert_eq!(parse_command(&gpu_args).unwrap().lane, Lane::Gpu);

        gpu_args[acceleration] = "auto".into();
        assert!(parse_command(&gpu_args).is_err());
    }

    #[test]
    fn parser_rejects_duplicates_extras_and_missing_pins() {
        let mut duplicate = valid_args();
        duplicate.extend(["--output".into(), "C:\\other.json".into()]);
        assert!(parse_command(&duplicate).is_err());

        let mut extra = valid_args();
        extra.push("unexpected".into());
        assert!(parse_command(&extra).is_err());

        for required_flag in ["--fixture-sha256", "--model-sha256"] {
            let mut missing = valid_args();
            let index = missing.iter().position(|arg| arg == required_flag).unwrap();
            missing.drain(index..=index + 1);
            assert!(parse_command(&missing).is_err());
        }
    }

    #[test]
    fn parser_rejects_non_unicode_arguments() {
        let mut args = valid_args();
        args.push(non_unicode_os_string());
        assert!(parse_command(&args).is_err());
    }

    #[test]
    fn input_verification_accepts_the_exact_size_and_rewinds_after_hashing() {
        let path = temp_path("valid-input");
        fs::write(&path, b"abc").unwrap();
        let expected = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

        let mut input = verify_input(&path, expected, 3, Some(3), "fixture").unwrap();

        assert_eq!(input.size_bytes, 3);
        assert_eq!(input.sha256, expected);
        assert_eq!(input.file.stream_position().unwrap(), 0);
        let mut contents = Vec::new();
        input.file.read_to_end(&mut contents).unwrap();
        assert_eq!(contents, b"abc");
        require_retained_digest(&mut input, "fixture").unwrap();
        assert_eq!(input.file.stream_position().unwrap(), 0);
        drop(input);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn input_verification_rejects_a_hash_mismatch_and_size_boundaries() {
        let path = temp_path("invalid-input");
        fs::write(&path, b"abc").unwrap();
        let expected = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

        let error = verify_input(&path, HASH, 3, Some(3), "fixture")
            .err()
            .expect("a mismatched pin must fail");
        assert!(error.to_string().contains("pinned SHA-256"));
        for (max_bytes, exact_size) in [(2, None), (3, Some(2)), (4, Some(4))] {
            let error = verify_input(&path, expected, max_bytes, exact_size, "fixture")
                .err()
                .expect("an unexpected size must fail");
            assert!(error.to_string().contains("bounded regular file"));
        }
        fs::write(&path, b"").unwrap();
        let error = verify_input(&path, expected, 3, None, "fixture")
            .err()
            .expect("an empty file must fail");
        assert!(error.to_string().contains("bounded regular file"));
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn input_verification_rejects_relative_missing_and_directory_paths() {
        let relative = verify_input(Path::new("fixture.wav"), HASH, 3, None, "fixture")
            .err()
            .expect("a relative path must fail");
        assert!(relative.to_string().contains("absolute path"));

        let path = temp_path("missing-or-directory-input");
        let missing = verify_input(&path, HASH, 3, None, "fixture")
            .err()
            .expect("a missing input must fail");
        assert!(missing.to_string().contains("unavailable"));

        fs::create_dir(&path).unwrap();
        let directory = verify_input(&path, HASH, 3, None, "fixture")
            .err()
            .expect("a directory must fail");
        assert!(directory.to_string().contains("bounded regular file"));
        fs::remove_dir(path).unwrap();
    }

    #[test]
    fn hashing_rejects_a_file_shorter_or_longer_than_the_recorded_size() {
        let path = temp_path("changed-input-size");
        fs::write(&path, b"abc").unwrap();
        let mut file = File::open(&path).unwrap();

        for expected_size in [2, 4] {
            assert!(hash_file(&mut file, expected_size).is_err());
        }

        drop(file);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn retained_digest_rejects_a_reopened_file_with_changed_same_size_content() {
        let path = temp_path("changed-input-digest");
        fs::write(&path, b"abc").unwrap();
        let expected = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        let original = verify_input(&path, expected, 3, Some(3), "fixture").unwrap();
        let pin = original.sha256.clone();
        let size_bytes = original.size_bytes;
        // Release Windows' retained-handle write protection before replacing the contents.
        drop(original);
        fs::write(&path, b"abd").unwrap();
        let mut reopened = VerifiedInput {
            path: path.clone(),
            file: File::open(&path).unwrap(),
            size_bytes,
            sha256: pin,
        };

        let error = require_retained_digest(&mut reopened, "fixture").unwrap_err();

        assert!(
            error
                .to_string()
                .contains("changed before campaign execution")
        );
        drop(reopened);
        fs::remove_file(path).unwrap();
    }

    #[cfg(unix)]
    fn non_unicode_os_string() -> OsString {
        use std::os::unix::ffi::OsStringExt;
        OsString::from_vec(vec![0xff])
    }

    #[cfg(windows)]
    fn non_unicode_os_string() -> OsString {
        use std::os::windows::ffi::OsStringExt;
        OsString::from_wide(&[0xd800])
    }

    #[test]
    fn output_is_create_new_and_contains_metadata_without_paths_or_raw_content() {
        let path = temp_path("new-output.json");
        fs::write(&path, b"existing").unwrap();
        assert!(write_report_bytes_new(&path, b"replacement").is_err());
        assert_eq!(fs::read(&path).unwrap(), b"existing");
        fs::remove_file(&path).unwrap();

        let report = CampaignReport {
            schema_version: 1,
            kind: "scribe_application_transcription_campaign",
            unsigned: true,
            unqualified: true,
            auto_eligible: false,
            release_approved: false,
            collector_build_revision: "fixture",
            recorded_at_unix_seconds: 1,
            metric: MetricDefinition {
                name: "application_transcription_latency_ms",
                starts_at: "dispatch",
                ends_at: "outcome",
                excludes: ["hashing", "preparation", "serialization"],
            },
            inputs: InputReport {
                model_id: "whisper_cpp_base_en".to_owned(),
                model_sha256: HASH.to_owned(),
                model_size_bytes: 10,
                fixture_sha256: HASH.to_owned(),
                fixture_size_bytes: 20,
                fixture_duration_ms: 30,
            },
            lane: Lane::Cpu,
            identity: identity(Lane::Cpu),
            runs: vec![RunRecord {
                phase: Phase::Cold,
                index: 1,
                measured: true,
                application_transcription_latency_ms: 2,
                warm_model_reused: false,
                normalized_transcript_sha256: HASH.to_owned(),
            }],
        };
        let json = serde_json::to_string(&report).unwrap();
        for forbidden in [
            "C:\\\\private\\\\fixture.wav",
            "raw transcript",
            "audio samples",
            "stderr payload",
            "stdout payload",
            "display label",
        ] {
            assert!(!json.contains(forbidden));
        }
        assert!(json.contains("\"unsigned\":true"));
        assert!(json.contains("\"auto_eligible\":false"));
        assert!(json.contains("application_transcription_latency_ms"));
    }

    #[test]
    fn partial_output_is_removed_when_writing_fails_after_creation() {
        let path = temp_path("failed-output.json");
        let error = write_report_bytes_new_with(&path, b"metadata", |file, _| {
            file.write_all(b"partial")?;
            Err(std::io::Error::other("synthetic write failure"))
        })
        .unwrap_err();
        assert!(error.to_string().contains("finish"));
        assert!(!path.exists());
    }

    #[cfg(unix)]
    #[test]
    fn new_output_uses_owner_only_permissions_on_unix() {
        use std::os::unix::fs::PermissionsExt;

        let path = temp_path("private-output.json");
        write_report_bytes_new(&path, b"{}\n").unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        fs::remove_file(&path).unwrap();
    }
}
