use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use rusqlite::{Connection, params};

use super::*;
use crate::prepared_audio::PREPARED_SAMPLE_RATE;

static TEST_SEQUENCE: AtomicU64 = AtomicU64::new(0);

struct TestRoot(PathBuf);

impl TestRoot {
    fn new(name: &str) -> Self {
        let sequence = TEST_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "scribe-history-{name}-{}-{sequence}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}

impl AsRef<Path> for TestRoot {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn metrics() -> HistoryMetrics {
    HistoryMetrics {
        audio_duration_ms: Some(100),
        processing_duration_ms: Some(40),
        realtime_factor: Some(0.4),
    }
}

fn new_entry(raw_text: &str, model_id: &str) -> NewHistoryEntry {
    NewHistoryEntry {
        raw_text: raw_text.into(),
        model_id: model_id.into(),
        source_app: Some("editor".into()),
        metrics: metrics(),
    }
}

fn completion(raw_text: &str, final_text: &str) -> CompletedHistoryEntry {
    CompletedHistoryEntry {
        raw_text: raw_text.into(),
        final_text: final_text.into(),
        metrics: metrics(),
    }
}

fn prepared_audio() -> PreparedAudio {
    PreparedAudio {
        samples: vec![-1.0, -0.25, 0.0, 0.25, 1.0],
        sample_rate: PREPARED_SAMPLE_RATE,
        source_sample_rate: PREPARED_SAMPLE_RATE,
        source_channels: 1,
        source_frames: 5,
    }
}

fn policy(max_unpinned_entries: u32) -> HistoryRetentionPolicy {
    HistoryRetentionPolicy {
        max_unpinned_entries,
        ..HistoryRetentionPolicy::default()
    }
}

fn completed(store: &HistoryStore, text: &str) -> HistoryRecord {
    let pending = store
        .create_pending(new_entry(text, "runtime-neutral-model"), None)
        .unwrap();
    store.complete(pending.id, completion(text, text)).unwrap()
}

#[test]
fn lifecycle_persists_pending_completed_and_failed_records() {
    let root = TestRoot::new("lifecycle");
    let store = HistoryStore::open(&root, policy(100)).unwrap();
    let pending = store
        .create_pending(new_entry("raw", "model-a"), None)
        .unwrap();
    assert_eq!(pending.status, HistoryStatus::Pending);
    assert_eq!(pending.raw_text, "raw");
    assert_eq!(pending.metrics, metrics());
    assert_eq!(pending.source_app.as_deref(), Some("editor"));

    let completed = store
        .complete(pending.id, completion("raw revised", "final"))
        .unwrap();
    assert_eq!(completed.status, HistoryStatus::Completed);
    assert_eq!(completed.final_text.as_deref(), Some("final"));
    assert!(completed.completed_at_ms.is_some());
    assert!(completed.failure.is_none());

    let failed = store
        .create_pending(new_entry("other", "model-b"), None)
        .unwrap();
    let failed = store.fail(failed.id, "decoder failed").unwrap();
    assert_eq!(failed.status, HistoryStatus::Failed);
    assert_eq!(failed.failure.as_deref(), Some("decoder failed"));
}

#[test]
fn lifecycle_rejects_invalid_transitions_and_invalid_metrics() {
    let root = TestRoot::new("transitions");
    let store = HistoryStore::open(&root, policy(100)).unwrap();
    let completed = completed(&store, "done");
    assert!(matches!(
        store.fail(completed.id, "late failure"),
        Err(HistoryError::InvalidTransition(_))
    ));
    assert!(matches!(
        store.complete(completed.id, completion("again", "again")),
        Err(HistoryError::InvalidTransition(_))
    ));

    let mut invalid = new_entry("raw", "model");
    invalid.metrics.realtime_factor = Some(f64::NAN);
    assert!(matches!(
        store.create_pending(invalid, None),
        Err(HistoryError::InvalidInput(_))
    ));
}

#[test]
fn completion_preserves_each_metric_not_supplied_by_the_runtime() {
    let root = TestRoot::new("metric-preservation");
    let store = HistoryStore::open(&root, policy(100)).unwrap();
    let pending = store
        .create_pending(new_entry("raw", "model"), None)
        .unwrap();
    let completed = store
        .complete(
            pending.id,
            CompletedHistoryEntry {
                raw_text: "raw".into(),
                final_text: "final".into(),
                metrics: HistoryMetrics {
                    audio_duration_ms: None,
                    processing_duration_ms: Some(55),
                    realtime_factor: None,
                },
            },
        )
        .unwrap();

    assert_eq!(completed.metrics.audio_duration_ms, Some(100));
    assert_eq!(completed.metrics.processing_duration_ms, Some(55));
    assert_eq!(completed.metrics.realtime_factor, Some(0.4));
}

#[test]
fn output_outcome_is_bounded_and_only_records_terminal_caller_results() {
    let root = TestRoot::new("output-outcome");
    let store = HistoryStore::open(&root, policy(100)).unwrap();
    let pending = store
        .create_pending(new_entry("raw", "model"), None)
        .unwrap();
    assert!(matches!(
        store.record_output_outcome(pending.id, "clipboard copied"),
        Err(HistoryError::InvalidTransition(_))
    ));

    let completed = store
        .complete(pending.id, completion("raw", "final"))
        .unwrap();
    let recorded = store
        .record_output_outcome(completed.id, "  clipboard\ncopied\u{7}  ")
        .unwrap();
    assert_eq!(recorded.output_outcome.as_deref(), Some("clipboard copied"));
    assert!(matches!(
        store.record_output_outcome(completed.id, "x".repeat(1_025)),
        Err(HistoryError::InvalidInput(_))
    ));
}

#[test]
fn search_is_parameterized_filtered_and_treats_wildcards_literally() {
    let root = TestRoot::new("search");
    let store = HistoryStore::open(&root, policy(100)).unwrap();
    completed(&store, "literal 100%_match");
    completed(&store, "ordinary text");
    let failed = store
        .create_pending(new_entry("failure needle", "model-z"), None)
        .unwrap();
    store.fail(failed.id, "failure").unwrap();

    let page = store
        .search(HistoryQuery {
            text: Some("100%_".into()),
            ..HistoryQuery::default()
        })
        .unwrap();
    assert_eq!(page.records.len(), 1);
    assert_eq!(page.records[0].raw_text, "literal 100%_match");

    let failed_page = store
        .search(HistoryQuery {
            text: Some("needle".into()),
            status: Some(HistoryStatus::Failed),
            ..HistoryQuery::default()
        })
        .unwrap();
    assert_eq!(failed_page.records.len(), 1);
    assert_eq!(failed_page.records[0].status, HistoryStatus::Failed);
}

#[test]
fn stable_keyset_pagination_has_no_duplicates_or_gaps() {
    let root = TestRoot::new("pagination");
    let store = HistoryStore::open(&root, policy(100)).unwrap();
    for index in 0..7 {
        completed(&store, &format!("record {index}"));
    }
    let mut before = None;
    let mut ids = Vec::new();
    loop {
        let page = store
            .search(HistoryQuery {
                before,
                limit: 3,
                ..HistoryQuery::default()
            })
            .unwrap();
        ids.extend(page.records.iter().map(|record| record.id));
        before = page.next;
        if before.is_none() {
            break;
        }
    }
    assert_eq!(ids.len(), 7);
    let unique = ids
        .iter()
        .copied()
        .collect::<std::collections::HashSet<_>>();
    assert_eq!(unique.len(), 7);
    assert!(ids.windows(2).all(|pair| pair[0] > pair[1]));
}

#[test]
fn retention_removes_only_old_unpinned_terminal_records() {
    let root = TestRoot::new("retention");
    let store = HistoryStore::open(&root, policy(2)).unwrap();
    let pinned = completed(&store, "pinned");
    store.set_pinned(pinned.id, true).unwrap();
    let oldest_unpinned = completed(&store, "oldest unpinned");
    let kept_a = completed(&store, "kept a");
    let kept_b = completed(&store, "kept b");

    assert!(matches!(
        store.set_pinned(oldest_unpinned.id, true),
        Err(HistoryError::NotFound(_))
    ));
    let page = store.search(HistoryQuery::default()).unwrap();
    assert_eq!(page.records.len(), 3);
    assert!(page.records.iter().any(|record| record.id == pinned.id));
    assert!(page.records.iter().any(|record| record.id == kept_a.id));
    assert!(page.records.iter().any(|record| record.id == kept_b.id));
}

#[test]
fn age_retention_expires_audio_then_transcript_and_never_touches_pins() {
    let root = TestRoot::new("age-retention");
    let (unpinned_id, pinned_id) = {
        let store = HistoryStore::open(&root, policy(100)).unwrap();
        let unpinned = store
            .create_pending(new_entry("unpinned", "model"), Some(&prepared_audio()))
            .unwrap();
        store.fail(unpinned.id, "failed").unwrap();
        let pinned = store
            .create_pending(new_entry("pinned", "model"), Some(&prepared_audio()))
            .unwrap();
        store.fail(pinned.id, "failed").unwrap();
        store.set_pinned(pinned.id, true).unwrap();
        (unpinned.id, pinned.id)
    };
    let connection = Connection::open(root.as_ref().join("history.sqlite3")).unwrap();
    connection
        .execute("UPDATE history SET created_at_ms = 0", [])
        .unwrap();
    drop(connection);

    let audio_policy = HistoryRetentionPolicy {
        max_unpinned_entries: 100,
        transcript_retention_days: None,
        audio_retention_days: Some(1),
    };
    let store = HistoryStore::open(&root, audio_policy).unwrap();
    let records = store.search(HistoryQuery::default()).unwrap().records;
    let unpinned = records
        .iter()
        .find(|record| record.id == unpinned_id)
        .unwrap();
    let pinned = records
        .iter()
        .find(|record| record.id == pinned_id)
        .unwrap();
    assert!(unpinned.audio_path.is_none());
    assert!(pinned.audio_path.is_some());

    store
        .set_retention_policy(HistoryRetentionPolicy {
            transcript_retention_days: Some(1),
            ..audio_policy
        })
        .unwrap();
    let records = store.search(HistoryQuery::default()).unwrap().records;
    assert!(!records.iter().any(|record| record.id == unpinned_id));
    assert!(records.iter().any(|record| record.id == pinned_id));
    assert!(store.validated_audio_path(pinned_id).unwrap().is_some());
}

#[test]
fn staged_audio_round_trips_and_retry_reuses_the_same_row() {
    let root = TestRoot::new("audio-retry");
    let store = HistoryStore::open(&root, policy(100)).unwrap();
    let audio = prepared_audio();
    let pending = store
        .create_pending(new_entry("raw", "model"), Some(&audio))
        .unwrap();
    let path = store.validated_audio_path(pending.id).unwrap().unwrap();
    assert!(path.is_file());
    assert!(!pending.audio_path.as_ref().unwrap().is_absolute());
    store.fail(pending.id, "transient").unwrap();

    let retry = store.retry(pending.id).unwrap();
    assert_eq!(retry.record.id, pending.id);
    assert_eq!(retry.record.status, HistoryStatus::Failed);
    assert_eq!(retry.record.retry_count, 0);
    assert_eq!(retry.audio.sample_rate, PREPARED_SAMPLE_RATE);
    assert_eq!(retry.audio.samples.len(), audio.samples.len());
    for (actual, expected) in retry.audio.samples.iter().zip(audio.samples) {
        assert!((actual - expected).abs() <= 1.0 / i16::MAX as f32);
    }
    let completed = store
        .complete_retry(
            pending.id,
            CompletedHistoryEntry {
                raw_text: "raw retry".into(),
                final_text: "final retry".into(),
                metrics: HistoryMetrics::default(),
            },
        )
        .unwrap();
    assert_eq!(completed.status, HistoryStatus::Completed);
    assert_eq!(completed.retry_count, 1);
}

#[test]
fn failed_retry_stays_failed_and_records_one_terminal_attempt() {
    let root = TestRoot::new("retry-failure-terminal");
    let store = HistoryStore::open(&root, policy(100)).unwrap();
    let record = store
        .create_pending(new_entry("raw", "model"), Some(&prepared_audio()))
        .unwrap();
    store.fail(record.id, "initial failure").unwrap();

    let loaded = store.retry(record.id).unwrap();
    assert_eq!(loaded.record.status, HistoryStatus::Failed);
    assert_eq!(loaded.record.retry_count, 0);
    let failed = store
        .fail_retry(record.id, "retry transcription failed")
        .unwrap();

    assert_eq!(failed.status, HistoryStatus::Failed);
    assert_eq!(failed.retry_count, 1);
    assert_eq!(
        failed.failure.as_deref(),
        Some("retry transcription failed")
    );
}

#[test]
fn active_retry_lease_survives_retention_triggered_by_another_row() {
    let root = TestRoot::new("retry-retention-lease");
    let store = HistoryStore::open(&root, policy(1)).unwrap();
    let leased = store
        .create_pending(new_entry("leased", "model"), Some(&prepared_audio()))
        .unwrap();
    store.fail(leased.id, "initial failure").unwrap();
    let retry = store.retry(leased.id).unwrap();
    assert_eq!(retry.record.status, HistoryStatus::Failed);

    let newer = store
        .create_pending(new_entry("newer", "model"), None)
        .unwrap();
    store.fail(newer.id, "newer failure").unwrap();
    let during_retry = store.search(HistoryQuery::default()).unwrap();
    assert!(
        during_retry
            .records
            .iter()
            .any(|record| record.id == leased.id)
    );
    assert!(store.validated_audio_path(leased.id).unwrap().is_some());
    assert!(matches!(
        store.delete(leased.id),
        Err(HistoryError::InvalidTransition(_))
    ));

    store
        .fail_retry(leased.id, "retry terminal failure")
        .unwrap();
    let after_terminal = store.search(HistoryQuery::default()).unwrap();
    assert_eq!(after_terminal.records.len(), 1);
    assert_eq!(after_terminal.records[0].id, newer.id);
}

#[test]
fn failed_terminal_retry_validation_releases_lease_and_restores_retention() {
    let root = TestRoot::new("retry-terminal-validation-release");
    let store = HistoryStore::open(&root, policy(1)).unwrap();
    let leased = store
        .create_pending(new_entry("leased", "model"), Some(&prepared_audio()))
        .unwrap();
    store.fail(leased.id, "initial failure").unwrap();
    store.retry(leased.id).unwrap();

    let invalid = store.complete_retry(
        leased.id,
        CompletedHistoryEntry {
            raw_text: "retry".into(),
            final_text: "retry".into(),
            metrics: HistoryMetrics {
                realtime_factor: Some(f64::NAN),
                ..HistoryMetrics::default()
            },
        },
    );
    assert!(matches!(invalid, Err(HistoryError::InvalidInput(_))));

    // The failed terminal attempt consumed its lease, so another retry is
    // admitted instead of leaving the row permanently stuck.
    store.retry(leased.id).unwrap();
    store.release_retry(leased.id).unwrap();
    store.release_retry(leased.id).unwrap();

    let newer = store
        .create_pending(new_entry("newer", "model"), None)
        .unwrap();
    store.fail(newer.id, "newer failure").unwrap();
    let records = store.search(HistoryQuery::default()).unwrap().records;
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].id, newer.id);
}

#[test]
fn release_acknowledgement_is_independent_of_retention_failure() {
    let root = TestRoot::new("retry-release-retention-error");
    let store = HistoryStore::open(&root, policy(1)).unwrap();
    let leased = store
        .create_pending(new_entry("leased", "model"), Some(&prepared_audio()))
        .unwrap();
    store.fail(leased.id, "initial failure").unwrap();
    let retained_path = store.validated_audio_path(leased.id).unwrap().unwrap();
    store.retry(leased.id).unwrap();

    let newer = store
        .create_pending(new_entry("newer", "model"), None)
        .unwrap();
    store.fail(newer.id, "newer failure").unwrap();
    fs::remove_file(&retained_path).unwrap();
    fs::create_dir(&retained_path).unwrap();

    let acknowledgement = store.release_retry(leased.id).unwrap();
    assert!(acknowledgement.retention_error.is_some());
    // Pinning is rejected for leased rows. Success proves the lease was
    // removed even though the post-release retention pass failed.
    assert!(store.set_pinned(leased.id, true).unwrap().pinned);
}

#[test]
fn retry_requires_failed_state_and_retained_valid_audio() {
    let root = TestRoot::new("retry-gating");
    let store = HistoryStore::open(&root, policy(100)).unwrap();
    let pending = store
        .create_pending(new_entry("raw", "model"), Some(&prepared_audio()))
        .unwrap();
    assert!(matches!(
        store.retry(pending.id),
        Err(HistoryError::InvalidTransition(_))
    ));
    store.fail(pending.id, "failed").unwrap();
    store.delete_audio(pending.id).unwrap();
    assert!(matches!(
        store.retry(pending.id),
        Err(HistoryError::InvalidTransition(_))
    ));
}

#[test]
fn retry_rejects_replaced_noncanonical_wav_without_changing_the_row() {
    let root = TestRoot::new("retry-corrupt-audio");
    let store = HistoryStore::open(&root, policy(100)).unwrap();
    let record = store
        .create_pending(new_entry("raw", "model"), Some(&prepared_audio()))
        .unwrap();
    store.fail(record.id, "failed").unwrap();
    let path = store.validated_audio_path(record.id).unwrap().unwrap();
    fs::write(path, b"not a wav").unwrap();

    assert!(matches!(
        store.retry(record.id),
        Err(HistoryError::Audio(_))
    ));
    let unchanged = store
        .search(HistoryQuery {
            status: Some(HistoryStatus::Failed),
            ..HistoryQuery::default()
        })
        .unwrap()
        .records
        .into_iter()
        .find(|candidate| candidate.id == record.id)
        .unwrap();
    assert_eq!(unchanged.retry_count, 0);
}

#[test]
fn audio_only_and_full_delete_have_distinct_results() {
    let root = TestRoot::new("delete-modes");
    let store = HistoryStore::open(&root, policy(100)).unwrap();
    let first = store
        .create_pending(new_entry("first", "model"), Some(&prepared_audio()))
        .unwrap();
    store.fail(first.id, "done").unwrap();
    let without_audio = store.delete_audio(first.id).unwrap();
    assert!(without_audio.audio_path.is_none());

    let second = completed(&store, "second");
    store.delete(second.id).unwrap();
    assert!(matches!(
        store.set_pinned(second.id, true),
        Err(HistoryError::NotFound(_))
    ));
}

#[test]
fn startup_reconciles_interrupted_pending_and_deletion_journal() {
    let root = TestRoot::new("startup-journal");
    let (pending_id, deleting_id, deleting_path) = {
        let store = HistoryStore::open(&root, policy(100)).unwrap();
        let pending = store
            .create_pending(new_entry("interrupted", "model"), None)
            .unwrap();
        let deleting = store
            .create_pending(new_entry("delete me", "model"), Some(&prepared_audio()))
            .unwrap();
        store.fail(deleting.id, "failed").unwrap();
        (
            pending.id,
            deleting.id,
            deleting.audio_path.unwrap().to_string_lossy().into_owned(),
        )
    };
    let connection = Connection::open(root.as_ref().join("history.sqlite3")).unwrap();
    connection
        .execute(
            "INSERT INTO deletion_journal
             (history_id, audio_path, delete_record, created_at_ms) VALUES (?1, ?2, 1, 0)",
            params![deleting_id, deleting_path],
        )
        .unwrap();
    drop(connection);

    let restarted = HistoryStore::open(&root, policy(100)).unwrap();
    let report = restarted.startup_reconciliation();
    assert_eq!(report.interrupted_pending_failed, 1);
    assert_eq!(report.deletions_completed, 1);
    let pending = restarted
        .search(HistoryQuery {
            status: Some(HistoryStatus::Failed),
            ..HistoryQuery::default()
        })
        .unwrap()
        .records
        .into_iter()
        .find(|record| record.id == pending_id)
        .unwrap();
    assert!(pending.failure.unwrap().contains("exited"));
    assert!(matches!(
        restarted.set_pinned(deleting_id, true),
        Err(HistoryError::NotFound(_))
    ));
}

#[test]
fn startup_removes_orphans_and_temps_and_clears_missing_audio() {
    let root = TestRoot::new("startup-files");
    let missing_id = {
        let store = HistoryStore::open(&root, policy(100)).unwrap();
        let record = store
            .create_pending(new_entry("missing", "model"), Some(&prepared_audio()))
            .unwrap();
        store.fail(record.id, "failed").unwrap();
        let path = store.validated_audio_path(record.id).unwrap().unwrap();
        fs::remove_file(path).unwrap();
        record.id
    };
    let audio_dir = root.as_ref().join("audio");
    fs::write(audio_dir.join("999-orphan.wav"), b"orphan").unwrap();
    fs::write(audio_dir.join(".stage-interrupted.tmp"), b"temp").unwrap();

    let restarted = HistoryStore::open(&root, policy(100)).unwrap();
    let report = restarted.startup_reconciliation();
    assert_eq!(report.missing_audio_cleared, 1);
    assert_eq!(report.orphan_audio_removed, 1);
    assert_eq!(report.temporary_audio_removed, 1);
    let missing = restarted
        .search(HistoryQuery::default())
        .unwrap()
        .records
        .into_iter()
        .find(|record| record.id == missing_id)
        .unwrap();
    assert!(missing.audio_path.is_none());
}

#[test]
fn traversal_in_database_is_rejected_without_touching_outside_file() {
    let root = TestRoot::new("traversal");
    let outside = root.as_ref().parent().unwrap().join(format!(
        "scribe-history-outside-{}-{}.wav",
        std::process::id(),
        TEST_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    fs::write(&outside, b"outside").unwrap();
    let id = {
        let store = HistoryStore::open(&root, policy(100)).unwrap();
        completed(&store, "record").id
    };
    let connection = Connection::open(root.as_ref().join("history.sqlite3")).unwrap();
    connection
        .execute(
            "UPDATE history SET audio_path = ?1 WHERE id = ?2",
            params![
                format!("../{}", outside.file_name().unwrap().to_string_lossy()),
                id
            ],
        )
        .unwrap();
    drop(connection);

    assert!(matches!(
        HistoryStore::open(&root, policy(100)),
        Err(HistoryError::UnsafePath(_))
    ));
    assert_eq!(fs::read(&outside).unwrap(), b"outside");
    fs::remove_file(outside).unwrap();
}

#[test]
fn newer_schema_is_refused_instead_of_modified() {
    let root = TestRoot::new("future-schema");
    {
        let store = HistoryStore::open(&root, policy(100)).unwrap();
        drop(store);
    }
    let connection = Connection::open(root.as_ref().join("history.sqlite3")).unwrap();
    connection.pragma_update(None, "user_version", 99).unwrap();
    drop(connection);

    assert!(matches!(
        HistoryStore::open(&root, policy(100)),
        Err(HistoryError::Corrupt(_))
    ));
}

#[cfg(unix)]
#[test]
fn symlinked_audio_directory_is_rejected() {
    use std::os::unix::fs::symlink;

    let root = TestRoot::new("symlink");
    let outside = TestRoot::new("symlink-outside");
    fs::remove_dir(root.as_ref().join("audio")).ok();
    symlink(outside.as_ref(), root.as_ref().join("audio")).unwrap();

    assert!(matches!(
        HistoryStore::open(&root, policy(100)),
        Err(HistoryError::UnsafePath(_))
    ));
}

#[cfg(windows)]
#[test]
fn reparse_audio_directory_is_rejected_when_symlink_creation_is_available() {
    use std::os::windows::fs::symlink_dir;

    let root = TestRoot::new("reparse");
    let outside = TestRoot::new("reparse-outside");
    let audio_dir = root.as_ref().join("audio");
    fs::create_dir(&audio_dir).unwrap();
    fs::remove_dir(&audio_dir).unwrap();
    if symlink_dir(outside.as_ref(), &audio_dir).is_err() {
        return;
    }
    assert!(matches!(
        HistoryStore::open(&root, policy(100)),
        Err(HistoryError::UnsafePath(_))
    ));
}

#[cfg(unix)]
#[test]
fn dangling_database_symlink_is_rejected_before_sqlite_open() {
    use std::os::unix::fs::symlink;

    let root = TestRoot::new("dangling-database-link");
    let outside = root.as_ref().join("outside-missing.sqlite3");
    symlink(&outside, root.as_ref().join("history.sqlite3")).unwrap();
    assert!(matches!(
        HistoryStore::open(&root, policy(100)),
        Err(HistoryError::UnsafePath(_))
    ));
    assert!(!outside.exists());
}

#[cfg(windows)]
#[test]
fn dangling_database_reparse_is_rejected_when_symlink_creation_is_available() {
    use std::os::windows::fs::symlink_file;

    let root = TestRoot::new("dangling-database-reparse");
    let outside = root.as_ref().join("outside-missing.sqlite3");
    if symlink_file(&outside, root.as_ref().join("history.sqlite3")).is_err() {
        return;
    }
    assert!(matches!(
        HistoryStore::open(&root, policy(100)),
        Err(HistoryError::UnsafePath(_))
    ));
    assert!(!outside.exists());
}

#[cfg(windows)]
fn set_restrictive_sidecar_dacl(path: &Path, user_sid: &str) {
    use std::os::windows::ffi::OsStrExt;
    use std::ptr;
    use windows_sys::Win32::Foundation::{GetLastError, LocalFree};
    use windows_sys::Win32::Security::Authorization::{
        ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
    };
    use windows_sys::Win32::Security::{
        DACL_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR,
        SetFileSecurityW,
    };

    let path_wide = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    // Keep WRITE_DAC for this process user so the production repair can
    // replace the DACL while normal read/write opens remain denied.
    let sddl = format!("D:P(A;OICI;WD;;;{user_sid})(A;OICI;FA;;;SY)")
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let mut descriptor: PSECURITY_DESCRIPTOR = ptr::null_mut();
    let converted = unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1,
            &mut descriptor,
            ptr::null_mut(),
        )
    };
    assert_ne!(converted, 0, "failed to build restrictive sidecar DACL");
    let applied = unsafe {
        SetFileSecurityW(
            path_wide.as_ptr(),
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            descriptor,
        )
    };
    let error = (applied == 0).then(|| unsafe { GetLastError() });
    unsafe {
        LocalFree(descriptor);
    }
    assert!(
        error.is_none(),
        "failed to apply restrictive sidecar DACL: {}",
        std::io::Error::from_raw_os_error(error.unwrap_or_default() as i32)
    );
}

#[cfg(windows)]
fn sidecar_security_descriptor_sddl(path: &Path) -> String {
    use std::os::windows::ffi::OsStrExt;
    use std::ptr;
    use windows_sys::Win32::Foundation::{GetLastError, LocalFree};
    use windows_sys::Win32::Security::Authorization::{
        ConvertSecurityDescriptorToStringSecurityDescriptorW, GetNamedSecurityInfoW,
        SDDL_REVISION_1, SE_FILE_OBJECT,
    };
    use windows_sys::Win32::Security::{DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID};

    let path_wide = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let mut descriptor: PSECURITY_DESCRIPTOR = ptr::null_mut();
    let mut dacl = ptr::null_mut();
    let status = unsafe {
        GetNamedSecurityInfoW(
            path_wide.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            ptr::null_mut::<PSID>(),
            ptr::null_mut::<PSID>(),
            &mut dacl,
            ptr::null_mut(),
            &mut descriptor,
        )
    };
    assert_eq!(status, 0, "failed to read sidecar DACL: {status}");

    let mut descriptor_sddl = ptr::null_mut();
    let mut descriptor_sddl_length = 0;
    let converted = unsafe {
        ConvertSecurityDescriptorToStringSecurityDescriptorW(
            descriptor,
            SDDL_REVISION_1,
            DACL_SECURITY_INFORMATION,
            &mut descriptor_sddl,
            &mut descriptor_sddl_length,
        )
    };
    if converted == 0 {
        let error = unsafe { GetLastError() };
        unsafe {
            LocalFree(descriptor);
        }
        panic!(
            "failed to stringify sidecar DACL: {}",
            std::io::Error::from_raw_os_error(error as i32)
        );
    }
    let length = unsafe { (0..).find(|&index| *descriptor_sddl.add(index) == 0) };
    let length = length.unwrap_or(descriptor_sddl_length as usize);
    let sddl =
        String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(descriptor_sddl, length) });
    unsafe {
        LocalFree(descriptor_sddl.cast());
        LocalFree(descriptor);
    }
    sddl
}

#[cfg(windows)]
fn assert_sidecar_open_is_denied(path: &Path) {
    let error = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .expect_err("restrictive sidecar DACL unexpectedly allowed read/write open");
    assert_eq!(
        error.raw_os_error(),
        Some(5),
        "expected ERROR_ACCESS_DENIED from restrictive sidecar DACL, got {error:?}"
    );
}

#[cfg(windows)]
#[test]
fn stale_wal_and_shm_acl_is_repaired_before_history_reopen() {
    use rusqlite::Connection;

    let root = TestRoot::new("sidecar-acl-recovery");
    let record_id = {
        let store = HistoryStore::open(&root, policy(100)).unwrap();
        completed(&store, "acl recovery record").id
    };
    let database_path = root.as_ref().join("history.sqlite3");
    let wal_path = root.as_ref().join("history.sqlite3-wal");
    let shm_path = root.as_ref().join("history.sqlite3-shm");

    // Keep a second SQLite connection open so SQLite leaves both sidecars in
    // place while the ACLs are denied and the history store is reopened.
    let writer = Connection::open(&database_path).unwrap();
    writer.pragma_update(None, "journal_mode", "WAL").unwrap();
    writer
        .pragma_update(None, "wal_autocheckpoint", 0i64)
        .unwrap();
    writer
        .execute_batch("BEGIN; UPDATE history SET updated_at_ms = updated_at_ms + 1; COMMIT;")
        .unwrap();
    let keeper = Connection::open(&database_path).unwrap();
    assert!(wal_path.is_file(), "SQLite did not create the WAL sidecar");
    assert!(shm_path.is_file(), "SQLite did not create the SHM sidecar");
    drop(writer);

    let user_sid = super::audio::current_process_user_sid().unwrap();
    set_restrictive_sidecar_dacl(&wal_path, &user_sid);
    set_restrictive_sidecar_dacl(&shm_path, &user_sid);
    assert_sidecar_open_is_denied(&wal_path);
    assert_sidecar_open_is_denied(&shm_path);

    let reopened = HistoryStore::open(&root, policy(100)).unwrap();
    let page = reopened.search(HistoryQuery::default()).unwrap();
    assert!(page.records.iter().any(|record| record.id == record_id));

    for sidecar in [&wal_path, &shm_path] {
        let sddl = sidecar_security_descriptor_sddl(sidecar);
        assert!(
            sddl.starts_with("D:P"),
            "sidecar DACL is not protected: {sddl}"
        );
        assert!(
            sddl.contains(&format!(";;;{user_sid}")),
            "repaired sidecar DACL omitted current user SID {user_sid}: {sddl}"
        );
        assert!(
            sddl.contains(";;;SY"),
            "repaired sidecar DACL omitted SY: {sddl}"
        );
        for principal in [";;;OW", ";;;AU", ";;;BU", ";;;WD"] {
            assert!(
                !sddl.contains(principal),
                "repaired sidecar DACL unexpectedly contains {principal}: {sddl}"
            );
        }
    }

    drop(reopened);
    drop(keeper);
}

#[test]
fn cloned_handles_share_one_worker_and_only_last_drop_stops_it() {
    let root = TestRoot::new("clone");
    let store = HistoryStore::open(&root, policy(100)).unwrap();
    let helper = store.clone();
    drop(store);
    let record = std::thread::spawn(move || completed(&helper, "from helper"))
        .join()
        .unwrap();
    assert_eq!(record.status, HistoryStatus::Completed);
}

#[test]
fn a_second_store_cannot_reconcile_the_same_root_concurrently() {
    let root = TestRoot::new("single-owner");
    let store = HistoryStore::open(&root, policy(100)).unwrap();
    assert!(matches!(
        HistoryStore::open(&root, policy(100)),
        Err(HistoryError::AlreadyOpen)
    ));
    drop(store);
    let reopened = HistoryStore::open(&root, policy(100)).unwrap();
    assert_eq!(reopened.startup_reconciliation(), Default::default());
}

#[test]
fn queued_create_is_ordered_before_immediate_completion() {
    let root = TestRoot::new("queued-create-order");
    let store = HistoryStore::open(&root, policy(100)).unwrap();
    let id = HistoryStore::reserve_id();
    let created = store
        .enqueue_pending(id, new_entry("", "model"), None)
        .unwrap();
    let completed = store
        .complete(
            id,
            CompletedHistoryEntry {
                raw_text: "raw".into(),
                final_text: "final".into(),
                metrics: HistoryMetrics::default(),
            },
        )
        .unwrap();
    assert_eq!(created.recv().unwrap().unwrap().id, id);
    assert_eq!(completed.id, id);
    assert_eq!(completed.status, HistoryStatus::Completed);
}
