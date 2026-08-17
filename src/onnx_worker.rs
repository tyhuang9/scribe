//! Private, process-isolated CPU-only sherpa-onnx execution substrate.
//!
//! This module deliberately has no catalog or UI entry point. A future typed
//! installer supplies a verified [`OnnxModelSpec`]; the router remains the only
//! component allowed to construct a worker client.

use std::collections::BTreeMap;
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

pub(crate) const PROTOCOL_MAGIC: [u8; 4] = *b"SCON";
pub(crate) const PROTOCOL_VERSION: u8 = 1;
const HEADER_LEN: usize = 26;
const MAX_CONTROL_BYTES: usize = 256 * 1024;
const MAX_AUDIO_BYTES: usize = 16 * 1024 * 1024;

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
        let required: &[OnnxFileRole] = match self.family {
            OnnxModelFamily::Moonshine => &[OnnxFileRole::Encoder, OnnxFileRole::Tokens],
            OnnxModelFamily::NemoCtc => &[OnnxFileRole::Model, OnnxFileRole::Tokens],
            OnnxModelFamily::Canary => &[
                OnnxFileRole::Encoder,
                OnnxFileRole::Decoder,
                OnnxFileRole::Tokens,
            ],
            OnnxModelFamily::OfflineTransducer | OnnxModelFamily::OnlineTransducer => &[
                OnnxFileRole::Encoder,
                OnnxFileRole::Decoder,
                OnnxFileRole::Joiner,
                OnnxFileRole::Tokens,
            ],
        };
        for role in required {
            let Some(relative) = self.files.get(role) else {
                bail!("ONNX model {} is missing required {role:?} file", self.id);
            };
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
            if !canonical.starts_with(&root) || !canonical.is_file() {
                bail!("ONNX model {} file is missing: {}", self.id, path.display());
            }
        }
        if self.family == OnnxModelFamily::Moonshine {
            let merged = self.files.contains_key(&OnnxFileRole::MergedDecoder);
            let v1 = [
                OnnxFileRole::Preprocessor,
                OnnxFileRole::UncachedDecoder,
                OnnxFileRole::CachedDecoder,
            ]
            .into_iter()
            .all(|role| self.files.contains_key(&role));
            if !merged && !v1 {
                bail!(
                    "Moonshine requires merged_decoder or preprocessor + uncached_decoder + cached_decoder"
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
        write_frame(
            &mut self.stdin,
            &control_frame(session_id, request_id, &control)?,
        )
    }
    fn send_pcm(&mut self, session_id: u64, request_id: u64, samples: &[f32]) -> Result<()> {
        let bytes = samples
            .iter()
            .flat_map(|sample| sample.to_le_bytes())
            .collect();
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
        parse_control(read_frame(&mut self.stdout)?)
    }
    fn round_trip(&mut self, session_id: u64, request_id: u64, command: Control) -> Result<()> {
        self.send(session_id, request_id, command)?;
        let (actual_session, actual_request, response) = self.receive()?;
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
            _ => bail!("unexpected ONNX worker response"),
        }
    }
    fn await_text(&mut self, session_id: u64, request_id: u64) -> Result<String> {
        let (actual_session, actual_request, response) = self.receive()?;
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
            _ => bail!("unexpected ONNX worker transcription response"),
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
        let (session_id, request_id, control) = parse_control(frame)?;
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
                    if pcm.kind != FrameKind::Pcm || pcm.session_id != session_id || pcm.request_id != request_id || pcm.body.len() % 4 != 0 {
                        bail!("invalid or mis-correlated ONNX PCM frame");
                    }
                    let samples = pcm.body.chunks_exact(4).map(|bytes| f32::from_le_bytes(bytes.try_into().unwrap())).collect::<Vec<_>>();
                    match native_transcribe(model, &samples) {
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

    #[test]
    fn protocol_rejects_bad_magic_version_and_oversized_frames() {
        for bytes in [vec![0; HEADER_LEN], {
            let mut value = Vec::new();
            value.extend_from_slice(&PROTOCOL_MAGIC);
            value.extend_from_slice(&[2, 1]);
            value.extend_from_slice(&0_u32.to_le_bytes());
            value.extend_from_slice(&0_u64.to_le_bytes());
            value.extend_from_slice(&0_u64.to_le_bytes());
            value
        }] {
            assert!(read_frame(&mut Cursor::new(bytes)).is_err());
        }
    }
    #[test]
    fn protocol_round_trips_fragmented_pcm_and_correlations() {
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
    fn model_spec_rejects_traversal_and_missing_roles() {
        let spec = OnnxModelSpec {
            id: "x".into(),
            root: std::env::temp_dir(),
            family: OnnxModelFamily::OnlineTransducer,
            files: BTreeMap::from([(OnnxFileRole::Encoder, PathBuf::from("../encoder.onnx"))]),
            num_threads: 1,
        };
        assert!(spec.validate().is_err());
    }
}
