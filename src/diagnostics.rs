use std::collections::{HashMap, HashSet, VecDeque};
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crossbeam_channel::{Receiver, SendTimeoutError, Sender, TrySendError};
use serde::{Deserialize, Serialize};

const DIAGNOSTIC_SCHEMA_VERSION: u32 = 3;
const DOWNLOAD_EVENT_SCHEMA_VERSION: u32 = 1;
const MAX_SESSION_SNAPSHOTS: usize = 50;
const MAX_DOWNLOAD_SNAPSHOTS: usize = 100;
const DOWNLOAD_CHANNEL_CAPACITY: usize = 128;
const DOWNLOAD_LOG_MAX_BYTES: u64 = 1024 * 1024;
const DOWNLOAD_LOG_LINE_MAX_BYTES: usize = 16 * 1024;
const DOWNLOAD_LOG_NAME: &str = "downloads.jsonl";

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SessionOutcome {
    Completed,
    Cancelled,
    Failed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum FailureStage {
    Capture,
    NoSpeech,
    Transcription,
    Output,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub(crate) struct SessionMetrics {
    pub hotkey_to_overlay_visible_ms: Option<u64>,
    pub hotkey_to_capture_started_ms: Option<u64>,
    pub hotkey_to_first_meter_update_ms: Option<u64>,
    pub maximum_input_rms: Option<f32>,
    pub maximum_input_peak: Option<f32>,
    pub speech_start_detected_ms: Option<u64>,
    pub model_load_ms: Option<u64>,
    pub first_partial_ms: Option<u64>,
    pub recording_duration_ms: Option<u64>,
    pub stop_to_capture_finalized_ms: Option<u64>,
    pub recording_end_to_final_text_ms: Option<u64>,
    pub post_processing_ms: Option<u64>,
    pub final_text_to_paste_ms: Option<u64>,
    pub final_text_to_output_completed_ms: Option<u64>,
    pub total_end_to_end_ms: Option<u64>,
    pub realtime_factor: Option<f64>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub(crate) struct SessionDiagnostic {
    pub session_id: u64,
    pub outcome: SessionOutcome,
    pub failure_stage: Option<FailureStage>,
    pub trigger: &'static str,
    pub model_id: Option<String>,
    pub model_architecture: Option<String>,
    pub resolved_backend: Option<String>,
    pub runtime_package_version: Option<String>,
    pub compute_backend: Option<String>,
    pub streaming_mode: Option<String>,
    pub cold_or_warm: Option<String>,
    pub output_outcome: Option<String>,
    pub metrics: SessionMetrics,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DownloadDiagnosticIdError;

impl std::fmt::Display for DownloadDiagnosticIdError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("diagnostic identifier is not a safe public identifier")
    }
}

impl std::error::Error for DownloadDiagnosticIdError {}

macro_rules! diagnostic_id {
    ($name:ident) => {
        #[derive(Clone, Debug, Eq, Hash, PartialEq)]
        pub(crate) struct $name(String);
        impl $name {
            pub(crate) fn new(value: impl Into<String>) -> Result<Self, DownloadDiagnosticIdError> {
                let value = value.into();
                is_safe_diagnostic_id(&value)
                    .then_some(Self(value))
                    .ok_or(DownloadDiagnosticIdError)
            }
        }
    };
}
diagnostic_id!(DownloadRunId);
diagnostic_id!(DownloadJobId);
diagnostic_id!(PublicArtifactId);

fn is_safe_diagnostic_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && !value.contains("://")
        && !value.contains("..")
        && !value.starts_with(['/', '\\'])
        && !value.contains('\\')
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b':' | b'/'))
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DownloadSourceClass {
    ModelRepository,
    ContentDeliveryNetwork,
    DirectDownload,
    BundledMirror,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DownloadFaultCategory {
    Connectivity,
    Timeout,
    RemoteUnavailable,
    RangeRejected,
    Integrity,
    LocalStorage,
    ProcessInterrupted,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DownloadDiagnosticOutcome {
    Admission,
    FirstStall,
    RetryScheduled,
    Reconnection,
    PhaseTransition,
    Pause,
    Failure,
    Completion,
    PriorRunInterruption,
}

impl DownloadDiagnosticOutcome {
    fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Failure | Self::Completion | Self::PriorRunInterruption
        )
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DownloadDiagnosticEvent {
    schema_version: u32,
    app_version: String,
    timestamp_unix_ms: u64,
    run_id: String,
    job_id: String,
    public_artifact_id: String,
    source_class: DownloadSourceClass,
    completed_bytes: u64,
    total_bytes: Option<u64>,
    retry_number: Option<u32>,
    fault_category: Option<DownloadFaultCategory>,
    outcome: DownloadDiagnosticOutcome,
}

impl DownloadDiagnosticEvent {
    pub(crate) fn admission(
        run: DownloadRunId,
        job: DownloadJobId,
        artifact: PublicArtifactId,
        source: DownloadSourceClass,
        total: Option<u64>,
    ) -> Self {
        Self::new(
            run,
            job,
            artifact,
            source,
            0,
            total,
            None,
            None,
            DownloadDiagnosticOutcome::Admission,
        )
    }
    pub(crate) fn first_stall(
        run: DownloadRunId,
        job: DownloadJobId,
        artifact: PublicArtifactId,
        source: DownloadSourceClass,
        completed: u64,
        total: Option<u64>,
        fault: DownloadFaultCategory,
    ) -> Self {
        Self::new(
            run,
            job,
            artifact,
            source,
            completed,
            total,
            None,
            Some(fault),
            DownloadDiagnosticOutcome::FirstStall,
        )
    }
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn retry_scheduled(
        run: DownloadRunId,
        job: DownloadJobId,
        artifact: PublicArtifactId,
        source: DownloadSourceClass,
        completed: u64,
        total: Option<u64>,
        retry: u32,
        fault: DownloadFaultCategory,
    ) -> Self {
        Self::new(
            run,
            job,
            artifact,
            source,
            completed,
            total,
            Some(retry),
            Some(fault),
            DownloadDiagnosticOutcome::RetryScheduled,
        )
    }
    pub(crate) fn reconnection(
        run: DownloadRunId,
        job: DownloadJobId,
        artifact: PublicArtifactId,
        source: DownloadSourceClass,
        completed: u64,
        total: Option<u64>,
        retry: u32,
    ) -> Self {
        Self::new(
            run,
            job,
            artifact,
            source,
            completed,
            total,
            Some(retry),
            None,
            DownloadDiagnosticOutcome::Reconnection,
        )
    }
    pub(crate) fn phase_transition(
        run: DownloadRunId,
        job: DownloadJobId,
        artifact: PublicArtifactId,
        source: DownloadSourceClass,
        completed: u64,
        total: Option<u64>,
    ) -> Self {
        Self::new(
            run,
            job,
            artifact,
            source,
            completed,
            total,
            None,
            None,
            DownloadDiagnosticOutcome::PhaseTransition,
        )
    }
    pub(crate) fn pause(
        run: DownloadRunId,
        job: DownloadJobId,
        artifact: PublicArtifactId,
        source: DownloadSourceClass,
        completed: u64,
        total: Option<u64>,
    ) -> Self {
        Self::new(
            run,
            job,
            artifact,
            source,
            completed,
            total,
            None,
            None,
            DownloadDiagnosticOutcome::Pause,
        )
    }
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn failure(
        run: DownloadRunId,
        job: DownloadJobId,
        artifact: PublicArtifactId,
        source: DownloadSourceClass,
        completed: u64,
        total: Option<u64>,
        retry: Option<u32>,
        fault: DownloadFaultCategory,
    ) -> Self {
        Self::new(
            run,
            job,
            artifact,
            source,
            completed,
            total,
            retry,
            Some(fault),
            DownloadDiagnosticOutcome::Failure,
        )
    }
    pub(crate) fn completion(
        run: DownloadRunId,
        job: DownloadJobId,
        artifact: PublicArtifactId,
        source: DownloadSourceClass,
        completed: u64,
        total: Option<u64>,
        retry: Option<u32>,
    ) -> Self {
        Self::new(
            run,
            job,
            artifact,
            source,
            completed,
            total,
            retry,
            None,
            DownloadDiagnosticOutcome::Completion,
        )
    }
    #[allow(clippy::too_many_arguments)]
    fn new(
        run: DownloadRunId,
        job: DownloadJobId,
        artifact: PublicArtifactId,
        source_class: DownloadSourceClass,
        completed_bytes: u64,
        total_bytes: Option<u64>,
        retry_number: Option<u32>,
        fault_category: Option<DownloadFaultCategory>,
        outcome: DownloadDiagnosticOutcome,
    ) -> Self {
        Self {
            schema_version: DOWNLOAD_EVENT_SCHEMA_VERSION,
            app_version: env!("CARGO_PKG_VERSION").into(),
            timestamp_unix_ms: unix_time_ms(),
            run_id: run.0,
            job_id: job.0,
            public_artifact_id: artifact.0,
            source_class,
            completed_bytes,
            total_bytes,
            retry_number,
            fault_category,
            outcome,
        }
    }
    fn interrupted_from(previous: &Self) -> Self {
        let mut event = previous.clone();
        event.app_version = env!("CARGO_PKG_VERSION").to_owned();
        event.timestamp_unix_ms = unix_time_ms();
        event.retry_number = None;
        event.fault_category = Some(DownloadFaultCategory::ProcessInterrupted);
        event.outcome = DownloadDiagnosticOutcome::PriorRunInterruption;
        event
    }

    fn is_valid_for_load(&self) -> bool {
        self.schema_version == DOWNLOAD_EVENT_SCHEMA_VERSION
            && is_safe_diagnostic_id(&self.run_id)
            && is_safe_diagnostic_id(&self.job_id)
            && is_safe_diagnostic_id(&self.public_artifact_id)
            && !self.app_version.is_empty()
            && self.app_version.len() <= 64
            && self.app_version.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'+' | b'_')
            })
            && self
                .total_bytes
                .is_none_or(|total| self.completed_bytes <= total)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DownloadDiagnosticEnqueue {
    Queued,
    IgnoredDuplicate,
    DroppedFull,
    WriterUnavailable,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DownloadDiagnosticFlush {
    Flushed,
    TimedOut,
    StorageUnavailable,
    WriterUnavailable,
}

#[derive(Debug)]
enum DownloadWriterCommand {
    Event(DownloadDiagnosticEvent),
    Flush(Sender<bool>),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DownloadDiagnosticsError {
    StorageUnavailable,
    WriterUnavailable,
}

impl DownloadDiagnosticsError {
    pub(crate) fn settings_diagnostic(self) -> &'static str {
        match self {
            Self::StorageUnavailable => {
                "Download diagnostics could not be saved. Downloads can continue normally."
            }
            Self::WriterUnavailable => {
                "Download diagnostics are unavailable. Downloads can continue normally."
            }
        }
    }
}

#[derive(Debug, Default)]
struct DownloadDiagnosticsState {
    snapshots: RwLock<VecDeque<DownloadDiagnosticEvent>>,
    first_stalls: Mutex<HashSet<(String, String)>>,
    error: Mutex<Option<DownloadDiagnosticsError>>,
    error_observed: AtomicBool,
}

impl DownloadDiagnosticsState {
    fn report_error(&self, error: DownloadDiagnosticsError) {
        let mut current = self.error.lock().unwrap_or_else(|e| e.into_inner());
        if current.is_none() {
            *current = Some(error);
        }
    }
    fn append(&self, event: DownloadDiagnosticEvent) {
        let mut first_stalls = self.first_stalls.lock().unwrap_or_else(|e| e.into_inner());
        if event.outcome == DownloadDiagnosticOutcome::FirstStall {
            first_stalls.insert((event.run_id.clone(), event.job_id.clone()));
        } else if event.outcome.is_terminal() {
            first_stalls.remove(&(event.run_id.clone(), event.job_id.clone()));
        }
        drop(first_stalls);
        self.append_snapshot(event);
    }
    fn append_snapshot(&self, event: DownloadDiagnosticEvent) {
        let mut events = self.snapshots.write().unwrap_or_else(|e| e.into_inner());
        events.push_back(event);
        while events.len() > MAX_DOWNLOAD_SNAPSHOTS {
            events.pop_front();
        }
    }
    fn nonfatal_error(&self) -> Option<DownloadDiagnosticsError> {
        *self.error.lock().unwrap_or_else(|e| e.into_inner())
    }
}

#[derive(Clone, Debug)]
pub(crate) struct DownloadDiagnostics {
    sender: Sender<DownloadWriterCommand>,
    state: Arc<DownloadDiagnosticsState>,
}

impl DownloadDiagnostics {
    pub(crate) fn start(directory: impl Into<PathBuf>, current_run: &DownloadRunId) -> Self {
        let directory = directory.into();
        let state = Arc::new(DownloadDiagnosticsState::default());
        let mut secure_directory = match ensure_private_directory(&directory) {
            Ok(directory) => Some(directory),
            Err(_) => {
                state.report_error(DownloadDiagnosticsError::StorageUnavailable);
                None
            }
        };
        let loaded = secure_directory
            .as_deref()
            .map(|directory| load_download_events(directory, &state))
            .unwrap_or_default();
        if state.nonfatal_error() == Some(DownloadDiagnosticsError::StorageUnavailable) {
            secure_directory = None;
        }
        let interruptions = classify_prior_run_interruptions(&loaded, current_run);
        for event in loaded.into_iter().chain(interruptions.iter().cloned()) {
            state.append(event);
        }
        let (sender, receiver) = crossbeam_channel::bounded(DOWNLOAD_CHANNEL_CAPACITY);
        let writer_state = Arc::clone(&state);
        if std::thread::Builder::new()
            .name("download-diagnostics".into())
            .spawn(move || run_download_writer(secure_directory, receiver, writer_state))
            .is_err()
        {
            state.report_error(DownloadDiagnosticsError::WriterUnavailable);
        }
        let result = Self { sender, state };
        for event in interruptions {
            let _ = result.sender.try_send(DownloadWriterCommand::Event(event));
        }
        result
    }
    pub(crate) fn record(&self, event: DownloadDiagnosticEvent) -> DownloadDiagnosticEnqueue {
        let mut first_stalls = self
            .state
            .first_stalls
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if event.outcome == DownloadDiagnosticOutcome::FirstStall
            && first_stalls.contains(&(event.run_id.clone(), event.job_id.clone()))
        {
            return DownloadDiagnosticEnqueue::IgnoredDuplicate;
        }
        match self
            .sender
            .try_send(DownloadWriterCommand::Event(event.clone()))
        {
            Ok(()) => {
                if event.outcome == DownloadDiagnosticOutcome::FirstStall {
                    first_stalls.insert((event.run_id.clone(), event.job_id.clone()));
                } else if event.outcome.is_terminal() {
                    first_stalls.remove(&(event.run_id.clone(), event.job_id.clone()));
                }
                drop(first_stalls);
                self.state.append_snapshot(event);
                DownloadDiagnosticEnqueue::Queued
            }
            Err(TrySendError::Full(_)) => DownloadDiagnosticEnqueue::DroppedFull,
            Err(TrySendError::Disconnected(_)) => DownloadDiagnosticEnqueue::WriterUnavailable,
        }
    }
    pub(crate) fn snapshot(&self) -> Vec<DownloadDiagnosticEvent> {
        self.state
            .snapshots
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .cloned()
            .collect()
    }
    pub(crate) fn nonfatal_error(&self) -> Option<DownloadDiagnosticsError> {
        self.state.nonfatal_error()
    }
    pub(crate) fn take_new_nonfatal_error(&self) -> Option<DownloadDiagnosticsError> {
        let error = self.nonfatal_error()?;
        (!self.state.error_observed.swap(true, Ordering::AcqRel)).then_some(error)
    }

    pub(crate) fn flush(&self, timeout: Duration) -> DownloadDiagnosticFlush {
        let started = Instant::now();
        let (acknowledge, acknowledged) = crossbeam_channel::bounded(1);
        match self
            .sender
            .send_timeout(DownloadWriterCommand::Flush(acknowledge), timeout)
        {
            Ok(()) => {}
            Err(SendTimeoutError::Timeout(_)) => return DownloadDiagnosticFlush::TimedOut,
            Err(SendTimeoutError::Disconnected(_)) => {
                return DownloadDiagnosticFlush::WriterUnavailable;
            }
        }
        let remaining = timeout.saturating_sub(started.elapsed());
        match acknowledged.recv_timeout(remaining) {
            Ok(true) => DownloadDiagnosticFlush::Flushed,
            Ok(false) => DownloadDiagnosticFlush::StorageUnavailable,
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => DownloadDiagnosticFlush::TimedOut,
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                DownloadDiagnosticFlush::WriterUnavailable
            }
        }
    }
}

fn load_download_events(
    directory: &Path,
    state: &DownloadDiagnosticsState,
) -> Vec<DownloadDiagnosticEvent> {
    let mut events = VecDeque::new();
    for path in [
        directory.join(format!("{DOWNLOAD_LOG_NAME}.2")),
        directory.join(format!("{DOWNLOAD_LOG_NAME}.1")),
        directory.join(DOWNLOAD_LOG_NAME),
    ] {
        let file = match open_existing_log(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(_) => {
                state.report_error(DownloadDiagnosticsError::StorageUnavailable);
                continue;
            }
        };
        let mut reader = BufReader::new(file.take(DOWNLOAD_LOG_MAX_BYTES));
        loop {
            match read_bounded_jsonl_line(&mut reader) {
                Ok(BoundedLine::Complete(line)) => {
                    let Ok(line) = std::str::from_utf8(&line) else {
                        continue;
                    };
                    let Ok(event) = serde_json::from_str::<DownloadDiagnosticEvent>(line) else {
                        continue;
                    };
                    if !event.is_valid_for_load() {
                        continue;
                    }
                    events.push_back(event);
                    while events.len() > MAX_DOWNLOAD_SNAPSHOTS {
                        events.pop_front();
                    }
                }
                Ok(BoundedLine::Overlong) => continue,
                Ok(BoundedLine::Unterminated | BoundedLine::End) => break,
                Err(_) => {
                    state.report_error(DownloadDiagnosticsError::StorageUnavailable);
                    break;
                }
            }
        }
    }
    events.into_iter().collect()
}

enum BoundedLine {
    Complete(Vec<u8>),
    Overlong,
    Unterminated,
    End,
}

fn read_bounded_jsonl_line(reader: &mut impl BufRead) -> io::Result<BoundedLine> {
    let mut line = Vec::with_capacity(512);
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return Ok(if line.is_empty() {
                BoundedLine::End
            } else {
                BoundedLine::Unterminated
            });
        }
        if let Some(newline) = available.iter().position(|byte| *byte == b'\n') {
            if line.len().saturating_add(newline) > DOWNLOAD_LOG_LINE_MAX_BYTES {
                reader.consume(newline + 1);
                return Ok(BoundedLine::Overlong);
            }
            line.extend_from_slice(&available[..newline]);
            reader.consume(newline + 1);
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            return Ok(BoundedLine::Complete(line));
        }
        if line.len().saturating_add(available.len()) > DOWNLOAD_LOG_LINE_MAX_BYTES {
            let consumed = available.len();
            reader.consume(consumed);
            discard_through_newline(reader)?;
            return Ok(BoundedLine::Overlong);
        }
        line.extend_from_slice(available);
        let consumed = available.len();
        reader.consume(consumed);
    }
}

fn discard_through_newline(reader: &mut impl BufRead) -> io::Result<()> {
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return Ok(());
        }
        let consumed = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(available.len(), |newline| newline + 1);
        let found_newline =
            consumed <= available.len() && available.get(consumed - 1) == Some(&b'\n');
        reader.consume(consumed);
        if found_newline {
            return Ok(());
        }
    }
}

fn classify_prior_run_interruptions(
    events: &[DownloadDiagnosticEvent],
    current_run: &DownloadRunId,
) -> Vec<DownloadDiagnosticEvent> {
    let mut latest = HashMap::<(&str, &str), usize>::new();
    for (index, event) in events.iter().enumerate() {
        latest.insert((&event.run_id, &event.job_id), index);
    }
    let mut indexes: Vec<_> = latest.into_values().collect();
    indexes.sort_unstable();
    indexes
        .into_iter()
        .filter_map(|index| {
            let event = &events[index];
            (event.run_id != current_run.0 && !event.outcome.is_terminal())
                .then(|| DownloadDiagnosticEvent::interrupted_from(event))
        })
        .collect()
}

fn run_download_writer(
    directory: Option<PathBuf>,
    receiver: Receiver<DownloadWriterCommand>,
    state: Arc<DownloadDiagnosticsState>,
) {
    let mut writer = None;
    for command in receiver {
        match command {
            DownloadWriterCommand::Event(event) => {
                let Some(directory) = directory.as_deref() else {
                    continue;
                };
                let Ok(mut line) = serde_json::to_vec(&event) else {
                    continue;
                };
                line.push(b'\n');
                if write_download_line(directory, &mut writer, &line).is_err() {
                    state.report_error(DownloadDiagnosticsError::StorageUnavailable);
                    writer = None;
                }
            }
            DownloadWriterCommand::Flush(acknowledge) => {
                let storage_available = *state.error.lock().unwrap_or_else(|e| e.into_inner())
                    != Some(DownloadDiagnosticsError::StorageUnavailable);
                let directory_secure = directory
                    .as_deref()
                    .is_some_and(|directory| ensure_private_directory(directory).is_ok());
                let succeeded = directory_secure
                    && storage_available
                    && writer.as_mut().is_none_or(|writer| {
                        writer
                            .file
                            .flush()
                            .and_then(|()| writer.file.sync_data())
                            .is_ok()
                    });
                if !succeeded {
                    state.report_error(DownloadDiagnosticsError::StorageUnavailable);
                    writer = None;
                }
                let _ = acknowledge.try_send(succeeded);
            }
        }
    }
    if let Some(mut writer) = writer {
        let _ = writer.file.flush();
    }
}

fn write_download_line(
    directory: &Path,
    writer: &mut Option<SecureLogWriter>,
    line: &[u8],
) -> io::Result<()> {
    if line.len() > DOWNLOAD_LOG_LINE_MAX_BYTES || line.len() as u64 > DOWNLOAD_LOG_MAX_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "download diagnostic event exceeds the bounded line size",
        ));
    }
    let directory = ensure_private_directory(directory)?;
    if writer.is_none() {
        *writer = Some(open_download_log(&directory)?);
    }
    let active = writer.as_ref().expect("writer initialized");
    validate_log_identity(&directory.join(DOWNLOAD_LOG_NAME), &active.identity)?;
    let length = active.file.metadata()?.len();
    if length.saturating_add(line.len() as u64) > DOWNLOAD_LOG_MAX_BYTES {
        *writer = None;
        rotate_download_logs(&directory)?;
        *writer = Some(open_download_log(&directory)?);
    }
    writer
        .as_mut()
        .expect("writer initialized")
        .file
        .write_all(line)
}

#[derive(Debug)]
struct SecureLogWriter {
    file: File,
    identity: FileIdentity,
}

fn open_download_log(directory: &Path) -> io::Result<SecureLogWriter> {
    let mut options = OpenOptions::new();
    options.append(true).create(true);
    configure_no_follow_file_open(&mut options, 0o600);
    let path = directory.join(DOWNLOAD_LOG_NAME);
    let file = options.open(&path)?;
    apply_private_file_permissions(&file)?;
    let identity = validated_file_identity(&file, true)?;
    validate_log_identity(&path, &identity)?;
    Ok(SecureLogWriter { file, identity })
}

fn open_existing_log(path: &Path) -> io::Result<File> {
    open_existing_regular_file(path, true)
}

fn open_existing_regular_file(path: &Path, enforce_size_cap: bool) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    configure_no_follow_file_open(&mut options, 0);
    let file = options.open(path)?;
    apply_private_file_permissions(&file)?;
    validated_file_identity(&file, enforce_size_cap)?;
    Ok(file)
}

fn rotate_download_logs(directory: &Path) -> io::Result<()> {
    let directory = ensure_private_directory(directory)?;
    let oldest = directory.join(format!("{DOWNLOAD_LOG_NAME}.2"));
    remove_log_if_present(&oldest)?;
    rename_log_if_present(&directory.join(format!("{DOWNLOAD_LOG_NAME}.1")), &oldest)?;
    rename_log_if_present(
        &directory.join(DOWNLOAD_LOG_NAME),
        &directory.join(format!("{DOWNLOAD_LOG_NAME}.1")),
    )?;
    ensure_private_directory(&directory)?;
    Ok(())
}

fn remove_log_if_present(path: &Path) -> io::Result<()> {
    let identity = match log_identity_from_path(path) {
        Ok(identity) => identity,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    validate_log_identity(path, &identity)?;
    fs::remove_file(path)
}

fn rename_log_if_present(from: &Path, to: &Path) -> io::Result<()> {
    let identity = match log_identity_from_path(from) {
        Ok(identity) => identity,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    validate_log_identity(from, &identity)?;
    match log_identity_from_path(to) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Ok(_) => {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "download diagnostic rotation destination unexpectedly exists",
            ));
        }
        Err(error) => return Err(error),
    }
    validate_log_identity(from, &identity)?;
    fs::rename(from, to)?;
    validate_log_identity(to, &identity)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FileIdentity {
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
    #[cfg(windows)]
    volume: u32,
    #[cfg(windows)]
    index: u64,
    #[cfg(not(any(unix, windows)))]
    length: u64,
    #[cfg(not(any(unix, windows)))]
    modified: Option<SystemTime>,
}

fn ensure_private_directory(directory: &Path) -> io::Result<PathBuf> {
    if directory.as_os_str().is_empty()
        || directory
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "diagnostics directory must not contain parent traversal",
        ));
    }
    let absolute = if directory.is_absolute() {
        directory.to_path_buf()
    } else {
        std::env::current_dir()?.join(directory)
    };
    let mut current = PathBuf::new();
    for component in absolute.components() {
        current.push(component.as_os_str());
        if matches!(component, std::path::Component::Prefix(_)) {
            continue;
        }
        match fs::symlink_metadata(&current) {
            Ok(metadata) => validate_directory_metadata(&metadata)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                create_private_directory_component(&current)?;
                validate_directory_metadata(&fs::symlink_metadata(&current)?)?;
            }
            Err(error) => return Err(error),
        }
    }
    let handle = open_directory_no_follow(&absolute)?;
    let metadata = handle.metadata()?;
    validate_directory_metadata(&metadata)?;
    let identity = object_identity(&handle, &metadata)?;
    apply_private_directory_permissions(&handle)?;
    validate_directory_components(&absolute)?;
    let reopened = open_directory_no_follow(&absolute)?;
    let reopened_metadata = reopened.metadata()?;
    validate_directory_metadata(&reopened_metadata)?;
    if object_identity(&reopened, &reopened_metadata)? != identity {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "diagnostics directory identity changed during validation",
        ));
    }
    Ok(absolute)
}

#[cfg(unix)]
fn create_private_directory_component(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    let mut builder = fs::DirBuilder::new();
    builder.mode(0o700).create(path)
}

#[cfg(not(unix))]
fn create_private_directory_component(path: &Path) -> io::Result<()> {
    fs::create_dir(path)
}

fn validate_directory_components(directory: &Path) -> io::Result<()> {
    let mut current = PathBuf::new();
    for component in directory.components() {
        current.push(component.as_os_str());
        if !matches!(component, std::path::Component::Prefix(_)) {
            validate_directory_metadata(&fs::symlink_metadata(&current)?)?;
        }
    }
    Ok(())
}

fn validate_directory_metadata(metadata: &fs::Metadata) -> io::Result<()> {
    if !metadata.is_dir() || metadata.file_type().is_symlink() || metadata_is_reparse(metadata) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "diagnostics directory contains a link, reparse point, or non-directory component",
        ));
    }
    Ok(())
}

fn open_directory_no_follow(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_CLOEXEC | libc::O_DIRECTORY | libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        options.custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT);
    }
    options.open(path)
}

fn configure_no_follow_file_open(options: &mut OpenOptions, unix_mode: u32) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW);
        if unix_mode != 0 {
            options.mode(unix_mode);
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        let _ = unix_mode;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    #[cfg(not(any(unix, windows)))]
    let _ = (options, unix_mode);
}

fn validated_file_identity(file: &File, enforce_size_cap: bool) -> io::Result<FileIdentity> {
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata_is_reparse(&metadata) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "download diagnostic log is not a regular no-follow file",
        ));
    }
    if enforce_size_cap && metadata.len() > DOWNLOAD_LOG_MAX_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "download diagnostic log exceeds its rotation bound",
        ));
    }
    object_identity(file, &metadata)
}

fn object_identity(file: &File, _metadata: &fs::Metadata) -> io::Result<FileIdentity> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Ok(FileIdentity {
            device: _metadata.dev(),
            inode: _metadata.ino(),
        })
    }
    #[cfg(windows)]
    {
        use std::mem::MaybeUninit;
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Storage::FileSystem::{
            BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
        };
        let mut information = MaybeUninit::<BY_HANDLE_FILE_INFORMATION>::zeroed();
        // SAFETY: `file` owns a valid handle and `information` points to writable storage
        // for the exact structure required by GetFileInformationByHandle.
        let succeeded =
            unsafe { GetFileInformationByHandle(file.as_raw_handle(), information.as_mut_ptr()) };
        if succeeded == 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: the successful call above initialized the whole output structure.
        let information = unsafe { information.assume_init() };
        Ok(FileIdentity {
            volume: information.dwVolumeSerialNumber,
            index: (u64::from(information.nFileIndexHigh) << 32)
                | u64::from(information.nFileIndexLow),
        })
    }
    #[cfg(not(any(unix, windows)))]
    {
        Ok(FileIdentity {
            length: _metadata.len(),
            modified: _metadata.modified().ok(),
        })
    }
}

fn log_identity_from_path(path: &Path) -> io::Result<FileIdentity> {
    let file = open_existing_log(path)?;
    validated_file_identity(&file, true)
}

fn validate_log_identity(path: &Path, expected: &FileIdentity) -> io::Result<()> {
    validate_regular_file_identity(path, expected, true)
}

fn validate_regular_file_identity(
    path: &Path,
    expected: &FileIdentity,
    enforce_size_cap: bool,
) -> io::Result<()> {
    let file = open_existing_regular_file(path, enforce_size_cap)?;
    let actual = validated_file_identity(&file, enforce_size_cap)?;
    if actual != *expected {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "download diagnostic log identity changed during a path operation",
        ));
    }
    Ok(())
}

#[cfg(windows)]
fn metadata_is_reparse(metadata: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(windows))]
fn metadata_is_reparse(_metadata: &fs::Metadata) -> bool {
    false
}

#[cfg(unix)]
fn apply_private_directory_permissions(directory: &File) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    directory.set_permissions(fs::Permissions::from_mode(0o700))
}

#[cfg(not(unix))]
fn apply_private_directory_permissions(_directory: &File) -> io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn apply_private_file_permissions(file: &File) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    file.set_permissions(fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn apply_private_file_permissions(_file: &File) -> io::Result<()> {
    Ok(())
}

#[derive(Clone, Debug, Default)]
pub(crate) struct DiagnosticsStore {
    sessions: VecDeque<SessionDiagnostic>,
    download_diagnostics: Option<DownloadDiagnostics>,
}

impl DiagnosticsStore {
    pub fn record(&mut self, diagnostic: SessionDiagnostic) {
        if let Some(existing) = self
            .sessions
            .iter()
            .position(|e| e.session_id == diagnostic.session_id)
        {
            self.sessions.remove(existing);
        }
        self.sessions.push_back(diagnostic);
        while self.sessions.len() > MAX_SESSION_SNAPSHOTS {
            self.sessions.pop_front();
        }
    }
    pub fn len(&self) -> usize {
        self.sessions.len()
    }
    pub(crate) fn attach_download_diagnostics(&mut self, diagnostics: &DownloadDiagnostics) {
        self.download_diagnostics = Some(diagnostics.clone());
    }
    fn report(&self) -> DiagnosticReport<'_> {
        DiagnosticReport {
            schema_version: DIAGNOSTIC_SCHEMA_VERSION,
            generated_at_unix_ms: unix_time_ms(),
            application: ApplicationDiagnostic {
                name: env!("CARGO_PKG_NAME"),
                version: env!("CARGO_PKG_VERSION"),
                operating_system: std::env::consts::OS,
                architecture: std::env::consts::ARCH,
            },
            privacy: PrivacyDiagnostic {
                transcript_content_included: false,
                audio_content_included: false,
                secrets_included: false,
                filesystem_paths_included: false,
                raw_errors_included: false,
            },
            sessions: self.sessions.iter().collect(),
            download_events: self
                .download_diagnostics
                .as_ref()
                .map(DownloadDiagnostics::snapshot)
                .unwrap_or_default(),
        }
    }
}

#[derive(Serialize)]
struct DiagnosticReport<'a> {
    schema_version: u32,
    generated_at_unix_ms: u64,
    application: ApplicationDiagnostic,
    privacy: PrivacyDiagnostic,
    sessions: Vec<&'a SessionDiagnostic>,
    download_events: Vec<DownloadDiagnosticEvent>,
}
#[derive(Serialize)]
struct ApplicationDiagnostic {
    name: &'static str,
    version: &'static str,
    operating_system: &'static str,
    architecture: &'static str,
}
#[derive(Serialize)]
struct PrivacyDiagnostic {
    transcript_content_included: bool,
    audio_content_included: bool,
    secrets_included: bool,
    filesystem_paths_included: bool,
    raw_errors_included: bool,
}

pub(crate) fn export_redacted(
    directory: &Path,
    diagnostics: &DiagnosticsStore,
) -> io::Result<PathBuf> {
    let directory = ensure_private_directory(directory)?;
    let timestamp = unix_time_ms();
    let mut attempt = 0_u32;
    loop {
        let suffix = if attempt == 0 {
            String::new()
        } else {
            format!("-{attempt}")
        };
        let path = directory.join(format!("scribe-diagnostics-{timestamp}{suffix}.json"));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        configure_no_follow_file_open(&mut options, 0o600);
        match options.open(&path) {
            Ok(mut file) => {
                apply_private_file_permissions(&file)?;
                let identity = validated_file_identity(&file, false)?;
                serde_json::to_writer_pretty(&mut file, &diagnostics.report())
                    .map_err(io::Error::other)?;
                file.write_all(b"\n")?;
                file.sync_all()?;
                validate_regular_file_identity(&path, &identity, false)?;
                return Ok(path);
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                attempt = attempt.saturating_add(1)
            }
            Err(error) => return Err(error),
        }
    }
}

fn unix_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Barrier;
    use std::thread;
    use std::time::{Duration, Instant};

    fn temp(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "scribe-{label}-{}-{}",
            std::process::id(),
            unix_time_ms()
        ))
    }
    fn diagnostic(session_id: u64) -> SessionDiagnostic {
        SessionDiagnostic {
            session_id,
            outcome: SessionOutcome::Completed,
            failure_stage: None,
            trigger: "app_action",
            model_id: Some("local-model".into()),
            model_architecture: None,
            resolved_backend: Some("transcribe-cpp".into()),
            runtime_package_version: None,
            compute_backend: Some("CPU".into()),
            streaming_mode: Some("rolling".into()),
            cold_or_warm: Some("warm".into()),
            output_outcome: None,
            metrics: SessionMetrics {
                model_load_ms: Some(0),
                ..Default::default()
            },
        }
    }
    fn run(v: &str) -> DownloadRunId {
        DownloadRunId::new(v).unwrap()
    }
    fn job(v: &str) -> DownloadJobId {
        DownloadJobId::new(v).unwrap()
    }
    fn artifact() -> PublicArtifactId {
        PublicArtifactId::new("whisper/tiny-en").unwrap()
    }
    fn admission(r: &str, j: &str) -> DownloadDiagnosticEvent {
        DownloadDiagnosticEvent::admission(
            run(r),
            job(j),
            artifact(),
            DownloadSourceClass::ModelRepository,
            Some(75_000_000),
        )
    }
    fn wait_lines(path: &Path, expected: usize) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while fs::read_to_string(path)
            .map(|s| s.lines().count())
            .unwrap_or(0)
            < expected
        {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for log writes"
            );
            thread::sleep(Duration::from_millis(10));
        }
    }

    #[cfg(unix)]
    fn create_directory_link(target: &Path, link: &Path) -> io::Result<()> {
        std::os::unix::fs::symlink(target, link)
    }

    #[cfg(windows)]
    fn create_directory_link(target: &Path, link: &Path) -> io::Result<()> {
        std::os::windows::fs::symlink_dir(target, link)
    }

    #[cfg(unix)]
    fn create_file_link(target: &Path, link: &Path) -> io::Result<()> {
        std::os::unix::fs::symlink(target, link)
    }

    #[cfg(windows)]
    fn create_file_link(target: &Path, link: &Path) -> io::Result<()> {
        std::os::windows::fs::symlink_file(target, link)
    }

    #[cfg(unix)]
    fn remove_directory_link(link: &Path) -> io::Result<()> {
        fs::remove_file(link)
    }

    #[cfg(windows)]
    fn remove_directory_link(link: &Path) -> io::Result<()> {
        fs::remove_dir(link)
    }

    #[test]
    fn report_is_allowlisted_and_marks_private_content_excluded() {
        let mut store = DiagnosticsStore::default();
        let mut entry = diagnostic(7);
        entry.output_outcome = Some("inserted_clipboard_restore_skipped".into());
        store.record(entry);
        let json = serde_json::to_string_pretty(&store.report()).unwrap();
        assert!(json.contains("\"schema_version\": 3"));
        assert!(json.contains("\"output_outcome\": \"inserted_clipboard_restore_skipped\""));
        assert!(json.contains("\"transcript_content_included\": false"));
        assert!(json.contains("\"audio_content_included\": false"));
        assert!(json.contains("\"secrets_included\": false"));
        for marker in [
            "transcript_text",
            "audio_path",
            "stdout",
            "stderr",
            "api_key",
        ] {
            assert!(!json.contains(marker));
        }
    }

    #[test]
    fn missing_measurements_remain_null_instead_of_becoming_zero() {
        let mut store = DiagnosticsStore::default();
        store.record(diagnostic(8));
        let value = serde_json::to_value(store.report()).unwrap();
        let metrics = &value["sessions"][0]["metrics"];
        assert!(metrics["hotkey_to_capture_started_ms"].is_null());
        assert!(metrics["hotkey_to_first_meter_update_ms"].is_null());
        assert!(metrics["maximum_input_rms"].is_null());
        assert!(metrics["maximum_input_peak"].is_null());
        assert!(metrics["speech_start_detected_ms"].is_null());
        assert!(metrics["post_processing_ms"].is_null());
        assert_eq!(metrics["model_load_ms"], 0);
    }

    #[test]
    fn store_replaces_sessions_and_stays_bounded() {
        let mut store = DiagnosticsStore::default();
        store.record(diagnostic(1));
        let mut replacement = diagnostic(1);
        replacement.outcome = SessionOutcome::Failed;
        replacement.failure_stage = Some(FailureStage::Output);
        store.record(replacement);
        for id in 2..=60 {
            store.record(diagnostic(id));
        }
        assert_eq!(store.len(), 50);
        assert_eq!(store.sessions.back().unwrap().session_id, 60);
        assert!(store.sessions.iter().all(|entry| entry.session_id >= 11));
    }

    #[test]
    fn concurrent_records_are_valid_jsonl() {
        let root = temp("concurrent-download");
        let diagnostics = DownloadDiagnostics::start(&root, &run("current"));
        let barrier = Arc::new(Barrier::new(8));
        let mut threads = vec![];
        for worker in 0..8 {
            let d = diagnostics.clone();
            let b = barrier.clone();
            threads.push(thread::spawn(move || {
                b.wait();
                for item in 0..12 {
                    assert_eq!(
                        d.record(admission("current", &format!("job-{worker}-{item}"))),
                        DownloadDiagnosticEnqueue::Queued
                    );
                }
            }));
        }
        for handle in threads {
            handle.join().unwrap();
        }
        wait_lines(&root.join(DOWNLOAD_LOG_NAME), 96);
        let data = fs::read_to_string(root.join(DOWNLOAD_LOG_NAME)).unwrap();
        assert_eq!(data.lines().count(), 96);
        assert!(
            data.lines()
                .all(|line| serde_json::from_str::<DownloadDiagnosticEvent>(line).is_ok())
        );
        drop(diagnostics);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rotation_keeps_exactly_three_bounded_files() {
        let root = temp("rotation");
        fs::create_dir_all(&root).unwrap();
        let mut writer = None;
        let mut event = admission("run", "job");
        event.public_artifact_id = "a".repeat(120);
        let mut line = serde_json::to_vec(&event).unwrap();
        line.push(b'\n');
        for _ in 0..((DOWNLOAD_LOG_MAX_BYTES as usize / line.len() + 1) * 4) {
            write_download_line(&root, &mut writer, &line).unwrap();
        }
        drop(writer);
        for suffix in ["", ".1", ".2"] {
            let path = root.join(format!("{DOWNLOAD_LOG_NAME}{suffix}"));
            assert!(path.is_file());
            assert!(fs::metadata(path).unwrap().len() <= DOWNLOAD_LOG_MAX_BYTES);
        }
        assert!(!root.join(format!("{DOWNLOAD_LOG_NAME}.3")).exists());
        assert_eq!(fs::read_dir(&root).unwrap().count(), 3);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn startup_classifies_prior_nonterminal_jobs() {
        let root = temp("interrupted");
        fs::create_dir_all(&root).unwrap();
        let completed = DownloadDiagnosticEvent::completion(
            run("prior"),
            job("finished"),
            artifact(),
            DownloadSourceClass::ModelRepository,
            10,
            Some(10),
            None,
        );
        fs::write(
            root.join(DOWNLOAD_LOG_NAME),
            format!(
                "{}\n{}\n",
                serde_json::to_string(&admission("prior", "unfinished")).unwrap(),
                serde_json::to_string(&completed).unwrap()
            ),
        )
        .unwrap();
        let d = DownloadDiagnostics::start(&root, &run("current"));
        let snapshot = d.snapshot();
        let interrupted: Vec<_> = snapshot
            .iter()
            .filter(|e| e.outcome == DownloadDiagnosticOutcome::PriorRunInterruption)
            .collect();
        assert_eq!(interrupted.len(), 1);
        assert_eq!(interrupted[0].job_id, "unfinished");
        wait_lines(&root.join(DOWNLOAD_LOG_NAME), 3);
        drop(d);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn malformed_and_unknown_versions_are_ignored() {
        let root = temp("malformed");
        fs::create_dir_all(&root).unwrap();
        let mut unknown = serde_json::to_value(admission("prior", "unknown")).unwrap();
        unknown["schema_version"] = serde_json::json!(999);
        fs::write(
            root.join(DOWNLOAD_LOG_NAME),
            format!(
                "bad json\n{}\n{}\n",
                unknown,
                serde_json::to_string(&admission("prior", "valid")).unwrap()
            ),
        )
        .unwrap();
        let d = DownloadDiagnostics::start(&root, &run("current"));
        let snapshot = d.snapshot();
        assert!(snapshot.iter().any(|e| e.job_id == "valid"));
        assert!(!snapshot.iter().any(|e| e.job_id == "unknown"));
        drop(d);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn oversized_rotation_is_rejected_without_scanning() {
        let root = temp("oversized-log");
        fs::create_dir_all(&root).unwrap();
        let oversized = File::create(root.join(DOWNLOAD_LOG_NAME)).unwrap();
        oversized.set_len(DOWNLOAD_LOG_MAX_BYTES + 1).unwrap();
        drop(oversized);

        let diagnostics = DownloadDiagnostics::start(&root, &run("current"));
        assert!(diagnostics.snapshot().is_empty());
        assert_eq!(
            diagnostics.nonfatal_error(),
            Some(DownloadDiagnosticsError::StorageUnavailable)
        );
        assert_eq!(
            diagnostics.record(admission("current", "blocked-by-oversize")),
            DownloadDiagnosticEnqueue::Queued
        );
        assert_eq!(
            diagnostics.flush(Duration::from_secs(2)),
            DownloadDiagnosticFlush::StorageUnavailable
        );
        drop(diagnostics);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn overlong_and_unterminated_lines_are_discarded_with_bounded_work() {
        let root = temp("bounded-lines");
        fs::create_dir_all(&root).unwrap();
        let first = DownloadDiagnosticEvent::completion(
            run("prior"),
            job("first"),
            artifact(),
            DownloadSourceClass::ModelRepository,
            10,
            Some(10),
            None,
        );
        let second = DownloadDiagnosticEvent::completion(
            run("prior"),
            job("second"),
            artifact(),
            DownloadSourceClass::ModelRepository,
            10,
            Some(10),
            None,
        );
        let unterminated = DownloadDiagnosticEvent::completion(
            run("prior"),
            job("unterminated"),
            artifact(),
            DownloadSourceClass::ModelRepository,
            10,
            Some(10),
            None,
        );
        let mut contents = format!("{}\n", serde_json::to_string(&first).unwrap()).into_bytes();
        contents.extend(std::iter::repeat_n(b'x', DOWNLOAD_LOG_LINE_MAX_BYTES + 1));
        contents.push(b'\n');
        contents
            .extend_from_slice(format!("{}\n", serde_json::to_string(&second).unwrap()).as_bytes());
        contents.extend_from_slice(serde_json::to_string(&unterminated).unwrap().as_bytes());
        fs::write(root.join(DOWNLOAD_LOG_NAME), contents).unwrap();

        let diagnostics = DownloadDiagnostics::start(&root, &run("current"));
        let snapshot = diagnostics.snapshot();
        assert_eq!(snapshot.len(), 2);
        assert_eq!(snapshot[0].job_id, "first");
        assert_eq!(snapshot[1].job_id, "second");
        drop(diagnostics);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn explicit_flush_persists_queued_events_within_the_timeout() {
        let root = temp("flush");
        let diagnostics = DownloadDiagnostics::start(&root, &run("current"));
        assert_eq!(
            diagnostics.record(admission("current", "flush-job")),
            DownloadDiagnosticEnqueue::Queued
        );
        assert_eq!(
            diagnostics.flush(Duration::from_secs(2)),
            DownloadDiagnosticFlush::Flushed
        );
        assert_eq!(
            fs::read_to_string(root.join(DOWNLOAD_LOG_NAME))
                .unwrap()
                .lines()
                .count(),
            1
        );
        drop(diagnostics);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn linked_diagnostics_directory_is_rejected() {
        let root = temp("linked-directory");
        let target = root.join("target");
        let linked = root.join("linked");
        fs::create_dir_all(&target).unwrap();
        if create_directory_link(&target, &linked).is_err() {
            let _ = fs::remove_dir_all(root);
            return;
        }

        let diagnostics = DownloadDiagnostics::start(&linked, &run("current"));
        assert_eq!(
            diagnostics.nonfatal_error(),
            Some(DownloadDiagnosticsError::StorageUnavailable)
        );
        assert_eq!(
            diagnostics.flush(Duration::from_secs(2)),
            DownloadDiagnosticFlush::StorageUnavailable
        );
        assert!(!target.join(DOWNLOAD_LOG_NAME).exists());
        drop(diagnostics);
        remove_directory_link(&linked).unwrap();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn linked_log_generations_are_rejected_without_touching_targets() {
        let root = temp("linked-logs");
        for (index, generation) in [
            DOWNLOAD_LOG_NAME.to_owned(),
            format!("{DOWNLOAD_LOG_NAME}.1"),
            format!("{DOWNLOAD_LOG_NAME}.2"),
        ]
        .into_iter()
        .enumerate()
        {
            let directory = root.join(index.to_string());
            fs::create_dir_all(&directory).unwrap();
            let target = directory.join("outside-target");
            fs::write(&target, b"unchanged").unwrap();
            let linked = directory.join(generation);
            if create_file_link(&target, &linked).is_err() {
                let _ = fs::remove_dir_all(root);
                return;
            }

            let diagnostics = DownloadDiagnostics::start(&directory, &run("current"));
            assert_eq!(
                diagnostics.nonfatal_error(),
                Some(DownloadDiagnosticsError::StorageUnavailable)
            );
            assert_eq!(
                diagnostics.record(admission("current", "linked-log-job")),
                DownloadDiagnosticEnqueue::Queued
            );
            assert_eq!(
                diagnostics.flush(Duration::from_secs(2)),
                DownloadDiagnosticFlush::StorageUnavailable
            );
            assert_eq!(fs::read(&target).unwrap(), b"unchanged");
            if linked.file_name() != Some(std::ffi::OsStr::new(DOWNLOAD_LOG_NAME)) {
                assert!(!directory.join(DOWNLOAD_LOG_NAME).exists());
            }
            drop(diagnostics);
            let _ = fs::remove_file(linked);
        }
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn diagnostics_permissions_are_private_on_unix() {
        use std::os::unix::fs::PermissionsExt;

        let root = temp("private-permissions");
        let diagnostics = DownloadDiagnostics::start(&root, &run("current"));
        assert_eq!(
            diagnostics.record(admission("current", "private-job")),
            DownloadDiagnosticEnqueue::Queued
        );
        assert_eq!(
            diagnostics.flush(Duration::from_secs(2)),
            DownloadDiagnosticFlush::Flushed
        );
        assert_eq!(
            fs::metadata(&root).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(root.join(DOWNLOAD_LOG_NAME))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        drop(diagnostics);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn startup_loads_only_latest_hundred_events_across_rotations() {
        let root = temp("latest-hundred");
        fs::create_dir_all(&root).unwrap();
        let mut serialized = Vec::new();
        for index in 0..120 {
            let event = DownloadDiagnosticEvent::completion(
                run("prior"),
                job(&format!("job-{index}")),
                artifact(),
                DownloadSourceClass::ModelRepository,
                10,
                Some(10),
                None,
            );
            serialized.push(format!("{}\n", serde_json::to_string(&event).unwrap()));
        }
        fs::write(
            root.join(format!("{DOWNLOAD_LOG_NAME}.2")),
            serialized[..40].concat(),
        )
        .unwrap();
        fs::write(
            root.join(format!("{DOWNLOAD_LOG_NAME}.1")),
            serialized[40..80].concat(),
        )
        .unwrap();
        fs::write(root.join(DOWNLOAD_LOG_NAME), serialized[80..].concat()).unwrap();

        let diagnostics = DownloadDiagnostics::start(&root, &run("current"));
        let snapshot = diagnostics.snapshot();
        assert_eq!(snapshot.len(), 100);
        assert_eq!(snapshot.first().unwrap().job_id, "job-20");
        assert_eq!(snapshot.last().unwrap().job_id, "job-119");
        drop(diagnostics);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn duplicate_first_stalls_are_suppressed() {
        let root = temp("first-stall");
        let diagnostics = DownloadDiagnostics::start(&root, &run("current"));
        let stalled = || {
            DownloadDiagnosticEvent::first_stall(
                run("current"),
                job("job"),
                artifact(),
                DownloadSourceClass::ModelRepository,
                12,
                Some(100),
                DownloadFaultCategory::Timeout,
            )
        };
        assert_eq!(
            diagnostics.record(stalled()),
            DownloadDiagnosticEnqueue::Queued
        );
        assert_eq!(
            diagnostics.record(stalled()),
            DownloadDiagnosticEnqueue::IgnoredDuplicate
        );
        drop(diagnostics);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn io_failure_is_nonfatal_and_reported_once() {
        let root = temp("io-failure");
        fs::write(&root, b"file").unwrap();
        let d = DownloadDiagnostics::start(&root, &run("current"));
        assert_eq!(
            d.nonfatal_error(),
            Some(DownloadDiagnosticsError::StorageUnavailable)
        );
        assert!(
            d.take_new_nonfatal_error()
                .unwrap()
                .settings_diagnostic()
                .contains("continue normally")
        );
        assert_eq!(d.take_new_nonfatal_error(), None);
        drop(d);
        let _ = fs::remove_file(root);
    }

    #[test]
    fn logs_and_export_exclude_private_markers() {
        let root = temp("redaction");
        let log_dir = root.join("logs");
        let export_dir = root.join("export");
        let d = DownloadDiagnostics::start(&log_dir, &run("current"));
        assert_eq!(
            d.record(DownloadDiagnosticEvent::failure(
                run("current"),
                job("job"),
                artifact(),
                DownloadSourceClass::ModelRepository,
                12,
                Some(100),
                Some(2),
                DownloadFaultCategory::Connectivity
            )),
            DownloadDiagnosticEnqueue::Queued
        );
        wait_lines(&log_dir.join(DOWNLOAD_LOG_NAME), 1);
        let log = fs::read_to_string(log_dir.join(DOWNLOAD_LOG_NAME)).unwrap();
        let mut store = DiagnosticsStore::default();
        store.record(diagnostic(9));
        store.attach_download_diagnostics(&d);
        let export = fs::read_to_string(export_redacted(&export_dir, &store).unwrap()).unwrap();
        assert!(export.contains("\"download_events\""));
        for marker in [
            "PRIVATE_TRANSCRIPT",
            "secret-token",
            "C:\\Users\\patient\\audio.wav",
            "https://private.example",
            "Authorization",
            "response body",
        ] {
            assert!(!log.contains(marker));
            assert!(!export.contains(marker));
        }
        drop(d);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn identifiers_reject_sensitive_shapes() {
        for marker in [
            "https://example.test/model",
            "C:\\Users\\patient\\audio.wav",
            "Bearer secret-token",
            "../transcript.txt",
            "response body",
        ] {
            assert!(DownloadRunId::new(marker).is_err());
            assert!(DownloadJobId::new(marker).is_err());
            assert!(PublicArtifactId::new(marker).is_err());
        }
    }

    #[test]
    fn download_event_schema_contains_only_allowlisted_fields() {
        let value = serde_json::to_value(admission("run", "job")).unwrap();
        let keys: std::collections::BTreeSet<_> = value
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            keys,
            std::collections::BTreeSet::from([
                "app_version",
                "completed_bytes",
                "fault_category",
                "job_id",
                "outcome",
                "public_artifact_id",
                "retry_number",
                "run_id",
                "schema_version",
                "source_class",
                "timestamp_unix_ms",
                "total_bytes",
            ])
        );
    }

    #[test]
    fn export_contains_no_private_marker_from_process_state() {
        let root = temp("redacted-diagnostics");
        let private_marker = "PRIVATE_TRANSCRIPT secret-token C:\\Users\\patient\\audio.wav";
        let mut store = DiagnosticsStore::default();
        store.record(diagnostic(9));

        let path = export_redacted(&root, &store).unwrap();
        let json = fs::read_to_string(path).unwrap();
        assert!(!json.contains(private_marker));
        assert!(!json.contains("PRIVATE_TRANSCRIPT"));
        assert!(!json.contains("secret-token"));
        assert!(!json.contains("patient"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn export_io_failure_preserves_sessions() {
        let root = temp("export-failure");
        fs::write(&root, b"file").unwrap();
        let mut store = DiagnosticsStore::default();
        store.record(diagnostic(10));
        assert!(export_redacted(&root, &store).is_err());
        assert_eq!(store.len(), 1);
        let _ = fs::remove_file(root);
    }
}
