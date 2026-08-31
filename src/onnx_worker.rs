//! Private, process-isolated CPU-only sherpa-onnx execution substrate.
//!
//! This module deliberately has no catalog or UI entry point. A future typed
//! installer supplies a verified [`OnnxModelSpec`]; the router remains the only
//! component allowed to construct a worker client.

use std::collections::hash_map::Entry;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::time::{Duration, Instant};

use crate::backend_policy::{BackendSelection, BackendTarget};
use crate::config;
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

#[cfg(unix)]
use std::os::unix::process::CommandExt;
#[cfg(windows)]
use std::os::windows::io::AsRawHandle;
#[cfg(windows)]
use std::os::windows::process::CommandExt;

pub(crate) const PROTOCOL_MAGIC: [u8; 4] = *b"SCIF";
pub(crate) const PROTOCOL_VERSION: u8 = 5;
pub(crate) const INFERENCE_WORKER_FLAG: &str = "--scribe-inference-worker";
pub(crate) const VAD_WORKER_FLAG: &str = "--scribe-vad-worker";
const WORKER_ABI_VERSION: u16 = 1;
const DESKTOP_BUILD_ID: &str = concat!(
    "local-transcriber@",
    env!("CARGO_PKG_VERSION"),
    "#",
    env!("SCRIBE_BUILD_REVISION")
);
const INFERENCE_WORKER_BUILD_ID: &str = concat!(
    "scribe-inference-worker@",
    env!("CARGO_PKG_VERSION"),
    "#",
    env!("SCRIBE_BUILD_REVISION")
);
const VAD_WORKER_BUILD_ID: &str = concat!(
    "scribe-vad-worker@",
    env!("CARGO_PKG_VERSION"),
    "#",
    env!("SCRIBE_BUILD_REVISION")
);
const PARENT_LIVENESS_ENV: &str = "SCRIBE_PRIVATE_PARENT_LIVENESS";
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
}

impl MonotonicDeadline {
    fn after(budget: Duration) -> Result<Self> {
        if budget.is_zero() {
            bail!("Silero VAD acquisition budget must be positive");
        }
        let started = Instant::now();
        let expires_at = started
            .checked_add(budget)
            .ok_or_else(|| anyhow!("Silero VAD acquisition deadline overflowed"))?;
        Ok(Self { expires_at, budget })
    }

    fn remaining(self) -> Result<Duration> {
        let remaining = self.expires_at.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            bail!(
                "Silero VAD acquisition deadline exceeded after {} ms",
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
    if cfg!(feature = "vulkan-acceleration") {
        WorkerProvider::Vulkan
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
    let bundled_worker_sha256 = match role {
        WorkerRole::Inference => {
            #[cfg(test)]
            {
                String::new()
            }
            #[cfg(not(test))]
            {
                let executable = std::env::current_exe()
                    .context("could not locate inference worker for capability fingerprint")?;
                sha256_file(&executable)
                    .context("could not fingerprint inference worker for capability handshake")?
            }
        }
        WorkerRole::Vad => "same-executable".to_owned(),
    };
    Ok(WorkerCapability {
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
    })
}

fn random_challenge() -> Result<String> {
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes)
        .map_err(|error| anyhow!("could not obtain worker handshake randomness: {error}"))?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn sha256_file(path: &Path) -> Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
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
    let local = expected_worker(actual_role);
    // Final-image verification is directional: the parent owns the verified
    // executable descriptor and digest. The child validates the protocol and
    // build contract but does not compare the parent-supplied digest against a
    // compile-time environment value.
    if expected.app_build != local.app_build
        || expected.worker_build != local.worker_build
        || expected.abi != local.abi
        || expected.role != local.role
        || expected.provider != local.provider
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
    Poisoned,
    UnsupportedModel,
    WorkerUnavailable,
    OnnxUnavailable,
    RetryableWorkerFailure,
    Cancelled,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum WireFailureCategory {
    Artifact,
    InvalidInput,
    Decode,
    Callback,
    Worker,
    Unsupported,
    Cancellation,
    Provider,
}

impl Default for WireFailureCategory {
    fn default() -> Self {
        Self::Worker
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum WireRetryDisposition {
    Never,
    NextProviderBeforeOutput,
}

impl Default for WireRetryDisposition {
    fn default() -> Self {
        Self::Never
    }
}

impl WireRuntimeError {
    fn from_runtime(error: &RuntimeError) -> Self {
        let code = match error {
            RuntimeError::InvalidAudio { .. } => WireRuntimeErrorCode::InvalidAudio,
            RuntimeError::Inference(_) => WireRuntimeErrorCode::Inference,
            RuntimeError::Callback(_) => WireRuntimeErrorCode::Callback,
            RuntimeError::Engine(_) => WireRuntimeErrorCode::Engine,
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
            RuntimeError::Callback(_) => WireFailureCategory::Callback,
            RuntimeError::Poisoned
            | RuntimeError::WorkerUnavailable(_)
            | RuntimeError::RetryableWorkerFailure(_) => WireFailureCategory::Worker,
            RuntimeError::UnsupportedModel(_) | RuntimeError::OnnxUnavailable(_) => {
                WireFailureCategory::Unsupported
            }
            RuntimeError::Cancelled(_) => WireFailureCategory::Cancellation,
        };
        let retry = if matches!(error, RuntimeError::RetryableWorkerFailure(_)) {
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

#[derive(Debug)]
struct VerifiedWorkerExecutable {
    path: PathBuf,
    root: PathBuf,
    expected_name: std::ffi::OsString,
    expected_sha256: String,
    identity: WorkerExecutableIdentity,
    // On Windows this read-only, non-delete-sharing handle prevents replacement
    // between verification and CreateProcess. Other platforms still retain the
    // open inode while the immediate identity recheck closes ordinary races.
    _open_file: std::fs::File,
}

impl VerifiedWorkerExecutable {
    fn revalidate(&self) -> Result<Self> {
        let verified = verify_worker_executable(
            &self.path,
            &self.root,
            &self.expected_name,
            &self.expected_sha256,
        )?;
        if verified.identity != self.identity {
            bail!("worker executable identity changed before process creation");
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
        return Ok(WorkerExecutableIdentity {
            length: metadata.len(),
            sha256: sha256_file(path)?,
            volume_serial: information.dwVolumeSerialNumber,
            file_index: (u64::from(information.nFileIndexHigh) << 32)
                | u64::from(information.nFileIndexLow),
        });
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.nlink() != 1 {
            bail!("worker executable must not be a hardlink");
        }
        return Ok(WorkerExecutableIdentity {
            length: metadata.len(),
            sha256: sha256_file(path)?,
            device: metadata.dev(),
            inode: metadata.ino(),
        });
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
        _open_file: open_file,
    })
}

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
        #[cfg(all(unix, not(debug_assertions)))]
        {
            let _ = role;
            bail!(
                "release inference workers are unsupported on Unix until launch is bound to the verified executable descriptor"
            );
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
        let worker_flag = match self.role {
            WorkerRole::Inference => INFERENCE_WORKER_FLAG,
            WorkerRole::Vad => VAD_WORKER_FLAG,
        };
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
    role: WorkerRole,
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
        role: WorkerRole,
        launcher: Arc<dyn WorkerLauncher>,
        deadlines: SupervisorDeadlines,
    ) -> Self {
        Self {
            inner: Arc::new(SupervisorInner {
                launcher,
                role,
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
    #[cfg(test)]
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
                        "Silero VAD acquisition deadline expired before Hello",
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
        let expected = expected_worker(self.inner.role);
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
                validate_worker_capability(&capability, &challenge, &expected)
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
        let generation = self
            .transport
            .ensure_generation()
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
                generation,
                correlation,
                correlation,
                &[frame],
                self.transport.inner.deadlines.load,
            )
            .map_err(worker_unavailable)?
        {
            Control::RuntimeLoaded { execution } => Ok(execution.into()),
            Control::RuntimeFailed { error } => {
                Err(error.into_runtime_for_generation(&self.transport, generation))
            }
            Control::Error { message } => Err(RuntimeError::WorkerUnavailable(message)),
            _ => {
                let _ = self.transport.invalidate_generation(
                    generation,
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
        let generation = self
            .transport
            .ensure_generation()
            .map_err(worker_unavailable)?;
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
            Control::RuntimeTranscript { execution } => Ok(execution.into()),
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

fn worker_unavailable(error: impl std::fmt::Display) -> RuntimeError {
    RuntimeError::RetryableWorkerFailure(error.to_string())
}

#[derive(Clone)]
struct InferenceWorkerRoute {
    provider: WorkerProvider,
    supervisor: InferenceWorkerSupervisor,
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
}

impl InferenceWorkerRegistry {
    pub(crate) fn for_current_build() -> Self {
        let provider = if cfg!(feature = "vulkan-acceleration") {
            WorkerProvider::Vulkan
        } else {
            WorkerProvider::Cpu
        };
        Self {
            routes: Arc::new(vec![InferenceWorkerRoute {
                provider,
                supervisor: InferenceWorkerSupervisor::unstarted(),
            }]),
        }
    }

    pub(crate) fn cpu_only() -> Self {
        Self {
            routes: Arc::new(vec![InferenceWorkerRoute {
                provider: WorkerProvider::Cpu,
                supervisor: InferenceWorkerSupervisor::unstarted(),
            }]),
        }
    }

    #[cfg(test)]
    pub(crate) fn with_cpu_supervisor(supervisor: InferenceWorkerSupervisor) -> Self {
        Self {
            routes: Arc::new(vec![InferenceWorkerRoute {
                provider: WorkerProvider::Cpu,
                supervisor,
            }]),
        }
    }

    fn eligible_routes(
        &self,
        preference: AccelerationPreference,
    ) -> impl Iterator<Item = &InferenceWorkerRoute> {
        self.routes.iter().filter(move |route| match preference {
            AccelerationPreference::Gpu => route.provider.is_gpu(),
            AccelerationPreference::Cpu => !route.provider.is_gpu(),
            AccelerationPreference::Auto => true,
        })
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

    pub(crate) fn load(
        &self,
        artifact: RuntimeArtifact,
        preference: AccelerationPreference,
    ) -> Result<RuntimeLoadExecution, RuntimeError> {
        let mut last_retryable = None;
        let mut attempted = false;
        for route in self.eligible_routes(preference).take(4) {
            attempted = true;
            match route.supervisor.load(artifact.clone(), preference) {
                Ok(execution) => return Ok(execution),
                Err(error @ RuntimeError::RetryableWorkerFailure(_)) => {
                    last_retryable = Some(error);
                }
                Err(error) => return Err(error),
            }
        }
        Err(last_retryable.unwrap_or_else(|| {
            debug_assert!(!attempted);
            Self::no_route_error(preference)
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
        let mut last_retryable = None;
        let mut attempted = false;
        for route in self.eligible_routes(preference).take(4) {
            attempted = true;
            match route.supervisor.transcribe(
                artifact.clone(),
                preference,
                audio,
                options.clone(),
                cancellation_snapshot,
                cancellation_generation,
            ) {
                Ok(execution) => return Ok(execution),
                Err(error @ RuntimeError::RetryableWorkerFailure(_)) => {
                    last_retryable = Some(error);
                }
                Err(error) => return Err(error),
            }
        }
        Err(last_retryable.unwrap_or_else(|| {
            debug_assert!(!attempted);
            Self::no_route_error(preference)
        }))
    }

    pub(crate) fn health(
        &self,
        artifact: RuntimeArtifact,
        preference: AccelerationPreference,
    ) -> Result<(), RuntimeError> {
        let mut last_retryable = None;
        let mut attempted = false;
        for route in self.eligible_routes(preference).take(4) {
            attempted = true;
            match route.supervisor.health(artifact.clone(), preference) {
                Ok(()) => return Ok(()),
                Err(error @ RuntimeError::RetryableWorkerFailure(_)) => {
                    last_retryable = Some(error);
                }
                Err(error) => return Err(error),
            }
        }
        Err(last_retryable.unwrap_or_else(|| {
            debug_assert!(!attempted);
            Self::no_route_error(preference)
        }))
    }

    pub(crate) fn unload(&self) -> Result<(), RuntimeError> {
        for route in self.routes.iter() {
            route.supervisor.unload()?;
        }
        Ok(())
    }

    pub(crate) fn unload_if_idle(&self) -> Result<(), RuntimeError> {
        for route in self.routes.iter() {
            route.supervisor.unload_if_idle()?;
        }
        Ok(())
    }

    pub(crate) fn cancel_active(&self) {
        for route in self.routes.iter() {
            route.supervisor.cancel_active();
        }
    }

    pub(crate) fn shutdown(&self) -> Result<(), RuntimeError> {
        for route in self.routes.iter() {
            route.supervisor.shutdown()?;
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
                run_test_worker(input, output, mode);
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

    fn run_test_worker(mut input: impl Read, mut output: impl Write, mode: TestMode) {
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
        let expected = if cfg!(feature = "vulkan-acceleration") {
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
            expected
        );
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
                assert!(cfg!(windows));
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
            process_index: index,
        }
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
        };
        let registry = InferenceWorkerRegistry {
            routes: Arc::new(vec![route(first.clone()), route(second.clone())]),
        };
        assert!(
            registry
                .load(missing_gguf_artifact(), AccelerationPreference::Cpu)
                .is_err()
        );
        assert_eq!(first.launches.load(Ordering::Acquire), 1);
        assert_eq!(second.launches.load(Ordering::Acquire), 1);
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
