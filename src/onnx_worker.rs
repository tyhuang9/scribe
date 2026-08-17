//! Private, process-isolated CPU-only sherpa-onnx execution substrate.
//!
//! This module deliberately has no catalog or UI entry point. A future typed
//! installer supplies a verified [`OnnxModelSpec`]; the router remains the only
//! component allowed to construct a worker client.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sherpa_onnx::{
    OfflineCanaryModelConfig, OfflineMoonshineModelConfig, OfflineNemoEncDecCtcModelConfig,
    OfflineRecognizer, OfflineRecognizerConfig, OfflineTransducerModelConfig, OnlineRecognizer,
    OnlineRecognizerConfig, OnlineTransducerModelConfig,
};

use crate::transcription::{AccelerationPreference, ComputeDevice, ResolvedAcceleration};

pub(crate) const PROTOCOL_MAGIC: [u8; 4] = *b"SCON";
pub(crate) const PROTOCOL_VERSION: u8 = 1;
const HEADER_LEN: usize = 26;
const MAX_CONTROL_BYTES: usize = 256 * 1024;
const MAX_AUDIO_BYTES: usize = 16 * 1024 * 1024;
const MAX_AUDIO_SAMPLES: usize = MAX_AUDIO_BYTES / size_of::<f32>();

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
        }
        Ok(())
    }

    fn fingerprint(&self) -> Result<String> {
        self.validate()?;
        Ok(format!("{:x}", Sha256::digest(serde_json::to_vec(self)?)))
    }

    fn path(&self, role: OnnxFileRole) -> Result<String> {
        self.files
            .get(&role)
            .map(|path| self.root.join(path).to_string_lossy().into_owned())
            .ok_or_else(|| anyhow!("ONNX model {} missing {role:?}", self.id))
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
    if body.len() % size_of::<f32>() != 0 {
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
    Load { model: OnnxModelSpec },
    Transcribe,
    StartStream,
    EndStream,
    Cancel,
    Unload,
    Health,
    Shutdown,
    Ready,
    Text { text: String, final_result: bool },
    Ok,
    Error { message: String },
}

impl Control {
    fn is_parent_command(&self) -> bool {
        matches!(
            self,
            Self::Hello
                | Self::Load { .. }
                | Self::Transcribe
                | Self::StartStream
                | Self::EndStream
                | Self::Cancel
                | Self::Unload
                | Self::Health
                | Self::Shutdown
        )
    }

    fn is_worker_response(&self) -> bool {
        matches!(
            self,
            Self::Ready | Self::Text { .. } | Self::Ok | Self::Error { .. }
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

/// Parent-side persistent worker client. It accepts only one active request and
/// drops a process on protocol failure, so stale responses cannot survive a
/// generation restart.
pub(crate) struct OnnxWorkerClient {
    child: Child,
    stdin: ChildStdin,
    stdout: ChildStdout,
    generation: u64,
    active_model: Option<String>,
}

impl OnnxWorkerClient {
    pub(crate) fn spawn() -> Result<Self> {
        let (child, stdin, stdout) = Self::spawn_process()?;
        let mut client = Self {
            child,
            stdin,
            stdout,
            generation: 1,
            active_model: None,
        };
        client.round_trip(0, 0, Control::Hello)?;
        Ok(client)
    }

    fn spawn_process() -> Result<(Child, ChildStdin, ChildStdout)> {
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
        Ok((child, stdin, stdout))
    }

    fn ensure_worker(&mut self) -> Result<()> {
        if self.child.try_wait()?.is_none() {
            return Ok(());
        }
        let (child, stdin, stdout) = Self::spawn_process()?;
        self.child = child;
        self.stdin = stdin;
        self.stdout = stdout;
        self.generation = self.generation.saturating_add(1);
        self.active_model = None;
        self.round_trip(0, 0, Control::Hello)
    }

    pub(crate) fn load(
        &mut self,
        session_id: u64,
        request_id: u64,
        model: OnnxModelSpec,
    ) -> Result<bool> {
        self.ensure_worker()?;
        let identity = model.fingerprint()?;
        let reused = self.active_model.as_deref() == Some(&identity);
        if !reused {
            self.round_trip(
                session_id,
                request_id,
                Control::Load {
                    model: model.clone(),
                },
            )?;
            self.active_model = Some(identity);
        }
        Ok(reused)
    }

    pub(crate) fn transcribe(
        &mut self,
        session_id: u64,
        request_id: u64,
        samples: &[f32],
    ) -> Result<String> {
        self.ensure_worker()?;
        self.send(session_id, request_id, Control::Transcribe)?;
        self.send_pcm(session_id, request_id, samples)?;
        self.await_text(session_id, request_id)
    }

    pub(crate) fn cancel(&mut self, session_id: u64, request_id: u64) -> Result<()> {
        // Native offline decode is not interruptible. The process boundary is
        // the cancellation mechanism: terminate without attempting a blocking
        // receive, then lazily respawn on the next use.
        self.kill();
        let _ = (session_id, request_id);
        Ok(())
    }

    pub(crate) fn unload(&mut self) -> Result<()> {
        self.ensure_worker()?;
        self.round_trip(0, 0, Control::Unload)?;
        self.active_model = None;
        Ok(())
    }
    fn send(&mut self, session_id: u64, request_id: u64, control: Control) -> Result<()> {
        if !control.is_parent_command() {
            bail!("cannot send a response-only control to the ONNX worker");
        }
        write_frame(
            &mut self.stdin,
            &control_frame(session_id, request_id, &control)?,
        )
    }
    fn send_pcm(&mut self, session_id: u64, request_id: u64, samples: &[f32]) -> Result<()> {
        let bytes = encode_pcm(samples)?;
        write_frame(
            &mut self.stdin,
            &Frame {
                kind: FrameKind::Pcm,
                session_id,
                request_id,
                body: bytes,
            },
        )
    }
    fn receive(&mut self) -> Result<(u64, u64, Control)> {
        parse_worker_control(read_frame(&mut self.stdout)?)
    }
    fn round_trip(&mut self, session_id: u64, request_id: u64, command: Control) -> Result<()> {
        self.send(session_id, request_id, command)?;
        let (actual_session, actual_request, response) = match self.receive() {
            Ok(response) => response,
            Err(error) => {
                self.kill();
                return Err(error);
            }
        };
        if actual_session != session_id || actual_request != request_id {
            self.kill();
            bail!(
                "stale or mis-correlated ONNX worker response from generation {}",
                self.generation
            );
        }
        match response {
            Control::Ok | Control::Ready => Ok(()),
            Control::Error { message } => bail!("ONNX worker: {message}"),
            _ => {
                self.kill();
                bail!("unexpected ONNX worker response")
            }
        }
    }
    fn await_text(&mut self, session_id: u64, request_id: u64) -> Result<String> {
        let (actual_session, actual_request, response) = match self.receive() {
            Ok(response) => response,
            Err(error) => {
                self.kill();
                return Err(error);
            }
        };
        if actual_session != session_id || actual_request != request_id {
            self.kill();
            bail!("stale ONNX worker response");
        }
        match response {
            Control::Text {
                text,
                final_result: true,
            } => Ok(text),
            Control::Error { message } => bail!("ONNX worker: {message}"),
            _ => {
                self.kill();
                bail!("unexpected ONNX worker transcription response")
            }
        }
    }
    fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        self.generation = self.generation.saturating_add(1);
        self.active_model = None;
    }
}

impl Drop for OnnxWorkerClient {
    fn drop(&mut self) {
        let _ = self.send(0, 0, Control::Shutdown);
        self.kill();
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
    let mut loaded: Option<OnnxModelSpec> = None;
    loop {
        let frame = read_frame(&mut input)?;
        let (session_id, request_id, control) = parse_parent_control(frame)?;
        match control {
            Control::Hello => write_frame(&mut output, &control_frame(session_id, request_id, &Control::Ready)?)?,
            Control::Load { model } => { model.validate()?; loaded = Some(model); write_frame(&mut output, &control_frame(session_id, request_id, &Control::Ok)?)?; }
            Control::Health => write_frame(&mut output, &control_frame(session_id, request_id, &Control::Ok)?)?,
            Control::Unload => { loaded = None; write_frame(&mut output, &control_frame(session_id, request_id, &Control::Ok)?)?; }
            Control::Cancel => write_frame(&mut output, &control_frame(session_id, request_id, &Control::Ok)?)?,
            Control::Shutdown => { write_frame(&mut output, &control_frame(session_id, request_id, &Control::Ok)?)?; return Ok(()); }
            Control::Transcribe => match loaded.as_ref() {
                None => write_frame(&mut output, &control_frame(session_id, request_id, &Control::Error { message: "no ONNX model is loaded".into() })?)?,
                Some(model) => {
                    let pcm = read_frame(&mut input)?;
                    if pcm.kind != FrameKind::Pcm || pcm.session_id != session_id || pcm.request_id != request_id {
                        bail!("invalid or mis-correlated ONNX PCM frame");
                    }
                    match decode_pcm(&pcm.body).and_then(|samples| native_transcribe(model, &samples)) {
                        Ok(text) => write_frame(&mut output, &control_frame(session_id, request_id, &Control::Text { text, final_result: true })?)?,
                        Err(error) => write_frame(&mut output, &control_frame(session_id, request_id, &Control::Error { message: error.to_string() })?)?,
                    }
                }
            },
            Control::StartStream | Control::EndStream => write_frame(&mut output, &control_frame(session_id, request_id, &Control::Error { message: "streaming lifecycle is unavailable until an online ONNX variant is admitted".into() })?)?,
            Control::Ready | Control::Text { .. } | Control::Ok | Control::Error { .. } => bail!("parent sent worker response"),
        }
    }
}

/// Creates real sherpa-onnx safe API recognizers. This is intentionally kept
/// in the child process: a native failure cannot take down the eframe process.
fn native_transcribe(model: &OnnxModelSpec, samples: &[f32]) -> Result<String> {
    validate_pcm_samples(samples)?;
    if model.family == OnnxModelFamily::OnlineTransducer {
        let mut config = OnlineRecognizerConfig::default();
        config.model_config.provider = Some("cpu".into());
        config.model_config.num_threads = i32::from(model.num_threads);
        config.model_config.tokens = Some(model.path(OnnxFileRole::Tokens)?);
        config.model_config.transducer = OnlineTransducerModelConfig {
            encoder: Some(model.path(OnnxFileRole::Encoder)?),
            decoder: Some(model.path(OnnxFileRole::Decoder)?),
            joiner: Some(model.path(OnnxFileRole::Joiner)?),
        };
        let recognizer = OnlineRecognizer::create(&config)
            .ok_or_else(|| anyhow!("sherpa-onnx failed to create CPU online recognizer"))?;
        let stream = recognizer.create_stream();
        stream.accept_waveform(16_000, samples);
        stream.input_finished();
        while recognizer.is_ready(&stream) {
            recognizer.decode(&stream);
        }
        return recognizer
            .get_result(&stream)
            .map(|result| result.text)
            .ok_or_else(|| anyhow!("sherpa-onnx online recognizer returned no result"));
    }

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
        OnnxModelFamily::OnlineTransducer => unreachable!(),
    }
    let recognizer = OfflineRecognizer::create(&config)
        .ok_or_else(|| anyhow!("sherpa-onnx failed to create CPU offline recognizer"))?;
    let stream = recognizer.create_stream();
    stream.accept_waveform(16_000, samples);
    recognizer.decode(&stream);
    stream
        .get_result()
        .map(|result| result.text)
        .ok_or_else(|| anyhow!("sherpa-onnx offline recognizer returned no result"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

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
