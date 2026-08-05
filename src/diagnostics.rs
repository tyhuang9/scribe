use std::collections::VecDeque;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

const DIAGNOSTIC_SCHEMA_VERSION: u32 = 1;
const MAX_SESSION_SNAPSHOTS: usize = 50;

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
    pub metrics: SessionMetrics,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct DiagnosticsStore {
    sessions: VecDeque<SessionDiagnostic>,
}

impl DiagnosticsStore {
    pub fn record(&mut self, diagnostic: SessionDiagnostic) {
        if let Some(existing) = self
            .sessions
            .iter()
            .position(|entry| entry.session_id == diagnostic.session_id)
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
                attempt = attempt.saturating_add(1);
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
            metrics: SessionMetrics {
                model_load_ms: Some(0),
                ..SessionMetrics::default()
            },
        }
    }

    #[test]
    fn report_is_allowlisted_and_marks_private_content_excluded() {
        let mut store = DiagnosticsStore::default();
        store.record(diagnostic(7));
        let json = serde_json::to_string_pretty(&store.report()).unwrap();

        assert!(json.contains("\"transcript_content_included\": false"));
        assert!(json.contains("\"audio_content_included\": false"));
        assert!(json.contains("\"secrets_included\": false"));
        assert!(!json.contains("transcript_text"));
        assert!(!json.contains("audio_path"));
        assert!(!json.contains("stdout"));
        assert!(!json.contains("stderr"));
        assert!(!json.contains("api_key"));
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

        assert_eq!(store.len(), MAX_SESSION_SNAPSHOTS);
        assert_eq!(store.sessions.back().unwrap().session_id, 60);
        assert!(store.sessions.iter().all(|entry| entry.session_id >= 11));
    }

    #[test]
    fn export_contains_no_private_marker_from_process_state() {
        let root = std::env::temp_dir().join(format!(
            "scribe-redacted-diagnostics-{}-{}",
            std::process::id(),
            unix_time_ms()
        ));
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
    fn export_io_failure_preserves_session_snapshots() {
        let root = std::env::temp_dir().join(format!(
            "scribe-diagnostics-file-parent-{}-{}",
            std::process::id(),
            unix_time_ms()
        ));
        fs::write(&root, b"not a directory").unwrap();
        let mut store = DiagnosticsStore::default();
        store.record(diagnostic(10));

        let error = export_redacted(&root, &store).unwrap_err();

        assert!(matches!(
            error.kind(),
            io::ErrorKind::AlreadyExists | io::ErrorKind::NotADirectory
        ));
        assert_eq!(store.len(), 1);
        assert_eq!(store.sessions.front().unwrap().session_id, 10);
        let _ = fs::remove_file(root);
    }
}
