use std::collections::{HashMap, HashSet, VecDeque};
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

use crossbeam_channel::{Receiver, Sender, TrySendError};
use serde::{Deserialize, Serialize};

const DIAGNOSTIC_SCHEMA_VERSION: u32 = 3;
const DOWNLOAD_EVENT_SCHEMA_VERSION: u32 = 1;
const MAX_SESSION_SNAPSHOTS: usize = 50;
const MAX_DOWNLOAD_SNAPSHOTS: usize = 100;
const DOWNLOAD_CHANNEL_CAPACITY: usize = 128;
const DOWNLOAD_LOG_MAX_BYTES: u64 = 1024 * 1024;
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
}

#[derive(Clone, Debug)]
pub(crate) struct DownloadDiagnostics {
    sender: Sender<DownloadDiagnosticEvent>,
    state: Arc<DownloadDiagnosticsState>,
}

impl DownloadDiagnostics {
    pub(crate) fn start(directory: impl Into<PathBuf>, current_run: &DownloadRunId) -> Self {
        let directory = directory.into();
        let state = Arc::new(DownloadDiagnosticsState::default());
        let loaded = load_download_events(&directory, &state);
        let interruptions = classify_prior_run_interruptions(&loaded, current_run);
        for event in loaded.into_iter().chain(interruptions.iter().cloned()) {
            state.append(event);
        }
        let (sender, receiver) = crossbeam_channel::bounded(DOWNLOAD_CHANNEL_CAPACITY);
        let writer_state = Arc::clone(&state);
        if std::thread::Builder::new()
            .name("download-diagnostics".into())
            .spawn(move || run_download_writer(directory, receiver, writer_state))
            .is_err()
        {
            state.report_error(DownloadDiagnosticsError::WriterUnavailable);
        }
        let result = Self { sender, state };
        for event in interruptions {
            let _ = result.sender.try_send(event);
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
        match self.sender.try_send(event.clone()) {
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
        *self.state.error.lock().unwrap_or_else(|e| e.into_inner())
    }
    pub(crate) fn take_new_nonfatal_error(&self) -> Option<DownloadDiagnosticsError> {
        let error = self.nonfatal_error()?;
        (!self.state.error_observed.swap(true, Ordering::AcqRel)).then_some(error)
    }
}

fn load_download_events(
    directory: &Path,
    state: &DownloadDiagnosticsState,
) -> Vec<DownloadDiagnosticEvent> {
    if fs::create_dir_all(directory).is_err() {
        state.report_error(DownloadDiagnosticsError::StorageUnavailable);
        return Vec::new();
    }
    let mut events = VecDeque::new();
    for path in [
        directory.join(format!("{DOWNLOAD_LOG_NAME}.2")),
        directory.join(format!("{DOWNLOAD_LOG_NAME}.1")),
        directory.join(DOWNLOAD_LOG_NAME),
    ] {
        let file = match File::open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(_) => {
                state.report_error(DownloadDiagnosticsError::StorageUnavailable);
                continue;
            }
        };
        for line in BufReader::new(file).lines() {
            let Ok(line) = line else {
                state.report_error(DownloadDiagnosticsError::StorageUnavailable);
                break;
            };
            let Ok(event) = serde_json::from_str::<DownloadDiagnosticEvent>(&line) else {
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
    }
    events.into_iter().collect()
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
    directory: PathBuf,
    receiver: Receiver<DownloadDiagnosticEvent>,
    state: Arc<DownloadDiagnosticsState>,
) {
    let mut writer = None;
    for event in receiver {
        let Ok(mut line) = serde_json::to_vec(&event) else {
            continue;
        };
        line.push(b'\n');
        if write_download_line(&directory, &mut writer, &line).is_err() {
            state.report_error(DownloadDiagnosticsError::StorageUnavailable);
            writer = None;
        }
    }
    if let Some(mut file) = writer {
        let _ = file.flush();
    }
}

fn write_download_line(directory: &Path, writer: &mut Option<File>, line: &[u8]) -> io::Result<()> {
    fs::create_dir_all(directory)?;
    if writer.is_none() {
        *writer = Some(open_download_log(directory)?);
    }
    let length = writer
        .as_ref()
        .expect("writer initialized")
        .metadata()?
        .len();
    if length.saturating_add(line.len() as u64) > DOWNLOAD_LOG_MAX_BYTES {
        *writer = None;
        rotate_download_logs(directory)?;
        *writer = Some(open_download_log(directory)?);
    }
    writer.as_mut().expect("writer initialized").write_all(line)
}

fn open_download_log(directory: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.append(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(directory.join(DOWNLOAD_LOG_NAME))
}

fn rotate_download_logs(directory: &Path) -> io::Result<()> {
    let oldest = directory.join(format!("{DOWNLOAD_LOG_NAME}.2"));
    match fs::remove_file(&oldest) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    rename_if_present(&directory.join(format!("{DOWNLOAD_LOG_NAME}.1")), &oldest)?;
    rename_if_present(
        &directory.join(DOWNLOAD_LOG_NAME),
        &directory.join(format!("{DOWNLOAD_LOG_NAME}.1")),
    )
}

fn rename_if_present(from: &Path, to: &Path) -> io::Result<()> {
    match fs::rename(from, to) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
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
    fs::create_dir_all(directory)?;
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
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        match options.open(&path) {
            Ok(mut file) => {
                serde_json::to_writer_pretty(&mut file, &diagnostics.report())
                    .map_err(io::Error::other)?;
                file.write_all(b"\n")?;
                file.sync_all()?;
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
