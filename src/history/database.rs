use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crossbeam_channel::{Receiver, Sender};
use rusqlite::types::{Type, Value};
use rusqlite::{Connection, OptionalExtension, Row, Transaction, params, params_from_iter};

use super::audio;
use super::{
    Command, CompletedHistoryEntry, HistoryCursor, HistoryError, HistoryMetrics, HistoryPage,
    HistoryQuery, HistoryRecord, HistoryResult, HistoryRetentionPolicy, HistoryStatus,
    NewHistoryEntry, ReconciliationReport, RetryHistoryEntry, RetryReleaseAcknowledgement,
    recv_command,
};

const SCHEMA_VERSION: i64 = 1;
const MAX_TEXT_BYTES: usize = 1_048_576;
const MAX_IDENTIFIER_BYTES: usize = 512;
const MAX_FAILURE_BYTES: usize = 16_384;
const MAX_OUTPUT_OUTCOME_BYTES: usize = 1_024;
const INTERRUPTED_FAILURE: &str = "Scribe exited before this transcription finished";

pub(super) fn run_worker(
    root: PathBuf,
    retention_policy: HistoryRetentionPolicy,
    receiver: Receiver<Command>,
    ready: Sender<HistoryResult<ReconciliationReport>>,
) {
    let initialized = Worker::open(root, retention_policy).and_then(|mut worker| {
        let report = worker.reconcile()?;
        Ok((worker, report))
    });
    let (mut worker, report) = match initialized {
        Ok(value) => value,
        Err(error) => {
            let _ = ready.send(Err(error));
            return;
        }
    };
    if ready.send(Ok(report)).is_err() {
        return;
    }

    while let Some(command) = recv_command(&receiver) {
        match command {
            Command::Create {
                id,
                entry,
                audio,
                reply,
            } => {
                let _ = reply.send(worker.create(id, entry, audio.as_deref()));
            }
            Command::Complete { id, entry, reply } => {
                let _ = reply.send(worker.complete(id, entry));
            }
            Command::CompleteRetry { id, entry, reply } => {
                let _ = reply.send(worker.complete_retry(id, entry));
            }
            Command::Fail { id, failure, reply } => {
                let _ = reply.send(worker.fail(id, failure));
            }
            Command::FailRetry { id, failure, reply } => {
                let _ = reply.send(worker.fail_retry(id, failure));
            }
            Command::ReleaseRetry { id, reply } => {
                let _ = reply.send(worker.release_retry(id));
            }
            Command::Retry { id, reply } => {
                let _ = reply.send(worker.retry(id));
            }
            Command::Search { query, reply } => {
                let _ = reply.send(worker.search(query));
            }
            Command::Pin { id, pinned, reply } => {
                let _ = reply.send(worker.set_pinned(id, pinned));
            }
            Command::DeleteAudio { id, reply } => {
                let _ = reply.send(worker.delete_audio(id));
            }
            Command::Delete { id, reply } => {
                let _ = reply.send(worker.delete(id));
            }
            Command::AudioPath { id, reply } => {
                let _ = reply.send(worker.validated_audio_path(id));
            }
            Command::SetRetentionPolicy { policy, reply } => {
                let _ = reply.send(worker.set_retention_policy(policy));
            }
            Command::RecordOutputOutcome { id, outcome, reply } => {
                let _ = reply.send(worker.record_output_outcome(id, outcome));
            }
            Command::Shutdown => break,
        }
    }
}

struct Worker {
    root: PathBuf,
    connection: Connection,
    retention_policy: HistoryRetentionPolicy,
    leased_retry_ids: HashSet<i64>,
}

impl Worker {
    fn open(root: PathBuf, retention_policy: HistoryRetentionPolicy) -> HistoryResult<Self> {
        validate_retention_policy(retention_policy)?;
        audio::initialize_root(&root)?;
        let database_path = root.join("history.sqlite3");
        audio::validate_database_files_before_open(&database_path)?;
        let connection = Connection::open(&database_path)?;
        connection.busy_timeout(std::time::Duration::from_millis(750))?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.pragma_update(None, "synchronous", "FULL")?;
        audio::secure_database_files(&database_path)?;
        migrate(&connection)?;
        Ok(Self {
            root,
            connection,
            retention_policy,
            leased_retry_ids: HashSet::new(),
        })
    }

    fn create(
        &mut self,
        id: i64,
        entry: NewHistoryEntry,
        prepared_audio: Option<&crate::prepared_audio::PreparedAudio>,
    ) -> HistoryResult<HistoryRecord> {
        validate_new_entry(&entry)?;
        let now = now_ms();
        self.connection.execute(
            "INSERT INTO history (
                id, created_at_ms, updated_at_ms, status, raw_text, model_id,
                audio_duration_ms, processing_duration_ms, realtime_factor, source_app
             ) VALUES (?1, ?2, ?2, 'pending', ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                id,
                now,
                entry.raw_text,
                entry.model_id,
                optional_u64_to_i64(entry.metrics.audio_duration_ms)?,
                optional_u64_to_i64(entry.metrics.processing_duration_ms)?,
                entry.metrics.realtime_factor,
                entry.source_app,
            ],
        )?;
        if let Some(prepared_audio) = prepared_audio {
            let relative = match audio::stage_audio(&self.root, id, prepared_audio) {
                Ok(relative) => relative,
                Err(error) => {
                    let _ = self
                        .connection
                        .execute("DELETE FROM history WHERE id = ?1", [id]);
                    return Err(error);
                }
            };
            if let Err(error) = self.connection.execute(
                "UPDATE history SET audio_path = ?1 WHERE id = ?2",
                params![path_to_database(&relative)?, id],
            ) {
                let _ = audio::remove_audio(&self.root, &relative);
                let _ = self
                    .connection
                    .execute("DELETE FROM history WHERE id = ?1", [id]);
                return Err(error.into());
            }
        }
        self.record(id)
    }

    fn complete(&mut self, id: i64, entry: CompletedHistoryEntry) -> HistoryResult<HistoryRecord> {
        validate_text("raw text", &entry.raw_text, MAX_TEXT_BYTES, true)?;
        validate_text("final text", &entry.final_text, MAX_TEXT_BYTES, true)?;
        validate_metrics(&entry.metrics)?;
        self.require_status(id, HistoryStatus::Pending)?;
        let now = now_ms();
        self.connection.execute(
            "UPDATE history SET status = 'completed', updated_at_ms = ?1,
                completed_at_ms = ?1, raw_text = ?2, final_text = ?3,
                audio_duration_ms = COALESCE(?4, audio_duration_ms),
                processing_duration_ms = COALESCE(?5, processing_duration_ms),
                realtime_factor = COALESCE(?6, realtime_factor), failure = NULL
             WHERE id = ?7 AND status = 'pending'",
            params![
                now,
                entry.raw_text,
                entry.final_text,
                optional_u64_to_i64(entry.metrics.audio_duration_ms)?,
                optional_u64_to_i64(entry.metrics.processing_duration_ms)?,
                entry.metrics.realtime_factor,
                id,
            ],
        )?;
        self.enforce_retention()?;
        self.record(id)
    }

    fn fail(&mut self, id: i64, failure: String) -> HistoryResult<HistoryRecord> {
        validate_text("failure", &failure, MAX_FAILURE_BYTES, false)?;
        self.require_status(id, HistoryStatus::Pending)?;
        let now = now_ms();
        self.connection.execute(
            "UPDATE history SET status = 'failed', updated_at_ms = ?1,
                completed_at_ms = ?1, failure = ?2
             WHERE id = ?3 AND status = 'pending'",
            params![now, failure, id],
        )?;
        self.enforce_retention()?;
        self.record(id)
    }

    fn complete_retry(
        &mut self,
        id: i64,
        entry: CompletedHistoryEntry,
    ) -> HistoryResult<HistoryRecord> {
        if !self.leased_retry_ids.contains(&id) {
            return Err(HistoryError::InvalidTransition(format!(
                "record {id} has no active retry lease"
            )));
        }
        let result = (|| {
            validate_text("raw text", &entry.raw_text, MAX_TEXT_BYTES, true)?;
            validate_text("final text", &entry.final_text, MAX_TEXT_BYTES, true)?;
            validate_metrics(&entry.metrics)?;
            self.require_status(id, HistoryStatus::Failed)?;
            let now = now_ms();
            let changed = self.connection.execute(
                "UPDATE history SET status = 'completed', updated_at_ms = ?1,
                    completed_at_ms = ?1, raw_text = ?2, final_text = ?3,
                    audio_duration_ms = COALESCE(?4, audio_duration_ms),
                    processing_duration_ms = COALESCE(?5, processing_duration_ms),
                    realtime_factor = COALESCE(?6, realtime_factor), failure = NULL,
                    output_outcome = NULL, retry_count = retry_count + 1
                 WHERE id = ?7 AND status = 'failed' AND retry_count < 4294967295",
                params![
                    now,
                    entry.raw_text,
                    entry.final_text,
                    optional_u64_to_i64(entry.metrics.audio_duration_ms)?,
                    optional_u64_to_i64(entry.metrics.processing_duration_ms)?,
                    entry.metrics.realtime_factor,
                    id,
                ],
            )?;
            if changed != 1 {
                return Err(HistoryError::InvalidTransition(format!(
                    "record {id} retry count overflowed"
                )));
            }
            self.record(id)
        })();
        // A terminal attempt consumes the lease even when validation or the
        // database mutation fails. Otherwise one bad write makes the row
        // permanently unretryable and immune to retention until restart.
        self.leased_retry_ids.remove(&id);
        match result {
            Ok(record) => {
                self.enforce_retention()?;
                Ok(record)
            }
            Err(error) => {
                let _ = self.enforce_retention();
                Err(error)
            }
        }
    }

    fn fail_retry(&mut self, id: i64, failure: String) -> HistoryResult<HistoryRecord> {
        if !self.leased_retry_ids.contains(&id) {
            return Err(HistoryError::InvalidTransition(format!(
                "record {id} has no active retry lease"
            )));
        }
        let result = (|| {
            validate_text("failure", &failure, MAX_FAILURE_BYTES, false)?;
            self.require_status(id, HistoryStatus::Failed)?;
            let now = now_ms();
            let changed = self.connection.execute(
                "UPDATE history SET updated_at_ms = ?1, completed_at_ms = ?1,
                    failure = ?2, output_outcome = NULL, retry_count = retry_count + 1
                 WHERE id = ?3 AND status = 'failed' AND retry_count < 4294967295",
                params![now, failure, id],
            )?;
            if changed != 1 {
                return Err(HistoryError::InvalidTransition(format!(
                    "record {id} retry count overflowed"
                )));
            }
            self.record(id)
        })();
        self.leased_retry_ids.remove(&id);
        match result {
            Ok(record) => {
                self.enforce_retention()?;
                Ok(record)
            }
            Err(error) => {
                let _ = self.enforce_retention();
                Err(error)
            }
        }
    }

    fn release_retry(&mut self, id: i64) -> RetryReleaseAcknowledgement {
        self.leased_retry_ids.remove(&id);
        RetryReleaseAcknowledgement {
            retention_error: self
                .enforce_retention()
                .err()
                .map(|error| error.to_string()),
        }
    }

    fn retry(&mut self, id: i64) -> HistoryResult<RetryHistoryEntry> {
        if self.leased_retry_ids.contains(&id) {
            return Err(HistoryError::InvalidTransition(format!(
                "record {id} already has an active retry"
            )));
        }
        let record = self.record(id)?;
        if record.status != HistoryStatus::Failed {
            return Err(HistoryError::InvalidTransition(format!(
                "record {id} is {}, expected failed",
                record.status.as_str()
            )));
        }
        let relative = record.audio_path.as_deref().ok_or_else(|| {
            HistoryError::InvalidTransition(format!("record {id} has no retained audio"))
        })?;
        let prepared_audio = audio::load_audio(&self.root, relative)?;
        self.leased_retry_ids.insert(id);
        Ok(RetryHistoryEntry {
            record,
            audio: prepared_audio,
        })
    }

    fn search(&self, query: HistoryQuery) -> HistoryResult<HistoryPage> {
        let mut sql = String::from(
            "SELECT id, created_at_ms, updated_at_ms, completed_at_ms, status,
                raw_text, final_text, model_id, audio_duration_ms,
                processing_duration_ms, realtime_factor, pinned, source_app,
                audio_path, failure, retry_count, output_outcome FROM history WHERE 1 = 1",
        );
        let mut values = Vec::<Value>::new();
        if let Some(text) = query
            .text
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty())
        {
            validate_text("search text", text, MAX_IDENTIFIER_BYTES, false)?;
            sql.push_str(
                " AND (raw_text LIKE ? ESCAPE '\\' OR final_text LIKE ? ESCAPE '\\'
                  OR model_id LIKE ? ESCAPE '\\' OR source_app LIKE ? ESCAPE '\\')",
            );
            let pattern = Value::Text(format!("%{}%", escape_like(text)));
            values.extend([pattern.clone(), pattern.clone(), pattern.clone(), pattern]);
        }
        if let Some(status) = query.status {
            sql.push_str(" AND status = ?");
            values.push(Value::Text(status.as_str().into()));
        }
        if let Some(pinned) = query.pinned {
            sql.push_str(" AND pinned = ?");
            values.push(Value::Integer(i64::from(pinned)));
        }
        if let Some(cursor) = query.before {
            sql.push_str(" AND (created_at_ms < ? OR (created_at_ms = ? AND id < ?))");
            values.extend([
                Value::Integer(cursor.created_at_ms),
                Value::Integer(cursor.created_at_ms),
                Value::Integer(cursor.id),
            ]);
        }
        sql.push_str(" ORDER BY created_at_ms DESC, id DESC LIMIT ?");
        values.push(Value::Integer((query.limit + 1) as i64));
        let mut statement = self.connection.prepare(&sql)?;
        let rows = statement.query_map(params_from_iter(values), map_record)?;
        let mut records = rows.collect::<Result<Vec<_>, _>>()?;
        let next = if records.len() > query.limit {
            records.truncate(query.limit);
            records.last().map(|record| HistoryCursor {
                created_at_ms: record.created_at_ms,
                id: record.id,
            })
        } else {
            None
        };
        Ok(HistoryPage { records, next })
    }

    fn set_pinned(&mut self, id: i64, pinned: bool) -> HistoryResult<HistoryRecord> {
        self.reject_leased_mutation(id, "change pin state")?;
        self.record(id)?;
        self.connection.execute(
            "UPDATE history SET pinned = ?1, updated_at_ms = ?2 WHERE id = ?3",
            params![pinned, now_ms(), id],
        )?;
        let record = self.record(id)?;
        if !pinned {
            self.enforce_retention()?;
        }
        Ok(record)
    }

    fn set_retention_policy(&mut self, policy: HistoryRetentionPolicy) -> HistoryResult<()> {
        validate_retention_policy(policy)?;
        self.retention_policy = policy;
        self.enforce_retention()
    }

    fn record_output_outcome(&mut self, id: i64, outcome: String) -> HistoryResult<HistoryRecord> {
        validate_text("output outcome", &outcome, MAX_OUTPUT_OUTCOME_BYTES, false)?;
        let outcome = sanitize_output_outcome(&outcome);
        if outcome.is_empty() {
            return Err(HistoryError::InvalidInput(
                "output outcome must contain visible text".into(),
            ));
        }
        let status = self.record(id)?.status;
        if status == HistoryStatus::Pending {
            return Err(HistoryError::InvalidTransition(format!(
                "record {id} is pending; output outcome requires a terminal record"
            )));
        }
        self.connection.execute(
            "UPDATE history SET output_outcome = ?1, updated_at_ms = ?2 WHERE id = ?3",
            params![outcome, now_ms(), id],
        )?;
        self.record(id)
    }

    fn delete_audio(&mut self, id: i64) -> HistoryResult<HistoryRecord> {
        self.reject_leased_mutation(id, "delete audio")?;
        let record = self.record(id)?;
        if let Some(relative) = record.audio_path {
            self.begin_deletion(id, Some(&relative), false)?;
            self.finish_deletion(id, Some(&relative), false)?;
        }
        self.record(id)
    }

    fn delete(&mut self, id: i64) -> HistoryResult<()> {
        self.reject_leased_mutation(id, "delete entry")?;
        let record = self.record(id)?;
        self.begin_deletion(id, record.audio_path.as_deref(), true)?;
        self.finish_deletion(id, record.audio_path.as_deref(), true)
    }

    fn validated_audio_path(&self, id: i64) -> HistoryResult<Option<PathBuf>> {
        let record = self.record(id)?;
        record
            .audio_path
            .as_deref()
            .map(|relative| audio::resolve_audio_path(&self.root, relative, true))
            .transpose()
    }

    fn record(&self, id: i64) -> HistoryResult<HistoryRecord> {
        self.connection
            .query_row(
                "SELECT id, created_at_ms, updated_at_ms, completed_at_ms, status,
                    raw_text, final_text, model_id, audio_duration_ms,
                    processing_duration_ms, realtime_factor, pinned, source_app,
                    audio_path, failure, retry_count, output_outcome FROM history WHERE id = ?1",
                [id],
                map_record,
            )
            .optional()?
            .ok_or(HistoryError::NotFound(id))
    }

    fn require_status(&self, id: i64, expected: HistoryStatus) -> HistoryResult<()> {
        let actual = self.record(id)?.status;
        if actual != expected {
            return Err(HistoryError::InvalidTransition(format!(
                "record {id} is {}, expected {}",
                actual.as_str(),
                expected.as_str()
            )));
        }
        Ok(())
    }

    fn begin_deletion(
        &mut self,
        id: i64,
        relative: Option<&Path>,
        delete_record: bool,
    ) -> HistoryResult<()> {
        let transaction = self.connection.transaction()?;
        transaction.execute(
            "INSERT INTO deletion_journal (history_id, audio_path, delete_record, created_at_ms)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(history_id) DO UPDATE SET audio_path = excluded.audio_path,
                delete_record = excluded.delete_record, created_at_ms = excluded.created_at_ms",
            params![
                id,
                relative.map(path_to_database).transpose()?,
                delete_record,
                now_ms()
            ],
        )?;
        transaction.commit()?;
        Ok(())
    }

    fn finish_deletion(
        &mut self,
        id: i64,
        relative: Option<&Path>,
        delete_record: bool,
    ) -> HistoryResult<()> {
        if let Some(relative) = relative {
            audio::remove_audio(&self.root, relative)?;
        }
        let transaction = self.connection.transaction()?;
        finish_database_deletion(&transaction, id, delete_record)?;
        transaction.commit()?;
        Ok(())
    }

    fn enforce_retention(&mut self) -> HistoryResult<()> {
        let now = now_ms();
        if let Some(days) = self.retention_policy.audio_retention_days {
            let cutoff = retention_cutoff(now, days);
            let audio_ids = {
                let mut statement = self.connection.prepare(
                    "SELECT id FROM history WHERE pinned = 0 AND status != 'pending'
                     AND audio_path IS NOT NULL AND created_at_ms < ?1
                     ORDER BY created_at_ms, id",
                )?;
                statement
                    .query_map([cutoff], |row| row.get(0))?
                    .collect::<Result<Vec<i64>, _>>()?
            };
            for id in audio_ids {
                if !self.leased_retry_ids.contains(&id) {
                    self.delete_audio(id)?;
                }
            }
        }
        if let Some(days) = self.retention_policy.transcript_retention_days {
            let cutoff = retention_cutoff(now, days);
            let expired_ids = {
                let mut statement = self.connection.prepare(
                    "SELECT id FROM history WHERE pinned = 0 AND status != 'pending'
                     AND created_at_ms < ?1 ORDER BY created_at_ms, id",
                )?;
                statement
                    .query_map([cutoff], |row| row.get(0))?
                    .collect::<Result<Vec<i64>, _>>()?
            };
            for id in expired_ids {
                if !self.leased_retry_ids.contains(&id) {
                    self.delete(id)?;
                }
            }
        }
        let ids = {
            let mut statement = self.connection.prepare(
                "SELECT id FROM history
                 WHERE pinned = 0 AND status != 'pending'
                 ORDER BY created_at_ms DESC, id DESC LIMIT -1 OFFSET ?1",
            )?;
            statement
                .query_map(
                    [i64::from(self.retention_policy.max_unpinned_entries)],
                    |row| row.get(0),
                )?
                .collect::<Result<Vec<i64>, _>>()?
        };
        for id in ids {
            if !self.leased_retry_ids.contains(&id) {
                self.delete(id)?;
            }
        }
        Ok(())
    }

    fn reject_leased_mutation(&self, id: i64, operation: &str) -> HistoryResult<()> {
        if self.leased_retry_ids.contains(&id) {
            return Err(HistoryError::InvalidTransition(format!(
                "cannot {operation} for record {id} during an active retry"
            )));
        }
        Ok(())
    }

    fn reconcile(&mut self) -> HistoryResult<ReconciliationReport> {
        let mut report = ReconciliationReport::default();
        let now = now_ms();
        report.interrupted_pending_failed = self.connection.execute(
            "UPDATE history SET status = 'failed', updated_at_ms = ?1,
                completed_at_ms = ?1, failure = ?2 WHERE status = 'pending'",
            params![now, INTERRUPTED_FAILURE],
        )?;

        let journal = {
            let mut statement = self.connection.prepare(
                "SELECT history_id, audio_path, delete_record
                 FROM deletion_journal ORDER BY history_id",
            )?;
            statement
                .query_map([], |row| {
                    let path = row.get::<_, Option<String>>(1)?.map(PathBuf::from);
                    Ok((row.get::<_, i64>(0)?, path, row.get::<_, bool>(2)?))
                })?
                .collect::<Result<Vec<_>, _>>()?
        };
        for (id, relative, delete_record) in journal {
            self.finish_deletion(id, relative.as_deref(), delete_record)?;
            report.deletions_completed += 1;
        }

        let referenced_rows = {
            let mut statement = self
                .connection
                .prepare("SELECT id, audio_path FROM history WHERE audio_path IS NOT NULL")?;
            statement
                .query_map([], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        PathBuf::from(row.get::<_, String>(1)?),
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()?
        };
        let mut referenced = HashSet::new();
        for (id, relative) in referenced_rows {
            match audio::resolve_audio_path(&self.root, &relative, true) {
                Ok(_) => {
                    referenced.insert(relative);
                }
                Err(HistoryError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                    self.connection.execute(
                        "UPDATE history SET audio_path = NULL, updated_at_ms = ?1 WHERE id = ?2",
                        params![now_ms(), id],
                    )?;
                    report.missing_audio_cleared += 1;
                }
                Err(error) => return Err(error),
            }
        }
        let (orphaned, temporary) = audio::reconcile_audio_directory(&self.root, &referenced)?;
        report.orphan_audio_removed = orphaned;
        report.temporary_audio_removed = temporary;
        self.enforce_retention()?;
        Ok(report)
    }
}

fn migrate(connection: &Connection) -> HistoryResult<()> {
    let version = connection.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))?;
    if version > SCHEMA_VERSION {
        return Err(HistoryError::Corrupt(format!(
            "history schema version {version} is newer than supported version {SCHEMA_VERSION}"
        )));
    }
    if version == 0 {
        connection.execute_batch(
            "BEGIN IMMEDIATE;
             CREATE TABLE history (
                id INTEGER PRIMARY KEY,
                created_at_ms INTEGER NOT NULL,
                updated_at_ms INTEGER NOT NULL,
                completed_at_ms INTEGER,
                status TEXT NOT NULL CHECK(status IN ('pending', 'completed', 'failed')),
                raw_text TEXT NOT NULL,
                final_text TEXT,
                model_id TEXT NOT NULL,
                audio_duration_ms INTEGER,
                processing_duration_ms INTEGER,
                realtime_factor REAL,
                pinned INTEGER NOT NULL DEFAULT 0 CHECK(pinned IN (0, 1)),
                source_app TEXT,
                audio_path TEXT,
                failure TEXT,
                retry_count INTEGER NOT NULL DEFAULT 0 CHECK(retry_count >= 0),
                output_outcome TEXT
             );
             CREATE INDEX history_order_idx ON history(created_at_ms DESC, id DESC);
             CREATE INDEX history_retention_idx
                ON history(pinned, status, created_at_ms DESC, id DESC);
             CREATE TABLE deletion_journal (
                history_id INTEGER PRIMARY KEY,
                audio_path TEXT,
                delete_record INTEGER NOT NULL CHECK(delete_record IN (0, 1)),
                created_at_ms INTEGER NOT NULL
             );
             PRAGMA user_version = 1;
             COMMIT;",
        )?;
    }
    Ok(())
}

fn finish_database_deletion(
    transaction: &Transaction<'_>,
    id: i64,
    delete_record: bool,
) -> HistoryResult<()> {
    if delete_record {
        transaction.execute("DELETE FROM history WHERE id = ?1", [id])?;
    } else {
        transaction.execute(
            "UPDATE history SET audio_path = NULL, updated_at_ms = ?1 WHERE id = ?2",
            params![now_ms(), id],
        )?;
    }
    transaction.execute("DELETE FROM deletion_journal WHERE history_id = ?1", [id])?;
    Ok(())
}

fn map_record(row: &Row<'_>) -> rusqlite::Result<HistoryRecord> {
    let status_text = row.get::<_, String>(4)?;
    let status = HistoryStatus::parse(&status_text).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(4, Type::Text, Box::new(error))
    })?;
    let audio_duration = row.get::<_, Option<i64>>(8)?;
    let processing_duration = row.get::<_, Option<i64>>(9)?;
    let retry_count = row.get::<_, i64>(15)?;
    Ok(HistoryRecord {
        id: row.get(0)?,
        created_at_ms: row.get(1)?,
        updated_at_ms: row.get(2)?,
        completed_at_ms: row.get(3)?,
        status,
        raw_text: row.get(5)?,
        final_text: row.get(6)?,
        model_id: row.get(7)?,
        metrics: HistoryMetrics {
            audio_duration_ms: optional_i64_to_u64(audio_duration, 8)?,
            processing_duration_ms: optional_i64_to_u64(processing_duration, 9)?,
            realtime_factor: row.get(10)?,
        },
        pinned: row.get(11)?,
        source_app: row.get(12)?,
        audio_path: row.get::<_, Option<String>>(13)?.map(PathBuf::from),
        failure: row.get(14)?,
        retry_count: u32::try_from(retry_count).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(15, Type::Integer, Box::new(error))
        })?,
        output_outcome: row.get(16)?,
    })
}

fn validate_new_entry(entry: &NewHistoryEntry) -> HistoryResult<()> {
    validate_text("raw text", &entry.raw_text, MAX_TEXT_BYTES, true)?;
    validate_text("model id", &entry.model_id, MAX_IDENTIFIER_BYTES, false)?;
    if let Some(source_app) = &entry.source_app {
        validate_text("source app", source_app, MAX_IDENTIFIER_BYTES, false)?;
    }
    validate_metrics(&entry.metrics)
}

fn validate_retention_policy(policy: HistoryRetentionPolicy) -> HistoryResult<()> {
    if policy.max_unpinned_entries == 0 {
        return Err(HistoryError::InvalidInput(
            "maximum unpinned history entries must be non-zero".into(),
        ));
    }
    Ok(())
}

fn validate_metrics(metrics: &HistoryMetrics) -> HistoryResult<()> {
    optional_u64_to_i64(metrics.audio_duration_ms)?;
    optional_u64_to_i64(metrics.processing_duration_ms)?;
    if metrics
        .realtime_factor
        .is_some_and(|value| !value.is_finite() || value < 0.0)
    {
        return Err(HistoryError::InvalidInput(
            "realtime factor must be finite and non-negative".into(),
        ));
    }
    Ok(())
}

fn validate_text(
    name: &str,
    value: &str,
    maximum_bytes: usize,
    allow_empty: bool,
) -> HistoryResult<()> {
    if (!allow_empty && value.trim().is_empty()) || value.len() > maximum_bytes {
        return Err(HistoryError::InvalidInput(format!(
            "{name} must {}and contain at most {maximum_bytes} UTF-8 bytes",
            if allow_empty { "" } else { "be non-empty " }
        )));
    }
    Ok(())
}

fn optional_u64_to_i64(value: Option<u64>) -> HistoryResult<Option<i64>> {
    value
        .map(|value| {
            i64::try_from(value).map_err(|_| {
                HistoryError::InvalidInput("metric exceeds SQLite integer range".into())
            })
        })
        .transpose()
}

fn optional_i64_to_u64(value: Option<i64>, column: usize) -> rusqlite::Result<Option<u64>> {
    value
        .map(|value| {
            u64::try_from(value).map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(column, Type::Integer, Box::new(error))
            })
        })
        .transpose()
}

fn retention_cutoff(now: i64, days: u32) -> i64 {
    now.saturating_sub(i64::from(days).saturating_mul(86_400_000))
}

fn escape_like(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

fn sanitize_output_outcome(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn path_to_database(path: &Path) -> HistoryResult<String> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| HistoryError::UnsafePath(path.display().to_string()))
}

fn now_ms() -> i64 {
    let value = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    i64::try_from(value).unwrap_or(i64::MAX)
}
