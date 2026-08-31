//! Private, process-isolated CPU-only sherpa-onnx execution substrate.
//!
//! This module deliberately has no catalog or UI entry point. A future typed
//! installer supplies a verified [`OnnxModelSpec`]; the router remains the only
//! component allowed to construct a worker client.

#[cfg(any(feature = "inference-worker", test))]
use std::cell::RefCell;
use std::collections::hash_map::Entry;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::time::{Duration, Instant};

use crate::backend_policy::{
    BackendFailureCategory, BackendFallback, BackendKind, BackendSelection, BackendSelectionReason,
    BackendSkipReason, BackendTarget, DeviceClass, GpuVendor, PowerPolicyDecision, PowerSource,
    SkippedBackend,
};
use crate::config;
use crate::gpu_auto_qualification::{
    AUTO_QUALIFICATION_POLICY_VERSION, AutoQualificationPolicy, QualificationDecision,
};
use crate::gpu_worker_pack::health::{
    FailureObservation, HealthCache, HealthDecision, HealthKey, HealthWitnesses, SystemClock,
};
use crate::gpu_worker_pack::manifest::{
    PackBackend, PackVerifier, ProductionTrustRoot, VerifiedPackLease,
};
use crate::gpu_worker_pack::{ResolverHelloBindingBridge, VerifiedPackLaunchBinding};
use crate::model_catalog::ArtifactFormat;
use crate::prepared_audio::{PREPARED_SAMPLE_RATE, PreparedAudio};
use crate::runtime_artifact::{
    OnnxFileRole, OnnxModelFamily, OnnxModelSpec, RuntimeArtifact, RuntimeModel,
};
use crate::runtime_contract::{
    NativeRuntimeDiagnostics, RuntimeError, RuntimeExecution, RuntimeLoadExecution,
};
use crate::runtime_router::RuntimeRouter;
use crate::silero_vad_native::{SileroVadModel, VadThreshold, WINDOW_SAMPLES};
use crate::transcription::{
    AccelerationPreference, ComputeDevice, ModelId, ResolvedAcceleration, RuntimeCapabilities,
    Transcript, TranscriptSegment, TranscriptionOptions,
};
use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub(crate) use crate::worker_identity::{
    DESKTOP_BUILD_ID, INFERENCE_WORKER_BUILD_ID, PROTOCOL_VERSION, WORKER_ABI_VERSION,
};

#[cfg(unix)]
use std::os::unix::process::CommandExt;
#[cfg(windows)]
use std::os::windows::io::AsRawHandle;
#[cfg(windows)]
use std::os::windows::process::CommandExt;

pub(crate) const PROTOCOL_MAGIC: [u8; 4] = *b"SCIF";
pub(crate) const INFERENCE_WORKER_FLAG: &str = "--scribe-inference-worker";
pub(crate) const VAD_WORKER_FLAG: &str = "--scribe-vad-worker";
const VAD_WORKER_BUILD_ID: &str = concat!(
    "scribe-vad-worker@",
    env!("CARGO_PKG_VERSION"),
    "#",
    env!("SCRIBE_BUILD_REVISION")
);
const PARENT_LIVENESS_ENV: &str = "SCRIBE_PRIVATE_PARENT_LIVENESS";
#[cfg(all(target_os = "linux", target_arch = "x86_64", target_env = "gnu"))]
const LINUX_EXECUTABLE_FD_ENV: &str = "SCRIBE_PRIVATE_EXECUTABLE_FD";
#[cfg(all(target_os = "linux", target_arch = "x86_64", target_env = "gnu"))]
const LINUX_EXECUTABLE_FD: i32 = 3;
const PACK_ID_ENV: &str = "SCRIBE_PRIVATE_PACK_ID";
const PACK_VERSION_ENV: &str = "SCRIBE_PRIVATE_PACK_VERSION";
const PACK_DIGEST_ENV: &str = "SCRIBE_PRIVATE_PACK_DIGEST";
const PACK_SECURITY_EPOCH_ENV: &str = "SCRIBE_PRIVATE_PACK_SECURITY_EPOCH";
const PACK_RUNTIME_ABI_ENV: &str = "SCRIBE_PRIVATE_PACK_RUNTIME_ABI";
const PACK_BACKEND_ENV: &str = "SCRIBE_PRIVATE_PACK_BACKEND";
pub(crate) const PACK_PROVIDER_ENV: &str = "SCRIBE_PRIVATE_PACK_PROVIDER";
pub(crate) const PACK_DEVICE_ID_ENV: &str = "SCRIBE_PRIVATE_PACK_DEVICE_ID";
pub(crate) const PACK_DRIVER_ID_ENV: &str = "SCRIBE_PRIVATE_PACK_DRIVER_ID";
const PARENT_CONTROL_CANCEL: u8 = b'C';
const HEADER_LEN: usize = 26;
const MAX_CONTROL_BYTES: usize = 256 * 1024;
const MAX_PCM_FRAME_BYTES: usize = 16 * 1024 * 1024;
const MAX_PCM_FRAME_SAMPLES: usize = MAX_PCM_FRAME_BYTES / size_of::<f32>();
const MAX_TRANSCRIPT_TEXT_BYTES: usize = 96 * 1024;
const MAX_TRANSCRIPT_SEGMENTS: usize = 1024;
const MAX_SEGMENT_TEXT_BYTES: usize = 8 * 1024;
const MAX_LANGUAGE_BYTES: usize = 256;
const MAX_SUPPORTED_LANGUAGES: usize = 512;
const MAX_DIAGNOSTIC_BYTES: usize = 16 * 1024;
const MAX_ARCHITECTURE_BYTES: usize = 512;
const MAX_WORKER_ERROR_BYTES: usize = 16 * 1024;
const MAX_BACKEND_SELECTION_TARGETS: usize = 128;
const MAX_BACKEND_IDENTITY_BYTES: usize = 1024;
const MAX_PACK_DEVICES: usize = 16;
const MAX_GPU_PROVIDER_PROBES: usize = 8;
const GPU_PROVIDER_DISCOVERY_BUDGET: Duration = Duration::from_secs(10);

#[derive(Clone, Copy)]
struct SupervisorDeadlines {
    hello: Duration,
    load: Duration,
    health: Duration,
    data: Duration,
    control: Duration,
    cancel: Duration,
}

impl Default for SupervisorDeadlines {
    fn default() -> Self {
        Self {
            hello: Duration::from_secs(10),
            load: Duration::from_secs(15 * 60),
            health: Duration::from_secs(10),
            data: Duration::from_secs(60 * 60),
            control: Duration::from_secs(30),
            cancel: Duration::from_millis(250),
        }
    }
}

#[derive(Clone, Copy)]
struct VadDeadlines {
    acquisition: Duration,
    operation: Duration,
}

impl Default for VadDeadlines {
    fn default() -> Self {
        Self {
            // This single budget covers process launch/Hello, model load, both
            // health checks, and session start before the microphone plays.
            acquisition: Duration::from_secs(2),
            // One stall consumes at most one eighth of the two-second capture
            // ring, so capture fails closed well before the callback exhausts it.
            operation: Duration::from_millis(250),
        }
    }
}

#[derive(Clone, Copy)]
struct MonotonicDeadline {
    expires_at: Instant,
    budget: Duration,
    label: &'static str,
}

impl MonotonicDeadline {
    fn after(budget: Duration) -> Result<Self> {
        Self::after_for(budget, "Silero VAD acquisition")
    }

    fn after_for(budget: Duration, label: &'static str) -> Result<Self> {
        if budget.is_zero() {
            bail!("{label} budget must be positive");
        }
        let started = Instant::now();
        let expires_at = started
            .checked_add(budget)
            .ok_or_else(|| anyhow!("{label} deadline overflowed"))?;
        Ok(Self {
            expires_at,
            budget,
            label,
        })
    }

    fn remaining(self) -> Result<Duration> {
        let remaining = self.expires_at.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            bail!(
                "{} deadline exceeded after {} ms",
                self.label,
                self.budget.as_millis()
            );
        }
        Ok(remaining)
    }
}

impl OnnxModelSpec {
    pub(crate) fn validate(&self) -> Result<()> {
        self.validated().map(|_| ())
    }

    fn validated(&self) -> Result<ValidatedOnnxModel> {
        if self.id.trim().is_empty() || !(1..=64).contains(&self.num_threads) {
            bail!("ONNX model id must be non-empty and thread count must be within [1, 64]");
        }
        let root = std::fs::canonicalize(&self.root)
            .map_err(|error| anyhow!("ONNX model root is unavailable: {error}"))?;
        reject_link_components(&self.root)?;
        if !root.is_dir() {
            bail!(
                "ONNX model root is not a directory: {}",
                self.root.display()
            );
        }
        let actual_roles = self.files.keys().copied().collect::<BTreeSet<_>>();
        let expected_layouts = expected_role_layouts(self.family);
        if !expected_layouts
            .iter()
            .any(|roles| roles.iter().copied().collect::<BTreeSet<_>>() == actual_roles)
        {
            bail!(
                "ONNX model {} has an invalid {:?} artifact role set: expected {}, got {}",
                self.id,
                self.family,
                format_role_layouts(expected_layouts),
                format_roles(&actual_roles)
            );
        }
        let mut canonical_files = BTreeMap::new();
        for (role, relative) in &self.files {
            if relative.is_absolute()
                || relative
                    .components()
                    .any(|part| !matches!(part, Component::Normal(_)))
            {
                bail!(
                    "ONNX model {} has unsafe relative file path {}",
                    self.id,
                    relative.display()
                );
            }
            let path = self.root.join(relative);
            reject_link_components(&path)?;
            let canonical = std::fs::canonicalize(&path)
                .map_err(|error| anyhow!("ONNX model {} file is unavailable: {error}", self.id))?;
            if !canonical_file_is_within_root(&root, &canonical) {
                bail!(
                    "ONNX model {} {role:?} file is outside its canonical root or is not a file: {}",
                    self.id,
                    path.display()
                );
            }
            canonical_files.insert(*role, canonical);
        }
        Ok(ValidatedOnnxModel {
            id: self.id.clone(),
            root,
            family: self.family,
            files: canonical_files,
            num_threads: self.num_threads,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct ValidatedOnnxModel {
    pub(crate) id: String,
    pub(crate) root: PathBuf,
    pub(crate) family: OnnxModelFamily,
    pub(crate) files: BTreeMap<OnnxFileRole, PathBuf>,
    pub(crate) num_threads: u16,
}

impl ValidatedOnnxModel {
    #[allow(
        dead_code,
        reason = "the validated-path accessor remains available to the isolated worker fixture boundary"
    )]
    pub(crate) fn path(&self, role: OnnxFileRole) -> Result<String> {
        let path = self
            .files
            .get(&role)
            .ok_or_else(|| anyhow!("ONNX model {} missing {role:?}", self.id))?;
        reject_link_components(path)?;
        let current = std::fs::canonicalize(path).map_err(|error| {
            anyhow!(
                "validated ONNX model {} {role:?} artifact is unavailable: {error}",
                self.id
            )
        })?;
        if current != *path || !canonical_file_is_within_root(&self.root, &current) {
            bail!(
                "validated ONNX model {} {role:?} artifact changed after admission",
                self.id
            );
        }
        Ok(path.to_string_lossy().into_owned())
    }
}

const MOONSHINE_MERGED_ROLES: &[OnnxFileRole] = &[
    OnnxFileRole::Encoder,
    OnnxFileRole::Tokens,
    OnnxFileRole::MergedDecoder,
];
const MOONSHINE_V1_ROLES: &[OnnxFileRole] = &[
    OnnxFileRole::Encoder,
    OnnxFileRole::Tokens,
    OnnxFileRole::Preprocessor,
    OnnxFileRole::UncachedDecoder,
    OnnxFileRole::CachedDecoder,
];
const NEMO_CTC_ROLES: &[OnnxFileRole] = &[OnnxFileRole::Model, OnnxFileRole::Tokens];
const CANARY_ROLES: &[OnnxFileRole] = &[
    OnnxFileRole::Encoder,
    OnnxFileRole::Decoder,
    OnnxFileRole::Tokens,
];
const TRANSDUCER_ROLES: &[OnnxFileRole] = &[
    OnnxFileRole::Encoder,
    OnnxFileRole::Decoder,
    OnnxFileRole::Joiner,
    OnnxFileRole::Tokens,
];

fn expected_role_layouts(family: OnnxModelFamily) -> &'static [&'static [OnnxFileRole]] {
    match family {
        OnnxModelFamily::Moonshine => &[MOONSHINE_MERGED_ROLES, MOONSHINE_V1_ROLES],
        OnnxModelFamily::NemoCtc => &[NEMO_CTC_ROLES],
        OnnxModelFamily::Canary => &[CANARY_ROLES],
        OnnxModelFamily::OfflineTransducer | OnnxModelFamily::OnlineTransducer => {
            &[TRANSDUCER_ROLES]
        }
    }
}

fn format_role_layouts(layouts: &[&[OnnxFileRole]]) -> String {
    layouts
        .iter()
        .map(|roles| format_roles(&roles.iter().copied().collect()))
        .collect::<Vec<_>>()
        .join(" or ")
}

fn format_roles(roles: &BTreeSet<OnnxFileRole>) -> String {
    roles
        .iter()
        .map(|role| format!("{role:?}"))
        .collect::<Vec<_>>()
        .join("+")
}

pub(crate) fn resolve_cpu_only_acceleration(
    requested: AccelerationPreference,
) -> Result<ResolvedAcceleration> {
    match requested {
        AccelerationPreference::Auto => Ok(ResolvedAcceleration {
            requested,
            resolved: ComputeDevice::Cpu,
            diagnostic: Some(
                "sherpa-onnx runs in an isolated CPU-only worker; Auto selected CPU".to_owned(),
            ),
            selection: None,
        }),
        AccelerationPreference::Cpu => Ok(ResolvedAcceleration {
            requested,
            resolved: ComputeDevice::Cpu,
            diagnostic: None,
            selection: None,
        }),
        AccelerationPreference::Gpu => bail!(
            "GPU acceleration is unavailable for sherpa-onnx because the isolated runtime is CPU-only; select Auto or CPU only"
        ),
    }
}

fn canonical_file_is_within_root(root: &Path, file: &Path) -> bool {
    file.starts_with(root) && file.is_file()
}

fn reject_link_components(path: &Path) -> Result<()> {
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component);
        if matches!(component, Component::Prefix(_) | Component::RootDir) {
            continue;
        }
        match std::fs::symlink_metadata(&current) {
            Ok(metadata) if metadata_is_link_or_reparse(&metadata) => {
                bail!(
                    "ONNX path contains a symbolic link or reparse point: {}",
                    current.display()
                );
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

#[cfg(windows)]
fn metadata_is_link_or_reparse(metadata: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;

    metadata.file_type().is_symlink()
        || metadata.file_attributes()
            & windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT
            != 0
}

#[cfg(not(windows))]
fn metadata_is_link_or_reparse(metadata: &std::fs::Metadata) -> bool {
    metadata.file_type().is_symlink()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
enum FrameKind {
    Control = 1,
    Pcm = 2,
}

impl TryFrom<u8> for FrameKind {
    type Error = anyhow::Error;
    fn try_from(value: u8) -> Result<Self> {
        match value {
            1 => Ok(Self::Control),
            2 => Ok(Self::Pcm),
            _ => bail!("unknown worker frame kind {value}"),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Frame {
    kind: FrameKind,
    session_id: u64,
    request_id: u64,
    body: Vec<u8>,
}

fn write_frame(writer: &mut impl Write, frame: &Frame) -> Result<()> {
    let limit = match frame.kind {
        FrameKind::Control => MAX_CONTROL_BYTES,
        FrameKind::Pcm => MAX_PCM_FRAME_BYTES,
    };
    if frame.body.len() > limit {
        bail!("worker frame exceeds {limit}-byte limit");
    }
    writer.write_all(&PROTOCOL_MAGIC)?;
    writer.write_all(&[PROTOCOL_VERSION, frame.kind as u8])?;
    writer.write_all(&(frame.body.len() as u32).to_le_bytes())?;
    writer.write_all(&frame.session_id.to_le_bytes())?;
    writer.write_all(&frame.request_id.to_le_bytes())?;
    writer.write_all(&frame.body)?;
    writer.flush()?;
    Ok(())
}

fn read_frame(reader: &mut impl Read) -> Result<Frame> {
    let mut header = [0_u8; HEADER_LEN];
    reader.read_exact(&mut header)?;
    if header[..4] != PROTOCOL_MAGIC {
        bail!("invalid process worker frame magic");
    }
    if header[4] != PROTOCOL_VERSION {
        bail!("unsupported process worker protocol version {}", header[4]);
    }
    let kind = FrameKind::try_from(header[5])?;
    let body_len = u32::from_le_bytes(header[6..10].try_into().unwrap()) as usize;
    let limit = match kind {
        FrameKind::Control => MAX_CONTROL_BYTES,
        FrameKind::Pcm => MAX_PCM_FRAME_BYTES,
    };
    if body_len > limit {
        bail!("process worker frame body exceeds {limit}-byte limit");
    }
    let mut body = vec![0; body_len];
    reader.read_exact(&mut body)?;
    Ok(Frame {
        kind,
        session_id: u64::from_le_bytes(header[10..18].try_into().unwrap()),
        request_id: u64::from_le_bytes(header[18..26].try_into().unwrap()),
        body,
    })
}

fn encode_pcm(samples: &[f32]) -> Result<Vec<u8>> {
    validate_pcm_samples(samples)?;
    Ok(samples
        .iter()
        .flat_map(|sample| sample.to_le_bytes())
        .collect())
}

fn decode_pcm(body: &[u8]) -> Result<Vec<f32>> {
    if body.is_empty() {
        bail!("ONNX PCM must contain at least one sample");
    }
    if !body.len().is_multiple_of(size_of::<f32>()) {
        bail!("ONNX PCM byte length must be a multiple of four");
    }
    if body.len() > MAX_PCM_FRAME_BYTES {
        bail!("ONNX PCM frame exceeds the {MAX_PCM_FRAME_BYTES}-byte limit");
    }
    let samples = body
        .chunks_exact(size_of::<f32>())
        .map(|bytes| f32::from_le_bytes(bytes.try_into().expect("f32 chunk width is exact")))
        .collect::<Vec<_>>();
    validate_pcm_samples(&samples)?;
    Ok(samples)
}

pub(crate) fn validate_pcm_samples(samples: &[f32]) -> Result<()> {
    if samples.is_empty() {
        bail!("ONNX PCM must contain at least one sample");
    }
    if samples.len() > MAX_PCM_FRAME_SAMPLES {
        bail!("ONNX PCM frame exceeds the {MAX_PCM_FRAME_BYTES}-byte limit");
    }
    if let Some((index, sample)) = samples
        .iter()
        .copied()
        .enumerate()
        .find(|(_, sample)| !sample.is_finite() || !(-1.0..=1.0).contains(sample))
    {
        bail!("ONNX PCM sample {index} is non-finite or outside [-1, 1]: {sample}");
    }
    Ok(())
}

fn prepared_sample_count_for_seconds(seconds: u32) -> Result<usize> {
    usize::try_from(seconds)
        .ok()
        .and_then(|seconds| seconds.checked_mul(PREPARED_SAMPLE_RATE as usize))
        .ok_or_else(|| anyhow!("prepared-audio duration exceeds addressable sample count"))
}

fn max_cumulative_audio_samples() -> Result<usize> {
    config::MAX_RECORDING_SECONDS
        .checked_add(config::RECORDING_CAPTURE_SAFETY_ALLOWANCE_SECONDS)
        .ok_or_else(|| anyhow!("recording duration plus capture allowance overflowed"))
        .and_then(prepared_sample_count_for_seconds)
}

fn validate_cumulative_sample_count(sample_count: usize) -> Result<()> {
    if sample_count == 0 {
        bail!("runtime audio must contain at least one sample");
    }
    let limit = max_cumulative_audio_samples()?;
    if sample_count > limit {
        bail!("runtime audio exceeds the {limit}-sample cumulative limit");
    }
    Ok(())
}

fn checked_cumulative_sample_count(current: usize, incoming: usize) -> Result<usize> {
    let next = current
        .checked_add(incoming)
        .ok_or_else(|| anyhow!("runtime audio sample count overflowed"))?;
    validate_cumulative_sample_count(next)?;
    Ok(next)
}

fn validate_cumulative_pcm_samples(samples: &[f32]) -> Result<()> {
    validate_cumulative_sample_count(samples.len())?;
    if let Some((index, sample)) = samples
        .iter()
        .copied()
        .enumerate()
        .find(|(_, sample)| !sample.is_finite() || !(-1.0..=1.0).contains(sample))
    {
        bail!("runtime PCM sample {index} is non-finite or outside [-1, 1]: {sample}");
    }
    Ok(())
}

fn reserve_batch_samples(declared_samples: usize) -> Result<Vec<f32>> {
    let mut samples = Vec::new();
    samples.try_reserve_exact(declared_samples).map_err(|_| {
        anyhow::Error::new(RuntimeError::Engine(
            "runtime batch could not reserve its declared bounded audio buffer".to_owned(),
        ))
    })?;
    Ok(samples)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case", deny_unknown_fields)]
enum Control {
    Hello {
        challenge: String,
        expected: WorkerExpectation,
    },
    LoadRuntime {
        artifact: WireRuntimeArtifact,
        preference: AccelerationPreference,
    },
    BeginBatch {
        artifact: WireRuntimeArtifact,
        preference: AccelerationPreference,
        options: TranscriptionOptions,
        source_sample_rate: u32,
        source_channels: u16,
        source_frames: usize,
        declared_samples: usize,
    },
    EndBatch,
    StartStream,
    AudioChunk,
    EndStream,
    LoadVad {
        num_threads: u16,
    },
    StartVad {
        threshold: f32,
    },
    VadWindow,
    ResetVad,
    EndVad,
    Cancel {
        target_session_id: u64,
        target_request_id: u64,
    },
    Unload,
    Health,
    Shutdown,
    Ready {
        capability: WorkerCapability,
    },
    RuntimeLoaded {
        execution: WireRuntimeLoadExecution,
    },
    RuntimeTranscript {
        execution: WireRuntimeExecution,
    },
    RuntimeFailed {
        error: WireRuntimeError,
    },
    Text {
        text: String,
        final_result: bool,
    },
    VadDecision {
        probability: f32,
        speech: bool,
    },
    Ok,
    Error {
        message: String,
    },
}

impl Control {
    fn is_parent_command(&self) -> bool {
        matches!(
            self,
            Self::Hello { .. }
                | Self::LoadRuntime { .. }
                | Self::BeginBatch { .. }
                | Self::EndBatch
                | Self::StartStream
                | Self::AudioChunk
                | Self::EndStream
                | Self::LoadVad { .. }
                | Self::StartVad { .. }
                | Self::VadWindow
                | Self::ResetVad
                | Self::EndVad
                | Self::Cancel { .. }
                | Self::Unload
                | Self::Health
                | Self::Shutdown
        )
    }

    fn is_worker_response(&self) -> bool {
        matches!(
            self,
            Self::Ready { .. }
                | Self::RuntimeLoaded { .. }
                | Self::RuntimeTranscript { .. }
                | Self::RuntimeFailed { .. }
                | Self::Text { .. }
                | Self::VadDecision { .. }
                | Self::Ok
                | Self::Error { .. }
        )
    }
}

fn control_frame(session_id: u64, request_id: u64, control: &Control) -> Result<Frame> {
    Ok(Frame {
        kind: FrameKind::Control,
        session_id,
        request_id,
        body: serde_json::to_vec(control)?,
    })
}

fn parse_control(frame: Frame) -> Result<(u64, u64, Control)> {
    if frame.kind != FrameKind::Control {
        bail!("expected control frame");
    }
    if frame.body.len() > MAX_CONTROL_BYTES {
        bail!("oversized worker control body");
    }
    Ok((
        frame.session_id,
        frame.request_id,
        serde_json::from_slice(&frame.body)?,
    ))
}

fn parse_parent_control(frame: Frame) -> Result<(u64, u64, Control)> {
    let parsed = parse_control(frame)?;
    if !parsed.2.is_parent_command() {
        bail!("worker received a response-only control from its parent");
    }
    Ok(parsed)
}

fn parse_worker_control(frame: Frame) -> Result<(u64, u64, Control)> {
    let parsed = parse_control(frame)?;
    if !parsed.2.is_worker_response() {
        bail!("parent received a command-only control from its worker");
    }
    Ok(parsed)
}

type PendingResult = std::result::Result<Control, String>;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct Correlation {
    generation: u64,
    session_id: u64,
    request_id: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CancelOutcome {
    NoActiveRequest,
    CooperativeSettled,
    HardInvalidated,
}

trait WorkerProcess: Send + Sync {
    fn is_running(&self) -> Result<bool>;
    fn request_cooperative_cancel(&self) -> Result<bool> {
        Ok(false)
    }
    fn terminate(&self) -> Result<()>;
    fn wait(&self) -> Result<()>;
}

struct SpawnedWorker {
    stdin: Box<dyn Write + Send>,
    stdout: Box<dyn Read + Send>,
    process: Arc<dyn WorkerProcess>,
    expectation: WorkerExpectation,
    pack_launch: Option<PackLaunchContext>,
}

trait WorkerLauncher: Send + Sync {
    fn launch(&self) -> Result<SpawnedWorker>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum WorkerRole {
    Inference,
    Vad,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum WorkerProvider {
    Cpu,
    Cuda,
    Vulkan,
    Metal,
}

impl WorkerProvider {
    fn is_gpu(self) -> bool {
        !matches!(self, Self::Cpu)
    }
}

fn compiled_worker_provider(role: WorkerRole) -> WorkerProvider {
    if role == WorkerRole::Vad {
        return WorkerProvider::Cpu;
    }
    if cfg!(feature = "cuda-acceleration") {
        WorkerProvider::Cuda
    } else if cfg!(feature = "vulkan-acceleration") {
        WorkerProvider::Vulkan
    } else if cfg!(feature = "metal-acceleration") {
        WorkerProvider::Metal
    } else {
        WorkerProvider::Cpu
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum WorkerArtifactKind {
    Gguf,
    OnnxAsr,
    SileroVad,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkerArtifactTarget {
    artifact: WorkerArtifactKind,
    target: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkerExpectation {
    app_build: String,
    worker_build: String,
    bundled_worker_sha256: String,
    abi: u16,
    role: WorkerRole,
    provider: WorkerProvider,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pack: Option<WorkerPackExpectation>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkerPackExpectation {
    pack_id: String,
    pack_version: String,
    pack_digest: String,
    security_epoch: u64,
    runtime_abi: u16,
    backend: WorkerProvider,
    provider: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkerPackCapability {
    expectation: WorkerPackExpectation,
    devices: Vec<WorkerPackDeviceCapability>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkerPackDeviceCapability {
    stable_device_identity: String,
    process_index: usize,
    display_name: String,
    driver_version: Option<String>,
    device_class: DeviceClass,
    vendor: GpuVendor,
    memory_total_bytes: u64,
    memory_available_bytes: u64,
}

#[derive(Clone, Debug)]
#[cfg(any(feature = "inference-worker", test))]
struct CurrentPackRuntimeDeviceCatalog {
    provider: String,
    devices: Vec<CurrentPackRuntimeDevice>,
}

/// Volatile facts obtained from the launched worker's own challenge-bound
/// Hello. This is worker-local state only; it is never serialized, persisted,
/// or forwarded to another process as a device selector.
#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg_attr(not(any(feature = "inference-worker", test)), allow(dead_code))]
pub(crate) struct CurrentPackRuntimeDevice {
    pub(crate) stable_device_identity: String,
    pub(crate) process_index: usize,
    pub(crate) display_name: String,
    pub(crate) driver_version: Option<String>,
    pub(crate) device_class: DeviceClass,
    pub(crate) vendor: GpuVendor,
    pub(crate) memory_total_bytes: u64,
    pub(crate) memory_available_bytes: u64,
}

#[cfg(any(feature = "inference-worker", test))]
impl From<&WorkerPackDeviceCapability> for CurrentPackRuntimeDevice {
    fn from(device: &WorkerPackDeviceCapability) -> Self {
        Self {
            stable_device_identity: device.stable_device_identity.clone(),
            process_index: device.process_index,
            display_name: device.display_name.clone(),
            driver_version: device.driver_version.clone(),
            device_class: device.device_class,
            vendor: device.vendor,
            memory_total_bytes: device.memory_total_bytes,
            memory_available_bytes: device.memory_available_bytes,
        }
    }
}

#[cfg(any(feature = "inference-worker", test))]
thread_local! {
    // This worker-local cache is populated from the Hello enumeration of the
    // currently running process. It is intentionally neither serialized nor
    // inherited by a child process: stable identity is the only cross-process
    // binding.
    static CURRENT_PACK_RUNTIME_DEVICES: RefCell<Option<CurrentPackRuntimeDeviceCatalog>> = const { RefCell::new(None) };
}

#[cfg(any(feature = "inference-worker", test))]
fn clear_current_pack_runtime_devices() {
    CURRENT_PACK_RUNTIME_DEVICES.with(|catalog| *catalog.borrow_mut() = None);
}

#[cfg(not(any(feature = "inference-worker", test)))]
fn clear_current_pack_runtime_devices() {}

#[cfg(any(feature = "inference-worker", test))]
fn install_current_pack_runtime_devices(capability: &WorkerPackCapability) {
    CURRENT_PACK_RUNTIME_DEVICES.with(|catalog| {
        *catalog.borrow_mut() = Some(CurrentPackRuntimeDeviceCatalog {
            provider: capability.expectation.provider.clone(),
            devices: capability
                .devices
                .iter()
                .map(CurrentPackRuntimeDevice::from)
                .collect(),
        });
    });
}

#[cfg(any(feature = "inference-worker", test))]
pub(crate) fn current_pack_runtime_device(
    stable_device_identity: &str,
    provider: &str,
) -> Option<CurrentPackRuntimeDevice> {
    CURRENT_PACK_RUNTIME_DEVICES.with(|catalog| {
        let catalog = catalog.borrow();
        let catalog = catalog.as_ref()?;
        (catalog.provider == provider)
            .then(|| {
                catalog
                    .devices
                    .iter()
                    .find(|device| device.stable_device_identity == stable_device_identity)
                    .cloned()
            })
            .flatten()
    })
}

#[cfg(any(feature = "inference-worker", test))]
fn validate_unique_provider_process_indexes(devices: &[WorkerPackDeviceCapability]) -> Result<()> {
    let mut process_indexes = devices
        .iter()
        .map(|device| device.process_index)
        .collect::<Vec<_>>();
    process_indexes.sort_unstable();
    if process_indexes.windows(2).any(|pair| pair[0] == pair[1]) {
        bail!("compiled GPU provider reported duplicate process indexes");
    }
    Ok(())
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WorkerCapability {
    challenge: String,
    app_build: String,
    worker_build: String,
    bundled_worker_sha256: String,
    abi: u16,
    role: WorkerRole,
    provider: WorkerProvider,
    artifacts: Vec<WorkerArtifactTarget>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pack: Option<WorkerPackCapability>,
}

fn expected_worker(role: WorkerRole) -> WorkerExpectation {
    WorkerExpectation {
        app_build: DESKTOP_BUILD_ID.to_owned(),
        worker_build: match role {
            WorkerRole::Inference => INFERENCE_WORKER_BUILD_ID,
            WorkerRole::Vad => VAD_WORKER_BUILD_ID,
        }
        .to_owned(),
        bundled_worker_sha256: match role {
            WorkerRole::Inference => option_env!("SCRIBE_BUNDLED_WORKER_SHA256").unwrap_or(""),
            WorkerRole::Vad => "same-executable",
        }
        .to_owned(),
        abi: WORKER_ABI_VERSION,
        role,
        provider: compiled_worker_provider(role),
        pack: None,
    }
}

fn worker_capability(role: WorkerRole, challenge: String) -> Result<WorkerCapability> {
    let target = format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH);
    let artifacts = match role {
        WorkerRole::Inference => vec![
            WorkerArtifactTarget {
                artifact: WorkerArtifactKind::Gguf,
                target: target.clone(),
            },
            WorkerArtifactTarget {
                artifact: WorkerArtifactKind::OnnxAsr,
                target,
            },
        ],
        WorkerRole::Vad => vec![WorkerArtifactTarget {
            artifact: WorkerArtifactKind::SileroVad,
            target,
        }],
    };
    let inference_fingerprint = match role {
        WorkerRole::Inference => Some(inference_worker_capability_fingerprint()?),
        WorkerRole::Vad => None,
    };
    let bundled_worker_sha256 = inference_fingerprint.as_ref().map_or_else(
        || "same-executable".to_owned(),
        |value| value.sha256.clone(),
    );
    let capability = WorkerCapability {
        challenge,
        app_build: DESKTOP_BUILD_ID.to_owned(),
        worker_build: match role {
            WorkerRole::Inference => INFERENCE_WORKER_BUILD_ID,
            WorkerRole::Vad => VAD_WORKER_BUILD_ID,
        }
        .to_owned(),
        bundled_worker_sha256,
        abi: WORKER_ABI_VERSION,
        role,
        provider: compiled_worker_provider(role),
        artifacts,
        pack: worker_pack_capability(role)?,
    };
    // On Linux this closes inherited FD 3 only after the one Hello capability
    // has been completely formed from its sealed image digest.
    drop(inference_fingerprint);
    Ok(capability)
}

struct InferenceWorkerFingerprint {
    sha256: String,
    #[cfg(all(
        not(test),
        target_os = "linux",
        target_arch = "x86_64",
        target_env = "gnu"
    ))]
    _inherited_image: std::fs::File,
}

#[cfg(test)]
fn inference_worker_capability_fingerprint() -> Result<InferenceWorkerFingerprint> {
    Ok(InferenceWorkerFingerprint {
        sha256: String::new(),
    })
}

#[cfg(all(
    not(test),
    target_os = "linux",
    target_arch = "x86_64",
    target_env = "gnu"
))]
fn inference_worker_capability_fingerprint() -> Result<InferenceWorkerFingerprint> {
    use std::io::{Seek, SeekFrom};
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::fs::MetadataExt;

    const REQUIRED_SEALS: i32 =
        libc::F_SEAL_SEAL | libc::F_SEAL_SHRINK | libc::F_SEAL_GROW | libc::F_SEAL_WRITE;
    const TMPFS_MAGIC: libc::c_long = 0x0102_1994;

    if std::env::var(LINUX_EXECUTABLE_FD_ENV).as_deref() != Ok("3") {
        bail!("Linux inference worker received an unexpected executable-image descriptor");
    }

    // SAFETY: the Linux launcher transfers sole ownership of fixed FD 3 to
    // this one-Hello capability path. Every return path closes it.
    let inherited = unsafe { std::fs::File::from_raw_fd(LINUX_EXECUTABLE_FD) };
    let metadata = inherited
        .metadata()
        .context("could not inspect sealed Linux worker image")?;
    if !metadata.file_type().is_file() || metadata.nlink() != 0 {
        bail!("Linux worker image descriptor is not an anonymous regular memfd");
    }
    let mut filesystem = std::mem::MaybeUninit::<libc::statfs>::zeroed();
    if unsafe { libc::fstatfs(inherited.as_raw_fd(), filesystem.as_mut_ptr()) } == -1 {
        return Err(std::io::Error::last_os_error())
            .context("could not inspect Linux worker image filesystem");
    }
    // SAFETY: fstatfs initialized the structure on success.
    if unsafe { filesystem.assume_init() }.f_type as libc::c_long != TMPFS_MAGIC {
        bail!("Linux worker image descriptor is not backed by memfd tmpfs");
    }
    let seals = unsafe { libc::fcntl(inherited.as_raw_fd(), libc::F_GET_SEALS) };
    if seals == -1 {
        return Err(std::io::Error::last_os_error())
            .context("could not inspect Linux worker image seals");
    }
    if seals != REQUIRED_SEALS {
        bail!("Linux worker image descriptor does not have the exact required seals");
    }
    let duplicate = unsafe {
        libc::fcntl(
            inherited.as_raw_fd(),
            libc::F_DUPFD_CLOEXEC,
            LINUX_EXECUTABLE_FD + 1,
        )
    };
    if duplicate == -1 {
        return Err(std::io::Error::last_os_error())
            .context("could not duplicate sealed Linux worker image");
    }
    // SAFETY: fcntl returned a new owned descriptor.
    let mut image = unsafe { std::fs::File::from_raw_fd(duplicate) };
    image
        .seek(SeekFrom::Start(0))
        .context("could not rewind sealed Linux worker image")?;
    let sha256 = sha256_reader(image)
        .context("could not fingerprint sealed inference worker for capability handshake")?;
    Ok(InferenceWorkerFingerprint {
        sha256,
        _inherited_image: inherited,
    })
}

#[cfg(all(
    not(test),
    not(all(target_os = "linux", target_arch = "x86_64", target_env = "gnu"))
))]
fn inference_worker_capability_fingerprint() -> Result<InferenceWorkerFingerprint> {
    let executable = std::env::current_exe()
        .context("could not locate inference worker for capability fingerprint")?;
    Ok(InferenceWorkerFingerprint {
        sha256: sha256_file(&executable)
            .context("could not fingerprint inference worker for capability handshake")?,
    })
}

fn worker_pack_expectation_from_private_env() -> Result<Option<WorkerPackExpectation>> {
    let names = [
        PACK_ID_ENV,
        PACK_VERSION_ENV,
        PACK_DIGEST_ENV,
        PACK_SECURITY_EPOCH_ENV,
        PACK_RUNTIME_ABI_ENV,
        PACK_BACKEND_ENV,
        PACK_PROVIDER_ENV,
    ];
    let fields = names
        .iter()
        .map(std::env::var)
        .collect::<Vec<std::result::Result<String, std::env::VarError>>>();
    if fields.iter().all(|field| field.is_err()) {
        return Ok(None);
    }
    if fields.iter().any(Result::is_err) {
        bail!("GPU worker pack build identity is incomplete");
    }
    let fields = fields
        .into_iter()
        .map(|field| field.expect("checked above"))
        .collect::<Vec<_>>();
    let backend = match fields[5].as_str() {
        "cuda" => WorkerProvider::Cuda,
        "vulkan" => WorkerProvider::Vulkan,
        "metal" => WorkerProvider::Metal,
        _ => bail!("GPU worker pack backend is invalid"),
    };
    if backend != compiled_worker_provider(WorkerRole::Inference) {
        bail!("GPU worker pack backend does not match the compiled provider");
    }
    Ok(Some(WorkerPackExpectation {
        pack_id: fields[0].clone(),
        pack_version: fields[1].clone(),
        pack_digest: fields[2].clone(),
        security_epoch: fields[3]
            .parse()
            .context("GPU worker pack security epoch is invalid")?,
        runtime_abi: fields[4]
            .parse()
            .context("GPU worker pack runtime ABI is invalid")?,
        backend,
        provider: fields[6].clone(),
    }))
}

#[cfg(windows)]
struct PackDriverCatalog {
    by_pci_location: BTreeMap<(u32, u32, u32), String>,
}

#[cfg(windows)]
impl PackDriverCatalog {
    fn discover() -> Result<Self> {
        use windows_sys::Win32::Devices::DeviceAndDriverInstallation::{
            DIGCF_PRESENT, GUID_DEVCLASS_DISPLAY, SP_DEVINFO_DATA, SetupDiDestroyDeviceInfoList,
            SetupDiEnumDeviceInfo, SetupDiGetClassDevsW, SetupDiGetDevicePropertyW,
        };
        use windows_sys::Win32::Devices::Properties::{
            DEVPKEY_Device_Address, DEVPKEY_Device_BusNumber, DEVPKEY_Device_DriverVersion,
            DEVPROP_TYPE_STRING, DEVPROP_TYPE_UINT32, DEVPROPKEY,
        };

        struct DeviceInfoSet(isize);

        impl Drop for DeviceInfoSet {
            fn drop(&mut self) {
                // SAFETY: the handle came from SetupDiGetClassDevsW and is
                // owned exclusively by this guard.
                unsafe {
                    SetupDiDestroyDeviceInfoList(self.0);
                }
            }
        }

        fn property_u32(set: isize, device: &SP_DEVINFO_DATA, key: &DEVPROPKEY) -> Option<u32> {
            let mut property_type = 0;
            let mut required_size = 0;
            let mut bytes = [0_u8; std::mem::size_of::<u32>()];
            // SAFETY: all pointers reference initialized, correctly sized
            // buffers for the duration of the synchronous SetupAPI call.
            let ok = unsafe {
                SetupDiGetDevicePropertyW(
                    set,
                    device,
                    key,
                    &mut property_type,
                    bytes.as_mut_ptr(),
                    bytes.len() as u32,
                    &mut required_size,
                    0,
                )
            };
            (ok != 0 && property_type == DEVPROP_TYPE_UINT32 && required_size == bytes.len() as u32)
                .then(|| u32::from_ne_bytes(bytes))
        }

        fn driver_property(
            set: isize,
            device: &SP_DEVINFO_DATA,
            key: &DEVPROPKEY,
        ) -> Option<String> {
            let mut property_type = 0;
            let mut required_size = 0;
            let mut utf16 = [0_u16; 128];
            // SAFETY: the UTF-16 output buffer is writable and byte-sized as
            // required by SetupDiGetDevicePropertyW.
            let ok = unsafe {
                SetupDiGetDevicePropertyW(
                    set,
                    device,
                    key,
                    &mut property_type,
                    utf16.as_mut_ptr().cast(),
                    std::mem::size_of_val(&utf16) as u32,
                    &mut required_size,
                    0,
                )
            };
            if ok == 0
                || property_type != DEVPROP_TYPE_STRING
                || required_size < 2
                || required_size > std::mem::size_of_val(&utf16) as u32
                || required_size % 2 != 0
            {
                return None;
            }
            let units = required_size as usize / 2;
            let end = utf16[..units]
                .iter()
                .position(|unit| *unit == 0)
                .unwrap_or(units);
            let value = String::from_utf16(&utf16[..end]).ok()?;
            let canonical = value.trim().to_ascii_lowercase();
            (!canonical.is_empty()
                && canonical.len() <= 96
                && canonical.bytes().all(|byte| {
                    byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_' | b'+')
                }))
            .then(|| format!("windows-display:{canonical}"))
        }

        // SAFETY: the class GUID is process-static, the enumerator is null,
        // and no window handle is used. SetupAPI returns a new owned set.
        let raw_set = unsafe {
            SetupDiGetClassDevsW(
                &GUID_DEVCLASS_DISPLAY,
                std::ptr::null(),
                std::ptr::null_mut(),
                DIGCF_PRESENT,
            )
        };
        if raw_set == -1_isize {
            bail!("Windows display-adapter metadata is unavailable");
        }
        let set = DeviceInfoSet(raw_set);
        let mut by_pci_location = BTreeMap::new();
        for index in 0..64_u32 {
            let mut device = SP_DEVINFO_DATA {
                cbSize: std::mem::size_of::<SP_DEVINFO_DATA>() as u32,
                ClassGuid: windows_sys::core::GUID::from_u128(0),
                DevInst: 0,
                Reserved: 0,
            };
            // SAFETY: device has the required cbSize and remains writable for
            // the synchronous enumeration call.
            if unsafe { SetupDiEnumDeviceInfo(set.0, index, &mut device) } == 0 {
                break;
            }
            let Some(bus) = property_u32(set.0, &device, &DEVPKEY_Device_BusNumber) else {
                continue;
            };
            let Some(address) = property_u32(set.0, &device, &DEVPKEY_Device_Address) else {
                continue;
            };
            let Some(driver) = driver_property(set.0, &device, &DEVPKEY_Device_DriverVersion)
            else {
                continue;
            };
            let location = (bus, address >> 16, address & 0xffff);
            if by_pci_location.insert(location, driver).is_some() {
                bail!("Windows reported duplicate display-adapter PCI locations");
            }
        }
        if by_pci_location.is_empty() {
            bail!("Windows reported no display adapter with bounded driver metadata");
        }
        Ok(Self { by_pci_location })
    }

    #[cfg(any(feature = "inference-worker", test))]
    fn identity_for(&self, stable_device_identity: &str) -> Result<String> {
        let (bus, device, function) = parse_native_pci_location(stable_device_identity)
            .ok_or_else(|| anyhow!("GPU stable identity is not a canonical PCI location"))?;
        self.by_pci_location
            .get(&(bus, device, function))
            .cloned()
            .ok_or_else(|| anyhow!("GPU PCI identity has no matching Windows driver metadata"))
    }

    fn fingerprint(&self) -> String {
        let mut digest = Sha256::new();
        digest.update((self.by_pci_location.len() as u64).to_le_bytes());
        for ((bus, device, function), driver) in &self.by_pci_location {
            digest.update(bus.to_le_bytes());
            digest.update(device.to_le_bytes());
            digest.update(function.to_le_bytes());
            digest.update((driver.len() as u64).to_le_bytes());
            digest.update(driver.as_bytes());
        }
        format!("{:x}", digest.finalize())
    }
}

#[cfg(all(windows, feature = "vulkan-acceleration"))]
struct VulkanInstanceGuard(ash::Instance);

#[cfg(all(windows, feature = "vulkan-acceleration"))]
impl Drop for VulkanInstanceGuard {
    fn drop(&mut self) {
        // SAFETY: this guard exclusively owns the instance and all physical
        // device queries have completed before it is dropped.
        unsafe { self.0.destroy_instance(None) };
    }
}

#[cfg(all(windows, any(feature = "inference-worker", test)))]
#[derive(Clone, Debug, Eq, PartialEq)]
struct VulkanDeviceIdentity {
    normalized_display_name: String,
    device_class: DeviceClass,
    vendor: GpuVendor,
    stable_device_identity: String,
    driver_identity: String,
    claimed: bool,
}

#[cfg(all(windows, any(feature = "inference-worker", test)))]
struct VulkanDeviceCatalog {
    devices: Vec<VulkanDeviceIdentity>,
}

#[cfg(all(windows, any(feature = "inference-worker", test)))]
impl VulkanDeviceCatalog {
    #[cfg(feature = "vulkan-acceleration")]
    fn discover() -> Result<Self> {
        use ash::vk;
        use std::ffi::CStr;

        fn hex(bytes: &[u8]) -> String {
            use std::fmt::Write as _;

            let mut encoded = String::with_capacity(bytes.len() * 2);
            for byte in bytes {
                write!(&mut encoded, "{byte:02x}").expect("writing to a String cannot fail");
            }
            encoded
        }

        // SAFETY: loading occurs only inside the verified Vulkan worker after
        // its environment and DLL search path have been sanitized. The desktop
        // does not compile this feature in production or call this code.
        let entry =
            unsafe { ash::Entry::load() }.context("could not load the Windows Vulkan loader")?;
        let application_name = c"scribe-gpu-worker";
        let application = vk::ApplicationInfo::builder()
            .application_name(application_name)
            .application_version(1)
            .api_version(vk::API_VERSION_1_1);
        let create_info = vk::InstanceCreateInfo::builder().application_info(&application);
        // SAFETY: create_info contains no extension/layer pointers and remains
        // live for the synchronous loader call.
        let instance = VulkanInstanceGuard(
            unsafe { entry.create_instance(&create_info, None) }
                .context("could not create the Vulkan identity-query instance")?,
        );
        // SAFETY: the instance remains live through every returned physical
        // device query and the bounded vector is consumed before destruction.
        let physical_devices = unsafe { instance.0.enumerate_physical_devices() }
            .context("could not enumerate Vulkan physical devices for stable identity")?;
        if physical_devices.is_empty() || physical_devices.len() > 64 {
            bail!("Vulkan physical-device identity list is empty or oversized");
        }

        let mut devices = Vec::with_capacity(physical_devices.len());
        for physical_device in physical_devices {
            let mut id = vk::PhysicalDeviceIDProperties::default();
            let mut driver = vk::PhysicalDeviceDriverProperties::default();
            let mut properties = vk::PhysicalDeviceProperties2::builder()
                .push_next(&mut id)
                .push_next(&mut driver)
                .build();
            // SAFETY: all pNext structures are correctly initialized and live
            // for the synchronous properties query.
            unsafe {
                instance
                    .0
                    .get_physical_device_properties2(physical_device, &mut properties)
            };
            let device_class = match properties.properties.device_type {
                vk::PhysicalDeviceType::DISCRETE_GPU => DeviceClass::DiscreteGpu,
                vk::PhysicalDeviceType::INTEGRATED_GPU => DeviceClass::IntegratedGpu,
                _ => continue,
            };
            let vendor = match properties.properties.vendor_id {
                0x10de => GpuVendor::Nvidia,
                0x1002 | 0x1022 => GpuVendor::Amd,
                0x8086 => GpuVendor::Intel,
                _ => GpuVendor::Other,
            };
            // SAFETY: Vulkan guarantees that deviceName is a terminated UTF-8
            // string within this fixed-size array.
            let display_name =
                unsafe { CStr::from_ptr(properties.properties.device_name.as_ptr()) }
                    .to_str()
                    .context("Vulkan device name is not UTF-8")?;
            let normalized_display_name = normalize_gpu_display_name(display_name)
                .ok_or_else(|| anyhow!("Vulkan device name is empty or malformed"))?;
            let stable_device_identity = if id.device_luid_valid == vk::TRUE
                && id.device_luid.iter().any(|byte| *byte != 0)
            {
                format!("native:luid:{}", hex(&id.device_luid))
            } else if id.device_uuid.iter().any(|byte| *byte != 0) {
                format!("native:uuid:{}", hex(&id.device_uuid))
            } else {
                bail!("Vulkan physical device omitted both LUID and UUID identity");
            };
            if id.driver_uuid.iter().all(|byte| *byte == 0) {
                bail!("Vulkan physical device omitted driver UUID identity");
            }
            let driver_identity = format!(
                "vulkan:{:04x}:{:08x}:{:08x}:{}",
                properties.properties.vendor_id,
                driver.driver_id.as_raw(),
                properties.properties.driver_version,
                hex(&id.driver_uuid)
            );
            devices.push(VulkanDeviceIdentity {
                normalized_display_name,
                device_class,
                vendor,
                stable_device_identity,
                driver_identity,
                claimed: false,
            });
        }
        if devices.is_empty() {
            bail!("Vulkan reported no discrete or integrated physical GPU identity");
        }
        let mut identities = devices
            .iter()
            .map(|device| device.stable_device_identity.as_str())
            .collect::<Vec<_>>();
        identities.sort_unstable();
        if identities.windows(2).any(|pair| pair[0] == pair[1]) {
            bail!("Vulkan reported duplicate stable physical-device identities");
        }
        Ok(Self { devices })
    }

    fn claim(
        &mut self,
        display_name: &str,
        device_class: DeviceClass,
        vendor: GpuVendor,
    ) -> Result<(String, String)> {
        let normalized = normalize_gpu_display_name(display_name)
            .ok_or_else(|| anyhow!("GPU provider device name is empty or malformed"))?;
        let matching = self
            .devices
            .iter()
            .enumerate()
            .filter(|(_, device)| {
                !device.claimed
                    && device.normalized_display_name == normalized
                    && device.device_class == device_class
                    && device.vendor == vendor
            })
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        let [index] = matching.as_slice() else {
            if matching.is_empty() {
                bail!("GPU provider device has no matching Vulkan LUID/UUID identity");
            }
            bail!(
                "GPU provider device identity is ambiguous across {} Vulkan physical devices",
                matching.len()
            );
        };
        let device = &mut self.devices[*index];
        device.claimed = true;
        Ok((
            device.stable_device_identity.clone(),
            device.driver_identity.clone(),
        ))
    }
}

#[cfg(all(windows, any(feature = "inference-worker", test)))]
fn resolve_vulkan_provider_identity(
    native_id: Option<&str>,
    display_name: &str,
    device_class: DeviceClass,
    vendor: GpuVendor,
    driver_catalog: &PackDriverCatalog,
    vulkan_catalog: &mut VulkanDeviceCatalog,
) -> Result<(String, String)> {
    if let Some(native_id) = native_id.filter(|native_id| !native_id.is_empty()) {
        if !native_id.is_ascii() {
            bail!("GPU provider device stable identity is malformed");
        }
        let stable_device_identity = format!("native:{}", native_id.to_ascii_lowercase());
        if parse_native_pci_location(&stable_device_identity).is_some() {
            return Ok((
                stable_device_identity.clone(),
                driver_catalog.identity_for(&stable_device_identity)?,
            ));
        }
        // Provider-owned opaque IDs do not identify a Windows display adapter
        // by PCI address. Reconcile them against the independently enumerated
        // Vulkan LUID/UUID catalog instead of misclassifying them as malformed
        // PCI identity. `claim` requires one exact, unclaimed device fact set.
    }
    vulkan_catalog.claim(display_name, device_class, vendor)
}

#[cfg(all(windows, any(feature = "inference-worker", test)))]
fn normalize_gpu_display_name(value: &str) -> Option<String> {
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    (!normalized.is_empty()
        && normalized.len() <= 256
        && normalized
            .bytes()
            .all(|byte| byte.is_ascii_graphic() || byte == b' '))
    .then(|| normalized.to_ascii_lowercase())
}

#[cfg(target_os = "macos")]
struct PackDriverCatalog {
    os_build_witness: String,
}

#[cfg(target_os = "macos")]
impl PackDriverCatalog {
    fn discover() -> Result<Self> {
        let os_build_witness = crate::macos_power::os_build_witness()
            .context("could not obtain the macOS Metal runtime witness")?;
        Ok(Self { os_build_witness })
    }

    #[cfg(any(feature = "inference-worker", test))]
    fn identity_for(&self, stable_device_identity: &str) -> Result<String> {
        if !stable_device_identity.starts_with("metal-registry:") {
            bail!("Metal registry identity is not canonical");
        }
        Ok(self.os_build_witness.clone())
    }

    fn fingerprint(&self) -> String {
        let mut digest = Sha256::new();
        update_health_digest(&mut digest, "macos-parent-os-build-v1");
        update_health_digest(&mut digest, &self.os_build_witness);
        format!("{:x}", digest.finalize())
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
struct PackDriverCatalog;

#[cfg(not(any(windows, target_os = "macos")))]
impl PackDriverCatalog {
    fn discover() -> Result<Self> {
        bail!("Stage 4 GPU worker packs require Windows driver metadata")
    }

    #[cfg(any(feature = "inference-worker", test))]
    fn identity_for(&self, _stable_device_identity: &str) -> Result<String> {
        bail!("Stage 4 GPU worker packs require Windows driver metadata")
    }

    fn fingerprint(&self) -> String {
        "unsupported".to_owned()
    }
}

#[cfg(any(all(windows, feature = "inference-worker"), test))]
fn parse_native_pci_location(identity: &str) -> Option<(u32, u32, u32)> {
    if identity != identity.to_ascii_lowercase() {
        return None;
    }
    let raw = identity
        .strip_prefix("native:")?
        .strip_prefix("pci:")
        .unwrap_or_else(|| identity.strip_prefix("native:").expect("checked above"));
    let mut colon = raw.split(':');
    let domain_text = colon.next()?;
    let bus_text = colon.next()?;
    let device_function = colon.next()?;
    if !matches!(domain_text.len(), 4 | 8) || bus_text.len() != 2 || device_function.len() != 4 {
        return None;
    }
    let domain = u32::from_str_radix(domain_text, 16).ok()?;
    let bus = u32::from_str_radix(bus_text, 16).ok()?;
    if colon.next().is_some() || domain != 0 {
        return None;
    }
    let (device, function) = device_function.split_once('.')?;
    let device = u32::from_str_radix(device, 16).ok()?;
    let function = u32::from_str_radix(function, 16).ok()?;
    (device <= 0x1f && function <= 7).then_some((bus, device, function))
}

#[cfg(feature = "inference-worker")]
fn worker_pack_capability(role: WorkerRole) -> Result<Option<WorkerPackCapability>> {
    use transcribe_cpp::DeviceType;

    clear_current_pack_runtime_devices();
    let Some(expectation) = worker_pack_expectation_from_private_env()? else {
        return Ok(None);
    };
    if role != WorkerRole::Inference {
        bail!("only the inference role may advertise a GPU worker pack");
    }
    transcribe_cpp::init_backends_default()
        .context("could not initialize the compiled GPU provider for Hello")?;
    let expected_kind = match expectation.backend {
        WorkerProvider::Cuda => "cuda",
        WorkerProvider::Vulkan => "vulkan",
        WorkerProvider::Metal => "metal",
        WorkerProvider::Cpu => bail!("CPU is not a GPU worker-pack backend"),
    };
    let expected_device_id = std::env::var(PACK_DEVICE_ID_ENV).ok();
    let provider_devices = transcribe_cpp::devices()
        .into_iter()
        .filter(|device| device.kind.trim().eq_ignore_ascii_case(expected_kind))
        .collect::<Vec<_>>();
    if provider_devices.len() > MAX_PACK_DEVICES {
        bail!("compiled GPU provider exceeded the bounded device-list limit");
    }
    #[cfg(all(target_os = "linux", target_arch = "x86_64", target_env = "gnu"))]
    if matches!(
        expectation.backend,
        WorkerProvider::Cuda | WorkerProvider::Vulkan
    ) {
        use crate::linux_gpu::{
            KernelLinuxGpuFactSource, LinuxGpuBackend, LinuxGpuVendor, ProviderLinuxGpuDevice,
            route_provider_devices,
        };

        let backend = match expectation.backend {
            WorkerProvider::Cuda => LinuxGpuBackend::Cuda,
            WorkerProvider::Vulkan => LinuxGpuBackend::Vulkan,
            WorkerProvider::Cpu | WorkerProvider::Metal => unreachable!("matched above"),
        };
        let provider = provider_devices
            .iter()
            .map(|device| {
                let process_index = device.index.ok_or_else(|| {
                    anyhow!("Linux GPU provider device has no live process index")
                })?;
                let display_name = [&device.description, &device.name]
                    .into_iter()
                    .map(|value| value.trim())
                    .find(|value| !value.is_empty() && value.is_ascii())
                    .ok_or_else(|| {
                        anyhow!("Linux GPU provider device has no bounded display name")
                    })?;
                let normalized =
                    format!("{} {}", device.name, device.description).to_ascii_lowercase();
                let vendor = match backend {
                    LinuxGpuBackend::Cuda => LinuxGpuVendor::Nvidia,
                    LinuxGpuBackend::Vulkan if normalized.contains("nvidia") => {
                        LinuxGpuVendor::Nvidia
                    }
                    LinuxGpuBackend::Vulkan
                        if normalized.contains("amd") || normalized.contains("radeon") =>
                    {
                        LinuxGpuVendor::Amd
                    }
                    LinuxGpuBackend::Vulkan if normalized.contains("intel") => {
                        LinuxGpuVendor::Intel
                    }
                    LinuxGpuBackend::Vulkan => LinuxGpuVendor::Other,
                };
                Ok(ProviderLinuxGpuDevice {
                    process_index,
                    native_identity_or_alias: device.device_id.clone(),
                    display_name: display_name.to_owned(),
                    vendor,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let routed = route_provider_devices(&KernelLinuxGpuFactSource, backend, &provider)
            .map_err(anyhow::Error::msg)
            .context("could not bind Linux GPU provider devices to kernel PCI identity")?;
        let routed = routed
            .into_iter()
            .map(|device| (device.process_index, device))
            .collect::<BTreeMap<_, _>>();
        let devices = provider_devices
            .into_iter()
            .map(|device| {
                let process_index = device.index.ok_or_else(|| {
                    anyhow!("Linux GPU provider device has no live process index")
                })?;
                let routed = routed
                    .get(&process_index)
                    .ok_or_else(|| anyhow!("Linux GPU provider process index was not remapped"))?;
                let display_name = [&device.description, &device.name]
                    .into_iter()
                    .map(|value| value.trim())
                    .find(|value| !value.is_empty() && value.is_ascii())
                    .ok_or_else(|| {
                        anyhow!("Linux GPU provider device has no bounded display name")
                    })?
                    .to_owned();
                let device_class = match device.device_type {
                    DeviceType::Gpu => DeviceClass::DiscreteGpu,
                    DeviceType::Igpu => DeviceClass::IntegratedGpu,
                    DeviceType::Unknown | DeviceType::Cpu | DeviceType::Accel => {
                        bail!("Linux GPU provider returned an incomplete or non-GPU device class")
                    }
                };
                let vendor = match routed.vendor {
                    LinuxGpuVendor::Nvidia => GpuVendor::Nvidia,
                    LinuxGpuVendor::Amd => GpuVendor::Amd,
                    LinuxGpuVendor::Intel => GpuVendor::Intel,
                    LinuxGpuVendor::Other => GpuVendor::Other,
                };
                Ok(WorkerPackDeviceCapability {
                    stable_device_identity: routed.stable_device_identity.clone(),
                    process_index,
                    display_name,
                    driver_version: Some(routed.driver_identity.clone()),
                    device_class,
                    vendor,
                    memory_total_bytes: device.memory_total,
                    memory_available_bytes: device.memory_free.min(device.memory_total),
                })
            })
            .collect::<Result<Vec<_>>>()?;
        return finish_worker_pack_capability(expectation, expected_device_id, devices);
    }
    #[cfg(all(target_os = "linux", target_arch = "x86_64", target_env = "gnu"))]
    if expectation.backend == WorkerProvider::Metal {
        bail!("Metal GPU worker pack requires macOS Metal acceleration");
    }
    let driver_catalog = PackDriverCatalog::discover()
        .context("could not obtain authoritative GPU driver identity")?;
    #[cfg(all(windows, feature = "vulkan-acceleration"))]
    let mut vulkan_catalog = (expectation.backend == WorkerProvider::Vulkan)
        .then(VulkanDeviceCatalog::discover)
        .transpose()
        .context("could not obtain Vulkan LUID/UUID identity")?;
    #[cfg(all(target_os = "macos", feature = "metal-acceleration"))]
    if expectation.backend == WorkerProvider::Metal {
        let provider = provider_devices
            .iter()
            .map(|device| {
                let process_index = device
                    .index
                    .ok_or_else(|| anyhow!("Metal provider device has no live process index"))?;
                let display_name = [&device.description, &device.name]
                    .into_iter()
                    .map(|value| value.trim())
                    .find(|value| !value.is_empty() && value.is_ascii())
                    .ok_or_else(|| anyhow!("Metal provider device has no bounded display name"))?;
                Ok(crate::macos_gpu::ProviderMetalDevice {
                    process_index,
                    display_name: display_name.to_owned(),
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let metal = crate::macos_gpu::discover_devices()
            .context("could not obtain MTLDevice registry identities")?;
        let devices = crate::macos_gpu::remap_provider_devices(&provider, &metal)?
            .into_iter()
            .map(|device| {
                Ok(WorkerPackDeviceCapability {
                    stable_device_identity: device.metal.stable_identity.clone(),
                    process_index: device.process_index,
                    display_name: device.metal.display_name,
                    driver_version: Some(
                        driver_catalog.identity_for(&device.metal.stable_identity)?,
                    ),
                    device_class: device.metal.device_class,
                    vendor: device.metal.vendor,
                    memory_total_bytes: device.metal.memory_total_bytes,
                    memory_available_bytes: device.metal.memory_available_bytes,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        return finish_worker_pack_capability(expectation, expected_device_id, devices);
    }
    if expectation.backend == WorkerProvider::Metal {
        bail!("Metal GPU worker pack requires macOS Metal acceleration");
    }
    let mut devices = Vec::with_capacity(provider_devices.len());
    for device in provider_devices {
        let process_index = device
            .index
            .ok_or_else(|| anyhow!("GPU provider device has no live process index"))?;
        let display_name = [&device.description, &device.name]
            .into_iter()
            .map(|value| value.trim())
            .find(|value| !value.is_empty() && value.is_ascii())
            .ok_or_else(|| anyhow!("GPU provider device has no bounded display name"))?
            .to_owned();
        let device_class = match device.device_type {
            DeviceType::Gpu if expectation.backend == WorkerProvider::Metal => {
                DeviceClass::UnifiedGpu
            }
            DeviceType::Gpu => DeviceClass::DiscreteGpu,
            DeviceType::Igpu if expectation.backend == WorkerProvider::Metal => {
                DeviceClass::UnifiedGpu
            }
            DeviceType::Igpu => DeviceClass::IntegratedGpu,
            DeviceType::Unknown => DeviceClass::Unknown,
            DeviceType::Cpu | DeviceType::Accel => {
                bail!("GPU provider returned a non-GPU device")
            }
        };
        let normalized = format!("{} {}", device.name, device.description).to_ascii_lowercase();
        let vendor = match expectation.backend {
            WorkerProvider::Cuda => GpuVendor::Nvidia,
            WorkerProvider::Metal => GpuVendor::Apple,
            WorkerProvider::Vulkan if normalized.contains("nvidia") => GpuVendor::Nvidia,
            WorkerProvider::Vulkan
                if normalized.contains("amd") || normalized.contains("radeon") =>
            {
                GpuVendor::Amd
            }
            WorkerProvider::Vulkan if normalized.contains("intel") => GpuVendor::Intel,
            WorkerProvider::Vulkan => GpuVendor::Other,
            WorkerProvider::Cpu => GpuVendor::Unknown,
        };
        let native_id = device.device_id.as_deref().map(str::trim);
        let (stable_device_identity, driver_version) =
            if expectation.backend == WorkerProvider::Vulkan {
                #[cfg(all(windows, feature = "vulkan-acceleration"))]
                {
                    resolve_vulkan_provider_identity(
                        native_id,
                        &display_name,
                        device_class,
                        vendor,
                        &driver_catalog,
                        vulkan_catalog
                            .as_mut()
                            .expect("Vulkan catalog exists for the compiled Vulkan worker"),
                    )?
                }
                #[cfg(not(all(windows, feature = "vulkan-acceleration")))]
                {
                    bail!("Vulkan GPU worker pack was built without Vulkan acceleration")
                }
            } else {
                let native_id = native_id
                    .filter(|native_id| !native_id.is_empty() && native_id.is_ascii())
                    .ok_or_else(|| anyhow!("GPU provider device has no stable identity"))?;
                let stable_device_identity = format!("native:{}", native_id.to_ascii_lowercase());
                let driver_version = driver_catalog.identity_for(&stable_device_identity)?;
                (stable_device_identity, driver_version)
            };
        devices.push(WorkerPackDeviceCapability {
            stable_device_identity,
            process_index,
            display_name,
            driver_version: Some(driver_version),
            device_class,
            vendor,
            memory_total_bytes: device.memory_total,
            memory_available_bytes: device.memory_free.min(device.memory_total),
        });
    }
    finish_worker_pack_capability(expectation, expected_device_id, devices)
}

#[cfg(feature = "inference-worker")]
fn finish_worker_pack_capability(
    expectation: WorkerPackExpectation,
    expected_device_id: Option<String>,
    devices: Vec<WorkerPackDeviceCapability>,
) -> Result<Option<WorkerPackCapability>> {
    // Validate before the parent-selected stable ID narrows the list. A
    // duplicate index in another provider device must not be hidden by that
    // filter and later make an index-only remap ambiguous.
    validate_unique_provider_process_indexes(&devices)?;
    let mut devices = devices
        .into_iter()
        .filter(|device| {
            expected_device_id
                .as_deref()
                .is_none_or(|expected| expected == device.stable_device_identity)
        })
        .collect::<Vec<_>>();
    devices.sort_by(|left, right| {
        left.stable_device_identity
            .cmp(&right.stable_device_identity)
    });
    if devices.is_empty() {
        bail!("compiled GPU provider reported no addressable stable device");
    }
    if devices.len() > MAX_PACK_DEVICES {
        bail!("compiled GPU provider exceeded the bounded device-list limit");
    }
    if devices
        .windows(2)
        .any(|pair| pair[0].stable_device_identity == pair[1].stable_device_identity)
    {
        bail!("compiled GPU provider reported duplicate stable device identities");
    }
    let capability = WorkerPackCapability {
        expectation,
        devices,
    };
    install_current_pack_runtime_devices(&capability);
    debug_assert!(capability.devices.iter().all(|device| {
        current_pack_runtime_device(
            &device.stable_device_identity,
            &capability.expectation.provider,
        )
        .is_some_and(|current| current.process_index == device.process_index)
    }));
    Ok(Some(capability))
}

#[cfg(not(feature = "inference-worker"))]
fn worker_pack_capability(_role: WorkerRole) -> Result<Option<WorkerPackCapability>> {
    clear_current_pack_runtime_devices();
    worker_pack_expectation_from_private_env().and_then(|pack| {
        if pack.is_some() {
            bail!("desktop process cannot advertise a GPU worker pack")
        }
        Ok(None)
    })
}

fn random_challenge() -> Result<String> {
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes)
        .map_err(|error| anyhow!("could not obtain worker handshake randomness: {error}"))?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn sha256_file(path: &Path) -> Result<String> {
    sha256_reader(std::fs::File::open(path)?)
}

fn sha256_reader(mut reader: impl Read) -> Result<String> {
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn validate_worker_capability(
    capability: &WorkerCapability,
    challenge: &str,
    expected: &WorkerExpectation,
) -> Result<()> {
    if challenge.len() != 64
        || !challenge.bytes().all(|byte| byte.is_ascii_hexdigit())
        || capability.challenge != challenge
    {
        bail!("worker capability challenge did not bind to this process generation");
    }
    if capability.app_build != expected.app_build
        || capability.worker_build != expected.worker_build
        || (!expected.bundled_worker_sha256.is_empty()
            && capability.bundled_worker_sha256 != expected.bundled_worker_sha256)
        || capability.abi != expected.abi
        || capability.role != expected.role
        || capability.provider != expected.provider
        || capability.pack.as_ref().map(|pack| &pack.expectation) != expected.pack.as_ref()
    {
        bail!("worker capability is incompatible with the requesting application");
    }
    let expected_artifacts = match expected.role {
        WorkerRole::Inference => [WorkerArtifactKind::Gguf, WorkerArtifactKind::OnnxAsr].as_slice(),
        WorkerRole::Vad => [WorkerArtifactKind::SileroVad].as_slice(),
    };
    if capability.artifacts.len() != expected_artifacts.len()
        || !expected_artifacts.iter().all(|kind| {
            capability.artifacts.iter().any(|target| {
                target.artifact == *kind
                    && target.target
                        == format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
            })
        })
    {
        bail!("worker capability does not advertise the required artifact targets");
    }
    Ok(())
}

fn validate_worker_hello(
    role: Option<WorkerRole>,
    challenge: &str,
    expected: &WorkerExpectation,
) -> Result<WorkerRole> {
    if challenge.len() != 64 || !challenge.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("worker handshake challenge must be 32 random bytes encoded as hexadecimal");
    }
    let actual_role = role.unwrap_or(expected.role);
    let mut local = expected_worker(actual_role);
    local.pack = worker_pack_expectation_from_private_env()?;
    // Final-image verification is directional: the parent owns the verified
    // executable descriptor and digest. The child validates the protocol and
    // build contract but does not compare the parent-supplied digest against a
    // compile-time environment value.
    if expected.app_build != local.app_build
        || expected.worker_build != local.worker_build
        || expected.abi != local.abi
        || expected.role != local.role
        || expected.provider != local.provider
        || expected.pack != local.pack
    {
        bail!("worker handshake expectation is incompatible with this worker");
    }
    Ok(actual_role)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum WireArtifactFormat {
    Gguf,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireRuntimeModel {
    id: String,
    path: PathBuf,
    format: WireArtifactFormat,
    expected_size_bytes: u64,
    expected_sha256: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "runtime",
    content = "model",
    rename_all = "snake_case",
    deny_unknown_fields
)]
enum WireRuntimeArtifact {
    Gguf(WireRuntimeModel),
    OnnxBundle(OnnxModelSpec),
}

impl From<RuntimeArtifact> for WireRuntimeArtifact {
    fn from(artifact: RuntimeArtifact) -> Self {
        match artifact {
            RuntimeArtifact::Gguf(model) => Self::Gguf(model.into()),
            RuntimeArtifact::OnnxBundle(model) => Self::OnnxBundle(model),
        }
    }
}

impl TryFrom<WireRuntimeArtifact> for RuntimeArtifact {
    type Error = anyhow::Error;

    fn try_from(artifact: WireRuntimeArtifact) -> Result<Self> {
        artifact.validate()?;
        Ok(match artifact {
            WireRuntimeArtifact::Gguf(model) => Self::Gguf(model.try_into()?),
            WireRuntimeArtifact::OnnxBundle(model) => Self::OnnxBundle(model),
        })
    }
}

impl WireRuntimeArtifact {
    fn validate(&self) -> Result<()> {
        match self {
            Self::Gguf(model) => {
                validate_wire_runtime_model(model)?;
                if model.format != WireArtifactFormat::Gguf {
                    bail!("GGUF runtime tag does not match its artifact format");
                }
            }
            Self::OnnxBundle(model) => model.validate()?,
        }
        Ok(())
    }
}

fn validate_wire_runtime_model(model: &WireRuntimeModel) -> Result<()> {
    if model.id.trim().is_empty() || model.id.len() > 512 {
        bail!("runtime model id must contain between 1 and 512 bytes");
    }
    if model.path.as_os_str().is_empty() || model.path.to_string_lossy().len() > 32 * 1024 {
        bail!("runtime model path is empty or oversized");
    }
    if model.expected_size_bytes == 0 {
        bail!("runtime model expected size must be non-zero");
    }
    if model.expected_sha256.len() != 64
        || !model
            .expected_sha256
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        bail!("runtime model SHA-256 must contain exactly 64 hexadecimal characters");
    }
    Ok(())
}

impl From<RuntimeModel> for WireRuntimeModel {
    fn from(model: RuntimeModel) -> Self {
        Self {
            id: model.id.into_inner(),
            path: model.path,
            format: match model.format {
                ArtifactFormat::Gguf => WireArtifactFormat::Gguf,
            },
            expected_size_bytes: model.expected_size_bytes,
            expected_sha256: model.expected_sha256,
        }
    }
}

impl TryFrom<WireRuntimeModel> for RuntimeModel {
    type Error = anyhow::Error;

    fn try_from(model: WireRuntimeModel) -> Result<Self> {
        let format = match model.format {
            WireArtifactFormat::Gguf => ArtifactFormat::Gguf,
        };
        Ok(Self {
            id: ModelId::new(model.id),
            path: model.path,
            format,
            expected_size_bytes: model.expected_size_bytes,
            expected_sha256: model.expected_sha256,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireRuntimeDiagnostics {
    resolved_acceleration: ResolvedAcceleration,
    runtime_location: PathBuf,
    warm_reused: bool,
    model_load_duration_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireRuntimeExecution {
    transcript: WireTranscript,
    diagnostics: WireRuntimeDiagnostics,
    processing_duration_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireTranscript {
    text: String,
    segments: Vec<WireTranscriptSegment>,
    detected_language: Option<String>,
    duration_ms: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireTranscriptSegment {
    text: String,
    start_ms: Option<u64>,
    end_ms: Option<u64>,
    confidence: Option<f32>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireRuntimeLoadExecution {
    diagnostics: WireRuntimeDiagnostics,
    detected_architecture: String,
    capabilities: RuntimeCapabilities,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireRuntimeError {
    code: WireRuntimeErrorCode,
    #[serde(default)]
    category: WireFailureCategory,
    #[serde(default)]
    retry: WireRetryDisposition,
    fatal: bool,
    message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    artifact_path: Option<PathBuf>,
    sample_rate_hz: Option<u32>,
    channels: Option<u16>,
    model_id: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum WireRuntimeErrorCode {
    ArtifactIntegrity,
    InvalidAudio,
    Inference,
    Callback,
    Engine,
    BackendInitializationFailed,
    BackendUnavailable,
    OutOfMemory,
    Poisoned,
    UnsupportedModel,
    WorkerUnavailable,
    OnnxUnavailable,
    RetryableWorkerFailure,
    Cancelled,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum WireFailureCategory {
    Artifact,
    InvalidInput,
    Decode,
    Callback,
    #[default]
    Worker,
    Unsupported,
    Cancellation,
    Provider,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum WireRetryDisposition {
    #[default]
    Never,
    NextProviderBeforeOutput,
}

impl WireRuntimeError {
    fn from_runtime(error: &RuntimeError) -> Self {
        let code = match error {
            RuntimeError::InvalidAudio { .. } => WireRuntimeErrorCode::InvalidAudio,
            RuntimeError::Inference(_) => WireRuntimeErrorCode::Inference,
            RuntimeError::Callback(_) => WireRuntimeErrorCode::Callback,
            RuntimeError::Engine(_) => WireRuntimeErrorCode::Engine,
            RuntimeError::BackendInitializationFailed(_) => {
                WireRuntimeErrorCode::BackendInitializationFailed
            }
            RuntimeError::BackendUnavailable(_) => WireRuntimeErrorCode::BackendUnavailable,
            RuntimeError::OutOfMemory(_) => WireRuntimeErrorCode::OutOfMemory,
            RuntimeError::ArtifactIntegrity { .. } => WireRuntimeErrorCode::ArtifactIntegrity,
            RuntimeError::Poisoned => WireRuntimeErrorCode::Poisoned,
            RuntimeError::UnsupportedModel(_) => WireRuntimeErrorCode::UnsupportedModel,
            RuntimeError::WorkerUnavailable(_) => WireRuntimeErrorCode::WorkerUnavailable,
            RuntimeError::OnnxUnavailable(_) => WireRuntimeErrorCode::OnnxUnavailable,
            RuntimeError::RetryableWorkerFailure(_) => WireRuntimeErrorCode::RetryableWorkerFailure,
            RuntimeError::Cancelled(_) => WireRuntimeErrorCode::Cancelled,
        };
        let category = match error {
            RuntimeError::ArtifactIntegrity { .. } => WireFailureCategory::Artifact,
            RuntimeError::InvalidAudio { .. } => WireFailureCategory::InvalidInput,
            RuntimeError::Inference(_) | RuntimeError::Engine(_) => WireFailureCategory::Decode,
            RuntimeError::BackendInitializationFailed(_)
            | RuntimeError::BackendUnavailable(_)
            | RuntimeError::OutOfMemory(_) => WireFailureCategory::Provider,
            RuntimeError::Callback(_) => WireFailureCategory::Callback,
            RuntimeError::Poisoned
            | RuntimeError::WorkerUnavailable(_)
            | RuntimeError::RetryableWorkerFailure(_) => WireFailureCategory::Worker,
            RuntimeError::UnsupportedModel(_) | RuntimeError::OnnxUnavailable(_) => {
                WireFailureCategory::Unsupported
            }
            RuntimeError::Cancelled(_) => WireFailureCategory::Cancellation,
        };
        let retry = if matches!(
            error,
            RuntimeError::RetryableWorkerFailure(_)
                | RuntimeError::BackendInitializationFailed(_)
                | RuntimeError::BackendUnavailable(_)
                | RuntimeError::OutOfMemory(_)
        ) {
            WireRetryDisposition::NextProviderBeforeOutput
        } else {
            WireRetryDisposition::Never
        };
        Self {
            code,
            category,
            retry,
            fatal: matches!(
                error,
                RuntimeError::Poisoned | RuntimeError::WorkerUnavailable(_)
            ),
            message: error.to_string(),
            artifact_path: match error {
                RuntimeError::ArtifactIntegrity { path, .. } => Some(path.clone()),
                _ => None,
            },
            sample_rate_hz: match error {
                RuntimeError::InvalidAudio { sample_rate_hz, .. } => Some(*sample_rate_hz),
                _ => None,
            },
            channels: match error {
                RuntimeError::InvalidAudio { channels, .. } => Some(*channels),
                _ => None,
            },
            model_id: match error {
                RuntimeError::UnsupportedModel(model_id) => Some(model_id.as_str().to_owned()),
                _ => None,
            },
        }
    }

    fn into_runtime(self) -> RuntimeError {
        match self.code {
            WireRuntimeErrorCode::Inference => RuntimeError::Inference(self.message),
            WireRuntimeErrorCode::Callback => RuntimeError::Callback(self.message),
            WireRuntimeErrorCode::InvalidAudio => RuntimeError::InvalidAudio {
                sample_rate_hz: self.sample_rate_hz.unwrap_or_default(),
                channels: self.channels.unwrap_or_default(),
            },
            WireRuntimeErrorCode::Engine => RuntimeError::Engine(self.message),
            WireRuntimeErrorCode::BackendInitializationFailed => {
                RuntimeError::BackendInitializationFailed(self.message)
            }
            WireRuntimeErrorCode::BackendUnavailable => {
                RuntimeError::BackendUnavailable(self.message)
            }
            WireRuntimeErrorCode::OutOfMemory => RuntimeError::OutOfMemory(self.message),
            WireRuntimeErrorCode::ArtifactIntegrity => RuntimeError::ArtifactIntegrity {
                path: self
                    .artifact_path
                    .unwrap_or_else(|| PathBuf::from("<inference-worker artifact>")),
                message: self.message,
            },
            WireRuntimeErrorCode::OnnxUnavailable => RuntimeError::OnnxUnavailable(self.message),
            WireRuntimeErrorCode::Poisoned => RuntimeError::Poisoned,
            WireRuntimeErrorCode::UnsupportedModel => RuntimeError::UnsupportedModel(ModelId::new(
                self.model_id.unwrap_or_else(|| self.message.clone()),
            )),
            WireRuntimeErrorCode::WorkerUnavailable => {
                RuntimeError::WorkerUnavailable(self.message)
            }
            WireRuntimeErrorCode::RetryableWorkerFailure => {
                if self.retry == WireRetryDisposition::NextProviderBeforeOutput {
                    RuntimeError::RetryableWorkerFailure(self.message)
                } else {
                    RuntimeError::WorkerUnavailable(self.message)
                }
            }
            WireRuntimeErrorCode::Cancelled => RuntimeError::Cancelled(self.message),
        }
    }

    fn into_runtime_for_generation(
        self,
        transport: &ProcessWorkerSupervisor,
        generation: u64,
    ) -> RuntimeError {
        if self.fatal {
            let _ = transport.invalidate_generation(
                generation,
                "worker reported a fatal runtime error",
                true,
            );
        }
        self.into_runtime()
    }
}

impl From<NativeRuntimeDiagnostics> for WireRuntimeDiagnostics {
    fn from(value: NativeRuntimeDiagnostics) -> Self {
        Self {
            resolved_acceleration: value.resolved_acceleration,
            runtime_location: value.runtime_location,
            warm_reused: value.warm_reused,
            model_load_duration_ms: u64::try_from(value.model_load_duration_ms).unwrap_or(u64::MAX),
        }
    }
}

impl From<WireRuntimeDiagnostics> for NativeRuntimeDiagnostics {
    fn from(value: WireRuntimeDiagnostics) -> Self {
        Self {
            resolved_acceleration: value.resolved_acceleration,
            runtime_location: value.runtime_location,
            warm_reused: value.warm_reused,
            model_load_duration_ms: u128::from(value.model_load_duration_ms),
        }
    }
}

impl From<RuntimeExecution> for WireRuntimeExecution {
    fn from(value: RuntimeExecution) -> Self {
        Self {
            transcript: value.transcript.into(),
            diagnostics: value.diagnostics.into(),
            processing_duration_ms: u64::try_from(value.processing_duration_ms).unwrap_or(u64::MAX),
        }
    }
}

impl From<WireRuntimeExecution> for RuntimeExecution {
    fn from(value: WireRuntimeExecution) -> Self {
        Self {
            transcript: value.transcript.into(),
            diagnostics: value.diagnostics.into(),
            processing_duration_ms: u128::from(value.processing_duration_ms),
        }
    }
}

impl From<Transcript> for WireTranscript {
    fn from(value: Transcript) -> Self {
        Self {
            text: value.text,
            segments: value.segments.into_iter().map(Into::into).collect(),
            detected_language: value.detected_language,
            duration_ms: value
                .duration_ms
                .map(|duration| u64::try_from(duration).unwrap_or(u64::MAX)),
        }
    }
}

impl From<WireTranscript> for Transcript {
    fn from(value: WireTranscript) -> Self {
        Self {
            text: value.text,
            segments: value.segments.into_iter().map(Into::into).collect(),
            detected_language: value.detected_language,
            duration_ms: value.duration_ms.map(u128::from),
        }
    }
}

impl From<TranscriptSegment> for WireTranscriptSegment {
    fn from(value: TranscriptSegment) -> Self {
        Self {
            text: value.text,
            start_ms: value.start_ms,
            end_ms: value.end_ms,
            confidence: value.confidence,
        }
    }
}

impl From<WireTranscriptSegment> for TranscriptSegment {
    fn from(value: WireTranscriptSegment) -> Self {
        Self {
            text: value.text,
            start_ms: value.start_ms,
            end_ms: value.end_ms,
            confidence: value.confidence,
        }
    }
}

impl From<RuntimeLoadExecution> for WireRuntimeLoadExecution {
    fn from(value: RuntimeLoadExecution) -> Self {
        Self {
            diagnostics: value.diagnostics.into(),
            detected_architecture: value.detected_architecture,
            capabilities: value.capabilities,
        }
    }
}

impl From<WireRuntimeLoadExecution> for RuntimeLoadExecution {
    fn from(value: WireRuntimeLoadExecution) -> Self {
        Self {
            diagnostics: value.diagnostics.into(),
            detected_architecture: value.detected_architecture,
            capabilities: value.capabilities,
        }
    }
}

fn control_allowed_for_role(control: &Control, role: WorkerRole) -> bool {
    match role {
        WorkerRole::Inference => matches!(
            control,
            Control::Hello { .. }
                | Control::LoadRuntime { .. }
                | Control::BeginBatch { .. }
                | Control::EndBatch
                | Control::StartStream
                | Control::AudioChunk
                | Control::EndStream
                | Control::Cancel { .. }
                | Control::Unload
                | Control::Health
                | Control::Shutdown
        ),
        WorkerRole::Vad => matches!(
            control,
            Control::Hello { .. }
                | Control::LoadVad { .. }
                | Control::StartVad { .. }
                | Control::VadWindow
                | Control::ResetVad
                | Control::EndVad
                | Control::Cancel { .. }
                | Control::Unload
                | Control::Health
                | Control::Shutdown
        ),
    }
}

trait WorkerExecutableResolver: Send + Sync {
    fn resolve(&self, role: WorkerRole) -> Result<VerifiedWorkerExecutable>;
}

struct InstalledWorkerExecutableResolver;

#[derive(Clone, Debug, Eq, PartialEq)]
struct WorkerExecutableIdentity {
    length: u64,
    sha256: String,
    #[cfg(windows)]
    volume_serial: u32,
    #[cfg(windows)]
    file_index: u64,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
}

#[derive(Clone, Debug)]
struct PackLaunchContext {
    lease: Arc<VerifiedPackLease>,
    expectation: WorkerPackExpectation,
    expected_device: Option<BackendTarget>,
    #[cfg(unix)]
    unix_exec_authority: Arc<crate::gpu_worker_pack::UnixPackExecAuthority>,
}

#[cfg(test)]
impl PackLaunchContext {
    fn fixture(lease: Arc<VerifiedPackLease>, expected_device: Option<BackendTarget>) -> Self {
        #[cfg(unix)]
        let unix_exec_authority = {
            use std::os::unix::fs::OpenOptionsExt;

            let executable_fd = std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open(lease.worker_path())
                .expect("fixture worker must open without following links");
            let dependency_root_fd = std::fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open(&lease.verified_pack().root)
                .expect("fixture pack root must open without following links");
            Arc::new(crate::gpu_worker_pack::UnixPackExecAuthority::fixture(
                Arc::clone(&lease),
                executable_fd,
                dependency_root_fd,
            ))
        };

        Self {
            expectation: pack_expectation(&lease),
            lease,
            expected_device,
            #[cfg(unix)]
            unix_exec_authority,
        }
    }
}

struct BoundPackHelloBridge<'a> {
    context: &'a PackLaunchContext,
    capability: &'a WorkerPackCapability,
    device: &'a WorkerPackDeviceCapability,
}

impl ResolverHelloBindingBridge for BoundPackHelloBridge<'_> {
    fn resolver_verified_pack_lease(&self) -> Arc<VerifiedPackLease> {
        Arc::clone(&self.context.lease)
    }

    #[cfg(unix)]
    fn resolver_unix_launch_authority(
        &self,
    ) -> Option<Arc<crate::gpu_worker_pack::UnixPackExecAuthority>> {
        Some(Arc::clone(&self.context.unix_exec_authority))
    }

    fn hello_pack_id(&self) -> &str {
        &self.capability.expectation.pack_id
    }

    fn hello_pack_version(&self) -> &str {
        &self.capability.expectation.pack_version
    }

    fn hello_pack_digest(&self) -> &str {
        &self.capability.expectation.pack_digest
    }

    fn hello_security_epoch(&self) -> u64 {
        self.capability.expectation.security_epoch
    }

    fn hello_runtime_abi(&self) -> u16 {
        self.capability.expectation.runtime_abi
    }

    fn hello_backend(&self) -> PackBackend {
        pack_backend_from_worker_provider(self.capability.expectation.backend)
            .expect("a worker-pack capability is always a GPU backend")
    }

    fn hello_provider(&self) -> &str {
        &self.capability.expectation.provider
    }

    fn hello_stable_device_identity(&self) -> &str {
        &self.device.stable_device_identity
    }

    fn hello_process_index(&self) -> Option<usize> {
        Some(self.device.process_index)
    }

    fn hello_display_name(&self) -> &str {
        &self.device.display_name
    }

    fn hello_driver_version(&self) -> Option<&str> {
        self.device.driver_version.as_deref()
    }

    fn hello_device_class(&self) -> DeviceClass {
        self.device.device_class
    }

    fn hello_vendor(&self) -> GpuVendor {
        self.device.vendor
    }

    fn hello_memory_total_bytes(&self) -> u64 {
        self.device.memory_total_bytes
    }

    fn hello_memory_available_bytes(&self) -> u64 {
        self.device.memory_available_bytes
    }
}

fn bind_pack_hello(
    context: &PackLaunchContext,
    capability: &WorkerPackCapability,
) -> Result<Vec<VerifiedPackLaunchBinding>> {
    if capability.expectation != context.expectation {
        bail!("GPU worker Hello pack identity differs from resolver authority");
    }
    if capability.devices.is_empty() || capability.devices.len() > MAX_PACK_DEVICES {
        bail!("GPU worker Hello device list is empty or oversized");
    }
    let mut prior = None;
    let mut process_indexes = Vec::with_capacity(capability.devices.len());
    for device in &capability.devices {
        if prior
            .as_ref()
            .is_some_and(|identity: &String| identity >= &device.stable_device_identity)
        {
            bail!("GPU worker Hello device list is not strictly stable-ID sorted");
        }
        prior = Some(device.stable_device_identity.clone());
        process_indexes.push(device.process_index);
    }
    process_indexes.sort_unstable();
    if process_indexes.windows(2).any(|pair| pair[0] == pair[1]) {
        bail!("GPU worker Hello device list contains duplicate process indexes");
    }
    let observed_backend = match capability.expectation.backend {
        WorkerProvider::Cuda => BackendKind::Cuda,
        WorkerProvider::Vulkan => BackendKind::Vulkan,
        WorkerProvider::Metal => BackendKind::Metal,
        WorkerProvider::Cpu => bail!("GPU worker Hello advertised CPU"),
    };
    let devices = if let Some(expected) = &context.expected_device {
        let device = capability
            .devices
            .iter()
            .find(|device| device.stable_device_identity == expected.device_id.as_str())
            .ok_or_else(|| anyhow!("GPU worker Hello omitted the parent-selected stable device"))?;
        if observed_backend != expected.backend
            || device.vendor != expected.vendor
            || device.device_class != expected.device_class
            || device.memory_total_bytes != expected.memory_total_bytes
            || device.driver_version != expected.driver_version
        {
            bail!("GPU worker Hello device facts differ from the parent-observed target");
        }
        // The enumeration index is deliberately refreshed from this Hello.
        // Stable identity, rather than a persisted index, is the authority.
        std::slice::from_ref(device)
    } else {
        capability.devices.as_slice()
    };
    devices
        .iter()
        .map(|device| {
            VerifiedPackLaunchBinding::try_from_resolver_hello_bridge(&BoundPackHelloBridge {
                context,
                capability,
                device,
            })
            .ok_or_else(|| anyhow!("GPU worker resolver/Hello binding was rejected"))
        })
        .collect()
}

struct VerifiedWorkerExecutable {
    path: PathBuf,
    root: PathBuf,
    expected_name: std::ffi::OsString,
    expected_sha256: String,
    identity: WorkerExecutableIdentity,
    pack_launch: Option<PackLaunchContext>,
    #[cfg(all(target_os = "linux", target_arch = "x86_64", target_env = "gnu"))]
    linux_exec_authority: Option<Arc<dyn crate::linux_worker_launch::LinuxExecAuthority>>,
    // On Windows this read-only, non-delete-sharing handle prevents replacement
    // between verification and CreateProcess. Other platforms still retain the
    // open inode while the immediate identity recheck closes ordinary races.
    _open_file: std::fs::File,
}

impl std::fmt::Debug for VerifiedWorkerExecutable {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut debug = formatter.debug_struct("VerifiedWorkerExecutable");
        debug
            .field("path", &self.path)
            .field("root", &self.root)
            .field("expected_name", &self.expected_name)
            .field("expected_sha256", &self.expected_sha256)
            .field("identity", &self.identity)
            .field("pack_launch", &self.pack_launch);
        #[cfg(all(target_os = "linux", target_arch = "x86_64", target_env = "gnu"))]
        debug.field(
            "has_linux_exec_authority",
            &self.linux_exec_authority.is_some(),
        );
        debug.finish_non_exhaustive()
    }
}

impl VerifiedWorkerExecutable {
    fn revalidate(&self) -> Result<Self> {
        let mut verified = verify_worker_executable(
            &self.path,
            &self.root,
            &self.expected_name,
            &self.expected_sha256,
        )?;
        if verified.identity != self.identity {
            bail!("worker executable identity changed before process creation");
        }
        verified.pack_launch = self.pack_launch.clone();
        #[cfg(all(target_os = "linux", target_arch = "x86_64", target_env = "gnu"))]
        {
            verified.linux_exec_authority = self.linux_exec_authority.clone();
        }
        Ok(verified)
    }
}

fn directory_contains_exact_name(root: &Path, expected_name: &std::ffi::OsStr) -> Result<bool> {
    let mut exact = false;
    let expected_lossy = expected_name.to_string_lossy();
    for entry in std::fs::read_dir(root)? {
        let name = entry?.file_name();
        if name == expected_name {
            exact = true;
        } else if name.to_string_lossy().eq_ignore_ascii_case(&expected_lossy) {
            bail!("worker executable name differs from the packaged case");
        }
    }
    Ok(exact)
}

fn open_worker_no_follow(path: &Path) -> Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ,
        };
        options
            .share_mode(FILE_SHARE_READ)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    options.open(path).with_context(|| {
        format!(
            "could not open worker executable through a no-follow handle: {}",
            path.display()
        )
    })
}

fn worker_identity(file: &std::fs::File, path: &Path) -> Result<WorkerExecutableIdentity> {
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata_is_link_or_reparse(&metadata) {
        bail!("worker executable is not a regular, non-reparse file");
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::Storage::FileSystem::{
            BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
        };
        let mut information = unsafe { std::mem::zeroed::<BY_HANDLE_FILE_INFORMATION>() };
        if unsafe { GetFileInformationByHandle(file.as_raw_handle() as _, &mut information) } == 0 {
            return Err(std::io::Error::last_os_error())
                .context("could not read worker file identity from its verified handle");
        }
        if information.nNumberOfLinks != 1 {
            bail!("worker executable must not be a hardlink");
        }
        Ok(WorkerExecutableIdentity {
            length: metadata.len(),
            sha256: sha256_file(path)?,
            volume_serial: information.dwVolumeSerialNumber,
            file_index: (u64::from(information.nFileIndexHigh) << 32)
                | u64::from(information.nFileIndexLow),
        })
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.nlink() != 1 {
            bail!("worker executable must not be a hardlink");
        }
        Ok(WorkerExecutableIdentity {
            length: metadata.len(),
            sha256: sha256_file(path)?,
            device: metadata.dev(),
            inode: metadata.ino(),
        })
    }
    #[cfg(not(any(windows, unix)))]
    Ok(WorkerExecutableIdentity {
        length: metadata.len(),
        sha256: sha256_file(path)?,
    })
}

fn verify_worker_executable(
    candidate: &Path,
    expected_root: &Path,
    expected_name: &std::ffi::OsStr,
    expected_sha256: &str,
) -> Result<VerifiedWorkerExecutable> {
    if candidate.file_name() != Some(expected_name)
        || candidate
            .file_name()
            .is_some_and(|name| name.to_string_lossy().contains(':'))
    {
        bail!("worker executable path has an unexpected name or alternate data stream");
    }
    reject_link_components(expected_root)?;
    reject_link_components(candidate)?;
    let root = std::fs::canonicalize(expected_root)
        .context("could not canonicalize the worker install directory")?;
    let path =
        std::fs::canonicalize(candidate).context("could not canonicalize the worker executable")?;
    if path.parent() != Some(root.as_path())
        || !directory_contains_exact_name(&root, expected_name)?
    {
        bail!("worker executable resolved outside its exact canonical install directory");
    }
    let open_file = open_worker_no_follow(&path)?;
    let identity = worker_identity(&open_file, &path)?;
    if !expected_sha256.is_empty() && identity.sha256 != expected_sha256 {
        bail!("bundled inference worker SHA-256 does not match the desktop trust anchor");
    }
    Ok(VerifiedWorkerExecutable {
        path,
        root,
        expected_name: expected_name.to_owned(),
        expected_sha256: expected_sha256.to_owned(),
        identity,
        pack_launch: None,
        #[cfg(all(target_os = "linux", target_arch = "x86_64", target_env = "gnu"))]
        linux_exec_authority: None,
        _open_file: open_file,
    })
}

fn worker_provider_from_pack_backend(backend: PackBackend) -> WorkerProvider {
    match backend {
        PackBackend::Cuda => WorkerProvider::Cuda,
        PackBackend::Vulkan => WorkerProvider::Vulkan,
        PackBackend::Metal => WorkerProvider::Metal,
    }
}

fn pack_backend_from_worker_provider(provider: WorkerProvider) -> Option<PackBackend> {
    match provider {
        WorkerProvider::Cuda => Some(PackBackend::Cuda),
        WorkerProvider::Vulkan => Some(PackBackend::Vulkan),
        WorkerProvider::Metal => Some(PackBackend::Metal),
        WorkerProvider::Cpu => None,
    }
}

fn pack_expectation(lease: &VerifiedPackLease) -> WorkerPackExpectation {
    let pack = lease.verified_pack();
    WorkerPackExpectation {
        pack_id: pack.pack_id.as_str().to_owned(),
        pack_version: pack.pack_version.as_str().to_owned(),
        pack_digest: pack.pack_digest.clone(),
        security_epoch: pack.security_epoch,
        runtime_abi: pack.runtime_abi_version,
        backend: worker_provider_from_pack_backend(pack.backend),
        provider: pack.provider.clone(),
    }
}

struct VerifiedPackWorkerExecutableResolver {
    binding: VerifiedPackLaunchBinding,
}

impl WorkerExecutableResolver for VerifiedPackWorkerExecutableResolver {
    fn resolve(&self, role: WorkerRole) -> Result<VerifiedWorkerExecutable> {
        if role != WorkerRole::Inference {
            bail!("verified GPU worker packs can launch only the inference role");
        }
        let lease = self.binding.verified_pack_lease_arc();
        let expected_device = self.binding.backend_target();
        let pack = lease.verified_pack();
        if expected_device.pack.as_ref().is_none_or(|identity| {
            identity.pack_digest != pack.pack_digest
                || identity.runtime_abi != pack.runtime_abi_version
                || identity.security_epoch != pack.security_epoch
        }) {
            bail!("verified GPU launch binding no longer matches its retained pack");
        }
        let executable = resolve_verified_pack_executable(lease, Some(expected_device))?;
        #[cfg(unix)]
        {
            let mut executable = executable;
            if let Some(context) = executable.pack_launch.as_mut() {
                // Preserve the exact descriptor authority that launched the probe
                // and survived challenge-bound Hello validation.
                context.unix_exec_authority = self.binding.unix_exec_authority_arc();
            }
            return Ok(executable);
        }
        #[cfg(not(unix))]
        Ok(executable)
    }
}

/// Bootstrap is deliberately separate from an execution route. It can launch
/// only a bounded Hello probe from a retained, freshly reverified pack lease;
/// the resulting opaque binding is required before any model command route is
/// constructed.
struct VerifiedPackProbeExecutableResolver {
    lease: Arc<VerifiedPackLease>,
}

impl WorkerExecutableResolver for VerifiedPackProbeExecutableResolver {
    fn resolve(&self, role: WorkerRole) -> Result<VerifiedWorkerExecutable> {
        if role != WorkerRole::Inference {
            bail!("verified GPU worker probes can launch only the inference role");
        }
        resolve_verified_pack_executable(Arc::clone(&self.lease), None)
    }
}

fn resolve_verified_pack_executable(
    lease: Arc<VerifiedPackLease>,
    expected_device: Option<BackendTarget>,
) -> Result<VerifiedWorkerExecutable> {
    let pack = lease.verified_pack();
    let allowed = match pack.backend {
        PackBackend::Cuda => &[PackBackend::Cuda][..],
        PackBackend::Vulkan => &[PackBackend::Vulkan][..],
        PackBackend::Metal => &[PackBackend::Metal][..],
    };
    #[cfg(test)]
    let trust_root = lease
        .test_reverification_trust()
        .unwrap_or(&ProductionTrustRoot);
    #[cfg(not(test))]
    let trust_root = &ProductionTrustRoot;
    let verifier = PackVerifier::new(
        trust_root,
        crate::gpu_worker_pack::manifest::Compatibility {
            app_build: DESKTOP_BUILD_ID,
            worker_build: INFERENCE_WORKER_BUILD_ID,
            target_os: std::env::consts::OS,
            target_arch: std::env::consts::ARCH,
            allowed_backends: allowed,
        },
    );
    let launchable = verifier
        .launchable_worker(&lease)
        .context("GPU worker pack changed before launch")?;
    let worker_path = launchable.path();
    let expected_sha256 = lease
        .copy_entries()
        .iter()
        .find(|entry| entry.path == pack.worker_relative_path)
        .map(|entry| entry.sha256.as_str())
        .ok_or_else(|| anyhow!("GPU worker pack inventory omitted its worker"))?;
    let root = worker_path
        .parent()
        .ok_or_else(|| anyhow!("GPU worker pack executable has no parent directory"))?;
    let name = worker_path
        .file_name()
        .ok_or_else(|| anyhow!("GPU worker pack executable has no filename"))?;
    let mut executable = verify_worker_executable(worker_path, root, name, expected_sha256)?;
    #[cfg(unix)]
    let unix_exec_authority =
        crate::gpu_worker_pack::UnixPackExecAuthority::from_verified_pack_lease(Arc::clone(&lease))
            .context("could not retain verified Unix pack launch authority")?;
    #[cfg(all(test, target_os = "linux", target_arch = "x86_64", target_env = "gnu"))]
    {
        executable.linux_exec_authority = Some(unix_exec_authority.clone());
    }
    executable.pack_launch = Some(PackLaunchContext {
        expectation: pack_expectation(&lease),
        lease,
        expected_device,
        #[cfg(unix)]
        unix_exec_authority,
    });
    Ok(executable)
}

#[allow(
    dead_code,
    reason = "the desktop keeps the hardened resolver compiled while production routing remains CPU-only"
)]
fn resolve_adjacent_inference_worker(current_executable: &Path) -> Result<PathBuf> {
    let current = std::fs::canonicalize(current_executable)
        .context("could not canonicalize the running Scribe executable")?;
    let install_dir = current
        .parent()
        .ok_or_else(|| anyhow!("running Scribe executable has no parent directory"))?;
    let candidate = install_dir.join(format!(
        "scribe-inference-worker{}",
        std::env::consts::EXE_SUFFIX
    ));
    let expected_name = candidate
        .file_name()
        .ok_or_else(|| anyhow!("dedicated inference worker has no file name"))?;
    verify_worker_executable(&candidate, install_dir, expected_name, "")
        .map(|verified| verified.path)
}

impl WorkerExecutableResolver for InstalledWorkerExecutableResolver {
    fn resolve(&self, role: WorkerRole) -> Result<VerifiedWorkerExecutable> {
        #[cfg(all(
            unix,
            not(debug_assertions),
            not(all(target_os = "linux", target_arch = "x86_64", target_env = "gnu"))
        ))]
        {
            let _ = role;
            bail!(
                "release inference workers are unsupported on Unix until launch is bound to the verified executable descriptor"
            );
        }
        #[cfg(all(target_os = "linux", target_arch = "x86_64", target_env = "gnu"))]
        if role == WorkerRole::Inference {
            let authority = crate::linux_worker_launch::InstalledWorkerAuthority::open_production(
                option_env!("SCRIBE_BUNDLED_WORKER_SHA256").unwrap_or(""),
            )
            .context("could not admit the packaged Linux CPU inference worker")?;
            let identity = authority.identity();
            let open_file = authority
                .duplicate_executable()
                .context("could not retain the packaged Linux worker descriptor")?;
            let linux_exec_authority: Arc<dyn crate::linux_worker_launch::LinuxExecAuthority> =
                authority;
            return Ok(VerifiedWorkerExecutable {
                path: PathBuf::from(crate::linux_worker_launch::INSTALL_ROOT)
                    .join(crate::linux_worker_launch::WORKER_NAME),
                root: PathBuf::from(crate::linux_worker_launch::INSTALL_ROOT),
                expected_name: crate::linux_worker_launch::WORKER_NAME.into(),
                expected_sha256: identity.sha256.clone(),
                identity: WorkerExecutableIdentity {
                    length: identity.length,
                    sha256: identity.sha256,
                    device: identity.device,
                    inode: identity.inode,
                },
                pack_launch: None,
                linux_exec_authority: Some(linux_exec_authority),
                _open_file: open_file,
            });
        }
        let current = std::fs::canonicalize(std::env::current_exe()?)
            .context("could not canonicalize the running Scribe executable")?;
        if role == WorkerRole::Vad {
            let root = current
                .parent()
                .ok_or_else(|| anyhow!("running Scribe executable has no parent directory"))?;
            let name = current
                .file_name()
                .ok_or_else(|| anyhow!("running Scribe executable has no file name"))?;
            return verify_worker_executable(&current, root, name, "");
        }
        let root = current
            .parent()
            .ok_or_else(|| anyhow!("running Scribe executable has no parent directory"))?;
        let candidate = root.join(format!(
            "scribe-inference-worker{}",
            std::env::consts::EXE_SUFFIX
        ));
        let name = candidate
            .file_name()
            .ok_or_else(|| anyhow!("dedicated inference worker has no file name"))?;
        verify_worker_executable(
            &candidate,
            root,
            name,
            option_env!("SCRIBE_BUNDLED_WORKER_SHA256").unwrap_or(""),
        )
    }
}

#[cfg(test)]
struct FixedWorkerExecutableResolver(PathBuf);

#[cfg(test)]
impl WorkerExecutableResolver for FixedWorkerExecutableResolver {
    fn resolve(&self, _role: WorkerRole) -> Result<VerifiedWorkerExecutable> {
        let root = self
            .0
            .parent()
            .ok_or_else(|| anyhow!("test worker has no parent directory"))?;
        let name = self
            .0
            .file_name()
            .ok_or_else(|| anyhow!("test worker has no file name"))?;
        verify_worker_executable(&self.0, root, name, "")
    }
}

struct OsWorkerLauncher {
    role: WorkerRole,
    resolver: Arc<dyn WorkerExecutableResolver>,
}

impl OsWorkerLauncher {
    fn inference() -> Self {
        Self {
            role: WorkerRole::Inference,
            resolver: Arc::new(InstalledWorkerExecutableResolver),
        }
    }

    fn vad() -> Self {
        Self {
            role: WorkerRole::Vad,
            resolver: Arc::new(InstalledWorkerExecutableResolver),
        }
    }

    fn for_pack_binding(binding: VerifiedPackLaunchBinding) -> Self {
        Self {
            role: WorkerRole::Inference,
            resolver: Arc::new(VerifiedPackWorkerExecutableResolver { binding }),
        }
    }

    fn for_pack_probe(lease: Arc<VerifiedPackLease>) -> Self {
        Self {
            role: WorkerRole::Inference,
            resolver: Arc::new(VerifiedPackProbeExecutableResolver { lease }),
        }
    }

    #[cfg(test)]
    fn for_executable(role: WorkerRole, executable: PathBuf) -> Self {
        Self {
            role,
            resolver: Arc::new(FixedWorkerExecutableResolver(executable)),
        }
    }
}

impl WorkerLauncher for OsWorkerLauncher {
    fn launch(&self) -> Result<SpawnedWorker> {
        let executable = self.resolver.resolve(self.role)?;
        let mut expectation = expected_worker(self.role);
        if let Some(pack) = &executable.pack_launch {
            expectation.provider = pack.expectation.backend;
            expectation.pack = Some(pack.expectation.clone());
            expectation.bundled_worker_sha256 = executable.identity.sha256.clone();
        }
        let worker_flag = match self.role {
            WorkerRole::Inference => INFERENCE_WORKER_FLAG,
            WorkerRole::Vad => VAD_WORKER_FLAG,
        };
        #[cfg(all(target_os = "linux", target_arch = "x86_64", target_env = "gnu"))]
        if let Some(authority) = executable.linux_exec_authority.clone() {
            let bindings = executable
                .pack_launch
                .as_ref()
                .map(worker_pack_environment_bindings)
                .unwrap_or_default();
            let launched = crate::linux_worker_launch::launch_verified_worker(
                authority,
                worker_flag,
                &bindings,
            )
            .context("descriptor-bound Linux worker launch failed")?;
            return Ok(SpawnedWorker {
                stdin: Box::new(launched.stdin),
                stdout: Box::new(launched.stdout),
                process: Arc::new(launched.process),
                expectation,
                pack_launch: executable.pack_launch,
            });
        }
        #[cfg(all(target_os = "linux", target_arch = "x86_64", target_env = "gnu"))]
        if self.role == WorkerRole::Inference || executable.pack_launch.is_some() {
            bail!("Linux inference worker resolved without descriptor launch authority");
        }
        #[cfg(target_os = "macos")]
        if let Some(pack) = executable.pack_launch.as_ref() {
            let launched = crate::macos_worker_launch::launch_verified_worker(
                &pack.unix_exec_authority,
                worker_flag,
                &worker_pack_environment_bindings(pack),
            )?;
            return Ok(SpawnedWorker {
                stdin: Box::new(launched.stdin),
                stdout: Box::new(launched.stdout),
                process: Arc::new(launched.process),
                expectation,
                pack_launch: executable.pack_launch,
            });
        }
        let mut command = Command::new(&executable.path);
        command
            .arg(worker_flag)
            .current_dir(
                executable
                    .path
                    .parent()
                    .ok_or_else(|| anyhow!("worker executable has no trusted parent directory"))?,
            )
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        configure_worker_environment(&mut command);
        if let Some(pack) = &executable.pack_launch {
            configure_worker_pack_environment(&mut command, pack);
        }
        let mut parent_liveness = ParentLivenessChannel::attach(&mut command)?;
        configure_hidden_worker_command(&mut command);
        let _immediate_identity_check = executable.revalidate()?;
        let mut child = command.spawn()?;
        parent_liveness.child_spawned();
        let process_guard =
            bind_worker_process_tree_or_terminate(&mut child, bind_worker_process_tree)?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("process worker stdin unavailable"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("process worker stdout unavailable"))?;
        Ok(SpawnedWorker {
            stdin: Box::new(stdin),
            stdout: Box::new(stdout),
            process: Arc::new(OsWorkerProcess {
                child: Mutex::new(child),
                _process_guard: process_guard,
                _parent_liveness: parent_liveness,
            }),
            expectation,
            pack_launch: executable.pack_launch,
        })
    }
}

fn bind_worker_process_tree_or_terminate(
    child: &mut Child,
    bind: impl FnOnce(&Child) -> Result<ProcessTreeGuard>,
) -> Result<ProcessTreeGuard> {
    match bind(child) {
        Ok(guard) => Ok(guard),
        Err(bind_error) => {
            let kill_error = child.kill().err();
            let wait_error = child.wait().err();
            match (kill_error, wait_error) {
                (None, None) => Err(bind_error.context(
                    "worker process-tree supervision failed; child was terminated and reaped",
                )),
                (kill_error, wait_error) => bail!(
                    "worker process-tree supervision failed: {bind_error:#}; child cleanup failed (kill: {}, reap: {})",
                    kill_error
                        .map(|error| error.to_string())
                        .unwrap_or_else(|| "ok".to_owned()),
                    wait_error
                        .map(|error| error.to_string())
                        .unwrap_or_else(|| "ok".to_owned())
                ),
            }
        }
    }
}

fn configure_worker_environment(command: &mut Command) {
    command.env_clear();
    #[cfg(windows)]
    const REQUIRED: &[&str] = &[
        "SystemRoot",
        "WINDIR",
        "TEMP",
        "TMP",
        "USERPROFILE",
        "LOCALAPPDATA",
        "APPDATA",
    ];
    #[cfg(unix)]
    const REQUIRED: &[&str] = &[
        "HOME",
        "TMPDIR",
        "XDG_CACHE_HOME",
        "XDG_CONFIG_HOME",
        "LANG",
        "LC_ALL",
    ];
    #[cfg(not(any(windows, unix)))]
    const REQUIRED: &[&str] = &[];

    for name in REQUIRED {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
}

fn configure_worker_pack_environment(command: &mut Command, context: &PackLaunchContext) {
    for (name, value) in worker_pack_environment_bindings(context) {
        command.env(name, value);
    }
}

fn worker_pack_environment_bindings(context: &PackLaunchContext) -> Vec<(String, String)> {
    let pack = &context.expectation;
    let mut bindings = vec![
        (PACK_ID_ENV.to_owned(), pack.pack_id.clone()),
        (PACK_VERSION_ENV.to_owned(), pack.pack_version.clone()),
        (PACK_DIGEST_ENV.to_owned(), pack.pack_digest.clone()),
        (
            PACK_SECURITY_EPOCH_ENV.to_owned(),
            pack.security_epoch.to_string(),
        ),
        (
            PACK_RUNTIME_ABI_ENV.to_owned(),
            pack.runtime_abi.to_string(),
        ),
        (
            PACK_BACKEND_ENV.to_owned(),
            match pack.backend {
                WorkerProvider::Cuda => "cuda",
                WorkerProvider::Vulkan => "vulkan",
                WorkerProvider::Metal => "metal",
                WorkerProvider::Cpu => "cpu",
            }
            .to_owned(),
        ),
        (PACK_PROVIDER_ENV.to_owned(), pack.provider.clone()),
    ];
    if let Some(device) = &context.expected_device {
        bindings.push((
            PACK_DEVICE_ID_ENV.to_owned(),
            device.device_id.as_str().to_owned(),
        ));
        if let Some(driver) = &device.driver_version {
            bindings.push((PACK_DRIVER_ID_ENV.to_owned(), driver.clone()));
        }
    }
    bindings
}

#[cfg(target_os = "macos")]
impl WorkerProcess for crate::macos_worker_launch::MacWorkerProcess {
    fn is_running(&self) -> Result<bool> {
        self.is_running()
    }

    fn request_cooperative_cancel(&self) -> Result<bool> {
        self.request_cooperative_cancel()
    }

    fn terminate(&self) -> Result<()> {
        self.terminate()
    }

    fn wait(&self) -> Result<()> {
        self.wait()
    }
}

#[cfg(all(target_os = "linux", target_arch = "x86_64", target_env = "gnu"))]
impl WorkerProcess for crate::linux_worker_launch::LinuxWorkerProcess {
    fn is_running(&self) -> Result<bool> {
        Ok(self.is_running()?)
    }

    fn request_cooperative_cancel(&self) -> Result<bool> {
        Ok(self.request_cooperative_cancel()?)
    }

    fn terminate(&self) -> Result<()> {
        Ok(self.terminate()?)
    }

    fn wait(&self) -> Result<()> {
        Ok(self.wait()?)
    }
}

#[cfg(windows)]
pub(crate) fn harden_windows_dll_search() -> Result<()> {
    use windows_sys::Win32::System::LibraryLoader::{
        LOAD_LIBRARY_SEARCH_APPLICATION_DIR, LOAD_LIBRARY_SEARCH_SYSTEM32,
        SetDefaultDllDirectories, SetDllDirectoryW,
    };

    if unsafe {
        SetDefaultDllDirectories(LOAD_LIBRARY_SEARCH_APPLICATION_DIR | LOAD_LIBRARY_SEARCH_SYSTEM32)
    } == 0
    {
        return Err(std::io::Error::last_os_error())
            .context("could not restrict Windows default DLL search directories");
    }
    let empty_directory = [0_u16];
    if unsafe { SetDllDirectoryW(empty_directory.as_ptr()) } == 0 {
        return Err(std::io::Error::last_os_error())
            .context("could not remove the current directory from Windows DLL search");
    }
    Ok(())
}

#[cfg(not(windows))]
pub(crate) fn harden_windows_dll_search() -> Result<()> {
    Ok(())
}

#[cfg(all(target_os = "linux", target_arch = "x86_64", target_env = "gnu"))]
#[allow(
    dead_code,
    reason = "the shared supervisor module exposes this only to the dedicated worker binary"
)]
pub(crate) fn validate_linux_worker_entrypoint() -> Result<()> {
    let mut arguments = std::env::args_os();
    let _program = arguments
        .next()
        .ok_or_else(|| anyhow!("Linux inference worker received no argv[0]"))?;
    if arguments.next().as_deref() != Some(std::ffi::OsStr::new(INFERENCE_WORKER_FLAG))
        || arguments.next().is_some()
    {
        bail!("Linux inference worker requires exactly its private inference flag");
    }
    if std::env::var(PARENT_LIVENESS_ENV).as_deref() != Ok("5") {
        bail!("Linux inference worker received an unexpected parent-liveness descriptor");
    }
    if std::env::var(LINUX_EXECUTABLE_FD_ENV).as_deref() != Ok("3") {
        bail!("Linux inference worker received an unexpected executable-image descriptor");
    }
    for (name, _) in std::env::vars_os() {
        let Some(name) = name.to_str() else {
            bail!("Linux inference worker environment name is not UTF-8");
        };
        if !matches!(name, "PATH" | "LANG") && !name.starts_with("SCRIBE_PRIVATE_") {
            bail!("Linux inference worker received an unexpected environment field");
        }
    }
    if unsafe { libc::prctl(libc::PR_GET_NO_NEW_PRIVS, 0, 0, 0, 0) } != 1 {
        bail!("Linux inference worker was not launched with no_new_privs");
    }
    if unsafe { libc::getpgrp() } != unsafe { libc::getpid() } {
        bail!("Linux inference worker is not the leader of its private process group");
    }
    Ok(())
}

#[cfg(not(all(target_os = "linux", target_arch = "x86_64", target_env = "gnu")))]
#[allow(
    dead_code,
    reason = "non-Linux desktop tests compile the dedicated worker entrypoint shim"
)]
pub(crate) fn validate_linux_worker_entrypoint() -> Result<()> {
    Ok(())
}

#[cfg(windows)]
struct ParentLivenessChannel {
    child_read: isize,
    parent_write: isize,
}

#[cfg(windows)]
unsafe impl Send for ParentLivenessChannel {}
#[cfg(windows)]
unsafe impl Sync for ParentLivenessChannel {}

#[cfg(windows)]
impl ParentLivenessChannel {
    fn attach(command: &mut Command) -> Result<Self> {
        use windows_sys::Win32::Foundation::{HANDLE_FLAG_INHERIT, SetHandleInformation};
        use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
        use windows_sys::Win32::System::Pipes::CreatePipe;

        let security = SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: std::ptr::null_mut(),
            bInheritHandle: 1,
        };
        let mut child_read = std::ptr::null_mut();
        let mut parent_write = std::ptr::null_mut();
        if unsafe { CreatePipe(&mut child_read, &mut parent_write, &security, 0) } == 0 {
            return Err(std::io::Error::last_os_error())
                .context("could not create parent-liveness pipe");
        }
        if unsafe { SetHandleInformation(parent_write, HANDLE_FLAG_INHERIT, 0) } == 0 {
            let error = std::io::Error::last_os_error();
            unsafe {
                windows_sys::Win32::Foundation::CloseHandle(child_read);
                windows_sys::Win32::Foundation::CloseHandle(parent_write);
            }
            return Err(error).context("could not protect parent-liveness writer inheritance");
        }
        command.env(PARENT_LIVENESS_ENV, (child_read as usize).to_string());
        Ok(Self {
            child_read: child_read as isize,
            parent_write: parent_write as isize,
        })
    }

    fn child_spawned(&mut self) {
        if self.child_read != 0 {
            unsafe {
                windows_sys::Win32::Foundation::CloseHandle(self.child_read as _);
            }
            self.child_read = 0;
        }
    }

    fn request_cancel(&self) -> Result<bool> {
        use windows_sys::Win32::Storage::FileSystem::WriteFile;

        if self.parent_write == 0 {
            return Ok(false);
        }
        let mut written = 0_u32;
        let byte = [PARENT_CONTROL_CANCEL];
        if unsafe {
            WriteFile(
                self.parent_write as _,
                byte.as_ptr().cast(),
                1,
                &mut written,
                std::ptr::null_mut(),
            )
        } == 0
        {
            return Err(std::io::Error::last_os_error())
                .context("could not request cooperative worker cancellation");
        }
        Ok(written == 1)
    }
}

#[cfg(windows)]
impl Drop for ParentLivenessChannel {
    fn drop(&mut self) {
        for handle in [&mut self.child_read, &mut self.parent_write] {
            if *handle != 0 {
                unsafe {
                    windows_sys::Win32::Foundation::CloseHandle(*handle as _);
                }
                *handle = 0;
            }
        }
    }
}

#[cfg(unix)]
struct ParentLivenessChannel {
    child_read: i32,
    parent_write: i32,
}

#[cfg(unix)]
impl ParentLivenessChannel {
    fn attach(command: &mut Command) -> Result<Self> {
        let mut descriptors = [0_i32; 2];
        if unsafe { libc::pipe(descriptors.as_mut_ptr()) } == -1 {
            return Err(std::io::Error::last_os_error())
                .context("could not create parent-liveness pipe");
        }
        let child_read = descriptors[0];
        let parent_write = descriptors[1];
        if unsafe { libc::fcntl(parent_write, libc::F_SETFD, libc::FD_CLOEXEC) } == -1
            || unsafe { libc::fcntl(child_read, libc::F_SETFD, 0) } == -1
        {
            let error = std::io::Error::last_os_error();
            unsafe {
                libc::close(child_read);
                libc::close(parent_write);
            }
            return Err(error).context("could not configure parent-liveness descriptors");
        }
        command.env(PARENT_LIVENESS_ENV, child_read.to_string());
        Ok(Self {
            child_read,
            parent_write,
        })
    }

    fn child_spawned(&mut self) {
        if self.child_read >= 0 {
            unsafe {
                libc::close(self.child_read);
            }
            self.child_read = -1;
        }
    }

    fn request_cancel(&self) -> Result<bool> {
        if self.parent_write < 0 {
            return Ok(false);
        }
        let byte = [PARENT_CONTROL_CANCEL];
        let written = unsafe { libc::write(self.parent_write, byte.as_ptr().cast(), byte.len()) };
        if written == -1 {
            return Err(std::io::Error::last_os_error())
                .context("could not request cooperative worker cancellation");
        }
        Ok(written == 1)
    }
}

#[cfg(unix)]
impl Drop for ParentLivenessChannel {
    fn drop(&mut self) {
        for descriptor in [&mut self.child_read, &mut self.parent_write] {
            if *descriptor >= 0 {
                unsafe {
                    libc::close(*descriptor);
                }
                *descriptor = -1;
            }
        }
    }
}

fn take_parent_control_reader_from_env() -> Result<Option<std::fs::File>> {
    let Some(raw) = std::env::var_os(PARENT_LIVENESS_ENV) else {
        if cfg!(test) {
            return Ok(None);
        }
        bail!("private worker did not receive its parent-liveness descriptor");
    };
    unsafe {
        std::env::remove_var(PARENT_LIVENESS_ENV);
    }
    let raw = raw
        .to_str()
        .ok_or_else(|| anyhow!("parent-liveness descriptor is not valid UTF-8"))?;

    #[cfg(windows)]
    let reader = {
        use std::os::windows::io::FromRawHandle;
        let handle = raw
            .parse::<usize>()
            .context("parent-liveness handle is invalid")?;
        if handle == 0 {
            bail!("parent-liveness handle must be non-zero");
        }
        unsafe { std::fs::File::from_raw_handle(handle as _) }
    };
    #[cfg(unix)]
    let reader = {
        use std::os::unix::io::FromRawFd;
        let descriptor = raw
            .parse::<i32>()
            .context("parent-liveness descriptor is invalid")?;
        if descriptor < 0 {
            bail!("parent-liveness descriptor must be non-negative");
        }
        unsafe { std::fs::File::from_raw_fd(descriptor) }
    };

    Ok(Some(reader))
}

fn start_parent_control_watchdog(
    mut reader: std::fs::File,
    runtime_router: Option<RuntimeRouter>,
) -> Result<()> {
    std::thread::Builder::new()
        .name("scribe-parent-liveness".to_owned())
        .spawn(move || {
            let mut byte = [0_u8; 1];
            loop {
                match reader.read(&mut byte) {
                    Ok(0) => std::process::exit(1),
                    Ok(_) if byte[0] == PARENT_CONTROL_CANCEL => {
                        if let Some(router) = &runtime_router {
                            router.cancel_active();
                        }
                    }
                    Ok(_) => std::process::exit(1),
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(_) => std::process::exit(1),
                }
            }
        })
        .context("could not start parent-liveness watchdog")?;
    Ok(())
}

fn configure_hidden_worker_command(command: &mut Command) {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Threading::{CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW};
        command.creation_flags(CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP);
    }
    #[cfg(unix)]
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

#[cfg(windows)]
struct ProcessTreeGuard(isize);

#[cfg(windows)]
unsafe impl Send for ProcessTreeGuard {}
#[cfg(windows)]
unsafe impl Sync for ProcessTreeGuard {}

#[cfg(windows)]
impl Drop for ProcessTreeGuard {
    fn drop(&mut self) {
        if self.0 != 0 {
            unsafe {
                windows_sys::Win32::Foundation::CloseHandle(self.0 as _);
            }
        }
    }
}

#[cfg(unix)]
struct ProcessTreeGuard;

fn bind_worker_process_tree(child: &Child) -> Result<ProcessTreeGuard> {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
            SetInformationJobObject,
        };

        let job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if job.is_null() {
            return Err(std::io::Error::last_os_error())
                .context("could not create worker job object");
        }
        let mut limits = unsafe { std::mem::zeroed::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() };
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let configured = unsafe {
            SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        };
        let assigned = configured != 0
            && unsafe { AssignProcessToJobObject(job, child.as_raw_handle() as _) } != 0;
        if !assigned {
            let error = std::io::Error::last_os_error();
            unsafe { windows_sys::Win32::Foundation::CloseHandle(job) };
            return Err(error).context("could not bind worker to its kill-on-close job object");
        }
        Ok(ProcessTreeGuard(job as isize))
    }
    #[cfg(unix)]
    {
        let _ = child;
        Ok(ProcessTreeGuard)
    }
}

struct OsWorkerProcess {
    child: Mutex<Child>,
    _process_guard: ProcessTreeGuard,
    _parent_liveness: ParentLivenessChannel,
}

impl WorkerProcess for OsWorkerProcess {
    fn is_running(&self) -> Result<bool> {
        Ok(self
            .child
            .lock()
            .map_err(|_| anyhow!("process worker process lock was poisoned"))?
            .try_wait()?
            .is_none())
    }

    fn request_cooperative_cancel(&self) -> Result<bool> {
        self._parent_liveness.request_cancel()
    }

    fn terminate(&self) -> Result<()> {
        let mut child = self
            .child
            .lock()
            .map_err(|_| anyhow!("process worker process lock was poisoned"))?;
        #[cfg(unix)]
        if child.try_wait()?.is_none() {
            let process_group = -(child.id() as i32);
            let result = unsafe { libc::kill(process_group, libc::SIGKILL) };
            if result == -1 {
                let error = std::io::Error::last_os_error();
                if error.raw_os_error() != Some(libc::ESRCH) {
                    return Err(anyhow!(
                        "could not terminate inference worker process group: {error}"
                    ));
                }
            }
            return Ok(());
        }
        #[cfg(not(unix))]
        if child.try_wait()?.is_none()
            && let Err(kill_error) = child.kill()
            && child.try_wait()?.is_none()
        {
            return Err(anyhow!(
                "could not terminate inference worker: {kill_error}"
            ));
        }
        Ok(())
    }

    fn wait(&self) -> Result<()> {
        let _ = self
            .child
            .lock()
            .map_err(|_| anyhow!("process worker process lock was poisoned"))?
            .wait()?;
        Ok(())
    }
}

struct WriterSlot {
    generation: u64,
    stdin: Box<dyn Write + Send>,
}

struct CurrentGeneration {
    generation: u64,
    process: Arc<dyn WorkerProcess>,
    expectation: WorkerExpectation,
    pack_launch: Option<PackLaunchContext>,
    pack_bindings: Vec<VerifiedPackLaunchBinding>,
}

struct WorkerGenerationContext {
    generation: u64,
    pack_bindings: Vec<VerifiedPackLaunchBinding>,
}

#[derive(Clone, Copy)]
struct SupervisorStream {
    generation: u64,
    session_id: u64,
    last_request_id: u64,
}

#[derive(Default)]
struct SupervisorState {
    next_generation: u64,
    current: Option<CurrentGeneration>,
    retiring_generations: BTreeSet<u64>,
    active_request: Option<Correlation>,
    active_stream: Option<SupervisorStream>,
    active_model: Option<(u64, String)>,
}

struct SupervisorInner {
    // Locking rule: spawn_gate is the sole outer lock during generation startup;
    // no other path acquires it. State, writer, and pending are otherwise taken
    // sequentially, never nested. Invalidation uses writer.try_lock so process
    // termination never depends on pipe progress.
    launcher: Arc<dyn WorkerLauncher>,
    deadlines: SupervisorDeadlines,
    spawn_gate: Mutex<()>,
    retirement_changed: Condvar,
    state: Mutex<SupervisorState>,
    writer: Mutex<Option<WriterSlot>>,
    pending: Mutex<HashMap<Correlation, SyncSender<PendingResult>>>,
}

/// Cloneable parent-side worker supervisor. Request threads block only on their
/// own correlated reply channel. A dedicated reader owns each generation's
/// stdout, and stdin is locked only while a complete logical request is emitted.
#[derive(Clone)]
pub(crate) struct ProcessWorkerSupervisor {
    inner: Arc<SupervisorInner>,
}

impl ProcessWorkerSupervisor {
    #[cfg(test)]
    fn with_launcher(launcher: Arc<dyn WorkerLauncher>) -> Result<Self> {
        Self::with_launcher_and_deadlines(launcher, SupervisorDeadlines::default())
    }

    #[cfg(test)]
    fn with_launcher_and_deadlines(
        launcher: Arc<dyn WorkerLauncher>,
        deadlines: SupervisorDeadlines,
    ) -> Result<Self> {
        let supervisor = Self::unstarted_with_launcher_and_deadlines(launcher, deadlines);
        supervisor.ensure_generation()?;
        Ok(supervisor)
    }

    fn unstarted_with_launcher_and_deadlines(
        launcher: Arc<dyn WorkerLauncher>,
        deadlines: SupervisorDeadlines,
    ) -> Self {
        Self::unstarted_for_role_with_launcher_and_deadlines(
            WorkerRole::Inference,
            launcher,
            deadlines,
        )
    }

    fn unstarted_for_role_with_launcher_and_deadlines(
        _role: WorkerRole,
        launcher: Arc<dyn WorkerLauncher>,
        deadlines: SupervisorDeadlines,
    ) -> Self {
        Self {
            inner: Arc::new(SupervisorInner {
                launcher,
                deadlines,
                spawn_gate: Mutex::new(()),
                retirement_changed: Condvar::new(),
                state: Mutex::new(SupervisorState::default()),
                writer: Mutex::new(None),
                pending: Mutex::new(HashMap::new()),
            }),
        }
    }

    #[cfg(test)]
    pub(crate) fn start_stream(&self, session_id: u64, request_id: u64) -> Result<()> {
        let generation = self.ensure_generation()?;
        if self
            .inner
            .state
            .lock()
            .map_err(|_| anyhow!("process worker supervisor state lock was poisoned"))?
            .active_stream
            .is_some()
        {
            bail!("a worker stream is already active");
        }
        let frame = control_frame(session_id, request_id, &Control::StartStream)?;
        match self.active_round_trip(generation, session_id, request_id, &[frame])? {
            Control::Ok => {
                let mut state =
                    self.inner.state.lock().map_err(|_| {
                        anyhow!("process worker supervisor state lock was poisoned")
                    })?;
                if state
                    .current
                    .as_ref()
                    .is_none_or(|current| current.generation != generation)
                {
                    bail!("process worker generation {generation} is unavailable");
                }
                state.active_stream = Some(SupervisorStream {
                    generation,
                    session_id,
                    last_request_id: request_id,
                });
                Ok(())
            }
            Control::Error { message } => bail!("process worker: {message}"),
            _ => {
                self.invalidate_generation(
                    generation,
                    "unexpected process worker start-stream response",
                    true,
                )?;
                bail!("unexpected process worker start-stream response")
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn audio_chunk(
        &self,
        session_id: u64,
        request_id: u64,
        samples: &[f32],
    ) -> Result<String> {
        let pcm = encode_pcm(samples)?;
        let generation = self.require_stream(session_id)?.generation;
        let frames = [
            control_frame(session_id, request_id, &Control::AudioChunk)?,
            Frame {
                kind: FrameKind::Pcm,
                session_id,
                request_id,
                body: pcm,
            },
        ];
        match self.active_round_trip(generation, session_id, request_id, &frames)? {
            Control::Text {
                text,
                final_result: false,
            } => {
                if let Ok(mut state) = self.inner.state.lock()
                    && let Some(stream) = state.active_stream.as_mut()
                    && stream.generation == generation
                    && stream.session_id == session_id
                {
                    stream.last_request_id = request_id;
                }
                Ok(text)
            }
            Control::Error { message } => {
                self.clear_stream(generation, session_id);
                bail!("process worker: {message}")
            }
            _ => {
                self.invalidate_generation(
                    generation,
                    "unexpected process worker audio-chunk response",
                    true,
                )?;
                bail!("unexpected process worker audio-chunk response")
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn end_stream(&self, session_id: u64, request_id: u64) -> Result<String> {
        let generation = self.require_stream(session_id)?.generation;
        let frame = control_frame(session_id, request_id, &Control::EndStream)?;
        let response = self.active_round_trip(generation, session_id, request_id, &[frame]);
        self.clear_stream(generation, session_id);
        match response? {
            Control::Text {
                text,
                final_result: true,
            } => Ok(text),
            Control::Error { message } => bail!("process worker: {message}"),
            _ => {
                self.invalidate_generation(
                    generation,
                    "unexpected process worker end-stream response",
                    true,
                )?;
                bail!("unexpected process worker end-stream response")
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn cancel_stream(&self, session_id: u64, request_id: u64) -> Result<()> {
        self.cancel_stream_with_timeout(session_id, request_id, self.inner.deadlines.cancel)
    }

    fn cancel_stream_with_timeout(
        &self,
        session_id: u64,
        request_id: u64,
        timeout: Duration,
    ) -> Result<()> {
        let (stream, active_request) = {
            let state = self
                .inner
                .state
                .lock()
                .map_err(|_| anyhow!("process worker supervisor state lock was poisoned"))?;
            let stream = state
                .active_stream
                .filter(|stream| stream.session_id == session_id)
                .ok_or_else(|| anyhow!("no worker stream is active for session {session_id}"))?;
            (stream, state.active_request)
        };
        if let Some(active_request) = active_request {
            if active_request.generation == stream.generation
                && active_request.session_id == stream.session_id
            {
                return self.cancel_active();
            }
            bail!("another worker request is active");
        }
        self.round_trip_on_generation(
            stream.generation,
            session_id,
            request_id,
            Control::Cancel {
                target_session_id: stream.session_id,
                target_request_id: stream.last_request_id,
            },
            timeout,
        )?;
        self.clear_stream(stream.generation, stream.session_id);
        Ok(())
    }

    pub(crate) fn health(&self, session_id: u64, request_id: u64) -> Result<()> {
        let generation = self.ensure_generation()?;
        let frame = control_frame(session_id, request_id, &Control::Health)?;
        self.active_round_trip_with_timeout(
            generation,
            session_id,
            request_id,
            &[frame],
            self.inner.deadlines.health,
        )
        .and_then(expect_ok)
    }

    pub(crate) fn unload(&self) -> Result<()> {
        let Some(generation) = self.current_generation()? else {
            return Ok(());
        };
        self.round_trip_on_generation(
            generation,
            0,
            0,
            Control::Unload,
            self.inner.deadlines.control,
        )?;
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| anyhow!("process worker supervisor state lock was poisoned"))?;
        if state
            .current
            .as_ref()
            .is_some_and(|current| current.generation == generation)
        {
            state.active_model = None;
            state.active_stream = None;
        }
        Ok(())
    }

    fn current_generation(&self) -> Result<Option<u64>> {
        Ok(self
            .inner
            .state
            .lock()
            .map_err(|_| anyhow!("process worker supervisor state lock was poisoned"))?
            .current
            .as_ref()
            .map(|current| current.generation))
    }

    fn has_active_stream(&self) -> Result<bool> {
        Ok(self
            .inner
            .state
            .lock()
            .map_err(|_| anyhow!("process worker supervisor state lock was poisoned"))?
            .active_stream
            .is_some())
    }

    /// Requests lock-free worker-local cancellation through the independent
    /// parent-control pipe, then waits only through the bounded cancel deadline.
    /// Runtimes without a cooperative primitive, failed control writes, and
    /// unacknowledged requests fall back to generation invalidation plus owned
    /// process-tree kill/reap. No request is replayed after either outcome.
    pub(crate) fn cancel_active(&self) -> Result<()> {
        self.cancel_active_outcome().map(|_| ())
    }

    fn cancel_active_outcome(&self) -> Result<CancelOutcome> {
        let (target, process) = {
            let state = self
                .inner
                .state
                .lock()
                .map_err(|_| anyhow!("process worker supervisor state lock was poisoned"))?;
            let Some(target) = state.active_request else {
                return Ok(CancelOutcome::NoActiveRequest);
            };
            let Some(current) = state.current.as_ref() else {
                return Ok(CancelOutcome::NoActiveRequest);
            };
            if current.generation != target.generation {
                return Ok(CancelOutcome::NoActiveRequest);
            }
            (target, Arc::clone(&current.process))
        };

        if matches!(process.request_cooperative_cancel(), Ok(true)) {
            let deadline = Instant::now()
                .checked_add(self.inner.deadlines.cancel)
                .ok_or_else(|| anyhow!("worker cancellation deadline overflowed"))?;
            loop {
                let still_active = self
                    .inner
                    .state
                    .lock()
                    .map_err(|_| anyhow!("process worker supervisor state lock was poisoned"))?
                    .active_request
                    == Some(target);
                if !still_active {
                    return Ok(CancelOutcome::CooperativeSettled);
                }
                if Instant::now() >= deadline {
                    break;
                }
                std::thread::sleep(Duration::from_millis(2));
            }
        }

        let waiter = self
            .inner
            .pending
            .lock()
            .map_err(|_| anyhow!("process worker pending map lock was poisoned"))?
            .remove(&target);
        if let Some(waiter) = waiter {
            let _ = waiter.send(Err("process worker request was cancelled".to_owned()));
        } else {
            // The stdout reader already claimed the response. Treat that as
            // completion winning the race rather than killing a healthy
            // generation after its request has completed.
            return Ok(CancelOutcome::CooperativeSettled);
        }
        if let Ok(mut state) = self.inner.state.lock()
            && state.active_request == Some(target)
        {
            state.active_request = None;
        }
        self.invalidate_generation(target.generation, "process worker request cancelled", true)?;
        Ok(CancelOutcome::HardInvalidated)
    }

    /// Abandons a stream without waiting for child I/O. Used only by RAII drop
    /// paths, where blocking can deadlock application teardown.
    pub(crate) fn abandon_stream(&self, session_id: u64) {
        let generation = self.inner.state.lock().ok().and_then(|state| {
            state
                .active_stream
                .filter(|stream| stream.session_id == session_id)
                .map(|stream| stream.generation)
        });
        if let Some(generation) = generation
            && let Err(error) =
                self.invalidate_generation(generation, "ONNX stream was abandoned", true)
        {
            eprintln!("could not retire abandoned ONNX stream generation: {error:#}");
        }
    }

    /// Retires the current generation without depending on which operation
    /// failed. This is the router's fail-closed boundary for alternate and test
    /// supervisor implementations whose `load` errors do not invalidate their
    /// own process generation.
    pub(crate) fn terminate_current(&self) -> Result<()> {
        let generation = self
            .inner
            .state
            .lock()
            .map_err(|_| anyhow!("process worker supervisor state lock was poisoned"))?
            .current
            .as_ref()
            .map(|current| current.generation);
        if let Some(generation) = generation {
            self.invalidate_generation(
                generation,
                "process worker generation was explicitly retired",
                true,
            )?;
        }
        Ok(())
    }

    fn ensure_generation(&self) -> Result<u64> {
        self.ensure_generation_before(None, None)
    }

    fn ensure_generation_before(
        &self,
        deadline: Option<MonotonicDeadline>,
        cancelled: Option<&AtomicBool>,
    ) -> Result<u64> {
        let _spawn_guard = self
            .inner
            .spawn_gate
            .lock()
            .map_err(|_| anyhow!("process worker spawn lock was poisoned"))?;
        let existing = {
            let state = self
                .inner
                .state
                .lock()
                .map_err(|_| anyhow!("process worker supervisor state lock was poisoned"))?;
            state
                .current
                .as_ref()
                .map(|current| (current.generation, Arc::clone(&current.process)))
        };
        if let Some((generation, process)) = existing {
            match process.is_running() {
                Ok(true) => return Ok(generation),
                Ok(false) => {
                    self.invalidate_generation(generation, "process worker exited", false)?;
                }
                Err(error) => {
                    self.invalidate_generation(
                        generation,
                        "could not inspect process worker process",
                        true,
                    )?;
                    return Err(anyhow!("could not inspect process worker process: {error}"));
                }
            }
        }

        let spawned = match deadline {
            Some(deadline) => self.launch_before(deadline, cancelled)?,
            None => self.inner.launcher.launch()?,
        };
        let SpawnedWorker {
            stdin,
            stdout,
            process,
            expectation,
            pack_launch,
        } = spawned;
        let generation = {
            let mut state = self
                .inner
                .state
                .lock()
                .map_err(|_| anyhow!("process worker supervisor state lock was poisoned"))?;
            state.next_generation = state.next_generation.saturating_add(1).max(1);
            let generation = state.next_generation;
            state.current = Some(CurrentGeneration {
                generation,
                process: Arc::clone(&process),
                expectation: expectation.clone(),
                pack_launch,
                pack_bindings: Vec::new(),
            });
            state.active_stream = None;
            state.active_model = None;
            generation
        };
        *self
            .inner
            .writer
            .lock()
            .map_err(|_| anyhow!("process worker writer lock was poisoned"))? =
            Some(WriterSlot { generation, stdin });
        if let Err(error) = Self::start_reader(&self.inner, generation, stdout) {
            self.invalidate_generation(generation, &error.to_string(), true)?;
            return Err(error);
        }
        let hello_timeout = match deadline {
            Some(deadline) => match deadline.remaining() {
                Ok(remaining) => remaining,
                Err(error) => {
                    self.invalidate_generation(
                        generation,
                        &format!("{} deadline expired before Hello", deadline.label),
                        true,
                    )?;
                    return Err(error);
                }
            },
            None => self.inner.deadlines.hello,
        };
        let challenge = match random_challenge() {
            Ok(challenge) => challenge,
            Err(error) => {
                self.invalidate_generation(
                    generation,
                    "could not create process worker handshake challenge",
                    true,
                )?;
                return Err(error);
            }
        };
        let expected = expectation;
        if let Err(error) = self.round_trip_on_generation_with_cancellation(
            generation,
            0,
            0,
            Control::Hello {
                challenge,
                expected,
            },
            hello_timeout,
            cancelled,
        ) {
            self.invalidate_generation(generation, &error.to_string(), true)?;
            return Err(error);
        }
        Ok(generation)
    }

    fn launch_before(
        &self,
        deadline: MonotonicDeadline,
        cancelled: Option<&AtomicBool>,
    ) -> Result<SpawnedWorker> {
        let launcher = Arc::clone(&self.inner.launcher);
        let (result_tx, result_rx) = sync_channel(1);
        std::thread::Builder::new()
            .name("scribe-process-worker-launch".to_owned())
            .spawn(move || {
                let result = launcher.launch();
                if deadline.remaining().is_err() {
                    if let Ok(worker) = result {
                        retire_unpublished_worker_synchronously(worker);
                    }
                    return;
                }
                if let Err(send_error) = result_tx.send(result)
                    && let Ok(worker) = send_error.0
                {
                    retire_unpublished_worker_synchronously(worker);
                }
            })
            .context("could not start bounded process worker launcher")?;

        loop {
            if cancelled.is_some_and(|flag| flag.load(Ordering::Acquire)) {
                bail!("Silero VAD acquisition was cancelled");
            }
            let remaining = deadline.remaining()?;
            match result_rx.recv_timeout(remaining.min(Duration::from_millis(10))) {
                Ok(result) => {
                    let worker = result?;
                    if cancelled.is_some_and(|flag| flag.load(Ordering::Acquire)) {
                        retire_unpublished_worker(worker);
                        bail!("Silero VAD acquisition was cancelled");
                    }
                    if let Err(error) = deadline.remaining() {
                        retire_unpublished_worker(worker);
                        return Err(error);
                    }
                    return Ok(worker);
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    deadline.remaining()?;
                    bail!("process worker launcher disconnected")
                }
            }
        }
    }

    fn start_reader(
        inner: &Arc<SupervisorInner>,
        generation: u64,
        mut stdout: Box<dyn Read + Send>,
    ) -> Result<()> {
        let weak = Arc::downgrade(inner);
        std::thread::Builder::new()
            .name(format!("scribe-process-worker-reader-{generation}"))
            .spawn(move || {
                loop {
                    let response = read_frame(&mut stdout).and_then(parse_worker_control);
                    let Some(inner) = Weak::upgrade(&weak) else {
                        break;
                    };
                    let (session_id, request_id, control) = match response {
                        Ok(response) => response,
                        Err(error) => {
                            if let Err(retire_error) = ProcessWorkerSupervisor::from_inner(inner)
                                .invalidate_generation(
                                generation,
                                &format!("process worker stdout failed: {error}"),
                                true,
                            ) {
                                eprintln!(
                                    "could not retire failed process worker generation {generation}: {retire_error:#}"
                                );
                            }
                            break;
                        }
                    };
                    let correlation = Correlation {
                        generation,
                        session_id,
                        request_id,
                    };
                    let waiter = match inner.pending.lock() {
                        Ok(mut pending) => pending.remove(&correlation),
                        Err(_) => None,
                    };
                    if let Some(waiter) = waiter {
                        // Release the reader's transient strong supervisor
                        // reference before waking the request thread. If that
                        // request owns the last public supervisor handle, its
                        // subsequent drop must synchronously reach shutdown.
                        drop(inner);
                        let _ = waiter.send(Ok(control));
                        continue;
                    }
                    if let Err(error) = ProcessWorkerSupervisor::from_inner(inner)
                        .invalidate_generation(
                        generation,
                        "stale or mis-correlated process worker response",
                        true,
                    ) {
                        eprintln!(
                            "could not retire mis-correlated process worker generation {generation}: {error:#}"
                        );
                    }
                    break;
                }
            })
            .map(|_| ())
            .map_err(|error| anyhow!("could not start process worker stdout reader: {error}"))
    }

    fn from_inner(inner: Arc<SupervisorInner>) -> Self {
        Self { inner }
    }

    fn register(&self, correlation: Correlation) -> Result<Receiver<PendingResult>> {
        let (reply, response) = sync_channel(1);
        let mut pending = self
            .inner
            .pending
            .lock()
            .map_err(|_| anyhow!("process worker pending map lock was poisoned"))?;
        match pending.entry(correlation) {
            Entry::Vacant(entry) => {
                entry.insert(reply);
            }
            Entry::Occupied(_) => {
                bail!(
                    "duplicate process worker request correlation for generation {}, session {}, request {}",
                    correlation.generation,
                    correlation.session_id,
                    correlation.request_id
                );
            }
        }
        drop(pending);
        let current = match self.inner.state.lock() {
            Ok(state) => state
                .current
                .as_ref()
                .is_some_and(|current| current.generation == correlation.generation),
            Err(_) => {
                self.unregister(correlation);
                bail!("process worker supervisor state lock was poisoned");
            }
        };
        if !current {
            self.unregister(correlation);
            bail!(
                "process worker generation {} is unavailable",
                correlation.generation
            );
        }
        Ok(response)
    }

    fn unregister(&self, correlation: Correlation) {
        if let Ok(mut pending) = self.inner.pending.lock() {
            pending.remove(&correlation);
        }
    }

    fn active_round_trip(
        &self,
        generation: u64,
        session_id: u64,
        request_id: u64,
        frames: &[Frame],
    ) -> Result<Control> {
        self.active_round_trip_with_timeout(
            generation,
            session_id,
            request_id,
            frames,
            self.inner.deadlines.data,
        )
    }

    fn active_round_trip_with_timeout(
        &self,
        generation: u64,
        session_id: u64,
        request_id: u64,
        frames: &[Frame],
        timeout: Duration,
    ) -> Result<Control> {
        self.active_round_trip_with_timeout_and_cancellation(
            generation, session_id, request_id, frames, timeout, None,
        )
    }

    fn active_round_trip_with_timeout_and_cancellation(
        &self,
        generation: u64,
        session_id: u64,
        request_id: u64,
        frames: &[Frame],
        timeout: Duration,
        cancelled: Option<&AtomicBool>,
    ) -> Result<Control> {
        let correlation = Correlation {
            generation,
            session_id,
            request_id,
        };
        let response = self.register(correlation)?;
        let active_error = match self.inner.state.lock() {
            Ok(mut state)
                if state
                    .current
                    .as_ref()
                    .is_some_and(|current| current.generation == generation) =>
            {
                if state.active_request.is_some() {
                    Some(anyhow!("a process worker request is already active"))
                } else {
                    state.active_request = Some(correlation);
                    None
                }
            }
            Ok(_) => Some(anyhow!(
                "process worker generation {generation} is unavailable"
            )),
            Err(_) => Some(anyhow!("process worker supervisor state lock was poisoned")),
        };
        if let Some(error) = active_error {
            self.unregister(correlation);
            return Err(error);
        }
        if let Err(error) = self.write_frames(generation, frames) {
            self.unregister(correlation);
            self.invalidate_generation(generation, &error.to_string(), true)?;
            return Err(error);
        }
        let result =
            self.await_response_with_cancellation(correlation, response, timeout, cancelled);
        self.clear_active(correlation);
        result
    }

    fn require_stream(&self, session_id: u64) -> Result<SupervisorStream> {
        self.inner
            .state
            .lock()
            .map_err(|_| anyhow!("process worker supervisor state lock was poisoned"))?
            .active_stream
            .filter(|stream| stream.session_id == session_id)
            .ok_or_else(|| anyhow!("no worker stream is active for session {session_id}"))
    }

    fn clear_stream(&self, generation: u64, session_id: u64) {
        if let Ok(mut state) = self.inner.state.lock()
            && state.active_stream.is_some_and(|stream| {
                stream.generation == generation && stream.session_id == session_id
            })
        {
            state.active_stream = None;
        }
    }

    fn round_trip_on_generation(
        &self,
        generation: u64,
        session_id: u64,
        request_id: u64,
        command: Control,
        timeout: Duration,
    ) -> Result<()> {
        self.round_trip_on_generation_with_cancellation(
            generation, session_id, request_id, command, timeout, None,
        )
    }

    fn round_trip_on_generation_with_cancellation(
        &self,
        generation: u64,
        session_id: u64,
        request_id: u64,
        command: Control,
        timeout: Duration,
        cancelled: Option<&AtomicBool>,
    ) -> Result<()> {
        if !command.is_parent_command() {
            bail!("cannot send a response-only control to the process worker");
        }
        let hello = match &command {
            Control::Hello {
                challenge,
                expected,
            } => Some((challenge.clone(), expected.clone())),
            _ => None,
        };
        let correlation = Correlation {
            generation,
            session_id,
            request_id,
        };
        let response = self.register(correlation)?;
        let frame = match control_frame(session_id, request_id, &command) {
            Ok(frame) => frame,
            Err(error) => {
                self.unregister(correlation);
                return Err(error);
            }
        };
        if let Err(error) = self.write_frames(generation, &[frame]) {
            self.unregister(correlation);
            self.invalidate_generation(generation, &error.to_string(), true)?;
            return Err(error);
        }
        match self.await_response_with_cancellation(correlation, response, timeout, cancelled)? {
            Control::Ready { capability } if hello.is_some() => {
                let (challenge, expected) = hello.expect("checked above");
                validate_worker_capability(&capability, &challenge, &expected)?;
                self.bind_generation_pack_capability(generation, &expected, &capability)
            }
            Control::Ok if hello.is_none() => Ok(()),
            Control::Error { message } => bail!("process worker: {message}"),
            _ => {
                self.invalidate_generation(
                    generation,
                    "unexpected process worker control response",
                    true,
                )?;
                bail!("unexpected process worker control response")
            }
        }
    }

    fn bind_generation_pack_capability(
        &self,
        generation: u64,
        expected: &WorkerExpectation,
        capability: &WorkerCapability,
    ) -> Result<()> {
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| anyhow!("process worker supervisor state lock was poisoned"))?;
        let current = state
            .current
            .as_mut()
            .filter(|current| current.generation == generation)
            .ok_or_else(|| anyhow!("process worker generation changed during Hello"))?;
        if current.expectation != *expected {
            bail!("process worker launch expectation changed during Hello");
        }
        match (&current.pack_launch, &capability.pack) {
            (None, None) => Ok(()),
            (Some(context), Some(pack)) => {
                current.pack_bindings = bind_pack_hello(context, pack)?;
                Ok(())
            }
            _ => bail!("process worker pack capability did not match launch authority"),
        }
    }

    fn generation_context(&self) -> Result<WorkerGenerationContext> {
        let generation = self.ensure_generation()?;
        Ok(WorkerGenerationContext {
            generation,
            pack_bindings: self.pack_bindings_for_generation(generation)?,
        })
    }

    fn pack_bindings_for_generation(
        &self,
        generation: u64,
    ) -> Result<Vec<VerifiedPackLaunchBinding>> {
        self.inner
            .state
            .lock()
            .map_err(|_| anyhow!("process worker supervisor state lock was poisoned"))?
            .current
            .as_ref()
            .filter(|current| current.generation == generation)
            .map(|current| current.pack_bindings.clone())
            .ok_or_else(|| anyhow!("process worker generation {generation} is unavailable"))
    }

    fn write_frames(&self, generation: u64, frames: &[Frame]) -> Result<()> {
        let mut writer = self
            .inner
            .writer
            .lock()
            .map_err(|_| anyhow!("process worker writer lock was poisoned"))?;
        let slot = writer
            .as_mut()
            .filter(|slot| slot.generation == generation)
            .ok_or_else(|| anyhow!("process worker generation {generation} is unavailable"))?;
        for frame in frames {
            write_frame(&mut slot.stdin, frame)?;
        }
        Ok(())
    }

    fn await_response_with_cancellation(
        &self,
        correlation: Correlation,
        response: Receiver<PendingResult>,
        timeout: Duration,
        cancelled: Option<&AtomicBool>,
    ) -> Result<Control> {
        let deadline = Instant::now()
            .checked_add(timeout)
            .ok_or_else(|| anyhow!("process worker response deadline overflowed"))?;
        loop {
            if cancelled.is_some_and(|flag| flag.load(Ordering::Acquire)) {
                self.unregister(correlation);
                self.invalidate_generation(
                    correlation.generation,
                    "Silero VAD acquisition was cancelled",
                    true,
                )?;
                bail!("Silero VAD acquisition was cancelled");
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                self.unregister(correlation);
                self.invalidate_generation(
                    correlation.generation,
                    "process worker response deadline exceeded",
                    true,
                )?;
                bail!(
                    "process worker response deadline exceeded after {} ms",
                    timeout.as_millis()
                );
            }
            let wait = if cancelled.is_some() {
                remaining.min(Duration::from_millis(10))
            } else {
                remaining
            };
            match response.recv_timeout(wait) {
                Ok(result) => return result.map_err(anyhow::Error::msg),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    bail!("process worker response channel disconnected")
                }
            }
        }
    }

    fn clear_active(&self, correlation: Correlation) {
        if let Ok(mut state) = self.inner.state.lock()
            && state.active_request == Some(correlation)
        {
            state.active_request = None;
        }
    }

    fn invalidate_generation(&self, generation: u64, reason: &str, force_kill: bool) -> Result<()> {
        let process = {
            let mut state = self
                .inner
                .state
                .lock()
                .map_err(|_| anyhow!("process worker supervisor state lock was poisoned"))?;
            loop {
                let Some(current) = state.current.as_ref() else {
                    return Ok(());
                };
                if current.generation != generation {
                    return Ok(());
                }
                let process = Arc::clone(&current.process);
                if state.retiring_generations.insert(generation) {
                    break process;
                }
                let (next_state, timeout) = self
                    .inner
                    .retirement_changed
                    .wait_timeout(state, self.inner.deadlines.cancel)
                    .map_err(|_| anyhow!("process worker supervisor state lock was poisoned"))?;
                state = next_state;
                if timeout.timed_out() && state.retiring_generations.contains(&generation) {
                    bail!(
                        "process worker generation {generation} termination did not complete within {} ms",
                        self.inner.deadlines.cancel.as_millis()
                    );
                }
            }
        };
        if force_kill && let Err(error) = process.terminate() {
            if let Ok(mut state) = self.inner.state.lock() {
                state.retiring_generations.remove(&generation);
            }
            self.inner.retirement_changed.notify_all();
            return Err(error).with_context(|| {
                format!("could not initiate termination of process worker generation {generation}")
            });
        }
        {
            let mut state = self
                .inner
                .state
                .lock()
                .map_err(|_| anyhow!("process worker supervisor state lock was poisoned"))?;
            let Some(current) = state.current.as_ref() else {
                state.retiring_generations.remove(&generation);
                self.inner.retirement_changed.notify_all();
                return Ok(());
            };
            if current.generation != generation {
                state.retiring_generations.remove(&generation);
                self.inner.retirement_changed.notify_all();
                return Ok(());
            }
            state.current = None;
            state.active_request = None;
            state.active_stream = None;
            state.active_model = None;
            state.retiring_generations.remove(&generation);
        }
        self.inner.retirement_changed.notify_all();
        let failed = if let Ok(mut pending) = self.inner.pending.lock() {
            let correlations = pending
                .keys()
                .copied()
                .filter(|correlation| correlation.generation == generation)
                .collect::<Vec<_>>();
            correlations
                .into_iter()
                .filter_map(|correlation| pending.remove(&correlation))
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        for waiter in failed {
            let _ = waiter.send(Err(reason.to_owned()));
        }
        reap_process(process, generation)?;
        // Invalidation, especially cancellation, must never wait for a pipe
        // writer that is blocked in an OS write. Killing the process releases
        // that writer; a later generation replaces the stale slot. Clear it
        // eagerly only when the mutex is immediately available.
        if let Ok(mut writer) = self.inner.writer.try_lock()
            && writer
                .as_ref()
                .is_some_and(|slot| slot.generation == generation)
        {
            *writer = None;
        }
        Ok(())
    }

    #[cfg(test)]
    fn abandon_generation_for_test(&self, generation: u64, reason: &str) {
        let process = {
            let mut state = self.inner.state.lock().unwrap();
            let current = state.current.take().unwrap();
            assert_eq!(current.generation, generation);
            state.active_request = None;
            state.active_stream = None;
            state.active_model = None;
            current.process
        };
        self.inner.writer.lock().unwrap().take();
        let failed = {
            let mut pending = self.inner.pending.lock().unwrap();
            let keys = pending
                .keys()
                .copied()
                .filter(|key| key.generation == generation)
                .collect::<Vec<_>>();
            keys.into_iter()
                .filter_map(|key| pending.remove(&key))
                .collect::<Vec<_>>()
        };
        for waiter in failed {
            let _ = waiter.send(Err(reason.to_owned()));
        }
        drop(process);
    }

    #[cfg(test)]
    fn generation_for_test(&self) -> u64 {
        self.inner
            .state
            .lock()
            .unwrap()
            .current
            .as_ref()
            .unwrap()
            .generation
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct SileroVadDecision {
    pub(crate) probability: f32,
    pub(crate) speech: bool,
}

/// Dedicated VAD-only supervisor. Each instance owns a separate hidden worker
/// process and cannot submit transcription commands through this API.
#[derive(Clone)]
pub(crate) struct SileroVadWorkerSupervisor {
    transport: ProcessWorkerSupervisor,
    deadlines: VadDeadlines,
}

impl SileroVadWorkerSupervisor {
    /// Starts a dedicated worker and establishes a ready VAD session within one
    /// aggregate monotonic budget. The returned request id is the first id that
    /// may be used for a window; ids before it belong to acquisition controls.
    pub(crate) fn acquire_session(
        session_id: u64,
        first_request_id: u64,
        num_threads: u16,
        threshold: VadThreshold,
        cancelled: &AtomicBool,
    ) -> Result<(Self, u64)> {
        Self::acquire_session_with_launcher_and_deadlines(
            Arc::new(OsWorkerLauncher::vad()),
            session_id,
            first_request_id,
            num_threads,
            threshold,
            VadDeadlines::default(),
            cancelled,
        )
    }

    fn acquire_session_with_launcher_and_deadlines(
        launcher: Arc<dyn WorkerLauncher>,
        session_id: u64,
        first_request_id: u64,
        num_threads: u16,
        threshold: VadThreshold,
        deadlines: VadDeadlines,
        cancelled: &AtomicBool,
    ) -> Result<(Self, u64)> {
        let deadline = MonotonicDeadline::after(deadlines.acquisition)?;
        let transport_deadlines = SupervisorDeadlines {
            hello: deadlines.acquisition,
            cancel: deadlines.operation,
            ..SupervisorDeadlines::default()
        };
        let supervisor = Self {
            transport: ProcessWorkerSupervisor::unstarted_for_role_with_launcher_and_deadlines(
                WorkerRole::Vad,
                launcher,
                transport_deadlines,
            ),
            deadlines,
        };
        let generation = supervisor
            .transport
            .ensure_generation_before(Some(deadline), Some(cancelled))?;
        let mut request_id = first_request_id;
        supervisor.load_on_generation(
            generation,
            session_id,
            request_id,
            num_threads,
            supervisor.acquisition_remaining(deadline, generation, "load")?,
            Some(cancelled),
        )?;
        request_id = request_id.wrapping_add(1).max(1);
        supervisor.health_on_generation(
            generation,
            session_id,
            request_id,
            supervisor.acquisition_remaining(deadline, generation, "health check")?,
            Some(cancelled),
        )?;
        request_id = request_id.wrapping_add(1).max(1);
        supervisor.start_session_on_generation(
            generation,
            session_id,
            request_id,
            threshold,
            supervisor.acquisition_remaining(deadline, generation, "session start")?,
            Some(cancelled),
        )?;
        request_id = request_id.wrapping_add(1).max(1);
        supervisor.health_on_generation(
            generation,
            session_id,
            request_id,
            supervisor.acquisition_remaining(deadline, generation, "readiness check")?,
            Some(cancelled),
        )?;
        Ok((supervisor, request_id.wrapping_add(1).max(1)))
    }

    fn acquisition_remaining(
        &self,
        deadline: MonotonicDeadline,
        generation: u64,
        stage: &str,
    ) -> Result<Duration> {
        match deadline.remaining() {
            Ok(remaining) => Ok(remaining),
            Err(error) => {
                self.transport.invalidate_generation(
                    generation,
                    &format!("Silero VAD acquisition deadline expired before {stage}"),
                    true,
                )?;
                Err(error)
            }
        }
    }

    #[cfg(test)]
    fn with_transport(transport: ProcessWorkerSupervisor) -> Self {
        Self {
            transport,
            deadlines: VadDeadlines::default(),
        }
    }

    #[cfg(test)]
    fn with_transport_and_deadlines(
        transport: ProcessWorkerSupervisor,
        deadlines: VadDeadlines,
    ) -> Self {
        Self {
            transport,
            deadlines,
        }
    }

    #[cfg(test)]
    pub(crate) fn load(&self, session_id: u64, request_id: u64, num_threads: u16) -> Result<bool> {
        let generation = self.transport.ensure_generation()?;
        self.load_on_generation(
            generation,
            session_id,
            request_id,
            num_threads,
            self.deadlines.acquisition,
            None,
        )
    }

    fn load_on_generation(
        &self,
        generation: u64,
        session_id: u64,
        request_id: u64,
        num_threads: u16,
        timeout: Duration,
        cancelled: Option<&AtomicBool>,
    ) -> Result<bool> {
        if num_threads == 0 || num_threads > 64 {
            bail!("Silero VAD thread count must be within [1, 64]");
        }
        let identity = format!(
            "silero-vad:{}:{num_threads}",
            crate::support_assets::SILERO_VAD_SHA256
        );
        let reused = self
            .transport
            .inner
            .state
            .lock()
            .map_err(|_| anyhow!("process worker supervisor state lock was poisoned"))?
            .active_model
            .as_ref()
            .is_some_and(|(loaded_generation, loaded_identity)| {
                *loaded_generation == generation && loaded_identity == &identity
            });
        if reused {
            return Ok(true);
        }
        let frame = control_frame(session_id, request_id, &Control::LoadVad { num_threads })?;
        if let Err(error) = self
            .transport
            .active_round_trip_with_timeout_and_cancellation(
                generation,
                session_id,
                request_id,
                &[frame],
                timeout,
                cancelled,
            )
            .and_then(expect_ok)
        {
            self.transport
                .invalidate_generation(generation, "Silero VAD load failed", true)?;
            return Err(error);
        }
        let mut state = self
            .transport
            .inner
            .state
            .lock()
            .map_err(|_| anyhow!("process worker supervisor state lock was poisoned"))?;
        if state
            .current
            .as_ref()
            .is_some_and(|current| current.generation == generation)
        {
            state.active_model = Some((generation, identity));
        }
        Ok(false)
    }

    #[cfg(test)]
    pub(crate) fn start_session(
        &self,
        session_id: u64,
        request_id: u64,
        threshold: VadThreshold,
    ) -> Result<()> {
        let generation = self.transport.ensure_generation()?;
        self.start_session_on_generation(
            generation,
            session_id,
            request_id,
            threshold,
            self.deadlines.acquisition,
            None,
        )
    }

    pub(crate) fn start_session_with_cancellation(
        &self,
        session_id: u64,
        request_id: u64,
        threshold: VadThreshold,
        cancelled: &AtomicBool,
    ) -> Result<()> {
        let generation = self.transport.ensure_generation()?;
        self.start_session_on_generation(
            generation,
            session_id,
            request_id,
            threshold,
            self.deadlines.acquisition,
            Some(cancelled),
        )
    }

    fn start_session_on_generation(
        &self,
        generation: u64,
        session_id: u64,
        request_id: u64,
        threshold: VadThreshold,
        timeout: Duration,
        cancelled: Option<&AtomicBool>,
    ) -> Result<()> {
        if self
            .transport
            .inner
            .state
            .lock()
            .map_err(|_| anyhow!("process worker supervisor state lock was poisoned"))?
            .active_stream
            .is_some()
        {
            bail!("a Silero VAD session is already active");
        }
        let frame = control_frame(
            session_id,
            request_id,
            &Control::StartVad {
                threshold: threshold.value(),
            },
        )?;
        let response = match self
            .transport
            .active_round_trip_with_timeout_and_cancellation(
                generation,
                session_id,
                request_id,
                &[frame],
                timeout,
                cancelled,
            ) {
            Ok(response) => response,
            Err(error) => {
                self.transport.invalidate_generation(
                    generation,
                    "Silero VAD start failed",
                    true,
                )?;
                return Err(error);
            }
        };
        match response {
            Control::Ok => {
                let mut state =
                    self.transport.inner.state.lock().map_err(|_| {
                        anyhow!("process worker supervisor state lock was poisoned")
                    })?;
                if state
                    .current
                    .as_ref()
                    .is_none_or(|current| current.generation != generation)
                {
                    bail!("Silero VAD worker generation {generation} is unavailable");
                }
                state.active_stream = Some(SupervisorStream {
                    generation,
                    session_id,
                    last_request_id: request_id,
                });
                Ok(())
            }
            Control::Error { message } => {
                self.transport.invalidate_generation(
                    generation,
                    "Silero VAD start failed",
                    true,
                )?;
                bail!("Silero VAD worker: {message}")
            }
            _ => {
                self.transport.invalidate_generation(
                    generation,
                    "unexpected Silero VAD start response",
                    true,
                )?;
                bail!("unexpected Silero VAD start response")
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn compute(
        &self,
        session_id: u64,
        request_id: u64,
        samples: &[f32],
    ) -> Result<SileroVadDecision> {
        self.compute_with_cancellation(session_id, request_id, samples, None)
    }

    pub(crate) fn compute_with_cancellation(
        &self,
        session_id: u64,
        request_id: u64,
        samples: &[f32],
        cancelled: Option<&AtomicBool>,
    ) -> Result<SileroVadDecision> {
        if samples.len() != WINDOW_SAMPLES {
            bail!("Silero VAD input must contain exactly {WINDOW_SAMPLES} samples");
        }
        let pcm = encode_pcm(samples)?;
        let stream = self.transport.require_stream(session_id)?;
        let generation = stream.generation;
        let frames = [
            control_frame(session_id, request_id, &Control::VadWindow)?,
            Frame {
                kind: FrameKind::Pcm,
                session_id,
                request_id,
                body: pcm,
            },
        ];
        let response = match self
            .transport
            .active_round_trip_with_timeout_and_cancellation(
                generation,
                session_id,
                request_id,
                &frames,
                self.deadlines.operation,
                cancelled,
            ) {
            Ok(response) => response,
            Err(error) => {
                self.retire_failed_session(stream, "Silero VAD compute transport failed")?;
                return Err(error);
            }
        };
        match response {
            Control::VadDecision {
                probability,
                speech,
            } if probability.is_finite() && (0.0..=1.0).contains(&probability) => {
                if let Ok(mut state) = self.transport.inner.state.lock()
                    && let Some(active) = state.active_stream.as_mut()
                    && active.generation == generation
                    && active.session_id == session_id
                {
                    active.last_request_id = request_id;
                }
                Ok(SileroVadDecision {
                    probability,
                    speech,
                })
            }
            Control::Error { message } => {
                self.retire_failed_session(stream, "Silero VAD compute failed")?;
                bail!("Silero VAD worker: {message}")
            }
            _ => {
                self.transport.invalidate_generation(
                    generation,
                    "unexpected Silero VAD decision response",
                    true,
                )?;
                bail!("unexpected Silero VAD decision response")
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn reset(&self, session_id: u64, request_id: u64) -> Result<()> {
        let stream = self.transport.require_stream(session_id)?;
        let result = self.transport.round_trip_on_generation(
            stream.generation,
            session_id,
            request_id,
            Control::ResetVad,
            self.deadlines.operation,
        );
        if let Err(error) = result {
            self.retire_failed_session(stream, "Silero VAD reset failed")?;
            return Err(error);
        }
        if let Ok(mut state) = self.transport.inner.state.lock()
            && let Some(active) = state.active_stream.as_mut()
            && active.generation == stream.generation
            && active.session_id == session_id
        {
            active.last_request_id = request_id;
        }
        Ok(())
    }

    pub(crate) fn end_session(&self, session_id: u64, request_id: u64) -> Result<()> {
        let stream = self.transport.require_stream(session_id)?;
        let result = self.transport.round_trip_on_generation(
            stream.generation,
            session_id,
            request_id,
            Control::EndVad,
            self.deadlines.operation,
        );
        self.transport
            .clear_stream(stream.generation, stream.session_id);
        if let Err(error) = result {
            self.transport.invalidate_generation(
                stream.generation,
                "Silero VAD end failed",
                true,
            )?;
            return Err(error);
        }
        Ok(())
    }

    pub(crate) fn cancel_session(&self, session_id: u64, request_id: u64) -> Result<()> {
        let stream = self.transport.require_stream(session_id)?;
        if let Err(error) = self.transport.cancel_stream_with_timeout(
            session_id,
            request_id,
            self.deadlines.operation,
        ) {
            self.retire_failed_session(stream, "Silero VAD cancel failed")?;
            return Err(error);
        }
        Ok(())
    }

    pub(crate) fn abandon_session(&self, session_id: u64) {
        self.transport.abandon_stream(session_id);
    }

    #[cfg(test)]
    pub(crate) fn health(&self, session_id: u64, request_id: u64) -> Result<()> {
        let generation = self.transport.ensure_generation()?;
        self.health_on_generation(
            generation,
            session_id,
            request_id,
            self.deadlines.acquisition,
            None,
        )
    }

    fn health_on_generation(
        &self,
        generation: u64,
        session_id: u64,
        request_id: u64,
        timeout: Duration,
        cancelled: Option<&AtomicBool>,
    ) -> Result<()> {
        let frame = control_frame(session_id, request_id, &Control::Health)?;
        let result = self
            .transport
            .active_round_trip_with_timeout_and_cancellation(
                generation,
                session_id,
                request_id,
                &[frame],
                timeout,
                cancelled,
            )
            .and_then(expect_ok);
        if let Err(error) = result {
            self.transport.invalidate_generation(
                generation,
                "Silero VAD health check failed",
                true,
            )?;
            return Err(error);
        }
        Ok(())
    }

    fn retire_failed_session(&self, stream: SupervisorStream, reason: &str) -> Result<()> {
        self.transport
            .clear_stream(stream.generation, stream.session_id);
        self.transport
            .invalidate_generation(stream.generation, reason, true)
    }
}

fn expect_ok(response: Control) -> Result<()> {
    match response {
        Control::Ok => Ok(()),
        Control::Error { message } => bail!("process worker: {message}"),
        _ => bail!("unexpected process worker control response"),
    }
}

fn retire_unpublished_worker(worker: SpawnedWorker) {
    let SpawnedWorker {
        stdin,
        stdout,
        process,
        ..
    } = worker;
    drop(stdin);
    drop(stdout);
    if let Err(error) = process.terminate() {
        eprintln!("could not terminate late process worker launch: {error:#}");
    }
    if let Err(error) = reap_process(process, 0) {
        eprintln!("could not reap late process worker launch: {error:#}");
    }
}

fn retire_unpublished_worker_synchronously(worker: SpawnedWorker) {
    let SpawnedWorker {
        stdin,
        stdout,
        process,
        ..
    } = worker;
    drop(stdin);
    drop(stdout);
    if let Err(error) = process.terminate() {
        eprintln!("could not terminate late process worker launch: {error:#}");
    }
    if let Err(error) = process.wait() {
        eprintln!("could not reap late process worker launch: {error:#}");
    }
}

fn reap_process(process: Arc<dyn WorkerProcess>, generation: u64) -> Result<()> {
    // Termination, when needed, has already been initiated by the caller.
    // Waiting happens on a generation-local thread, so an indefinitely stalled
    // wait can never delay termination of a later generation.
    let reaper_process = Arc::clone(&process);
    if let Err(error) = std::thread::Builder::new()
        .name(format!("scribe-process-worker-reaper-{generation}"))
        .spawn(move || {
            if let Err(error) = reaper_process.wait() {
                eprintln!("process worker generation {generation} reaper failed: {error:#}");
            }
        })
    {
        // Thread creation failure is exceptional, but the child must still be
        // reaped before this process can safely forget it.
        process.wait().with_context(|| {
            format!(
                "could not start process worker generation {generation} reaper ({error}) and synchronous reaping failed"
            )
        })?;
    }
    Ok(())
}

impl Drop for SupervisorInner {
    fn drop(&mut self) {
        self.writer
            .get_mut()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        if let Some(current) = self
            .state
            .get_mut()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .current
            .take()
            && let Err(error) = current
                .process
                .terminate()
                .and_then(|()| current.process.wait())
        {
            eprintln!("process worker shutdown failed: {error:#}");
        }
    }
}

/// Dispatches only the VAD role retained in the desktop executable.
pub(crate) fn maybe_run_vad_worker() -> Option<i32> {
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    let role = match worker_role_from_args(&args) {
        Ok(Some(WorkerRole::Vad)) => WorkerRole::Vad,
        Ok(Some(WorkerRole::Inference)) => {
            eprintln!("the inference role is available only in scribe-inference-worker");
            return Some(2);
        }
        Ok(None) => return None,
        Err(error) => {
            eprintln!("invalid private Scribe worker invocation: {error}");
            return Some(2);
        }
    };
    let parent_control = match take_parent_control_reader_from_env() {
        Ok(reader) => reader,
        Err(error) => {
            eprintln!("Scribe {role:?} worker liveness setup failed: {error:#}");
            return Some(1);
        }
    };
    Some(
        match worker_loop_for_role(
            std::io::stdin().lock(),
            std::io::stdout().lock(),
            role,
            parent_control,
        ) {
            Ok(()) => 0,
            Err(error) => {
                eprintln!("Scribe {role:?} worker failed: {error:#}");
                1
            }
        },
    )
}

/// Entrypoint substrate for the separately packaged inference executable.
///
/// The worker-only binary supplies the native recognizer factory. Keeping that
/// factory out of this module prevents an all-features desktop build from
/// compiling ASR recognizer/server code into the UI executable.
#[allow(
    dead_code,
    reason = "the shared worker module exposes this only to the dedicated inference-server binary"
)]
pub(crate) fn run_inference_worker_with_factory<F: WorkerRecognizerFactory>(factory: &F) -> i32 {
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    match worker_role_from_args(&args) {
        Ok(Some(WorkerRole::Inference)) => {}
        Ok(Some(WorkerRole::Vad)) => {
            eprintln!("the dedicated inference executable cannot run the VAD role");
            return 2;
        }
        Ok(None) => {
            eprintln!("the dedicated inference executable requires {INFERENCE_WORKER_FLAG}");
            return 2;
        }
        Err(error) => {
            eprintln!("invalid private Scribe inference worker invocation: {error}");
            return 2;
        }
    }
    let parent_control = match take_parent_control_reader_from_env() {
        Ok(reader) => reader,
        Err(error) => {
            eprintln!("Scribe inference worker liveness setup failed: {error:#}");
            return 1;
        }
    };
    match worker_loop_with_factories(
        std::io::stdin().lock(),
        std::io::stdout().lock(),
        factory,
        &NativeVadFactory,
        Some(WorkerRole::Inference),
        parent_control,
    ) {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("Scribe inference worker failed: {error:#}");
            1
        }
    }
}

fn worker_role_from_args(args: &[std::ffi::OsString]) -> Result<Option<WorkerRole>> {
    let known_flag_present = args
        .iter()
        .any(|arg| arg == INFERENCE_WORKER_FLAG || arg == VAD_WORKER_FLAG);
    let private_worker_shape_present = args.iter().any(|arg| {
        let value = arg.to_string_lossy();
        value == "--onnx-worker" || (value.starts_with("--scribe-") && value.ends_with("-worker"))
    });
    match args {
        [arg] if arg == INFERENCE_WORKER_FLAG => Ok(Some(WorkerRole::Inference)),
        [arg] if arg == VAD_WORKER_FLAG => Ok(Some(WorkerRole::Vad)),
        _ if known_flag_present => {
            bail!("worker flags are mutually exclusive and accept no additional arguments")
        }
        _ if private_worker_shape_present => bail!("unknown private Scribe worker role"),
        _ => Ok(None),
    }
}

/// Generic parent-side facade for the one process-isolated STT generation.
/// It owns no native model/session/recognizer objects.
#[derive(Clone)]
pub(crate) struct InferenceWorkerSupervisor {
    transport: ProcessWorkerSupervisor,
    next_correlation: Arc<std::sync::atomic::AtomicU64>,
}

impl InferenceWorkerSupervisor {
    pub(crate) fn unstarted() -> Self {
        Self {
            transport: ProcessWorkerSupervisor::unstarted_with_launcher_and_deadlines(
                Arc::new(OsWorkerLauncher::inference()),
                SupervisorDeadlines::default(),
            ),
            next_correlation: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        }
    }

    fn for_pack_binding(binding: VerifiedPackLaunchBinding) -> Self {
        Self {
            transport: ProcessWorkerSupervisor::unstarted_with_launcher_and_deadlines(
                Arc::new(OsWorkerLauncher::for_pack_binding(binding)),
                SupervisorDeadlines::default(),
            ),
            next_correlation: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        }
    }

    fn for_pack_probe(lease: Arc<VerifiedPackLease>) -> Self {
        Self {
            transport: ProcessWorkerSupervisor::unstarted_with_launcher_and_deadlines(
                Arc::new(OsWorkerLauncher::for_pack_probe(lease)),
                SupervisorDeadlines::default(),
            ),
            next_correlation: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        }
    }

    #[cfg(test)]
    fn verified_pack_bindings(&self) -> Result<Vec<VerifiedPackLaunchBinding>> {
        let bindings = self.transport.generation_context()?.pack_bindings;
        if bindings.is_empty() {
            bail!("inference worker did not produce a verified pack binding");
        }
        Ok(bindings)
    }

    fn verified_pack_bindings_before(
        &self,
        deadline: MonotonicDeadline,
    ) -> Result<Vec<VerifiedPackLaunchBinding>> {
        let generation = self
            .transport
            .ensure_generation_before(Some(deadline), None)?;
        let bindings = self.transport.pack_bindings_for_generation(generation)?;
        if bindings.is_empty() {
            bail!("inference worker did not produce a verified pack binding");
        }
        Ok(bindings)
    }

    fn reconcile_verified_pack_diagnostics(
        bindings: &[VerifiedPackLaunchBinding],
        resolved: &mut ResolvedAcceleration,
    ) -> Result<(), RuntimeError> {
        if bindings.is_empty() {
            return Ok(());
        }
        if bindings.len() != 1 {
            return Err(RuntimeError::WorkerUnavailable(
                "active GPU worker produced an ambiguous device binding".to_owned(),
            ));
        }
        let expected = bindings[0].backend_target();
        reconcile_verified_pack_target(resolved, expected)
    }

    /// Test-only process launcher for diagnostics that must exercise the real
    /// hidden worker role from a separately built Scribe executable. Production
    /// construction remains pinned to `current_exe()` and cannot be redirected
    /// by environment or configuration.
    #[cfg(test)]
    pub(crate) fn unstarted_for_executable(executable: PathBuf) -> Self {
        Self {
            transport: ProcessWorkerSupervisor::unstarted_with_launcher_and_deadlines(
                Arc::new(OsWorkerLauncher::for_executable(
                    WorkerRole::Inference,
                    executable,
                )),
                SupervisorDeadlines::default(),
            ),
            next_correlation: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        }
    }

    fn next_id(&self) -> u64 {
        self.next_correlation
            .fetch_add(1, Ordering::AcqRel)
            .wrapping_add(1)
            .max(1)
    }

    pub(crate) fn load(
        &self,
        artifact: RuntimeArtifact,
        preference: AccelerationPreference,
    ) -> Result<RuntimeLoadExecution, RuntimeError> {
        if matches!(artifact, RuntimeArtifact::OnnxBundle(_)) {
            resolve_cpu_only_acceleration(preference)
                .map_err(|error| RuntimeError::OnnxUnavailable(error.to_string()))?;
        }
        let context = self
            .transport
            .generation_context()
            .map_err(worker_unavailable)?;
        let correlation = self.next_id();
        let frame = control_frame(
            correlation,
            correlation,
            &Control::LoadRuntime {
                artifact: artifact.into(),
                preference,
            },
        )
        .map_err(worker_unavailable)?;
        match self
            .transport
            .active_round_trip_with_timeout(
                context.generation,
                correlation,
                correlation,
                &[frame],
                self.transport.inner.deadlines.load,
            )
            .map_err(worker_unavailable)?
        {
            Control::RuntimeLoaded { execution } => {
                let mut execution: RuntimeLoadExecution = execution.into();
                Self::reconcile_verified_pack_diagnostics(
                    &context.pack_bindings,
                    &mut execution.diagnostics.resolved_acceleration,
                )?;
                Ok(execution)
            }
            Control::RuntimeFailed { error } => {
                Err(error.into_runtime_for_generation(&self.transport, context.generation))
            }
            Control::Error { message } => Err(RuntimeError::WorkerUnavailable(message)),
            _ => {
                let _ = self.transport.invalidate_generation(
                    context.generation,
                    "unexpected inference load response",
                    true,
                );
                Err(RuntimeError::WorkerUnavailable(
                    "unexpected inference load response".to_owned(),
                ))
            }
        }
    }

    pub(crate) fn transcribe(
        &self,
        artifact: RuntimeArtifact,
        preference: AccelerationPreference,
        audio: &PreparedAudio,
        options: TranscriptionOptions,
        cancellation_snapshot: u64,
        cancellation_generation: &std::sync::atomic::AtomicU64,
    ) -> Result<RuntimeExecution, RuntimeError> {
        let cancelled = || cancellation_generation.load(Ordering::Acquire) != cancellation_snapshot;
        if cancelled() {
            return Err(RuntimeError::Cancelled(
                "transcription request was cancelled before inference dispatch".to_owned(),
            ));
        }
        validate_cumulative_pcm_samples(&audio.samples)
            .map_err(|error| RuntimeError::Engine(error.to_string()))?;
        let context = self
            .transport
            .generation_context()
            .map_err(worker_unavailable)?;
        let generation = context.generation;
        if cancelled() {
            let _ = self.transport.invalidate_generation(
                generation,
                "inference request cancelled before batch begin",
                true,
            );
            return Err(RuntimeError::Cancelled(
                "transcription request was cancelled".to_owned(),
            ));
        }
        let session_id = self.next_id();
        let begin_id = self.next_id();
        let begin = control_frame(
            session_id,
            begin_id,
            &Control::BeginBatch {
                artifact: artifact.into(),
                preference,
                options,
                source_sample_rate: audio.source_sample_rate,
                source_channels: audio.source_channels,
                source_frames: audio.source_frames,
                declared_samples: audio.samples.len(),
            },
        )
        .map_err(worker_unavailable)?;
        match self
            .transport
            .active_round_trip_with_timeout(
                generation,
                session_id,
                begin_id,
                &[begin],
                self.transport.inner.deadlines.load,
            )
            .map_err(worker_unavailable)?
        {
            Control::Ok => {}
            Control::RuntimeFailed { error } => {
                return Err(error.into_runtime_for_generation(&self.transport, generation));
            }
            Control::Error { message } => return Err(RuntimeError::WorkerUnavailable(message)),
            _ => {
                let _ = self.transport.invalidate_generation(
                    generation,
                    "unexpected inference batch-begin response",
                    true,
                );
                return Err(RuntimeError::WorkerUnavailable(
                    "unexpected inference batch-begin response".to_owned(),
                ));
            }
        }

        const CHUNK_SAMPLES: usize = 256 * 1024;
        for samples in audio.samples.chunks(CHUNK_SAMPLES) {
            if cancelled() {
                let _ = self.transport.invalidate_generation(
                    generation,
                    "inference request cancelled while sending batch audio",
                    true,
                );
                return Err(RuntimeError::Cancelled(
                    "transcription request was cancelled".to_owned(),
                ));
            }
            let request_id = self.next_id();
            let frames = [
                control_frame(session_id, request_id, &Control::AudioChunk)
                    .map_err(worker_unavailable)?,
                Frame {
                    kind: FrameKind::Pcm,
                    session_id,
                    request_id,
                    body: encode_pcm(samples).map_err(worker_unavailable)?,
                },
            ];
            expect_ok(
                self.transport
                    .active_round_trip(generation, session_id, request_id, &frames)
                    .map_err(worker_unavailable)?,
            )
            .map_err(worker_unavailable)?;
        }
        if cancelled() {
            let _ = self.transport.invalidate_generation(
                generation,
                "inference request cancelled before batch decode",
                true,
            );
            return Err(RuntimeError::Cancelled(
                "transcription request was cancelled".to_owned(),
            ));
        }
        let end_id = self.next_id();
        let end =
            control_frame(session_id, end_id, &Control::EndBatch).map_err(worker_unavailable)?;
        match self
            .transport
            .active_round_trip(generation, session_id, end_id, &[end])
            .map_err(worker_unavailable)?
        {
            Control::RuntimeTranscript { execution } => {
                let mut execution: RuntimeExecution = execution.into();
                Self::reconcile_verified_pack_diagnostics(
                    &context.pack_bindings,
                    &mut execution.diagnostics.resolved_acceleration,
                )?;
                Ok(execution)
            }
            Control::RuntimeFailed { error } => {
                Err(error.into_runtime_for_generation(&self.transport, generation))
            }
            Control::Error { message } => Err(RuntimeError::WorkerUnavailable(message)),
            _ => {
                let _ = self.transport.invalidate_generation(
                    generation,
                    "unexpected inference batch response",
                    true,
                );
                Err(RuntimeError::WorkerUnavailable(
                    "unexpected inference batch response".to_owned(),
                ))
            }
        }
    }

    pub(crate) fn health(
        &self,
        artifact: RuntimeArtifact,
        preference: AccelerationPreference,
    ) -> Result<(), RuntimeError> {
        self.load(artifact, preference)?;
        let id = self.next_id();
        self.transport.health(id, id).map_err(worker_unavailable)
    }

    pub(crate) fn unload(&self) -> Result<(), RuntimeError> {
        self.transport.unload().map_err(worker_unavailable)
    }

    pub(crate) fn unload_if_idle(&self) -> Result<(), RuntimeError> {
        if self
            .transport
            .has_active_stream()
            .map_err(worker_unavailable)?
        {
            return Ok(());
        }
        self.unload()
    }

    fn retire(&self) -> Result<(), RuntimeError> {
        self.transport
            .terminate_current()
            .map_err(worker_unavailable)
    }

    pub(crate) fn cancel_active(&self) {
        let outcome = self.transport.cancel_active_outcome();
        if matches!(
            outcome,
            Ok(CancelOutcome::CooperativeSettled | CancelOutcome::HardInvalidated)
        ) {
            return;
        }
        // If cancellation landed between two correlated batch requests there
        // may have been no active waiter for cancel_active() to retire. Kill
        // the still-current generation as well so that the next batch frame
        // cannot cross the cancellation boundary.
        if let Ok(Some(generation)) = self.transport.current_generation() {
            let _ = self.transport.invalidate_generation(
                generation,
                "inference batch cancelled between requests",
                true,
            );
        }
    }

    pub(crate) fn shutdown(&self) -> Result<(), RuntimeError> {
        let generation = self
            .transport
            .inner
            .state
            .lock()
            .map_err(|_| RuntimeError::WorkerUnavailable("inference state lock poisoned".into()))?
            .current
            .as_ref()
            .map(|current| current.generation);
        if let Some(generation) = generation {
            self.transport
                .round_trip_on_generation(
                    generation,
                    0,
                    self.next_id(),
                    Control::Shutdown,
                    self.transport.inner.deadlines.control,
                )
                .map_err(worker_unavailable)?;
            self.transport
                .invalidate_generation(generation, "inference worker shut down", false)
                .map_err(worker_unavailable)?;
        }
        Ok(())
    }
}

#[allow(
    dead_code,
    reason = "the client-side worker adapter is excluded from the dedicated inference-worker target"
)]
fn worker_unavailable(error: impl std::fmt::Display) -> RuntimeError {
    RuntimeError::RetryableWorkerFailure(error.to_string())
}

fn reconcile_verified_pack_target(
    resolved: &mut ResolvedAcceleration,
    expected: BackendTarget,
) -> Result<(), RuntimeError> {
    let selection = resolved.selection.as_mut().ok_or_else(|| {
        RuntimeError::WorkerUnavailable(
            "GPU worker diagnostics omitted the typed backend selection".to_owned(),
        )
    })?;
    let observed = &selection.target;
    let mut mismatches = Vec::new();
    for (matches, field) in [
        (observed.backend == expected.backend, "backend"),
        (observed.provider_id == expected.provider_id, "provider"),
        (observed.device_id == expected.device_id, "device identity"),
        (
            observed.driver_version == expected.driver_version,
            "driver identity",
        ),
        (
            observed.device_class == expected.device_class,
            "device class",
        ),
        (observed.vendor == expected.vendor, "vendor"),
        (
            observed.memory_total_bytes == expected.memory_total_bytes,
            "total memory",
        ),
    ] {
        if !matches {
            mismatches.push(field);
        }
    }
    if !mismatches.is_empty() {
        return Err(RuntimeError::WorkerUnavailable(format!(
            "GPU worker runtime diagnostics differ from the verified launch binding: {}",
            mismatches.join(", ")
        )));
    }
    // BackendTarget deliberately excludes the volatile provider index from
    // serde so it can never be persisted as device identity. The current
    // index was already challenge-bound and validated in the fresh Hello;
    // restore that resolver-owned value when projecting private diagnostics.
    debug_assert!(observed.process_index.is_none());
    selection.target = expected;
    Ok(())
}

#[derive(Clone)]
#[allow(
    dead_code,
    reason = "the client-side route is excluded from the dedicated inference-worker target"
)]
struct InferenceWorkerRoute {
    provider: WorkerProvider,
    supervisor: InferenceWorkerSupervisor,
    target: Option<BackendTarget>,
    health_key: Option<HealthKey>,
    consume_retry_bypass: bool,
    auto_evidence_id: Option<String>,
}

#[derive(Debug)]
enum RoutePreparationError {
    DeviceRollbackAuthority(RuntimeError),
    Fatal(RuntimeError),
}

#[derive(Clone)]
struct InferenceWorkerRoutePlan {
    routes: Arc<Vec<InferenceWorkerRoute>>,
    diagnostic: Option<String>,
    health: Option<GpuRouteHealth>,
    selection_power_source: Option<PowerSource>,
    skipped_targets: Vec<SkippedBackend>,
}

#[derive(Clone)]
struct VerifiedGpuRouteCatalog {
    routes: Arc<Vec<InferenceWorkerRoute>>,
    diagnostic: Option<String>,
    device_set_digest: String,
    discovery_fingerprint: String,
    provider_probe_incomplete: bool,
    skipped_targets: Vec<SkippedBackend>,
}

const GPU_PROVIDER_PROBE_BACKOFF: Duration = Duration::from_secs(15);
const MAX_INFERENCE_ROUTE_ATTEMPTS: usize = 4;

#[derive(Clone)]
struct FailedGpuProbe {
    discovery_fingerprint: String,
    retry_not_before: Instant,
    diagnostic: Option<String>,
    explicit_retry_available: bool,
}

#[derive(Default)]
struct GpuRouteCatalogCache {
    successful: Option<VerifiedGpuRouteCatalog>,
    failed: Option<FailedGpuProbe>,
}

enum GpuRouteCatalogLookup {
    Successful(VerifiedGpuRouteCatalog),
    Backoff(Option<String>),
    Probe,
}

impl GpuRouteCatalogCache {
    fn lookup(&self, discovery_fingerprint: &str, now: Instant) -> GpuRouteCatalogLookup {
        let successful = self.successful.as_ref().filter(|catalog| {
            !catalog.routes.is_empty() && catalog.discovery_fingerprint == discovery_fingerprint
        });
        if let Some(failed) = self
            .failed
            .as_ref()
            .filter(|failed| failed.discovery_fingerprint == discovery_fingerprint)
        {
            if now >= failed.retry_not_before {
                return GpuRouteCatalogLookup::Probe;
            }
            return successful.map_or_else(
                || GpuRouteCatalogLookup::Backoff(failed.diagnostic.clone()),
                |catalog| GpuRouteCatalogLookup::Successful(catalog.clone()),
            );
        }
        if let Some(catalog) = successful {
            return GpuRouteCatalogLookup::Successful(catalog.clone());
        }
        GpuRouteCatalogLookup::Probe
    }

    fn record_probe(&mut self, catalog: VerifiedGpuRouteCatalog, now: Instant) {
        let routes_empty = catalog.routes.is_empty();
        let retry_required = routes_empty || catalog.provider_probe_incomplete;
        self.failed = retry_required.then(|| FailedGpuProbe {
            discovery_fingerprint: catalog.discovery_fingerprint.clone(),
            retry_not_before: now + GPU_PROVIDER_PROBE_BACKOFF,
            diagnostic: catalog.diagnostic.clone(),
            explicit_retry_available: true,
        });
        if routes_empty {
            self.successful = None;
        } else {
            self.successful = Some(catalog);
        }
    }

    fn grant_explicit_probe_retry(&mut self, discovery_fingerprint: &str, now: Instant) -> bool {
        let Some(failed) = self.failed.as_mut().filter(|failed| {
            failed.discovery_fingerprint == discovery_fingerprint && failed.explicit_retry_available
        }) else {
            return false;
        };
        failed.explicit_retry_available = false;
        failed.retry_not_before = now;
        true
    }

    fn successful(&self) -> Option<&VerifiedGpuRouteCatalog> {
        self.successful.as_ref()
    }

    fn invalidate_after_runtime_failure(&mut self) {
        self.successful = None;
        self.failed = None;
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct InferenceRouteIdentity {
    provider: WorkerProvider,
    backend: BackendKind,
    stable_device_identity: String,
    driver_version: Option<String>,
    pack_digest: Option<String>,
}

#[derive(Clone)]
struct ActiveInferenceRoute {
    identity: InferenceRouteIdentity,
    supervisor: InferenceWorkerSupervisor,
}

#[derive(Clone)]
struct GpuRouteHealth {
    path: Arc<PathBuf>,
    witnesses: HealthWitnesses,
}

static GPU_HEALTH_CLOCK: SystemClock = SystemClock;

impl GpuRouteHealth {
    fn cache(&self) -> HealthCache<'static> {
        HealthCache::open(&*self.path, self.witnesses.clone(), &GPU_HEALTH_CLOCK)
    }

    fn consume_retry(&self, route: &InferenceWorkerRoute) -> bool {
        if !route.consume_retry_bypass {
            return true;
        }
        let Some(key) = route.health_key.as_ref() else {
            return false;
        };
        self.cache().consume_explicit_retry(key).unwrap_or(false)
    }

    fn record_failure(&self, route: &InferenceWorkerRoute, observation: FailureObservation) {
        let Some(key) = route.health_key.clone() else {
            return;
        };
        let _ = self.cache().record_observed_failure(key, observation);
    }

    fn record_idle_probe_success(&self, route: &InferenceWorkerRoute) {
        let Some(key) = route.health_key.as_ref() else {
            return;
        };
        let _ = self.cache().record_idle_probe_success(key);
    }

    #[allow(
        dead_code,
        reason = "Stage 5 exposes the existing internal explicit-retry action in the UI"
    )]
    fn grant_explicit_retry(&self, key: &HealthKey) -> bool {
        self.cache().grant_explicit_retry(key).unwrap_or(false)
    }
}

/// Bounded provider registry above the process supervisor.
///
/// A route may be retried only before any transcript has crossed the worker
/// boundary. Stage 2 ships one CPU route, but keeping this contract here means
/// later verified GPU packs do not have to weaken strict GPU semantics or add
/// replay logic in application/UI code.
#[derive(Clone)]
pub(crate) struct InferenceWorkerRegistry {
    routes: Arc<Vec<InferenceWorkerRoute>>,
    gpu_routes: Arc<Mutex<GpuRouteCatalogCache>>,
    /// Auto has an independent cache so a prior explicit GPU probe cannot
    /// widen its pre-probe qualification allowlist.
    auto_gpu_routes: Arc<Mutex<GpuRouteCatalogCache>>,
    gpu_health_path: Option<Arc<PathBuf>>,
    active_route: Arc<Mutex<Option<ActiveInferenceRoute>>>,
    route_execution: Arc<Mutex<()>>,
    #[cfg(test)]
    gpu_routes_for_testing: Option<VerifiedGpuRouteCatalog>,
}

struct PendingPackProbe {
    pack_id: crate::gpu_worker_pack::manifest::StoreComponent,
    backend: crate::gpu_worker_pack::manifest::PackBackend,
    supervisor: InferenceWorkerSupervisor,
}

fn discover_pack_launch_bindings_with_budget(
    probes: Vec<PendingPackProbe>,
    mut diagnostics: Vec<crate::gpu_worker_pack::PackDiscoveryDiagnostic>,
    budget: Duration,
) -> crate::gpu_worker_pack::ProductionPackRegistry {
    use crate::gpu_worker_pack::{PackDiscoveryDiagnostic, PackDiscoveryIssue};

    let deadline = match MonotonicDeadline::after_for(budget, "GPU provider discovery") {
        Ok(deadline) => deadline,
        Err(_) => {
            diagnostics.push(PackDiscoveryDiagnostic::catalog(
                PackDiscoveryIssue::ProviderProbeRejected,
            ));
            return crate::gpu_worker_pack::ProductionPackRegistry::empty()
                .with_diagnostics(diagnostics);
        }
    };
    let mut bindings = Vec::new();
    for probe in probes.into_iter().take(MAX_GPU_PROVIDER_PROBES) {
        if deadline.remaining().is_err() {
            diagnostics.push(PackDiscoveryDiagnostic::pack(
                PackDiscoveryIssue::ProviderProbeRejected,
                &probe.pack_id,
                probe.backend,
            ));
            continue;
        }
        match probe.supervisor.verified_pack_bindings_before(deadline) {
            Ok(observed) => {
                for binding in observed {
                    if binding.backend_target().driver_version.is_none() {
                        diagnostics.push(PackDiscoveryDiagnostic::pack(
                            PackDiscoveryIssue::DriverVersionUnavailable,
                            &probe.pack_id,
                            probe.backend,
                        ));
                    } else {
                        bindings.push(binding);
                    }
                }
            }
            Err(_) => diagnostics.push(PackDiscoveryDiagnostic::pack(
                PackDiscoveryIssue::ProviderProbeRejected,
                &probe.pack_id,
                probe.backend,
            )),
        }
        if probe.supervisor.retire().is_err() {
            diagnostics.push(PackDiscoveryDiagnostic::pack(
                PackDiscoveryIssue::ProviderProbeRejected,
                &probe.pack_id,
                probe.backend,
            ));
        }
    }
    crate::gpu_worker_pack::ProductionPackRegistry::from_launch_bindings(bindings)
        .with_diagnostics(diagnostics)
}

fn discover_production_pack_launch_bindings(
    discovery: crate::gpu_worker_pack::PackLeaseDiscovery,
) -> crate::gpu_worker_pack::ProductionPackRegistry {
    let probes = discovery
        .leases
        .into_iter()
        .take(MAX_GPU_PROVIDER_PROBES)
        .map(|lease| PendingPackProbe {
            pack_id: lease.verified_pack().pack_id.clone(),
            backend: lease.verified_pack().backend,
            supervisor: InferenceWorkerSupervisor::for_pack_probe(lease),
        })
        .collect();
    discover_pack_launch_bindings_with_budget(
        probes,
        discovery.diagnostics,
        GPU_PROVIDER_DISCOVERY_BUDGET,
    )
}

/// Filters immutable pack metadata before constructing a probe supervisor. A
/// non-matching pack is never loaded merely to find out whether it would be
/// fast enough: release evidence, not machine calibration, is authoritative.
fn auto_qualified_pack_discovery(
    discovery: crate::gpu_worker_pack::PackLeaseDiscovery,
    policy: &AutoQualificationPolicy,
    model_digest: &str,
) -> crate::gpu_worker_pack::PackLeaseDiscovery {
    use crate::gpu_worker_pack::{PackDiscoveryDiagnostic, PackDiscoveryIssue};

    let mut leases = Vec::new();
    let mut diagnostics = discovery.diagnostics;
    for lease in discovery.leases {
        let pack = lease.verified_pack();
        let backend = match pack.backend {
            PackBackend::Cuda => BackendKind::Cuda,
            PackBackend::Vulkan => BackendKind::Vulkan,
            PackBackend::Metal => BackendKind::Metal,
        };
        let identity = crate::backend_policy::BackendPackIdentity {
            pack_id: pack.pack_id.as_str().to_owned(),
            pack_version: pack.pack_version.as_str().to_owned(),
            pack_digest: pack.pack_digest.clone(),
            security_epoch: pack.security_epoch,
            runtime_abi: pack.runtime_abi_version,
        };
        let provider = crate::backend_policy::ProviderIdentity::new(pack.provider.clone());
        if policy
            .qualify_pack(backend, &provider, &identity, model_digest)
            .is_approved()
        {
            leases.push(lease);
        } else {
            diagnostics.push(PackDiscoveryDiagnostic::pack(
                PackDiscoveryIssue::NotAutoQualified,
                &pack.pack_id,
                pack.backend,
            ));
        }
    }
    crate::gpu_worker_pack::PackLeaseDiscovery {
        leases,
        diagnostics,
        catalog_generation: discovery.catalog_generation,
    }
}

fn auto_gpu_discovery_fingerprint(
    discovery: &crate::gpu_worker_pack::PackLeaseDiscovery,
    policy: &AutoQualificationPolicy,
    model_digest: &str,
) -> String {
    let mut digest = Sha256::new();
    update_health_digest(&mut digest, "auto-qualification-v1");
    update_health_digest(&mut digest, &policy.policy_version().to_string());
    update_health_digest(&mut digest, &policy.manifest_digest());
    update_health_digest(&mut digest, model_digest);
    update_health_digest(
        &mut digest,
        discovery
            .catalog_generation
            .as_deref()
            .unwrap_or("catalog-unavailable"),
    );
    for lease in &discovery.leases {
        let pack = lease.verified_pack();
        update_health_digest(&mut digest, pack.pack_id.as_str());
        update_health_digest(&mut digest, pack.pack_version.as_str());
        update_health_digest(&mut digest, &pack.pack_digest);
        digest.update(pack.security_epoch.to_le_bytes());
        digest.update(pack.runtime_abi_version.to_le_bytes());
        update_health_digest(&mut digest, &pack.provider);
    }
    // The adapter fingerprint makes cached target facts stale after a driver
    // or enumerated-device change without ever loading a GPU provider.
    update_health_digest(&mut digest, &explicit_gpu_discovery_fingerprint(discovery));
    format!("{:x}", digest.finalize())
}

fn append_pack_diagnostic(resolved: &mut ResolvedAcceleration, diagnostic: Option<&str>) {
    let Some(diagnostic) = diagnostic.filter(|value| !value.is_empty()) else {
        return;
    };
    let combined = match resolved.diagnostic.take() {
        Some(existing) => format!("{existing}; {diagnostic}"),
        None => diagnostic.to_owned(),
    };
    resolved.diagnostic = Some(combined.chars().take(MAX_DIAGNOSTIC_BYTES).collect());
}

fn update_health_digest(hasher: &mut Sha256, value: &str) {
    hasher.update((value.len() as u64).to_le_bytes());
    hasher.update(value.as_bytes());
}

fn verified_gpu_device_set_digest(routes: &[InferenceWorkerRoute]) -> String {
    let mut identities = routes
        .iter()
        .filter_map(|route| {
            let target = route.target.as_ref()?;
            let pack = target.pack.as_ref()?;
            let driver = target.driver_version.as_deref()?;
            Some((
                target.backend.label(),
                target.provider_id.as_str(),
                target.device_id.as_str(),
                driver,
                format!("{:?}", target.vendor),
                format!("{:?}", target.device_class),
                target.memory_total_bytes,
                pack.pack_id.as_str(),
                pack.pack_version.as_str(),
                pack.pack_digest.as_str(),
                pack.security_epoch,
                pack.runtime_abi,
            ))
        })
        .collect::<Vec<_>>();
    identities.sort();
    let mut digest = Sha256::new();
    digest.update((identities.len() as u64).to_le_bytes());
    for identity in identities {
        update_health_digest(&mut digest, identity.0);
        update_health_digest(&mut digest, identity.1);
        update_health_digest(&mut digest, identity.2);
        update_health_digest(&mut digest, identity.3);
        update_health_digest(&mut digest, &identity.4);
        update_health_digest(&mut digest, &identity.5);
        digest.update(identity.6.to_le_bytes());
        update_health_digest(&mut digest, identity.7);
        update_health_digest(&mut digest, identity.8);
        update_health_digest(&mut digest, identity.9);
        digest.update(identity.10.to_le_bytes());
        digest.update(identity.11.to_le_bytes());
    }
    format!("{:x}", digest.finalize())
}

/// Health invalidation follows the physical adapter/driver set, not the
/// current preference, qualification subset, or power-policy subset. Sharing
/// this witness between explicit GPU and Auto prevents a normal mode or AC /
/// battery transition from making otherwise valid health records look corrupt.
fn physical_gpu_device_set_digest() -> String {
    let adapter_fingerprint = PackDriverCatalog::discover()
        .map(|catalog| catalog.fingerprint())
        .unwrap_or_else(|_| "adapter-metadata-unavailable".to_owned());
    let mut digest = Sha256::new();
    update_health_digest(&mut digest, "physical-gpu-device-set-v1");
    update_health_digest(&mut digest, &adapter_fingerprint);
    format!("{:x}", digest.finalize())
}

fn route_health_key(route: &InferenceWorkerRoute, model_digest: &str) -> Option<HealthKey> {
    let target = route.target.as_ref()?;
    let pack = target.pack.as_ref()?;
    let driver = target.driver_version.as_ref()?;
    let canonical_model_digest = model_digest.to_ascii_lowercase();
    if canonical_model_digest.len() != 64
        || !canonical_model_digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return None;
    }
    Some(HealthKey {
        pack_digest: pack.pack_digest.clone(),
        runtime_abi: pack.runtime_abi,
        os_arch: format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
        driver_version: driver.clone(),
        stable_device_identity: target.device_id.as_str().to_owned(),
        model_digest: canonical_model_digest,
    })
}

fn append_route_plan_diagnostic(diagnostic: &mut Option<String>, message: &str) {
    let mut combined = diagnostic.take().unwrap_or_default();
    if !combined.is_empty() {
        combined.push_str("; ");
    }
    combined.push_str(message);
    *diagnostic = Some(combined.chars().take(MAX_DIAGNOSTIC_BYTES).collect());
}

fn health_observation_for_runtime_error(
    error: &RuntimeError,
    cancelled: bool,
    output_published: bool,
) -> FailureObservation {
    if output_published {
        return FailureObservation::PartialOutput;
    }
    if cancelled {
        return FailureObservation::Cancellation;
    }
    match error {
        RuntimeError::InvalidAudio { .. } => FailureObservation::InvalidInput,
        RuntimeError::ArtifactIntegrity { .. }
        | RuntimeError::UnsupportedModel(_)
        | RuntimeError::OnnxUnavailable(_) => FailureObservation::ModelCorruption,
        RuntimeError::Inference(_) | RuntimeError::Callback(_) | RuntimeError::Engine(_) => {
            FailureObservation::DecodeContent
        }
        RuntimeError::BackendInitializationFailed(_) => FailureObservation::ProviderInitialization,
        RuntimeError::BackendUnavailable(_) => FailureObservation::DeviceLost,
        RuntimeError::OutOfMemory(_) => FailureObservation::OutOfMemory,
        RuntimeError::Cancelled(_) => FailureObservation::Cancellation,
        RuntimeError::RetryableWorkerFailure(_) | RuntimeError::Poisoned => {
            FailureObservation::WorkerCrash
        }
        RuntimeError::WorkerUnavailable(_) => FailureObservation::Protocol,
    }
}

fn backend_failure_category(error: &RuntimeError) -> Option<BackendFailureCategory> {
    match error {
        RuntimeError::BackendInitializationFailed(_) => {
            Some(BackendFailureCategory::InitializationFailed)
        }
        RuntimeError::BackendUnavailable(_) => Some(BackendFailureCategory::DeviceLost),
        RuntimeError::OutOfMemory(_) => Some(BackendFailureCategory::OutOfMemory),
        RuntimeError::WorkerUnavailable(_) => Some(BackendFailureCategory::BackendUnavailable),
        RuntimeError::RetryableWorkerFailure(_) | RuntimeError::Poisoned => {
            Some(BackendFailureCategory::WorkerFailed)
        }
        RuntimeError::InvalidAudio { .. }
        | RuntimeError::Inference(_)
        | RuntimeError::Callback(_)
        | RuntimeError::Engine(_)
        | RuntimeError::ArtifactIntegrity { .. }
        | RuntimeError::UnsupportedModel(_)
        | RuntimeError::Cancelled(_)
        | RuntimeError::OnnxUnavailable(_) => None,
    }
}

fn record_backend_fallback(
    history: &mut Vec<BackendFallback>,
    route: &InferenceWorkerRoute,
    error: &RuntimeError,
) {
    let Some(target) = route.target.clone() else {
        return;
    };
    let Some(category) = backend_failure_category(error) else {
        return;
    };
    history.push(BackendFallback { target, category });
}

fn explicit_gpu_discovery_fingerprint(
    discovery: &crate::gpu_worker_pack::PackLeaseDiscovery,
) -> String {
    let mut digest = Sha256::new();
    update_health_digest(
        &mut digest,
        discovery
            .catalog_generation
            .as_deref()
            .unwrap_or("catalog-unavailable"),
    );
    for diagnostic in &discovery.diagnostics {
        update_health_digest(&mut digest, &format!("{:?}", diagnostic.issue));
        update_health_digest(
            &mut digest,
            diagnostic.pack_id.as_deref().unwrap_or("no-pack"),
        );
        update_health_digest(
            &mut digest,
            &diagnostic
                .backend
                .map(|backend| format!("{backend:?}"))
                .unwrap_or_else(|| "no-backend".to_owned()),
        );
    }
    let adapter_fingerprint = PackDriverCatalog::discover()
        .map(|catalog| catalog.fingerprint())
        .unwrap_or_else(|_| "adapter-metadata-unavailable".to_owned());
    update_health_digest(&mut digest, &adapter_fingerprint);
    format!("{:x}", digest.finalize())
}

fn verified_gpu_route_catalog(
    registry: crate::gpu_worker_pack::ProductionPackRegistry,
    discovery_fingerprint: String,
) -> VerifiedGpuRouteCatalog {
    let (bindings, diagnostics) = registry.into_parts();
    let provider_probe_incomplete = diagnostics.iter().any(|diagnostic| {
        matches!(
            diagnostic.issue,
            crate::gpu_worker_pack::PackDiscoveryIssue::ProviderProbeRejected
                | crate::gpu_worker_pack::PackDiscoveryIssue::DriverVersionUnavailable
        )
    });
    let mut routes = bindings
        .into_iter()
        .map(|binding| {
            let target = binding.backend_target();
            InferenceWorkerRoute {
                provider: match target.backend {
                    BackendKind::Cuda => WorkerProvider::Cuda,
                    BackendKind::Vulkan => WorkerProvider::Vulkan,
                    BackendKind::Metal => WorkerProvider::Metal,
                    BackendKind::Cpu => WorkerProvider::Cpu,
                },
                supervisor: InferenceWorkerSupervisor::for_pack_binding(binding),
                target: Some(target),
                health_key: None,
                consume_retry_bypass: false,
                auto_evidence_id: None,
            }
        })
        .filter(|route| route.provider.is_gpu())
        .collect::<Vec<_>>();
    routes.sort_by(|left, right| {
        let key = |route: &InferenceWorkerRoute| {
            let target = route.target.as_ref().expect("GPU route target");
            (
                match target.backend {
                    BackendKind::Cuda => 0,
                    BackendKind::Vulkan => 1,
                    BackendKind::Metal => 2,
                    BackendKind::Cpu => 3,
                },
                target.device_id.as_str().to_owned(),
            )
        };
        key(left).cmp(&key(right))
    });
    #[cfg(target_os = "macos")]
    let device_set_digest = verified_gpu_device_set_digest(&routes);
    #[cfg(not(target_os = "macos"))]
    let device_set_digest = physical_gpu_device_set_digest();
    let mut diagnostic = crate::gpu_worker_pack::diagnostic_summary(&diagnostics);
    if routes.len() > 4 {
        let skipped = routes.len() - 4;
        if !diagnostic.is_empty() {
            diagnostic.push_str("; ");
        }
        diagnostic.push_str(&format!(
            "{skipped} additional verified GPU route(s) are outside the four-route fallback bound"
        ));
    }
    VerifiedGpuRouteCatalog {
        routes: Arc::new(routes),
        diagnostic: (!diagnostic.is_empty()).then_some(diagnostic),
        device_set_digest,
        discovery_fingerprint,
        provider_probe_incomplete,
        skipped_targets: Vec::new(),
    }
}

fn auto_qualified_gpu_route_catalog(
    mut catalog: VerifiedGpuRouteCatalog,
    policy: &AutoQualificationPolicy,
    model_digest: &str,
    power_source: PowerSource,
) -> VerifiedGpuRouteCatalog {
    let mut qualified_routes = Vec::with_capacity(catalog.routes.len());
    let mut qualification_denied = 0_usize;
    let mut battery_denied = 0_usize;
    let mut qualification_diagnostic = None;
    for route in catalog.routes.iter() {
        let Some(target) = route.target.as_ref() else {
            qualification_denied = qualification_denied.saturating_add(1);
            continue;
        };
        let qualification = policy.qualify_target(target, model_digest);
        match qualification {
            QualificationDecision::Approved { evidence_id } => {
                let mut qualified = route.clone();
                qualified.auto_evidence_id = Some(evidence_id);
                qualified_routes.push(qualified);
            }
            denied @ QualificationDecision::Denied(_) => {
                qualification_denied = qualification_denied.saturating_add(1);
                qualification_diagnostic.get_or_insert_with(|| denied.diagnostic());
                catalog.skipped_targets.push(SkippedBackend {
                    target: target.clone(),
                    reason: BackendSkipReason::NotAutoQualified,
                });
            }
        }
    }
    let mut routes = Vec::with_capacity(qualified_routes.len());
    for route in qualified_routes {
        let battery_eligible = power_source != PowerSource::Battery
            || route
                .target
                .as_ref()
                .is_some_and(|target| target.device_class.is_battery_eligible_gpu());
        if battery_eligible {
            routes.push(route);
        } else {
            battery_denied = battery_denied.saturating_add(1);
            if let Some(target) = route.target.clone() {
                catalog.skipped_targets.push(SkippedBackend {
                    target,
                    reason: BackendSkipReason::BatteryPolicy,
                });
            }
        }
    }
    catalog.routes = Arc::new(routes);
    if qualification_denied != 0 {
        append_route_plan_diagnostic(
            &mut catalog.diagnostic,
            &format!(
                "{qualification_denied} probed GPU route(s) did not match Auto qualification evidence{}",
                qualification_diagnostic
                    .as_deref()
                    .map(|detail| format!(": {detail}"))
                    .unwrap_or_default()
            ),
        );
    }
    if battery_denied != 0 {
        append_route_plan_diagnostic(
            &mut catalog.diagnostic,
            &format!(
                "{battery_denied} discrete or unclassified GPU route(s) were excluded on battery"
            ),
        );
    }
    catalog
}

fn auto_gpu_route_plan(
    cpu_routes: Arc<Vec<InferenceWorkerRoute>>,
    catalog: VerifiedGpuRouteCatalog,
    policy: &AutoQualificationPolicy,
    model_digest: &str,
    health_path: Option<&Arc<PathBuf>>,
    allow_unprobed_idle_health: bool,
) -> InferenceWorkerRoutePlan {
    let selection_power_source = PowerSource::current();
    let catalog =
        auto_qualified_gpu_route_catalog(catalog, policy, model_digest, selection_power_source);
    let mut plan = health_filtered_gpu_route_plan(
        catalog,
        model_digest,
        health_path,
        allow_unprobed_idle_health,
    );
    plan.selection_power_source = Some(selection_power_source);
    reserve_auto_cpu_fallback(&mut plan, &cpu_routes);
    plan
}

fn reserve_auto_cpu_fallback(
    plan: &mut InferenceWorkerRoutePlan,
    cpu_routes: &Arc<Vec<InferenceWorkerRoute>>,
) {
    let mut routes = Vec::with_capacity(plan.routes.len() + cpu_routes.len());
    let max_gpu_routes =
        MAX_INFERENCE_ROUTE_ATTEMPTS.saturating_sub(usize::from(!cpu_routes.is_empty()));
    if plan.routes.len() > max_gpu_routes {
        plan.skipped_targets.extend(
            plan.routes
                .iter()
                .skip(max_gpu_routes)
                .filter_map(|route| route.target.clone())
                .map(|target| SkippedBackend {
                    target,
                    reason: BackendSkipReason::FallbackBound,
                }),
        );
        append_route_plan_diagnostic(
            &mut plan.diagnostic,
            &format!(
                "{} additional qualified GPU route(s) were skipped to reserve Auto's guaranteed CPU fallback",
                plan.routes.len() - max_gpu_routes
            ),
        );
    }
    routes.extend(plan.routes.iter().take(max_gpu_routes).cloned());
    routes.extend(cpu_routes.iter().cloned());
    if plan.routes.is_empty() {
        append_route_plan_diagnostic(
            &mut plan.diagnostic,
            "Auto selected the guaranteed CPU fallback",
        );
    }
    plan.routes = Arc::new(routes);
}

fn health_filtered_gpu_route_plan(
    catalog: VerifiedGpuRouteCatalog,
    model_digest: &str,
    health_path: Option<&Arc<PathBuf>>,
    allow_unprobed_idle_health: bool,
) -> InferenceWorkerRoutePlan {
    if catalog.routes.is_empty() {
        return InferenceWorkerRoutePlan {
            routes: catalog.routes,
            diagnostic: catalog.diagnostic,
            health: None,
            selection_power_source: None,
            skipped_targets: catalog.skipped_targets,
        };
    }
    let Some(path) = health_path else {
        let mut diagnostic = catalog.diagnostic;
        let mut skipped_targets = catalog.skipped_targets;
        skipped_targets.extend(catalog.routes.iter().filter_map(|route| {
            route.target.clone().map(|target| SkippedBackend {
                target,
                reason: BackendSkipReason::Unhealthy,
            })
        }));
        append_route_plan_diagnostic(
            &mut diagnostic,
            "private GPU health state is unavailable; verified GPU routes were denied",
        );
        return InferenceWorkerRoutePlan {
            routes: Arc::new(Vec::new()),
            diagnostic,
            health: None,
            selection_power_source: None,
            skipped_targets,
        };
    };
    let health = GpuRouteHealth {
        path: Arc::clone(path),
        witnesses: HealthWitnesses {
            app_build: DESKTOP_BUILD_ID.to_owned(),
            device_set_digest: catalog.device_set_digest,
        },
    };
    let mut diagnostic = catalog.diagnostic;
    let mut recovery_routes = Vec::with_capacity(catalog.routes.len());
    let mut routes = Vec::with_capacity(catalog.routes.len());
    let mut quarantined = 0_usize;
    let mut awaiting_probe = 0_usize;
    let mut skipped_targets = catalog.skipped_targets;
    for base in catalog.routes.iter() {
        let Some(key) = route_health_key(base, model_digest) else {
            if let Some(target) = base.target.clone() {
                skipped_targets.push(SkippedBackend {
                    target,
                    reason: BackendSkipReason::Unaddressable,
                });
            }
            append_route_plan_diagnostic(
                &mut diagnostic,
                "a verified GPU route lacked a complete bounded health identity",
            );
            continue;
        };
        let mut route = base.clone();
        route.health_key = Some(key.clone());
        match health.cache().decision(&key) {
            HealthDecision::Available => routes.push(route),
            HealthDecision::RetryBypass => {
                route.consume_retry_bypass = true;
                routes.push(route);
            }
            HealthDecision::Quarantined { .. } => {
                quarantined = quarantined.saturating_add(1);
                if let Some(target) = route.target.clone() {
                    skipped_targets.push(SkippedBackend {
                        target,
                        reason: BackendSkipReason::Quarantined,
                    });
                }
            }
            HealthDecision::InvalidOrUnprobed => {
                awaiting_probe = awaiting_probe.saturating_add(1);
                if allow_unprobed_idle_health {
                    recovery_routes.push(route);
                } else if let Some(target) = route.target.clone() {
                    skipped_targets.push(SkippedBackend {
                        target,
                        reason: BackendSkipReason::Unhealthy,
                    });
                }
            }
        }
    }
    if allow_unprobed_idle_health && !recovery_routes.is_empty() {
        recovery_routes.extend(routes);
        routes = recovery_routes;
    }
    if quarantined != 0 {
        append_route_plan_diagnostic(
            &mut diagnostic,
            &format!("{quarantined} verified GPU route(s) are quarantined"),
        );
    }
    if awaiting_probe != 0 {
        append_route_plan_diagnostic(
            &mut diagnostic,
            &format!(
                "{awaiting_probe} verified GPU route(s) require successful explicit idle probes"
            ),
        );
    }
    InferenceWorkerRoutePlan {
        routes: Arc::new(routes),
        diagnostic,
        health: Some(health),
        selection_power_source: None,
        skipped_targets,
    }
}

impl InferenceWorkerRegistry {
    fn lock_route_execution(&self) -> Result<std::sync::MutexGuard<'_, ()>, RuntimeError> {
        self.route_execution.lock().map_err(|_| {
            RuntimeError::WorkerUnavailable("inference route execution lock poisoned".to_owned())
        })
    }

    fn invalidate_gpu_catalog_after_runtime_failure(
        &self,
        route: &InferenceWorkerRoute,
    ) -> Result<(), RuntimeError> {
        if !route.provider.is_gpu() {
            return Ok(());
        }
        // Auto and explicit GPU keep separate admission caches, but both bind
        // the same physical pack/device facts. A runtime or Hello mismatch in
        // either mode makes both catalogs stale, especially when a device at
        // the same PCI location receives a new LUID/UUID.
        for cache in [&self.auto_gpu_routes, &self.gpu_routes] {
            cache
                .lock()
                .map_err(|_| {
                    RuntimeError::WorkerUnavailable(
                        "GPU worker registry lock poisoned during invalidation".to_owned(),
                    )
                })?
                .invalidate_after_runtime_failure();
        }
        Ok(())
    }

    pub(crate) fn for_current_build() -> Self {
        let provider = WorkerProvider::Cpu;
        Self {
            routes: Arc::new(vec![InferenceWorkerRoute {
                provider,
                supervisor: InferenceWorkerSupervisor::unstarted(),
                target: Some(BackendTarget::cpu()),
                health_key: None,
                consume_retry_bypass: false,
                auto_evidence_id: None,
            }]),
            gpu_routes: Arc::new(Mutex::new(GpuRouteCatalogCache::default())),
            auto_gpu_routes: Arc::new(Mutex::new(GpuRouteCatalogCache::default())),
            gpu_health_path: config::project_dirs().ok().map(|directories| {
                Arc::new(
                    directories
                        .data_local_dir()
                        .join("gpu-worker-health-v3.json"),
                )
            }),
            active_route: Arc::new(Mutex::new(None)),
            route_execution: Arc::new(Mutex::new(())),
            #[cfg(test)]
            gpu_routes_for_testing: None,
        }
    }

    #[allow(
        dead_code,
        reason = "the explicit CPU-only registry constructor remains a fail-closed integration seam"
    )]
    pub(crate) fn cpu_only() -> Self {
        Self {
            routes: Arc::new(vec![InferenceWorkerRoute {
                provider: WorkerProvider::Cpu,
                supervisor: InferenceWorkerSupervisor::unstarted(),
                target: Some(BackendTarget::cpu()),
                health_key: None,
                consume_retry_bypass: false,
                auto_evidence_id: None,
            }]),
            gpu_routes: Arc::new(Mutex::new(GpuRouteCatalogCache::default())),
            auto_gpu_routes: Arc::new(Mutex::new(GpuRouteCatalogCache::default())),
            gpu_health_path: config::project_dirs().ok().map(|directories| {
                Arc::new(
                    directories
                        .data_local_dir()
                        .join("gpu-worker-health-v3.json"),
                )
            }),
            active_route: Arc::new(Mutex::new(None)),
            route_execution: Arc::new(Mutex::new(())),
            #[cfg(test)]
            gpu_routes_for_testing: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn with_cpu_supervisor(supervisor: InferenceWorkerSupervisor) -> Self {
        Self {
            routes: Arc::new(vec![InferenceWorkerRoute {
                provider: WorkerProvider::Cpu,
                supervisor,
                target: Some(BackendTarget::cpu()),
                health_key: None,
                consume_retry_bypass: false,
                auto_evidence_id: None,
            }]),
            gpu_routes: Arc::new(Mutex::new(GpuRouteCatalogCache::default())),
            auto_gpu_routes: Arc::new(Mutex::new(GpuRouteCatalogCache::default())),
            gpu_health_path: None,
            active_route: Arc::new(Mutex::new(None)),
            route_execution: Arc::new(Mutex::new(())),
            gpu_routes_for_testing: None,
        }
    }

    fn route_identity(route: &InferenceWorkerRoute) -> InferenceRouteIdentity {
        let target = route
            .target
            .as_ref()
            .cloned()
            .unwrap_or_else(BackendTarget::cpu);
        InferenceRouteIdentity {
            provider: route.provider,
            backend: target.backend,
            stable_device_identity: target.device_id.as_str().to_owned(),
            driver_version: target.driver_version,
            pack_digest: target.pack.map(|pack| pack.pack_digest),
        }
    }

    fn same_supervisor(
        left: &InferenceWorkerSupervisor,
        right: &InferenceWorkerSupervisor,
    ) -> bool {
        Arc::ptr_eq(&left.transport.inner, &right.transport.inner)
    }

    fn retire_active_worker(&self) -> Result<(), RuntimeError> {
        let active = self
            .active_route
            .lock()
            .map_err(|_| {
                RuntimeError::WorkerUnavailable("active inference route lock poisoned".to_owned())
            })?
            .take();
        if let Some(active) = active {
            active.supervisor.retire()?;
        }
        Ok(())
    }

    fn retire_gpu_workers_after_authority_rejection(
        &self,
        route: &InferenceWorkerRoute,
    ) -> Result<(), RuntimeError> {
        let active_gpu = {
            let mut active = self.active_route.lock().map_err(|_| {
                RuntimeError::WorkerUnavailable("active inference route lock poisoned".to_owned())
            })?;
            active
                .as_ref()
                .is_some_and(|current| current.identity.backend != BackendKind::Cpu)
                .then(|| active.take())
                .flatten()
        };
        let route_retirement = route.supervisor.retire();
        let active_retirement = if active_gpu
            .as_ref()
            .is_some_and(|active| !Self::same_supervisor(&active.supervisor, &route.supervisor))
        {
            active_gpu
                .as_ref()
                .expect("checked active GPU route")
                .supervisor
                .retire()
        } else {
            Ok(())
        };
        route_retirement.and(active_retirement)
    }

    fn revalidate_route_activation(
        &self,
        route: &InferenceWorkerRoute,
    ) -> Result<(), RoutePreparationError> {
        if !route.provider.is_gpu() {
            return Ok(());
        }
        let target_security_epoch = route
            .target
            .as_ref()
            .and_then(|target| target.pack.as_ref())
            .map(|pack| pack.security_epoch);
        if crate::gpu_worker_pack::revalidate_production_device_epoch(target_security_epoch).is_ok()
        {
            return Ok(());
        }
        if self
            .retire_gpu_workers_after_authority_rejection(route)
            .is_err()
        {
            return Err(RoutePreparationError::Fatal(
                RuntimeError::WorkerUnavailable(
                    "GPU route rejected by the device-local release rollback authority; worker retirement was incomplete"
                        .to_owned(),
                ),
            ));
        }
        Err(RoutePreparationError::DeviceRollbackAuthority(
            RuntimeError::WorkerUnavailable(
                "GPU route rejected by the device-local release rollback authority".to_owned(),
            ),
        ))
    }

    fn prepare_route(&self, route: &InferenceWorkerRoute) -> Result<(), RoutePreparationError> {
        let identity = Self::route_identity(route);
        let prior = {
            let mut active = self.active_route.lock().map_err(|_| {
                RoutePreparationError::Fatal(RuntimeError::WorkerUnavailable(
                    "active inference route lock poisoned".to_owned(),
                ))
            })?;
            if active.as_ref().is_some_and(|current| {
                current.identity == identity
                    && Self::same_supervisor(&current.supervisor, &route.supervisor)
            }) {
                drop(active);
                // Warm-route reuse is still a new request and must recheck the
                // device authority immediately before use.
                self.revalidate_route_activation(route)?;
                return Ok(());
            }
            active.take()
        };
        if let Some(prior) = prior {
            prior
                .supervisor
                .retire()
                .map_err(RoutePreparationError::Fatal)?;
        }
        // Discovery, cache lookup, and retiring a prior route can take time.
        // Recheck directly before publication. An epoch advanced after this
        // boundary belongs to the next request; active work is never migrated.
        self.revalidate_route_activation(route)?;
        *self.active_route.lock().map_err(|_| {
            RoutePreparationError::Fatal(RuntimeError::WorkerUnavailable(
                "active inference route lock poisoned".to_owned(),
            ))
        })? = Some(ActiveInferenceRoute {
            identity,
            supervisor: route.supervisor.clone(),
        });
        Ok(())
    }

    fn retire_failed_route(&self, route: &InferenceWorkerRoute) -> Result<(), RuntimeError> {
        route.supervisor.retire()?;
        let identity = Self::route_identity(route);
        let mut active = self.active_route.lock().map_err(|_| {
            RuntimeError::WorkerUnavailable("active inference route lock poisoned".to_owned())
        })?;
        if active.as_ref().is_some_and(|current| {
            current.identity == identity
                && Self::same_supervisor(&current.supervisor, &route.supervisor)
        }) {
            *active = None;
        }
        Ok(())
    }

    fn routes_for_preference(
        &self,
        preference: AccelerationPreference,
        artifact: &RuntimeArtifact,
    ) -> Result<InferenceWorkerRoutePlan, RuntimeError> {
        self.routes_for_preference_with_idle_health(preference, artifact, false)
    }

    fn routes_for_preference_with_idle_health(
        &self,
        preference: AccelerationPreference,
        artifact: &RuntimeArtifact,
        allow_unprobed_idle_health: bool,
    ) -> Result<InferenceWorkerRoutePlan, RuntimeError> {
        if preference == AccelerationPreference::Cpu {
            return Ok(InferenceWorkerRoutePlan {
                routes: Arc::clone(&self.routes),
                diagnostic: None,
                health: None,
                selection_power_source: None,
                skipped_targets: Vec::new(),
            });
        }
        if preference == AccelerationPreference::Auto {
            let RuntimeArtifact::Gguf(model) = artifact else {
                return Ok(InferenceWorkerRoutePlan {
                    routes: Arc::clone(&self.routes),
                    diagnostic: Some(
                        "Auto GPU qualification applies only to verified GGUF worker packs; Auto selected CPU"
                            .to_owned(),
                    ),
                    health: None,
                    selection_power_source: None,
                    skipped_targets: Vec::new(),
                });
            };
            #[cfg(test)]
            if let Some(catalog) = self.gpu_routes_for_testing.as_ref() {
                let mut plan = InferenceWorkerRoutePlan {
                    routes: Arc::clone(&catalog.routes),
                    diagnostic: catalog.diagnostic.clone(),
                    health: None,
                    selection_power_source: Some(PowerSource::Ac),
                    skipped_targets: catalog.skipped_targets.clone(),
                };
                reserve_auto_cpu_fallback(&mut plan, &self.routes);
                return Ok(plan);
            }
            let policy = match AutoQualificationPolicy::embedded_current_platform() {
                Ok(policy) => policy,
                Err(error) => {
                    return Ok(InferenceWorkerRoutePlan {
                        routes: Arc::clone(&self.routes),
                        diagnostic: Some(format!(
                            "Auto GPU qualification manifest was rejected; Auto selected CPU: {error}"
                        )),
                        health: None,
                        selection_power_source: None,
                        skipped_targets: Vec::new(),
                    });
                }
            };
            // Filter verified pack descriptors before creating any provider
            // probe. The checked-in production policy has zero entries, so
            // ordinary releases take this CPU path without loading a GPU DLL.
            let discovery = auto_qualified_pack_discovery(
                crate::gpu_worker_pack::discover_production_pack_leases(),
                policy,
                &model.expected_sha256,
            );
            if discovery.leases.is_empty() {
                let mut diagnostic = discovery.diagnostic_summary();
                if !diagnostic.is_empty() {
                    diagnostic.push_str("; ");
                }
                diagnostic.push_str("Auto selected the guaranteed CPU fallback");
                return Ok(InferenceWorkerRoutePlan {
                    routes: Arc::clone(&self.routes),
                    diagnostic: Some(diagnostic.chars().take(MAX_DIAGNOSTIC_BYTES).collect()),
                    health: None,
                    selection_power_source: None,
                    skipped_targets: Vec::new(),
                });
            }
            let discovery_fingerprint =
                auto_gpu_discovery_fingerprint(&discovery, policy, &model.expected_sha256);
            let lookup_at = Instant::now();
            let cached_catalog = {
                let cached = self.auto_gpu_routes.lock().map_err(|_| {
                    RuntimeError::WorkerUnavailable(
                        "Auto GPU worker registry lock poisoned".to_owned(),
                    )
                })?;
                cached.lookup(&discovery_fingerprint, lookup_at)
            };
            let catalog = match cached_catalog {
                GpuRouteCatalogLookup::Successful(cached) => cached,
                GpuRouteCatalogLookup::Backoff(diagnostic) => VerifiedGpuRouteCatalog {
                    routes: Arc::new(Vec::new()),
                    diagnostic,
                    device_set_digest: verified_gpu_device_set_digest(&[]),
                    discovery_fingerprint,
                    provider_probe_incomplete: true,
                    skipped_targets: Vec::new(),
                },
                GpuRouteCatalogLookup::Probe => {
                    self.retire_active_worker()?;
                    let discovered = verified_gpu_route_catalog(
                        discover_production_pack_launch_bindings(discovery),
                        discovery_fingerprint,
                    );
                    let probe_completed_at = Instant::now();
                    self.auto_gpu_routes
                        .lock()
                        .map_err(|_| {
                            RuntimeError::WorkerUnavailable(
                                "Auto GPU worker registry lock poisoned".to_owned(),
                            )
                        })?
                        .record_probe(discovered.clone(), probe_completed_at);
                    discovered
                }
            };
            return Ok(auto_gpu_route_plan(
                Arc::clone(&self.routes),
                catalog,
                policy,
                &model.expected_sha256,
                self.gpu_health_path.as_ref(),
                allow_unprobed_idle_health,
            ));
        }
        let RuntimeArtifact::Gguf(model) = artifact else {
            return Ok(InferenceWorkerRoutePlan {
                routes: Arc::new(Vec::new()),
                diagnostic: Some(
                    "verified GPU worker packs support only pinned GGUF artifacts".to_owned(),
                ),
                health: None,
                selection_power_source: None,
                skipped_targets: Vec::new(),
            });
        };

        #[cfg(test)]
        if let Some(catalog) = self.gpu_routes_for_testing.as_ref() {
            if self.gpu_health_path.is_some() {
                return Ok(health_filtered_gpu_route_plan(
                    catalog.clone(),
                    &model.expected_sha256,
                    self.gpu_health_path.as_ref(),
                    allow_unprobed_idle_health,
                ));
            }
            return Ok(InferenceWorkerRoutePlan {
                routes: Arc::clone(&catalog.routes),
                diagnostic: catalog.diagnostic.clone(),
                health: None,
                selection_power_source: None,
                skipped_targets: catalog.skipped_targets.clone(),
            });
        }

        // Catalog activation and Windows SetupAPI adapter metadata provide a
        // cheap request-bound fingerprint. Reuse the warm winning supervisor
        // while it is unchanged; a changed fingerprint retires that worker
        // before any provider probe can start.
        let lease_discovery = crate::gpu_worker_pack::discover_production_pack_leases();
        let discovery_fingerprint = explicit_gpu_discovery_fingerprint(&lease_discovery);
        let lookup_at = Instant::now();
        let cached_catalog = {
            let cached = self.gpu_routes.lock().map_err(|_| {
                RuntimeError::WorkerUnavailable("GPU worker registry lock poisoned".to_owned())
            })?;
            cached.lookup(&discovery_fingerprint, lookup_at)
        };
        let catalog = match cached_catalog {
            GpuRouteCatalogLookup::Successful(cached) => cached,
            GpuRouteCatalogLookup::Backoff(diagnostic) => VerifiedGpuRouteCatalog {
                routes: Arc::new(Vec::new()),
                diagnostic,
                device_set_digest: verified_gpu_device_set_digest(&[]),
                discovery_fingerprint,
                provider_probe_incomplete: true,
                skipped_targets: Vec::new(),
            },
            GpuRouteCatalogLookup::Probe => {
                self.retire_active_worker()?;
                let discovered = verified_gpu_route_catalog(
                    discover_production_pack_launch_bindings(lease_discovery),
                    discovery_fingerprint,
                );
                let probe_completed_at = Instant::now();
                self.gpu_routes
                    .lock()
                    .map_err(|_| {
                        RuntimeError::WorkerUnavailable(
                            "GPU worker registry lock poisoned".to_owned(),
                        )
                    })?
                    .record_probe(discovered.clone(), probe_completed_at);
                discovered
            }
        };

        Ok(health_filtered_gpu_route_plan(
            catalog,
            &model.expected_sha256,
            self.gpu_health_path.as_ref(),
            allow_unprobed_idle_health,
        ))
    }

    #[allow(
        dead_code,
        reason = "Stage 5 connects this exact-target retry authority to diagnostics UI"
    )]
    pub(crate) fn grant_explicit_gpu_retry(
        &self,
        artifact: &RuntimeArtifact,
        target: &BackendTarget,
    ) -> Result<bool, RuntimeError> {
        let RuntimeArtifact::Gguf(model) = artifact else {
            return Ok(false);
        };
        let Some(path) = self.gpu_health_path.as_ref() else {
            return Ok(false);
        };
        let cached = self.gpu_routes.lock().map_err(|_| {
            RuntimeError::WorkerUnavailable("GPU worker registry lock poisoned".to_owned())
        })?;
        let Some(catalog) = cached.successful() else {
            return Ok(false);
        };
        let health = GpuRouteHealth {
            path: Arc::clone(path),
            witnesses: HealthWitnesses {
                app_build: DESKTOP_BUILD_ID.to_owned(),
                device_set_digest: catalog.device_set_digest.clone(),
            },
        };
        let key = catalog.routes.iter().find_map(|route| {
            let candidate = route.target.as_ref()?;
            (candidate.backend == target.backend
                && candidate.device_id == target.device_id
                && candidate.driver_version == target.driver_version
                && candidate.pack == target.pack)
                .then(|| route_health_key(route, &model.expected_sha256))
                .flatten()
        });
        Ok(key.is_some_and(|key| health.grant_explicit_retry(&key)))
    }

    #[allow(
        dead_code,
        reason = "Stage 5 connects this bounded provider-reprobe action to diagnostics UI"
    )]
    pub(crate) fn grant_explicit_gpu_provider_probe_retry(&self) -> Result<bool, RuntimeError> {
        let discovery = crate::gpu_worker_pack::discover_production_pack_leases();
        let fingerprint = explicit_gpu_discovery_fingerprint(&discovery);
        let granted = self
            .gpu_routes
            .lock()
            .map_err(|_| {
                RuntimeError::WorkerUnavailable("GPU worker registry lock poisoned".to_owned())
            })?
            .grant_explicit_probe_retry(&fingerprint, Instant::now());
        Ok(granted)
    }

    fn no_route_error(preference: AccelerationPreference) -> RuntimeError {
        RuntimeError::WorkerUnavailable(match preference {
            AccelerationPreference::Gpu => {
                "no GPU inference worker is registered; CPU fallback is forbidden for explicit GPU"
                    .to_owned()
            }
            AccelerationPreference::Auto | AccelerationPreference::Cpu => {
                "no compatible inference worker is registered".to_owned()
            }
        })
    }

    fn worker_preference_for_route(
        route: &InferenceWorkerRoute,
        requested: AccelerationPreference,
    ) -> AccelerationPreference {
        // The parent has already made a strict Auto decision from verified
        // pack, target, power, and health facts. The selected GPU worker must
        // receive `Gpu` so its local generic policy cannot replace that
        // parent-authorized target with its CPU fallback.
        if requested == AccelerationPreference::Auto && route.provider.is_gpu() {
            AccelerationPreference::Gpu
        } else {
            requested
        }
    }

    fn project_auto_route_diagnostics(
        diagnostics: &mut NativeRuntimeDiagnostics,
        route: &InferenceWorkerRoute,
        power_source: PowerSource,
        fallback_targets: Vec<BackendTarget>,
        fallback_history: &[BackendFallback],
        skipped_targets: &[SkippedBackend],
    ) {
        let target = route.target.clone().unwrap_or_else(BackendTarget::cpu);
        diagnostics.resolved_acceleration.requested = AccelerationPreference::Auto;
        diagnostics.resolved_acceleration.selection = Some(BackendSelection {
            requested: AccelerationPreference::Auto,
            reason: if target.backend == BackendKind::Cpu {
                BackendSelectionReason::AutoCpuFallback
            } else {
                BackendSelectionReason::AutoPriority
            },
            target,
            power_source,
            power_policy: match power_source {
                PowerSource::Battery => PowerPolicyDecision::BatteryEfficientGpuOnly,
                PowerSource::Ac | PowerSource::Unknown => PowerPolicyDecision::Unrestricted,
            },
            qualification_policy_version: AUTO_QUALIFICATION_POLICY_VERSION,
            fallback_targets,
            fallback_history: fallback_history.to_vec(),
            skipped_targets: skipped_targets.to_vec(),
        });
        if let Some(evidence_id) = route.auto_evidence_id.as_deref() {
            let evidence = format!("Auto qualification selected release evidence {evidence_id}");
            append_pack_diagnostic(
                &mut diagnostics.resolved_acceleration,
                Some(evidence.as_str()),
            );
        }
    }

    pub(crate) fn load(
        &self,
        artifact: RuntimeArtifact,
        preference: AccelerationPreference,
    ) -> Result<RuntimeLoadExecution, RuntimeError> {
        let _route_execution = self.lock_route_execution()?;
        let mut last_retryable = None;
        let mut fallback_history = Vec::new();
        let mut attempted = false;
        let plan = self.routes_for_preference(preference, &artifact)?;
        if plan.routes.is_empty() {
            self.retire_active_worker()?;
        }
        for (route_index, route) in plan
            .routes
            .iter()
            .take(MAX_INFERENCE_ROUTE_ATTEMPTS)
            .enumerate()
        {
            if plan
                .health
                .as_ref()
                .is_some_and(|health| !health.consume_retry(route))
            {
                continue;
            }
            attempted = true;
            match self.prepare_route(route) {
                Ok(()) => {}
                Err(RoutePreparationError::DeviceRollbackAuthority(error)) => {
                    self.invalidate_gpu_catalog_after_runtime_failure(route)?;
                    let error = if preference == AccelerationPreference::Gpu {
                        RuntimeError::WorkerUnavailable(format!(
                            "{error}; CPU fallback is forbidden for explicit GPU"
                        ))
                    } else {
                        RuntimeError::RetryableWorkerFailure(error.to_string())
                    };
                    record_backend_fallback(&mut fallback_history, route, &error);
                    last_retryable = Some(error);
                    continue;
                }
                Err(RoutePreparationError::Fatal(error)) => return Err(error),
            }
            match route.supervisor.load(
                artifact.clone(),
                Self::worker_preference_for_route(route, preference),
            ) {
                Ok(mut execution) => {
                    if preference == AccelerationPreference::Auto {
                        Self::project_auto_route_diagnostics(
                            &mut execution.diagnostics,
                            route,
                            plan.selection_power_source
                                .unwrap_or_else(PowerSource::current),
                            plan.routes
                                .iter()
                                .skip(route_index + 1)
                                .filter_map(|candidate| candidate.target.clone())
                                .collect(),
                            &fallback_history,
                            &plan.skipped_targets,
                        );
                    }
                    if matches!(artifact, RuntimeArtifact::Gguf(_)) {
                        append_pack_diagnostic(
                            &mut execution.diagnostics.resolved_acceleration,
                            plan.diagnostic.as_deref(),
                        );
                    }
                    return Ok(execution);
                }
                Err(error @ RuntimeError::RetryableWorkerFailure(_)) => {
                    if let Some(health) = &plan.health {
                        health.record_failure(
                            route,
                            health_observation_for_runtime_error(&error, false, false),
                        );
                    }
                    record_backend_fallback(&mut fallback_history, route, &error);
                    self.invalidate_gpu_catalog_after_runtime_failure(route)?;
                    self.retire_failed_route(route)?;
                    last_retryable = Some(error);
                }
                Err(
                    error @ (RuntimeError::BackendInitializationFailed(_)
                    | RuntimeError::BackendUnavailable(_)
                    | RuntimeError::OutOfMemory(_)),
                ) if preference != AccelerationPreference::Cpu && route.provider.is_gpu() => {
                    if let Some(health) = &plan.health {
                        health.record_failure(
                            route,
                            health_observation_for_runtime_error(&error, false, false),
                        );
                    }
                    record_backend_fallback(&mut fallback_history, route, &error);
                    self.invalidate_gpu_catalog_after_runtime_failure(route)?;
                    self.retire_failed_route(route)?;
                    last_retryable = Some(error);
                }
                // This is still before load can publish any output.  An Auto
                // GPU route that fails its private worker protocol must not
                // turn a CPU-safe request into a hard failure; record the
                // protocol failure, retire the worker, and continue to the
                // next bounded candidate (ultimately CPU). Explicit GPU
                // keeps its strict no-CPU-fallback behavior below.
                Err(error @ RuntimeError::WorkerUnavailable(_))
                    if preference != AccelerationPreference::Cpu && route.provider.is_gpu() =>
                {
                    if let Some(health) = &plan.health {
                        health.record_failure(
                            route,
                            health_observation_for_runtime_error(&error, false, false),
                        );
                    }
                    record_backend_fallback(&mut fallback_history, route, &error);
                    self.invalidate_gpu_catalog_after_runtime_failure(route)?;
                    self.retire_failed_route(route)?;
                    last_retryable = Some(RuntimeError::RetryableWorkerFailure(error.to_string()));
                }
                Err(error) => {
                    if let Some(health) = &plan.health {
                        health.record_failure(
                            route,
                            health_observation_for_runtime_error(&error, false, false),
                        );
                    }
                    self.retire_failed_route(route)?;
                    return Err(error);
                }
            }
        }
        if !attempted {
            self.retire_active_worker()?;
        }
        Err(last_retryable.unwrap_or_else(|| {
            debug_assert!(!attempted);
            match plan.diagnostic {
                Some(diagnostic) if preference == AccelerationPreference::Gpu => {
                    RuntimeError::WorkerUnavailable(format!(
                        "no GPU inference worker is registered; CPU fallback is forbidden for explicit GPU; {diagnostic}"
                    ))
                }
                _ => Self::no_route_error(preference),
            }
        }))
    }

    pub(crate) fn transcribe(
        &self,
        artifact: RuntimeArtifact,
        preference: AccelerationPreference,
        audio: &PreparedAudio,
        options: TranscriptionOptions,
        cancellation_snapshot: u64,
        cancellation_generation: &std::sync::atomic::AtomicU64,
    ) -> Result<RuntimeExecution, RuntimeError> {
        let _route_execution = self.lock_route_execution()?;
        let mut last_retryable = None;
        let mut fallback_history = Vec::new();
        let mut attempted = false;
        let plan = self.routes_for_preference(preference, &artifact)?;
        if plan.routes.is_empty() {
            self.retire_active_worker()?;
        }
        for (route_index, route) in plan
            .routes
            .iter()
            .take(MAX_INFERENCE_ROUTE_ATTEMPTS)
            .enumerate()
        {
            if plan
                .health
                .as_ref()
                .is_some_and(|health| !health.consume_retry(route))
            {
                continue;
            }
            attempted = true;
            match self.prepare_route(route) {
                Ok(()) => {}
                Err(RoutePreparationError::DeviceRollbackAuthority(error)) => {
                    self.invalidate_gpu_catalog_after_runtime_failure(route)?;
                    let error = if preference == AccelerationPreference::Gpu {
                        RuntimeError::WorkerUnavailable(format!(
                            "{error}; CPU fallback is forbidden for explicit GPU"
                        ))
                    } else {
                        RuntimeError::RetryableWorkerFailure(error.to_string())
                    };
                    record_backend_fallback(&mut fallback_history, route, &error);
                    last_retryable = Some(error);
                    continue;
                }
                Err(RoutePreparationError::Fatal(error)) => return Err(error),
            }
            match route.supervisor.transcribe(
                artifact.clone(),
                Self::worker_preference_for_route(route, preference),
                audio,
                options.clone(),
                cancellation_snapshot,
                cancellation_generation,
            ) {
                Ok(mut execution) => {
                    if preference == AccelerationPreference::Auto {
                        Self::project_auto_route_diagnostics(
                            &mut execution.diagnostics,
                            route,
                            plan.selection_power_source
                                .unwrap_or_else(PowerSource::current),
                            plan.routes
                                .iter()
                                .skip(route_index + 1)
                                .filter_map(|candidate| candidate.target.clone())
                                .collect(),
                            &fallback_history,
                            &plan.skipped_targets,
                        );
                    }
                    if matches!(artifact, RuntimeArtifact::Gguf(_)) {
                        append_pack_diagnostic(
                            &mut execution.diagnostics.resolved_acceleration,
                            plan.diagnostic.as_deref(),
                        );
                    }
                    return Ok(execution);
                }
                Err(error @ RuntimeError::RetryableWorkerFailure(_)) => {
                    let cancelled =
                        cancellation_generation.load(Ordering::Acquire) != cancellation_snapshot;
                    if let Some(health) = &plan.health {
                        health.record_failure(
                            route,
                            health_observation_for_runtime_error(&error, cancelled, false),
                        );
                    }
                    self.invalidate_gpu_catalog_after_runtime_failure(route)?;
                    self.retire_failed_route(route)?;
                    if cancelled {
                        return Err(RuntimeError::Cancelled(
                            "transcription request was cancelled before output".to_owned(),
                        ));
                    }
                    record_backend_fallback(&mut fallback_history, route, &error);
                    last_retryable = Some(error);
                }
                Err(
                    error @ (RuntimeError::BackendInitializationFailed(_)
                    | RuntimeError::BackendUnavailable(_)
                    | RuntimeError::OutOfMemory(_)),
                ) if preference != AccelerationPreference::Cpu && route.provider.is_gpu() => {
                    let cancelled =
                        cancellation_generation.load(Ordering::Acquire) != cancellation_snapshot;
                    if let Some(health) = &plan.health {
                        health.record_failure(
                            route,
                            health_observation_for_runtime_error(&error, cancelled, false),
                        );
                    }
                    self.invalidate_gpu_catalog_after_runtime_failure(route)?;
                    self.retire_failed_route(route)?;
                    if cancelled {
                        return Err(RuntimeError::Cancelled(
                            "transcription request was cancelled before output".to_owned(),
                        ));
                    }
                    record_backend_fallback(&mut fallback_history, route, &error);
                    last_retryable = Some(error);
                }
                // Transcription has not returned a result at this point, so
                // the worker protocol failure is pre-output. Auto may safely
                // try its next qualified route; cancellation remains a hard
                // stop and explicit GPU remains strict.
                Err(error @ RuntimeError::WorkerUnavailable(_))
                    if preference != AccelerationPreference::Cpu && route.provider.is_gpu() =>
                {
                    let cancelled =
                        cancellation_generation.load(Ordering::Acquire) != cancellation_snapshot;
                    if let Some(health) = &plan.health {
                        health.record_failure(
                            route,
                            health_observation_for_runtime_error(&error, cancelled, false),
                        );
                    }
                    self.invalidate_gpu_catalog_after_runtime_failure(route)?;
                    self.retire_failed_route(route)?;
                    if cancelled {
                        return Err(RuntimeError::Cancelled(
                            "transcription request was cancelled before output".to_owned(),
                        ));
                    }
                    record_backend_fallback(&mut fallback_history, route, &error);
                    last_retryable = Some(RuntimeError::RetryableWorkerFailure(error.to_string()));
                }
                Err(error) => {
                    let cancelled =
                        cancellation_generation.load(Ordering::Acquire) != cancellation_snapshot;
                    if let Some(health) = &plan.health {
                        health.record_failure(
                            route,
                            health_observation_for_runtime_error(&error, cancelled, false),
                        );
                    }
                    self.retire_failed_route(route)?;
                    return Err(error);
                }
            }
        }
        if !attempted {
            self.retire_active_worker()?;
        }
        Err(last_retryable.unwrap_or_else(|| {
            debug_assert!(!attempted);
            match plan.diagnostic {
                Some(diagnostic) if preference == AccelerationPreference::Gpu => {
                    RuntimeError::WorkerUnavailable(format!(
                        "no GPU inference worker is registered; CPU fallback is forbidden for explicit GPU; {diagnostic}"
                    ))
                }
                _ => Self::no_route_error(preference),
            }
        }))
    }

    pub(crate) fn health(
        &self,
        artifact: RuntimeArtifact,
        preference: AccelerationPreference,
    ) -> Result<(), RuntimeError> {
        let _route_execution = self.lock_route_execution()?;
        let mut last_retryable = None;
        let mut attempted = false;
        // A health request is the only execution path allowed to exercise an
        // exact InvalidOrUnprobed route. Normal load/transcription selection
        // remains fail-closed until two successful health probes persist.
        let plan = self.routes_for_preference_with_idle_health(
            preference,
            &artifact,
            preference == AccelerationPreference::Gpu,
        )?;
        if plan.routes.is_empty() {
            self.retire_active_worker()?;
        }
        for route in plan.routes.iter().take(MAX_INFERENCE_ROUTE_ATTEMPTS) {
            if plan
                .health
                .as_ref()
                .is_some_and(|health| !health.consume_retry(route))
            {
                continue;
            }
            attempted = true;
            match self.prepare_route(route) {
                Ok(()) => {}
                Err(RoutePreparationError::DeviceRollbackAuthority(error)) => {
                    self.invalidate_gpu_catalog_after_runtime_failure(route)?;
                    last_retryable = Some(if preference == AccelerationPreference::Gpu {
                        RuntimeError::WorkerUnavailable(format!(
                            "{error}; CPU fallback is forbidden for explicit GPU"
                        ))
                    } else {
                        RuntimeError::RetryableWorkerFailure(error.to_string())
                    });
                    continue;
                }
                Err(RoutePreparationError::Fatal(error)) => return Err(error),
            }
            match route.supervisor.health(
                artifact.clone(),
                Self::worker_preference_for_route(route, preference),
            ) {
                Ok(()) => {
                    if let Some(health) = &plan.health {
                        health.record_idle_probe_success(route);
                    }
                    return Ok(());
                }
                Err(error @ RuntimeError::RetryableWorkerFailure(_)) => {
                    if let Some(health) = &plan.health {
                        health.record_failure(
                            route,
                            health_observation_for_runtime_error(&error, false, false),
                        );
                    }
                    self.invalidate_gpu_catalog_after_runtime_failure(route)?;
                    self.retire_failed_route(route)?;
                    last_retryable = Some(error);
                }
                Err(
                    error @ (RuntimeError::BackendInitializationFailed(_)
                    | RuntimeError::BackendUnavailable(_)
                    | RuntimeError::OutOfMemory(_)),
                ) if preference != AccelerationPreference::Cpu && route.provider.is_gpu() => {
                    if let Some(health) = &plan.health {
                        health.record_failure(
                            route,
                            health_observation_for_runtime_error(&error, false, false),
                        );
                    }
                    self.invalidate_gpu_catalog_after_runtime_failure(route)?;
                    self.retire_failed_route(route)?;
                    last_retryable = Some(error);
                }
                Err(error @ RuntimeError::WorkerUnavailable(_)) if route.provider.is_gpu() => {
                    if let Some(health) = &plan.health {
                        health.record_failure(
                            route,
                            health_observation_for_runtime_error(&error, false, false),
                        );
                    }
                    self.invalidate_gpu_catalog_after_runtime_failure(route)?;
                    self.retire_failed_route(route)?;
                    last_retryable = Some(RuntimeError::RetryableWorkerFailure(error.to_string()));
                }
                Err(error) => {
                    if let Some(health) = &plan.health {
                        health.record_failure(
                            route,
                            health_observation_for_runtime_error(&error, false, false),
                        );
                    }
                    self.retire_failed_route(route)?;
                    return Err(error);
                }
            }
        }
        if !attempted {
            self.retire_active_worker()?;
        }
        Err(last_retryable.unwrap_or_else(|| {
            debug_assert!(!attempted);
            match plan.diagnostic {
                Some(diagnostic) if preference == AccelerationPreference::Gpu => {
                    RuntimeError::WorkerUnavailable(format!(
                        "no GPU inference worker is registered; CPU fallback is forbidden for explicit GPU; {diagnostic}"
                    ))
                }
                _ => Self::no_route_error(preference),
            }
        }))
    }

    pub(crate) fn unload(&self) -> Result<(), RuntimeError> {
        let _route_execution = self.lock_route_execution()?;
        let active = self.active_route.lock().map_err(|_| {
            RuntimeError::WorkerUnavailable("active inference route lock poisoned".to_owned())
        })?;
        if let Some(active) = active.as_ref() {
            active.supervisor.unload()?;
        }
        Ok(())
    }

    pub(crate) fn unload_if_idle(&self) -> Result<(), RuntimeError> {
        let _route_execution = self.lock_route_execution()?;
        let active = self.active_route.lock().map_err(|_| {
            RuntimeError::WorkerUnavailable("active inference route lock poisoned".to_owned())
        })?;
        if let Some(active) = active.as_ref() {
            active.supervisor.unload_if_idle()?;
        }
        Ok(())
    }

    pub(crate) fn cancel_active(&self) {
        if let Ok(active) = self.active_route.lock()
            && let Some(active) = active.as_ref()
        {
            active.supervisor.cancel_active();
        }
    }

    pub(crate) fn shutdown(&self) -> Result<(), RuntimeError> {
        let _route_execution = self.lock_route_execution()?;
        self.retire_active_worker()?;
        for route in self.routes.iter() {
            route.supervisor.shutdown()?;
        }
        if let Ok(routes) = self.gpu_routes.lock()
            && let Some(routes) = routes.successful()
        {
            for route in routes.routes.iter() {
                route.supervisor.shutdown()?;
            }
        }
        if let Ok(routes) = self.auto_gpu_routes.lock()
            && let Some(routes) = routes.successful()
        {
            for route in routes.routes.iter() {
                route.supervisor.shutdown()?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
fn worker_loop(mut input: impl Read, mut output: impl Write) -> Result<()> {
    worker_loop_with_factories(
        &mut input,
        &mut output,
        &DisabledRecognizerFactory,
        &NativeVadFactory,
        None,
        None,
    )
}

fn worker_loop_for_role(
    mut input: impl Read,
    mut output: impl Write,
    role: WorkerRole,
    parent_control: Option<std::fs::File>,
) -> Result<()> {
    debug_assert_eq!(role, WorkerRole::Vad);
    worker_loop_with_factories(
        &mut input,
        &mut output,
        &DisabledRecognizerFactory,
        &NativeVadFactory,
        Some(role),
        parent_control,
    )
}

pub(crate) trait WorkerRecognizerFactory {
    type Recognizer: WorkerRecognizer;

    fn create(&self, model: &ValidatedOnnxModel) -> Result<Self::Recognizer>;
}

pub(crate) trait WorkerRecognizer {
    type Stream;

    fn transcribe(&self, samples: &[f32]) -> Result<String>;
    fn start_stream(&self) -> Result<Self::Stream>;
    fn accept_chunk(&self, stream: &mut Self::Stream, samples: &[f32]) -> Result<()>;
    fn input_finished(&self, stream: &mut Self::Stream) -> Result<()>;
    fn drain_ready(&self, stream: &mut Self::Stream) -> Result<()>;
    fn stream_result(&self, stream: &Self::Stream) -> Result<String>;
}

trait WorkerVadFactory {
    type Vad: WorkerVad;

    fn create(&self, num_threads: u16) -> Result<Self::Vad>;
}

trait WorkerVad {
    fn compute(&mut self, samples: &[f32]) -> Result<f32>;
    fn reset(&mut self) -> Result<()>;
}

struct LoadedWorkerRecognizer<R> {
    family: OnnxModelFamily,
    recognizer: R,
}

struct ActiveWorkerStream<S> {
    session_id: u64,
    last_request_id: u64,
    sample_count: usize,
    stream: S,
}

struct ActiveWorkerVad {
    session_id: u64,
    last_request_id: u64,
    threshold: VadThreshold,
}

#[cfg(test)]
fn worker_loop_with_factory<F: WorkerRecognizerFactory>(
    mut input: impl Read,
    mut output: impl Write,
    factory: &F,
) -> Result<()> {
    worker_loop_with_factories(
        &mut input,
        &mut output,
        factory,
        &NativeVadFactory,
        None,
        None,
    )
}

struct PendingWorkerBatch {
    session_id: u64,
    begin_request_id: u64,
    last_request_id: u64,
    declared_samples: usize,
    artifact: WireRuntimeArtifact,
    preference: AccelerationPreference,
    options: TranscriptionOptions,
    source_sample_rate: u32,
    source_channels: u16,
    source_frames: usize,
    load: WireRuntimeLoadExecution,
    samples: Vec<f32>,
}

#[derive(Clone)]
struct LoadedRuntimeMetadata {
    identity: String,
    artifact: WireRuntimeArtifact,
    load: WireRuntimeLoadExecution,
}

fn worker_loop_with_factories<F: WorkerRecognizerFactory, V: WorkerVadFactory>(
    mut input: impl Read,
    mut output: impl Write,
    factory: &F,
    vad_factory: &V,
    role: Option<WorkerRole>,
    parent_control: Option<std::fs::File>,
) -> Result<()> {
    // This declaration order makes the stream drop before its recognizer on
    // structural protocol failure. Normal replacement paths clear it explicitly.
    let mut loaded: Option<LoadedWorkerRecognizer<F::Recognizer>> = None;
    let mut active_stream: Option<ActiveWorkerStream<<F::Recognizer as WorkerRecognizer>::Stream>> =
        None;
    let mut loaded_vad: Option<V::Vad> = None;
    let mut active_vad: Option<ActiveWorkerVad> = None;
    // worker-only native runtime: the heavyweight router is constructed only
    // after the child entrypoint has claimed the process role.
    let runtime_router = RuntimeRouter::new();
    if let Some(reader) = parent_control {
        start_parent_control_watchdog(
            reader,
            role.filter(|role| *role == WorkerRole::Inference)
                .map(|_| runtime_router.clone()),
        )?;
    }
    let mut loaded_runtime: Option<LoadedRuntimeMetadata> = None;
    let mut pending_batch: Option<PendingWorkerBatch> = None;
    let mut handshake_complete = false;
    loop {
        let frame = match read_frame(&mut input) {
            Ok(frame) => frame,
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|io| io.kind() == std::io::ErrorKind::UnexpectedEof) =>
            {
                return Ok(());
            }
            Err(error) => return Err(error),
        };
        let (session_id, request_id, control) = parse_parent_control(frame)?;
        match (&control, handshake_complete) {
            (Control::Hello { .. }, false) => {}
            (Control::Hello { .. }, true) => {
                bail!("worker protocol permits Hello exactly once");
            }
            (_, false) => {
                bail!("worker protocol requires Hello before any command");
            }
            (_, true) => {}
        }
        if role.is_some_and(|role| !control_allowed_for_role(&control, role)) {
            write_worker_response(
                &mut output,
                session_id,
                request_id,
                Control::Error {
                    message: format!("{role:?} worker rejected a cross-role command"),
                },
            )?;
            continue;
        }
        match control {
            Control::Hello {
                challenge,
                expected,
            } => {
                let actual_role = validate_worker_hello(role, &challenge, &expected)?;
                handshake_complete = true;
                write_worker_response(
                    &mut output,
                    session_id,
                    request_id,
                    Control::Ready {
                        capability: worker_capability(actual_role, challenge)?,
                    },
                )?;
            }
            Control::LoadRuntime {
                artifact,
                preference,
            } => {
                let result = if pending_batch.is_some() {
                    Err(anyhow!("cannot replace a runtime while a batch is active"))
                } else {
                    load_worker_runtime(
                        &runtime_router,
                        factory,
                        &mut loaded,
                        &mut loaded_runtime,
                        &mut active_stream,
                        artifact,
                        preference,
                    )
                    .map(|execution| Control::RuntimeLoaded { execution })
                };
                write_runtime_result(&mut output, session_id, request_id, result)?;
            }
            Control::BeginBatch {
                artifact,
                preference,
                options,
                source_sample_rate,
                source_channels,
                source_frames,
                declared_samples,
            } => {
                let result = (|| {
                    if pending_batch.is_some() {
                        bail!("a runtime batch is already active");
                    }
                    if active_stream.is_some() || loaded_vad.is_some() {
                        bail!("cannot begin a runtime batch while a stream is active");
                    }
                    validate_cumulative_sample_count(declared_samples)?;
                    let samples = reserve_batch_samples(declared_samples)?;
                    let load = load_worker_runtime(
                        &runtime_router,
                        factory,
                        &mut loaded,
                        &mut loaded_runtime,
                        &mut active_stream,
                        artifact.clone(),
                        preference,
                    )?;
                    pending_batch = Some(PendingWorkerBatch {
                        session_id,
                        begin_request_id: request_id,
                        last_request_id: request_id,
                        declared_samples,
                        artifact,
                        preference,
                        options,
                        source_sample_rate,
                        source_channels,
                        source_frames,
                        load,
                        samples,
                    });
                    Ok(Control::Ok)
                })();
                write_runtime_result(&mut output, session_id, request_id, result)?;
            }
            Control::EndBatch => {
                let validation = pending_batch
                    .as_ref()
                    .ok_or_else(|| anyhow!("no runtime batch is active"))
                    .and_then(|batch| {
                        if batch.session_id != session_id {
                            bail!("runtime batch belongs to a different session");
                        }
                        if request_id <= batch.last_request_id {
                            bail!(
                                "runtime batch end request is stale relative to begin request {}",
                                batch.begin_request_id
                            );
                        }
                        if batch.samples.len() != batch.declared_samples {
                            bail!(
                                "runtime batch sample count mismatch: declared {}, received {}",
                                batch.declared_samples,
                                batch.samples.len()
                            );
                        }
                        Ok(())
                    });
                if let Err(error) = validation {
                    pending_batch = None;
                    write_worker_result(&mut output, session_id, request_id, Err(error))?;
                    continue;
                }
                let batch_is_onnx = pending_batch.as_ref().is_some_and(|batch| {
                    matches!(batch.artifact, WireRuntimeArtifact::OnnxBundle(_))
                });
                let result = pending_batch
                    .take()
                    .ok_or_else(|| anyhow!("runtime batch disappeared after validation"))
                    .and_then(|batch| {
                        execute_worker_batch(
                            &runtime_router,
                            loaded.as_ref(),
                            loaded_runtime.as_ref(),
                            batch,
                        )
                    })
                    .map(|execution| Control::RuntimeTranscript { execution });
                if batch_is_onnx && result.is_err() {
                    loaded = None;
                    loaded_runtime = None;
                }
                write_runtime_result(&mut output, session_id, request_id, result)?;
            }
            Control::Health => {
                write_worker_response(&mut output, session_id, request_id, Control::Ok)?;
            }
            Control::Unload => {
                pending_batch = None;
                active_stream = None;
                loaded = None;
                loaded_runtime = None;
                let _ = runtime_router.unload_all();
                active_vad = None;
                loaded_vad = None;
                write_worker_response(&mut output, session_id, request_id, Control::Ok)?;
            }
            Control::Cancel {
                target_session_id,
                target_request_id,
            } => {
                let result = match active_stream.as_ref() {
                    Some(stream)
                        if session_id == target_session_id
                            && stream.session_id == target_session_id
                            && stream.last_request_id == target_request_id =>
                    {
                        active_stream = None;
                        Ok(Control::Ok)
                    }
                    _ => match active_vad.as_ref() {
                        Some(vad)
                            if session_id == target_session_id
                                && vad.session_id == target_session_id
                                && vad.last_request_id == target_request_id =>
                        {
                            active_vad = None;
                            if let Some(vad) = loaded_vad.as_mut() {
                                vad.reset()?;
                            }
                            Ok(Control::Ok)
                        }
                        _ => Err(anyhow!("no matching ONNX stream is active")),
                    },
                };
                write_worker_result(&mut output, session_id, request_id, result)?;
            }
            Control::Shutdown => {
                drop(pending_batch.take());
                drop(active_stream.take());
                drop(loaded.take());
                drop(loaded_runtime.take());
                let _ = runtime_router.unload_all();
                drop(loaded_vad.take());
                write_worker_response(&mut output, session_id, request_id, Control::Ok)?;
                return Ok(());
            }
            Control::StartStream => {
                let result = (|| {
                    if loaded_vad.is_some() {
                        bail!("cannot start transcription in a VAD worker");
                    }
                    let recognizer = loaded
                        .as_ref()
                        .ok_or_else(|| anyhow!("no ONNX model is loaded"))?;
                    if recognizer.family != OnnxModelFamily::OnlineTransducer {
                        bail!("streaming requires an online ONNX transducer");
                    }
                    if active_stream.is_some() {
                        bail!("a worker stream is already active");
                    }
                    active_stream = Some(ActiveWorkerStream {
                        session_id,
                        last_request_id: request_id,
                        sample_count: 0,
                        stream: recognizer.recognizer.start_stream()?,
                    });
                    Ok(Control::Ok)
                })();
                write_worker_result(&mut output, session_id, request_id, result)?;
            }
            Control::AudioChunk => {
                let samples = read_correlated_pcm(&mut input, session_id, request_id);
                if pending_batch.is_some() {
                    let result = samples.and_then(|samples| {
                        let batch = pending_batch.as_mut().expect("runtime batch checked above");
                        if batch.session_id != session_id {
                            bail!("runtime batch belongs to a different session");
                        }
                        if request_id <= batch.last_request_id {
                            bail!("runtime batch audio request is stale");
                        }
                        let next_len =
                            checked_cumulative_sample_count(batch.samples.len(), samples.len())?;
                        if next_len > batch.declared_samples {
                            pending_batch = None;
                            bail!("runtime batch received more samples than it declared");
                        }
                        batch.samples.extend_from_slice(&samples);
                        batch.last_request_id = request_id;
                        Ok(Control::Ok)
                    });
                    if result.is_err() {
                        pending_batch = None;
                    }
                    write_worker_result(&mut output, session_id, request_id, result)?;
                    continue;
                }
                let result = match samples {
                    Ok(samples) => handle_audio_chunk(
                        loaded.as_ref(),
                        &mut active_stream,
                        session_id,
                        request_id,
                        &samples,
                    )
                    .map(|text| Control::Text {
                        text,
                        final_result: false,
                    }),
                    Err(error) => {
                        if active_stream
                            .as_ref()
                            .is_some_and(|stream| stream.session_id == session_id)
                        {
                            active_stream = None;
                        }
                        Err(error)
                    }
                };
                write_worker_result(&mut output, session_id, request_id, result)?;
            }
            Control::EndStream => {
                let result = finish_worker_stream(loaded.as_ref(), &mut active_stream, session_id)
                    .map(|text| Control::Text {
                        text,
                        final_result: true,
                    });
                write_worker_result(&mut output, session_id, request_id, result)?;
            }
            Control::LoadVad { num_threads } => {
                let result = (|| {
                    if !(1..=64).contains(&num_threads) {
                        bail!("Silero VAD thread count must be within [1, 64]");
                    }
                    if loaded.is_some() || active_stream.is_some() {
                        bail!("cannot load VAD in a transcription worker");
                    }
                    if active_vad.is_some() {
                        bail!("cannot replace VAD while a session is active");
                    }
                    loaded_vad = None;
                    loaded_vad = Some(vad_factory.create(num_threads)?);
                    Ok(Control::Ok)
                })();
                write_worker_result(&mut output, session_id, request_id, result)?;
            }
            Control::StartVad { threshold } => {
                let result = (|| {
                    let threshold = VadThreshold::new(threshold)?;
                    let vad = loaded_vad
                        .as_mut()
                        .ok_or_else(|| anyhow!("no Silero VAD model is loaded"))?;
                    if active_vad.is_some() {
                        bail!("a Silero VAD session is already active");
                    }
                    vad.reset()?;
                    active_vad = Some(ActiveWorkerVad {
                        session_id,
                        last_request_id: request_id,
                        threshold,
                    });
                    Ok(Control::Ok)
                })();
                write_worker_result(&mut output, session_id, request_id, result)?;
            }
            Control::VadWindow => {
                let samples = read_correlated_pcm(&mut input, session_id, request_id);
                let result = samples.and_then(|samples| {
                    let active = active_vad
                        .as_mut()
                        .ok_or_else(|| anyhow!("no Silero VAD session is active"))?;
                    if active.session_id != session_id {
                        bail!("Silero VAD session belongs to a different recording");
                    }
                    if samples.len() != WINDOW_SAMPLES {
                        bail!("Silero VAD input must contain exactly {WINDOW_SAMPLES} samples");
                    }
                    let vad = loaded_vad
                        .as_mut()
                        .ok_or_else(|| anyhow!("no Silero VAD model is loaded"))?;
                    let probability = vad.compute(&samples)?;
                    let speech = active.threshold.detects(probability)?;
                    active.last_request_id = request_id;
                    Ok(Control::VadDecision {
                        probability,
                        speech,
                    })
                });
                if result.is_err()
                    && active_vad
                        .as_ref()
                        .is_some_and(|active| active.session_id == session_id)
                {
                    active_vad = None;
                    if let Some(vad) = loaded_vad.as_mut() {
                        let _ = vad.reset();
                    }
                }
                write_worker_result(&mut output, session_id, request_id, result)?;
            }
            Control::ResetVad => {
                let result = (|| {
                    let active = active_vad
                        .as_mut()
                        .filter(|active| active.session_id == session_id)
                        .ok_or_else(|| {
                            anyhow!("no Silero VAD session is active for recording {session_id}")
                        })?;
                    loaded_vad
                        .as_mut()
                        .ok_or_else(|| anyhow!("no Silero VAD model is loaded"))?
                        .reset()?;
                    active.last_request_id = request_id;
                    Ok(Control::Ok)
                })();
                write_worker_result(&mut output, session_id, request_id, result)?;
            }
            Control::EndVad => {
                let result = (|| {
                    if active_vad
                        .as_ref()
                        .is_none_or(|active| active.session_id != session_id)
                    {
                        bail!("no Silero VAD session is active for recording {session_id}");
                    }
                    loaded_vad
                        .as_mut()
                        .ok_or_else(|| anyhow!("no Silero VAD model is loaded"))?
                        .reset()?;
                    active_vad = None;
                    Ok(Control::Ok)
                })();
                write_worker_result(&mut output, session_id, request_id, result)?;
            }
            Control::Ready { .. }
            | Control::RuntimeLoaded { .. }
            | Control::RuntimeTranscript { .. }
            | Control::RuntimeFailed { .. }
            | Control::Text { .. }
            | Control::VadDecision { .. }
            | Control::Ok
            | Control::Error { .. } => {
                bail!("parent sent worker response")
            }
        }
    }
}

fn read_correlated_pcm(
    input: &mut impl Read,
    session_id: u64,
    request_id: u64,
) -> Result<Vec<f32>> {
    let pcm = read_frame(input)?;
    if pcm.kind != FrameKind::Pcm || pcm.session_id != session_id || pcm.request_id != request_id {
        bail!("invalid or mis-correlated ONNX PCM frame");
    }
    decode_pcm(&pcm.body)
}

fn wire_artifact_identity(
    artifact: &WireRuntimeArtifact,
    preference: AccelerationPreference,
) -> Result<String> {
    let material = match artifact {
        WireRuntimeArtifact::OnnxBundle(model) => {
            serde_json::to_vec(&("onnx", model.validated()?, preference))?
        }
        WireRuntimeArtifact::Gguf(model) => {
            serde_json::to_vec(&("gguf", canonical_wire_runtime_model(model)?, preference))?
        }
    };
    Ok(format!("{:x}", Sha256::digest(material)))
}

fn canonical_wire_runtime_model(model: &WireRuntimeModel) -> Result<WireRuntimeModel> {
    let mut canonical = model.clone();
    canonical.path = std::fs::canonicalize(&model.path).map_err(|error| {
        anyhow!("runtime model path cannot be canonicalized for warm identity: {error}")
    })?;
    Ok(canonical)
}

fn onnx_architecture(family: OnnxModelFamily) -> String {
    match family {
        OnnxModelFamily::Moonshine => "moonshine",
        OnnxModelFamily::NemoCtc => "nemo-ctc",
        OnnxModelFamily::Canary => "canary",
        OnnxModelFamily::OfflineTransducer => "offline-transducer",
        OnnxModelFamily::OnlineTransducer => "online-transducer",
    }
    .to_owned()
}

fn onnx_runtime_capabilities(family: OnnxModelFamily) -> RuntimeCapabilities {
    RuntimeCapabilities {
        streaming: family == OnnxModelFamily::OnlineTransducer,
        cancellation: true,
        ..RuntimeCapabilities::default()
    }
}

fn load_worker_runtime<F: WorkerRecognizerFactory>(
    runtime_router: &RuntimeRouter,
    factory: &F,
    loaded_onnx: &mut Option<LoadedWorkerRecognizer<F::Recognizer>>,
    loaded_runtime: &mut Option<LoadedRuntimeMetadata>,
    active_stream: &mut Option<ActiveWorkerStream<<F::Recognizer as WorkerRecognizer>::Stream>>,
    artifact: WireRuntimeArtifact,
    preference: AccelerationPreference,
) -> Result<WireRuntimeLoadExecution> {
    if active_stream.is_some() {
        bail!("cannot replace a runtime while a stream is active");
    }
    artifact.validate()?;
    let identity = wire_artifact_identity(&artifact, preference)?;
    if matches!(artifact, WireRuntimeArtifact::OnnxBundle(_)) {
        // Validate the requested acceleration before considering warm reuse.
        resolve_cpu_only_acceleration(preference)?;
        if let Some(current) = loaded_runtime
            .as_ref()
            .filter(|current| current.identity == identity)
        {
            let mut reused = current.load.clone();
            reused.diagnostics.warm_reused = true;
            reused.diagnostics.model_load_duration_ms = 0;
            return Ok(reused);
        }
    } else if loaded_runtime
        .as_ref()
        .is_some_and(|current| current.identity == identity)
    {
        // The worker-local router owns transcribe-cpp's validation and warm
        // reuse rules. Calling load again preserves that validation while
        // avoiding a destructive unload of the matching native session.
        let execution = runtime_router
            .load(RuntimeArtifact::try_from(artifact.clone())?, preference)
            .map(WireRuntimeLoadExecution::from)
            .map_err(anyhow::Error::new)?;
        if let Some(current) = loaded_runtime.as_mut() {
            current.load = execution.clone();
        }
        return Ok(execution);
    }

    // A changed load is fail-cold: release the prior native owner before
    // validating or constructing its replacement.
    loaded_runtime.take();
    loaded_onnx.take();
    runtime_router
        .unload_all()
        .map_err(|error| anyhow!(error.to_string()))?;

    let load_started = Instant::now();
    let execution = match &artifact {
        WireRuntimeArtifact::OnnxBundle(model) => {
            let acceleration = resolve_cpu_only_acceleration(preference)?;
            let validated = model.validated()?;
            let recognizer = factory.create(&validated)?;
            *loaded_onnx = Some(LoadedWorkerRecognizer {
                family: model.family,
                recognizer,
            });
            WireRuntimeLoadExecution {
                diagnostics: WireRuntimeDiagnostics {
                    resolved_acceleration: acceleration,
                    runtime_location: PathBuf::from("<worker-local native sherpa-onnx>"),
                    warm_reused: false,
                    model_load_duration_ms: u64::try_from(load_started.elapsed().as_millis())
                        .unwrap_or(u64::MAX),
                },
                detected_architecture: onnx_architecture(model.family),
                capabilities: onnx_runtime_capabilities(model.family),
            }
        }
        WireRuntimeArtifact::Gguf(_) => {
            let runtime_artifact = RuntimeArtifact::try_from(artifact.clone())?;
            runtime_router
                .load(runtime_artifact, preference)
                .map(WireRuntimeLoadExecution::from)
                .map_err(anyhow::Error::new)?
        }
    };
    *loaded_runtime = Some(LoadedRuntimeMetadata {
        identity,
        artifact,
        load: execution.clone(),
    });
    Ok(execution)
}

fn execute_worker_batch<R: WorkerRecognizer>(
    runtime_router: &RuntimeRouter,
    loaded_onnx: Option<&LoadedWorkerRecognizer<R>>,
    loaded_runtime: Option<&LoadedRuntimeMetadata>,
    batch: PendingWorkerBatch,
) -> Result<WireRuntimeExecution> {
    validate_cumulative_pcm_samples(&batch.samples)?;
    let loaded = loaded_runtime.ok_or_else(|| anyhow!("no runtime model is loaded"))?;
    if loaded.identity != wire_artifact_identity(&batch.artifact, batch.preference)?
        || loaded.artifact != batch.artifact
    {
        bail!("runtime batch artifact does not match the loaded model");
    }
    let audio = PreparedAudio::from_captured_mono(
        batch.samples,
        batch.source_sample_rate,
        batch.source_channels,
        batch.source_frames,
    )?;
    if audio.sample_rate != PREPARED_SAMPLE_RATE {
        bail!("runtime batch was not prepared at {PREPARED_SAMPLE_RATE} Hz");
    }
    match batch.artifact {
        WireRuntimeArtifact::OnnxBundle(_) => {
            if batch.options != TranscriptionOptions::default() {
                bail!("sherpa-onnx worker currently accepts only default transcription options");
            }
            let recognizer = loaded_onnx.ok_or_else(|| anyhow!("no ONNX model is loaded"))?;
            let started = Instant::now();
            let text = recognizer.recognizer.transcribe(&audio.samples)?;
            let processing_duration_ms =
                u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
            let duration_ms = (audio.samples.len() as u128)
                .saturating_mul(1_000)
                .checked_div(u128::from(PREPARED_SAMPLE_RATE))
                .and_then(|value| u64::try_from(value).ok());
            Ok(WireRuntimeExecution {
                transcript: WireTranscript {
                    segments: if text.is_empty() {
                        Vec::new()
                    } else {
                        vec![WireTranscriptSegment {
                            text: text.clone(),
                            start_ms: None,
                            end_ms: duration_ms,
                            confidence: None,
                        }]
                    },
                    text,
                    detected_language: None,
                    duration_ms,
                },
                diagnostics: batch.load.diagnostics.clone(),
                processing_duration_ms,
            })
        }
        WireRuntimeArtifact::Gguf(_) => runtime_router
            .transcribe(
                RuntimeArtifact::try_from(batch.artifact)?,
                batch.preference,
                &audio,
                &batch.options,
                runtime_router.cancellation_snapshot(),
            )
            .map(WireRuntimeExecution::from)
            .map_err(anyhow::Error::new),
    }
}

fn handle_audio_chunk<R: WorkerRecognizer>(
    loaded: Option<&LoadedWorkerRecognizer<R>>,
    active_stream: &mut Option<ActiveWorkerStream<R::Stream>>,
    session_id: u64,
    request_id: u64,
    samples: &[f32],
) -> Result<String> {
    let recognizer = loaded.ok_or_else(|| anyhow!("no ONNX model is loaded"))?;
    if recognizer.family != OnnxModelFamily::OnlineTransducer {
        bail!("streaming requires an online ONNX transducer");
    }
    let current = active_stream
        .as_ref()
        .ok_or_else(|| anyhow!("no worker stream is active"))?;
    if current.session_id != session_id {
        bail!("ONNX stream belongs to a different session");
    }
    let sample_count = match checked_cumulative_sample_count(current.sample_count, samples.len()) {
        Ok(sample_count) => sample_count,
        Err(error) => {
            *active_stream = None;
            return Err(error);
        }
    };
    let stream = active_stream
        .as_mut()
        .expect("stream existence was validated before cumulative accounting");
    let result = recognizer
        .recognizer
        .accept_chunk(&mut stream.stream, samples)
        .and_then(|()| recognizer.recognizer.drain_ready(&mut stream.stream))
        .and_then(|()| recognizer.recognizer.stream_result(&stream.stream));
    match result {
        Ok(text) => {
            stream.sample_count = sample_count;
            stream.last_request_id = request_id;
            Ok(text)
        }
        Err(error) => {
            *active_stream = None;
            Err(error)
        }
    }
}

fn finish_worker_stream<R: WorkerRecognizer>(
    loaded: Option<&LoadedWorkerRecognizer<R>>,
    active_stream: &mut Option<ActiveWorkerStream<R::Stream>>,
    session_id: u64,
) -> Result<String> {
    let recognizer = loaded.ok_or_else(|| anyhow!("no ONNX model is loaded"))?;
    if recognizer.family != OnnxModelFamily::OnlineTransducer {
        bail!("streaming requires an online ONNX transducer");
    }
    if active_stream
        .as_ref()
        .is_none_or(|stream| stream.session_id != session_id)
    {
        bail!("no worker stream is active for session {session_id}");
    }
    let mut stream = active_stream.take().expect("stream checked above");
    recognizer
        .recognizer
        .input_finished(&mut stream.stream)
        .and_then(|()| recognizer.recognizer.drain_ready(&mut stream.stream))
        .and_then(|()| recognizer.recognizer.stream_result(&stream.stream))
}

fn write_worker_response(
    output: &mut impl Write,
    session_id: u64,
    request_id: u64,
    response: Control,
) -> Result<()> {
    let response = if validate_worker_response(&response).is_ok() {
        response
    } else {
        Control::Error {
            message: "worker response exceeded the private protocol limit".to_owned(),
        }
    };
    write_frame(output, &control_frame(session_id, request_id, &response)?)
}

fn validate_worker_response(response: &Control) -> Result<()> {
    match response {
        Control::RuntimeLoaded { execution } => validate_wire_load(execution),
        Control::RuntimeTranscript { execution } => {
            validate_wire_transcript(&execution.transcript)?;
            validate_wire_diagnostics(&execution.diagnostics)
        }
        Control::RuntimeFailed { error } => {
            validate_bounded_string("runtime error", &error.message, MAX_WORKER_ERROR_BYTES)?;
            if let Some(model_id) = &error.model_id {
                validate_bounded_string("runtime error model id", model_id, 512)?;
            }
            Ok(())
        }
        Control::Text { text, .. } => {
            validate_bounded_string("worker transcript", text, MAX_TRANSCRIPT_TEXT_BYTES)
        }
        Control::VadDecision { probability, .. } => {
            if !probability.is_finite() || !(0.0..=1.0).contains(probability) {
                bail!("worker VAD probability is invalid");
            }
            Ok(())
        }
        Control::Error { message } => {
            validate_bounded_string("worker error", message, MAX_WORKER_ERROR_BYTES)
        }
        Control::Ready { capability } => {
            validate_nonempty_bounded_string(
                "worker capability challenge",
                &capability.challenge,
                64,
            )?;
            if capability.artifacts.len() > 8 {
                bail!("worker capability advertises too many artifact targets");
            }
            for target in &capability.artifacts {
                validate_nonempty_bounded_string(
                    "worker artifact target",
                    &target.target,
                    MAX_BACKEND_IDENTITY_BYTES,
                )?;
            }
            Ok(())
        }
        Control::Ok => Ok(()),
        _ => bail!("attempted to serialize a command-only worker response"),
    }?;
    let serialized = serde_json::to_vec(response)?;
    if serialized.len() > MAX_CONTROL_BYTES {
        bail!("serialized worker response exceeds the private control-frame limit");
    }
    Ok(())
}

fn validate_wire_load(load: &WireRuntimeLoadExecution) -> Result<()> {
    validate_wire_diagnostics(&load.diagnostics)?;
    validate_bounded_string(
        "detected architecture",
        &load.detected_architecture,
        MAX_ARCHITECTURE_BYTES,
    )?;
    if load.capabilities.supported_languages.len() > MAX_SUPPORTED_LANGUAGES {
        bail!("worker capability language list is oversized");
    }
    let mut total = 0_usize;
    for language in &load.capabilities.supported_languages {
        validate_bounded_string("supported language", language, MAX_LANGUAGE_BYTES)?;
        total = total
            .checked_add(language.len())
            .ok_or_else(|| anyhow!("worker capability language bytes overflowed"))?;
    }
    if total > MAX_DIAGNOSTIC_BYTES {
        bail!("worker capability language bytes are oversized");
    }
    Ok(())
}

fn validate_wire_diagnostics(diagnostics: &WireRuntimeDiagnostics) -> Result<()> {
    if diagnostics.runtime_location.as_os_str().is_empty()
        || diagnostics.runtime_location.to_string_lossy().len() > 32 * 1024
    {
        bail!("worker runtime location is empty or oversized");
    }
    if let Some(diagnostic) = &diagnostics.resolved_acceleration.diagnostic {
        validate_bounded_string("acceleration diagnostic", diagnostic, MAX_DIAGNOSTIC_BYTES)?;
    }
    if let ComputeDevice::Gpu { name } = &diagnostics.resolved_acceleration.resolved {
        validate_bounded_string("GPU name", name, MAX_DIAGNOSTIC_BYTES)?;
    }
    if let Some(selection) = &diagnostics.resolved_acceleration.selection {
        validate_backend_selection(selection)?;
    }
    Ok(())
}

fn validate_backend_selection(selection: &BackendSelection) -> Result<()> {
    let target_count = 1_usize
        .checked_add(selection.fallback_targets.len())
        .and_then(|count| count.checked_add(selection.fallback_history.len()))
        .and_then(|count| count.checked_add(selection.skipped_targets.len()))
        .ok_or_else(|| anyhow!("worker backend selection target count overflowed"))?;
    if target_count > MAX_BACKEND_SELECTION_TARGETS {
        bail!("worker backend selection contains too many targets");
    }
    validate_backend_target(&selection.target)?;
    for target in &selection.fallback_targets {
        validate_backend_target(target)?;
    }
    for fallback in &selection.fallback_history {
        validate_backend_target(&fallback.target)?;
    }
    for skipped in &selection.skipped_targets {
        validate_backend_target(&skipped.target)?;
    }
    Ok(())
}

fn validate_backend_target(target: &BackendTarget) -> Result<()> {
    validate_nonempty_bounded_string(
        "backend provider identity",
        target.provider_id.as_str(),
        MAX_BACKEND_IDENTITY_BYTES,
    )?;
    validate_nonempty_bounded_string(
        "backend device identity",
        target.device_id.as_str(),
        MAX_BACKEND_IDENTITY_BYTES,
    )?;
    validate_nonempty_bounded_string(
        "backend display name",
        &target.display_name,
        MAX_DIAGNOSTIC_BYTES,
    )?;
    if let Some(driver_version) = &target.driver_version {
        validate_bounded_string(
            "backend driver version",
            driver_version,
            MAX_BACKEND_IDENTITY_BYTES,
        )?;
    }
    Ok(())
}

fn validate_nonempty_bounded_string(label: &str, value: &str, max_bytes: usize) -> Result<()> {
    validate_bounded_string(label, value, max_bytes)?;
    if value.trim().is_empty() {
        bail!("{label} is empty");
    }
    Ok(())
}

fn validate_wire_transcript(transcript: &WireTranscript) -> Result<()> {
    validate_bounded_string(
        "transcript text",
        &transcript.text,
        MAX_TRANSCRIPT_TEXT_BYTES,
    )?;
    if transcript.segments.len() > MAX_TRANSCRIPT_SEGMENTS {
        bail!("worker transcript has too many segments");
    }
    if let Some(language) = &transcript.detected_language {
        validate_bounded_string("detected language", language, MAX_LANGUAGE_BYTES)?;
    }
    let mut segment_bytes = 0_usize;
    for segment in &transcript.segments {
        validate_bounded_string("segment text", &segment.text, MAX_SEGMENT_TEXT_BYTES)?;
        if segment.confidence.is_some_and(|value| !value.is_finite()) {
            bail!("worker transcript segment confidence is non-finite");
        }
        segment_bytes = segment_bytes
            .checked_add(segment.text.len())
            .ok_or_else(|| anyhow!("worker transcript segment bytes overflowed"))?;
    }
    if segment_bytes > MAX_TRANSCRIPT_TEXT_BYTES {
        bail!("worker transcript segment text is oversized");
    }
    Ok(())
}

fn validate_bounded_string(label: &str, value: &str, max_bytes: usize) -> Result<()> {
    if value.len() > max_bytes {
        bail!("{label} exceeds the {max_bytes}-byte worker protocol limit");
    }
    Ok(())
}

fn write_worker_result(
    output: &mut impl Write,
    session_id: u64,
    request_id: u64,
    result: Result<Control>,
) -> Result<()> {
    let response = match result {
        Ok(response) => response,
        Err(error) => Control::Error {
            message: error.to_string(),
        },
    };
    write_worker_response(output, session_id, request_id, response)
}

fn write_runtime_result(
    output: &mut impl Write,
    session_id: u64,
    request_id: u64,
    result: Result<Control>,
) -> Result<()> {
    let response = match result {
        Ok(response) => response,
        Err(error) => match error.downcast_ref::<RuntimeError>() {
            Some(runtime) => Control::RuntimeFailed {
                error: WireRuntimeError::from_runtime(runtime),
            },
            None => Control::RuntimeFailed {
                error: WireRuntimeError::from_runtime(&RuntimeError::Engine(error.to_string())),
            },
        },
    };
    write_worker_response(output, session_id, request_id, response)
}

struct DisabledRecognizerFactory;

struct DisabledRecognizer;

struct DisabledRecognizerStream;

struct NativeVadFactory;

impl WorkerVadFactory for NativeVadFactory {
    type Vad = SileroVadModel;

    fn create(&self, num_threads: u16) -> Result<Self::Vad> {
        SileroVadModel::load_bundled(i32::from(num_threads))
    }
}

impl WorkerVad for SileroVadModel {
    fn compute(&mut self, samples: &[f32]) -> Result<f32> {
        SileroVadModel::compute(self, samples)
    }

    fn reset(&mut self) -> Result<()> {
        SileroVadModel::reset(self)
    }
}

impl WorkerRecognizerFactory for DisabledRecognizerFactory {
    type Recognizer = DisabledRecognizer;

    fn create(&self, _model: &ValidatedOnnxModel) -> Result<Self::Recognizer> {
        bail!("ASR recognizers are unavailable in the desktop executable")
    }
}

impl WorkerRecognizer for DisabledRecognizer {
    type Stream = DisabledRecognizerStream;

    fn transcribe(&self, _samples: &[f32]) -> Result<String> {
        bail!("ASR recognizers are unavailable in the desktop executable")
    }

    fn start_stream(&self) -> Result<Self::Stream> {
        bail!("ASR recognizers are unavailable in the desktop executable")
    }

    fn accept_chunk(&self, _stream: &mut Self::Stream, _samples: &[f32]) -> Result<()> {
        bail!("ASR recognizers are unavailable in the desktop executable")
    }

    fn input_finished(&self, _stream: &mut Self::Stream) -> Result<()> {
        bail!("ASR recognizers are unavailable in the desktop executable")
    }

    fn drain_ready(&self, _stream: &mut Self::Stream) -> Result<()> {
        bail!("ASR recognizers are unavailable in the desktop executable")
    }

    fn stream_result(&self, _stream: &Self::Stream) -> Result<String> {
        bail!("ASR recognizers are unavailable in the desktop executable")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend_policy::{
        BackendCandidate, BackendFailureCategory, BackendFallback, BackendKind,
        BackendQualificationPolicy, BackendSnapshot, BackendTarget, CandidateAvailability,
        DeviceClass, DeviceIdentity, GpuVendor, OperatingSystem, PowerSource, ProviderIdentity,
        select_backend,
    };
    use crate::prepared_audio::PreparedAudio;
    use std::collections::VecDeque;
    use std::io::Cursor;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::mpsc::{Receiver as TestReceiver, Sender as TestSender, channel};
    use std::thread::JoinHandle;
    use std::time::{Duration, Instant};

    enum PipeChunk {
        Bytes(Vec<u8>),
        Eof,
    }

    struct ChannelWriter {
        sender: TestSender<PipeChunk>,
    }

    impl Write for ChannelWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.sender
                .send(PipeChunk::Bytes(bytes.to_vec()))
                .map_err(|_| std::io::Error::new(std::io::ErrorKind::BrokenPipe, "pipe closed"))?;
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    struct ChannelReader {
        receiver: TestReceiver<PipeChunk>,
        buffered: Cursor<Vec<u8>>,
        eof: bool,
    }

    impl ChannelReader {
        fn new(receiver: TestReceiver<PipeChunk>) -> Self {
            Self {
                receiver,
                buffered: Cursor::new(Vec::new()),
                eof: false,
            }
        }
    }

    impl Read for ChannelReader {
        fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
            loop {
                let read = self.buffered.read(output)?;
                if read != 0 || self.eof {
                    return Ok(read);
                }
                match self.receiver.recv() {
                    Ok(PipeChunk::Bytes(bytes)) => self.buffered = Cursor::new(bytes),
                    Ok(PipeChunk::Eof) | Err(_) => self.eof = true,
                }
            }
        }
    }

    #[derive(Clone, Debug, Default)]
    struct FakeRecognizerState {
        create_attempts: usize,
        recognizers_created: usize,
        recognizer_drops: usize,
        transcriptions: usize,
        stream_starts: usize,
        chunks_accepted: usize,
        input_finished: usize,
        drains: usize,
        result_reads: usize,
        stream_drops: usize,
        offline_stream_backend_calls: usize,
        events: Vec<String>,
    }

    struct FakeRecognizerFactory {
        state: Arc<Mutex<FakeRecognizerState>>,
    }

    impl FakeRecognizerFactory {
        fn new() -> Self {
            Self {
                state: Arc::new(Mutex::new(FakeRecognizerState::default())),
            }
        }

        fn snapshot(&self) -> FakeRecognizerState {
            self.state.lock().unwrap().clone()
        }
    }

    struct FakeRecognizer {
        id: String,
        family: OnnxModelFamily,
        state: Arc<Mutex<FakeRecognizerState>>,
    }

    struct FakeOnlineStream {
        chunks: usize,
        finished: bool,
        state: Arc<Mutex<FakeRecognizerState>>,
    }

    impl Drop for FakeRecognizer {
        fn drop(&mut self) {
            let mut state = self.state.lock().unwrap();
            state.recognizer_drops += 1;
            state.events.push(format!("drop-recognizer:{}", self.id));
        }
    }

    impl Drop for FakeOnlineStream {
        fn drop(&mut self) {
            let mut state = self.state.lock().unwrap();
            state.stream_drops += 1;
            state.events.push("drop-stream".to_owned());
        }
    }

    impl WorkerRecognizerFactory for FakeRecognizerFactory {
        type Recognizer = FakeRecognizer;

        fn create(&self, model: &ValidatedOnnxModel) -> Result<Self::Recognizer> {
            let mut state = self.state.lock().unwrap();
            state.create_attempts += 1;
            state.events.push(format!("create-attempt:{}", model.id));
            if model.id.starts_with("fail-") {
                bail!("fake recognizer construction failed for {}", model.id);
            }
            state.recognizers_created += 1;
            state.events.push(format!("create:{}", model.id));
            drop(state);
            Ok(FakeRecognizer {
                id: model.id.clone(),
                family: model.family,
                state: Arc::clone(&self.state),
            })
        }
    }

    impl FakeRecognizer {
        fn require_online(&self) -> Result<()> {
            if self.family == OnnxModelFamily::OnlineTransducer {
                return Ok(());
            }
            self.state.lock().unwrap().offline_stream_backend_calls += 1;
            bail!("fake offline recognizer cannot stream")
        }
    }

    impl WorkerRecognizer for FakeRecognizer {
        type Stream = FakeOnlineStream;

        fn transcribe(&self, samples: &[f32]) -> Result<String> {
            validate_pcm_samples(samples)?;
            let mut state = self.state.lock().unwrap();
            state.transcriptions += 1;
            state.events.push(format!("transcribe:{}", self.id));
            if self.id.starts_with("decode-failure") {
                bail!("fake recognizer decode failed for {}", self.id);
            }
            Ok(format!("batch:{}:{}", self.id, state.transcriptions))
        }

        fn start_stream(&self) -> Result<Self::Stream> {
            self.require_online()?;
            let mut state = self.state.lock().unwrap();
            state.stream_starts += 1;
            state.events.push("start-stream".to_owned());
            drop(state);
            Ok(FakeOnlineStream {
                chunks: 0,
                finished: false,
                state: Arc::clone(&self.state),
            })
        }

        fn accept_chunk(&self, stream: &mut Self::Stream, samples: &[f32]) -> Result<()> {
            self.require_online()?;
            validate_pcm_samples(samples)?;
            stream.chunks += 1;
            self.state.lock().unwrap().chunks_accepted += 1;
            Ok(())
        }

        fn input_finished(&self, stream: &mut Self::Stream) -> Result<()> {
            self.require_online()?;
            stream.finished = true;
            self.state.lock().unwrap().input_finished += 1;
            Ok(())
        }

        fn drain_ready(&self, _stream: &mut Self::Stream) -> Result<()> {
            self.require_online()?;
            self.state.lock().unwrap().drains += 1;
            Ok(())
        }

        fn stream_result(&self, stream: &Self::Stream) -> Result<String> {
            self.require_online()?;
            self.state.lock().unwrap().result_reads += 1;
            let stage = if stream.finished { "final" } else { "partial" };
            Ok(format!("{stage}-{}", stream.chunks))
        }
    }

    #[derive(Clone, Debug, Default)]
    struct FakeVadState {
        creates: usize,
        computes: usize,
        resets: usize,
        drops: usize,
    }

    struct FakeVadFactory {
        state: Arc<Mutex<FakeVadState>>,
    }

    impl FakeVadFactory {
        fn new() -> Self {
            Self {
                state: Arc::new(Mutex::new(FakeVadState::default())),
            }
        }

        fn snapshot(&self) -> FakeVadState {
            self.state.lock().unwrap().clone()
        }
    }

    struct FakeVad {
        state: Arc<Mutex<FakeVadState>>,
        window_index: usize,
    }

    impl WorkerVadFactory for FakeVadFactory {
        type Vad = FakeVad;

        fn create(&self, num_threads: u16) -> Result<Self::Vad> {
            if !(1..=64).contains(&num_threads) {
                bail!("invalid fake VAD thread count");
            }
            self.state.lock().unwrap().creates += 1;
            Ok(FakeVad {
                state: Arc::clone(&self.state),
                window_index: 0,
            })
        }
    }

    impl WorkerVad for FakeVad {
        fn compute(&mut self, samples: &[f32]) -> Result<f32> {
            validate_pcm_samples(samples)?;
            if samples.len() != WINDOW_SAMPLES {
                bail!("fake VAD requires an exact window");
            }
            let probability = [0.4, 0.7][self.window_index.min(1)];
            self.window_index += 1;
            self.state.lock().unwrap().computes += 1;
            Ok(probability)
        }

        fn reset(&mut self) -> Result<()> {
            self.window_index = 0;
            self.state.lock().unwrap().resets += 1;
            Ok(())
        }
    }

    impl Drop for FakeVad {
        fn drop(&mut self) {
            self.state.lock().unwrap().drops += 1;
        }
    }

    struct TestProcess {
        running: AtomicBool,
        input: TestSender<PipeChunk>,
        output: TestSender<PipeChunk>,
        worker: Mutex<Option<JoinHandle<()>>>,
        kill_started: Option<TestSender<()>>,
        reaped: Option<TestSender<()>>,
    }

    impl WorkerProcess for TestProcess {
        fn is_running(&self) -> Result<bool> {
            Ok(self.running.load(Ordering::Acquire))
        }

        fn terminate(&self) -> Result<()> {
            if let Some(kill_started) = &self.kill_started {
                let _ = kill_started.send(());
            }
            self.running.store(false, Ordering::Release);
            let _ = self.input.send(PipeChunk::Eof);
            let _ = self.output.send(PipeChunk::Eof);
            Ok(())
        }

        fn wait(&self) -> Result<()> {
            if let Some(worker) = self
                .worker
                .lock()
                .map_err(|_| anyhow!("test worker lock poisoned"))?
                .take()
            {
                let _ = worker.join();
            }
            if let Some(reaped) = &self.reaped {
                let _ = reaped.send(());
            }
            Ok(())
        }
    }

    struct CooperativeCancelProcess {
        acknowledge: bool,
        cooperative_requests: AtomicUsize,
        terminated: AtomicBool,
        inner: Mutex<Option<Weak<SupervisorInner>>>,
        correlation: Correlation,
    }

    impl CooperativeCancelProcess {
        fn new(acknowledge: bool, correlation: Correlation) -> Self {
            Self {
                acknowledge,
                cooperative_requests: AtomicUsize::new(0),
                terminated: AtomicBool::new(false),
                inner: Mutex::new(None),
                correlation,
            }
        }
    }

    impl WorkerProcess for CooperativeCancelProcess {
        fn is_running(&self) -> Result<bool> {
            Ok(!self.terminated.load(Ordering::Acquire))
        }

        fn request_cooperative_cancel(&self) -> Result<bool> {
            self.cooperative_requests.fetch_add(1, Ordering::AcqRel);
            if self.acknowledge
                && let Some(inner) = self.inner.lock().unwrap().as_ref().and_then(Weak::upgrade)
            {
                if let Some(waiter) = inner.pending.lock().unwrap().remove(&self.correlation) {
                    let _ = waiter.send(Err("cooperative cancellation acknowledged".to_owned()));
                }
                if let Ok(mut state) = inner.state.lock()
                    && state.active_request == Some(self.correlation)
                {
                    state.active_request = None;
                }
            }
            Ok(true)
        }

        fn terminate(&self) -> Result<()> {
            self.terminated.store(true, Ordering::Release);
            Ok(())
        }

        fn wait(&self) -> Result<()> {
            Ok(())
        }
    }

    #[derive(Clone, Copy, Debug)]
    enum BlockedVadOperation {
        Load,
        Start,
        Health,
        Compute,
        Reset,
        End,
        Cancel,
    }

    #[derive(Clone, Copy, Debug)]
    enum CapabilityMismatch {
        Challenge,
        AppBuild,
        WorkerBuild,
        Abi,
        Role,
        ProviderArtifacts,
    }

    enum TestMode {
        Normal,
        CapabilityMismatch(CapabilityMismatch),
        DelayedLaunch {
            started: TestSender<()>,
            release: TestReceiver<()>,
            then: Box<TestMode>,
        },
        BlockedHello {
            started: TestSender<()>,
        },
        BlockedStreamOperation {
            end_stream: bool,
            started: TestSender<()>,
        },
        AbandonStream {
            completed: TestSender<()>,
        },
        HoldOne {
            started: TestSender<()>,
            release: TestReceiver<()>,
        },
        HoldCancel {
            started: TestSender<()>,
            release: TestReceiver<()>,
        },
        HoldStale {
            started: TestSender<()>,
            release: TestReceiver<()>,
            sent: TestSender<()>,
        },
        FailRequest {
            received: TestSender<()>,
            malformed: bool,
        },
        InvalidResponse {
            pcm_kind: bool,
        },
        RuntimeFailureOnLoad(RuntimeError),
        RuntimeLoadThenExit,
        VadNormal,
        VadCrashOnWindow,
        VadCrashOnReset,
        VadCrashOnEnd,
        BlockedVad {
            operation: BlockedVadOperation,
            started: TestSender<()>,
        },
        CumulativeVadAcquisition {
            stage_delay: Duration,
            blocked: TestSender<()>,
        },
        MalformedVadWindow,
    }

    struct TestLauncher {
        modes: Mutex<VecDeque<TestMode>>,
        launches: AtomicUsize,
        kill_started: Option<TestSender<()>>,
        reaped: Option<TestSender<()>>,
    }

    impl TestLauncher {
        fn new(modes: impl IntoIterator<Item = TestMode>) -> Self {
            Self {
                modes: Mutex::new(modes.into_iter().collect()),
                launches: AtomicUsize::new(0),
                kill_started: None,
                reaped: None,
            }
        }

        fn with_process_events(
            mut self,
            kill_started: TestSender<()>,
            reaped: TestSender<()>,
        ) -> Self {
            self.kill_started = Some(kill_started);
            self.reaped = Some(reaped);
            self
        }
    }

    impl WorkerLauncher for TestLauncher {
        fn launch(&self) -> Result<SpawnedWorker> {
            let mode = self
                .modes
                .lock()
                .map_err(|_| anyhow!("test launcher lock poisoned"))?
                .pop_front()
                .ok_or_else(|| anyhow!("test launcher has no generation script"))?;
            let mode = match mode {
                TestMode::DelayedLaunch {
                    started,
                    release,
                    then,
                } => {
                    started.send(()).unwrap();
                    release.recv().unwrap();
                    *then
                }
                mode => mode,
            };
            self.launches.fetch_add(1, Ordering::AcqRel);
            let (parent_input, worker_input) = channel();
            let (worker_output, parent_output) = channel();
            let process = Arc::new(TestProcess {
                running: AtomicBool::new(true),
                input: parent_input.clone(),
                output: worker_output.clone(),
                worker: Mutex::new(None),
                kill_started: self.kill_started.clone(),
                reaped: self.reaped.clone(),
            });
            let worker_process = Arc::clone(&process);
            let worker_output_for_exit = worker_output.clone();
            let worker = std::thread::spawn(move || {
                let input = ChannelReader::new(worker_input);
                let output = ChannelWriter {
                    sender: worker_output,
                };
                run_test_worker(input, output, mode, &worker_process.running);
                worker_process.running.store(false, Ordering::Release);
                let _ = worker_output_for_exit.send(PipeChunk::Eof);
            });
            *process.worker.lock().unwrap() = Some(worker);
            Ok(SpawnedWorker {
                stdin: Box::new(ChannelWriter {
                    sender: parent_input,
                }),
                stdout: Box::new(ChannelReader::new(parent_output)),
                process,
                expectation: expected_worker(WorkerRole::Inference),
                pack_launch: None,
            })
        }
    }

    fn read_parent_control(input: &mut impl Read) -> (u64, u64, Control) {
        parse_parent_control(read_frame(input).unwrap()).unwrap()
    }

    fn respond(output: &mut impl Write, session_id: u64, request_id: u64, control: Control) {
        let _ = write_frame(
            output,
            &control_frame(session_id, request_id, &control).unwrap(),
        );
    }

    fn test_hello(role: WorkerRole) -> Control {
        Control::Hello {
            challenge: "ab".repeat(32),
            expected: expected_worker(role),
        }
    }

    fn handshake(input: &mut impl Read, output: &mut impl Write) -> bool {
        let Ok(frame) = read_frame(input) else {
            return false;
        };
        let (session_id, request_id, control) = parse_parent_control(frame).unwrap();
        let Control::Hello {
            challenge,
            expected,
        } = control
        else {
            panic!("expected worker Hello");
        };
        let role = validate_worker_hello(None, &challenge, &expected).unwrap();
        respond(
            output,
            session_id,
            request_id,
            Control::Ready {
                capability: worker_capability(role, challenge).unwrap(),
            },
        );
        true
    }

    fn run_normal_worker(input: &mut impl Read, output: &mut impl Write) {
        loop {
            let Ok(frame) = read_frame(input) else {
                return;
            };
            let (session_id, request_id, control) = parse_parent_control(frame).unwrap();
            match control {
                Control::Shutdown => {
                    respond(output, session_id, request_id, Control::Ok);
                    return;
                }
                Control::Hello { .. }
                | Control::Cancel { .. }
                | Control::Unload
                | Control::Health => respond(output, session_id, request_id, Control::Ok),
                Control::LoadRuntime { .. } | Control::BeginBatch { .. } | Control::EndBatch => {
                    respond(
                        output,
                        session_id,
                        request_id,
                        Control::Error {
                            message: "generic runtime unavailable in legacy fake".to_owned(),
                        },
                    )
                }
                Control::StartStream | Control::AudioChunk | Control::EndStream => respond(
                    output,
                    session_id,
                    request_id,
                    Control::Error {
                        message: "streaming unavailable".to_owned(),
                    },
                ),
                Control::VadWindow => {
                    let pcm = read_frame(input).unwrap();
                    assert_eq!(pcm.kind, FrameKind::Pcm);
                    assert_eq!((pcm.session_id, pcm.request_id), (session_id, request_id));
                    respond(
                        output,
                        session_id,
                        request_id,
                        Control::Error {
                            message: "VAD unavailable".to_owned(),
                        },
                    );
                }
                Control::LoadVad { .. }
                | Control::StartVad { .. }
                | Control::ResetVad
                | Control::EndVad => respond(
                    output,
                    session_id,
                    request_id,
                    Control::Error {
                        message: "VAD unavailable".to_owned(),
                    },
                ),
                Control::Ready { .. }
                | Control::RuntimeLoaded { .. }
                | Control::RuntimeTranscript { .. }
                | Control::RuntimeFailed { .. }
                | Control::Text { .. }
                | Control::VadDecision { .. }
                | Control::Ok
                | Control::Error { .. } => {
                    panic!("test parent sent response-only control")
                }
            }
        }
    }

    fn run_test_worker(
        mut input: impl Read,
        mut output: impl Write,
        mode: TestMode,
        running: &AtomicBool,
    ) {
        let mode = match mode {
            TestMode::CapabilityMismatch(mismatch) => {
                let (session_id, request_id, control) = read_parent_control(&mut input);
                let Control::Hello {
                    challenge,
                    expected,
                } = control
                else {
                    panic!("expected worker Hello");
                };
                let mut capability = worker_capability(expected.role, challenge).unwrap();
                match mismatch {
                    CapabilityMismatch::Challenge => capability.challenge = "cd".repeat(32),
                    CapabilityMismatch::AppBuild => capability.app_build.push_str("-wrong"),
                    CapabilityMismatch::WorkerBuild => capability.worker_build.push_str("-wrong"),
                    CapabilityMismatch::Abi => capability.abi = capability.abi.saturating_add(1),
                    CapabilityMismatch::Role => {
                        capability.role = match capability.role {
                            WorkerRole::Inference => WorkerRole::Vad,
                            WorkerRole::Vad => WorkerRole::Inference,
                        };
                    }
                    CapabilityMismatch::ProviderArtifacts => capability.artifacts.clear(),
                }
                respond(
                    &mut output,
                    session_id,
                    request_id,
                    Control::Ready { capability },
                );
                return;
            }
            TestMode::BlockedHello { started } => {
                let Ok(frame) = read_frame(&mut input) else {
                    return;
                };
                let (_, _, control) = parse_parent_control(frame).unwrap();
                assert!(matches!(control, Control::Hello { .. }));
                started.send(()).unwrap();
                let _ = read_frame(&mut input);
                return;
            }
            mode => mode,
        };
        if !handshake(&mut input, &mut output) {
            return;
        }
        match mode {
            TestMode::DelayedLaunch { .. }
            | TestMode::CapabilityMismatch(_)
            | TestMode::BlockedHello { .. } => {
                unreachable!("launch-only test mode reached a worker")
            }
            TestMode::Normal => run_normal_worker(&mut input, &mut output),
            TestMode::VadNormal => {
                run_vad_test_worker(&mut input, &mut output, false, false, false)
            }
            TestMode::VadCrashOnWindow => {
                run_vad_test_worker(&mut input, &mut output, true, false, false)
            }
            TestMode::VadCrashOnReset => {
                run_vad_test_worker(&mut input, &mut output, false, true, false)
            }
            TestMode::VadCrashOnEnd => {
                run_vad_test_worker(&mut input, &mut output, false, false, true)
            }
            TestMode::BlockedVad { operation, started } => {
                run_blocked_vad_worker(&mut input, &mut output, operation, started)
            }
            TestMode::CumulativeVadAcquisition {
                stage_delay,
                blocked,
            } => {
                run_cumulative_vad_acquisition_worker(&mut input, &mut output, stage_delay, blocked)
            }
            TestMode::MalformedVadWindow => {
                run_malformed_vad_window_worker(&mut input, &mut output)
            }
            TestMode::BlockedStreamOperation {
                end_stream,
                started,
            } => {
                let (session_id, request_id, control) = read_parent_control(&mut input);
                assert!(matches!(control, Control::StartStream));
                respond(&mut output, session_id, request_id, Control::Ok);

                let (operation_session, operation_request, control) =
                    read_parent_control(&mut input);
                if end_stream {
                    assert!(matches!(control, Control::EndStream));
                } else {
                    assert!(matches!(control, Control::AudioChunk));
                    let pcm = read_frame(&mut input).unwrap();
                    assert_eq!(
                        (pcm.session_id, pcm.request_id),
                        (operation_session, operation_request)
                    );
                }
                started.send(()).unwrap();
                let _ = read_frame(&mut input);
            }
            TestMode::AbandonStream { completed } => {
                let (session_id, request_id, control) = read_parent_control(&mut input);
                assert!(matches!(control, Control::StartStream));
                respond(&mut output, session_id, request_id, Control::Ok);
                let error = read_frame(&mut input).expect_err("abandon must close the fake pipe");
                assert_eq!(
                    error
                        .downcast_ref::<std::io::Error>()
                        .map(std::io::Error::kind),
                    Some(std::io::ErrorKind::UnexpectedEof)
                );
                completed.send(()).unwrap();
            }
            TestMode::HoldOne { started, release } => {
                let (session_id, request_id, control) = read_parent_control(&mut input);
                assert!(matches!(control, Control::Health));
                started.send(()).unwrap();
                if release.recv().is_ok() {
                    respond(&mut output, session_id, request_id, Control::Ok);
                    run_normal_worker(&mut input, &mut output);
                }
            }
            TestMode::HoldCancel { started, release } => {
                let (session_id, request_id, control) = read_parent_control(&mut input);
                assert!(matches!(control, Control::StartStream));
                respond(&mut output, session_id, request_id, Control::Ok);
                let (session_id, request_id, control) = read_parent_control(&mut input);
                assert!(matches!(control, Control::Cancel { .. }));
                started.send(()).unwrap();
                if release.recv().is_ok() {
                    respond(&mut output, session_id, request_id, Control::Ok);
                }
            }
            TestMode::HoldStale {
                started,
                release,
                sent,
            } => {
                let (session_id, request_id, control) = read_parent_control(&mut input);
                assert!(matches!(control, Control::Health));
                started.send(()).unwrap();
                release.recv().unwrap();
                respond(&mut output, session_id, request_id, Control::Ok);
                sent.send(()).unwrap();
            }
            TestMode::FailRequest {
                received,
                malformed,
            } => {
                let (_, _, control) = read_parent_control(&mut input);
                assert!(matches!(control, Control::Health));
                received.send(()).unwrap();
                if malformed {
                    output.write_all(b"BAD!").unwrap();
                    output.flush().unwrap();
                }
            }
            TestMode::InvalidResponse { pcm_kind } => {
                let (session_id, request_id, control) = read_parent_control(&mut input);
                assert!(matches!(control, Control::Health));
                let frame = if pcm_kind {
                    Frame {
                        kind: FrameKind::Pcm,
                        session_id,
                        request_id,
                        body: 0.0_f32.to_le_bytes().to_vec(),
                    }
                } else {
                    control_frame(session_id, request_id, &Control::Health).unwrap()
                };
                write_frame(&mut output, &frame).unwrap();
            }
            TestMode::RuntimeFailureOnLoad(error) => {
                let (session_id, request_id, control) = read_parent_control(&mut input);
                assert!(matches!(control, Control::LoadRuntime { .. }));
                respond(
                    &mut output,
                    session_id,
                    request_id,
                    Control::RuntimeFailed {
                        error: WireRuntimeError::from_runtime(&error),
                    },
                );
            }
            TestMode::RuntimeLoadThenExit => {
                let (session_id, request_id, control) = read_parent_control(&mut input);
                let Control::LoadRuntime { preference, .. } = control else {
                    panic!("expected runtime load request");
                };
                running.store(false, Ordering::Release);
                respond(
                    &mut output,
                    session_id,
                    request_id,
                    Control::RuntimeLoaded {
                        execution: WireRuntimeLoadExecution {
                            diagnostics: WireRuntimeDiagnostics {
                                resolved_acceleration: ResolvedAcceleration {
                                    requested: preference,
                                    resolved: ComputeDevice::Cpu,
                                    diagnostic: None,
                                    selection: None,
                                },
                                runtime_location: PathBuf::from("test-worker-runtime"),
                                warm_reused: false,
                                model_load_duration_ms: 1,
                            },
                            detected_architecture: "test-runtime".to_owned(),
                            capabilities: RuntimeCapabilities::default(),
                        },
                    },
                );
            }
        }
    }

    fn run_vad_test_worker(
        input: &mut impl Read,
        output: &mut impl Write,
        crash_on_window: bool,
        crash_on_reset: bool,
        crash_on_end: bool,
    ) {
        let mut window_index = 0_usize;
        loop {
            let Ok(frame) = read_frame(input) else {
                return;
            };
            let (session_id, request_id, control) = parse_parent_control(frame).unwrap();
            match control {
                Control::LoadVad { .. } | Control::Health | Control::Unload => {
                    respond(output, session_id, request_id, Control::Ok);
                }
                Control::StartVad { .. } => {
                    window_index = 0;
                    respond(output, session_id, request_id, Control::Ok);
                }
                Control::ResetVad => {
                    if crash_on_reset {
                        return;
                    }
                    window_index = 0;
                    respond(output, session_id, request_id, Control::Ok);
                }
                Control::EndVad => {
                    if crash_on_end {
                        return;
                    }
                    respond(output, session_id, request_id, Control::Ok);
                }
                Control::Cancel { .. } => {
                    respond(output, session_id, request_id, Control::Ok);
                }
                Control::VadWindow => {
                    let pcm = read_frame(input).unwrap();
                    assert_eq!(pcm.kind, FrameKind::Pcm);
                    assert_eq!((pcm.session_id, pcm.request_id), (session_id, request_id));
                    assert_eq!(decode_pcm(&pcm.body).unwrap().len(), WINDOW_SAMPLES);
                    if crash_on_window {
                        return;
                    }
                    let probability = [0.4, 0.7][window_index.min(1)];
                    window_index += 1;
                    respond(
                        output,
                        session_id,
                        request_id,
                        Control::VadDecision {
                            probability,
                            speech: probability > 0.5,
                        },
                    );
                }
                Control::Shutdown => {
                    respond(output, session_id, request_id, Control::Ok);
                    return;
                }
                other => panic!("unexpected VAD test-worker command: {other:?}"),
            }
        }
    }

    fn run_blocked_vad_worker(
        input: &mut impl Read,
        output: &mut impl Write,
        operation: BlockedVadOperation,
        started: TestSender<()>,
    ) {
        loop {
            let Ok(frame) = read_frame(input) else {
                return;
            };
            let (session_id, request_id, control) = parse_parent_control(frame).unwrap();
            let is_blocked = matches!(
                (operation, &control),
                (BlockedVadOperation::Load, Control::LoadVad { .. })
                    | (BlockedVadOperation::Start, Control::StartVad { .. })
                    | (BlockedVadOperation::Health, Control::Health)
                    | (BlockedVadOperation::Compute, Control::VadWindow)
                    | (BlockedVadOperation::Reset, Control::ResetVad)
                    | (BlockedVadOperation::End, Control::EndVad)
                    | (BlockedVadOperation::Cancel, Control::Cancel { .. })
            );
            if matches!(&control, Control::VadWindow) {
                let pcm = read_frame(input).unwrap();
                assert_eq!(pcm.kind, FrameKind::Pcm);
                assert_eq!((pcm.session_id, pcm.request_id), (session_id, request_id));
                assert_eq!(decode_pcm(&pcm.body).unwrap().len(), WINDOW_SAMPLES);
            }
            if is_blocked {
                started.send(()).unwrap();
                // The fake process termination path writes EOF into this pipe,
                // so the worker and its owned reaper both terminate promptly.
                let _ = read_frame(input);
                return;
            }
            match control {
                Control::LoadVad { .. }
                | Control::StartVad { .. }
                | Control::Health
                | Control::ResetVad
                | Control::EndVad
                | Control::Cancel { .. } => {
                    respond(output, session_id, request_id, Control::Ok);
                }
                Control::VadWindow => respond(
                    output,
                    session_id,
                    request_id,
                    Control::VadDecision {
                        probability: 0.4,
                        speech: false,
                    },
                ),
                other => panic!("unexpected blocked-VAD command: {other:?}"),
            }
        }
    }

    fn run_cumulative_vad_acquisition_worker(
        input: &mut impl Read,
        output: &mut impl Write,
        stage_delay: Duration,
        blocked: TestSender<()>,
    ) {
        for expected in ["load", "health", "start"] {
            let (session_id, request_id, control) = read_parent_control(input);
            let matches_expected = match expected {
                "load" => matches!(control, Control::LoadVad { .. }),
                "health" => matches!(control, Control::Health),
                "start" => matches!(control, Control::StartVad { .. }),
                _ => unreachable!(),
            };
            assert!(
                matches_expected,
                "unexpected {expected} control: {control:?}"
            );
            std::thread::sleep(stage_delay);
            respond(output, session_id, request_id, Control::Ok);
        }
        let (_, _, control) = read_parent_control(input);
        assert!(matches!(control, Control::Health));
        blocked.send(()).unwrap();
        let _ = read_frame(input);
    }

    fn run_malformed_vad_window_worker(input: &mut impl Read, output: &mut impl Write) {
        loop {
            let frame = read_frame(input).unwrap();
            let (session_id, request_id, control) = parse_parent_control(frame).unwrap();
            match control {
                Control::LoadVad { .. } | Control::StartVad { .. } | Control::Health => {
                    respond(output, session_id, request_id, Control::Ok);
                }
                Control::VadWindow => {
                    let pcm = read_frame(input).unwrap();
                    assert_eq!((pcm.session_id, pcm.request_id), (session_id, request_id));
                    output.write_all(b"BAD!").unwrap();
                    output.flush().unwrap();
                    return;
                }
                other => panic!("unexpected malformed-VAD command: {other:?}"),
            }
        }
    }

    fn test_supervisor(launcher: Arc<TestLauncher>) -> ProcessWorkerSupervisor {
        ProcessWorkerSupervisor::with_launcher(launcher).unwrap()
    }

    fn short_deadlines() -> SupervisorDeadlines {
        SupervisorDeadlines {
            hello: Duration::from_secs(1),
            load: Duration::from_millis(40),
            health: Duration::from_millis(40),
            data: Duration::from_millis(40),
            control: Duration::from_millis(40),
            cancel: Duration::from_millis(40),
        }
    }

    fn short_vad_deadlines() -> VadDeadlines {
        VadDeadlines {
            acquisition: Duration::from_millis(40),
            operation: Duration::from_millis(40),
        }
    }

    fn acquisition_test_deadlines(budget: Duration) -> VadDeadlines {
        VadDeadlines {
            acquisition: budget,
            operation: Duration::from_millis(40),
        }
    }

    fn test_root(label: &str) -> PathBuf {
        let suffix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "scribe-onnx-{label}-{}-{suffix}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    fn spec_with_roles(
        root: &Path,
        family: OnnxModelFamily,
        roles: &[OnnxFileRole],
    ) -> OnnxModelSpec {
        let files = roles
            .iter()
            .copied()
            .map(|role| {
                let relative = PathBuf::from(format!("{role:?}.onnx").to_ascii_lowercase());
                std::fs::write(root.join(&relative), format!("fixture-{role:?}")).unwrap();
                (role, relative)
            })
            .collect();
        OnnxModelSpec {
            id: format!("test-{family:?}"),
            root: root.to_path_buf(),
            family,
            files,
            num_threads: 1,
        }
    }

    fn raw_header(version: u8, kind: u8, body_len: u32) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&PROTOCOL_MAGIC);
        bytes.extend_from_slice(&[version, kind]);
        bytes.extend_from_slice(&body_len.to_le_bytes());
        bytes.extend_from_slice(&7_u64.to_le_bytes());
        bytes.extend_from_slice(&11_u64.to_le_bytes());
        bytes
    }

    fn append_control(input: &mut Vec<u8>, session_id: u64, request_id: u64, control: Control) {
        write_frame(
            input,
            &control_frame(session_id, request_id, &control).unwrap(),
        )
        .unwrap();
    }

    fn append_pcm(input: &mut Vec<u8>, session_id: u64, request_id: u64, samples: &[f32]) {
        write_frame(
            input,
            &Frame {
                kind: FrameKind::Pcm,
                session_id,
                request_id,
                body: samples
                    .iter()
                    .flat_map(|sample| sample.to_le_bytes())
                    .collect(),
            },
        )
        .unwrap();
    }

    #[test]
    fn protocol_v5_has_distinct_inference_and_vad_roles() {
        assert_eq!(PROTOCOL_MAGIC, *b"SCIF");
        assert_eq!(PROTOCOL_VERSION, 5);
        assert_eq!(INFERENCE_WORKER_FLAG, "--scribe-inference-worker");
        assert_eq!(VAD_WORKER_FLAG, "--scribe-vad-worker");
        assert!(!control_allowed_for_role(
            &Control::LoadVad { num_threads: 1 },
            WorkerRole::Inference,
        ));
        assert!(!control_allowed_for_role(
            &Control::BeginBatch {
                artifact: WireRuntimeArtifact::Gguf(WireRuntimeModel {
                    id: "fixture".to_owned(),
                    path: PathBuf::from("fixture.gguf"),
                    format: WireArtifactFormat::Gguf,
                    expected_size_bytes: 1,
                    expected_sha256: "0".repeat(64),
                }),
                preference: AccelerationPreference::Cpu,
                options: TranscriptionOptions::default(),
                source_sample_rate: PREPARED_SAMPLE_RATE,
                source_channels: 1,
                source_frames: 1,
                declared_samples: 1,
            },
            WorkerRole::Vad,
        ));
        assert!(control_allowed_for_role(
            &Control::Health,
            WorkerRole::Inference,
        ));
        assert!(control_allowed_for_role(&Control::Health, WorkerRole::Vad,));
    }

    #[test]
    fn worker_entry_flags_require_an_exact_single_role_argument() {
        use std::ffi::OsString;

        assert_eq!(
            worker_role_from_args(&[OsString::from(INFERENCE_WORKER_FLAG)]).unwrap(),
            Some(WorkerRole::Inference)
        );
        assert_eq!(
            worker_role_from_args(&[OsString::from("--onnx-worker")])
                .unwrap_err()
                .to_string(),
            "unknown private Scribe worker role"
        );
        assert!(worker_role_from_args(&[OsString::from("--scribe-unknown-worker")]).is_err());
        assert_eq!(
            worker_role_from_args(&[OsString::from(VAD_WORKER_FLAG)]).unwrap(),
            Some(WorkerRole::Vad)
        );
        assert!(
            worker_role_from_args(&[
                OsString::from(INFERENCE_WORKER_FLAG),
                OsString::from("--extra")
            ])
            .is_err()
        );
        assert!(
            worker_role_from_args(&[
                OsString::from(INFERENCE_WORKER_FLAG),
                OsString::from(VAD_WORKER_FLAG)
            ])
            .is_err()
        );
        assert_eq!(
            worker_role_from_args(&[OsString::from("--ordinary-app-argument")]).unwrap(),
            None
        );
    }

    #[test]
    fn capability_handshake_binds_generation_build_abi_role_provider_and_targets() {
        for mismatch in [
            CapabilityMismatch::Challenge,
            CapabilityMismatch::AppBuild,
            CapabilityMismatch::WorkerBuild,
            CapabilityMismatch::Abi,
            CapabilityMismatch::Role,
            CapabilityMismatch::ProviderArtifacts,
        ] {
            let launcher = Arc::new(TestLauncher::new([TestMode::CapabilityMismatch(mismatch)]));
            let supervisor = ProcessWorkerSupervisor::unstarted_with_launcher_and_deadlines(
                launcher,
                short_deadlines(),
            );
            let error = supervisor.ensure_generation().unwrap_err().to_string();
            assert!(
                error.contains("capability"),
                "unexpected {mismatch:?} handshake error: {error}"
            );
            assert_eq!(supervisor.current_generation().unwrap(), None);
        }
    }

    #[test]
    fn capability_compatible_wrong_worker_digest_is_rejected() {
        let challenge = "ef".repeat(32);
        let mut expected = expected_worker(WorkerRole::Inference);
        expected.bundled_worker_sha256 = "11".repeat(32);
        let mut capability = worker_capability(WorkerRole::Inference, challenge.clone()).unwrap();
        capability.bundled_worker_sha256 = "22".repeat(32);
        let error = validate_worker_capability(&capability, &challenge, &expected).unwrap_err();
        assert!(error.to_string().contains("incompatible"));
    }

    #[test]
    fn directional_digest_handshake_binds_parent_anchor_without_child_compile_time_comparison() {
        let challenge = "ac".repeat(32);
        let mut expected = expected_worker(WorkerRole::Inference);
        expected.bundled_worker_sha256 = "31".repeat(32);

        assert_eq!(
            validate_worker_hello(Some(WorkerRole::Inference), &challenge, &expected).unwrap(),
            WorkerRole::Inference
        );

        let mut capability = worker_capability(WorkerRole::Inference, challenge.clone()).unwrap();
        capability.bundled_worker_sha256 = expected.bundled_worker_sha256.clone();
        validate_worker_capability(&capability, &challenge, &expected).unwrap();
    }

    #[test]
    fn compiled_inference_route_matches_the_worker_provider() {
        let expected = if cfg!(feature = "cuda-acceleration") {
            WorkerProvider::Cuda
        } else if cfg!(feature = "vulkan-acceleration") {
            WorkerProvider::Vulkan
        } else {
            WorkerProvider::Cpu
        };
        assert_eq!(compiled_worker_provider(WorkerRole::Inference), expected);
        assert_eq!(
            compiled_worker_provider(WorkerRole::Vad),
            WorkerProvider::Cpu
        );
        assert_eq!(
            InferenceWorkerRegistry::for_current_build().routes[0].provider,
            WorkerProvider::Cpu
        );
    }

    #[cfg(windows)]
    #[test]
    fn fixture_pack_launch_reverifies_with_fixture_only_trust() {
        let root =
            crate::gpu_worker_pack::manifest::test_support::temp_root("pack-fixture-launch-trust");
        let (_, lease) = crate::gpu_worker_pack::manifest::test_support::leased_fixture(&root);
        let executable = resolve_verified_pack_executable(Arc::new(lease), None)
            .expect("fixture launch must reverify with its test-only trust authority");
        assert_eq!(
            executable
                .pack_launch
                .as_ref()
                .expect("fixture launch keeps verified pack authority")
                .expectation
                .backend,
            WorkerProvider::Vulkan
        );
        drop(executable);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn verified_pack_hello_remaps_index_by_exact_stable_identity() {
        let root = crate::gpu_worker_pack::manifest::test_support::temp_root("pack-hello-remap");
        let (_, lease) = crate::gpu_worker_pack::manifest::test_support::leased_fixture(&root);
        let lease = Arc::new(lease);
        let expectation = pack_expectation(&lease);
        let mut expected = backend_target(BackendKind::Vulkan, "native:pci:0000:01:00.0", Some(9));
        expected.vendor = GpuVendor::Nvidia;
        expected.driver_version = Some("fixture-driver-1".to_owned());
        let context = PackLaunchContext::fixture(lease, Some(expected.clone()));
        let capability = WorkerPackCapability {
            expectation,
            devices: vec![WorkerPackDeviceCapability {
                stable_device_identity: "native:pci:0000:01:00.0".to_owned(),
                process_index: 2,
                display_name: "Fixture NVIDIA GPU".to_owned(),
                driver_version: Some("fixture-driver-1".to_owned()),
                device_class: DeviceClass::DiscreteGpu,
                vendor: GpuVendor::Nvidia,
                memory_total_bytes: 8 * 1024 * 1024 * 1024,
                memory_available_bytes: 6 * 1024 * 1024 * 1024,
            }],
        };
        assert_eq!(capability.expectation.backend, WorkerProvider::Vulkan);
        assert_eq!(
            capability.devices[0].stable_device_identity,
            expected.device_id.as_str()
        );
        assert_eq!(capability.devices[0].vendor, expected.vendor);
        assert_eq!(capability.devices[0].device_class, expected.device_class);
        assert_eq!(
            capability.devices[0].driver_version,
            expected.driver_version
        );
        assert_eq!(
            capability.devices[0].memory_total_bytes,
            expected.memory_total_bytes
        );
        let bindings =
            bind_pack_hello(&context, &capability).expect("stable identity remaps index");
        assert_eq!(bindings[0].backend_target().process_index, Some(2));

        let mut changed_driver = capability.clone();
        changed_driver.devices[0].driver_version = Some("fixture-driver-2".to_owned());
        assert!(bind_pack_hello(&context, &changed_driver).is_err());
        let mut wrong_device = capability;
        wrong_device.devices[0].stable_device_identity = "native:pci:0000:02:00.0".to_owned();
        assert!(bind_pack_hello(&context, &wrong_device).is_err());
        drop(bindings);
        drop(context);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn verified_pack_launch_never_exports_a_process_index() {
        let root = crate::gpu_worker_pack::manifest::test_support::temp_root("pack-index-env");
        let (_, lease) = crate::gpu_worker_pack::manifest::test_support::leased_fixture(&root);
        let lease = Arc::new(lease);
        let mut expected =
            backend_target(BackendKind::Vulkan, "native:luid:0000000000000001", Some(7));
        expected.driver_version = Some("vulkan:10de:1:1:fixture".to_owned());
        let context = PackLaunchContext::fixture(lease, Some(expected));
        let mut command = Command::new("scribe-inference-worker.exe");

        configure_worker_pack_environment(&mut command, &context);

        let environment = command
            .get_envs()
            .filter_map(|(key, value)| {
                value.map(|value| {
                    (
                        key.to_string_lossy().into_owned(),
                        value.to_string_lossy().into_owned(),
                    )
                })
            })
            .collect::<BTreeMap<_, _>>();
        assert_eq!(
            environment.get(PACK_DEVICE_ID_ENV).map(String::as_str),
            Some("native:luid:0000000000000001")
        );
        assert!(!environment.contains_key("SCRIBE_PRIVATE_PACK_DEVICE_PROCESS_INDEX"));
        drop(context);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn launched_worker_uses_its_own_hello_index_not_a_prior_probe_index() {
        clear_current_pack_runtime_devices();
        let probe_process_index = 9;
        let capability = WorkerPackCapability {
            expectation: WorkerPackExpectation {
                pack_id: "scribe-vulkan-windows-x64".to_owned(),
                pack_version: "fixture".to_owned(),
                pack_digest: "a".repeat(64),
                security_epoch: 1,
                runtime_abi: WORKER_ABI_VERSION,
                backend: WorkerProvider::Vulkan,
                provider: "transcribe-cpp-ggml-vulkan".to_owned(),
            },
            // The launch worker re-enumerated this device at index two. The
            // earlier probe used index nine for the same stable LUID.
            devices: vec![WorkerPackDeviceCapability {
                stable_device_identity: "native:luid:0000000000000001".to_owned(),
                process_index: 2,
                display_name: "Fixture NVIDIA GPU".to_owned(),
                driver_version: Some("vulkan:10de:1:1:fixture".to_owned()),
                device_class: DeviceClass::DiscreteGpu,
                vendor: GpuVendor::Nvidia,
                memory_total_bytes: 8 * 1024 * 1024 * 1024,
                memory_available_bytes: 6 * 1024 * 1024 * 1024,
            }],
        };

        install_current_pack_runtime_devices(&capability);

        assert_eq!(
            current_pack_runtime_device(
                "native:luid:0000000000000001",
                "transcribe-cpp-ggml-vulkan",
            )
            .map(|device| device.process_index),
            Some(2)
        );
        assert_ne!(
            current_pack_runtime_device(
                "native:luid:0000000000000001",
                "transcribe-cpp-ggml-vulkan",
            )
            .map(|device| device.process_index),
            Some(probe_process_index)
        );
        assert_eq!(
            current_pack_runtime_device("native:luid:0000000000000001", "transcribe-cpp-ggml-cuda",),
            None
        );
        clear_current_pack_runtime_devices();
    }

    #[test]
    fn full_provider_enumeration_rejects_duplicate_indexes_before_device_filtering() {
        let device = |identity: &str| WorkerPackDeviceCapability {
            stable_device_identity: identity.to_owned(),
            process_index: 2,
            display_name: format!("Fixture {identity}"),
            driver_version: Some("vulkan:10de:1:1:fixture".to_owned()),
            device_class: DeviceClass::DiscreteGpu,
            vendor: GpuVendor::Nvidia,
            memory_total_bytes: 8 * 1024 * 1024 * 1024,
            memory_available_bytes: 6 * 1024 * 1024 * 1024,
        };
        let all_provider_devices = vec![
            device("native:luid:0000000000000001"),
            device("native:luid:0000000000000002"),
        ];
        let selected_only = all_provider_devices
            .iter()
            .filter(|device| device.stable_device_identity == "native:luid:0000000000000001")
            .collect::<Vec<_>>();

        assert_eq!(selected_only.len(), 1);
        assert!(validate_unique_provider_process_indexes(&all_provider_devices).is_err());
    }

    #[test]
    fn verified_pack_probe_binds_every_sorted_device_and_rejects_bad_lists() {
        let root = crate::gpu_worker_pack::manifest::test_support::temp_root("pack-device-list");
        let (_, lease) = crate::gpu_worker_pack::manifest::test_support::leased_fixture(&root);
        let lease = Arc::new(lease);
        let expectation = pack_expectation(&lease);
        let context = PackLaunchContext::fixture(lease, None);
        let device = |identity: &str, index: usize| WorkerPackDeviceCapability {
            stable_device_identity: identity.to_owned(),
            process_index: index,
            display_name: format!("Fixture GPU {index}"),
            driver_version: Some("windows-display:32.0.15.8088".to_owned()),
            device_class: DeviceClass::DiscreteGpu,
            vendor: GpuVendor::Nvidia,
            memory_total_bytes: 8 * 1024 * 1024 * 1024,
            memory_available_bytes: 6 * 1024 * 1024 * 1024,
        };
        let first = device("native:pci:0000:01:00.0", 7);
        let second = device("native:pci:0000:02:00.0", 2);
        let capability = WorkerPackCapability {
            expectation: expectation.clone(),
            devices: vec![first.clone(), second.clone()],
        };
        let bindings = bind_pack_hello(&context, &capability).unwrap();
        assert_eq!(bindings.len(), 2);
        assert_eq!(bindings[0].backend_target().process_index, Some(7));
        assert_eq!(bindings[1].backend_target().process_index, Some(2));

        let reordered = WorkerPackCapability {
            expectation: expectation.clone(),
            devices: vec![second.clone(), first.clone()],
        };
        assert!(bind_pack_hello(&context, &reordered).is_err());
        let duplicate = WorkerPackCapability {
            expectation: expectation.clone(),
            devices: vec![first.clone(), first.clone()],
        };
        assert!(bind_pack_hello(&context, &duplicate).is_err());
        let mut duplicate_index_second = second.clone();
        duplicate_index_second.process_index = first.process_index;
        let duplicate_index = WorkerPackCapability {
            expectation: expectation.clone(),
            devices: vec![first.clone(), duplicate_index_second],
        };
        assert!(bind_pack_hello(&context, &duplicate_index).is_err());
        let oversized = WorkerPackCapability {
            expectation: expectation.clone(),
            devices: (0..=MAX_PACK_DEVICES)
                .map(|index| device(&format!("native:pci:0000:{index:02x}:00.0"), index))
                .collect(),
        };
        assert!(bind_pack_hello(&context, &oversized).is_err());
        let malformed = WorkerPackCapability {
            expectation,
            devices: vec![device("NATIVE:PCI:0000:01:00.0", 0)],
        };
        assert!(bind_pack_hello(&context, &malformed).is_err());
        drop(bindings);
        drop(context);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn native_pci_identity_parser_is_canonical_and_bounded() {
        assert_eq!(
            parse_native_pci_location("native:pci:0000:01:00.0"),
            Some((1, 0, 0))
        );
        assert_eq!(
            parse_native_pci_location("native:0000:ff:1f.7"),
            Some((255, 31, 7))
        );
        assert_eq!(
            parse_native_pci_location("native:pci:00000000:01:00.0"),
            Some((1, 0, 0))
        );
        for invalid in [
            "pci:0000:01:00.0",
            "native:pci:0001:01:00.0",
            "native:pci:0000:100:00.0",
            "native:pci:0000:01:20.0",
            "native:pci:0000:01:00.8",
            "native:pci:0000:01:00",
            "native:pci:0000:01:00.0:extra",
            "native:pci:0000:0A:00.0",
        ] {
            assert_eq!(parse_native_pci_location(invalid), None, "{invalid}");
        }
    }

    #[cfg(windows)]
    #[test]
    fn vulkan_identity_catalog_handles_missing_and_opaque_provider_ids() {
        let catalog_device = |identity: &str| VulkanDeviceIdentity {
            normalized_display_name: "fixture gpu".to_owned(),
            device_class: DeviceClass::DiscreteGpu,
            vendor: GpuVendor::Nvidia,
            stable_device_identity: identity.to_owned(),
            driver_identity: "vulkan:10de:1:1:fixture".to_owned(),
            claimed: false,
        };
        let drivers = PackDriverCatalog {
            by_pci_location: BTreeMap::new(),
        };

        let mut missing_id_catalog = VulkanDeviceCatalog {
            devices: vec![catalog_device("native:luid:0000000000000001")],
        };
        assert_eq!(
            resolve_vulkan_provider_identity(
                None,
                "Fixture GPU",
                DeviceClass::DiscreteGpu,
                GpuVendor::Nvidia,
                &drivers,
                &mut missing_id_catalog,
            )
            .unwrap(),
            (
                "native:luid:0000000000000001".to_owned(),
                "vulkan:10de:1:1:fixture".to_owned(),
            )
        );

        let mut opaque_id_catalog = VulkanDeviceCatalog {
            devices: vec![catalog_device(
                "native:uuid:00112233445566778899aabbccddeeff",
            )],
        };
        assert_eq!(
            resolve_vulkan_provider_identity(
                Some("opaque-provider-token"),
                "Fixture GPU",
                DeviceClass::DiscreteGpu,
                GpuVendor::Nvidia,
                &drivers,
                &mut opaque_id_catalog,
            )
            .unwrap(),
            (
                "native:uuid:00112233445566778899aabbccddeeff".to_owned(),
                "vulkan:10de:1:1:fixture".to_owned(),
            )
        );

        let mut ambiguous_catalog = VulkanDeviceCatalog {
            devices: vec![
                catalog_device("native:luid:0000000000000001"),
                catalog_device("native:luid:0000000000000002"),
            ],
        };
        assert!(
            resolve_vulkan_provider_identity(
                Some("opaque-provider-token"),
                "Fixture GPU",
                DeviceClass::DiscreteGpu,
                GpuVendor::Nvidia,
                &drivers,
                &mut ambiguous_catalog,
            )
            .is_err()
        );
    }

    #[cfg(windows)]
    #[test]
    fn vulkan_identity_catalog_rejects_ambiguity_and_tracks_driver_changes() {
        let device = |identity: &str, driver: &str| VulkanDeviceIdentity {
            normalized_display_name: "fixture duplicate gpu".to_owned(),
            device_class: DeviceClass::DiscreteGpu,
            vendor: GpuVendor::Nvidia,
            stable_device_identity: identity.to_owned(),
            driver_identity: driver.to_owned(),
            claimed: false,
        };
        let mut catalog = VulkanDeviceCatalog {
            devices: vec![
                device("native:luid:0000000000000001", "vulkan:10de:1:1:a"),
                device("native:luid:0000000000000002", "vulkan:10de:1:1:b"),
            ],
        };
        let ambiguity = catalog
            .claim(
                " Fixture   Duplicate GPU ",
                DeviceClass::DiscreteGpu,
                GpuVendor::Nvidia,
            )
            .unwrap_err();
        assert!(ambiguity.to_string().contains("ambiguous"));
        assert!(catalog.devices.iter().all(|device| !device.claimed));

        let mut changed = VulkanDeviceCatalog {
            devices: vec![device(
                "native:luid:0000000000000001",
                "vulkan:10de:1:2:changed",
            )],
        };
        assert_eq!(
            changed
                .claim(
                    "fixture duplicate gpu",
                    DeviceClass::DiscreteGpu,
                    GpuVendor::Nvidia,
                )
                .unwrap()
                .1,
            "vulkan:10de:1:2:changed"
        );
        let mut mismatch = VulkanDeviceCatalog {
            devices: vec![device("native:luid:0000000000000001", "vulkan:10de:1:1:a")],
        };
        assert!(
            mismatch
                .claim(
                    "fixture duplicate gpu",
                    DeviceClass::IntegratedGpu,
                    GpuVendor::Nvidia,
                )
                .is_err()
        );
    }

    #[cfg(all(
        windows,
        feature = "inference-worker",
        any(feature = "cuda-acceleration", feature = "vulkan-acceleration")
    ))]
    #[test]
    #[ignore = "requires a qualified Windows GPU and the pinned provider toolchain"]
    fn windows_provider_devices_have_authoritative_driver_identity() {
        transcribe_cpp::init_backends_default().unwrap();
        let expected_kind = if cfg!(feature = "cuda-acceleration") {
            "cuda"
        } else {
            "vulkan"
        };
        let catalog = PackDriverCatalog::discover().unwrap();
        let devices = transcribe_cpp::devices()
            .into_iter()
            .filter(|device| device.kind.eq_ignore_ascii_case(expected_kind))
            .collect::<Vec<_>>();
        assert!(!devices.is_empty());
        #[cfg(feature = "vulkan-acceleration")]
        let mut vulkan_catalog = VulkanDeviceCatalog::discover().unwrap();
        for device in devices {
            let (stable, driver) = if let Some(native_id) = device.device_id {
                let stable = format!("native:{}", native_id.to_ascii_lowercase());
                let driver = catalog.identity_for(&stable).unwrap();
                (stable, driver)
            } else {
                #[cfg(feature = "vulkan-acceleration")]
                {
                    let class = match device.device_type {
                        transcribe_cpp::DeviceType::Gpu => DeviceClass::DiscreteGpu,
                        transcribe_cpp::DeviceType::Igpu => DeviceClass::IntegratedGpu,
                        _ => panic!("provider returned a non-GPU Vulkan device"),
                    };
                    let normalized =
                        format!("{} {}", device.name, device.description).to_ascii_lowercase();
                    let vendor = if normalized.contains("nvidia") {
                        GpuVendor::Nvidia
                    } else if normalized.contains("amd") || normalized.contains("radeon") {
                        GpuVendor::Amd
                    } else if normalized.contains("intel") {
                        GpuVendor::Intel
                    } else {
                        GpuVendor::Other
                    };
                    vulkan_catalog
                        .claim(&device.description, class, vendor)
                        .unwrap()
                }
                #[cfg(not(feature = "vulkan-acceleration"))]
                panic!("CUDA provider omitted its stable identity");
            };
            assert!(
                stable.starts_with("native:pci:")
                    || stable.starts_with("native:luid:")
                    || stable.starts_with("native:uuid:")
            );
            assert!(driver.starts_with("windows-display:") || driver.starts_with("vulkan:"));
            assert!(driver.len() <= 128);
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_dll_search_hardening_accepts_an_explicit_empty_directory() {
        harden_windows_dll_search().unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn failed_job_binding_terminates_and_reaps_the_child() {
        let mut child = Command::new("cmd.exe")
            .args(["/d", "/q", "/c", "ping -n 30 127.0.0.1 > NUL"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let error = match bind_worker_process_tree_or_terminate(
            &mut child,
            |_| -> Result<ProcessTreeGuard> { bail!("injected job bind failure") },
        ) {
            Ok(_) => panic!("injected job bind failure unexpectedly succeeded"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("terminated and reaped"));
        assert!(child.try_wait().unwrap().is_some());
    }

    #[test]
    fn worker_protocol_requires_one_hello_before_commands() {
        let factory = FakeRecognizerFactory::new();
        let vad_factory = FakeVadFactory::new();

        let health = control_frame(1, 1, &Control::Health).unwrap();
        let mut health_bytes = Vec::new();
        write_frame(&mut health_bytes, &health).unwrap();
        let error = worker_loop_with_factories(
            &mut Cursor::new(health_bytes),
            &mut Vec::new(),
            &factory,
            &vad_factory,
            Some(WorkerRole::Inference),
            None,
        )
        .unwrap_err();
        assert!(error.to_string().contains("requires Hello"));

        let challenge = "ab".repeat(32);
        let hello = control_frame(
            2,
            1,
            &Control::Hello {
                challenge,
                expected: expected_worker(WorkerRole::Inference),
            },
        )
        .unwrap();
        let mut duplicate_bytes = Vec::new();
        write_frame(&mut duplicate_bytes, &hello).unwrap();
        write_frame(&mut duplicate_bytes, &hello).unwrap();
        let mut output = Vec::new();
        let error = worker_loop_with_factories(
            &mut Cursor::new(duplicate_bytes),
            &mut output,
            &factory,
            &vad_factory,
            Some(WorkerRole::Inference),
            None,
        )
        .unwrap_err();
        assert!(error.to_string().contains("exactly once"));
        assert!(matches!(
            parse_worker_control(read_frame(&mut Cursor::new(output)).unwrap())
                .unwrap()
                .2,
            Control::Ready { .. }
        ));
    }

    #[test]
    fn adjacent_inference_worker_resolution_is_exact_and_canonical() {
        let root = test_root("worker-resolution");
        let desktop = root.join(format!("local-transcriber{}", std::env::consts::EXE_SUFFIX));
        let worker = root.join(format!(
            "scribe-inference-worker{}",
            std::env::consts::EXE_SUFFIX
        ));
        std::fs::write(&desktop, b"desktop").unwrap();
        assert!(
            resolve_adjacent_inference_worker(&desktop)
                .unwrap_err()
                .to_string()
                .contains("canonicalize")
        );
        std::fs::write(&worker, b"worker").unwrap();
        assert_eq!(
            resolve_adjacent_inference_worker(&desktop).unwrap(),
            std::fs::canonicalize(&worker).unwrap()
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn worker_environment_is_allowlisted_and_backend_overrides_are_stripped() {
        let mut command = Command::new("worker-fixture");
        for (name, value) in [
            ("GGML_VK_VISIBLE_DEVICES", "1"),
            ("VULKAN_SDK", "untrusted"),
            ("VK_LAYER_PATH", "untrusted"),
            ("CUDA_VISIBLE_DEVICES", "1"),
            ("LD_LIBRARY_PATH", "untrusted"),
            ("LD_PRELOAD", "untrusted"),
            ("DYLD_LIBRARY_PATH", "untrusted"),
            ("PATH", "untrusted"),
        ] {
            command.env(name, value);
        }
        configure_worker_environment(&mut command);
        let environment = command
            .get_envs()
            .map(|(name, value)| (name.to_string_lossy().to_string(), value))
            .collect::<Vec<_>>();
        for stripped in [
            "GGML_VK_VISIBLE_DEVICES",
            "VULKAN_SDK",
            "VK_LAYER_PATH",
            "CUDA_VISIBLE_DEVICES",
            "LD_LIBRARY_PATH",
            "LD_PRELOAD",
            "DYLD_LIBRARY_PATH",
            "PATH",
        ] {
            assert!(
                environment
                    .iter()
                    .all(|(name, value)| name != stripped || value.is_none()),
                "worker inherited forbidden environment variable {stripped}"
            );
        }
    }

    #[test]
    #[allow(
        clippy::assertions_on_constants,
        reason = "the constant assertion documents the platform-specific write-sharing branch without making non-Windows builds fail at compile time"
    )]
    fn worker_identity_rejects_hardlinks_and_detects_replacement() {
        let root = test_root("worker-file-identity");
        let worker = root.join(format!("worker{}", std::env::consts::EXE_SUFFIX));
        let alias = root.join(format!("worker-alias{}", std::env::consts::EXE_SUFFIX));
        std::fs::write(&worker, b"trusted-worker").unwrap();
        std::fs::hard_link(&worker, &alias).unwrap();
        let name = worker.file_name().unwrap();
        assert!(
            verify_worker_executable(&worker, &root, name, "")
                .unwrap_err()
                .to_string()
                .contains("hardlink")
        );
        std::fs::remove_file(&alias).unwrap();
        let verified = verify_worker_executable(&worker, &root, name, "").unwrap();
        match std::fs::write(&worker, b"replacement") {
            Ok(()) => assert!(verified.revalidate().is_err()),
            Err(_) => {
                // Windows holds a non-delete/non-write-sharing handle through
                // process creation, so replacement itself must fail.
                #[cfg(not(windows))]
                panic!("worker replacement was unexpectedly denied");
            }
        }
        drop(verified);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn worker_identity_rejects_case_ads_and_reparse_paths() {
        use std::os::windows::fs::{symlink_dir, symlink_file};

        let root = test_root("worker-windows-identity");
        let actual = root.join("Scribe-Inference-Worker.exe");
        std::fs::write(&actual, b"worker").unwrap();
        let packaged_name = std::ffi::OsStr::new("scribe-inference-worker.exe");
        assert!(
            verify_worker_executable(&root.join(packaged_name), &root, packaged_name, "")
                .unwrap_err()
                .to_string()
                .contains("case")
        );
        let ads = root.join("Scribe-Inference-Worker.exe:payload");
        assert!(
            verify_worker_executable(&ads, &root, ads.file_name().unwrap(), "")
                .unwrap_err()
                .to_string()
                .contains("alternate data stream")
        );

        let file_link = root.join("linked-worker.exe");
        if symlink_file(&actual, &file_link).is_ok() {
            assert!(
                verify_worker_executable(&file_link, &root, file_link.file_name().unwrap(), "")
                    .unwrap_err()
                    .to_string()
                    .contains("reparse")
            );
        }
        let linked_root = root.with_file_name(format!(
            "{}-link",
            root.file_name().unwrap().to_string_lossy()
        ));
        if symlink_dir(&root, &linked_root).is_ok() {
            let linked_worker = linked_root.join(actual.file_name().unwrap());
            assert!(
                verify_worker_executable(
                    &linked_worker,
                    &linked_root,
                    actual.file_name().unwrap(),
                    ""
                )
                .unwrap_err()
                .to_string()
                .contains("reparse")
            );
            std::fs::remove_dir(&linked_root).unwrap();
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn worker_responses_reject_oversized_or_unbounded_values() {
        assert!(
            validate_worker_response(&Control::Text {
                text: "x".repeat(MAX_TRANSCRIPT_TEXT_BYTES + 1),
                final_result: true,
            })
            .is_err()
        );
        assert!(
            validate_worker_response(&Control::Error {
                message: "x".repeat(MAX_WORKER_ERROR_BYTES + 1),
            })
            .is_err()
        );
        let mut transcript = WireTranscript {
            text: String::new(),
            segments: vec![
                WireTranscriptSegment {
                    text: String::new(),
                    start_ms: None,
                    end_ms: None,
                    confidence: None,
                };
                MAX_TRANSCRIPT_SEGMENTS + 1
            ],
            detected_language: None,
            duration_ms: None,
        };
        assert!(validate_wire_transcript(&transcript).is_err());
        transcript.segments.clear();
        transcript.detected_language = Some("x".repeat(MAX_LANGUAGE_BYTES + 1));
        assert!(validate_wire_transcript(&transcript).is_err());

        let mut output = Vec::new();
        write_worker_response(
            &mut output,
            7,
            9,
            Control::Text {
                text: "x".repeat(MAX_TRANSCRIPT_TEXT_BYTES + 1),
                final_result: true,
            },
        )
        .unwrap();
        assert!(matches!(
            parse_worker_control(read_frame(&mut Cursor::new(output)).unwrap())
                .unwrap()
                .2,
            Control::Error { message }
                if message == "worker response exceeded the private protocol limit"
        ));
    }

    fn backend_target(backend: BackendKind, id: &str, index: Option<usize>) -> BackendTarget {
        BackendTarget {
            backend,
            provider_id: ProviderIdentity::new(format!("test:{}", backend.label())),
            driver_version: Some("test-driver-1".to_owned()),
            device_id: DeviceIdentity::new(id),
            display_name: format!("Test {} device", backend.label()),
            vendor: if backend == BackendKind::Cuda {
                GpuVendor::Nvidia
            } else {
                GpuVendor::Unknown
            },
            device_class: if backend.is_gpu() {
                DeviceClass::DiscreteGpu
            } else {
                DeviceClass::Cpu
            },
            memory_total_bytes: 8 * 1024 * 1024 * 1024,
            memory_available_bytes: 6 * 1024 * 1024 * 1024,
            pack: None,
            process_index: index,
        }
    }

    fn verified_gpu_route(
        backend: BackendKind,
        identity: &str,
        driver: &str,
        pack_digest: char,
    ) -> InferenceWorkerRoute {
        let mut target = backend_target(backend, identity, Some(0));
        target.driver_version = Some(driver.to_owned());
        target.pack = Some(crate::backend_policy::BackendPackIdentity {
            pack_id: format!("scribe-{}-windows-x64", backend.label()),
            pack_version: "1.0.0".to_owned(),
            pack_digest: pack_digest.to_string().repeat(64),
            security_epoch: 1,
            runtime_abi: crate::gpu_worker_pack::manifest::RUNTIME_ABI_VERSION,
        });
        InferenceWorkerRoute {
            provider: match backend {
                BackendKind::Cuda => WorkerProvider::Cuda,
                BackendKind::Vulkan => WorkerProvider::Vulkan,
                BackendKind::Metal => WorkerProvider::Metal,
                BackendKind::Cpu => WorkerProvider::Cpu,
            },
            supervisor: InferenceWorkerSupervisor {
                transport: ProcessWorkerSupervisor::unstarted_with_launcher_and_deadlines(
                    Arc::new(TestLauncher::new([])),
                    short_deadlines(),
                ),
                next_correlation: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            },
            target: Some(target),
            health_key: None,
            consume_retry_bypass: false,
            auto_evidence_id: None,
        }
    }

    fn inference_supervisor_with_launcher(
        launcher: Arc<TestLauncher>,
    ) -> InferenceWorkerSupervisor {
        InferenceWorkerSupervisor {
            transport: ProcessWorkerSupervisor::unstarted_with_launcher_and_deadlines(
                launcher,
                short_deadlines(),
            ),
            next_correlation: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        }
    }

    fn verified_gpu_catalog(routes: Vec<InferenceWorkerRoute>) -> VerifiedGpuRouteCatalog {
        VerifiedGpuRouteCatalog {
            device_set_digest: verified_gpu_device_set_digest(&routes),
            routes: Arc::new(routes),
            diagnostic: None,
            discovery_fingerprint: "test-catalog".to_owned(),
            provider_probe_incomplete: false,
            skipped_targets: Vec::new(),
        }
    }

    fn fixture_auto_policy(
        target: &BackendTarget,
        model_digest: &str,
        evidence_id: &str,
    ) -> AutoQualificationPolicy {
        let pack = target.pack.as_ref().expect("fixture target pack identity");
        let backend = match target.backend {
            BackendKind::Cuda => "cuda",
            BackendKind::Vulkan => "vulkan",
            BackendKind::Metal => "metal",
            BackendKind::Cpu => "cpu",
        };
        let vendor = match target.vendor {
            GpuVendor::Nvidia => "nvidia",
            GpuVendor::Amd => "amd",
            GpuVendor::Intel => "intel",
            GpuVendor::Apple => "apple",
            GpuVendor::Other => "other",
            GpuVendor::Unknown => "unknown",
        };
        let device_class = match target.device_class {
            DeviceClass::DiscreteGpu => "discrete_gpu",
            DeviceClass::IntegratedGpu => "integrated_gpu",
            DeviceClass::UnifiedGpu => "unified_gpu",
            DeviceClass::Cpu => "cpu",
            DeviceClass::Accelerator => "accelerator",
            DeviceClass::Unknown => "unknown",
        };
        let string = |value: &str| serde_json::to_string(value).unwrap();
        let manifest = format!(
            "{{\"schema_version\":1,\"mode\":\"default_deny\",\"target_os\":\"windows\",\"target_arch\":\"x86_64\",\"entries\":[{{\"pack\":{{\"pack_id\":{},\"pack_version\":{},\"pack_digest\":{},\"security_epoch\":{},\"runtime_abi\":{}}},\"model_digest\":{},\"backend\":{},\"provider_id\":{},\"vendor\":{},\"device_class\":{},\"minimum_total_memory_bytes\":{},\"driver\":{{\"kind\":\"exact\",\"value\":{}}},\"evidence\":{{\"id\":{},\"cold_runs\":5,\"warm_runs\":20,\"gpu_p95_ms\":110,\"cpu_p95_ms\":100,\"correctness_verified\":true,\"reliability_verified\":true,\"cold_evidence_sha256\":\"cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc\",\"warm_evidence_sha256\":\"dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd\",\"transcript_parity_evidence_sha256\":\"eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee\"}}}}]}}",
            string(&pack.pack_id),
            string(&pack.pack_version),
            string(&pack.pack_digest),
            pack.security_epoch,
            pack.runtime_abi,
            string(model_digest),
            string(backend),
            string(target.provider_id.as_str()),
            string(vendor),
            string(device_class),
            target.memory_total_bytes,
            string(target.driver_version.as_deref().expect("fixture driver")),
            string(evidence_id),
        );
        AutoQualificationPolicy::from_fixture_json(&manifest).unwrap()
    }

    #[test]
    fn gpu_health_identity_binds_exact_pack_driver_device_model_and_device_set() {
        let first = verified_gpu_route(
            BackendKind::Cuda,
            "native:pci:0000:01:00.0",
            "windows-display:32.0.16.1088",
            'a',
        );
        let key = route_health_key(&first, &"b".repeat(64)).unwrap();
        assert_eq!(key.pack_digest, "a".repeat(64));
        assert_eq!(key.model_digest, "b".repeat(64));
        assert_eq!(key.driver_version, "windows-display:32.0.16.1088");
        assert_eq!(key.stable_device_identity, "native:pci:0000:01:00.0");

        let mut reordered = first.clone();
        reordered.target.as_mut().unwrap().process_index = Some(7);
        reordered.target.as_mut().unwrap().memory_available_bytes /= 2;
        assert_eq!(
            verified_gpu_device_set_digest(std::slice::from_ref(&first)),
            verified_gpu_device_set_digest(std::slice::from_ref(&reordered)),
            "enumeration index and transient available memory are not stable health witnesses"
        );

        let mut changed_driver = first.clone();
        changed_driver.target.as_mut().unwrap().driver_version =
            Some("windows-display:32.0.16.2000".to_owned());
        assert_ne!(
            verified_gpu_device_set_digest(std::slice::from_ref(&first)),
            verified_gpu_device_set_digest(std::slice::from_ref(&changed_driver))
        );
        assert_ne!(
            route_health_key(&first, &"b".repeat(64)),
            route_health_key(&changed_driver, &"b".repeat(64))
        );
        assert_ne!(
            route_health_key(&first, &"b".repeat(64)),
            route_health_key(&first, &"c".repeat(64))
        );
    }

    #[test]
    fn quarantined_verified_route_needs_two_explicit_successful_idle_probes() {
        let root = test_root("gpu-health-idle-probes");
        let path = Arc::new(root.join("health.json"));
        let route = verified_gpu_route(
            BackendKind::Vulkan,
            "native:pci:0000:01:00.0",
            "windows-display:32.0.16.1088",
            'a',
        );
        let catalog = verified_gpu_catalog(vec![route.clone()]);
        let key = route_health_key(&route, &"b".repeat(64)).unwrap();
        let health = GpuRouteHealth {
            path: Arc::clone(&path),
            witnesses: HealthWitnesses {
                app_build: DESKTOP_BUILD_ID.to_owned(),
                device_set_digest: catalog.device_set_digest.clone(),
            },
        };
        let mut observed_route = route;
        observed_route.health_key = Some(key.clone());
        health.record_failure(&observed_route, FailureObservation::WorkerCrash);
        assert!(matches!(
            health.cache().decision(&key),
            HealthDecision::Quarantined { .. }
        ));

        let blocked =
            health_filtered_gpu_route_plan(catalog.clone(), &"b".repeat(64), Some(&path), false);
        assert!(blocked.routes.is_empty());
        assert!(
            blocked
                .diagnostic
                .as_deref()
                .is_some_and(|value| value.contains("quarantined"))
        );

        for expected_cleared in [false, true] {
            assert!(health.grant_explicit_retry(&key));
            let probe = health_filtered_gpu_route_plan(
                catalog.clone(),
                &"b".repeat(64),
                Some(&path),
                false,
            );
            assert_eq!(probe.routes.len(), 1);
            assert!(
                probe
                    .health
                    .as_ref()
                    .unwrap()
                    .consume_retry(&probe.routes[0])
            );
            probe
                .health
                .as_ref()
                .unwrap()
                .record_idle_probe_success(&probe.routes[0]);
            assert_eq!(
                health.cache().decision(&key) == HealthDecision::Available,
                expected_cleared
            );
        }
        assert_eq!(health.cache().decision(&key), HealthDecision::Available);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn invalid_health_state_allows_only_two_step_idle_probe_recovery() {
        #[derive(Clone, Copy, Debug)]
        enum InvalidFixture {
            Corrupt,
            OversizedUnreadable,
            WitnessMismatch,
        }

        for fixture in [
            InvalidFixture::Corrupt,
            InvalidFixture::OversizedUnreadable,
            InvalidFixture::WitnessMismatch,
        ] {
            let root = test_root(&format!("gpu-health-recovery-{fixture:?}"));
            let path = Arc::new(root.join("health.json"));
            let route = verified_gpu_route(
                BackendKind::Vulkan,
                "native:pci:0000:01:00.0",
                "windows-display:32.0.16.1088",
                'a',
            );
            let catalog = verified_gpu_catalog(vec![route.clone()]);
            let artifact = missing_gguf_artifact();
            let RuntimeArtifact::Gguf(model) = &artifact else {
                unreachable!("fixture is GGUF")
            };
            let key = route_health_key(&route, &model.expected_sha256).unwrap();
            let witnesses = HealthWitnesses {
                app_build: DESKTOP_BUILD_ID.to_owned(),
                device_set_digest: catalog.device_set_digest.clone(),
            };

            match fixture {
                InvalidFixture::Corrupt => {
                    std::fs::write(&*path, b"{not-canonical-health-state").unwrap();
                }
                InvalidFixture::OversizedUnreadable => {
                    std::fs::write(&*path, vec![b'x'; 256 * 1024 + 1]).unwrap();
                }
                InvalidFixture::WitnessMismatch => {
                    let mut stale = HealthCache::open(
                        &*path,
                        HealthWitnesses {
                            app_build: "different-app-build".to_owned(),
                            device_set_digest: catalog.device_set_digest.clone(),
                        },
                        &GPU_HEALTH_CLOCK,
                    );
                    stale
                        .record_provider_failure(
                            key.clone(),
                            crate::gpu_worker_pack::health::FailureCode::WorkerCrash,
                        )
                        .unwrap();
                }
            }

            let registry = InferenceWorkerRegistry {
                routes: Arc::new(Vec::new()),
                gpu_routes: Arc::new(Mutex::new(GpuRouteCatalogCache::default())),
                auto_gpu_routes: Arc::new(Mutex::new(GpuRouteCatalogCache::default())),
                gpu_health_path: Some(Arc::clone(&path)),
                active_route: Arc::new(Mutex::new(None)),
                route_execution: Arc::new(Mutex::new(())),
                gpu_routes_for_testing: Some(catalog),
            };

            let inference = registry
                .routes_for_preference(AccelerationPreference::Gpu, &artifact)
                .unwrap();
            assert!(inference.routes.is_empty());
            assert_eq!(
                HealthCache::open(&*path, witnesses.clone(), &GPU_HEALTH_CLOCK).decision(&key),
                HealthDecision::InvalidOrUnprobed
            );

            let first_probe = registry
                .routes_for_preference_with_idle_health(
                    AccelerationPreference::Gpu,
                    &artifact,
                    true,
                )
                .unwrap();
            assert_eq!(first_probe.routes.len(), 1);
            first_probe
                .health
                .as_ref()
                .unwrap()
                .record_idle_probe_success(&first_probe.routes[0]);
            assert_eq!(
                HealthCache::open(&*path, witnesses.clone(), &GPU_HEALTH_CLOCK).decision(&key),
                HealthDecision::InvalidOrUnprobed
            );
            assert!(
                registry
                    .routes_for_preference(AccelerationPreference::Gpu, &artifact)
                    .unwrap()
                    .routes
                    .is_empty(),
                "normal inference must remain blocked after the first idle probe"
            );

            let second_probe = registry
                .routes_for_preference_with_idle_health(
                    AccelerationPreference::Gpu,
                    &artifact,
                    true,
                )
                .unwrap();
            assert_eq!(second_probe.routes.len(), 1);
            second_probe
                .health
                .as_ref()
                .unwrap()
                .record_idle_probe_success(&second_probe.routes[0]);
            assert_eq!(
                HealthCache::open(&*path, witnesses, &GPU_HEALTH_CLOCK).decision(&key),
                HealthDecision::Available
            );
            assert_eq!(
                registry
                    .routes_for_preference(AccelerationPreference::Gpu, &artifact)
                    .unwrap()
                    .routes
                    .len(),
                1,
                "normal inference becomes eligible only after the second idle probe"
            );
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn two_invalid_health_routes_recover_without_starvation_or_early_inference() {
        let root = test_root("gpu-health-two-route-recovery");
        let path = Arc::new(root.join("health.json"));
        let first_identity = "native:pci:0000:01:00.0";
        let second_identity = "native:pci:0000:02:00.0";
        let first = verified_gpu_route(
            BackendKind::Vulkan,
            first_identity,
            "windows-display:32.0.16.1088",
            'a',
        );
        let second = verified_gpu_route(
            BackendKind::Vulkan,
            second_identity,
            "windows-display:32.0.16.1088",
            'b',
        );
        let catalog = verified_gpu_catalog(vec![first.clone(), second.clone()]);
        let artifact = missing_gguf_artifact();
        let RuntimeArtifact::Gguf(model) = &artifact else {
            unreachable!("fixture is GGUF")
        };
        let first_key = route_health_key(&first, &model.expected_sha256).unwrap();
        let second_key = route_health_key(&second, &model.expected_sha256).unwrap();
        let witnesses = HealthWitnesses {
            app_build: DESKTOP_BUILD_ID.to_owned(),
            device_set_digest: catalog.device_set_digest.clone(),
        };
        std::fs::write(&*path, b"{corrupt-health-state").unwrap();

        let registry = InferenceWorkerRegistry {
            routes: Arc::new(Vec::new()),
            gpu_routes: Arc::new(Mutex::new(GpuRouteCatalogCache::default())),
            auto_gpu_routes: Arc::new(Mutex::new(GpuRouteCatalogCache::default())),
            gpu_health_path: Some(Arc::clone(&path)),
            active_route: Arc::new(Mutex::new(None)),
            route_execution: Arc::new(Mutex::new(())),
            gpu_routes_for_testing: Some(catalog),
        };
        let eligible_identities = || {
            registry
                .routes_for_preference(AccelerationPreference::Gpu, &artifact)
                .unwrap()
                .routes
                .iter()
                .map(|route| route.target.as_ref().unwrap().device_id.as_str().to_owned())
                .collect::<Vec<_>>()
        };

        for (probe_number, expected_identity, eligible_before, eligible_after) in [
            (
                1,
                first_identity,
                Vec::<String>::new(),
                Vec::<String>::new(),
            ),
            (
                2,
                first_identity,
                Vec::new(),
                vec![first_identity.to_owned()],
            ),
            (
                3,
                second_identity,
                vec![first_identity.to_owned()],
                vec![first_identity.to_owned()],
            ),
            (
                4,
                second_identity,
                vec![first_identity.to_owned()],
                vec![first_identity.to_owned(), second_identity.to_owned()],
            ),
        ] {
            assert_eq!(eligible_identities(), eligible_before);
            let probe = registry
                .routes_for_preference_with_idle_health(
                    AccelerationPreference::Gpu,
                    &artifact,
                    true,
                )
                .unwrap();
            let selected_identity = probe.routes[0].target.as_ref().unwrap().device_id.as_str();
            assert_eq!(
                selected_identity, expected_identity,
                "idle probe {probe_number}"
            );
            probe
                .health
                .as_ref()
                .unwrap()
                .record_idle_probe_success(&probe.routes[0]);
            assert_eq!(eligible_identities(), eligible_after);
        }

        let cache = HealthCache::open(&*path, witnesses, &GPU_HEALTH_CLOCK);
        assert_eq!(cache.decision(&first_key), HealthDecision::Available);
        assert_eq!(cache.decision(&second_key), HealthDecision::Available);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn explicit_retry_is_exact_single_use_and_content_failures_do_not_quarantine() {
        let root = test_root("gpu-health-explicit-retry");
        let path = Arc::new(root.join("health.json"));
        let route = verified_gpu_route(
            BackendKind::Cuda,
            "native:pci:0000:01:00.0",
            "windows-display:32.0.16.1088",
            'a',
        );
        let catalog = verified_gpu_catalog(vec![route.clone()]);
        let key = route_health_key(&route, &"b".repeat(64)).unwrap();
        let health = GpuRouteHealth {
            path: Arc::clone(&path),
            witnesses: HealthWitnesses {
                app_build: DESKTOP_BUILD_ID.to_owned(),
                device_set_digest: catalog.device_set_digest.clone(),
            },
        };
        let mut observed_route = route;
        observed_route.health_key = Some(key.clone());
        for error in [
            RuntimeError::InvalidAudio {
                sample_rate_hz: 48_000,
                channels: 2,
            },
            RuntimeError::ArtifactIntegrity {
                path: PathBuf::from("private-model-path.gguf"),
                message: "digest mismatch".to_owned(),
            },
            RuntimeError::Cancelled("cancelled".to_owned()),
        ] {
            health.record_failure(
                &observed_route,
                health_observation_for_runtime_error(&error, false, false),
            );
        }
        health.record_failure(
            &observed_route,
            health_observation_for_runtime_error(
                &RuntimeError::RetryableWorkerFailure("cancel transport".to_owned()),
                true,
                false,
            ),
        );
        health.record_failure(
            &observed_route,
            health_observation_for_runtime_error(
                &RuntimeError::RetryableWorkerFailure("after output".to_owned()),
                false,
                true,
            ),
        );
        assert_eq!(health.cache().decision(&key), HealthDecision::Available);

        health.record_failure(
            &observed_route,
            health_observation_for_runtime_error(
                &RuntimeError::RetryableWorkerFailure("worker exited".to_owned()),
                false,
                false,
            ),
        );
        let artifact = RuntimeArtifact::Gguf(RuntimeModel {
            id: ModelId::new("health-retry-fixture"),
            path: PathBuf::from("unused-health-retry-fixture.gguf"),
            format: ArtifactFormat::Gguf,
            expected_size_bytes: 1,
            expected_sha256: "b".repeat(64),
        });
        let target = observed_route.target.as_ref().unwrap().clone();
        let registry = InferenceWorkerRegistry {
            routes: Arc::new(Vec::new()),
            gpu_routes: Arc::new(Mutex::new(GpuRouteCatalogCache {
                successful: Some(catalog.clone()),
                failed: None,
            })),
            auto_gpu_routes: Arc::new(Mutex::new(GpuRouteCatalogCache::default())),
            gpu_health_path: Some(Arc::clone(&path)),
            active_route: Arc::new(Mutex::new(None)),
            route_execution: Arc::new(Mutex::new(())),
            gpu_routes_for_testing: None,
        };
        assert!(
            registry
                .grant_explicit_gpu_retry(&artifact, &target)
                .unwrap()
        );
        let mut wrong_target = target;
        wrong_target.driver_version = Some("windows-display:wrong".to_owned());
        assert!(
            !registry
                .grant_explicit_gpu_retry(&artifact, &wrong_target)
                .unwrap()
        );
        let plan = health_filtered_gpu_route_plan(catalog, &"b".repeat(64), Some(&path), false);
        assert_eq!(plan.routes.len(), 1);
        assert!(plan.routes[0].consume_retry_bypass);
        assert!(plan.health.as_ref().unwrap().consume_retry(&plan.routes[0]));
        assert!(!plan.health.as_ref().unwrap().consume_retry(&plan.routes[0]));

        let persisted = std::fs::read_to_string(&*path).unwrap();
        assert!(!persisted.contains("private-model-path"));
        assert!(!persisted.contains("worker exited"));
        assert!(!persisted.contains("cancel transport"));
        assert!(!persisted.contains("after output"));
        std::fs::remove_dir_all(root).unwrap();
    }

    fn diagnostics_with_typed_backend_selection() -> WireRuntimeDiagnostics {
        let mut unhealthy = BackendCandidate::available(backend_target(
            BackendKind::Vulkan,
            "vulkan-unhealthy",
            Some(3),
        ));
        unhealthy.availability = CandidateAvailability::Unhealthy;
        let candidates = vec![
            BackendCandidate::available(backend_target(BackendKind::Cuda, "cuda-primary", Some(1))),
            BackendCandidate::available(backend_target(
                BackendKind::Vulkan,
                "vulkan-fallback",
                Some(2),
            )),
            BackendCandidate::available(BackendTarget::cpu()),
            unhealthy,
        ];
        let qualification_policy = BackendQualificationPolicy::qualify_all_for_testing(
            OperatingSystem::Windows,
            &candidates,
        );
        let mut selection = select_backend(
            AccelerationPreference::Auto,
            &BackendSnapshot {
                operating_system: OperatingSystem::Windows,
                power_source: PowerSource::Ac,
                candidates,
                qualification_policy,
            },
        )
        .unwrap();
        selection.fallback_history.push(BackendFallback {
            target: backend_target(BackendKind::Cuda, "cuda-failed", Some(4)),
            category: BackendFailureCategory::WorkerFailed,
        });

        WireRuntimeDiagnostics {
            resolved_acceleration: ResolvedAcceleration {
                requested: AccelerationPreference::Auto,
                resolved: ComputeDevice::Gpu {
                    name: "Test CUDA device".to_owned(),
                },
                diagnostic: None,
                selection: Some(selection),
            },
            runtime_location: PathBuf::from("worker-runtime"),
            warm_reused: false,
            model_load_duration_ms: 42,
        }
    }

    #[test]
    fn typed_backend_selection_round_trips_within_worker_wire_bounds() {
        let diagnostics = diagnostics_with_typed_backend_selection();
        let durable_diagnostics: WireRuntimeDiagnostics =
            serde_json::from_slice(&serde_json::to_vec(&diagnostics).unwrap()).unwrap();
        validate_wire_diagnostics(&diagnostics).unwrap();
        let response = Control::RuntimeLoaded {
            execution: WireRuntimeLoadExecution {
                diagnostics: diagnostics.clone(),
                detected_architecture: "whisper".to_owned(),
                capabilities: RuntimeCapabilities::default(),
            },
        };
        validate_worker_response(&response).unwrap();
        let frame = control_frame(7, 9, &response).unwrap();
        assert!(frame.body.len() < MAX_CONTROL_BYTES);

        let (session_id, request_id, decoded) = parse_worker_control(frame).unwrap();
        assert_eq!((session_id, request_id), (7, 9));
        let Control::RuntimeLoaded { execution } = decoded else {
            panic!("expected a runtime-loaded worker response");
        };
        assert_eq!(execution.diagnostics, durable_diagnostics);
        assert_eq!(
            execution
                .diagnostics
                .resolved_acceleration
                .selection
                .as_ref()
                .unwrap()
                .target
                .process_index,
            None
        );

        let mut oversized_identity = diagnostics.clone();
        oversized_identity
            .resolved_acceleration
            .selection
            .as_mut()
            .unwrap()
            .target
            .device_id = DeviceIdentity::new("x".repeat(MAX_BACKEND_IDENTITY_BYTES + 1));
        assert!(validate_wire_diagnostics(&oversized_identity).is_err());

        let mut too_many_targets = diagnostics;
        let selection = too_many_targets
            .resolved_acceleration
            .selection
            .as_mut()
            .unwrap();
        selection.fallback_targets = vec![selection.target.clone(); MAX_BACKEND_SELECTION_TARGETS];
        assert!(validate_wire_diagnostics(&too_many_targets).is_err());
    }

    #[test]
    fn verified_pack_target_is_projected_into_typed_backend_diagnostics() {
        let mut diagnostics = diagnostics_with_typed_backend_selection();
        let observed = diagnostics
            .resolved_acceleration
            .selection
            .as_mut()
            .unwrap();
        observed.target.driver_version = Some("windows-display:32.0.15.8088".to_owned());
        let mut expected = observed.target.clone();
        observed.target.process_index = None;
        expected.pack = Some(crate::backend_policy::BackendPackIdentity {
            pack_id: "scribe-cuda-windows-x64".to_owned(),
            pack_version: "1.0.0".to_owned(),
            pack_digest: "a".repeat(64),
            security_epoch: 1,
            runtime_abi: crate::gpu_worker_pack::manifest::RUNTIME_ABI_VERSION,
        });
        reconcile_verified_pack_target(&mut diagnostics.resolved_acceleration, expected.clone())
            .unwrap();
        assert_eq!(
            diagnostics
                .resolved_acceleration
                .selection
                .as_ref()
                .unwrap()
                .target,
            expected
        );

        let mut changed_driver = expected;
        changed_driver.driver_version = Some("windows-display:32.0.15.9000".to_owned());
        assert!(
            reconcile_verified_pack_target(&mut diagnostics.resolved_acceleration, changed_driver)
                .is_err()
        );
    }

    #[test]
    fn runtime_errors_keep_a_stable_typed_wire_category() {
        let mut output = Vec::new();
        write_runtime_result(
            &mut output,
            7,
            9,
            Err(anyhow::Error::new(RuntimeError::Inference(
                "decode failed".to_owned(),
            ))),
        )
        .unwrap();
        match parse_worker_control(read_frame(&mut Cursor::new(output)).unwrap())
            .unwrap()
            .2
        {
            Control::RuntimeFailed { error } => {
                assert_eq!(error.code, WireRuntimeErrorCode::Inference);
                assert!(!error.fatal);
                assert!(matches!(error.into_runtime(), RuntimeError::Inference(_)));
            }
            other => panic!("expected typed runtime failure, got {other:?}"),
        }

        let invalid = WireRuntimeError::from_runtime(&RuntimeError::InvalidAudio {
            sample_rate_hz: 48_000,
            channels: 2,
        });
        assert!(matches!(
            invalid.into_runtime(),
            RuntimeError::InvalidAudio {
                sample_rate_hz: 48_000,
                channels: 2
            }
        ));

        for (runtime, code) in [
            (
                RuntimeError::BackendInitializationFailed(
                    "fixture initialization failure".to_owned(),
                ),
                WireRuntimeErrorCode::BackendInitializationFailed,
            ),
            (
                RuntimeError::BackendUnavailable("fixture device loss".to_owned()),
                WireRuntimeErrorCode::BackendUnavailable,
            ),
            (
                RuntimeError::OutOfMemory("fixture allocation failure".to_owned()),
                WireRuntimeErrorCode::OutOfMemory,
            ),
        ] {
            let wire = WireRuntimeError::from_runtime(&runtime);
            assert_eq!(wire.code, code);
            assert_eq!(wire.category, WireFailureCategory::Provider);
            assert_eq!(wire.retry, WireRetryDisposition::NextProviderBeforeOutput);
            assert!(matches!(
                (code, wire.into_runtime()),
                (
                    WireRuntimeErrorCode::BackendInitializationFailed,
                    RuntimeError::BackendInitializationFailed(_)
                ) | (
                    WireRuntimeErrorCode::BackendUnavailable,
                    RuntimeError::BackendUnavailable(_)
                ) | (
                    WireRuntimeErrorCode::OutOfMemory,
                    RuntimeError::OutOfMemory(_)
                )
            ));
        }
        let content_failure = WireRuntimeError::from_runtime(&RuntimeError::Engine(
            "fixture content failure".to_owned(),
        ));
        assert_eq!(content_failure.retry, WireRetryDisposition::Never);

        let artifact_path = PathBuf::from("models").join("fixture.gguf");
        let integrity = WireRuntimeError::from_runtime(&RuntimeError::ArtifactIntegrity {
            path: artifact_path.clone(),
            message: "digest mismatch".to_owned(),
        });
        let encoded = serde_json::to_vec(&integrity).unwrap();
        let decoded: WireRuntimeError = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(
            decoded.artifact_path.as_deref(),
            Some(artifact_path.as_path())
        );
        assert!(matches!(
            decoded.into_runtime(),
            RuntimeError::ArtifactIntegrity { path, .. } if path == artifact_path
        ));

        let legacy: WireRuntimeError = serde_json::from_value(serde_json::json!({
            "code": "artifact_integrity",
            "fatal": false,
            "message": "legacy integrity failure",
            "sample_rate_hz": null,
            "channels": null,
            "model_id": null
        }))
        .unwrap();
        assert!(matches!(
            legacy.into_runtime(),
            RuntimeError::ArtifactIntegrity { path, .. }
                if path == Path::new("<inference-worker artifact>")
        ));
    }

    #[test]
    fn runtime_warm_identity_includes_acceleration_and_reports_reuse() {
        let root = test_root("runtime-warm-identity");
        let spec = spec_with_roles(
            &root,
            OnnxModelFamily::NemoCtc,
            &[OnnxFileRole::Model, OnnxFileRole::Tokens],
        );
        let artifact = WireRuntimeArtifact::OnnxBundle(spec.clone());
        let mut aliased = spec;
        aliased.root = aliased.root.join(".");
        assert_eq!(
            wire_artifact_identity(&artifact, AccelerationPreference::Cpu).unwrap(),
            wire_artifact_identity(
                &WireRuntimeArtifact::OnnxBundle(aliased),
                AccelerationPreference::Cpu
            )
            .unwrap()
        );
        let router = RuntimeRouter::new();
        let factory = FakeRecognizerFactory::new();
        let mut recognizer = None;
        let mut runtime = None;
        let mut stream = None;
        let first = load_worker_runtime(
            &router,
            &factory,
            &mut recognizer,
            &mut runtime,
            &mut stream,
            artifact.clone(),
            AccelerationPreference::Cpu,
        )
        .unwrap();
        assert!(!first.diagnostics.warm_reused);
        let second = load_worker_runtime(
            &router,
            &factory,
            &mut recognizer,
            &mut runtime,
            &mut stream,
            artifact.clone(),
            AccelerationPreference::Cpu,
        )
        .unwrap();
        assert!(second.diagnostics.warm_reused);
        assert_eq!(second.diagnostics.model_load_duration_ms, 0);
        assert!(
            load_worker_runtime(
                &router,
                &factory,
                &mut recognizer,
                &mut runtime,
                &mut stream,
                artifact,
                AccelerationPreference::Gpu,
            )
            .is_err()
        );
        assert_eq!(factory.snapshot().recognizers_created, 1);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn unload_without_a_generation_does_not_spawn_a_worker() {
        let launcher = Arc::new(TestLauncher::new([]));
        let supervisor = ProcessWorkerSupervisor::unstarted_with_launcher_and_deadlines(
            launcher.clone(),
            SupervisorDeadlines::default(),
        );
        supervisor.unload().unwrap();
        assert_eq!(launcher.launches.load(Ordering::Acquire), 0);
    }

    #[test]
    fn runtime_response_generation_binding_does_not_respawn_after_exit() {
        let launcher = Arc::new(TestLauncher::new([
            TestMode::RuntimeLoadThenExit,
            TestMode::Normal,
        ]));
        let inference = InferenceWorkerSupervisor {
            transport: ProcessWorkerSupervisor::unstarted_with_launcher_and_deadlines(
                launcher.clone(),
                short_deadlines(),
            ),
            next_correlation: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        };

        let loaded = inference
            .load(missing_gguf_artifact(), AccelerationPreference::Cpu)
            .unwrap();

        assert_eq!(loaded.detected_architecture, "test-runtime");
        let wait_until = Instant::now() + Duration::from_millis(250);
        while inference.transport.current_generation().unwrap().is_some()
            && Instant::now() < wait_until
        {
            std::thread::yield_now();
        }
        assert_eq!(inference.transport.current_generation().unwrap(), None);
        assert_eq!(
            launcher.launches.load(Ordering::Acquire),
            1,
            "post-response reconciliation must not inspect or spawn a replacement generation"
        );
    }

    #[test]
    fn onnx_gpu_rejection_happens_before_inference_child_spawn() {
        let root = test_root("gpu-before-spawn");
        let artifact = RuntimeArtifact::OnnxBundle(spec_with_roles(
            &root,
            OnnxModelFamily::NemoCtc,
            NEMO_CTC_ROLES,
        ));
        let launcher = Arc::new(TestLauncher::new([TestMode::Normal]));
        let inference = InferenceWorkerSupervisor {
            transport: ProcessWorkerSupervisor::unstarted_with_launcher_and_deadlines(
                launcher.clone(),
                short_deadlines(),
            ),
            next_correlation: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        };

        let error = inference
            .load(artifact, AccelerationPreference::Gpu)
            .unwrap_err();
        assert!(matches!(error, RuntimeError::OnnxUnavailable(_)));
        assert_eq!(launcher.launches.load(Ordering::Acquire), 0);
        std::fs::remove_dir_all(root).unwrap();
    }

    fn missing_gguf_artifact() -> RuntimeArtifact {
        RuntimeArtifact::Gguf(RuntimeModel {
            id: ModelId::new("missing-worker-fixture"),
            path: PathBuf::from("missing-worker-fixture.gguf"),
            format: ArtifactFormat::Gguf,
            expected_size_bytes: 1,
            expected_sha256: "0".repeat(64),
        })
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "requires a clean-built fixture-signed Vulkan pack, pinned GGUF/WAV fixtures, and a compatible physical GPU"]
    fn verified_vulkan_fixture_pack_scif_model_hardware_smoke() {
        let required_path = |name: &str| {
            PathBuf::from(
                std::env::var_os(name)
                    .unwrap_or_else(|| panic!("set {name} to the exact fixture path")),
            )
        };
        let required_hash = |name: &str| {
            let value = std::env::var(name)
                .unwrap_or_else(|_| panic!("set {name} to the exact lowercase SHA-256"));
            assert!(
                value.len() == 64
                    && value
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
                "{name} must be a canonical lowercase SHA-256"
            );
            value
        };
        let exact_file = |path: &Path, expected: &str| {
            let bytes = std::fs::read(path).expect("fixture file must be readable");
            assert_eq!(format!("{:x}", Sha256::digest(&bytes)), expected);
            bytes.len() as u64
        };

        let pack_root = required_path("SCRIBE_GPU_FIXTURE_PACK_ROOT");
        let model_path = required_path("SCRIBE_GPU_FIXTURE_MODEL");
        let model_sha256 = required_hash("SCRIBE_GPU_FIXTURE_MODEL_SHA256");
        let wav_path = required_path("SCRIBE_GPU_FIXTURE_WAV");
        let wav_sha256 = required_hash("SCRIBE_GPU_FIXTURE_WAV_SHA256");
        let expected_text = std::env::var("SCRIBE_GPU_FIXTURE_EXPECTED_TRANSCRIPT")
            .expect("set SCRIBE_GPU_FIXTURE_EXPECTED_TRANSCRIPT to a known spoken phrase")
            .to_ascii_lowercase();
        assert!(!expected_text.trim().is_empty());
        let model_size = exact_file(&model_path, &model_sha256);
        exact_file(&wav_path, &wav_sha256);

        let lease_owner = {
            let (lease_owner, lease) =
                crate::gpu_worker_pack::manifest::test_support::lease_existing_fixture(&pack_root)
                    .expect("fixture pack must verify into an immutable retained lease");
            let probe = InferenceWorkerSupervisor::for_pack_probe(Arc::new(lease));
            let bindings = probe
                .verified_pack_bindings()
                .expect("Vulkan pack probe must complete its challenge-bound SCIF Hello");
            probe.shutdown().unwrap();
            drop(probe);
            let expected_identity = std::env::var("SCRIBE_GPU_FIXTURE_STABLE_DEVICE_ID").ok();
            let binding = bindings
                .into_iter()
                .find(|candidate| {
                    let target = candidate.backend_target();
                    target.backend == BackendKind::Vulkan
                        && expected_identity.as_ref().map_or_else(
                            || {
                                target.vendor == GpuVendor::Nvidia
                                    && target.device_class == DeviceClass::DiscreteGpu
                            },
                            |identity| target.device_id.as_str() == identity,
                        )
                })
                .expect("fixture pack must advertise a Vulkan device");
            let target = binding.backend_target();
            assert!(
                target
                    .driver_version
                    .as_ref()
                    .is_some_and(|value| !value.is_empty())
            );
            assert!(target.memory_total_bytes > 0);
            if let Some(expected_identity) = expected_identity {
                assert_eq!(target.device_id.as_str(), expected_identity);
            }
            let gpu_route = InferenceWorkerRoute {
                provider: WorkerProvider::Vulkan,
                supervisor: InferenceWorkerSupervisor::for_pack_binding(binding),
                target: Some(target.clone()),
                health_key: None,
                consume_retry_bypass: false,
                auto_evidence_id: None,
            };
            let gpu_routes = Arc::new(vec![gpu_route]);
            let cpu_launcher = Arc::new(TestLauncher::new([]));
            let registry = InferenceWorkerRegistry {
                routes: Arc::new(vec![InferenceWorkerRoute {
                    provider: WorkerProvider::Cpu,
                    supervisor: InferenceWorkerSupervisor {
                        transport: ProcessWorkerSupervisor::unstarted_with_launcher_and_deadlines(
                            cpu_launcher.clone(),
                            short_deadlines(),
                        ),
                        next_correlation: Arc::new(std::sync::atomic::AtomicU64::new(0)),
                    },
                    target: Some(BackendTarget::cpu()),
                    health_key: None,
                    consume_retry_bypass: false,
                    auto_evidence_id: None,
                }]),
                gpu_routes: Arc::new(Mutex::new(GpuRouteCatalogCache::default())),
                auto_gpu_routes: Arc::new(Mutex::new(GpuRouteCatalogCache::default())),
                gpu_health_path: None,
                active_route: Arc::new(Mutex::new(None)),
                route_execution: Arc::new(Mutex::new(())),
                gpu_routes_for_testing: Some(VerifiedGpuRouteCatalog {
                    device_set_digest: verified_gpu_device_set_digest(&gpu_routes),
                    routes: gpu_routes,
                    diagnostic: Some("fixture-only hardware smoke".to_owned()),
                    discovery_fingerprint: "fixture-hardware-smoke".to_owned(),
                    provider_probe_incomplete: false,
                    skipped_targets: Vec::new(),
                }),
            };
            let artifact = RuntimeArtifact::Gguf(RuntimeModel {
                id: ModelId::new("whisper_cpp_base_en"),
                path: model_path,
                format: ArtifactFormat::Gguf,
                expected_size_bytes: model_size,
                expected_sha256: model_sha256,
            });
            let loaded = registry
                .load(artifact.clone(), AccelerationPreference::Gpu)
                .expect("explicit GPU load must succeed through the verified Vulkan worker");
            assert_eq!(
                loaded
                    .diagnostics
                    .resolved_acceleration
                    .selection
                    .as_ref()
                    .expect("GPU load must report typed selection")
                    .target,
                target
            );
            let audio = PreparedAudio::from_wav_path(wav_path).unwrap();
            let cancellation = std::sync::atomic::AtomicU64::new(0);
            let execution = registry
                .transcribe(
                    artifact,
                    AccelerationPreference::Gpu,
                    &audio,
                    TranscriptionOptions::default(),
                    0,
                    &cancellation,
                )
                .expect("explicit GPU transcription must succeed through SCIF");
            assert!(execution.diagnostics.warm_reused);
            assert!(
                execution
                    .transcript
                    .text
                    .to_ascii_lowercase()
                    .contains(&expected_text),
                "Vulkan transcript did not contain the known fixture phrase: {:?}",
                execution.transcript.text
            );
            assert_eq!(cpu_launcher.launches.load(Ordering::Acquire), 0);
            println!(
                "verified Vulkan SCIF smoke: device={} driver={} memory={} transcript_bytes={} warm_reused={}",
                target.device_id.as_str(),
                target.driver_version.as_deref().unwrap(),
                target.memory_total_bytes,
                execution.transcript.text.len(),
                execution.diagnostics.warm_reused
            );
            registry.shutdown().unwrap();
            lease_owner
        };
        let cleanup_deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match std::fs::remove_dir_all(&lease_owner) {
                Ok(()) => break,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
                Err(error)
                    if (error.kind() == std::io::ErrorKind::PermissionDenied
                        || error.raw_os_error() == Some(32))
                        && Instant::now() < cleanup_deadline =>
                {
                    std::thread::sleep(Duration::from_millis(25));
                }
                Err(error) => panic!("could not remove reaped fixture-pack lease: {error}"),
            }
        }
    }

    #[test]
    fn cpu_only_registry_never_silently_serves_explicit_gpu() {
        let launcher = Arc::new(TestLauncher::new([TestMode::Normal]));
        let registry = InferenceWorkerRegistry::with_cpu_supervisor(InferenceWorkerSupervisor {
            transport: ProcessWorkerSupervisor::unstarted_with_launcher_and_deadlines(
                launcher.clone(),
                short_deadlines(),
            ),
            next_correlation: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        });
        let error = registry
            .load(missing_gguf_artifact(), AccelerationPreference::Gpu)
            .unwrap_err()
            .to_string();
        assert!(error.contains("CPU fallback is forbidden"));
        assert_eq!(launcher.launches.load(Ordering::Acquire), 0);
    }

    #[test]
    fn auto_reports_pack_discovery_state_without_launching_a_gpu_probe() {
        let launcher = Arc::new(TestLauncher::new([]));
        let registry = InferenceWorkerRegistry::with_cpu_supervisor(InferenceWorkerSupervisor {
            transport: ProcessWorkerSupervisor::unstarted_with_launcher_and_deadlines(
                launcher.clone(),
                short_deadlines(),
            ),
            next_correlation: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        });
        let artifact = missing_gguf_artifact();
        let plan = registry
            .routes_for_preference(AccelerationPreference::Auto, &artifact)
            .unwrap();
        assert_eq!(plan.routes.len(), 1);
        assert_eq!(plan.routes[0].provider, WorkerProvider::Cpu);
        assert!(plan.diagnostic.is_some_and(|value| !value.is_empty()));
        assert_eq!(launcher.launches.load(Ordering::Acquire), 0);
        let cache = registry.gpu_routes.lock().unwrap();
        assert!(cache.successful.is_none());
        assert!(cache.failed.is_none());
        drop(cache);
        let auto_cache = registry.auto_gpu_routes.lock().unwrap();
        assert!(auto_cache.successful.is_none());
        assert!(auto_cache.failed.is_none());
    }

    #[test]
    fn auto_gpu_oom_falls_back_to_cpu_and_invalidates_the_cached_binding() {
        let gpu_launcher = Arc::new(TestLauncher::new([TestMode::RuntimeFailureOnLoad(
            RuntimeError::OutOfMemory("fixture GPU allocation failed".to_owned()),
        )]));
        let cpu_launcher = Arc::new(TestLauncher::new([TestMode::RuntimeLoadThenExit]));
        let mut gpu_route = verified_gpu_route(
            BackendKind::Cuda,
            "native:pci:0000:01:00.0",
            "windows-display:32.0.16.1088",
            'a',
        );
        gpu_route.supervisor = inference_supervisor_with_launcher(Arc::clone(&gpu_launcher));
        gpu_route.auto_evidence_id = Some("failed-cuda-evidence".to_owned());
        let catalog = verified_gpu_catalog(vec![gpu_route]);

        let mut registry = InferenceWorkerRegistry::with_cpu_supervisor(
            inference_supervisor_with_launcher(Arc::clone(&cpu_launcher)),
        );
        registry
            .auto_gpu_routes
            .lock()
            .unwrap()
            .record_probe(catalog.clone(), Instant::now());
        registry
            .gpu_routes
            .lock()
            .unwrap()
            .record_probe(catalog.clone(), Instant::now());
        registry.gpu_routes_for_testing = Some(catalog);

        let execution = registry
            .load(missing_gguf_artifact(), AccelerationPreference::Auto)
            .expect("Auto must retain its guaranteed CPU fallback after a pre-output GPU OOM");

        assert_eq!(gpu_launcher.launches.load(Ordering::Acquire), 1);
        assert_eq!(cpu_launcher.launches.load(Ordering::Acquire), 1);
        let selection = execution
            .diagnostics
            .resolved_acceleration
            .selection
            .as_ref()
            .expect("Auto CPU fallback must retain typed selection diagnostics");
        assert_eq!(selection.target.backend, BackendKind::Cpu);
        assert_eq!(selection.reason, BackendSelectionReason::AutoCpuFallback);
        assert_eq!(selection.fallback_history.len(), 1);
        assert_eq!(
            selection.fallback_history[0].category,
            BackendFailureCategory::OutOfMemory
        );
        assert_eq!(
            selection.fallback_history[0].target.backend,
            BackendKind::Cuda
        );
        assert!(
            !execution
                .diagnostics
                .resolved_acceleration
                .diagnostic
                .as_deref()
                .unwrap_or_default()
                .contains("failed-cuda-evidence")
        );
        for cache in [&registry.auto_gpu_routes, &registry.gpu_routes] {
            let cache = cache.lock().unwrap();
            assert!(cache.successful.is_none());
            assert!(cache.failed.is_none());
            assert!(matches!(
                cache.lookup("test-catalog", Instant::now()),
                GpuRouteCatalogLookup::Probe
            ));
        }
    }

    #[test]
    fn request_bound_device_epoch_rejection_keeps_auto_safe_and_explicit_gpu_strict() {
        struct ResetDeviceEpochRejection;
        impl Drop for ResetDeviceEpochRejection {
            fn drop(&mut self) {
                crate::gpu_worker_pack::set_test_device_epoch_rejected(false);
            }
        }

        crate::gpu_worker_pack::set_test_device_epoch_rejected(true);
        let _reset = ResetDeviceEpochRejection;

        let auto_gpu_launcher = Arc::new(TestLauncher::new([TestMode::Normal]));
        let auto_cpu_launcher = Arc::new(TestLauncher::new([TestMode::RuntimeLoadThenExit]));
        let mut auto_gpu_route = verified_gpu_route(
            BackendKind::Metal,
            "native:registry:metal-0",
            "macos:metal",
            'a',
        );
        auto_gpu_route.supervisor =
            inference_supervisor_with_launcher(Arc::clone(&auto_gpu_launcher));
        let mut auto_registry = InferenceWorkerRegistry::with_cpu_supervisor(
            inference_supervisor_with_launcher(Arc::clone(&auto_cpu_launcher)),
        );
        auto_registry.gpu_routes_for_testing = Some(verified_gpu_catalog(vec![auto_gpu_route]));

        let execution = auto_registry
            .load(missing_gguf_artifact(), AccelerationPreference::Auto)
            .expect("Auto must skip a request-bound rollback rejection and use CPU");
        assert_eq!(auto_gpu_launcher.launches.load(Ordering::Acquire), 0);
        assert_eq!(auto_cpu_launcher.launches.load(Ordering::Acquire), 1);
        let selection = execution
            .diagnostics
            .resolved_acceleration
            .selection
            .expect("Auto rollback fallback must retain typed diagnostics");
        assert_eq!(selection.target.backend, BackendKind::Cpu);
        assert_eq!(selection.fallback_history.len(), 1);

        let explicit_gpu_launcher = Arc::new(TestLauncher::new([TestMode::Normal]));
        let explicit_cpu_launcher = Arc::new(TestLauncher::new([TestMode::Normal]));
        let mut explicit_gpu_route = verified_gpu_route(
            BackendKind::Metal,
            "native:registry:metal-0",
            "macos:metal",
            'b',
        );
        explicit_gpu_route.supervisor =
            inference_supervisor_with_launcher(Arc::clone(&explicit_gpu_launcher));
        let mut explicit_registry = InferenceWorkerRegistry::with_cpu_supervisor(
            inference_supervisor_with_launcher(Arc::clone(&explicit_cpu_launcher)),
        );
        explicit_registry.gpu_routes_for_testing =
            Some(verified_gpu_catalog(vec![explicit_gpu_route]));

        let error = explicit_registry
            .load(missing_gguf_artifact(), AccelerationPreference::Gpu)
            .expect_err("explicit GPU must never use CPU after rollback rejection")
            .to_string();
        assert!(error.contains("device-local release rollback authority"));
        assert!(error.contains("CPU fallback is forbidden"));
        assert_eq!(explicit_gpu_launcher.launches.load(Ordering::Acquire), 0);
        assert_eq!(explicit_cpu_launcher.launches.load(Ordering::Acquire), 0);
    }

    #[test]
    fn warm_gpu_route_rechecks_device_epoch_before_reuse() {
        struct ResetDeviceEpochRejection;
        impl Drop for ResetDeviceEpochRejection {
            fn drop(&mut self) {
                crate::gpu_worker_pack::set_test_device_epoch_rejected(false);
            }
        }

        let route = verified_gpu_route(
            BackendKind::Metal,
            "native:registry:metal-0",
            "macos:metal",
            'c',
        );
        let registry =
            InferenceWorkerRegistry::with_cpu_supervisor(InferenceWorkerSupervisor::unstarted());
        registry.prepare_route(&route).unwrap();
        assert!(registry.active_route.lock().unwrap().is_some());

        crate::gpu_worker_pack::set_test_device_epoch_rejected(true);
        let _reset = ResetDeviceEpochRejection;
        assert!(matches!(
            registry.prepare_route(&route),
            Err(RoutePreparationError::DeviceRollbackAuthority(_))
        ));
        assert!(
            registry.active_route.lock().unwrap().is_none(),
            "rollback denial must retire the already-warm GPU route"
        );
    }

    #[test]
    fn auto_gpu_initialization_and_device_loss_failures_use_typed_cpu_fallback() {
        for (runtime_error, expected_category) in [
            (
                RuntimeError::BackendInitializationFailed(
                    "fixture GPU initialization failed".to_owned(),
                ),
                BackendFailureCategory::InitializationFailed,
            ),
            (
                RuntimeError::BackendUnavailable("fixture GPU device was lost".to_owned()),
                BackendFailureCategory::DeviceLost,
            ),
        ] {
            let gpu_launcher = Arc::new(TestLauncher::new([TestMode::RuntimeFailureOnLoad(
                runtime_error,
            )]));
            let cpu_launcher = Arc::new(TestLauncher::new([TestMode::RuntimeLoadThenExit]));
            let mut gpu_route = verified_gpu_route(
                BackendKind::Vulkan,
                "native:pci:0000:01:00.0",
                "windows-display:32.0.16.1088",
                'a',
            );
            gpu_route.supervisor = inference_supervisor_with_launcher(Arc::clone(&gpu_launcher));
            let mut registry = InferenceWorkerRegistry::with_cpu_supervisor(
                inference_supervisor_with_launcher(Arc::clone(&cpu_launcher)),
            );
            registry.gpu_routes_for_testing = Some(verified_gpu_catalog(vec![gpu_route]));

            let execution = registry
                .load(missing_gguf_artifact(), AccelerationPreference::Auto)
                .expect("a typed pre-output provider failure must retain Auto's CPU fallback");

            assert_eq!(gpu_launcher.launches.load(Ordering::Acquire), 1);
            assert_eq!(cpu_launcher.launches.load(Ordering::Acquire), 1);
            let selection = execution
                .diagnostics
                .resolved_acceleration
                .selection
                .as_ref()
                .expect("provider fallback must retain typed diagnostics");
            assert_eq!(selection.target.backend, BackendKind::Cpu);
            assert_eq!(selection.fallback_history.len(), 1);
            assert_eq!(selection.fallback_history[0].category, expected_category);
        }
    }

    #[test]
    fn auto_diagnostics_use_the_actual_winning_gpu_evidence_and_fallback_history() {
        let first_launcher = Arc::new(TestLauncher::new([TestMode::RuntimeFailureOnLoad(
            RuntimeError::OutOfMemory("fixture GPU allocation failed".to_owned()),
        )]));
        let second_launcher = Arc::new(TestLauncher::new([TestMode::RuntimeLoadThenExit]));
        let cpu_launcher = Arc::new(TestLauncher::new([]));
        let mut first = verified_gpu_route(
            BackendKind::Cuda,
            "native:pci:0000:01:00.0",
            "windows-display:32.0.16.1088",
            'a',
        );
        first.supervisor = inference_supervisor_with_launcher(Arc::clone(&first_launcher));
        first.auto_evidence_id = Some("failed-cuda-evidence".to_owned());
        let mut second = verified_gpu_route(
            BackendKind::Vulkan,
            "native:pci:0000:02:00.0",
            "windows-display:32.0.16.1088",
            'b',
        );
        second.supervisor = inference_supervisor_with_launcher(Arc::clone(&second_launcher));
        second.auto_evidence_id = Some("winning-vulkan-evidence".to_owned());
        let skipped_target = verified_gpu_route(
            BackendKind::Vulkan,
            "native:pci:0000:03:00.0",
            "windows-display:32.0.16.1088",
            'c',
        )
        .target
        .unwrap();
        let mut catalog = verified_gpu_catalog(vec![first, second]);
        catalog.skipped_targets.push(SkippedBackend {
            target: skipped_target,
            reason: BackendSkipReason::Quarantined,
        });

        let mut registry = InferenceWorkerRegistry::with_cpu_supervisor(
            inference_supervisor_with_launcher(Arc::clone(&cpu_launcher)),
        );
        registry.gpu_routes_for_testing = Some(catalog);
        let execution = registry
            .load(missing_gguf_artifact(), AccelerationPreference::Auto)
            .expect("the second qualified GPU route should win after a pre-output OOM");

        assert_eq!(first_launcher.launches.load(Ordering::Acquire), 1);
        assert_eq!(second_launcher.launches.load(Ordering::Acquire), 1);
        assert_eq!(cpu_launcher.launches.load(Ordering::Acquire), 0);
        let selection = execution
            .diagnostics
            .resolved_acceleration
            .selection
            .as_ref()
            .expect("winning GPU route must retain typed Auto diagnostics");
        assert_eq!(selection.target.backend, BackendKind::Vulkan);
        assert_eq!(selection.reason, BackendSelectionReason::AutoPriority);
        assert_eq!(selection.fallback_history.len(), 1);
        assert_eq!(
            selection.fallback_history[0].category,
            BackendFailureCategory::OutOfMemory
        );
        assert_eq!(selection.skipped_targets.len(), 1);
        assert_eq!(
            selection.skipped_targets[0].reason,
            BackendSkipReason::Quarantined
        );
        let diagnostic = execution
            .diagnostics
            .resolved_acceleration
            .diagnostic
            .as_deref()
            .unwrap_or_default();
        assert!(diagnostic.contains("winning-vulkan-evidence"));
        assert!(!diagnostic.contains("failed-cuda-evidence"));
    }

    #[test]
    fn power_filtering_does_not_change_the_physical_device_health_witness() {
        let mut route = verified_gpu_route(
            BackendKind::Cuda,
            "native:pci:0000:01:00.0",
            "windows-display:32.0.16.1088",
            'a',
        );
        let target = route.target.as_mut().unwrap();
        target.provider_id = ProviderIdentity::new("transcribe-cpp-ggml-cuda");
        target.vendor = GpuVendor::Nvidia;
        target.device_class = DeviceClass::DiscreteGpu;
        target.pack.as_mut().unwrap().pack_id = "scribe-cuda-windows-x64".to_owned();
        let model_digest = "b".repeat(64);
        let policy = fixture_auto_policy(target, &model_digest, "power-witness-fixture-v1");
        let catalog = verified_gpu_catalog(vec![route]);

        let ac = auto_qualified_gpu_route_catalog(
            catalog.clone(),
            &policy,
            &model_digest,
            PowerSource::Ac,
        );
        let battery =
            auto_qualified_gpu_route_catalog(catalog, &policy, &model_digest, PowerSource::Battery);

        assert_eq!(ac.routes.len(), 1);
        assert!(battery.routes.is_empty());
        assert_eq!(
            ac.device_set_digest, battery.device_set_digest,
            "power-policy filtering must not invalidate health for an unchanged physical device set"
        );
    }

    #[test]
    fn auto_diagnostics_preserve_the_selection_time_power_source() {
        let route = verified_gpu_route(
            BackendKind::Cuda,
            "native:pci:0000:01:00.0",
            "windows-display:32.0.16.1088",
            'a',
        );
        let mut diagnostics: NativeRuntimeDiagnostics =
            diagnostics_with_typed_backend_selection().into();

        InferenceWorkerRegistry::project_auto_route_diagnostics(
            &mut diagnostics,
            &route,
            PowerSource::Battery,
            Vec::new(),
            &[],
            &[],
        );

        let selection = diagnostics
            .resolved_acceleration
            .selection
            .expect("typed Auto selection");
        assert_eq!(selection.requested, AccelerationPreference::Auto);
        assert_eq!(selection.reason, BackendSelectionReason::AutoPriority);
        assert_eq!(selection.power_source, PowerSource::Battery);
        assert_eq!(
            selection.power_policy,
            PowerPolicyDecision::BatteryEfficientGpuOnly
        );
        assert_eq!(
            selection.qualification_policy_version,
            AUTO_QUALIFICATION_POLICY_VERSION
        );
    }

    #[test]
    fn auto_reserves_one_of_four_bounded_attempts_for_cpu() {
        let gpu_routes = (0..4)
            .map(|index| {
                verified_gpu_route(
                    BackendKind::Vulkan,
                    &format!("native:pci:0000:0{}:00.0", index + 1),
                    "windows-display:32.0.16.1088",
                    char::from(b'a' + index as u8),
                )
            })
            .collect::<Vec<_>>();
        let mut cpu = verified_gpu_route(
            BackendKind::Vulkan,
            "native:pci:0000:05:00.0",
            "windows-display:32.0.16.1088",
            'e',
        );
        cpu.provider = WorkerProvider::Cpu;
        cpu.target = Some(BackendTarget::cpu());
        let cpu_routes = Arc::new(vec![cpu]);
        let mut plan = InferenceWorkerRoutePlan {
            routes: Arc::new(gpu_routes),
            diagnostic: None,
            health: None,
            selection_power_source: Some(PowerSource::Ac),
            skipped_targets: Vec::new(),
        };

        reserve_auto_cpu_fallback(&mut plan, &cpu_routes);

        assert_eq!(plan.routes.len(), MAX_INFERENCE_ROUTE_ATTEMPTS);
        assert!(plan.routes[..3].iter().all(|route| route.provider.is_gpu()));
        assert_eq!(plan.routes[3].provider, WorkerProvider::Cpu);
        assert!(plan.diagnostic.as_deref().is_some_and(|diagnostic| {
            diagnostic.contains("reserve Auto's guaranteed CPU fallback")
        }));
    }

    #[test]
    fn auto_qualification_prefilters_verified_packs_before_provider_probe() {
        let root = crate::gpu_worker_pack::manifest::test_support::temp_root(
            "auto-qualification-preprobe",
        );
        let (_verifier, lease) =
            crate::gpu_worker_pack::manifest::test_support::leased_fixture(&root);
        let pack_id = lease.verified_pack().pack_id.clone();
        let discovery = crate::gpu_worker_pack::PackLeaseDiscovery {
            leases: vec![Arc::new(lease)],
            diagnostics: Vec::new(),
            catalog_generation: Some("fixture-generation".to_owned()),
        };
        let policy = AutoQualificationPolicy::embedded_windows_x64().unwrap();
        let filtered = auto_qualified_pack_discovery(discovery, policy, &"b".repeat(64));
        assert!(filtered.leases.is_empty());
        assert_eq!(
            filtered.diagnostics,
            vec![crate::gpu_worker_pack::PackDiscoveryDiagnostic::pack(
                crate::gpu_worker_pack::PackDiscoveryIssue::NotAutoQualified,
                &pack_id,
                PackBackend::Vulkan,
            )]
        );
        drop(filtered);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn failed_gpu_probe_backoff_allows_same_fingerprint_recovery_and_warm_reuse() {
        let now = Instant::now();
        let mut cache = GpuRouteCatalogCache::default();
        let mut failed = verified_gpu_catalog(Vec::new());
        failed.diagnostic = Some("verified provider probe failed".to_owned());
        cache.record_probe(failed.clone(), now);
        assert!(cache.successful.is_none());
        assert!(matches!(
            cache.lookup("test-catalog", now),
            GpuRouteCatalogLookup::Backoff(Some(message))
                if message == "verified provider probe failed"
        ));

        assert!(!cache.grant_explicit_probe_retry("changed-catalog", now));
        assert!(cache.grant_explicit_probe_retry("test-catalog", now));
        assert!(
            !cache.grant_explicit_probe_retry("test-catalog", now),
            "an explicit provider reprobe grant is one-shot"
        );
        assert!(matches!(
            cache.lookup("test-catalog", now),
            GpuRouteCatalogLookup::Probe
        ));

        cache.record_probe(failed, now);
        let later = now + GPU_PROVIDER_PROBE_BACKOFF;
        assert!(matches!(
            cache.lookup("test-catalog", later),
            GpuRouteCatalogLookup::Probe
        ));

        let successful = verified_gpu_catalog(vec![verified_gpu_route(
            BackendKind::Vulkan,
            "native:pci:0000:01:00.0",
            "windows-display:32.0.16.1088",
            'a',
        )]);
        cache.record_probe(successful, later);
        assert!(matches!(
            cache.lookup("test-catalog", later + GPU_PROVIDER_PROBE_BACKOFF),
            GpuRouteCatalogLookup::Successful(catalog) if catalog.routes.len() == 1
        ));
        assert!(matches!(
            cache.lookup("changed-catalog", later),
            GpuRouteCatalogLookup::Probe
        ));
    }

    #[test]
    fn runtime_binding_failure_invalidates_a_successful_catalog_and_forces_reprobe() {
        let now = Instant::now();
        let mut cache = GpuRouteCatalogCache::default();
        let catalog = verified_gpu_catalog(vec![verified_gpu_route(
            BackendKind::Vulkan,
            "native:pci:0000:01:00.0",
            "windows-display:32.0.16.1088",
            'a',
        )]);
        cache.record_probe(catalog, now);
        assert!(matches!(
            cache.lookup("test-catalog", now),
            GpuRouteCatalogLookup::Successful(_)
        ));

        cache.invalidate_after_runtime_failure();

        assert!(cache.successful.is_none());
        assert!(cache.failed.is_none());
        assert!(matches!(
            cache.lookup("test-catalog", now),
            GpuRouteCatalogLookup::Probe
        ));
    }

    #[test]
    fn failed_gpu_probe_backoff_starts_after_a_long_probe_completes() {
        let probe_started_at = Instant::now();
        let probe_completed_at =
            probe_started_at + GPU_PROVIDER_PROBE_BACKOFF + Duration::from_secs(5);
        let mut cache = GpuRouteCatalogCache::default();
        let mut failed = verified_gpu_catalog(Vec::new());
        failed.diagnostic = Some("slow verified provider probe failed".to_owned());

        cache.record_probe(failed, probe_completed_at);

        assert!(matches!(
            cache.lookup("test-catalog", probe_completed_at),
            GpuRouteCatalogLookup::Backoff(Some(message))
                if message == "slow verified provider probe failed"
        ));
        assert!(matches!(
            cache.lookup(
                "test-catalog",
                probe_completed_at + GPU_PROVIDER_PROBE_BACKOFF - Duration::from_millis(1),
            ),
            GpuRouteCatalogLookup::Backoff(_)
        ));
        assert!(matches!(
            cache.lookup(
                "test-catalog",
                probe_completed_at + GPU_PROVIDER_PROBE_BACKOFF,
            ),
            GpuRouteCatalogLookup::Probe
        ));
    }

    #[test]
    fn mixed_gpu_probe_failure_retries_without_discarding_healthy_routes() {
        let first_probe_completed_at = Instant::now();
        let mut cache = GpuRouteCatalogCache::default();
        let mut cuda_failed_vulkan_succeeded = verified_gpu_catalog(vec![verified_gpu_route(
            BackendKind::Vulkan,
            "native:pci:0000:01:00.0",
            "windows-display:32.0.16.1088",
            'a',
        )]);
        cuda_failed_vulkan_succeeded.provider_probe_incomplete = true;
        cuda_failed_vulkan_succeeded.diagnostic =
            Some("CUDA provider probe was rejected".to_owned());

        cache.record_probe(
            cuda_failed_vulkan_succeeded.clone(),
            first_probe_completed_at,
        );
        assert!(matches!(
            cache.lookup("test-catalog", first_probe_completed_at),
            GpuRouteCatalogLookup::Successful(catalog)
                if catalog.routes.len() == 1
                    && catalog.routes[0].provider == WorkerProvider::Vulkan
        ));
        assert!(matches!(
            cache.lookup("changed-catalog", first_probe_completed_at),
            GpuRouteCatalogLookup::Probe
        ));

        let explicit_retry_at = first_probe_completed_at + Duration::from_secs(1);
        assert!(cache.grant_explicit_probe_retry("test-catalog", explicit_retry_at));
        assert!(!cache.grant_explicit_probe_retry("test-catalog", explicit_retry_at));
        assert!(matches!(
            cache.lookup("test-catalog", explicit_retry_at),
            GpuRouteCatalogLookup::Probe
        ));

        let retry_completed_at = explicit_retry_at + Duration::from_secs(20);
        cache.record_probe(cuda_failed_vulkan_succeeded, retry_completed_at);
        assert!(matches!(
            cache.lookup(
                "test-catalog",
                retry_completed_at + GPU_PROVIDER_PROBE_BACKOFF - Duration::from_millis(1),
            ),
            GpuRouteCatalogLookup::Successful(catalog)
                if catalog.routes.len() == 1
                    && catalog.routes[0].provider == WorkerProvider::Vulkan
        ));
        assert!(matches!(
            cache.lookup(
                "test-catalog",
                retry_completed_at + GPU_PROVIDER_PROBE_BACKOFF,
            ),
            GpuRouteCatalogLookup::Probe
        ));

        let recovered = verified_gpu_catalog(vec![
            verified_gpu_route(
                BackendKind::Cuda,
                "native:pci:0000:01:00.0",
                "windows-display:32.0.16.1088",
                'b',
            ),
            verified_gpu_route(
                BackendKind::Vulkan,
                "native:pci:0000:01:00.0",
                "windows-display:32.0.16.1088",
                'a',
            ),
        ]);
        cache.record_probe(recovered, retry_completed_at + GPU_PROVIDER_PROBE_BACKOFF);
        assert!(matches!(
            cache.lookup(
                "test-catalog",
                retry_completed_at + GPU_PROVIDER_PROBE_BACKOFF * 2,
            ),
            GpuRouteCatalogLookup::Successful(catalog) if catalog.routes.len() == 2
        ));
    }

    #[test]
    fn provider_discovery_uses_one_budget_and_retires_a_hung_probe() {
        let (hello_started_tx, hello_started_rx) = channel();
        let (kill_started_tx, kill_started_rx) = channel();
        let (reaped_tx, reaped_rx) = channel();
        let first_launcher = Arc::new(
            TestLauncher::new([TestMode::BlockedHello {
                started: hello_started_tx,
            }])
            .with_process_events(kill_started_tx, reaped_tx),
        );
        let (second_started_tx, _second_started_rx) = channel();
        let second_launcher = Arc::new(TestLauncher::new([TestMode::BlockedHello {
            started: second_started_tx,
        }]));
        let supervisor = |launcher: Arc<TestLauncher>| InferenceWorkerSupervisor {
            transport: ProcessWorkerSupervisor::unstarted_with_launcher_and_deadlines(
                launcher,
                short_deadlines(),
            ),
            next_correlation: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        };
        let probes = vec![
            PendingPackProbe {
                pack_id: crate::gpu_worker_pack::manifest::StoreComponent::new(
                    "scribe-cuda-windows-x64",
                )
                .unwrap(),
                backend: PackBackend::Cuda,
                supervisor: supervisor(Arc::clone(&first_launcher)),
            },
            PendingPackProbe {
                pack_id: crate::gpu_worker_pack::manifest::StoreComponent::new(
                    "scribe-vulkan-windows-x64",
                )
                .unwrap(),
                backend: PackBackend::Vulkan,
                supervisor: supervisor(Arc::clone(&second_launcher)),
            },
        ];
        let budget = Duration::from_millis(80);
        let started_at = Instant::now();

        let registry = discover_pack_launch_bindings_with_budget(probes, Vec::new(), budget);

        let elapsed = started_at.elapsed();
        assert!(elapsed >= budget);
        assert!(
            elapsed < Duration::from_millis(500),
            "aggregate discovery exceeded its bounded cleanup allowance: {elapsed:?}"
        );
        hello_started_rx.try_recv().unwrap();
        kill_started_rx
            .recv_timeout(Duration::from_millis(100))
            .unwrap();
        reaped_rx.recv_timeout(Duration::from_millis(100)).unwrap();
        assert_eq!(first_launcher.launches.load(Ordering::Acquire), 1);
        assert_eq!(second_launcher.launches.load(Ordering::Acquire), 0);
        let (bindings, diagnostics) = registry.into_parts();
        assert!(bindings.is_empty());
        assert_eq!(
            diagnostics
                .iter()
                .filter(|diagnostic| {
                    diagnostic.issue
                        == crate::gpu_worker_pack::PackDiscoveryIssue::ProviderProbeRejected
                })
                .count(),
            2,
            "the timed-out active probe and skipped remaining probe are both diagnosed"
        );
    }

    #[test]
    fn active_route_ownership_retires_cpu_gpu_and_device_switches() {
        let cpu_launcher = Arc::new(TestLauncher::new([TestMode::Normal, TestMode::Normal]));
        let gpu_one_launcher = Arc::new(TestLauncher::new([TestMode::Normal]));
        let gpu_two_launcher = Arc::new(TestLauncher::new([TestMode::Normal]));
        let route = |provider: WorkerProvider,
                     backend: BackendKind,
                     identity: &str,
                     launcher: Arc<TestLauncher>| InferenceWorkerRoute {
            provider,
            supervisor: InferenceWorkerSupervisor {
                transport: ProcessWorkerSupervisor::unstarted_with_launcher_and_deadlines(
                    launcher,
                    short_deadlines(),
                ),
                next_correlation: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            },
            target: Some(backend_target(backend, identity, Some(0))),
            health_key: None,
            consume_retry_bypass: false,
            auto_evidence_id: None,
        };
        let cpu = route(
            WorkerProvider::Cpu,
            BackendKind::Cpu,
            "cpu",
            Arc::clone(&cpu_launcher),
        );
        let gpu_one = route(
            WorkerProvider::Vulkan,
            BackendKind::Vulkan,
            "native:pci:0000:01:00.0",
            Arc::clone(&gpu_one_launcher),
        );
        let gpu_two = route(
            WorkerProvider::Vulkan,
            BackendKind::Vulkan,
            "native:pci:0000:02:00.0",
            Arc::clone(&gpu_two_launcher),
        );
        let registry = InferenceWorkerRegistry::with_cpu_supervisor(cpu.supervisor.clone());

        registry.prepare_route(&cpu).unwrap();
        let cpu_generation = cpu.supervisor.transport.ensure_generation().unwrap();
        registry.prepare_route(&gpu_one).unwrap();
        assert_eq!(cpu.supervisor.transport.current_generation().unwrap(), None);
        let gpu_generation = gpu_one.supervisor.transport.ensure_generation().unwrap();

        registry.prepare_route(&gpu_one).unwrap();
        assert_eq!(
            gpu_one.supervisor.transport.ensure_generation().unwrap(),
            gpu_generation,
            "the same winning route keeps its warm worker during the idle window"
        );
        assert_eq!(gpu_one_launcher.launches.load(Ordering::Acquire), 1);

        registry.prepare_route(&gpu_two).unwrap();
        assert_eq!(
            gpu_one.supervisor.transport.current_generation().unwrap(),
            None
        );
        gpu_two.supervisor.transport.ensure_generation().unwrap();

        registry.prepare_route(&cpu).unwrap();
        assert_eq!(
            gpu_two.supervisor.transport.current_generation().unwrap(),
            None
        );
        assert_ne!(
            cpu.supervisor.transport.ensure_generation().unwrap(),
            cpu_generation,
            "switching back to CPU starts a new sole owner"
        );
        assert_eq!(cpu_launcher.launches.load(Ordering::Acquire), 2);
        registry.shutdown().unwrap();
    }

    #[test]
    fn registry_fallback_is_bounded_to_registered_pre_output_failures() {
        let first = Arc::new(TestLauncher::new([TestMode::CapabilityMismatch(
            CapabilityMismatch::Challenge,
        )]));
        let second = Arc::new(TestLauncher::new([TestMode::CapabilityMismatch(
            CapabilityMismatch::Abi,
        )]));
        let route = |launcher: Arc<TestLauncher>| InferenceWorkerRoute {
            provider: WorkerProvider::Cpu,
            supervisor: InferenceWorkerSupervisor {
                transport: ProcessWorkerSupervisor::unstarted_with_launcher_and_deadlines(
                    launcher,
                    short_deadlines(),
                ),
                next_correlation: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            },
            target: Some(BackendTarget::cpu()),
            health_key: None,
            consume_retry_bypass: false,
            auto_evidence_id: None,
        };
        let registry = InferenceWorkerRegistry {
            routes: Arc::new(vec![route(first.clone()), route(second.clone())]),
            gpu_routes: Arc::new(Mutex::new(GpuRouteCatalogCache::default())),
            auto_gpu_routes: Arc::new(Mutex::new(GpuRouteCatalogCache::default())),
            gpu_health_path: None,
            active_route: Arc::new(Mutex::new(None)),
            route_execution: Arc::new(Mutex::new(())),
            gpu_routes_for_testing: None,
        };
        assert!(
            registry
                .load(missing_gguf_artifact(), AccelerationPreference::Cpu)
                .is_err()
        );
        assert_eq!(first.launches.load(Ordering::Acquire), 1);
        assert_eq!(second.launches.load(Ordering::Acquire), 1);
        assert_eq!(
            registry.routes[0]
                .supervisor
                .transport
                .current_generation()
                .unwrap(),
            None
        );
        assert_eq!(
            registry.routes[1]
                .supervisor
                .transport
                .current_generation()
                .unwrap(),
            None
        );
    }

    #[test]
    fn explicit_gpu_fallback_advances_to_the_next_device_without_cpu() {
        let cpu = Arc::new(TestLauncher::new([]));
        let first = Arc::new(TestLauncher::new([TestMode::CapabilityMismatch(
            CapabilityMismatch::Challenge,
        )]));
        let second = Arc::new(TestLauncher::new([TestMode::Normal]));
        let route = |launcher: Arc<TestLauncher>, identity: &str| InferenceWorkerRoute {
            provider: WorkerProvider::Vulkan,
            supervisor: InferenceWorkerSupervisor {
                transport: ProcessWorkerSupervisor::unstarted_with_launcher_and_deadlines(
                    launcher,
                    short_deadlines(),
                ),
                next_correlation: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            },
            target: Some(backend_target(BackendKind::Vulkan, identity, Some(0))),
            health_key: None,
            consume_retry_bypass: false,
            auto_evidence_id: None,
        };
        let gpu_routes = Arc::new(vec![
            route(first.clone(), "native:pci:0000:01:00.0"),
            route(second.clone(), "native:pci:0000:02:00.0"),
        ]);
        let registry = InferenceWorkerRegistry {
            routes: Arc::new(vec![InferenceWorkerRoute {
                provider: WorkerProvider::Cpu,
                supervisor: InferenceWorkerSupervisor {
                    transport: ProcessWorkerSupervisor::unstarted_with_launcher_and_deadlines(
                        cpu.clone(),
                        short_deadlines(),
                    ),
                    next_correlation: Arc::new(std::sync::atomic::AtomicU64::new(0)),
                },
                target: Some(BackendTarget::cpu()),
                health_key: None,
                consume_retry_bypass: false,
                auto_evidence_id: None,
            }]),
            gpu_routes: Arc::new(Mutex::new(GpuRouteCatalogCache::default())),
            auto_gpu_routes: Arc::new(Mutex::new(GpuRouteCatalogCache::default())),
            gpu_health_path: None,
            active_route: Arc::new(Mutex::new(None)),
            route_execution: Arc::new(Mutex::new(())),
            gpu_routes_for_testing: Some(VerifiedGpuRouteCatalog {
                device_set_digest: verified_gpu_device_set_digest(&gpu_routes),
                routes: gpu_routes,
                diagnostic: None,
                discovery_fingerprint: "test-catalog".to_owned(),
                provider_probe_incomplete: false,
                skipped_targets: Vec::new(),
            }),
        };
        assert!(
            registry
                .load(missing_gguf_artifact(), AccelerationPreference::Gpu)
                .is_err()
        );
        assert_eq!(first.launches.load(Ordering::Acquire), 1);
        assert_eq!(second.launches.load(Ordering::Acquire), 1);
        assert_eq!(cpu.launches.load(Ordering::Acquire), 0);
        for route in registry
            .gpu_routes_for_testing
            .as_ref()
            .unwrap()
            .routes
            .iter()
        {
            assert_eq!(
                route.supervisor.transport.current_generation().unwrap(),
                None,
                "every failed fallback route is retired"
            );
        }
    }

    #[test]
    fn stale_batch_cancellation_is_rejected_before_worker_launch() {
        let root = test_root("stale-batch-cancellation");
        let launcher = Arc::new(TestLauncher::new([]));
        let supervisor = InferenceWorkerSupervisor {
            transport: ProcessWorkerSupervisor::unstarted_with_launcher_and_deadlines(
                launcher.clone(),
                SupervisorDeadlines::default(),
            ),
            next_correlation: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        };
        let artifact = RuntimeArtifact::OnnxBundle(spec_with_roles(
            &root,
            OnnxModelFamily::NemoCtc,
            &[OnnxFileRole::Model, OnnxFileRole::Tokens],
        ));
        let audio =
            PreparedAudio::from_captured_mono(vec![0.1], PREPARED_SAMPLE_RATE, 1, 1).unwrap();
        let cancellation = std::sync::atomic::AtomicU64::new(2);
        assert!(matches!(
            supervisor.transcribe(
                artifact,
                AccelerationPreference::Cpu,
                &audio,
                TranscriptionOptions::default(),
                1,
                &cancellation,
            ),
            Err(RuntimeError::Cancelled(_))
        ));
        assert_eq!(launcher.launches.load(Ordering::Acquire), 0);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn cancellation_between_batch_requests_retires_the_current_generation() {
        let launcher = Arc::new(TestLauncher::new([TestMode::Normal]));
        let transport = ProcessWorkerSupervisor::unstarted_with_launcher_and_deadlines(
            launcher,
            SupervisorDeadlines::default(),
        );
        transport.ensure_generation().unwrap();
        let supervisor = InferenceWorkerSupervisor {
            transport: transport.clone(),
            next_correlation: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        };
        supervisor.cancel_active();
        assert_eq!(transport.current_generation().unwrap(), None);
    }

    fn cooperative_cancel_fixture(
        acknowledge: bool,
    ) -> (
        ProcessWorkerSupervisor,
        Arc<CooperativeCancelProcess>,
        TestReceiver<PendingResult>,
    ) {
        let correlation = Correlation {
            generation: 1,
            session_id: 7,
            request_id: 9,
        };
        let supervisor = ProcessWorkerSupervisor::unstarted_with_launcher_and_deadlines(
            Arc::new(TestLauncher::new([])),
            SupervisorDeadlines {
                cancel: Duration::from_millis(20),
                ..SupervisorDeadlines::default()
            },
        );
        let process = Arc::new(CooperativeCancelProcess::new(acknowledge, correlation));
        *process.inner.lock().unwrap() = Some(Arc::downgrade(&supervisor.inner));
        let (reply, response) = sync_channel(1);
        supervisor
            .inner
            .pending
            .lock()
            .unwrap()
            .insert(correlation, reply);
        let process_trait: Arc<dyn WorkerProcess> = process.clone();
        let mut state = supervisor.inner.state.lock().unwrap();
        state.current = Some(CurrentGeneration {
            generation: correlation.generation,
            process: process_trait,
            expectation: expected_worker(WorkerRole::Inference),
            pack_launch: None,
            pack_bindings: Vec::new(),
        });
        state.active_request = Some(correlation);
        drop(state);
        (supervisor, process, response)
    }

    #[test]
    fn cooperative_cancellation_precedes_hard_fallback_and_preserves_generation() {
        let (supervisor, process, response) = cooperative_cancel_fixture(true);
        assert_eq!(
            supervisor.cancel_active_outcome().unwrap(),
            CancelOutcome::CooperativeSettled
        );
        assert_eq!(process.cooperative_requests.load(Ordering::Acquire), 1);
        assert!(!process.terminated.load(Ordering::Acquire));
        assert_eq!(supervisor.current_generation().unwrap(), Some(1));
        assert!(response.recv().unwrap().is_err());
    }

    #[test]
    fn cooperative_cancellation_timeout_hard_invalidates_generation() {
        let (supervisor, process, response) = cooperative_cancel_fixture(false);
        assert_eq!(
            supervisor.cancel_active_outcome().unwrap(),
            CancelOutcome::HardInvalidated
        );
        assert_eq!(process.cooperative_requests.load(Ordering::Acquire), 1);
        assert!(process.terminated.load(Ordering::Acquire));
        assert_eq!(supervisor.current_generation().unwrap(), None);
        assert!(response.recv().unwrap().is_err());
    }

    #[test]
    fn unified_worker_routes_exact_moonshine_bundle_in_one_child() {
        let root = test_root("unified-chunked-batch");
        let mut spec = spec_with_roles(
            &root,
            OnnxModelFamily::Moonshine,
            &[
                OnnxFileRole::Encoder,
                OnnxFileRole::MergedDecoder,
                OnnxFileRole::Tokens,
            ],
        );
        spec.id = "moonshine-tiny-en-int8-onnx".to_owned();
        let artifact = WireRuntimeArtifact::OnnxBundle(spec);
        let mut input = Vec::new();
        append_control(&mut input, 0, 0, test_hello(WorkerRole::Inference));
        append_control(
            &mut input,
            7,
            1,
            Control::LoadRuntime {
                artifact: artifact.clone(),
                preference: AccelerationPreference::Cpu,
            },
        );
        append_control(
            &mut input,
            7,
            2,
            Control::BeginBatch {
                artifact: artifact.clone(),
                preference: AccelerationPreference::Cpu,
                options: TranscriptionOptions::default(),
                source_sample_rate: PREPARED_SAMPLE_RATE,
                source_channels: 1,
                source_frames: 4,
                declared_samples: 4,
            },
        );
        append_control(&mut input, 7, 3, Control::AudioChunk);
        append_pcm(&mut input, 7, 3, &[0.1, 0.2]);
        append_control(&mut input, 7, 4, Control::AudioChunk);
        append_pcm(&mut input, 7, 4, &[-0.1, -0.2]);
        append_control(&mut input, 7, 5, Control::EndBatch);
        append_control(&mut input, 7, 6, Control::Shutdown);

        let recognizers = FakeRecognizerFactory::new();
        let vad = FakeVadFactory::new();
        let mut output = Vec::new();
        worker_loop_with_factories(
            Cursor::new(input),
            &mut output,
            &recognizers,
            &vad,
            Some(WorkerRole::Inference),
            None,
        )
        .unwrap();
        let mut output = Cursor::new(output);
        let mut responses = Vec::new();
        while output.position() < output.get_ref().len() as u64 {
            responses.push(
                parse_worker_control(read_frame(&mut output).unwrap())
                    .unwrap()
                    .2,
            );
        }
        assert!(matches!(responses[0], Control::Ready { .. }));
        assert!(matches!(responses[1], Control::RuntimeLoaded { .. }));
        assert!(matches!(responses[2], Control::Ok));
        assert!(matches!(responses[3], Control::Ok));
        assert!(matches!(responses[4], Control::Ok));
        match &responses[5] {
            Control::RuntimeTranscript { execution } => {
                assert_eq!(
                    execution.transcript.text,
                    "batch:moonshine-tiny-en-int8-onnx:1"
                );
            }
            other => panic!("expected runtime transcript, got {other:?}"),
        }
        assert!(matches!(responses[6], Control::Ok));
        assert_eq!(recognizers.snapshot().transcriptions, 1);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn one_inference_child_routes_gguf_then_onnx_without_nested_worker() {
        let root = test_root("unified-gguf-then-onnx");
        let missing_gguf = root.join("missing.gguf");
        let onnx = WireRuntimeArtifact::OnnxBundle(spec_with_roles(
            &root,
            OnnxModelFamily::NemoCtc,
            &[OnnxFileRole::Model, OnnxFileRole::Tokens],
        ));
        let mut input = Vec::new();
        append_control(&mut input, 0, 0, test_hello(WorkerRole::Inference));
        append_control(
            &mut input,
            1,
            1,
            Control::LoadRuntime {
                artifact: WireRuntimeArtifact::Gguf(WireRuntimeModel {
                    id: "missing-gguf".to_owned(),
                    path: missing_gguf,
                    format: WireArtifactFormat::Gguf,
                    expected_size_bytes: 1,
                    expected_sha256: "0".repeat(64),
                }),
                preference: AccelerationPreference::Cpu,
            },
        );
        append_control(
            &mut input,
            2,
            2,
            Control::BeginBatch {
                artifact: onnx,
                preference: AccelerationPreference::Cpu,
                options: TranscriptionOptions::default(),
                source_sample_rate: PREPARED_SAMPLE_RATE,
                source_channels: 1,
                source_frames: 1,
                declared_samples: 1,
            },
        );
        append_control(&mut input, 2, 3, Control::AudioChunk);
        append_pcm(&mut input, 2, 3, &[0.1]);
        append_control(&mut input, 2, 4, Control::EndBatch);
        append_control(&mut input, 0, 5, Control::Shutdown);

        let factory = FakeRecognizerFactory::new();
        let responses = run_framed_fake_worker(&factory, input);
        assert!(matches!(responses[0].2, Control::Ready { .. }));
        assert!(matches!(
            responses[1].2,
            Control::RuntimeFailed { .. } | Control::Error { .. }
        ));
        assert!(matches!(responses[2].2, Control::Ok));
        assert!(matches!(responses[3].2, Control::Ok));
        assert!(matches!(responses[4].2, Control::RuntimeTranscript { .. }));
        assert!(matches!(responses[5].2, Control::Ok));
        assert_eq!(factory.snapshot().recognizers_created, 1);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn runtime_batch_rejects_cross_session_chunks_and_count_mismatch() {
        let root = test_root("runtime-batch-ownership");
        let artifact = WireRuntimeArtifact::OnnxBundle(spec_with_roles(
            &root,
            OnnxModelFamily::NemoCtc,
            &[OnnxFileRole::Model, OnnxFileRole::Tokens],
        ));
        let mut input = Vec::new();
        append_control(&mut input, 0, 0, test_hello(WorkerRole::Inference));
        append_control(
            &mut input,
            7,
            1,
            Control::BeginBatch {
                artifact: artifact.clone(),
                preference: AccelerationPreference::Cpu,
                options: TranscriptionOptions::default(),
                source_sample_rate: PREPARED_SAMPLE_RATE,
                source_channels: 1,
                source_frames: 2,
                declared_samples: 2,
            },
        );
        append_control(&mut input, 8, 2, Control::AudioChunk);
        append_pcm(&mut input, 8, 2, &[0.1]);
        append_control(
            &mut input,
            7,
            3,
            Control::BeginBatch {
                artifact: artifact.clone(),
                preference: AccelerationPreference::Cpu,
                options: TranscriptionOptions::default(),
                source_sample_rate: PREPARED_SAMPLE_RATE,
                source_channels: 1,
                source_frames: 2,
                declared_samples: 2,
            },
        );
        append_control(&mut input, 7, 4, Control::AudioChunk);
        append_pcm(&mut input, 7, 4, &[0.1]);
        append_control(&mut input, 7, 5, Control::EndBatch);
        append_control(
            &mut input,
            7,
            6,
            Control::BeginBatch {
                artifact,
                preference: AccelerationPreference::Cpu,
                options: TranscriptionOptions::default(),
                source_sample_rate: PREPARED_SAMPLE_RATE,
                source_channels: 1,
                source_frames: 1,
                declared_samples: 1,
            },
        );
        append_control(&mut input, 7, 7, Control::AudioChunk);
        append_pcm(&mut input, 7, 7, &[0.2]);
        append_control(&mut input, 7, 8, Control::EndBatch);
        append_control(&mut input, 0, 9, Control::Shutdown);

        let responses = run_framed_fake_worker(&FakeRecognizerFactory::new(), input);
        assert_error(&responses[2], "different session");
        assert!(matches!(responses[3].2, Control::Ok));
        assert!(matches!(responses[4].2, Control::Ok));
        assert_error(&responses[5], "sample count mismatch");
        assert!(matches!(responses[6].2, Control::Ok));
        assert!(matches!(responses[7].2, Control::Ok));
        assert!(matches!(responses[8].2, Control::RuntimeTranscript { .. }));
        assert!(matches!(responses[9].2, Control::Ok));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn onnx_batch_decode_failure_discards_warm_recognizer() {
        let root = test_root("onnx-batch-decode-failure");
        let mut spec = spec_with_roles(
            &root,
            OnnxModelFamily::NemoCtc,
            &[OnnxFileRole::Model, OnnxFileRole::Tokens],
        );
        spec.id = "decode-failure-model".to_owned();
        let artifact = WireRuntimeArtifact::OnnxBundle(spec);
        let mut input = Vec::new();
        append_control(&mut input, 0, 0, test_hello(WorkerRole::Inference));
        for (session, request) in [(7, 1), (8, 4)] {
            append_control(
                &mut input,
                session,
                request,
                Control::BeginBatch {
                    artifact: artifact.clone(),
                    preference: AccelerationPreference::Cpu,
                    options: TranscriptionOptions::default(),
                    source_sample_rate: PREPARED_SAMPLE_RATE,
                    source_channels: 1,
                    source_frames: 1,
                    declared_samples: 1,
                },
            );
            append_control(&mut input, session, request + 1, Control::AudioChunk);
            append_pcm(&mut input, session, request + 1, &[0.1]);
            append_control(&mut input, session, request + 2, Control::EndBatch);
        }
        append_control(&mut input, 0, 7, Control::Shutdown);

        let factory = FakeRecognizerFactory::new();
        let responses = run_framed_fake_worker(&factory, input);
        assert_error(&responses[3], "decode failed");
        assert_error(&responses[6], "decode failed");
        assert_eq!(factory.snapshot().recognizers_created, 2);
        assert_eq!(factory.snapshot().recognizer_drops, 2);
        std::fs::remove_dir_all(root).unwrap();
    }

    fn run_framed_fake_worker(
        factory: &FakeRecognizerFactory,
        input: Vec<u8>,
    ) -> Vec<(u64, u64, Control)> {
        let mut output = Vec::new();
        worker_loop_with_factory(Cursor::new(input), &mut output, factory).unwrap();
        let mut output = Cursor::new(output);
        let mut responses = Vec::new();
        while output.position() < output.get_ref().len() as u64 {
            responses.push(parse_worker_control(read_frame(&mut output).unwrap()).unwrap());
        }
        responses
    }

    fn run_framed_fake_vad_worker(
        recognizer_factory: &FakeRecognizerFactory,
        vad_factory: &FakeVadFactory,
        input: Vec<u8>,
    ) -> Vec<(u64, u64, Control)> {
        let mut output = Vec::new();
        worker_loop_with_factories(
            Cursor::new(input),
            &mut output,
            recognizer_factory,
            vad_factory,
            None,
            None,
        )
        .unwrap();
        let mut output = Cursor::new(output);
        let mut responses = Vec::new();
        while output.position() < output.get_ref().len() as u64 {
            responses.push(parse_worker_control(read_frame(&mut output).unwrap()).unwrap());
        }
        responses
    }

    fn assert_error(response: &(u64, u64, Control), text: &str) {
        match &response.2 {
            Control::Error { message } => assert!(
                message.contains(text),
                "expected error containing {text:?}, got {message:?}"
            ),
            Control::RuntimeFailed { error } => assert!(
                error.message.contains(text),
                "expected error containing {text:?}, got {:?}",
                error.message
            ),
            other => panic!("expected worker error, got {other:?}"),
        }
    }

    fn required_fixture_path(name: &str) -> PathBuf {
        PathBuf::from(std::env::var(name).unwrap_or_else(|_| {
            panic!("set {name} to the reviewed local fixture before running this ignored test")
        }))
    }

    fn run_native_worker(input: Vec<u8>) -> Vec<(u64, u64, Control)> {
        let mut output = Vec::new();
        worker_loop(Cursor::new(input), &mut output).unwrap();
        let mut output = Cursor::new(output);
        let mut responses = Vec::new();
        while output.position() < output.get_ref().len() as u64 {
            responses.push(parse_worker_control(read_frame(&mut output).unwrap()).unwrap());
        }
        responses
    }

    #[test]
    fn framed_vad_worker_applies_threshold_and_resets_recurrent_state() {
        let recognizer_factory = FakeRecognizerFactory::new();
        let vad_factory = FakeVadFactory::new();
        let window = [0.1; WINDOW_SAMPLES];
        let mut input = Vec::new();
        append_control(&mut input, 0, 1, test_hello(WorkerRole::Vad));
        append_control(&mut input, 0, 2, Control::LoadVad { num_threads: 1 });
        append_control(&mut input, 41, 3, Control::StartVad { threshold: 0.5 });
        append_control(&mut input, 41, 4, Control::VadWindow);
        append_pcm(&mut input, 41, 4, &window);
        append_control(&mut input, 41, 5, Control::VadWindow);
        append_pcm(&mut input, 41, 5, &window);
        append_control(&mut input, 41, 6, Control::ResetVad);
        append_control(&mut input, 41, 7, Control::VadWindow);
        append_pcm(&mut input, 41, 7, &window);
        append_control(&mut input, 41, 8, Control::EndVad);
        append_control(&mut input, 42, 9, Control::StartVad { threshold: 0.3 });
        append_control(&mut input, 42, 10, Control::VadWindow);
        append_pcm(&mut input, 42, 10, &window);
        append_control(&mut input, 42, 11, Control::EndVad);
        append_control(&mut input, 0, 12, Control::Shutdown);

        let responses = run_framed_fake_vad_worker(&recognizer_factory, &vad_factory, input);
        assert_eq!(responses.len(), 12);
        for (index, probability, speech) in [
            (3, 0.4, false),
            (4, 0.7, true),
            (6, 0.4, false),
            (9, 0.4, true),
        ] {
            match responses[index].2 {
                Control::VadDecision {
                    probability: actual,
                    speech: actual_speech,
                } => {
                    assert_eq!(actual, probability);
                    assert_eq!(actual_speech, speech);
                }
                ref other => panic!("expected VAD decision, got {other:?}"),
            }
        }
        let state = vad_factory.snapshot();
        assert_eq!(state.creates, 1);
        assert_eq!(state.computes, 4);
        assert_eq!(state.resets, 5);
        assert_eq!(state.drops, 1);
        assert_eq!(recognizer_factory.snapshot().create_attempts, 0);
    }

    #[test]
    fn framed_vad_worker_rejects_invalid_controls_and_loses_failed_session() {
        let recognizer_factory = FakeRecognizerFactory::new();
        let vad_factory = FakeVadFactory::new();
        let mut input = Vec::new();
        append_control(&mut input, 0, 1, test_hello(WorkerRole::Vad));
        append_control(&mut input, 0, 2, Control::LoadVad { num_threads: 0 });
        append_control(&mut input, 0, 3, Control::LoadVad { num_threads: 1 });
        append_control(&mut input, 51, 4, Control::StartVad { threshold: 0.19 });
        append_control(&mut input, 51, 5, Control::StartVad { threshold: 0.5 });
        append_control(&mut input, 51, 6, Control::VadWindow);
        append_pcm(&mut input, 51, 6, &[0.0; WINDOW_SAMPLES - 1]);
        append_control(&mut input, 51, 7, Control::VadWindow);
        append_pcm(&mut input, 51, 7, &[0.0; WINDOW_SAMPLES]);
        append_control(&mut input, 0, 8, Control::Shutdown);

        let responses = run_framed_fake_vad_worker(&recognizer_factory, &vad_factory, input);
        assert_error(&responses[1], "thread count");
        assert_error(&responses[3], "threshold");
        assert_error(&responses[5], "exactly 512");
        assert_error(&responses[6], "no Silero VAD session is active");
        let state = vad_factory.snapshot();
        assert_eq!(state.creates, 1);
        assert_eq!(state.computes, 0);
        assert_eq!(state.resets, 2);
    }

    #[test]
    fn real_native_vad_runs_through_worker_protocol_and_resets_exactly() {
        let window = (0..WINDOW_SAMPLES)
            .map(|index| ((index as f32 * 0.071).sin() * 0.25).clamp(-1.0, 1.0))
            .collect::<Vec<_>>();
        let mut input = Vec::new();
        append_control(&mut input, 0, 1, test_hello(WorkerRole::Vad));
        append_control(&mut input, 0, 2, Control::LoadVad { num_threads: 1 });
        append_control(&mut input, 61, 3, Control::StartVad { threshold: 0.2 });
        append_control(&mut input, 61, 4, Control::VadWindow);
        append_pcm(&mut input, 61, 4, &window);
        append_control(&mut input, 61, 5, Control::VadWindow);
        append_pcm(&mut input, 61, 5, &window);
        append_control(&mut input, 61, 6, Control::ResetVad);
        append_control(&mut input, 61, 7, Control::VadWindow);
        append_pcm(&mut input, 61, 7, &window);
        append_control(&mut input, 61, 8, Control::EndVad);
        append_control(&mut input, 0, 9, Control::Shutdown);

        let responses = run_native_worker(input);
        let probability = |index: usize| match responses[index].2 {
            Control::VadDecision { probability, .. } => probability,
            ref other => panic!("expected VAD decision, got {other:?}"),
        };
        let first = probability(3);
        let second = probability(4);
        let after_reset = probability(6);
        println!(
            "worker_first_probability={first:.9} worker_second_probability={second:.9} worker_after_reset_probability={after_reset:.9}"
        );
        assert!(first.is_finite() && second.is_finite());
        assert_eq!(after_reset, first);
    }

    #[test]
    #[ignore = "requires SCRIBE_INFERENCE_WORKER_EXE to name the built dedicated CPU worker; runs SCIF v5 without downloading"]
    fn hidden_inference_worker_manual_protocol_smoke() {
        let executable = required_fixture_path("SCRIBE_INFERENCE_WORKER_EXE");
        let SpawnedWorker {
            mut stdin,
            mut stdout,
            process,
            ..
        } = OsWorkerLauncher::for_executable(WorkerRole::Inference, executable)
            .launch()
            .expect("spawn hidden inference worker executable");
        for (request_id, control, expected) in [
            (1, test_hello(WorkerRole::Inference), "ready"),
            (2, Control::Health, "ok"),
            (3, Control::Shutdown, "ok"),
        ] {
            write_frame(&mut stdin, &control_frame(0, request_id, &control).unwrap()).unwrap();
            let (_, received_request, response) =
                parse_worker_control(read_frame(&mut stdout).unwrap()).unwrap();
            assert_eq!(received_request, request_id);
            match expected {
                "ready" => assert!(matches!(response, Control::Ready { .. })),
                "ok" => assert!(matches!(response, Control::Ok)),
                _ => unreachable!(),
            }
        }
        drop(stdin);
        process.wait().unwrap();
    }

    #[test]
    #[ignore = "requires SCRIBE_DESKTOP_EXE to name the built desktop executable; runs the same-executable VAD protocol without downloading"]
    fn hidden_vad_worker_manual_protocol_smoke() {
        let executable = required_fixture_path("SCRIBE_DESKTOP_EXE");
        let SpawnedWorker {
            mut stdin,
            mut stdout,
            process,
            ..
        } = OsWorkerLauncher::for_executable(WorkerRole::Vad, executable)
            .launch()
            .expect("spawn hidden VAD worker executable");
        for (request_id, control, expected) in [
            (1, test_hello(WorkerRole::Vad), "ready"),
            (2, Control::Health, "ok"),
            (3, Control::LoadVad { num_threads: 1 }, "ok"),
            (4, Control::StartVad { threshold: 0.5 }, "ok"),
        ] {
            let session_id = if request_id >= 4 { 71 } else { 0 };
            write_frame(
                &mut stdin,
                &control_frame(session_id, request_id, &control).unwrap(),
            )
            .unwrap();
            let (_, received_request, response) =
                parse_worker_control(read_frame(&mut stdout).unwrap()).unwrap();
            assert_eq!(received_request, request_id);
            match expected {
                "ready" => assert!(matches!(response, Control::Ready { .. })),
                "ok" => assert!(matches!(response, Control::Ok)),
                _ => unreachable!(),
            }
        }
        let window = (0..WINDOW_SAMPLES)
            .map(|index| ((index as f32 * 0.071).sin() * 0.25).clamp(-1.0, 1.0))
            .collect::<Vec<_>>();
        let mut probabilities = Vec::new();
        for request_id in [5, 7] {
            write_frame(
                &mut stdin,
                &control_frame(71, request_id, &Control::VadWindow).unwrap(),
            )
            .unwrap();
            write_frame(
                &mut stdin,
                &Frame {
                    kind: FrameKind::Pcm,
                    session_id: 71,
                    request_id,
                    body: encode_pcm(&window).unwrap(),
                },
            )
            .unwrap();
            let (_, received_request, response) =
                parse_worker_control(read_frame(&mut stdout).unwrap()).unwrap();
            assert_eq!(received_request, request_id);
            match response {
                Control::VadDecision { probability, .. } => probabilities.push(probability),
                other => panic!("expected native VAD decision, got {other:?}"),
            }
            if request_id == 5 {
                write_frame(
                    &mut stdin,
                    &control_frame(71, 6, &Control::ResetVad).unwrap(),
                )
                .unwrap();
                assert!(matches!(
                    parse_worker_control(read_frame(&mut stdout).unwrap())
                        .unwrap()
                        .2,
                    Control::Ok
                ));
            }
        }
        assert_eq!(probabilities[0], probabilities[1]);
        for (session_id, request_id, control) in
            [(71, 8, Control::EndVad), (0, 9, Control::Shutdown)]
        {
            write_frame(
                &mut stdin,
                &control_frame(session_id, request_id, &control).unwrap(),
            )
            .unwrap();
            assert!(matches!(
                parse_worker_control(read_frame(&mut stdout).unwrap())
                    .unwrap()
                    .2,
                Control::Ok
            ));
        }
        drop(stdin);
        process.wait().unwrap();
    }

    #[test]
    fn vad_and_transcription_supervisors_own_independent_transports() {
        let transcription_launcher = Arc::new(TestLauncher::new([TestMode::Normal]));
        let vad_launcher = Arc::new(TestLauncher::new([TestMode::VadNormal]));
        let transcription = test_supervisor(transcription_launcher.clone());
        let vad_transport = test_supervisor(vad_launcher.clone());
        assert!(!Arc::ptr_eq(&transcription.inner, &vad_transport.inner));
        let vad = SileroVadWorkerSupervisor::with_transport(vad_transport);

        transcription.health(1, 1).unwrap();
        vad.health(2, 1).unwrap();
        assert_eq!(transcription_launcher.launches.load(Ordering::Acquire), 1);
        assert_eq!(vad_launcher.launches.load(Ordering::Acquire), 1);
    }

    #[test]
    fn crashed_vad_window_loses_current_session_and_recovers_on_new_generation() {
        let launcher = Arc::new(TestLauncher::new([
            TestMode::VadCrashOnWindow,
            TestMode::VadNormal,
        ]));
        let transport = ProcessWorkerSupervisor::with_launcher_and_deadlines(
            launcher.clone(),
            short_deadlines(),
        )
        .unwrap();
        let vad = SileroVadWorkerSupervisor::with_transport(transport);
        let threshold = VadThreshold::new(0.5).unwrap();
        let window = [0.1; WINDOW_SAMPLES];

        vad.load(0, 1, 1).unwrap();
        vad.start_session(101, 2, threshold).unwrap();
        assert!(vad.compute(101, 3, &window).is_err());
        assert!(vad.compute(101, 4, &window).is_err());

        assert!(!vad.load(0, 5, 1).unwrap());
        vad.start_session(202, 6, threshold).unwrap();
        let decision = vad.compute(202, 7, &window).unwrap();
        assert_eq!(decision.probability, 0.4);
        assert!(!decision.speech);
        assert!(vad.compute(101, 8, &window).is_err());
        vad.end_session(202, 9).unwrap();
        assert_eq!(launcher.launches.load(Ordering::Acquire), 2);
    }

    fn assert_vad_control_crash_retires_generation(mode: TestMode, fail_end: bool) {
        let launcher = Arc::new(TestLauncher::new([mode, TestMode::VadNormal]));
        let transport = ProcessWorkerSupervisor::with_launcher_and_deadlines(
            launcher.clone(),
            short_deadlines(),
        )
        .unwrap();
        let vad = SileroVadWorkerSupervisor::with_transport(transport);
        let threshold = VadThreshold::new(0.5).unwrap();
        let window = [0.1; WINDOW_SAMPLES];

        vad.load(0, 1, 1).unwrap();
        vad.start_session(301, 2, threshold).unwrap();
        let failed = if fail_end {
            vad.end_session(301, 3)
        } else {
            vad.reset(301, 3)
        };
        assert!(failed.is_err());
        assert!(vad.compute(301, 4, &window).is_err());

        vad.load(0, 5, 1).unwrap();
        vad.start_session(302, 6, threshold).unwrap();
        assert_eq!(vad.compute(302, 7, &window).unwrap().probability, 0.4);
        vad.end_session(302, 8).unwrap();
        assert_eq!(launcher.launches.load(Ordering::Acquire), 2);
    }

    #[test]
    fn reset_transport_failure_retires_vad_generation() {
        assert_vad_control_crash_retires_generation(TestMode::VadCrashOnReset, false);
    }

    #[test]
    fn end_transport_failure_retires_vad_generation() {
        assert_vad_control_crash_retires_generation(TestMode::VadCrashOnEnd, true);
    }

    fn assert_blocked_vad_request_retires_reaps_and_recovers(operation: BlockedVadOperation) {
        let (started_tx, started_rx) = channel();
        let (kill_started_tx, kill_started_rx) = channel();
        let (reaped_tx, reaped_rx) = channel();
        let launcher = Arc::new(
            TestLauncher::new([
                TestMode::BlockedVad {
                    operation,
                    started: started_tx,
                },
                TestMode::VadNormal,
            ])
            .with_process_events(kill_started_tx, reaped_tx),
        );
        let transport = ProcessWorkerSupervisor::with_launcher_and_deadlines(
            launcher.clone(),
            short_deadlines(),
        )
        .unwrap();
        let vad = SileroVadWorkerSupervisor::with_transport_and_deadlines(
            transport,
            short_vad_deadlines(),
        );
        let threshold = VadThreshold::new(0.5).unwrap();
        let window = [0.9; WINDOW_SAMPLES];
        let old_session = 501;
        let mut request_id = 1;

        if !matches!(operation, BlockedVadOperation::Load) {
            vad.load(old_session, request_id, 1).unwrap();
            request_id += 1;
        }
        if matches!(
            operation,
            BlockedVadOperation::Compute
                | BlockedVadOperation::Reset
                | BlockedVadOperation::End
                | BlockedVadOperation::Cancel
        ) {
            vad.start_session(old_session, request_id, threshold)
                .unwrap();
            request_id += 1;
        }

        let failure_started = Instant::now();
        let result = match operation {
            BlockedVadOperation::Load => vad.load(old_session, request_id, 1).map(|_| ()),
            BlockedVadOperation::Start => vad
                .start_session(old_session, request_id, threshold)
                .map(|_| ()),
            BlockedVadOperation::Health => vad.health(old_session, request_id).map(|_| ()),
            BlockedVadOperation::Compute => {
                vad.compute(old_session, request_id, &window).map(|_| ())
            }
            BlockedVadOperation::Reset => vad.reset(old_session, request_id).map(|_| ()),
            BlockedVadOperation::End => vad.end_session(old_session, request_id).map(|_| ()),
            BlockedVadOperation::Cancel => vad.cancel_session(old_session, request_id).map(|_| ()),
        };
        let error = result.unwrap_err();
        started_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(error.to_string().contains("deadline exceeded"));
        kill_started_rx
            .recv_timeout(Duration::from_millis(250))
            .unwrap();
        let remaining = Duration::from_millis(250).saturating_sub(failure_started.elapsed());
        reaped_rx.recv_timeout(remaining).unwrap();
        let elapsed = failure_started.elapsed();
        eprintln!("blocked VAD {operation:?} retired and reaped in {elapsed:?}");
        assert!(
            elapsed < Duration::from_millis(250),
            "blocked VAD {operation:?} exceeded the test bound: {elapsed:?}"
        );
        assert!(vad.compute(old_session, request_id + 1, &window).is_err());

        let new_session = 502;
        assert!(!vad.load(new_session, 100, 1).unwrap());
        vad.health(new_session, 101).unwrap();
        vad.start_session(new_session, 102, threshold).unwrap();
        let decision = vad.compute(new_session, 103, &window).unwrap();
        assert_eq!(decision.probability, 0.4);
        assert!(!decision.speech);
        vad.end_session(new_session, 104).unwrap();
        assert_eq!(launcher.launches.load(Ordering::Acquire), 2);
    }

    #[test]
    fn production_vad_deadlines_are_bounded_without_changing_stt_deadlines() {
        let stt = SupervisorDeadlines::default();
        assert_eq!(stt.load, Duration::from_secs(15 * 60));
        assert_eq!(stt.data, Duration::from_secs(60 * 60));

        let vad = VadDeadlines::default();
        assert_eq!(vad.acquisition, Duration::from_secs(2));
        assert_eq!(vad.operation, Duration::from_millis(250));
    }

    #[test]
    fn blocked_hello_obeys_aggregate_budget_reaps_and_later_session_recovers() {
        let budget = Duration::from_millis(120);
        let (blocked_tx, blocked_rx) = channel();
        let (kill_tx, kill_rx) = channel();
        let (reaped_tx, reaped_rx) = channel();
        let launcher = Arc::new(
            TestLauncher::new([
                TestMode::BlockedHello {
                    started: blocked_tx,
                },
                TestMode::VadNormal,
            ])
            .with_process_events(kill_tx, reaped_tx),
        );
        let cancelled = AtomicBool::new(false);
        let threshold = VadThreshold::new(0.5).unwrap();

        let started = Instant::now();
        let error = SileroVadWorkerSupervisor::acquire_session_with_launcher_and_deadlines(
            launcher.clone(),
            701,
            1,
            1,
            threshold,
            acquisition_test_deadlines(budget),
            &cancelled,
        )
        .err()
        .expect("blocked Hello must fail acquisition");
        let elapsed = started.elapsed();
        blocked_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        kill_rx.recv_timeout(Duration::from_millis(100)).unwrap();
        reaped_rx.recv_timeout(Duration::from_millis(100)).unwrap();
        assert!(error.to_string().contains("deadline exceeded"));
        assert!(
            elapsed <= budget + Duration::from_millis(80),
            "blocked Hello exceeded aggregate bound: {elapsed:?}"
        );

        let (vad, next_request_id) =
            SileroVadWorkerSupervisor::acquire_session_with_launcher_and_deadlines(
                launcher.clone(),
                702,
                1,
                1,
                threshold,
                acquisition_test_deadlines(budget),
                &cancelled,
            )
            .unwrap();
        let decision = vad
            .compute(702, next_request_id, &[0.9; WINDOW_SAMPLES])
            .unwrap();
        assert_eq!(decision.probability, 0.4);
        assert!(!decision.speech);
        assert_eq!(launcher.launches.load(Ordering::Acquire), 2);
        vad.end_session(702, next_request_id + 1).unwrap();
        vad.transport.terminate_current().unwrap();
        kill_rx.recv_timeout(Duration::from_millis(100)).unwrap();
        reaped_rx.recv_timeout(Duration::from_millis(100)).unwrap();
        eprintln!("blocked Hello aggregate acquisition elapsed: {elapsed:?}");
    }

    #[test]
    fn cumulative_acquisition_delays_share_one_budget_and_recover_cleanly() {
        let budget = Duration::from_millis(140);
        let stage_delay = Duration::from_millis(30);
        let (blocked_tx, blocked_rx) = channel();
        let (kill_tx, kill_rx) = channel();
        let (reaped_tx, reaped_rx) = channel();
        let launcher = Arc::new(
            TestLauncher::new([
                TestMode::CumulativeVadAcquisition {
                    stage_delay,
                    blocked: blocked_tx,
                },
                TestMode::VadNormal,
            ])
            .with_process_events(kill_tx, reaped_tx),
        );
        let cancelled = AtomicBool::new(false);
        let threshold = VadThreshold::new(0.5).unwrap();

        let started = Instant::now();
        let error = SileroVadWorkerSupervisor::acquire_session_with_launcher_and_deadlines(
            launcher.clone(),
            711,
            1,
            1,
            threshold,
            acquisition_test_deadlines(budget),
            &cancelled,
        )
        .err()
        .expect("final readiness must share the spent stage budget");
        let elapsed = started.elapsed();
        blocked_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        kill_rx.recv_timeout(Duration::from_millis(100)).unwrap();
        reaped_rx.recv_timeout(Duration::from_millis(100)).unwrap();
        assert!(error.to_string().contains("deadline exceeded"));
        assert!(
            elapsed >= stage_delay * 3,
            "successful stages did not spend the shared budget: {elapsed:?}"
        );
        assert!(
            elapsed <= budget + Duration::from_millis(80),
            "stages received independent budgets: {elapsed:?}"
        );

        let (vad, next_request_id) =
            SileroVadWorkerSupervisor::acquire_session_with_launcher_and_deadlines(
                launcher.clone(),
                712,
                1,
                1,
                threshold,
                acquisition_test_deadlines(budget),
                &cancelled,
            )
            .unwrap();
        assert_eq!(
            vad.compute(712, next_request_id, &[0.9; WINDOW_SAMPLES])
                .unwrap()
                .probability,
            0.4
        );
        vad.end_session(712, next_request_id + 1).unwrap();
        vad.transport.terminate_current().unwrap();
        kill_rx.recv_timeout(Duration::from_millis(100)).unwrap();
        reaped_rx.recv_timeout(Duration::from_millis(100)).unwrap();
        assert_eq!(launcher.launches.load(Ordering::Acquire), 2);
        eprintln!("cumulative VAD acquisition elapsed: {elapsed:?}");
    }

    #[test]
    fn late_launch_is_never_published_and_is_killed_reaped_before_recovery() {
        let budget = Duration::from_millis(70);
        let (launch_started_tx, launch_started_rx) = channel();
        let (release_tx, release_rx) = channel();
        let (kill_tx, kill_rx) = channel();
        let (reaped_tx, reaped_rx) = channel();
        let launcher = Arc::new(
            TestLauncher::new([
                TestMode::DelayedLaunch {
                    started: launch_started_tx,
                    release: release_rx,
                    then: Box::new(TestMode::VadNormal),
                },
                TestMode::VadNormal,
            ])
            .with_process_events(kill_tx, reaped_tx),
        );
        let acquisition_launcher = launcher.clone();
        let cancelled = Arc::new(AtomicBool::new(false));
        let acquisition_cancelled = Arc::clone(&cancelled);
        let threshold = VadThreshold::new(0.5).unwrap();
        let (result_tx, result_rx) = channel();
        let started = Instant::now();
        let acquisition = std::thread::spawn(move || {
            let result = SileroVadWorkerSupervisor::acquire_session_with_launcher_and_deadlines(
                acquisition_launcher,
                721,
                1,
                1,
                threshold,
                acquisition_test_deadlines(budget),
                acquisition_cancelled.as_ref(),
            );
            result_tx.send(result.map(|_| ())).unwrap();
        });
        launch_started_rx
            .recv_timeout(Duration::from_secs(1))
            .unwrap();
        let error = result_rx
            .recv_timeout(budget + Duration::from_millis(100))
            .unwrap()
            .unwrap_err();
        let elapsed = started.elapsed();
        assert!(error.to_string().contains("deadline exceeded"));
        assert!(elapsed <= budget + Duration::from_millis(80));
        acquisition.join().unwrap();

        release_tx.send(()).unwrap();
        kill_rx.recv_timeout(Duration::from_millis(100)).unwrap();
        reaped_rx.recv_timeout(Duration::from_millis(100)).unwrap();

        let (vad, next_request_id) =
            SileroVadWorkerSupervisor::acquire_session_with_launcher_and_deadlines(
                launcher.clone(),
                722,
                1,
                1,
                threshold,
                acquisition_test_deadlines(budget),
                cancelled.as_ref(),
            )
            .unwrap();
        assert_eq!(
            vad.compute(722, next_request_id, &[0.9; WINDOW_SAMPLES])
                .unwrap()
                .probability,
            0.4
        );
        vad.end_session(722, next_request_id + 1).unwrap();
        vad.transport.terminate_current().unwrap();
        kill_rx.recv_timeout(Duration::from_millis(100)).unwrap();
        reaped_rx.recv_timeout(Duration::from_millis(100)).unwrap();
        assert_eq!(launcher.launches.load(Ordering::Acquire), 2);
        eprintln!("late launch acquisition returned in {elapsed:?}");
    }

    #[test]
    fn cancellation_interrupts_acquisition_and_reaps_before_later_recovery() {
        let budget = Duration::from_millis(400);
        let (blocked_tx, blocked_rx) = channel();
        let (kill_tx, kill_rx) = channel();
        let (reaped_tx, reaped_rx) = channel();
        let launcher = Arc::new(
            TestLauncher::new([
                TestMode::BlockedHello {
                    started: blocked_tx,
                },
                TestMode::VadNormal,
            ])
            .with_process_events(kill_tx, reaped_tx),
        );
        let acquisition_launcher = launcher.clone();
        let cancelled = Arc::new(AtomicBool::new(false));
        let acquisition_cancelled = Arc::clone(&cancelled);
        let threshold = VadThreshold::new(0.5).unwrap();
        let acquisition = std::thread::spawn(move || {
            SileroVadWorkerSupervisor::acquire_session_with_launcher_and_deadlines(
                acquisition_launcher,
                731,
                1,
                1,
                threshold,
                acquisition_test_deadlines(budget),
                acquisition_cancelled.as_ref(),
            )
            .map(|_| ())
        });
        blocked_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        let cancelled_at = Instant::now();
        cancelled.store(true, Ordering::Release);
        let error = acquisition.join().unwrap().unwrap_err();
        let cancellation_elapsed = cancelled_at.elapsed();
        kill_rx.recv_timeout(Duration::from_millis(100)).unwrap();
        reaped_rx.recv_timeout(Duration::from_millis(100)).unwrap();
        assert!(error.to_string().contains("cancelled"));
        assert!(cancellation_elapsed <= Duration::from_millis(100));

        cancelled.store(false, Ordering::Release);
        let (vad, next_request_id) =
            SileroVadWorkerSupervisor::acquire_session_with_launcher_and_deadlines(
                launcher.clone(),
                732,
                1,
                1,
                threshold,
                acquisition_test_deadlines(budget),
                cancelled.as_ref(),
            )
            .unwrap();
        assert_eq!(
            vad.compute(732, next_request_id, &[0.9; WINDOW_SAMPLES])
                .unwrap()
                .probability,
            0.4
        );
        vad.end_session(732, next_request_id + 1).unwrap();
        vad.transport.terminate_current().unwrap();
        kill_rx.recv_timeout(Duration::from_millis(100)).unwrap();
        reaped_rx.recv_timeout(Duration::from_millis(100)).unwrap();
        assert_eq!(launcher.launches.load(Ordering::Acquire), 2);
        eprintln!("VAD acquisition cancellation elapsed: {cancellation_elapsed:?}");
    }

    #[test]
    fn blocked_vad_load_is_bounded_reaped_and_recovers() {
        assert_blocked_vad_request_retires_reaps_and_recovers(BlockedVadOperation::Load);
    }

    #[test]
    fn blocked_vad_start_is_bounded_reaped_and_recovers() {
        assert_blocked_vad_request_retires_reaps_and_recovers(BlockedVadOperation::Start);
    }

    #[test]
    fn blocked_vad_health_is_bounded_reaped_and_recovers() {
        assert_blocked_vad_request_retires_reaps_and_recovers(BlockedVadOperation::Health);
    }

    #[test]
    fn blocked_vad_compute_is_bounded_reaped_and_recovers_without_a_decision() {
        assert_blocked_vad_request_retires_reaps_and_recovers(BlockedVadOperation::Compute);
    }

    #[test]
    fn blocked_vad_reset_is_bounded_reaped_and_recovers() {
        assert_blocked_vad_request_retires_reaps_and_recovers(BlockedVadOperation::Reset);
    }

    #[test]
    fn blocked_vad_end_is_bounded_reaped_and_recovers() {
        assert_blocked_vad_request_retires_reaps_and_recovers(BlockedVadOperation::End);
    }

    #[test]
    fn blocked_vad_cancel_is_bounded_reaped_and_recovers() {
        assert_blocked_vad_request_retires_reaps_and_recovers(BlockedVadOperation::Cancel);
    }

    #[test]
    fn malformed_vad_compute_retires_generation_and_recovers_without_a_decision() {
        let (kill_started_tx, kill_started_rx) = channel();
        let (reaped_tx, reaped_rx) = channel();
        let launcher = Arc::new(
            TestLauncher::new([TestMode::MalformedVadWindow, TestMode::VadNormal])
                .with_process_events(kill_started_tx, reaped_tx),
        );
        let transport = test_supervisor(launcher.clone());
        let vad = SileroVadWorkerSupervisor::with_transport(transport);
        let threshold = VadThreshold::new(0.5).unwrap();
        let window = [0.9; WINDOW_SAMPLES];

        vad.load(601, 1, 1).unwrap();
        vad.start_session(601, 2, threshold).unwrap();
        assert!(vad.compute(601, 3, &window).is_err());
        kill_started_rx
            .recv_timeout(Duration::from_millis(250))
            .unwrap();
        reaped_rx.recv_timeout(Duration::from_millis(250)).unwrap();

        assert!(!vad.load(602, 4, 1).unwrap());
        vad.start_session(602, 5, threshold).unwrap();
        assert_eq!(vad.compute(602, 6, &window).unwrap().probability, 0.4);
        vad.end_session(602, 7).unwrap();
        assert_eq!(launcher.launches.load(Ordering::Acquire), 2);
    }

    #[test]
    fn hung_health_expires_its_deadline_invalidates_generation_and_recovers() {
        let (started_tx, started_rx) = channel();
        let (release_tx, release_rx) = channel();
        let launcher = Arc::new(TestLauncher::new([
            TestMode::HoldOne {
                started: started_tx,
                release: release_rx,
            },
            TestMode::Normal,
        ]));
        let supervisor = ProcessWorkerSupervisor::with_launcher_and_deadlines(
            launcher.clone(),
            short_deadlines(),
        )
        .unwrap();
        let waiting = supervisor.clone();
        let request = std::thread::spawn(move || waiting.health(70, 71));
        started_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        let started = Instant::now();
        let error = request.join().unwrap().unwrap_err();
        assert!(started.elapsed() < Duration::from_millis(500));
        assert!(error.to_string().contains("deadline exceeded"));
        drop(release_tx);
        supervisor.health(72, 73).unwrap();
        assert_eq!(launcher.launches.load(Ordering::Acquire), 2);
    }

    #[test]
    fn cooperative_stream_cancel_timeout_invalidates_and_kills_generation() {
        let (started_tx, started_rx) = channel();
        let (release_tx, release_rx) = channel();
        let (kill_started_tx, kill_started_rx) = channel();
        let (reaped_tx, _reaped_rx) = channel();
        let launcher = Arc::new(
            TestLauncher::new([TestMode::HoldCancel {
                started: started_tx,
                release: release_rx,
            }])
            .with_process_events(kill_started_tx, reaped_tx),
        );
        let supervisor =
            ProcessWorkerSupervisor::with_launcher_and_deadlines(launcher, short_deadlines())
                .unwrap();
        supervisor.start_stream(88, 89).unwrap();
        let waiting = supervisor.clone();
        let cancel = std::thread::spawn(move || waiting.cancel_stream(88, 90));
        started_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        let started = Instant::now();
        let error = cancel.join().unwrap().unwrap_err();
        assert!(started.elapsed() < Duration::from_millis(250));
        assert!(error.to_string().contains("deadline exceeded"));
        kill_started_rx
            .recv_timeout(Duration::from_millis(250))
            .unwrap();
        drop(release_tx);
    }

    #[test]
    fn abandoning_stream_returns_without_waiting_for_worker_io() {
        let (kill_started_tx, kill_started_rx) = channel();
        let (reaped_tx, reaped_rx) = channel();
        let (completed_tx, completed_rx) = channel();
        let launcher = Arc::new(
            TestLauncher::new([TestMode::AbandonStream {
                completed: completed_tx,
            }])
            .with_process_events(kill_started_tx, reaped_tx),
        );
        let supervisor = test_supervisor(launcher);
        supervisor.start_stream(90, 91).unwrap();
        let started = Instant::now();
        supervisor.abandon_stream(90);
        assert!(started.elapsed() < Duration::from_millis(50));
        kill_started_rx
            .recv_timeout(Duration::from_millis(250))
            .unwrap();
        completed_rx
            .recv_timeout(Duration::from_millis(250))
            .expect("the fake worker must treat abandon EOF as clean termination");
        reaped_rx
            .recv_timeout(Duration::from_millis(250))
            .expect("the abandoned fake worker must be joined");
    }

    #[test]
    fn blocked_stream_chunk_and_end_are_cancelled_and_reaped_within_250_milliseconds() {
        for end_stream in [false, true] {
            let (started_tx, started_rx) = channel();
            let (kill_started_tx, kill_started_rx) = channel();
            let (reaped_tx, reaped_rx) = channel();
            let launcher = Arc::new(
                TestLauncher::new([
                    TestMode::BlockedStreamOperation {
                        end_stream,
                        started: started_tx,
                    },
                    TestMode::Normal,
                ])
                .with_process_events(kill_started_tx, reaped_tx),
            );
            let supervisor = test_supervisor(launcher);
            supervisor.start_stream(33, 1).unwrap();

            let operation_supervisor = supervisor.clone();
            let operation = std::thread::spawn(move || {
                if end_stream {
                    operation_supervisor.end_stream(33, 2)
                } else {
                    operation_supervisor.audio_chunk(33, 2, &[0.1])
                }
            });
            started_rx.recv_timeout(Duration::from_secs(1)).unwrap();

            let cancel_started = Instant::now();
            supervisor.cancel_stream(33, 3).unwrap();
            let error = operation.join().unwrap().unwrap_err();
            kill_started_rx
                .recv_timeout(Duration::from_millis(250))
                .unwrap();
            let remaining = Duration::from_millis(250).saturating_sub(cancel_started.elapsed());
            reaped_rx.recv_timeout(remaining).unwrap();
            let cancel_and_reap_duration = cancel_started.elapsed();

            assert!(
                cancel_and_reap_duration <= Duration::from_millis(250),
                "stream cancellation and reaping took {cancel_and_reap_duration:?}"
            );
            assert!(error.to_string().contains("cancelled"));
            supervisor.health(34, 4).unwrap();
        }
    }

    #[test]
    fn duplicate_generation_correlation_is_rejected_without_displacing_original_waiter() {
        let (started_tx, started_rx) = channel();
        let (release_tx, release_rx) = channel();
        let launcher = Arc::new(TestLauncher::new([TestMode::HoldOne {
            started: started_tx,
            release: release_rx,
        }]));
        let supervisor = test_supervisor(launcher);
        let first_supervisor = supervisor.clone();
        let first = std::thread::spawn(move || first_supervisor.health(7, 11));
        started_rx.recv_timeout(Duration::from_secs(1)).unwrap();

        let duplicate = supervisor.health(7, 11).unwrap_err();
        assert!(duplicate.to_string().contains("duplicate"));
        release_tx.send(()).unwrap();
        first.join().unwrap().unwrap();
    }

    #[test]
    fn stale_old_generation_response_cannot_invalidate_replacement() {
        let (started_tx, started_rx) = channel();
        let (release_tx, release_rx) = channel();
        let (sent_tx, sent_rx) = channel();
        let launcher = Arc::new(TestLauncher::new([
            TestMode::HoldStale {
                started: started_tx,
                release: release_rx,
                sent: sent_tx,
            },
            TestMode::Normal,
        ]));
        let supervisor = test_supervisor(launcher);
        let old_generation = supervisor.generation_for_test();
        let old_supervisor = supervisor.clone();
        let old_request = std::thread::spawn(move || old_supervisor.health(1, 1));
        started_rx.recv_timeout(Duration::from_secs(1)).unwrap();

        supervisor.abandon_generation_for_test(old_generation, "test generation retired");
        assert!(old_request.join().unwrap().is_err());
        supervisor.health(2, 2).unwrap();
        let replacement_generation = supervisor.generation_for_test();
        assert!(replacement_generation > old_generation);

        release_tx.send(()).unwrap();
        sent_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        supervisor.health(3, 3).unwrap();
        assert_eq!(supervisor.generation_for_test(), replacement_generation);
    }

    fn assert_generation_failure_fails_all_pending_and_recovers(malformed: bool) {
        let (received_tx, received_rx) = channel();
        let (kill_started_tx, kill_started_rx) = channel();
        let (reaped_tx, reaped_rx) = channel();
        let launcher = Arc::new(
            TestLauncher::new([
                TestMode::FailRequest {
                    received: received_tx,
                    malformed,
                },
                TestMode::Normal,
            ])
            .with_process_events(kill_started_tx, reaped_tx),
        );
        let supervisor = test_supervisor(Arc::clone(&launcher));
        let first_supervisor = supervisor.clone();
        let first = std::thread::spawn(move || first_supervisor.health(10, 20));
        received_rx.recv_timeout(Duration::from_secs(1)).unwrap();

        assert!(first.join().unwrap().is_err());
        kill_started_rx
            .recv_timeout(Duration::from_secs(1))
            .unwrap();
        reaped_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        supervisor.health(12, 22).unwrap();
        assert_eq!(launcher.launches.load(Ordering::Acquire), 2);
    }

    #[test]
    fn protocol_failure_fails_all_pending_and_next_request_recovers() {
        assert_generation_failure_fails_all_pending_and_recovers(true);
    }

    #[test]
    fn eof_fails_all_pending_and_next_request_recovers() {
        assert_generation_failure_fails_all_pending_and_recovers(false);
    }

    #[test]
    fn bad_response_direction_and_frame_kind_invalidate_the_generation() {
        for pcm_kind in [false, true] {
            let launcher = Arc::new(TestLauncher::new([
                TestMode::InvalidResponse { pcm_kind },
                TestMode::Normal,
            ]));
            let supervisor = test_supervisor(launcher);
            let generation = supervisor.generation_for_test();
            assert!(supervisor.health(30, 40).is_err());
            supervisor.health(31, 41).unwrap();
            assert!(supervisor.generation_for_test() > generation);
        }
    }

    #[test]
    fn dropping_last_supervisor_owner_synchronously_kills_and_reaps_worker() {
        let (kill_started_tx, kill_started_rx) = channel();
        let (reaped_tx, reaped_rx) = channel();
        let launcher = Arc::new(
            TestLauncher::new([TestMode::Normal]).with_process_events(kill_started_tx, reaped_tx),
        );
        let supervisor = test_supervisor(launcher);

        drop(supervisor);

        kill_started_rx.try_recv().unwrap();
        reaped_rx.try_recv().unwrap();
    }

    #[test]
    fn protocol_rejects_bad_magic_version_kind_truncation_and_oversized_frames() {
        for bytes in [
            vec![0; HEADER_LEN],
            raw_header(PROTOCOL_VERSION + 1, FrameKind::Control as u8, 0),
            raw_header(PROTOCOL_VERSION, 99, 0),
            raw_header(PROTOCOL_VERSION, FrameKind::Control as u8, 1),
            raw_header(
                PROTOCOL_VERSION,
                FrameKind::Control as u8,
                (MAX_CONTROL_BYTES + 1) as u32,
            ),
            raw_header(
                PROTOCOL_VERSION,
                FrameKind::Pcm as u8,
                (MAX_PCM_FRAME_BYTES + 1) as u32,
            ),
        ] {
            assert!(read_frame(&mut Cursor::new(bytes)).is_err());
        }
    }

    #[test]
    fn protocol_v5_rejects_v4_and_removed_direct_commands() {
        let v4 = raw_header(4, FrameKind::Control as u8, 0);
        assert!(read_frame(&mut Cursor::new(v4)).is_err());

        for removed in [
            br#"{"command":"load","model":{}}"#.as_slice(),
            br#"{"command":"transcribe"}"#.as_slice(),
        ] {
            assert!(serde_json::from_slice::<Control>(removed).is_err());
        }

        assert!(worker_role_from_args(&[std::ffi::OsString::from("--onnx-worker")]).is_err());
        assert!(
            worker_role_from_args(&[std::ffi::OsString::from("--scribe-onnx-worker")]).is_err()
        );
    }

    #[test]
    fn protocol_round_trips_pcm_and_preserves_correlations() {
        let frame = Frame {
            kind: FrameKind::Pcm,
            session_id: 7,
            request_id: 11,
            body: vec![1, 0, 0, 0],
        };
        let mut bytes = Vec::new();
        write_frame(&mut bytes, &frame).unwrap();
        let decoded = read_frame(&mut Cursor::new(bytes)).unwrap();
        assert_eq!(decoded, frame);
    }

    #[test]
    fn control_parser_enforces_direction_and_json_shape() {
        let parent_command = control_frame(7, 11, &Control::Health).unwrap();
        let worker_response = control_frame(7, 11, &Control::Ok).unwrap();
        assert!(parse_parent_control(parent_command.clone()).is_ok());
        assert!(parse_worker_control(parent_command).is_err());
        assert!(parse_worker_control(worker_response.clone()).is_ok());
        assert!(parse_parent_control(worker_response).is_err());

        let malformed = Frame {
            kind: FrameKind::Control,
            session_id: 7,
            request_id: 11,
            body: br#"{"command":"unknown"}"#.to_vec(),
        };
        assert!(parse_control(malformed).is_err());
        let pcm = Frame {
            kind: FrameKind::Pcm,
            session_id: 7,
            request_id: 11,
            body: 0.0_f32.to_le_bytes().to_vec(),
        };
        assert!(parse_control(pcm).is_err());
    }

    #[test]
    fn every_model_family_accepts_only_its_exact_role_layouts() {
        let cases = [
            (OnnxModelFamily::Moonshine, MOONSHINE_MERGED_ROLES),
            (OnnxModelFamily::Moonshine, MOONSHINE_V1_ROLES),
            (OnnxModelFamily::NemoCtc, NEMO_CTC_ROLES),
            (OnnxModelFamily::Canary, CANARY_ROLES),
            (OnnxModelFamily::OfflineTransducer, TRANSDUCER_ROLES),
            (OnnxModelFamily::OnlineTransducer, TRANSDUCER_ROLES),
        ];
        for (index, (family, roles)) in cases.into_iter().enumerate() {
            let root = test_root(&format!("roles-{index}"));
            let valid = spec_with_roles(&root, family, roles);
            valid.validate().unwrap();

            let mut missing = valid.clone();
            missing.files.remove(roles.last().unwrap());
            assert!(
                missing.validate().is_err(),
                "{family:?} accepted a missing role"
            );

            let mut extra = valid;
            let unexpected = OnnxFileRole::Model;
            if !extra.files.contains_key(&unexpected) {
                let relative = PathBuf::from("unexpected.onnx");
                std::fs::write(root.join(&relative), b"unexpected").unwrap();
                extra.files.insert(unexpected, relative);
            } else {
                let relative = PathBuf::from("unexpected-joiner.onnx");
                std::fs::write(root.join(&relative), b"unexpected").unwrap();
                extra.files.insert(OnnxFileRole::Joiner, relative);
            }
            assert!(
                extra.validate().is_err(),
                "{family:?} accepted an extra role"
            );
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn moonshine_rejects_partial_or_mixed_decoder_layouts() {
        let root = test_root("moonshine-mixed");
        let mut mixed = spec_with_roles(&root, OnnxModelFamily::Moonshine, MOONSHINE_V1_ROLES);
        let merged = PathBuf::from("merged.onnx");
        std::fs::write(root.join(&merged), b"merged").unwrap();
        mixed.files.insert(OnnxFileRole::MergedDecoder, merged);
        assert!(mixed.validate().is_err());

        let partial_roles = [
            OnnxFileRole::Encoder,
            OnnxFileRole::Tokens,
            OnnxFileRole::Preprocessor,
            OnnxFileRole::UncachedDecoder,
        ];
        assert!(
            spec_with_roles(&root, OnnxModelFamily::Moonshine, &partial_roles)
                .validate()
                .is_err()
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn model_spec_rejects_traversal_and_canonical_escape() {
        let root = test_root("containment");
        let external = root
            .parent()
            .unwrap()
            .join(format!("scribe-onnx-external-{}", std::process::id()));
        std::fs::write(&external, b"external").unwrap();
        let canonical_root = std::fs::canonicalize(&root).unwrap();
        let canonical_external = std::fs::canonicalize(&external).unwrap();
        assert!(!canonical_file_is_within_root(
            &canonical_root,
            &canonical_external
        ));

        let mut spec = spec_with_roles(&root, OnnxModelFamily::OnlineTransducer, TRANSDUCER_ROLES);
        spec.files.insert(
            OnnxFileRole::Encoder,
            PathBuf::from("..").join(external.file_name().unwrap()),
        );
        assert!(spec.validate().is_err());
        std::fs::remove_dir_all(root).unwrap();
        std::fs::remove_file(external).unwrap();
    }

    #[test]
    fn validated_model_paths_are_canonical_and_fail_after_removal() {
        let root = test_root("validated-config-paths");
        let spec = spec_with_roles(&root, OnnxModelFamily::NemoCtc, NEMO_CTC_ROLES);
        let validated = spec.validated().unwrap();
        let canonical_model = std::fs::canonicalize(root.join("model.onnx")).unwrap();
        assert_eq!(
            validated.path(OnnxFileRole::Model).unwrap(),
            canonical_model.to_string_lossy()
        );

        std::fs::remove_file(&canonical_model).unwrap();
        assert!(validated.path(OnnxFileRole::Model).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn validated_artifact_rejects_a_symlink_swap_before_config_creation() {
        use std::os::unix::fs::symlink;

        let root = test_root("validated-symlink-swap");
        let external = root
            .parent()
            .unwrap()
            .join(format!("scribe-onnx-swap-external-{}", std::process::id()));
        std::fs::write(&external, b"replacement").unwrap();
        let spec = spec_with_roles(&root, OnnxModelFamily::NemoCtc, NEMO_CTC_ROLES);
        let validated = spec.validated().unwrap();
        let model_path = root.join("model.onnx");
        std::fs::remove_file(&model_path).unwrap();
        symlink(&external, &model_path).unwrap();
        assert!(validated.path(OnnxFileRole::Model).is_err());
        std::fs::remove_dir_all(root).unwrap();
        std::fs::remove_file(external).unwrap();
    }

    #[test]
    fn pcm_codec_rejects_empty_misaligned_nonfinite_out_of_range_and_oversized_audio() {
        assert!(validate_pcm_samples(&[]).is_err());
        assert!(decode_pcm(&[0, 1, 2]).is_err());
        for invalid in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, -1.01, 1.01] {
            assert!(
                validate_pcm_samples(&[invalid]).is_err(),
                "accepted {invalid}"
            );
            assert!(
                decode_pcm(&invalid.to_le_bytes()).is_err(),
                "decoded {invalid}"
            );
        }
        assert!(validate_cumulative_sample_count(usize::MAX).is_err());

        let samples = [-1.0, -0.25, 0.0, 0.25, 1.0];
        let encoded = encode_pcm(&samples).unwrap();
        assert_eq!(decode_pcm(&encoded).unwrap(), samples);
    }

    #[test]
    fn frame_and_cumulative_pcm_limits_cover_full_recording_duration_without_allocation() {
        let seconds_262 = prepared_sample_count_for_seconds(262).unwrap();
        let seconds_263 = prepared_sample_count_for_seconds(263).unwrap();
        let seconds_600 = prepared_sample_count_for_seconds(600).unwrap();
        let cumulative_limit = max_cumulative_audio_samples().unwrap();

        assert!(seconds_262 <= MAX_PCM_FRAME_SAMPLES);
        assert!(seconds_263 > MAX_PCM_FRAME_SAMPLES);
        assert!(validate_cumulative_sample_count(seconds_262).is_ok());
        assert!(validate_cumulative_sample_count(seconds_263).is_ok());
        assert!(validate_cumulative_sample_count(seconds_600).is_ok());
        assert_eq!(
            cumulative_limit,
            prepared_sample_count_for_seconds(
                config::MAX_RECORDING_SECONDS + config::RECORDING_CAPTURE_SAFETY_ALLOWANCE_SECONDS
            )
            .unwrap()
        );
        assert!(validate_cumulative_sample_count(cumulative_limit).is_ok());
        assert!(validate_cumulative_sample_count(cumulative_limit + 1).is_err());
    }

    #[test]
    fn stream_cumulative_accounting_accepts_the_limit_and_rejects_overflow() {
        let limit = max_cumulative_audio_samples().unwrap();
        assert_eq!(
            checked_cumulative_sample_count(limit - 1, 1).unwrap(),
            limit
        );
        assert!(checked_cumulative_sample_count(limit, 1).is_err());
        assert!(checked_cumulative_sample_count(usize::MAX, 1).is_err());
    }

    #[test]
    fn batch_reservation_failure_is_a_bounded_typed_runtime_error() {
        let error = reserve_batch_samples(usize::MAX).unwrap_err();
        assert!(matches!(
            error.downcast_ref::<RuntimeError>(),
            Some(RuntimeError::Engine(message))
                if message == "runtime batch could not reserve its declared bounded audio buffer"
        ));
    }

    #[test]
    fn cpu_only_acceleration_accepts_auto_and_cpu_but_rejects_gpu() {
        let auto = resolve_cpu_only_acceleration(AccelerationPreference::Auto).unwrap();
        assert_eq!(auto.resolved, ComputeDevice::Cpu);
        assert!(auto.diagnostic.is_some());
        assert_eq!(auto.selection, None);

        let cpu = resolve_cpu_only_acceleration(AccelerationPreference::Cpu).unwrap();
        assert_eq!(cpu.resolved, ComputeDevice::Cpu);
        assert_eq!(cpu.diagnostic, None);
        assert_eq!(cpu.selection, None);

        let gpu = resolve_cpu_only_acceleration(AccelerationPreference::Gpu).unwrap_err();
        assert!(gpu.to_string().contains("CPU-only"));
        assert!(gpu.to_string().contains("Auto or CPU"));
    }
}
