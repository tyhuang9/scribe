use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

pub const MAX_PENDING_PREVIEW_CHUNKS: usize = 2;
pub const PREVIEW_CHUNK_DURATION_MS: u64 = 5_000;
pub const PREVIEW_CHUNK_OVERLAP_MS: u64 = 500;

static NEXT_SESSION_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct PreviewSessionId(u64);

impl PreviewSessionId {
    pub fn next() -> Self {
        Self(NEXT_SESSION_ID.fetch_add(1, Ordering::Relaxed))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PreviewJobKey {
    pub session_id: PreviewSessionId,
    pub sequence: u64,
}

#[derive(Debug)]
pub struct PreviewArtifact {
    key: PreviewJobKey,
    path: PathBuf,
}

impl PreviewArtifact {
    pub(crate) fn new(session_id: PreviewSessionId, sequence: u64, path: PathBuf) -> Self {
        Self {
            key: PreviewJobKey {
                session_id,
                sequence,
            },
            path,
        }
    }

    pub fn key(&self) -> PreviewJobKey {
        self.key
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for PreviewArtifact {
    fn drop(&mut self) {
        if let Err(error) = fs::remove_file(&self.path)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            eprintln!(
                "failed to remove live preview chunk {}: {error}",
                self.path.display()
            );
        }
    }
}

#[derive(Clone, Debug)]
pub struct PreviewCancellation(Arc<AtomicBool>);

impl PreviewCancellation {
    fn new() -> Self {
        Self(Arc::new(AtomicBool::new(false)))
    }

    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

pub struct PreviewJob {
    artifact: PreviewArtifact,
    cancellation: PreviewCancellation,
}

impl PreviewJob {
    pub fn key(&self) -> PreviewJobKey {
        self.artifact.key()
    }

    pub fn path(&self) -> &Path {
        self.artifact.path()
    }

    pub fn cancellation(&self) -> PreviewCancellation {
        self.cancellation.clone()
    }
}

#[derive(Debug, Eq, PartialEq)]
pub enum PreviewCompletion {
    Applied,
    Failed(String),
    Stale,
}

struct InFlightPreview {
    key: PreviewJobKey,
    cancellation: PreviewCancellation,
}

#[derive(Default)]
pub struct LivePreviewState {
    active_session: Option<PreviewSessionId>,
    in_flight: Option<InFlightPreview>,
    pending: Option<PreviewArtifact>,
    latest_sequence: Option<u64>,
    provisional_text: String,
}

impl LivePreviewState {
    pub fn begin_session(&mut self) -> PreviewSessionId {
        let session_id = PreviewSessionId::next();
        self.cancel_in_flight();
        self.pending = None;
        self.active_session = Some(session_id);
        self.latest_sequence = None;
        self.provisional_text.clear();
        session_id
    }

    pub fn offer(&mut self, artifact: PreviewArtifact) -> bool {
        let key = artifact.key();
        if self.active_session != Some(key.session_id)
            || self
                .latest_sequence
                .is_some_and(|sequence| key.sequence <= sequence)
        {
            return false;
        }

        self.latest_sequence = Some(key.sequence);
        // Each partial is a complete overlapping window, so keep only the newest
        // opportunity instead of appending or building a backlog.
        self.pending = Some(artifact);
        true
    }

    pub fn take_next_job(&mut self) -> Option<PreviewJob> {
        if self.in_flight.is_some() {
            return None;
        }
        let artifact = self.pending.take()?;
        if self.active_session != Some(artifact.key().session_id) {
            return None;
        }
        let cancellation = PreviewCancellation::new();
        self.in_flight = Some(InFlightPreview {
            key: artifact.key(),
            cancellation: cancellation.clone(),
        });
        Some(PreviewJob {
            artifact,
            cancellation,
        })
    }

    pub fn complete(
        &mut self,
        key: PreviewJobKey,
        result: Result<String, String>,
    ) -> PreviewCompletion {
        let Some(in_flight) = self.in_flight.take() else {
            return PreviewCompletion::Stale;
        };
        if in_flight.key != key {
            self.in_flight = Some(in_flight);
            return PreviewCompletion::Stale;
        }
        if self.active_session != Some(key.session_id) {
            return PreviewCompletion::Stale;
        }

        match result {
            Ok(text) => {
                self.provisional_text = text.trim().to_owned();
                PreviewCompletion::Applied
            }
            Err(message) => PreviewCompletion::Failed(message),
        }
    }

    pub fn stop_session(&mut self, session_id: PreviewSessionId) {
        if self.active_session != Some(session_id) {
            return;
        }
        self.active_session = None;
        self.pending = None;
        self.cancel_in_flight();
    }

    pub fn final_wins(&mut self) {
        self.active_session = None;
        self.pending = None;
        self.cancel_in_flight();
        self.provisional_text.clear();
    }

    pub fn provisional_text(&self) -> &str {
        &self.provisional_text
    }

    pub fn has_in_flight_job(&self) -> bool {
        self.in_flight.is_some()
    }

    fn cancel_in_flight(&self) {
        if let Some(in_flight) = &self.in_flight {
            in_flight.cancellation.cancel();
        }
    }
}

impl Drop for LivePreviewState {
    fn drop(&mut self) {
        self.cancel_in_flight();
    }
}

pub fn is_live_preview_eligible(
    enabled: bool,
    is_transcribe_recording: bool,
    backend: &str,
    model_ready: bool,
) -> bool {
    enabled && is_transcribe_recording && backend == "whisper.cpp" && model_ready
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    struct FakePreviewRunner {
        jobs: Vec<PreviewJob>,
    }

    impl FakePreviewRunner {
        fn dispatch(&mut self, state: &mut LivePreviewState) {
            if let Some(job) = state.take_next_job() {
                self.jobs.push(job);
            }
        }
    }

    fn artifact(session_id: PreviewSessionId, sequence: u64) -> PreviewArtifact {
        let path = std::env::temp_dir().join(format!(
            "scribe-live-preview-state-{}-{}-{}-{}.wav",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            session_id.0,
            sequence
        ));
        fs::write(&path, b"preview").unwrap();
        PreviewArtifact::new(session_id, sequence, path)
    }

    #[test]
    fn fake_runner_enforces_one_job_and_coalesces_to_latest_chunk() {
        let mut state = LivePreviewState::default();
        let session = state.begin_session();
        let mut runner = FakePreviewRunner { jobs: Vec::new() };
        state.offer(artifact(session, 1));
        runner.dispatch(&mut state);
        assert_eq!(runner.jobs.len(), 1);

        let superseded = artifact(session, 2);
        let superseded_path = superseded.path().to_path_buf();
        state.offer(superseded);
        state.offer(artifact(session, 3));
        runner.dispatch(&mut state);

        assert_eq!(runner.jobs.len(), 1);
        assert!(!superseded_path.exists());
        let first_key = runner.jobs.remove(0).key();
        assert_eq!(
            state.complete(first_key, Ok("first".to_owned())),
            PreviewCompletion::Applied
        );
        runner.dispatch(&mut state);
        assert_eq!(runner.jobs[0].key().sequence, 3);
    }

    #[test]
    fn stale_session_and_sequence_results_cannot_replace_preview() {
        let mut state = LivePreviewState::default();
        let first = state.begin_session();
        state.offer(artifact(first, 1));
        let old_job = state.take_next_job().unwrap();
        let old_key = old_job.key();

        let second = state.begin_session();
        assert!(old_job.cancellation().is_cancelled());
        state.offer(artifact(second, 1));
        assert_eq!(
            state.complete(old_key, Ok("obsolete".to_owned())),
            PreviewCompletion::Stale
        );
        assert!(state.provisional_text().is_empty());

        let job = state.take_next_job().unwrap();
        let key = job.key();
        assert_eq!(
            state.complete(key, Ok("current".to_owned())),
            PreviewCompletion::Applied
        );
        assert_eq!(state.provisional_text(), "current");
        assert!(!state.offer(artifact(second, 1)));
    }

    #[test]
    fn stop_cancels_work_and_final_output_clears_provisional_text() {
        let mut state = LivePreviewState::default();
        let session = state.begin_session();
        state.offer(artifact(session, 1));
        let job = state.take_next_job().unwrap();
        let key = job.key();
        state.stop_session(session);

        assert!(job.cancellation().is_cancelled());
        assert_eq!(
            state.complete(key, Ok("too late".to_owned())),
            PreviewCompletion::Stale
        );
        state.final_wins();
        assert!(state.provisional_text().is_empty());
    }

    #[test]
    fn preview_failure_keeps_recording_state_usable() {
        let mut state = LivePreviewState::default();
        let session = state.begin_session();
        state.offer(artifact(session, 1));
        let job = state.take_next_job().unwrap();
        state.offer(artifact(session, 2));

        assert_eq!(
            state.complete(job.key(), Err("runner failed".to_owned())),
            PreviewCompletion::Failed("runner failed".to_owned())
        );
        assert!(state.provisional_text().is_empty());
        assert_eq!(state.take_next_job().unwrap().key().sequence, 2);
    }

    #[test]
    fn eligibility_excludes_playground_non_whisper_and_unready_models() {
        assert!(is_live_preview_eligible(true, true, "whisper.cpp", true));
        assert!(!is_live_preview_eligible(true, false, "whisper.cpp", true));
        assert!(!is_live_preview_eligible(true, true, "Vosk", true));
        assert!(!is_live_preview_eligible(true, true, "whisper.cpp", false));
        assert!(!is_live_preview_eligible(false, true, "whisper.cpp", true));
    }

    #[test]
    fn pending_and_finished_artifacts_are_removed_by_ownership() {
        let mut state = LivePreviewState::default();
        let session = state.begin_session();
        let pending = artifact(session, 1);
        let pending_path = pending.path().to_path_buf();
        state.offer(pending);
        state.stop_session(session);
        assert!(!pending_path.exists());

        let session = state.begin_session();
        let running = artifact(session, 1);
        let running_path = running.path().to_path_buf();
        state.offer(running);
        let job = state.take_next_job().unwrap();
        drop(job);
        assert!(!running_path.exists());
    }
}
