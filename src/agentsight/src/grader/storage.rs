//! SQLite persistence for grader evaluation runs.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use agentsight_sqlite_lifecycle::{
    CheckpointOutcome, ConnectionOptions, SizeBasis, SizePolicy, checkpoint_truncate,
    enforce_size_policy, measure_database, open_connection, retention_cutoff_ns,
};
use rusqlite::{Connection, params};

use super::types::{
    EvaluationResult, EvaluationRunRecord, EvaluationStatus, GraderError, GraderType, RootCause,
    TargetType, Verdict,
};

/// SQLite-backed persistence for evaluation runs.
pub struct EvaluationStore {
    conn: Mutex<Connection>,
    db_path: PathBuf,
    storage_policy: crate::config::InsertStoragePolicy,
    maintenance_insert_count: AtomicU64,
}

impl EvaluationStore {
    /// Open an evaluation store using the given SQLite path without automatic maintenance.
    pub fn new_with_path(path: &Path) -> Result<Self, GraderError> {
        Self::new_with_policy(path, crate::config::InsertStoragePolicy::default())
    }

    /// Open an evaluation store with an explicit shared GenAI lifecycle policy.
    pub fn new_with_policy(
        path: &Path,
        storage_policy: crate::config::InsertStoragePolicy,
    ) -> Result<Self, GraderError> {
        let conn = open_connection(path, ConnectionOptions::default())
            .map_err(|error| GraderError::Storage(error.to_string()))?;
        let store = EvaluationStore {
            conn: Mutex::new(conn),
            db_path: path.to_path_buf(),
            storage_policy,
            maintenance_insert_count: AtomicU64::new(0),
        };
        store.init_tables()?;
        Ok(store)
    }

    fn init_tables(&self) -> Result<(), GraderError> {
        let conn = self
            .conn
            .lock()
            .map_err(|error| GraderError::Storage(error.to_string()))?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS evaluation_runs (
                id               INTEGER PRIMARY KEY AUTOINCREMENT,
                run_id           TEXT NOT NULL UNIQUE,
                target_type      TEXT NOT NULL,
                target_id        TEXT NOT NULL,
                input_hash       TEXT NOT NULL,
                grader_type      TEXT NOT NULL,
                grader_version   TEXT NOT NULL,
                rubric_version   TEXT,
                judge_model      TEXT,
                prompt_hash      TEXT,
                confidence       REAL,
                status           TEXT NOT NULL,
                verdict          TEXT,
                score            REAL,
                root_cause       TEXT,
                created_at       DATETIME DEFAULT CURRENT_TIMESTAMP,
                completed_at     DATETIME,
                result_json      TEXT,
                UNIQUE(target_type, target_id, input_hash, grader_type, grader_version)
            );
            CREATE INDEX IF NOT EXISTS idx_evaluation_runs_target_latest
                ON evaluation_runs(target_type, target_id, created_at DESC);
            CREATE INDEX IF NOT EXISTS idx_evaluation_runs_run_id
                ON evaluation_runs(run_id);",
        )
        .map_err(|error| GraderError::Storage(error.to_string()))?;
        Ok(())
    }

    /// Return an existing completed run for the idempotency key, if present.
    pub fn find_completed(
        &self,
        target_type: TargetType,
        target_id: &str,
        input_hash: &str,
        grader_type: GraderType,
        grader_version: &str,
    ) -> Result<Option<EvaluationRunRecord>, GraderError> {
        let raw = {
            let conn = self
                .conn
                .lock()
                .map_err(|error| GraderError::Storage(error.to_string()))?;
            let mut stmt = conn
                .prepare(
                    "SELECT id, run_id, target_type, target_id, input_hash, grader_type,
                        grader_version, status, verdict, score, root_cause, created_at,
                        completed_at, result_json
                 FROM evaluation_runs
                 WHERE target_type=?1
                   AND target_id=?2
                   AND input_hash=?3
                   AND grader_type=?4
                   AND grader_version=?5
                   AND status='completed'
                 LIMIT 1",
                )
                .map_err(|error| GraderError::Storage(error.to_string()))?;
            let mut rows = stmt
                .query(params![
                    target_type.as_str(),
                    target_id,
                    input_hash,
                    grader_type.as_str(),
                    grader_version,
                ])
                .map_err(|error| GraderError::Storage(error.to_string()))?;
            match rows
                .next()
                .map_err(|error| GraderError::Storage(error.to_string()))?
            {
                Some(row) => Some(
                    read_raw_record(row)
                        .map_err(|error| GraderError::Storage(error.to_string()))?,
                ),
                None => None,
            }
        };
        raw.map(raw_to_record).transpose()
    }

    /// Insert a completed evaluation result.
    ///
    /// Returns `false` when an equivalent completed run already exists.
    pub fn insert_completed(&self, result: &EvaluationResult) -> Result<bool, GraderError> {
        let result_json = serde_json::to_string(result)?;
        let inserted = {
            let conn = self
                .conn
                .lock()
                .map_err(|error| GraderError::Storage(error.to_string()))?;
            conn.execute(
                "INSERT OR IGNORE INTO evaluation_runs (
                    run_id, target_type, target_id, input_hash, grader_type, grader_version,
                    rubric_version, judge_model, prompt_hash, confidence, status, verdict,
                    score, root_cause, completed_at, result_json
                ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,CURRENT_TIMESTAMP,?15)",
                params![
                    &result.run_id,
                    result.target_type.as_str(),
                    &result.target_id,
                    &result.input_hash,
                    result.metadata.grader_type.as_str(),
                    &result.metadata.grader_version,
                    &result.metadata.rubric_version,
                    &result.metadata.judge_model,
                    &result.metadata.prompt_hash,
                    result.metadata.confidence,
                    EvaluationStatus::Completed.as_str(),
                    result.verdict.as_str(),
                    result.score,
                    result.root_cause.as_str(),
                    result_json,
                ],
            )
            .map_err(|error| GraderError::Storage(error.to_string()))?
        };
        if inserted > 0 {
            self.maybe_maintain()?;
        }
        Ok(inserted > 0)
    }

    fn maybe_maintain(&self) -> Result<(), GraderError> {
        let interval = self.storage_policy.check_interval_inserts;
        if interval == 0 {
            return Ok(());
        }
        let count = self
            .maintenance_insert_count
            .fetch_add(1, Ordering::Relaxed)
            .saturating_add(1);
        if !count.is_multiple_of(interval) {
            return Ok(());
        }
        self.maintain()
            .map_err(|error| GraderError::Storage(error.to_string()))
    }

    fn maintain(&self) -> Result<(), Box<dyn std::error::Error>> {
        let deleted_by_age = if self.storage_policy.retention_days > 0 {
            let now_ns = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|duration| u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX))
                .unwrap_or(0);
            let cutoff = retention_cutoff_ns(now_ns, self.storage_policy.retention_days)?;
            let cutoff_seconds = i64::try_from(cutoff / 1_000_000_000).unwrap_or(i64::MAX);
            let conn = self
                .conn
                .lock()
                .map_err(|error| format!("evaluation store connection mutex poisoned: {error}"))?;
            conn.execute(
                "DELETE FROM evaluation_runs
                 WHERE CAST(strftime('%s', created_at) AS INTEGER) < ?1",
                params![cutoff_seconds],
            )?
        } else {
            0
        };
        if deleted_by_age > 0 {
            let conn = self
                .conn
                .lock()
                .map_err(|error| format!("evaluation store connection mutex poisoned: {error}"))?;
            if checkpoint_truncate(&conn)? == CheckpointOutcome::Busy {
                return Ok(());
            }
        }

        let limit_bytes = self
            .storage_policy
            .max_db_size_mb
            .saturating_mul(1024 * 1024);
        enforce_size_policy::<Box<dyn std::error::Error>>(
            SizePolicy {
                limit_bytes,
                trigger_bytes: limit_bytes,
                target_bytes: limit_bytes.saturating_mul(9) / 10,
                trigger_basis: SizeBasis::Physical,
                target_basis: SizeBasis::Logical,
                max_rounds: 20,
                max_stalled_rounds: 3,
            },
            || {
                let conn = self.conn.lock().map_err(|error| {
                    format!("evaluation store connection mutex poisoned: {error}")
                })?;
                measure_database(&self.db_path, &conn).map_err(Into::into)
            },
            |fraction| {
                let conn = self.conn.lock().map_err(|error| {
                    format!("evaluation store connection mutex poisoned: {error}")
                })?;
                let count: i64 =
                    conn.query_row("SELECT COUNT(*) FROM evaluation_runs", [], |row| row.get(0))?;
                let limit = if count > 0 {
                    ((count as f64 * fraction.clamp(0.0, 1.0)) as i64).max(1)
                } else {
                    0
                };
                conn.execute(
                    "DELETE FROM evaluation_runs WHERE id IN (
                        SELECT id FROM evaluation_runs ORDER BY created_at ASC, id ASC LIMIT ?1
                    )",
                    params![limit],
                )
                .map_err(Into::into)
            },
            || {
                let conn = self.conn.lock().map_err(|error| {
                    format!("evaluation store connection mutex poisoned: {error}")
                })?;
                checkpoint_truncate(&conn).map_err(Into::into)
            },
        )?;
        Ok(())
    }

    /// Return the latest completed run for a target.
    pub fn latest_completed(
        &self,
        target_type: TargetType,
        target_id: &str,
    ) -> Result<Option<EvaluationRunRecord>, GraderError> {
        let raw = {
            let conn = self
                .conn
                .lock()
                .map_err(|error| GraderError::Storage(error.to_string()))?;
            let mut stmt = conn
                .prepare(
                    "SELECT id, run_id, target_type, target_id, input_hash, grader_type,
                        grader_version, status, verdict, score, root_cause, created_at,
                        completed_at, result_json
                 FROM evaluation_runs
                 WHERE target_type=?1
                   AND target_id=?2
                   AND status='completed'
                 ORDER BY created_at DESC, id DESC
                 LIMIT 1",
                )
                .map_err(|error| GraderError::Storage(error.to_string()))?;
            let mut rows = stmt
                .query(params![target_type.as_str(), target_id])
                .map_err(|error| GraderError::Storage(error.to_string()))?;
            match rows
                .next()
                .map_err(|error| GraderError::Storage(error.to_string()))?
            {
                Some(row) => Some(
                    read_raw_record(row)
                        .map_err(|error| GraderError::Storage(error.to_string()))?,
                ),
                None => None,
            }
        };
        raw.map(raw_to_record).transpose()
    }
}

struct RawEvaluationRunRecord {
    id: i64,
    run_id: String,
    target_type: String,
    target_id: String,
    input_hash: String,
    grader_type: String,
    grader_version: String,
    status: String,
    verdict: Option<String>,
    score: Option<f64>,
    root_cause: Option<String>,
    created_at: String,
    completed_at: Option<String>,
    result_json: Option<String>,
}

fn read_raw_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<RawEvaluationRunRecord> {
    Ok(RawEvaluationRunRecord {
        id: row.get(0)?,
        run_id: row.get(1)?,
        target_type: row.get(2)?,
        target_id: row.get(3)?,
        input_hash: row.get(4)?,
        grader_type: row.get(5)?,
        grader_version: row.get(6)?,
        status: row.get(7)?,
        verdict: row.get(8)?,
        score: row.get(9)?,
        root_cause: row.get(10)?,
        created_at: row.get(11)?,
        completed_at: row.get(12)?,
        result_json: row.get(13)?,
    })
}

fn raw_to_record(raw: RawEvaluationRunRecord) -> Result<EvaluationRunRecord, GraderError> {
    let target_type = parse_target_type(raw.target_type)?;
    let grader_type = parse_grader_type(raw.grader_type)?;
    let status = parse_status(raw.status)?;
    let verdict = raw.verdict.map(parse_verdict).transpose()?;
    let root_cause = raw.root_cause.map(parse_root_cause).transpose()?;
    let result = raw
        .result_json
        .as_deref()
        .map(serde_json::from_str)
        .transpose()?;

    Ok(EvaluationRunRecord {
        id: raw.id,
        run_id: raw.run_id,
        target_type,
        target_id: raw.target_id,
        input_hash: raw.input_hash,
        grader_type,
        grader_version: raw.grader_version,
        status,
        verdict,
        score: raw.score,
        root_cause,
        created_at: raw.created_at,
        completed_at: raw.completed_at,
        result,
    })
}

fn parse_target_type(value: String) -> Result<TargetType, GraderError> {
    match value.as_str() {
        "conversation" => Ok(TargetType::Conversation),
        _ => Err(GraderError::Storage(format!(
            "unknown target_type: {value}"
        ))),
    }
}

fn parse_grader_type(value: String) -> Result<GraderType, GraderError> {
    match value.as_str() {
        "rule" => Ok(GraderType::Rule),
        "llm" => Ok(GraderType::Llm),
        "agent" => Ok(GraderType::Agent),
        _ => Err(GraderError::Storage(format!(
            "unknown grader_type: {value}"
        ))),
    }
}

fn parse_status(value: String) -> Result<EvaluationStatus, GraderError> {
    match value.as_str() {
        "completed" => Ok(EvaluationStatus::Completed),
        "failed" => Ok(EvaluationStatus::Failed),
        _ => Err(GraderError::Storage(format!(
            "unknown evaluation status: {value}"
        ))),
    }
}

fn parse_verdict(value: String) -> Result<Verdict, GraderError> {
    match value.as_str() {
        "pass" => Ok(Verdict::Pass),
        "warn" => Ok(Verdict::Warn),
        "fail" => Ok(Verdict::Fail),
        _ => Err(GraderError::Storage(format!("unknown verdict: {value}"))),
    }
}

fn parse_root_cause(value: String) -> Result<RootCause, GraderError> {
    match value.as_str() {
        "none" => Ok(RootCause::None),
        "no_final_answer" => Ok(RootCause::NoFinalAnswer),
        "interrupted_main_call" => Ok(RootCause::InterruptedMainCall),
        "agent_crash" => Ok(RootCause::AgentCrash),
        "runtime_error" => Ok(RootCause::RuntimeError),
        "tool_failure" => Ok(RootCause::ToolFailure),
        "safety_risk" => Ok(RootCause::SafetyRisk),
        "loop_detected" => Ok(RootCause::LoopDetected),
        "excessive_cost" => Ok(RootCause::ExcessiveCost),
        "partial_snapshot" => Ok(RootCause::PartialSnapshot),
        _ => Err(GraderError::Storage(format!("unknown root_cause: {value}"))),
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;
    use crate::grader::types::{
        EvaluationMetadata, EvaluationResult, GraderType, RULE_GRADER_VERSION, TargetType, Verdict,
    };

    #[test]
    fn insert_completed_is_idempotent_for_same_input_key() {
        let path = temp_db_path("grader_store_idempotency");
        let store = EvaluationStore::new_with_path(&path).unwrap();
        let first = evaluation_result("run-first", "input-hash-1");
        let duplicate = evaluation_result("run-duplicate", "input-hash-1");

        assert!(store.insert_completed(&first).unwrap());
        assert!(!store.insert_completed(&duplicate).unwrap());

        let found = store
            .find_completed(
                TargetType::Conversation,
                "conv-1",
                "input-hash-1",
                GraderType::Rule,
                RULE_GRADER_VERSION,
            )
            .unwrap()
            .expect("completed run should be found");

        assert_eq!(found.run_id, "run-first");
        assert_eq!(
            found.result.expect("result_json should exist").run_id,
            "run-first"
        );

        cleanup_db(&path);
    }

    #[test]
    fn evaluation_writes_apply_shared_retention_policy() {
        let path = temp_db_path("grader_store_retention");
        let store = EvaluationStore::new_with_policy(
            &path,
            crate::config::InsertStoragePolicy {
                retention_days: 1,
                max_db_size_mb: 0,
                check_interval_inserts: 1,
            },
        )
        .unwrap();
        let expired = evaluation_result("run-expired", "expired-hash");
        assert!(store.insert_completed(&expired).unwrap());
        store
            .conn
            .lock()
            .unwrap()
            .execute(
                "UPDATE evaluation_runs SET created_at = '2000-01-01 00:00:00' WHERE run_id = ?1",
                params!["run-expired"],
            )
            .unwrap();

        let current = evaluation_result("run-current", "current-hash");
        assert!(store.insert_completed(&current).unwrap());

        assert!(
            store
                .find_completed(
                    TargetType::Conversation,
                    "conv-1",
                    "expired-hash",
                    GraderType::Rule,
                    RULE_GRADER_VERSION,
                )
                .unwrap()
                .is_none()
        );
        cleanup_db(&path);
    }

    fn evaluation_result(run_id: &str, input_hash: &str) -> EvaluationResult {
        EvaluationResult {
            target_type: TargetType::Conversation,
            target_id: "conv-1".to_string(),
            run_id: run_id.to_string(),
            input_hash: input_hash.to_string(),
            verdict: Verdict::Pass,
            score: 1.0,
            summary: "ok".to_string(),
            root_cause: RootCause::None,
            recommended_action: "none".to_string(),
            dimensions: Vec::new(),
            findings: Vec::new(),
            metadata: EvaluationMetadata {
                evaluated_with_pending: false,
                pending_call_count: 0,
                input_event_count: 1,
                grader_type: GraderType::Rule,
                grader_version: RULE_GRADER_VERSION.to_string(),
                rubric_version: None,
                judge_model: None,
                prompt_hash: None,
                confidence: Some(1.0),
            },
        }
    }

    fn temp_db_path(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "agentsight_{label}_{}.db",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    fn cleanup_db(path: &std::path::Path) {
        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_file(format!("{}-wal", path.display()));
        let _ = std::fs::remove_file(format!("{}-shm", path.display()));
    }
}
