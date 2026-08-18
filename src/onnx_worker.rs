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
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sherpa_onnx::{
    OfflineCanaryModelConfig, OfflineMoonshineModelConfig, OfflineNemoEncDecCtcModelConfig,
    OfflineRecognizer, OfflineRecognizerConfig, OfflineTransducerModelConfig, OnlineRecognizer,
    OnlineRecognizerConfig, OnlineStream, OnlineTransducerModelConfig,
};

use crate::silero_vad_native::{SileroVadModel, VadThreshold, WINDOW_SAMPLES};
use crate::transcription::{AccelerationPreference, ComputeDevice, ResolvedAcceleration};

pub(crate) const PROTOCOL_MAGIC: [u8; 4] = *b"SCON";
pub(crate) const PROTOCOL_VERSION: u8 = 2;
const HEADER_LEN: usize = 26;
const MAX_CONTROL_BYTES: usize = 256 * 1024;
const MAX_AUDIO_BYTES: usize = 16 * 1024 * 1024;
const MAX_AUDIO_SAMPLES: usize = MAX_AUDIO_BYTES / size_of::<f32>();

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
    startup: Duration,
    operation: Duration,
}

impl Default for VadDeadlines {
    fn default() -> Self {
        Self {
            // Model construction may legitimately take longer than one 32 ms
            // window, but capture must still fail before the microphone plays.
            startup: Duration::from_secs(2),
            // One stall consumes at most one eighth of the two-second capture
            // ring, so capture fails closed well before the callback exhausts it.
            operation: Duration::from_millis(250),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum OnnxFileRole {
    Model,
    Encoder,
    Decoder,
    Joiner,
    Tokens,
    Preprocessor,
    UncachedDecoder,
    CachedDecoder,
    MergedDecoder,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum OnnxModelFamily {
    Moonshine,
    NemoCtc,
    Canary,
    OfflineTransducer,
    OnlineTransducer,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct OnnxModelSpec {
    pub id: String,
    pub root: PathBuf,
    pub family: OnnxModelFamily,
    pub files: BTreeMap<OnnxFileRole, PathBuf>,
    pub num_threads: u16,
}

impl OnnxModelSpec {
    pub(crate) fn validate(&self) -> Result<()> {
        self.validated().map(|_| ())
    }

    fn validated(&self) -> Result<ValidatedOnnxModel> {
        if self.id.trim().is_empty() || self.num_threads == 0 {
            bail!("ONNX model id and thread count must be non-empty/non-zero");
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

    fn fingerprint(&self) -> Result<String> {
        self.validated()?.fingerprint()
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct ValidatedOnnxModel {
    id: String,
    root: PathBuf,
    family: OnnxModelFamily,
    files: BTreeMap<OnnxFileRole, PathBuf>,
    num_threads: u16,
}

impl ValidatedOnnxModel {
    fn fingerprint(&self) -> Result<String> {
        Ok(format!("{:x}", Sha256::digest(serde_json::to_vec(self)?)))
    }

    fn path(&self, role: OnnxFileRole) -> Result<String> {
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
        }),
        AccelerationPreference::Cpu => Ok(ResolvedAcceleration {
            requested,
            resolved: ComputeDevice::Cpu,
            diagnostic: None,
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
        if current.exists()
            && std::fs::symlink_metadata(&current)?
                .file_type()
                .is_symlink()
        {
            bail!("ONNX path contains a symbolic link: {}", current.display());
        }
    }
    Ok(())
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
        FrameKind::Pcm => MAX_AUDIO_BYTES,
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
        bail!("invalid ONNX worker frame magic");
    }
    if header[4] != PROTOCOL_VERSION {
        bail!("unsupported ONNX worker protocol version {}", header[4]);
    }
    let kind = FrameKind::try_from(header[5])?;
    let body_len = u32::from_le_bytes(header[6..10].try_into().unwrap()) as usize;
    let limit = match kind {
        FrameKind::Control => MAX_CONTROL_BYTES,
        FrameKind::Pcm => MAX_AUDIO_BYTES,
    };
    if body_len > limit {
        bail!("ONNX worker frame body exceeds {limit}-byte limit");
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
    if body.len() > MAX_AUDIO_BYTES {
        bail!("ONNX PCM exceeds the {MAX_AUDIO_BYTES}-byte limit");
    }
    let samples = body
        .chunks_exact(size_of::<f32>())
        .map(|bytes| f32::from_le_bytes(bytes.try_into().expect("f32 chunk width is exact")))
        .collect::<Vec<_>>();
    validate_pcm_samples(&samples)?;
    Ok(samples)
}

fn validate_pcm_samples(samples: &[f32]) -> Result<()> {
    if samples.is_empty() {
        bail!("ONNX PCM must contain at least one sample");
    }
    if samples.len() > MAX_AUDIO_SAMPLES {
        bail!("ONNX PCM exceeds the {MAX_AUDIO_BYTES}-byte limit");
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

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
enum Control {
    Hello,
    Load {
        model: OnnxModelSpec,
    },
    Transcribe,
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
    Ready,
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
            Self::Hello
                | Self::Load { .. }
                | Self::Transcribe
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
            Self::Ready
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

trait WorkerProcess: Send + Sync {
    fn is_running(&self) -> Result<bool>;
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

struct OsWorkerLauncher;

impl WorkerLauncher for OsWorkerLauncher {
    fn launch(&self) -> Result<SpawnedWorker> {
        let executable = std::env::current_exe()?;
        let mut child = Command::new(executable)
            .arg("--onnx-worker")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("ONNX worker stdin unavailable"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("ONNX worker stdout unavailable"))?;
        Ok(SpawnedWorker {
            stdin: Box::new(stdin),
            stdout: Box::new(stdout),
            process: Arc::new(OsWorkerProcess {
                child: Mutex::new(child),
            }),
        })
    }
}

struct OsWorkerProcess {
    child: Mutex<Child>,
}

impl WorkerProcess for OsWorkerProcess {
    fn is_running(&self) -> Result<bool> {
        Ok(self
            .child
            .lock()
            .map_err(|_| anyhow!("ONNX worker process lock was poisoned"))?
            .try_wait()?
            .is_none())
    }

    fn terminate(&self) -> Result<()> {
        let mut child = self
            .child
            .lock()
            .map_err(|_| anyhow!("ONNX worker process lock was poisoned"))?;
        if child.try_wait()?.is_none()
            && let Err(kill_error) = child.kill()
            && child.try_wait()?.is_none()
        {
            return Err(anyhow!("could not terminate ONNX worker: {kill_error}"));
        }
        Ok(())
    }

    fn wait(&self) -> Result<()> {
        let _ = self
            .child
            .lock()
            .map_err(|_| anyhow!("ONNX worker process lock was poisoned"))?
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
pub(crate) struct OnnxWorkerSupervisor {
    inner: Arc<SupervisorInner>,
}

impl OnnxWorkerSupervisor {
    pub(crate) fn spawn() -> Result<Self> {
        Self::with_launcher(Arc::new(OsWorkerLauncher))
    }

    fn with_launcher(launcher: Arc<dyn WorkerLauncher>) -> Result<Self> {
        Self::with_launcher_and_deadlines(launcher, SupervisorDeadlines::default())
    }

    fn with_launcher_and_deadlines(
        launcher: Arc<dyn WorkerLauncher>,
        deadlines: SupervisorDeadlines,
    ) -> Result<Self> {
        let supervisor = Self {
            inner: Arc::new(SupervisorInner {
                launcher,
                deadlines,
                spawn_gate: Mutex::new(()),
                retirement_changed: Condvar::new(),
                state: Mutex::new(SupervisorState::default()),
                writer: Mutex::new(None),
                pending: Mutex::new(HashMap::new()),
            }),
        };
        supervisor.ensure_generation()?;
        Ok(supervisor)
    }

    pub(crate) fn load(
        &self,
        session_id: u64,
        request_id: u64,
        model: OnnxModelSpec,
    ) -> Result<bool> {
        let identity = model.fingerprint()?;
        let generation = self.ensure_generation()?;
        let reused = self
            .inner
            .state
            .lock()
            .map_err(|_| anyhow!("ONNX supervisor state lock was poisoned"))?
            .active_model
            .as_ref()
            .is_some_and(|(loaded_generation, loaded_identity)| {
                *loaded_generation == generation && loaded_identity == &identity
            });
        if reused {
            return Ok(true);
        }
        let load = control_frame(session_id, request_id, &Control::Load { model })?;
        if let Err(error) = self
            .active_round_trip_with_timeout(
                generation,
                session_id,
                request_id,
                &[load],
                self.inner.deadlines.load,
            )
            .and_then(expect_ok)
        {
            self.invalidate_generation(generation, "ONNX model load failed", true)?;
            return Err(error);
        }
        let mut state = self
            .inner
            .state
            .lock()
            .map_err(|_| anyhow!("ONNX supervisor state lock was poisoned"))?;
        if state
            .current
            .as_ref()
            .is_some_and(|current| current.generation == generation)
        {
            state.active_model = Some((generation, identity));
        }
        Ok(false)
    }

    pub(crate) fn transcribe(
        &self,
        session_id: u64,
        request_id: u64,
        samples: &[f32],
    ) -> Result<String> {
        let pcm = encode_pcm(samples)?;
        let generation = self.ensure_generation()?;
        let control = control_frame(session_id, request_id, &Control::Transcribe)?;
        let frames = [
            control,
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
                final_result: true,
            } => Ok(text),
            Control::Error { message } => bail!("ONNX worker: {message}"),
            _ => {
                self.invalidate_generation(
                    generation,
                    "unexpected ONNX worker transcription response",
                    true,
                )?;
                bail!("unexpected ONNX worker transcription response")
            }
        }
    }

    pub(crate) fn start_stream(&self, session_id: u64, request_id: u64) -> Result<()> {
        let generation = self.ensure_generation()?;
        if self
            .inner
            .state
            .lock()
            .map_err(|_| anyhow!("ONNX supervisor state lock was poisoned"))?
            .active_stream
            .is_some()
        {
            bail!("an ONNX stream is already active");
        }
        let frame = control_frame(session_id, request_id, &Control::StartStream)?;
        match self.active_round_trip(generation, session_id, request_id, &[frame])? {
            Control::Ok => {
                let mut state = self
                    .inner
                    .state
                    .lock()
                    .map_err(|_| anyhow!("ONNX supervisor state lock was poisoned"))?;
                if state
                    .current
                    .as_ref()
                    .is_none_or(|current| current.generation != generation)
                {
                    bail!("ONNX worker generation {generation} is unavailable");
                }
                state.active_stream = Some(SupervisorStream {
                    generation,
                    session_id,
                    last_request_id: request_id,
                });
                Ok(())
            }
            Control::Error { message } => bail!("ONNX worker: {message}"),
            _ => {
                self.invalidate_generation(
                    generation,
                    "unexpected ONNX worker start-stream response",
                    true,
                )?;
                bail!("unexpected ONNX worker start-stream response")
            }
        }
    }

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
                bail!("ONNX worker: {message}")
            }
            _ => {
                self.invalidate_generation(
                    generation,
                    "unexpected ONNX worker audio-chunk response",
                    true,
                )?;
                bail!("unexpected ONNX worker audio-chunk response")
            }
        }
    }

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
            Control::Error { message } => bail!("ONNX worker: {message}"),
            _ => {
                self.invalidate_generation(
                    generation,
                    "unexpected ONNX worker end-stream response",
                    true,
                )?;
                bail!("unexpected ONNX worker end-stream response")
            }
        }
    }

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
                .map_err(|_| anyhow!("ONNX supervisor state lock was poisoned"))?;
            let stream = state
                .active_stream
                .filter(|stream| stream.session_id == session_id)
                .ok_or_else(|| anyhow!("no ONNX stream is active for session {session_id}"))?;
            (stream, state.active_request)
        };
        if let Some(active_request) = active_request {
            if active_request.generation == stream.generation
                && active_request.session_id == stream.session_id
            {
                return self.cancel_active();
            }
            bail!("another ONNX request is active");
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
        let generation = self.ensure_generation()?;
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
            .map_err(|_| anyhow!("ONNX supervisor state lock was poisoned"))?;
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

    /// Cancels without taking the request waiter's or stdin writer's lock. The
    /// worker currently reads controls and performs native decode on the same
    /// thread, so a cooperative Cancel cannot be observed while decode is
    /// blocked. The supervisor therefore fails the exact active correlation,
    /// invalidates its generation, and starts an owned OS kill/wait reaper
    /// immediately. Cancellation is independent of blocked pipe I/O, and stale
    /// output cannot reach a later generation.
    pub(crate) fn cancel_active(&self) -> Result<()> {
        let target = {
            let mut state = self
                .inner
                .state
                .lock()
                .map_err(|_| anyhow!("ONNX supervisor state lock was poisoned"))?;
            let Some(target) = state.active_request else {
                return Ok(());
            };
            let Some(current) = state.current.as_ref() else {
                return Ok(());
            };
            if current.generation != target.generation {
                return Ok(());
            }
            state.active_request = None;
            target
        };

        let waiter = self
            .inner
            .pending
            .lock()
            .map_err(|_| anyhow!("ONNX pending map lock was poisoned"))?
            .remove(&target);
        if let Some(waiter) = waiter {
            let _ = waiter.send(Err("ONNX transcription request was cancelled".to_owned()));
        } else {
            // The stdout reader already claimed the response. Treat that as
            // completion winning the race rather than killing a healthy
            // generation after its request has completed.
            return Ok(());
        }
        self.invalidate_generation(target.generation, "ONNX request cancelled", true)?;
        Ok(())
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
            .map_err(|_| anyhow!("ONNX supervisor state lock was poisoned"))?
            .current
            .as_ref()
            .map(|current| current.generation);
        if let Some(generation) = generation {
            self.invalidate_generation(
                generation,
                "ONNX worker generation was explicitly retired",
                true,
            )?;
        }
        Ok(())
    }

    fn ensure_generation(&self) -> Result<u64> {
        let _spawn_guard = self
            .inner
            .spawn_gate
            .lock()
            .map_err(|_| anyhow!("ONNX spawn lock was poisoned"))?;
        let existing = {
            let state = self
                .inner
                .state
                .lock()
                .map_err(|_| anyhow!("ONNX supervisor state lock was poisoned"))?;
            state
                .current
                .as_ref()
                .map(|current| (current.generation, Arc::clone(&current.process)))
        };
        if let Some((generation, process)) = existing {
            match process.is_running() {
                Ok(true) => return Ok(generation),
                Ok(false) => {
                    self.invalidate_generation(generation, "ONNX worker exited", false)?;
                }
                Err(error) => {
                    self.invalidate_generation(
                        generation,
                        "could not inspect ONNX worker process",
                        true,
                    )?;
                    return Err(anyhow!("could not inspect ONNX worker process: {error}"));
                }
            }
        }

        let SpawnedWorker {
            stdin,
            stdout,
            process,
        } = self.inner.launcher.launch()?;
        let generation = {
            let mut state = self
                .inner
                .state
                .lock()
                .map_err(|_| anyhow!("ONNX supervisor state lock was poisoned"))?;
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
            .map_err(|_| anyhow!("ONNX writer lock was poisoned"))? =
            Some(WriterSlot { generation, stdin });
        if let Err(error) = Self::start_reader(&self.inner, generation, stdout) {
            self.invalidate_generation(generation, &error.to_string(), true)?;
            return Err(error);
        }
        if let Err(error) = self.round_trip_on_generation(
            generation,
            0,
            0,
            Control::Hello,
            self.inner.deadlines.hello,
        ) {
            self.invalidate_generation(generation, &error.to_string(), true)?;
            return Err(error);
        }
        Ok(generation)
    }

    fn start_reader(
        inner: &Arc<SupervisorInner>,
        generation: u64,
        mut stdout: Box<dyn Read + Send>,
    ) -> Result<()> {
        let weak = Arc::downgrade(inner);
        std::thread::Builder::new()
            .name(format!("scribe-onnx-reader-{generation}"))
            .spawn(move || {
                loop {
                    let response = read_frame(&mut stdout).and_then(parse_worker_control);
                    let Some(inner) = Weak::upgrade(&weak) else {
                        break;
                    };
                    let (session_id, request_id, control) = match response {
                        Ok(response) => response,
                        Err(error) => {
                            if let Err(retire_error) = OnnxWorkerSupervisor::from_inner(inner)
                                .invalidate_generation(
                                generation,
                                &format!("ONNX worker stdout failed: {error}"),
                                true,
                            ) {
                                eprintln!(
                                    "could not retire failed ONNX worker generation {generation}: {retire_error:#}"
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
                    if let Err(error) = OnnxWorkerSupervisor::from_inner(inner)
                        .invalidate_generation(
                        generation,
                        "stale or mis-correlated ONNX worker response",
                        true,
                    ) {
                        eprintln!(
                            "could not retire mis-correlated ONNX worker generation {generation}: {error:#}"
                        );
                    }
                    break;
                }
            })
            .map(|_| ())
            .map_err(|error| anyhow!("could not start ONNX worker stdout reader: {error}"))
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
            .map_err(|_| anyhow!("ONNX pending map lock was poisoned"))?;
        match pending.entry(correlation) {
            Entry::Vacant(entry) => {
                entry.insert(reply);
            }
            Entry::Occupied(_) => {
                bail!(
                    "duplicate ONNX request correlation for generation {}, session {}, request {}",
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
                bail!("ONNX supervisor state lock was poisoned");
            }
        };
        if !current {
            self.unregister(correlation);
            bail!(
                "ONNX worker generation {} is unavailable",
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
                    Some(anyhow!("an ONNX transcription request is already active"))
                } else {
                    state.active_request = Some(correlation);
                    None
                }
            }
            Ok(_) => Some(anyhow!(
                "ONNX worker generation {generation} is unavailable"
            )),
            Err(_) => Some(anyhow!("ONNX supervisor state lock was poisoned")),
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
        let result = self.await_response(correlation, response, timeout);
        self.clear_active(correlation);
        result
    }

    fn require_stream(&self, session_id: u64) -> Result<SupervisorStream> {
        self.inner
            .state
            .lock()
            .map_err(|_| anyhow!("ONNX supervisor state lock was poisoned"))?
            .active_stream
            .filter(|stream| stream.session_id == session_id)
            .ok_or_else(|| anyhow!("no ONNX stream is active for session {session_id}"))
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
        if !command.is_parent_command() {
            bail!("cannot send a response-only control to the ONNX worker");
        }
        let expects_ready = matches!(command, Control::Hello);
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
        match self.await_response(correlation, response, timeout)? {
            Control::Ready if expects_ready => Ok(()),
            Control::Ok if !expects_ready => Ok(()),
            Control::Error { message } => bail!("ONNX worker: {message}"),
            _ => {
                self.invalidate_generation(
                    generation,
                    "unexpected ONNX worker control response",
                    true,
                )?;
                bail!("unexpected ONNX worker control response")
            }
        }
    }

    fn write_frames(&self, generation: u64, frames: &[Frame]) -> Result<()> {
        let mut writer = self
            .inner
            .writer
            .lock()
            .map_err(|_| anyhow!("ONNX writer lock was poisoned"))?;
        let slot = writer
            .as_mut()
            .filter(|slot| slot.generation == generation)
            .ok_or_else(|| anyhow!("ONNX worker generation {generation} is unavailable"))?;
        for frame in frames {
            write_frame(&mut slot.stdin, frame)?;
        }
        Ok(())
    }

    fn await_response(
        &self,
        correlation: Correlation,
        response: Receiver<PendingResult>,
        timeout: Duration,
    ) -> Result<Control> {
        match response.recv_timeout(timeout) {
            Ok(result) => result.map_err(anyhow::Error::msg),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                self.unregister(correlation);
                self.invalidate_generation(
                    correlation.generation,
                    "ONNX worker response deadline exceeded",
                    true,
                )?;
                bail!(
                    "ONNX worker response deadline exceeded after {} ms",
                    timeout.as_millis()
                )
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                bail!("ONNX worker response channel disconnected")
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
                .map_err(|_| anyhow!("ONNX supervisor state lock was poisoned"))?;
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
                    .map_err(|_| anyhow!("ONNX supervisor state lock was poisoned"))?;
                state = next_state;
                if timeout.timed_out() && state.retiring_generations.contains(&generation) {
                    bail!(
                        "ONNX worker generation {generation} termination did not complete within {} ms",
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
                format!("could not initiate termination of ONNX worker generation {generation}")
            });
        }
        {
            let mut state = self
                .inner
                .state
                .lock()
                .map_err(|_| anyhow!("ONNX supervisor state lock was poisoned"))?;
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
#[allow(dead_code)]
pub(crate) struct SileroVadWorkerSupervisor {
    transport: OnnxWorkerSupervisor,
    deadlines: VadDeadlines,
}

#[allow(dead_code)]
impl SileroVadWorkerSupervisor {
    pub(crate) fn spawn() -> Result<Self> {
        let deadlines = VadDeadlines::default();
        let transport_deadlines = SupervisorDeadlines {
            hello: deadlines.startup,
            ..SupervisorDeadlines::default()
        };
        Ok(Self {
            transport: OnnxWorkerSupervisor::with_launcher_and_deadlines(
                Arc::new(OsWorkerLauncher),
                transport_deadlines,
            )?,
            deadlines,
        })
    }

    #[cfg(test)]
    fn with_transport(transport: OnnxWorkerSupervisor) -> Self {
        Self {
            transport,
            deadlines: VadDeadlines::default(),
        }
    }

    #[cfg(test)]
    fn with_transport_and_deadlines(
        transport: OnnxWorkerSupervisor,
        deadlines: VadDeadlines,
    ) -> Self {
        Self {
            transport,
            deadlines,
        }
    }

    pub(crate) fn load(&self, session_id: u64, request_id: u64, num_threads: u16) -> Result<bool> {
        if num_threads == 0 || num_threads > 64 {
            bail!("Silero VAD thread count must be within [1, 64]");
        }
        let identity = format!(
            "silero-vad:{}:{num_threads}",
            crate::support_assets::SILERO_VAD_SHA256
        );
        let generation = self.transport.ensure_generation()?;
        let reused = self
            .transport
            .inner
            .state
            .lock()
            .map_err(|_| anyhow!("ONNX supervisor state lock was poisoned"))?
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
            .active_round_trip_with_timeout(
                generation,
                session_id,
                request_id,
                &[frame],
                self.deadlines.startup,
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
            .map_err(|_| anyhow!("ONNX supervisor state lock was poisoned"))?;
        if state
            .current
            .as_ref()
            .is_some_and(|current| current.generation == generation)
        {
            state.active_model = Some((generation, identity));
        }
        Ok(false)
    }

    pub(crate) fn start_session(
        &self,
        session_id: u64,
        request_id: u64,
        threshold: VadThreshold,
    ) -> Result<()> {
        let generation = self.transport.ensure_generation()?;
        if self
            .transport
            .inner
            .state
            .lock()
            .map_err(|_| anyhow!("ONNX supervisor state lock was poisoned"))?
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
        let response = match self.transport.active_round_trip_with_timeout(
            generation,
            session_id,
            request_id,
            &[frame],
            self.deadlines.startup,
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
                let mut state = self
                    .transport
                    .inner
                    .state
                    .lock()
                    .map_err(|_| anyhow!("ONNX supervisor state lock was poisoned"))?;
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

    pub(crate) fn compute(
        &self,
        session_id: u64,
        request_id: u64,
        samples: &[f32],
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
        let response = match self.transport.active_round_trip_with_timeout(
            generation,
            session_id,
            request_id,
            &frames,
            self.deadlines.operation,
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

    pub(crate) fn health(&self, session_id: u64, request_id: u64) -> Result<()> {
        let generation = self.transport.ensure_generation()?;
        let frame = control_frame(session_id, request_id, &Control::Health)?;
        let result = self
            .transport
            .active_round_trip_with_timeout(
                generation,
                session_id,
                request_id,
                &[frame],
                self.deadlines.startup,
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
        Control::Error { message } => bail!("ONNX worker: {message}"),
        _ => bail!("unexpected ONNX worker control response"),
    }
}

fn reap_process(process: Arc<dyn WorkerProcess>, generation: u64) -> Result<()> {
    // Termination, when needed, has already been initiated by the caller.
    // Waiting happens on a generation-local thread, so an indefinitely stalled
    // wait can never delay termination of a later generation.
    let reaper_process = Arc::clone(&process);
    if let Err(error) = std::thread::Builder::new()
        .name(format!("scribe-onnx-reaper-{generation}"))
        .spawn(move || {
            if let Err(error) = reaper_process.wait() {
                eprintln!("ONNX worker generation {generation} reaper failed: {error:#}");
            }
        })
    {
        // Thread creation failure is exceptional, but the child must still be
        // reaped before this process can safely forget it.
        process.wait().with_context(|| {
            format!(
                "could not start ONNX worker generation {generation} reaper ({error}) and synchronous reaping failed"
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
            eprintln!("ONNX worker shutdown failed: {error:#}");
        }
    }
}

/// Dispatches the private worker before any eframe initialization.
pub(crate) fn maybe_run_worker() -> Option<i32> {
    if !std::env::args_os()
        .skip(1)
        .any(|arg| arg == "--onnx-worker")
    {
        return None;
    }
    Some(
        match worker_loop(std::io::stdin().lock(), std::io::stdout().lock()) {
            Ok(()) => 0,
            Err(error) => {
                eprintln!("ONNX worker failed: {error:#}");
                1
            }
        },
    )
}

fn worker_loop(mut input: impl Read, mut output: impl Write) -> Result<()> {
    worker_loop_with_factories(
        &mut input,
        &mut output,
        &NativeRecognizerFactory,
        &NativeVadFactory,
    )
}

trait WorkerRecognizerFactory {
    type Recognizer: WorkerRecognizer;

    fn create(&self, model: &ValidatedOnnxModel) -> Result<Self::Recognizer>;
}

trait WorkerRecognizer {
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
    identity: String,
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
    worker_loop_with_factories(&mut input, &mut output, factory, &NativeVadFactory)
}

fn worker_loop_with_factories<F: WorkerRecognizerFactory, V: WorkerVadFactory>(
    mut input: impl Read,
    mut output: impl Write,
    factory: &F,
    vad_factory: &V,
) -> Result<()> {
    // This declaration order makes the stream drop before its recognizer on
    // structural protocol failure. Normal replacement paths clear it explicitly.
    let mut loaded: Option<LoadedWorkerRecognizer<F::Recognizer>> = None;
    let mut active_stream: Option<ActiveWorkerStream<<F::Recognizer as WorkerRecognizer>::Stream>> =
        None;
    let mut loaded_vad: Option<V::Vad> = None;
    let mut active_vad: Option<ActiveWorkerVad> = None;
    loop {
        let frame = read_frame(&mut input)?;
        let (session_id, request_id, control) = parse_parent_control(frame)?;
        match control {
            Control::Hello => {
                write_worker_response(&mut output, session_id, request_id, Control::Ready)?;
            }
            Control::Load { model } => {
                let result = (|| {
                    if loaded_vad.is_some() {
                        bail!("cannot load a transcription model in a VAD worker");
                    }
                    let model = model.validated()?;
                    let identity = model.fingerprint()?;
                    if loaded
                        .as_ref()
                        .is_some_and(|loaded| loaded.identity == identity)
                    {
                        return Ok(Control::Ok);
                    }
                    if active_stream.is_some() {
                        bail!("cannot replace an ONNX recognizer while a stream is active");
                    }
                    drop(loaded.take());
                    let recognizer = factory.create(&model)?;
                    loaded = Some(LoadedWorkerRecognizer {
                        identity,
                        family: model.family,
                        recognizer,
                    });
                    Ok(Control::Ok)
                })();
                write_worker_result(&mut output, session_id, request_id, result)?;
            }
            Control::Health => {
                write_worker_response(&mut output, session_id, request_id, Control::Ok)?;
            }
            Control::Unload => {
                active_stream = None;
                loaded = None;
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
                drop(active_stream.take());
                drop(loaded.take());
                drop(loaded_vad.take());
                write_worker_response(&mut output, session_id, request_id, Control::Ok)?;
                return Ok(());
            }
            Control::Transcribe => {
                let samples = read_correlated_pcm(&mut input, session_id, request_id);
                let result = samples.and_then(|samples| {
                    if active_stream.is_some() || loaded_vad.is_some() {
                        bail!("cannot run batch transcription while an ONNX stream is active");
                    }
                    let recognizer = loaded
                        .as_ref()
                        .ok_or_else(|| anyhow!("no ONNX model is loaded"))?;
                    recognizer
                        .recognizer
                        .transcribe(&samples)
                        .map(|text| Control::Text {
                            text,
                            final_result: true,
                        })
                });
                write_worker_result(&mut output, session_id, request_id, result)?;
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
                        bail!("an ONNX stream is already active");
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
            Control::Ready
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
    let stream = active_stream
        .as_mut()
        .ok_or_else(|| anyhow!("no ONNX stream is active"))?;
    if stream.session_id != session_id {
        bail!("ONNX stream belongs to a different session");
    }
    let Some(sample_count) = stream.sample_count.checked_add(samples.len()) else {
        *active_stream = None;
        bail!("ONNX stream sample count overflowed");
    };
    if sample_count > MAX_AUDIO_SAMPLES {
        *active_stream = None;
        bail!("ONNX stream exceeds the {MAX_AUDIO_BYTES}-byte cumulative limit");
    }
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
        bail!("no ONNX stream is active for session {session_id}");
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
    write_frame(output, &control_frame(session_id, request_id, &response)?)
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

/// Real sherpa recognizers stay entirely inside the child process so a native
/// failure cannot take down the eframe process.
struct NativeRecognizerFactory;

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

enum NativeRecognizer {
    Offline { recognizer: OfflineRecognizer },
    Online { recognizer: OnlineRecognizer },
}

impl WorkerRecognizerFactory for NativeRecognizerFactory {
    type Recognizer = NativeRecognizer;

    fn create(&self, model: &ValidatedOnnxModel) -> Result<Self::Recognizer> {
        if model.family == OnnxModelFamily::OnlineTransducer {
            let config = online_recognizer_config(model)?;
            return OnlineRecognizer::create(&config)
                .map(|recognizer| NativeRecognizer::Online { recognizer })
                .ok_or_else(|| anyhow!("sherpa-onnx failed to create CPU online recognizer"));
        }

        let config = offline_recognizer_config(model)?;
        OfflineRecognizer::create(&config)
            .map(|recognizer| NativeRecognizer::Offline { recognizer })
            .ok_or_else(|| anyhow!("sherpa-onnx failed to create CPU offline recognizer"))
    }
}

impl WorkerRecognizer for NativeRecognizer {
    type Stream = OnlineStream;

    fn transcribe(&self, samples: &[f32]) -> Result<String> {
        validate_pcm_samples(samples)?;
        match self {
            Self::Offline { recognizer } => {
                let stream = recognizer.create_stream();
                stream.accept_waveform(16_000, samples);
                recognizer.decode(&stream);
                stream
                    .get_result()
                    .map(|result| result.text)
                    .ok_or_else(|| anyhow!("sherpa-onnx offline recognizer returned no result"))
            }
            Self::Online { recognizer } => {
                let stream = recognizer.create_stream();
                stream.accept_waveform(16_000, samples);
                stream.input_finished();
                decode_online_ready(recognizer, &stream);
                Ok(recognizer
                    .get_result(&stream)
                    .map(|result| result.text)
                    .unwrap_or_default())
            }
        }
    }

    fn start_stream(&self) -> Result<Self::Stream> {
        match self {
            Self::Online { recognizer } => Ok(recognizer.create_stream()),
            Self::Offline { .. } => bail!("streaming requires an online ONNX transducer"),
        }
    }

    fn accept_chunk(&self, stream: &mut Self::Stream, samples: &[f32]) -> Result<()> {
        validate_pcm_samples(samples)?;
        let Self::Online { .. } = self else {
            bail!("streaming requires an online ONNX transducer");
        };
        stream.accept_waveform(16_000, samples);
        Ok(())
    }

    fn input_finished(&self, stream: &mut Self::Stream) -> Result<()> {
        let Self::Online { .. } = self else {
            bail!("streaming requires an online ONNX transducer");
        };
        stream.input_finished();
        Ok(())
    }

    fn drain_ready(&self, stream: &mut Self::Stream) -> Result<()> {
        let Self::Online { recognizer } = self else {
            bail!("streaming requires an online ONNX transducer");
        };
        decode_online_ready(recognizer, stream);
        Ok(())
    }

    fn stream_result(&self, stream: &Self::Stream) -> Result<String> {
        let Self::Online { recognizer } = self else {
            bail!("streaming requires an online ONNX transducer");
        };
        Ok(recognizer
            .get_result(stream)
            .map(|result| result.text)
            .unwrap_or_default())
    }
}

fn decode_online_ready(recognizer: &OnlineRecognizer, stream: &OnlineStream) {
    while recognizer.is_ready(stream) {
        recognizer.decode(stream);
    }
}

fn online_recognizer_config(model: &ValidatedOnnxModel) -> Result<OnlineRecognizerConfig> {
    let mut config = OnlineRecognizerConfig::default();
    config.model_config.provider = Some("cpu".into());
    config.model_config.num_threads = i32::from(model.num_threads);
    config.model_config.tokens = Some(model.path(OnnxFileRole::Tokens)?);
    config.model_config.transducer = OnlineTransducerModelConfig {
        encoder: Some(model.path(OnnxFileRole::Encoder)?),
        decoder: Some(model.path(OnnxFileRole::Decoder)?),
        joiner: Some(model.path(OnnxFileRole::Joiner)?),
    };
    Ok(config)
}

fn offline_recognizer_config(model: &ValidatedOnnxModel) -> Result<OfflineRecognizerConfig> {
    let mut config = OfflineRecognizerConfig::default();
    config.model_config.provider = Some("cpu".into());
    config.model_config.num_threads = i32::from(model.num_threads);
    config.model_config.tokens = Some(model.path(OnnxFileRole::Tokens)?);
    match model.family {
        OnnxModelFamily::Moonshine => {
            config.model_config.moonshine = OfflineMoonshineModelConfig {
                preprocessor: model
                    .files
                    .contains_key(&OnnxFileRole::Preprocessor)
                    .then(|| model.path(OnnxFileRole::Preprocessor))
                    .transpose()?,
                encoder: Some(model.path(OnnxFileRole::Encoder)?),
                uncached_decoder: model
                    .files
                    .contains_key(&OnnxFileRole::UncachedDecoder)
                    .then(|| model.path(OnnxFileRole::UncachedDecoder))
                    .transpose()?,
                cached_decoder: model
                    .files
                    .contains_key(&OnnxFileRole::CachedDecoder)
                    .then(|| model.path(OnnxFileRole::CachedDecoder))
                    .transpose()?,
                merged_decoder: model
                    .files
                    .contains_key(&OnnxFileRole::MergedDecoder)
                    .then(|| model.path(OnnxFileRole::MergedDecoder))
                    .transpose()?,
            }
        }
        OnnxModelFamily::NemoCtc => {
            config.model_config.nemo_ctc = OfflineNemoEncDecCtcModelConfig {
                model: Some(model.path(OnnxFileRole::Model)?),
            }
        }
        OnnxModelFamily::Canary => {
            config.model_config.canary = OfflineCanaryModelConfig {
                encoder: Some(model.path(OnnxFileRole::Encoder)?),
                decoder: Some(model.path(OnnxFileRole::Decoder)?),
                src_lang: Some("en".into()),
                tgt_lang: Some("en".into()),
                use_pnc: true,
            }
        }
        OnnxModelFamily::OfflineTransducer => {
            config.model_config.transducer = OfflineTransducerModelConfig {
                encoder: Some(model.path(OnnxFileRole::Encoder)?),
                decoder: Some(model.path(OnnxFileRole::Decoder)?),
                joiner: Some(model.path(OnnxFileRole::Joiner)?),
            };
            config.model_config.model_type = Some("nemo_transducer".into());
        }
        OnnxModelFamily::OnlineTransducer => {
            bail!("online transducers require the online recognizer")
        }
    }
    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;
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

    enum TestMode {
        Normal,
        BlockedDecode {
            started: TestSender<()>,
        },
        BlockedStreamOperation {
            end_stream: bool,
            started: TestSender<()>,
        },
        HoldOne {
            started: TestSender<()>,
            release: TestReceiver<()>,
        },
        HoldLoad {
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
        MalformedVadWindow,
        FailChangedLoad,
        FailChangedLoadWithActiveStream,
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
        write_frame(
            output,
            &control_frame(session_id, request_id, &control).unwrap(),
        )
        .unwrap();
    }

    fn handshake(input: &mut impl Read, output: &mut impl Write) {
        let (session_id, request_id, control) = read_parent_control(input);
        assert!(matches!(control, Control::Hello));
        respond(output, session_id, request_id, Control::Ready);
    }

    fn run_normal_worker(input: &mut impl Read, output: &mut impl Write) {
        loop {
            let Ok(frame) = read_frame(input) else {
                return;
            };
            let (session_id, request_id, control) = parse_parent_control(frame).unwrap();
            match control {
                Control::Transcribe => {
                    let pcm = read_frame(input).unwrap();
                    assert_eq!(pcm.kind, FrameKind::Pcm);
                    assert_eq!((pcm.session_id, pcm.request_id), (session_id, request_id));
                    respond(
                        output,
                        session_id,
                        request_id,
                        Control::Text {
                            text: "test transcript".to_owned(),
                            final_result: true,
                        },
                    );
                }
                Control::Shutdown => {
                    respond(output, session_id, request_id, Control::Ok);
                    return;
                }
                Control::Hello
                | Control::Load { .. }
                | Control::Cancel { .. }
                | Control::Unload
                | Control::Health => respond(output, session_id, request_id, Control::Ok),
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
                Control::Ready
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
        handshake(&mut input, &mut output);
        match mode {
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
            TestMode::MalformedVadWindow => {
                run_malformed_vad_window_worker(&mut input, &mut output)
            }
            TestMode::BlockedDecode { started } => {
                let (session_id, request_id, control) = read_parent_control(&mut input);
                assert!(matches!(control, Control::Transcribe));
                let pcm = read_frame(&mut input).unwrap();
                assert_eq!((pcm.session_id, pcm.request_id), (session_id, request_id));
                started.send(()).unwrap();
                let _ = read_frame(&mut input);
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
            TestMode::HoldOne { started, release } => {
                let (session_id, request_id, control) = read_parent_control(&mut input);
                assert!(matches!(control, Control::Health));
                started.send(()).unwrap();
                release.recv().unwrap();
                respond(&mut output, session_id, request_id, Control::Ok);
                run_normal_worker(&mut input, &mut output);
            }
            TestMode::HoldLoad { started, release } => {
                let (session_id, request_id, control) = read_parent_control(&mut input);
                assert!(matches!(control, Control::Load { .. }));
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
            TestMode::FailChangedLoad => {
                let (session_id, request_id, control) = read_parent_control(&mut input);
                assert!(matches!(control, Control::Load { .. }));
                respond(&mut output, session_id, request_id, Control::Ok);
                let (session_id, request_id, control) = read_parent_control(&mut input);
                assert!(matches!(control, Control::Load { .. }));
                respond(
                    &mut output,
                    session_id,
                    request_id,
                    Control::Error {
                        message: "replacement failed".to_owned(),
                    },
                );
                let _ = read_frame(&mut input);
            }
            TestMode::FailChangedLoadWithActiveStream => {
                let (session_id, request_id, control) = read_parent_control(&mut input);
                assert!(matches!(control, Control::Load { .. }));
                respond(&mut output, session_id, request_id, Control::Ok);
                let (session_id, request_id, control) = read_parent_control(&mut input);
                assert!(matches!(control, Control::StartStream));
                respond(&mut output, session_id, request_id, Control::Ok);
                let (session_id, request_id, control) = read_parent_control(&mut input);
                assert!(matches!(control, Control::Load { .. }));
                respond(
                    &mut output,
                    session_id,
                    request_id,
                    Control::Error {
                        message: "cannot replace a model while a stream is active".to_owned(),
                    },
                );
                let _ = read_frame(&mut input);
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

    fn test_supervisor(launcher: Arc<TestLauncher>) -> OnnxWorkerSupervisor {
        OnnxWorkerSupervisor::with_launcher(launcher).unwrap()
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
            startup: Duration::from_millis(40),
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
            other => panic!("expected worker error, got {other:?}"),
        }
    }

    fn required_fixture_path(name: &str) -> PathBuf {
        PathBuf::from(std::env::var(name).unwrap_or_else(|_| {
            panic!("set {name} to the reviewed local fixture before running this ignored test")
        }))
    }

    fn fixture_audio() -> PreparedAudio {
        PreparedAudio::from_wav_path(required_fixture_path("SCRIBE_ONNX_AUDIO"))
            .expect("SCRIBE_ONNX_AUDIO must name a readable speech WAV fixture")
    }

    fn moonshine_fixture_spec() -> OnnxModelSpec {
        let root = required_fixture_path("SCRIBE_ONNX_MOONSHINE_ROOT");
        OnnxModelSpec {
            id: "moonshine-tiny-en-local-fixture".to_owned(),
            root,
            family: OnnxModelFamily::Moonshine,
            files: BTreeMap::from([
                (OnnxFileRole::Encoder, PathBuf::from("encoder_model.ort")),
                (
                    OnnxFileRole::MergedDecoder,
                    PathBuf::from("decoder_model_merged.ort"),
                ),
                (OnnxFileRole::Tokens, PathBuf::from("tokens.txt")),
            ]),
            num_threads: 1,
        }
    }

    fn zipformer_fixture_spec() -> OnnxModelSpec {
        let root = required_fixture_path("SCRIBE_ONNX_ZIPFORMER_ROOT");
        OnnxModelSpec {
            id: "zipformer-en-20m-local-fixture".to_owned(),
            root,
            family: OnnxModelFamily::OnlineTransducer,
            files: BTreeMap::from([
                (
                    OnnxFileRole::Encoder,
                    PathBuf::from("encoder-epoch-99-avg-1.int8.onnx"),
                ),
                (
                    OnnxFileRole::Decoder,
                    PathBuf::from("decoder-epoch-99-avg-1.int8.onnx"),
                ),
                (
                    OnnxFileRole::Joiner,
                    PathBuf::from("joiner-epoch-99-avg-1.int8.onnx"),
                ),
                (OnnxFileRole::Tokens, PathBuf::from("tokens.txt")),
            ]),
            num_threads: 1,
        }
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
        append_control(&mut input, 0, 1, Control::Hello);
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
        append_control(&mut input, 0, 1, Control::Hello);
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
        append_control(&mut input, 0, 1, Control::Hello);
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
    #[ignore = "requires SCRIBE_ONNX_MOONSHINE_ROOT and SCRIBE_ONNX_AUDIO; local fixtures only, never downloads"]
    fn native_moonshine_offline_fixture_uses_the_typed_bundle_contract() {
        let model = moonshine_fixture_spec();
        model.validate().unwrap();
        let audio = fixture_audio();
        let mut input = Vec::new();
        append_control(&mut input, 0, 1, Control::Hello);
        append_control(&mut input, 0, 2, Control::Health);
        append_control(&mut input, 1, 3, Control::Load { model });
        append_control(&mut input, 1, 4, Control::Transcribe);
        append_pcm(&mut input, 1, 4, &audio.samples);
        append_control(&mut input, 0, 5, Control::Shutdown);

        let responses = run_native_worker(input);
        assert!(matches!(responses[0].2, Control::Ready));
        assert!(matches!(responses[1].2, Control::Ok));
        assert!(matches!(responses[2].2, Control::Ok));
        assert!(matches!(
            responses[3].2,
            Control::Text {
                final_result: true,
                ..
            }
        ));
        assert!(matches!(responses[4].2, Control::Ok));
    }

    #[test]
    #[ignore = "requires SCRIBE_ONNX_ZIPFORMER_ROOT and SCRIBE_ONNX_AUDIO; local fixtures only, never downloads"]
    fn native_zipformer_fixture_uses_true_online_streaming() {
        let model = zipformer_fixture_spec();
        model.validate().unwrap();
        let audio = fixture_audio();
        let midpoint = audio.samples.len() / 2;
        assert!(midpoint > 0, "fixture must contain at least two samples");
        let mut input = Vec::new();
        append_control(&mut input, 0, 1, Control::Hello);
        append_control(&mut input, 2, 2, Control::Load { model });
        append_control(&mut input, 8, 3, Control::StartStream);
        append_control(&mut input, 8, 4, Control::AudioChunk);
        append_pcm(&mut input, 8, 4, &audio.samples[..midpoint]);
        append_control(&mut input, 8, 5, Control::AudioChunk);
        append_pcm(&mut input, 8, 5, &audio.samples[midpoint..]);
        append_control(&mut input, 8, 6, Control::EndStream);
        append_control(&mut input, 0, 7, Control::Shutdown);

        let responses = run_native_worker(input);
        assert!(matches!(responses[0].2, Control::Ready));
        assert!(matches!(responses[1].2, Control::Ok));
        assert!(matches!(responses[2].2, Control::Ok));
        assert!(matches!(
            responses[3].2,
            Control::Text {
                final_result: false,
                ..
            }
        ));
        assert!(matches!(
            responses[4].2,
            Control::Text {
                final_result: false,
                ..
            }
        ));
        assert!(matches!(
            responses[5].2,
            Control::Text {
                final_result: true,
                ..
            }
        ));
        assert!(matches!(responses[6].2, Control::Ok));
    }

    #[test]
    #[ignore = "requires SCRIBE_ONNX_WORKER_EXE to name a built Scribe executable; runs the hidden worker protocol without downloading"]
    fn hidden_worker_manual_protocol_smoke() {
        let executable = required_fixture_path("SCRIBE_ONNX_WORKER_EXE");
        let mut child = Command::new(executable)
            .arg("--onnx-worker")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("spawn hidden ONNX worker executable");
        let mut input = child.stdin.take().expect("worker stdin");
        let mut output = child.stdout.take().expect("worker stdout");
        for (request_id, control, expected) in [
            (1, Control::Hello, "ready"),
            (2, Control::Health, "ok"),
            (3, Control::LoadVad { num_threads: 1 }, "ok"),
            (4, Control::StartVad { threshold: 0.5 }, "ok"),
        ] {
            let session_id = if request_id >= 4 { 71 } else { 0 };
            write_frame(
                &mut input,
                &control_frame(session_id, request_id, &control).unwrap(),
            )
            .unwrap();
            let (_, received_request, response) =
                parse_worker_control(read_frame(&mut output).unwrap()).unwrap();
            assert_eq!(received_request, request_id);
            match expected {
                "ready" => assert!(matches!(response, Control::Ready)),
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
                &mut input,
                &control_frame(71, request_id, &Control::VadWindow).unwrap(),
            )
            .unwrap();
            write_frame(
                &mut input,
                &Frame {
                    kind: FrameKind::Pcm,
                    session_id: 71,
                    request_id,
                    body: encode_pcm(&window).unwrap(),
                },
            )
            .unwrap();
            let (_, received_request, response) =
                parse_worker_control(read_frame(&mut output).unwrap()).unwrap();
            assert_eq!(received_request, request_id);
            match response {
                Control::VadDecision { probability, .. } => probabilities.push(probability),
                other => panic!("expected native VAD decision, got {other:?}"),
            }
            if request_id == 5 {
                write_frame(
                    &mut input,
                    &control_frame(71, 6, &Control::ResetVad).unwrap(),
                )
                .unwrap();
                assert!(matches!(
                    parse_worker_control(read_frame(&mut output).unwrap())
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
                &mut input,
                &control_frame(session_id, request_id, &control).unwrap(),
            )
            .unwrap();
            assert!(matches!(
                parse_worker_control(read_frame(&mut output).unwrap())
                    .unwrap()
                    .2,
                Control::Ok
            ));
        }
        assert!(child.wait().unwrap().success());
    }

    #[test]
    fn framed_worker_reuses_loaded_offline_recognizer_and_replaces_on_change() {
        let root = test_root("worker-offline-reuse");
        let first = spec_with_roles(&root, OnnxModelFamily::OfflineTransducer, TRANSDUCER_ROLES);
        let first_id = first.id.clone();
        let mut second = first.clone();
        second.id = "second-offline".to_owned();
        let factory = FakeRecognizerFactory::new();
        let mut input = Vec::new();
        append_control(&mut input, 0, 0, Control::Hello);
        append_control(
            &mut input,
            1,
            1,
            Control::Load {
                model: first.clone(),
            },
        );
        append_control(
            &mut input,
            1,
            2,
            Control::Load {
                model: first.clone(),
            },
        );
        append_control(&mut input, 1, 3, Control::Transcribe);
        append_pcm(&mut input, 1, 3, &[0.1]);
        append_control(&mut input, 1, 4, Control::Transcribe);
        append_pcm(&mut input, 1, 4, &[-0.1]);
        append_control(
            &mut input,
            1,
            5,
            Control::Load {
                model: second.clone(),
            },
        );
        append_control(&mut input, 0, 6, Control::Unload);
        append_control(&mut input, 1, 7, Control::Load { model: first });
        append_control(&mut input, 0, 8, Control::Shutdown);

        let responses = run_framed_fake_worker(&factory, input);
        assert_eq!(responses.len(), 9);
        assert!(matches!(responses[0].2, Control::Ready));
        assert!(matches!(responses[1].2, Control::Ok));
        assert!(matches!(responses[2].2, Control::Ok));
        assert!(matches!(
            responses[3].2,
            Control::Text {
                final_result: true,
                ..
            }
        ));
        assert!(matches!(
            responses[4].2,
            Control::Text {
                final_result: true,
                ..
            }
        ));
        assert!(responses[3].0 == 1 && responses[3].1 == 3);
        assert!(responses[4].0 == 1 && responses[4].1 == 4);

        let state = factory.snapshot();
        assert_eq!(state.create_attempts, 3);
        assert_eq!(state.recognizers_created, 3);
        assert_eq!(state.transcriptions, 2);
        assert_eq!(state.recognizer_drops, 3);
        let create_second = state
            .events
            .iter()
            .position(|event| event == "create:second-offline")
            .unwrap();
        let drop_first = state
            .events
            .iter()
            .position(|event| event == &format!("drop-recognizer:{first_id}"))
            .unwrap();
        assert!(drop_first < create_second);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn failed_changed_load_drops_previous_recognizer_and_leaves_worker_cold() {
        let root = test_root("worker-failed-load");
        let first = spec_with_roles(&root, OnnxModelFamily::NemoCtc, NEMO_CTC_ROLES);
        let mut failing = first.clone();
        failing.id = "fail-replacement".to_owned();
        let factory = FakeRecognizerFactory::new();
        let mut input = Vec::new();
        append_control(&mut input, 0, 0, Control::Hello);
        append_control(
            &mut input,
            1,
            1,
            Control::Load {
                model: first.clone(),
            },
        );
        append_control(&mut input, 1, 2, Control::Load { model: failing });
        append_control(&mut input, 1, 3, Control::Transcribe);
        append_pcm(&mut input, 1, 3, &[0.2]);
        append_control(&mut input, 0, 4, Control::Shutdown);

        let responses = run_framed_fake_worker(&factory, input);
        assert_error(&responses[2], "construction failed");
        assert_error(&responses[3], "no ONNX model is loaded");
        let state = factory.snapshot();
        assert_eq!(state.create_attempts, 2);
        assert_eq!(state.recognizers_created, 1);
        assert_eq!(state.transcriptions, 0);
        assert_eq!(state.recognizer_drops, 1);
        let dropped = state
            .events
            .iter()
            .position(|event| event == &format!("drop-recognizer:{}", first.id))
            .unwrap();
        let attempted = state
            .events
            .iter()
            .position(|event| event == "create-attempt:fail-replacement")
            .unwrap();
        assert!(dropped < attempted);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn framed_worker_rejects_all_stream_commands_for_offline_models() {
        let root = test_root("worker-offline-stream");
        let model = spec_with_roles(&root, OnnxModelFamily::Moonshine, MOONSHINE_MERGED_ROLES);
        let factory = FakeRecognizerFactory::new();
        let mut input = Vec::new();
        append_control(&mut input, 0, 0, Control::Hello);
        append_control(&mut input, 1, 1, Control::Load { model });
        append_control(&mut input, 7, 2, Control::StartStream);
        append_control(&mut input, 7, 3, Control::AudioChunk);
        append_pcm(&mut input, 7, 3, &[0.1]);
        append_control(&mut input, 7, 4, Control::EndStream);
        append_control(&mut input, 0, 5, Control::Shutdown);

        let responses = run_framed_fake_worker(&factory, input);
        assert_error(&responses[2], "online ONNX transducer");
        assert_error(&responses[3], "online ONNX transducer");
        assert_error(&responses[4], "online ONNX transducer");
        let state = factory.snapshot();
        assert_eq!(state.offline_stream_backend_calls, 0);
        assert_eq!(state.stream_starts, 0);
        assert_eq!(state.chunks_accepted, 0);
        assert_eq!(state.input_finished, 0);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn framed_worker_runs_true_online_chunks_and_finalizes_once() {
        let root = test_root("worker-online-stream");
        let model = spec_with_roles(&root, OnnxModelFamily::OnlineTransducer, TRANSDUCER_ROLES);
        let factory = FakeRecognizerFactory::new();
        let mut input = Vec::new();
        append_control(&mut input, 0, 0, Control::Hello);
        append_control(&mut input, 2, 1, Control::Load { model });
        append_control(&mut input, 9, 2, Control::StartStream);
        append_control(&mut input, 9, 3, Control::AudioChunk);
        append_pcm(&mut input, 9, 3, &[0.1, 0.2]);
        append_control(&mut input, 9, 4, Control::AudioChunk);
        append_pcm(&mut input, 9, 4, &[-0.1]);
        append_control(&mut input, 9, 5, Control::EndStream);
        append_control(&mut input, 2, 6, Control::Transcribe);
        append_pcm(&mut input, 2, 6, &[0.3]);
        append_control(&mut input, 0, 7, Control::Shutdown);

        let responses = run_framed_fake_worker(&factory, input);
        assert!(matches!(responses[2].2, Control::Ok));
        for (response, expected) in [(&responses[3], "partial-1"), (&responses[4], "partial-2")] {
            match &response.2 {
                Control::Text {
                    text,
                    final_result: false,
                } => assert_eq!(text, expected),
                other => panic!("expected non-final partial, got {other:?}"),
            }
        }
        match &responses[5].2 {
            Control::Text {
                text,
                final_result: true,
            } => assert_eq!(text, "final-2"),
            other => panic!("expected final stream result, got {other:?}"),
        }
        assert!(matches!(
            responses[6].2,
            Control::Text {
                final_result: true,
                ..
            }
        ));
        let state = factory.snapshot();
        assert_eq!(state.stream_starts, 1);
        assert_eq!(state.chunks_accepted, 2);
        assert_eq!(state.input_finished, 1);
        assert_eq!(state.drains, 3);
        assert_eq!(state.result_reads, 3);
        assert_eq!(state.stream_drops, 1);
        assert_eq!(state.transcriptions, 1);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn framed_worker_rejects_bad_stream_order_without_corrupting_valid_stream() {
        let root = test_root("worker-stream-order");
        let model = spec_with_roles(&root, OnnxModelFamily::OnlineTransducer, TRANSDUCER_ROLES);
        let factory = FakeRecognizerFactory::new();
        let mut input = Vec::new();
        append_control(&mut input, 0, 0, Control::Hello);
        append_control(&mut input, 1, 1, Control::Load { model });
        append_control(&mut input, 10, 2, Control::AudioChunk);
        append_pcm(&mut input, 10, 2, &[0.1]);
        append_control(&mut input, 10, 3, Control::EndStream);
        append_control(&mut input, 10, 4, Control::StartStream);
        append_control(&mut input, 11, 5, Control::StartStream);
        append_control(&mut input, 11, 6, Control::AudioChunk);
        append_pcm(&mut input, 11, 6, &[0.1]);
        append_control(&mut input, 11, 7, Control::EndStream);
        append_control(
            &mut input,
            10,
            8,
            Control::Cancel {
                target_session_id: 10,
                target_request_id: 999,
            },
        );
        append_control(&mut input, 10, 9, Control::AudioChunk);
        append_pcm(&mut input, 10, 9, &[0.2]);
        append_control(&mut input, 10, 10, Control::EndStream);
        append_control(&mut input, 0, 11, Control::Shutdown);

        let responses = run_framed_fake_worker(&factory, input);
        assert_error(&responses[2], "no ONNX stream");
        assert_error(&responses[3], "no ONNX stream");
        assert!(matches!(responses[4].2, Control::Ok));
        assert_error(&responses[5], "already active");
        assert_error(&responses[6], "different session");
        assert_error(&responses[7], "session 11");
        assert_error(&responses[8], "no matching");
        assert!(matches!(
            responses[9].2,
            Control::Text {
                final_result: false,
                ..
            }
        ));
        assert!(matches!(
            responses[10].2,
            Control::Text {
                final_result: true,
                ..
            }
        ));
        let state = factory.snapshot();
        assert_eq!(state.stream_starts, 1);
        assert_eq!(state.chunks_accepted, 1);
        assert_eq!(state.input_finished, 1);
        assert_eq!(state.stream_drops, 1);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn framed_worker_cancel_matches_latest_stream_request_and_clears_stream() {
        let root = test_root("worker-stream-cancel");
        let model = spec_with_roles(&root, OnnxModelFamily::OnlineTransducer, TRANSDUCER_ROLES);
        let factory = FakeRecognizerFactory::new();
        let mut input = Vec::new();
        append_control(&mut input, 0, 0, Control::Hello);
        append_control(&mut input, 1, 1, Control::Load { model });
        append_control(&mut input, 12, 2, Control::StartStream);
        append_control(&mut input, 12, 3, Control::AudioChunk);
        append_pcm(&mut input, 12, 3, &[0.1]);
        append_control(
            &mut input,
            12,
            4,
            Control::Cancel {
                target_session_id: 12,
                target_request_id: 3,
            },
        );
        append_control(&mut input, 12, 5, Control::EndStream);
        append_control(&mut input, 0, 6, Control::Shutdown);

        let responses = run_framed_fake_worker(&factory, input);
        assert!(matches!(responses[4].2, Control::Ok));
        assert_error(&responses[5], "no ONNX stream");
        let state = factory.snapshot();
        assert_eq!(state.stream_drops, 1);
        assert_eq!(state.input_finished, 0);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn framed_worker_rejects_invalid_pcm_before_fake_backend() {
        let root = test_root("worker-invalid-pcm");
        let model = spec_with_roles(&root, OnnxModelFamily::OnlineTransducer, TRANSDUCER_ROLES);
        let factory = FakeRecognizerFactory::new();
        let mut input = Vec::new();
        append_control(&mut input, 0, 0, Control::Hello);
        append_control(&mut input, 1, 1, Control::Load { model });
        append_control(&mut input, 14, 2, Control::StartStream);
        append_control(&mut input, 14, 3, Control::AudioChunk);
        append_pcm(&mut input, 14, 3, &[f32::NAN]);
        append_control(&mut input, 1, 4, Control::Transcribe);
        append_pcm(&mut input, 1, 4, &[1.01]);
        append_control(&mut input, 0, 5, Control::Shutdown);

        let responses = run_framed_fake_worker(&factory, input);
        assert_error(&responses[3], "non-finite or outside");
        assert_error(&responses[4], "non-finite or outside");
        let state = factory.snapshot();
        assert_eq!(state.chunks_accepted, 0);
        assert_eq!(state.transcriptions, 0);
        assert_eq!(state.stream_drops, 1);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn unload_and_shutdown_drop_stream_before_owning_recognizer() {
        let root = test_root("worker-drop-order");
        let first = spec_with_roles(&root, OnnxModelFamily::OnlineTransducer, TRANSDUCER_ROLES);
        let first_id = first.id.clone();
        let mut second = first.clone();
        second.id = "second-online".to_owned();
        let factory = FakeRecognizerFactory::new();
        let mut input = Vec::new();
        append_control(&mut input, 0, 0, Control::Hello);
        append_control(&mut input, 1, 1, Control::Load { model: first });
        append_control(&mut input, 20, 2, Control::StartStream);
        append_control(&mut input, 0, 3, Control::Unload);
        append_control(&mut input, 1, 4, Control::Load { model: second });
        append_control(&mut input, 21, 5, Control::StartStream);
        append_control(&mut input, 0, 6, Control::Shutdown);

        let responses = run_framed_fake_worker(&factory, input);
        assert!(
            responses
                .iter()
                .all(|response| !matches!(response.2, Control::Error { .. }))
        );
        let state = factory.snapshot();
        assert_eq!(state.stream_drops, 2);
        assert_eq!(state.recognizer_drops, 2);
        let relevant = state
            .events
            .iter()
            .filter(|event| event.starts_with("drop-"))
            .cloned()
            .collect::<Vec<_>>();
        assert_eq!(
            relevant,
            vec![
                "drop-stream".to_owned(),
                format!("drop-recognizer:{first_id}"),
                "drop-stream".to_owned(),
                "drop-recognizer:second-online".to_owned(),
            ]
        );
        std::fs::remove_dir_all(root).unwrap();
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
        let transport =
            OnnxWorkerSupervisor::with_launcher_and_deadlines(launcher.clone(), short_deadlines())
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
        let transport =
            OnnxWorkerSupervisor::with_launcher_and_deadlines(launcher.clone(), short_deadlines())
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
        let transport =
            OnnxWorkerSupervisor::with_launcher_and_deadlines(launcher.clone(), short_deadlines())
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
        assert_eq!(vad.startup, Duration::from_secs(2));
        assert_eq!(vad.operation, Duration::from_millis(250));
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
    fn blocked_transcription_is_cancelled_within_250_milliseconds() {
        let (started_tx, started_rx) = channel();
        let (kill_started_tx, kill_started_rx) = channel();
        let (reaped_tx, reaped_rx) = channel();
        let launcher = Arc::new(
            TestLauncher::new([
                TestMode::BlockedDecode {
                    started: started_tx,
                },
                TestMode::Normal,
            ])
            .with_process_events(kill_started_tx, reaped_tx),
        );
        let supervisor = test_supervisor(launcher);
        let transcription_supervisor = supervisor.clone();
        let transcription = std::thread::spawn(move || {
            transcription_supervisor.transcribe(41, 91, &[0.0, 0.25, -0.25])
        });
        started_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        // Model a request thread blocked inside an OS pipe write: cancellation
        // must not wait for or attempt to take the stdin writer mutex.
        let writer_guard = supervisor.inner.writer.lock().unwrap();

        let cancel_started = Instant::now();
        supervisor.cancel_active().unwrap();
        let error = transcription.join().unwrap().unwrap_err();
        kill_started_rx
            .recv_timeout(Duration::from_millis(250))
            .unwrap();
        let remaining = Duration::from_millis(250).saturating_sub(cancel_started.elapsed());
        reaped_rx.recv_timeout(remaining).unwrap();
        let cancel_and_reap_duration = cancel_started.elapsed();

        assert!(
            cancel_and_reap_duration <= Duration::from_millis(250),
            "cancellation and reaping took {cancel_and_reap_duration:?}"
        );
        assert!(error.to_string().contains("cancelled"));
        drop(writer_guard);
        supervisor.health(42, 92).unwrap();
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
        let supervisor =
            OnnxWorkerSupervisor::with_launcher_and_deadlines(launcher.clone(), short_deadlines())
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
    fn hung_load_is_cancellable_and_cannot_deliver_a_stale_success() {
        let root = test_root("hung-load-cancel");
        let model = spec_with_roles(&root, OnnxModelFamily::NemoCtc, NEMO_CTC_ROLES);
        let (started_tx, started_rx) = channel();
        let (release_tx, release_rx) = channel();
        let launcher = Arc::new(TestLauncher::new([
            TestMode::HoldLoad {
                started: started_tx,
                release: release_rx,
            },
            TestMode::Normal,
        ]));
        let supervisor =
            OnnxWorkerSupervisor::with_launcher_and_deadlines(launcher.clone(), short_deadlines())
                .unwrap();
        let waiting = supervisor.clone();
        let first_model = model.clone();
        let request = std::thread::spawn(move || waiting.load(80, 81, first_model));
        started_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        let cancel_started = Instant::now();
        supervisor.cancel_active().unwrap();
        assert!(cancel_started.elapsed() < Duration::from_millis(250));
        assert!(
            request
                .join()
                .unwrap()
                .unwrap_err()
                .to_string()
                .contains("cancelled")
        );
        drop(release_tx);
        assert!(!supervisor.load(82, 83, model).unwrap());
        assert_eq!(launcher.launches.load(Ordering::Acquire), 2);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn hung_load_deadline_fails_closed_and_recovers_on_a_new_generation() {
        let root = test_root("hung-load-deadline");
        let model = spec_with_roles(&root, OnnxModelFamily::NemoCtc, NEMO_CTC_ROLES);
        let (started_tx, started_rx) = channel();
        let (release_tx, release_rx) = channel();
        let launcher = Arc::new(TestLauncher::new([
            TestMode::HoldLoad {
                started: started_tx,
                release: release_rx,
            },
            TestMode::Normal,
        ]));
        let supervisor =
            OnnxWorkerSupervisor::with_launcher_and_deadlines(launcher.clone(), short_deadlines())
                .unwrap();
        let waiting = supervisor.clone();
        let first_model = model.clone();
        let request = std::thread::spawn(move || waiting.load(84, 85, first_model));
        started_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        let error = request.join().unwrap().unwrap_err();
        assert!(error.to_string().contains("deadline exceeded"));
        drop(release_tx);
        assert!(!supervisor.load(86, 87, model).unwrap());
        assert_eq!(launcher.launches.load(Ordering::Acquire), 2);
        std::fs::remove_dir_all(root).unwrap();
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
            OnnxWorkerSupervisor::with_launcher_and_deadlines(launcher, short_deadlines()).unwrap();
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
    fn blocked_old_generation_reap_cannot_delay_new_generation_termination() {
        let (first_started_tx, first_started_rx) = channel();
        let (first_release_tx, first_release_rx) = channel();
        let (second_started_tx, second_started_rx) = channel();
        let (kill_started_tx, kill_started_rx) = channel();
        let (reaped_tx, reaped_rx) = channel();
        let launcher = Arc::new(
            TestLauncher::new([
                TestMode::HoldCancel {
                    started: first_started_tx,
                    release: first_release_rx,
                },
                TestMode::BlockedDecode {
                    started: second_started_tx,
                },
            ])
            .with_process_events(kill_started_tx, reaped_tx),
        );
        let supervisor =
            OnnxWorkerSupervisor::with_launcher_and_deadlines(launcher, short_deadlines()).unwrap();

        supervisor.start_stream(100, 1).unwrap();
        let first_cancel_supervisor = supervisor.clone();
        let first_cancel =
            std::thread::spawn(move || first_cancel_supervisor.cancel_stream(100, 2));
        first_started_rx
            .recv_timeout(Duration::from_secs(1))
            .unwrap();
        assert!(first_cancel.join().unwrap().is_err());
        kill_started_rx
            .recv_timeout(Duration::from_millis(250))
            .unwrap();

        let second_request_supervisor = supervisor.clone();
        let second_request =
            std::thread::spawn(move || second_request_supervisor.transcribe(101, 3, &[0.0, 0.1]));
        second_started_rx
            .recv_timeout(Duration::from_secs(1))
            .unwrap();
        let cancel_started = Instant::now();
        supervisor.cancel_active().unwrap();
        assert!(
            cancel_started.elapsed() <= Duration::from_millis(250),
            "second-generation cancellation took {:?}",
            cancel_started.elapsed()
        );
        kill_started_rx
            .recv_timeout(Duration::from_millis(250))
            .expect("the blocked first-generation wait must not serialize the second kill");
        assert!(second_request.join().unwrap().is_err());

        first_release_tx.send(()).unwrap();
        for _ in 0..2 {
            reaped_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        }
    }

    #[test]
    fn abandoning_stream_returns_without_waiting_for_worker_io() {
        let (kill_started_tx, kill_started_rx) = channel();
        let (reaped_tx, _reaped_rx) = channel();
        let (operation_started_tx, _operation_started_rx) = channel();
        let launcher = Arc::new(
            TestLauncher::new([TestMode::BlockedStreamOperation {
                end_stream: false,
                started: operation_started_tx,
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

    #[test]
    fn failed_changed_load_invalidates_cached_identity_and_recovers_on_clean_generation() {
        let root = test_root("supervisor-failed-changed-load");
        let first = spec_with_roles(&root, OnnxModelFamily::NemoCtc, NEMO_CTC_ROLES);
        let mut changed = first.clone();
        changed.id = "changed-model".to_owned();
        let (kill_started_tx, kill_started_rx) = channel();
        let (reaped_tx, reaped_rx) = channel();
        let launcher = Arc::new(
            TestLauncher::new([TestMode::FailChangedLoad, TestMode::Normal])
                .with_process_events(kill_started_tx, reaped_tx),
        );
        let supervisor = test_supervisor(Arc::clone(&launcher));

        assert!(!supervisor.load(1, 1, first.clone()).unwrap());
        let error = supervisor.load(2, 2, changed).unwrap_err();
        assert!(error.to_string().contains("replacement failed"));
        kill_started_rx
            .recv_timeout(Duration::from_secs(1))
            .unwrap();
        reaped_rx.recv_timeout(Duration::from_secs(1)).unwrap();

        assert!(!supervisor.load(3, 3, first.clone()).unwrap());
        assert!(supervisor.load(4, 4, first).unwrap());
        assert_eq!(launcher.launches.load(Ordering::Acquire), 2);

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn changed_load_during_stream_retires_generation_and_stale_stream_never_spawns_worker() {
        let root = test_root("supervisor-stream-replacement");
        let first = spec_with_roles(&root, OnnxModelFamily::NemoCtc, NEMO_CTC_ROLES);
        let mut changed = first.clone();
        changed.id = "changed-stream-model".to_owned();
        let (kill_started_tx, kill_started_rx) = channel();
        let (reaped_tx, _reaped_rx) = channel();
        let launcher = Arc::new(
            TestLauncher::new([TestMode::FailChangedLoadWithActiveStream, TestMode::Normal])
                .with_process_events(kill_started_tx, reaped_tx),
        );
        let supervisor = test_supervisor(Arc::clone(&launcher));

        assert!(!supervisor.load(110, 1, first).unwrap());
        supervisor.start_stream(111, 2).unwrap();
        let error = supervisor.load(112, 3, changed).unwrap_err();
        assert!(error.to_string().contains("stream is active"));
        kill_started_rx
            .recv_timeout(Duration::from_millis(250))
            .unwrap();
        assert_eq!(launcher.launches.load(Ordering::Acquire), 1);

        assert!(supervisor.audio_chunk(111, 4, &[0.1]).is_err());
        assert!(supervisor.end_stream(111, 5).is_err());
        assert_eq!(
            launcher.launches.load(Ordering::Acquire),
            1,
            "a stale stream must not create an empty replacement generation"
        );
        supervisor.health(113, 6).unwrap();
        assert_eq!(launcher.launches.load(Ordering::Acquire), 2);

        std::fs::remove_dir_all(root).unwrap();
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
                (MAX_AUDIO_BYTES + 1) as u32,
            ),
        ] {
            assert!(read_frame(&mut Cursor::new(bytes)).is_err());
        }
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
    fn recognizer_configs_use_validated_canonical_paths_and_fail_after_removal() {
        let root = test_root("validated-config-paths");
        let spec = spec_with_roles(&root, OnnxModelFamily::NemoCtc, NEMO_CTC_ROLES);
        let validated = spec.validated().unwrap();
        let canonical_model = std::fs::canonicalize(root.join("model.onnx")).unwrap();
        let config = offline_recognizer_config(&validated).unwrap();
        assert_eq!(
            config.model_config.nemo_ctc.model.as_deref(),
            Some(canonical_model.to_string_lossy().as_ref())
        );

        std::fs::remove_file(&canonical_model).unwrap();
        assert!(offline_recognizer_config(&validated).is_err());
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
        assert!(offline_recognizer_config(&validated).is_err());
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
        assert!(validate_pcm_samples(&vec![0.0; MAX_AUDIO_SAMPLES + 1]).is_err());

        let samples = [-1.0, -0.25, 0.0, 0.25, 1.0];
        let encoded = encode_pcm(&samples).unwrap();
        assert_eq!(decode_pcm(&encoded).unwrap(), samples);
    }

    #[test]
    fn cpu_only_acceleration_accepts_auto_and_cpu_but_rejects_gpu() {
        let auto = resolve_cpu_only_acceleration(AccelerationPreference::Auto).unwrap();
        assert_eq!(auto.resolved, ComputeDevice::Cpu);
        assert!(auto.diagnostic.is_some());

        let cpu = resolve_cpu_only_acceleration(AccelerationPreference::Cpu).unwrap();
        assert_eq!(cpu.resolved, ComputeDevice::Cpu);
        assert_eq!(cpu.diagnostic, None);

        let gpu = resolve_cpu_only_acceleration(AccelerationPreference::Gpu).unwrap_err();
        assert!(gpu.to_string().contains("CPU-only"));
        assert!(gpu.to_string().contains("Auto or CPU"));
    }
}
