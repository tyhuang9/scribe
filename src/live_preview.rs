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

impl PreviewJobKey {
    pub fn chunk_offset_ms(self) -> u64 {
        self.sequence
            .saturating_mul(PREVIEW_CHUNK_DURATION_MS - PREVIEW_CHUNK_OVERLAP_MS)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PreviewSegment {
    pub start_ms: Option<u64>,
    pub end_ms: Option<u64>,
    pub text: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PreviewTranscript {
    pub text: String,
    pub segments: Vec<PreviewSegment>,
}

impl PreviewTranscript {
    #[cfg(test)]
    pub fn untimed(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            segments: Vec::new(),
        }
    }
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
    pub(crate) fn new() -> Self {
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

#[derive(Clone, Debug, Eq, PartialEq)]
struct TimedWord {
    start_ms: u64,
    end_ms: u64,
    text: String,
}

#[derive(Default)]
pub struct LivePreviewState {
    active_session: Option<PreviewSessionId>,
    in_flight: Option<InFlightPreview>,
    pending: Option<PreviewArtifact>,
    latest_sequence: Option<u64>,
    applied_sequence: Option<u64>,
    timed_words: Vec<TimedWord>,
    untimed_mode: bool,
    provisional_text: String,
}

impl LivePreviewState {
    pub fn begin_session(&mut self) -> PreviewSessionId {
        let session_id = PreviewSessionId::next();
        self.cancel_in_flight();
        self.pending = None;
        self.active_session = Some(session_id);
        self.latest_sequence = None;
        self.applied_sequence = None;
        self.timed_words.clear();
        self.untimed_mode = false;
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
        result: Result<PreviewTranscript, String>,
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
        if self
            .applied_sequence
            .is_some_and(|sequence| key.sequence <= sequence)
        {
            return PreviewCompletion::Stale;
        }

        match result {
            Ok(transcript) => {
                self.merge_transcript(key, transcript);
                self.applied_sequence = Some(key.sequence);
                PreviewCompletion::Applied
            }
            Err(message) => {
                self.timed_words.clear();
                self.untimed_mode = false;
                self.provisional_text.clear();
                PreviewCompletion::Failed(message)
            }
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
        self.timed_words.clear();
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

    fn merge_transcript(&mut self, key: PreviewJobKey, transcript: PreviewTranscript) {
        let timed_words = transcript
            .segments
            .iter()
            .flat_map(|segment| timed_segment_words(key.chunk_offset_ms(), segment))
            .collect::<Vec<_>>();
        if !self.untimed_mode && !timed_words.is_empty() {
            self.timed_words
                .retain(|existing| !timed_words.iter().any(|new| words_overlap(existing, new)));
            self.timed_words.extend(timed_words);
            self.timed_words
                .sort_by_key(|word| (word.start_ms, word.end_ms));
            self.provisional_text = self
                .timed_words
                .iter()
                .map(|word| word.text.as_str())
                .collect::<Vec<_>>()
                .join(" ");
            return;
        }

        self.timed_words.clear();
        self.untimed_mode = true;
        merge_untimed_text(&mut self.provisional_text, &transcript.text);
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

fn timed_segment_words(chunk_offset_ms: u64, segment: &PreviewSegment) -> Vec<TimedWord> {
    let (Some(start_ms), Some(end_ms)) = (segment.start_ms, segment.end_ms) else {
        return Vec::new();
    };
    if end_ms <= start_ms {
        return Vec::new();
    }
    let words = segment.text.split_whitespace().collect::<Vec<_>>();
    let Ok(word_count) = u64::try_from(words.len()) else {
        return Vec::new();
    };
    if word_count == 0 {
        return Vec::new();
    }
    let duration = end_ms - start_ms;
    words
        .into_iter()
        .enumerate()
        .map(|(index, text)| {
            let index = index as u64;
            let word_start = duration.saturating_mul(index) / word_count;
            let word_end = duration.saturating_mul(index + 1) / word_count;
            TimedWord {
                start_ms: chunk_offset_ms
                    .saturating_add(start_ms)
                    .saturating_add(word_start),
                end_ms: chunk_offset_ms
                    .saturating_add(start_ms)
                    .saturating_add(word_end),
                text: text.to_owned(),
            }
        })
        .collect()
}

fn words_overlap(left: &TimedWord, right: &TimedWord) -> bool {
    left.start_ms < right.end_ms && right.start_ms < left.end_ms
}

fn merge_untimed_text(cumulative: &mut String, chunk: &str) {
    let chunk_words = chunk.split_whitespace().collect::<Vec<_>>();
    if chunk_words.is_empty() {
        return;
    }
    let cumulative_words = cumulative.split_whitespace().collect::<Vec<_>>();
    let max_overlap = cumulative_words.len().min(chunk_words.len());
    let overlap = (1..=max_overlap)
        .rev()
        .find(|&count| {
            cumulative_words[cumulative_words.len() - count..]
                .iter()
                .zip(&chunk_words[..count])
                .all(|(left, right)| normalize_word(left) == normalize_word(right))
        })
        .unwrap_or(0);
    let addition = chunk_words[overlap..].join(" ");
    if addition.is_empty() {
        return;
    }
    if !cumulative.is_empty() {
        cumulative.push(' ');
    }
    cumulative.push_str(&addition);
}

fn normalize_word(word: &str) -> String {
    let normalized = word
        .chars()
        .filter(|character| character.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect::<String>();
    if normalized.is_empty() {
        word.to_lowercase()
    } else {
        normalized
    }
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

    fn untimed(text: &str) -> PreviewTranscript {
        PreviewTranscript::untimed(text)
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
            state.complete(first_key, Ok(untimed("first"))),
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
            state.complete(old_key, Ok(untimed("obsolete"))),
            PreviewCompletion::Stale
        );
        assert!(state.provisional_text().is_empty());

        let job = state.take_next_job().unwrap();
        let key = job.key();
        assert_eq!(
            state.complete(key, Ok(untimed("current"))),
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
            state.complete(key, Ok(untimed("too late"))),
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
        let first = state.take_next_job().unwrap();
        assert_eq!(
            state.complete(first.key(), Ok(untimed("stale cumulative words"))),
            PreviewCompletion::Applied
        );
        state.offer(artifact(session, 2));
        let failed = state.take_next_job().unwrap();
        state.offer(artifact(session, 3));

        assert_eq!(
            state.complete(failed.key(), Err("runner failed".to_owned())),
            PreviewCompletion::Failed("runner failed".to_owned())
        );
        assert!(state.provisional_text().is_empty());
        let recovery = state.take_next_job().unwrap();
        assert_eq!(recovery.key().sequence, 3);
        assert_eq!(
            state.complete(recovery.key(), Ok(untimed("fresh words"))),
            PreviewCompletion::Applied
        );
        assert_eq!(state.provisional_text(), "fresh words");
    }

    #[test]
    fn preview_failure_clears_timed_merge_state() {
        let mut state = LivePreviewState::default();
        let session = state.begin_session();
        state.offer(artifact(session, 0));
        let first = state.take_next_job().unwrap();
        state.complete(
            first.key(),
            Ok(PreviewTranscript {
                text: "stale timed words".to_owned(),
                segments: vec![PreviewSegment {
                    start_ms: Some(0),
                    end_ms: Some(1_000),
                    text: "stale timed words".to_owned(),
                }],
            }),
        );
        state.offer(artifact(session, 1));
        let failed = state.take_next_job().unwrap();
        assert_eq!(
            state.complete(failed.key(), Err("runner failed".to_owned())),
            PreviewCompletion::Failed("runner failed".to_owned())
        );

        state.offer(artifact(session, 2));
        let recovery = state.take_next_job().unwrap();
        state.complete(
            recovery.key(),
            Ok(PreviewTranscript {
                text: "fresh timed words".to_owned(),
                segments: vec![PreviewSegment {
                    start_ms: Some(0),
                    end_ms: Some(1_000),
                    text: "fresh timed words".to_owned(),
                }],
            }),
        );

        assert_eq!(state.provisional_text(), "fresh timed words");
    }

    #[test]
    fn repeated_boundary_uses_the_longest_overlap_deterministically() {
        let mut state = LivePreviewState::default();
        let session = state.begin_session();
        state.offer(artifact(session, 1));
        let first = state.take_next_job().unwrap();
        assert_eq!(
            state.complete(first.key(), Ok(untimed("go now go now"))),
            PreviewCompletion::Applied
        );
        state.offer(artifact(session, 2));
        let second = state.take_next_job().unwrap();
        assert_eq!(
            state.complete(second.key(), Ok(untimed("go now again"))),
            PreviewCompletion::Applied
        );

        assert_eq!(state.provisional_text(), "go now go now again");
    }

    #[test]
    fn untimed_chunks_append_when_there_is_no_overlap() {
        let mut state = LivePreviewState::default();
        let session = state.begin_session();
        state.offer(artifact(session, 0));
        let first = state.take_next_job().unwrap();
        state.complete(first.key(), Ok(untimed("first thought")));
        state.offer(artifact(session, 1));
        let second = state.take_next_job().unwrap();
        state.complete(second.key(), Ok(untimed("new words")));

        assert_eq!(state.provisional_text(), "first thought new words");
    }

    #[test]
    fn untimed_overlap_normalizes_case_and_punctuation_but_preserves_original_words() {
        let mut state = LivePreviewState::default();
        let session = state.begin_session();
        state.offer(artifact(session, 0));
        let first = state.take_next_job().unwrap();
        state.complete(first.key(), Ok(untimed("Keep This, Boundary!")));
        state.offer(artifact(session, 1));
        let second = state.take_next_job().unwrap();
        state.complete(second.key(), Ok(untimed("this boundary continues Here.")));

        assert_eq!(
            state.provisional_text(),
            "Keep This, Boundary! continues Here."
        );
    }

    #[test]
    fn timed_segments_replace_only_the_overlapping_absolute_timeline() {
        let mut state = LivePreviewState::default();
        let session = state.begin_session();
        state.offer(artifact(session, 0));
        let first = state.take_next_job().unwrap();
        state.complete(
            first.key(),
            Ok(PreviewTranscript {
                text: "old beginning old boundary".to_owned(),
                segments: vec![PreviewSegment {
                    start_ms: Some(0),
                    end_ms: Some(5_000),
                    text: "old beginning old boundary".to_owned(),
                }],
            }),
        );
        state.offer(artifact(session, 1));
        let second = state.take_next_job().unwrap();
        state.complete(
            second.key(),
            Ok(PreviewTranscript {
                text: "new boundary continues onward".to_owned(),
                segments: vec![PreviewSegment {
                    start_ms: Some(0),
                    end_ms: Some(4_000),
                    text: "new boundary continues onward".to_owned(),
                }],
            }),
        );

        assert_eq!(
            state.provisional_text(),
            "old beginning old new boundary continues onward"
        );
    }

    #[test]
    fn mixed_timing_permanently_downgrades_without_erasing_cumulative_text() {
        let mut state = LivePreviewState::default();
        let session = state.begin_session();
        state.offer(artifact(session, 0));
        let first = state.take_next_job().unwrap();
        state.complete(
            first.key(),
            Ok(PreviewTranscript {
                text: "alpha boundary".to_owned(),
                segments: vec![PreviewSegment {
                    start_ms: Some(0),
                    end_ms: Some(5_000),
                    text: "alpha boundary".to_owned(),
                }],
            }),
        );
        state.offer(artifact(session, 1));
        let second = state.take_next_job().unwrap();
        state.complete(second.key(), Ok(untimed("boundary middle")));
        state.offer(artifact(session, 2));
        let third = state.take_next_job().unwrap();
        state.complete(
            third.key(),
            Ok(PreviewTranscript {
                text: "middle final".to_owned(),
                segments: vec![PreviewSegment {
                    start_ms: Some(0),
                    end_ms: Some(5_000),
                    text: "middle final".to_owned(),
                }],
            }),
        );

        assert_eq!(state.provisional_text(), "alpha boundary middle final");
    }

    #[test]
    fn untimed_then_timed_uses_cumulative_word_overlap() {
        let mut state = LivePreviewState::default();
        let session = state.begin_session();
        state.offer(artifact(session, 0));
        let first = state.take_next_job().unwrap();
        state.complete(first.key(), Ok(untimed("alpha boundary")));
        state.offer(artifact(session, 1));
        let second = state.take_next_job().unwrap();
        state.complete(
            second.key(),
            Ok(PreviewTranscript {
                text: "boundary final".to_owned(),
                segments: vec![PreviewSegment {
                    start_ms: Some(0),
                    end_ms: Some(5_000),
                    text: "boundary final".to_owned(),
                }],
            }),
        );

        assert_eq!(state.provisional_text(), "alpha boundary final");
    }

    #[test]
    fn out_of_order_sequence_cannot_mutate_cumulative_text() {
        let mut state = LivePreviewState::default();
        let session = state.begin_session();
        state.offer(artifact(session, 2));
        let current = state.take_next_job().unwrap();
        let stale_key = PreviewJobKey {
            session_id: session,
            sequence: 1,
        };

        assert_eq!(
            state.complete(stale_key, Ok(untimed("stale"))),
            PreviewCompletion::Stale
        );
        assert!(state.provisional_text().is_empty());
        assert_eq!(
            state.complete(current.key(), Ok(untimed("current"))),
            PreviewCompletion::Applied
        );
        assert_eq!(state.provisional_text(), "current");
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
