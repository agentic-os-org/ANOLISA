//! SQLite persistence for trajectory reuse labels (`reuse.db`).
//!
//! A derived store: every row can be rebuilt from `trajectories.db` except the
//! human decisions, which exist nowhere else and are therefore never
//! overwritten by a recompute.
//!
//! Kept out of `trajectories.db` on purpose. That database belongs to the
//! collector, which runs on both Linux and macOS and must not start paying for
//! attribution schema it does not use; its migration also rewrites every row,
//! so bolting tables onto it makes collector startup cost unpredictable.
//!
//! Opened through the crate-internal `private_sqlite` helper, like
//! `security.db` and `enforcement.db`: label reasons quote user text, so the
//! database and its WAL sidecars must be owner-only. Callers pass the dedicated
//! private state directory rather than the shared data directory, because
//! opening tightens that directory to `0700`, and doing that to the directory
//! `trajectories.db` lives in would silently cut off every other reader of it.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use super::label::{
    ConfirmState, LabelAction, LabelEventKind, SessionLabel, TrajectoryIdentity, TrajectoryLabel,
};
use super::triage::{TriageMetrics, TriageOutcome};
use agentsight_sqlite_lifecycle::{
    CheckpointOutcome, MaintenanceReport, MaintenanceStatus, SizeBasis, SizePolicy,
    checkpoint_truncate, enforce_size_policy, measure_database, retention_cutoff_ns,
};
use rusqlite::{Connection, OptionalExtension, Row, TransactionBehavior, params};
use serde::Serialize;

/// Schema version recorded in `PRAGMA user_version`.
const SCHEMA_VERSION: i64 = 5;

/// Errors produced by [`ReuseStore`].
#[derive(Debug, thiserror::Error)]
pub enum ReuseStoreError {
    /// The private database could not be opened with owner-only permissions.
    /// Stringified because the underlying error type is crate-internal.
    #[error("open reuse database: {0}")]
    Open(String),
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    /// SQLite lifecycle measurement or maintenance failed.
    #[error("reuse database lifecycle error: {0}")]
    Lifecycle(#[from] agentsight_sqlite_lifecycle::LifecycleError),
    #[error("store mutex poisoned")]
    Poisoned,
    /// A decision was submitted for a trajectory that has not been triaged.
    /// Reported rather than silently creating a row, because a human verdict
    /// with no automatic verdict beside it cannot be checked for misfires.
    #[error("no label row for session {0}; run triage first")]
    UnknownSession(String),
    #[error("stored value {value:?} is not a valid {field}")]
    Corrupt { field: &'static str, value: String },
    /// The file was written by a newer binary. Refusing is the only safe
    /// option: an unknown schema cannot be read correctly, and guessing risks
    /// destroying human decisions.
    #[error("reuse db schema is at v{found}, this binary supports up to v{supported}")]
    SchemaTooNew { found: i64, supported: i64 },
    #[error("no migration registered from schema v{0}")]
    MissingMigration(i64),
}

type Result<T> = std::result::Result<T, ReuseStoreError>;

/// One recorded change to a label, for answering "who set this, and when".
#[derive(Debug, Clone, Serialize)]
pub struct LabelEvent {
    pub event_id: i64,
    pub session_id: String,
    pub from_label: Option<String>,
    pub to_label: String,
    pub action: String,
    pub reason: Option<String>,
    pub decided_by: String,
    pub created_at_ns: i64,
}

/// Filter for [`ReuseStore::list_labels`].
#[derive(Debug, Clone, Default)]
pub struct LabelFilter {
    /// Match rows whose *effective* label is any of these; empty means "any".
    pub effective_labels: Vec<TrajectoryLabel>,
    pub confirm_state: Option<ConfirmState>,
    /// Only rows whose automatic verdict drifted away from a human decision.
    pub only_changed_since_decision: bool,
    /// `0` or less means unlimited.
    pub limit: i64,
}

/// How often each rule's verdict was overturned by a person.
///
/// The original calibration corpus is gone, so these counts are the only
/// remaining way to rank which rule misfires most.
#[derive(Debug, Clone, Serialize)]
pub struct RuleOverrideStat {
    pub rule: String,
    /// Rows this rule contributed to that a human replaced.
    pub overridden: usize,
    /// Rows this rule contributed to that a human accepted.
    pub confirmed: usize,
}

/// Outcome of one retention and capacity maintenance pass.
#[derive(Debug, Clone, Copy)]
pub struct ReuseMaintenanceReport {
    /// Audit events removed because they exceeded the retention period.
    pub expired_events: usize,
    /// Capacity-enforcement result after age retention and checkpointing.
    pub size: MaintenanceReport,
}

/// Thread-safe store over a dedicated `reuse.db`.
#[derive(Debug)]
pub struct ReuseStore {
    conn: Mutex<Connection>,
}

impl ReuseStore {
    /// Name of the database inside the private state directory.
    const DB_NAME: &'static str = crate::config::REUSE_DB_NAME;

    /// Opens the label store in `state_dir` with owner-only permissions.
    ///
    /// `state_dir` must be the dedicated private directory (the one that also
    /// holds `security.db`), never the shared data directory — see the module
    /// docs.
    ///
    /// # Errors
    /// Returns [`ReuseStoreError::Open`] when the directory or database cannot
    /// be made owner-only, [`ReuseStoreError::SchemaTooNew`] when the file was
    /// written by a newer binary, or a SQL error if it cannot be migrated.
    pub fn open_private(state_dir: impl AsRef<Path>) -> Result<Self> {
        let connection =
            crate::private_sqlite::open_private_connection(state_dir.as_ref(), Self::DB_NAME)
                .map_err(|error| ReuseStoreError::Open(error.to_string()))?;
        Self::from_connection(connection)
    }

    /// Wraps an already-open connection and brings its schema up to date.
    ///
    /// # Errors
    /// Returns [`ReuseStoreError::SchemaTooNew`] when the file was written by a
    /// newer binary, or a SQL error if it cannot be migrated.
    fn from_connection(mut conn: Connection) -> Result<Self> {
        ensure_schema(&mut conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, Connection>> {
        self.conn.lock().map_err(|_| ReuseStoreError::Poisoned)
    }

    /// Records a fresh automatic verdict, preserving any human decision.
    ///
    /// Idempotent per `(session_id, triage_version, source_content_hash)`: an
    /// unchanged recompute rewrites the same values and logs nothing. When the
    /// verdict does change, an `auto_retriage` event is appended so a later
    /// disagreement with a human decision can be explained.
    ///
    /// # Errors
    /// Returns an error on SQL failure, a poisoned mutex, or unreadable stored
    /// enum values.
    pub fn upsert_auto_label(
        &self,
        session_id: &str,
        identity: TrajectoryIdentity,
        outcome: TriageOutcome,
        source_content_hash: &str,
        triage_version: &str,
    ) -> Result<SessionLabel> {
        let now = now_ns();
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;

        let existing = read_label(&tx, session_id)?;
        let (label, event) = match existing {
            Some(mut row) => {
                let previous = row.auto_label;
                row.apply_retriage(identity, outcome, source_content_hash, triage_version, now);
                let changed = previous != row.auto_label;
                let event = changed.then_some((LabelEventKind::AutoRetriage, previous));
                (row, event)
            }
            None => (
                SessionLabel::from_outcome(
                    session_id,
                    identity,
                    outcome,
                    source_content_hash,
                    triage_version,
                    now,
                ),
                None,
            ),
        };

        write_label(&tx, &label)?;
        if let Some((kind, previous)) = event {
            append_event(
                &tx,
                session_id,
                Some(previous.as_str()),
                label.auto_label.as_str(),
                kind,
                None,
                "system",
                now,
            )?;
        }
        tx.commit()?;
        Ok(label)
    }

    /// Returns the sessions whose effective label is in `labels`.
    ///
    /// Serves the `label` filter on `/api/trajectories`: an agent pulling
    /// history through a skill asks for "good trajectories" or "everything but
    /// useless" and reads the full original trajectory — the label is a filter
    /// over originals, not a summary of them. Sessions without a label row
    /// (never triaged) match nothing: their quality is unknown, and a filter
    /// asking for `good` must not serve them.
    ///
    /// # Errors
    /// Returns a store error on SQL failure.
    pub fn sessions_with_labels(&self, labels: &[TrajectoryLabel]) -> Result<Vec<String>> {
        if labels.is_empty() {
            return Ok(Vec::new());
        }
        let conn = self.lock()?;
        let placeholders = labels
            .iter()
            .map(|l| format!("'{}'", l.as_str().replace('\'', "''")))
            .collect::<Vec<_>>()
            .join(", ");
        // `effective_label` is derived, not stored: the human > model > rules
        // precedence lives in `label.rs` and is resolved on read. The SQL
        // below must mirror `SessionLabel::effective_label` exactly — a row
        // here whose SQL and Rust disagree would silently vanish from (or leak
        // into) the trajectory label filter.
        let ids: Vec<String> = conn
            .prepare(&format!(
                "SELECT session_id FROM session_labels
                 WHERE COALESCE(human_label, llm_label, auto_label) IN ({placeholders})"
            ))?
            .query_map([], |row| row.get(0))?
            .collect::<std::result::Result<_, _>>()?;
        Ok(ids)
    }

    /// Refreshes only the display identity of an existing row.
    ///
    /// Used on the triage fast path, where the verdict is unchanged but the row
    /// predates the identity columns or the conversation was re-titled. Touches
    /// nothing else and writes no audit event: identity is not a verdict.
    ///
    /// # Errors
    /// Returns [`ReuseStoreError::UnknownSession`] when the row is gone, or a SQL
    /// error on failure.
    pub fn set_identity(&self, session_id: &str, identity: &TrajectoryIdentity) -> Result<()> {
        let conn = self.lock()?;
        let changed = conn.execute(
            "UPDATE session_labels
             SET title = ?2, project = ?3, source = ?4, agent_name = ?5,
                 started_at = ?6, is_subagent = ?7
             WHERE session_id = ?1",
            params![
                session_id,
                identity.title,
                identity.project,
                identity.source,
                identity.agent_name,
                identity.started_at,
                identity.is_subagent as i64,
            ],
        )?;
        if changed == 0 {
            return Err(ReuseStoreError::UnknownSession(session_id.to_string()));
        }
        Ok(())
    }

    /// Records a model judgement on an existing row.
    ///
    /// Requires the row to exist for the same reason a human decision does: a
    /// verdict with no automatic verdict beside it cannot later be compared
    /// against one.
    ///
    /// # Errors
    /// Returns [`ReuseStoreError::UnknownSession`] when the trajectory has not
    /// been triaged, or a SQL error on failure.
    pub fn record_judgement(
        &self,
        session_id: &str,
        verdict: &super::judge::JudgeVerdict,
    ) -> Result<SessionLabel> {
        let now = now_ns();
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;

        let mut label = read_label(&tx, session_id)?
            .ok_or_else(|| ReuseStoreError::UnknownSession(session_id.to_string()))?;
        let before = label.effective_label();
        let kind = label.apply_judgement(
            verdict.label,
            verdict.reason.clone(),
            verdict.cited_steps.clone(),
            verdict.downgraded,
            now,
        );
        write_label(&tx, &label)?;
        append_event(
            &tx,
            session_id,
            Some(before.as_str()),
            label.effective_label().as_str(),
            kind,
            verdict.reason.as_str().into(),
            "model",
            now,
        )?;
        tx.commit()?;
        Ok(label)
    }

    /// Applies a human decision to an existing row.
    ///
    /// # Errors
    /// Returns [`ReuseStoreError::UnknownSession`] when the trajectory has not
    /// been triaged yet, or a SQL error on failure.
    pub fn apply_decision(
        &self,
        session_id: &str,
        action: LabelAction,
        decided_by: &str,
        reason: Option<String>,
    ) -> Result<SessionLabel> {
        let now = now_ns();
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;

        let mut label = read_label(&tx, session_id)?
            .ok_or_else(|| ReuseStoreError::UnknownSession(session_id.to_string()))?;
        let previous = label.effective_label();
        let kind = label.apply_decision(action, decided_by, reason.clone(), now);

        write_label(&tx, &label)?;
        append_event(
            &tx,
            session_id,
            Some(previous.as_str()),
            label.effective_label().as_str(),
            kind,
            reason.as_deref(),
            decided_by,
            now,
        )?;
        tx.commit()?;
        Ok(label)
    }

    /// Confirms several rows in one transaction, skipping any that are absent.
    ///
    /// Batch confirmation is how the trajectory list is meant to be worked
    /// through, so a stale id in the request must not fail the whole call.
    /// Returns the ids actually confirmed.
    ///
    /// # Errors
    /// Returns an error on SQL failure or a poisoned mutex.
    pub fn confirm_batch(&self, session_ids: &[String], decided_by: &str) -> Result<Vec<String>> {
        let now = now_ns();
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        let mut confirmed = Vec::new();
        for session_id in session_ids {
            let Some(mut label) = read_label(&tx, session_id)? else {
                continue;
            };
            let previous = label.effective_label();
            let kind = label.apply_decision(LabelAction::Confirm, decided_by, None, now);
            write_label(&tx, &label)?;
            append_event(
                &tx,
                session_id,
                Some(previous.as_str()),
                label.effective_label().as_str(),
                kind,
                None,
                decided_by,
                now,
            )?;
            confirmed.push(session_id.clone());
        }
        tx.commit()?;
        Ok(confirmed)
    }

    /// Applies audit-event retention and bounds reusable label storage.
    ///
    /// Age retention removes only expired `session_label_events`. Capacity
    /// maintenance starts when physical allocation exceeds `max_db_size_mb` and
    /// stops when logical usage reaches 90% of that limit. It deletes oldest
    /// audit events before removing unconfirmed labels with no human decision;
    /// confirmed, overridden, or otherwise human-owned labels are never eligible.
    /// A busy checkpoint stops the pass so repeated deletes cannot grow the WAL
    /// without converging. This method never runs `VACUUM`.
    ///
    /// A zero value disables the corresponding age or capacity policy.
    ///
    /// # Errors
    /// Returns a named SQLite lifecycle error, SQL error, or poisoned-mutex error.
    pub fn maintain(
        &self,
        retention_days: u64,
        max_db_size_mb: u64,
    ) -> Result<ReuseMaintenanceReport> {
        let expired_events = if retention_days == 0 {
            0
        } else {
            let cutoff = retention_cutoff_ns(u64::try_from(now_ns()).unwrap_or(0), retention_days)?;
            self.delete_events_before(i64::try_from(cutoff).unwrap_or(i64::MAX))?
        };

        let limit_bytes = max_db_size_mb.saturating_mul(1024 * 1024);
        if (expired_events > 0 || limit_bytes > 0) && self.checkpoint()? == CheckpointOutcome::Busy
        {
            let snapshot = self.size_snapshot()?;
            return Ok(ReuseMaintenanceReport {
                expired_events,
                size: MaintenanceReport {
                    status: MaintenanceStatus::CheckpointBusy,
                    rounds: 0,
                    deleted_rows: 0,
                    before: snapshot,
                    after: snapshot,
                },
            });
        }

        let size = enforce_size_policy::<ReuseStoreError>(
            SizePolicy {
                limit_bytes,
                trigger_bytes: limit_bytes,
                target_bytes: limit_bytes.saturating_mul(9) / 10,
                trigger_basis: SizeBasis::Physical,
                target_basis: SizeBasis::Logical,
                max_rounds: 20,
                max_stalled_rounds: 3,
            },
            || self.size_snapshot(),
            |fraction| self.delete_oldest_fraction(fraction),
            || self.checkpoint(),
        )?;

        Ok(ReuseMaintenanceReport {
            expired_events,
            size,
        })
    }

    fn delete_events_before(&self, cutoff_ns: i64) -> Result<usize> {
        let conn = self.lock()?;
        Ok(conn.execute(
            "DELETE FROM session_label_events WHERE created_at_ns < ?1",
            params![cutoff_ns],
        )?)
    }

    fn delete_oldest_fraction(&self, fraction: f64) -> Result<usize> {
        let mut conn = self.lock()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let event_count: i64 =
            tx.query_row("SELECT COUNT(*) FROM session_label_events", [], |row| {
                row.get(0)
            })?;
        if event_count > 0 {
            let limit = fractional_count(event_count, fraction);
            let deleted = tx.execute(
                "DELETE FROM session_label_events WHERE event_id IN (
                    SELECT event_id FROM session_label_events
                    ORDER BY created_at_ns ASC, event_id ASC LIMIT ?1
                )",
                params![limit],
            )?;
            tx.commit()?;
            return Ok(deleted);
        }

        let automatic_count: i64 = tx.query_row(
            "SELECT COUNT(*) FROM session_labels
             WHERE confirm_state = 'unconfirmed'
               AND human_label IS NULL
               AND decided_by IS NULL",
            [],
            |row| row.get(0),
        )?;
        let limit = fractional_count(automatic_count, fraction);
        let deleted = tx.execute(
            "DELETE FROM session_labels WHERE session_id IN (
                SELECT session_id FROM session_labels
                WHERE confirm_state = 'unconfirmed'
                  AND human_label IS NULL
                  AND decided_by IS NULL
                ORDER BY updated_at_ns ASC, session_id ASC LIMIT ?1
            )",
            params![limit],
        )?;
        tx.commit()?;
        Ok(deleted)
    }

    fn size_snapshot(&self) -> Result<agentsight_sqlite_lifecycle::SizeSnapshot> {
        let conn = self.lock()?;
        let path = conn
            .path()
            .filter(|path| !path.is_empty())
            .map_or_else(|| PathBuf::from(":memory:"), PathBuf::from);
        measure_database(&path, &conn).map_err(Into::into)
    }

    fn checkpoint(&self) -> Result<CheckpointOutcome> {
        let conn = self.lock()?;
        checkpoint_truncate(&conn).map_err(Into::into)
    }

    /// Reads one label row.
    ///
    /// # Errors
    /// Returns an error on SQL failure or unreadable stored enum values.
    pub fn get_label(&self, session_id: &str) -> Result<Option<SessionLabel>> {
        let conn = self.lock()?;
        read_label(&conn, session_id)
    }

    /// Lists label rows matching `filter`, newest update first.
    ///
    /// Filtering on the effective label happens in Rust rather than SQL: the
    /// effective value is a function of two columns, and duplicating that rule
    /// in a `WHERE` clause is how the two would drift apart.
    ///
    /// # Errors
    /// Returns an error on SQL failure or unreadable stored enum values.
    pub fn list_labels(&self, filter: &LabelFilter) -> Result<Vec<SessionLabel>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(&format!(
            "SELECT {SELECT_COLUMNS} FROM session_labels ORDER BY updated_at_ns DESC"
        ))?;
        let rows = stmt.query_map([], row_to_label)?;
        let mut out = Vec::new();
        for row in rows {
            let label = row??;
            if !filter.effective_labels.is_empty()
                && !filter.effective_labels.contains(&label.effective_label())
            {
                continue;
            }
            if let Some(state) = filter.confirm_state {
                if label.confirm_state != state {
                    continue;
                }
            }
            if filter.only_changed_since_decision && !label.auto_changed_since_decision {
                continue;
            }
            out.push(label);
            if filter.limit > 0 && out.len() as i64 >= filter.limit {
                break;
            }
        }
        Ok(out)
    }

    /// Session ids whose effective label keeps them out of retrieval.
    ///
    /// # Errors
    /// Returns an error on SQL failure or unreadable stored enum values.
    pub fn excluded_sessions(&self) -> Result<Vec<String>> {
        Ok(self
            .list_labels(&LabelFilter::default())?
            .into_iter()
            .filter(|l| l.effective_label().excluded_from_retrieval())
            .map(|l| l.session_id)
            .collect())
    }

    /// Reads the audit trail for one trajectory, oldest first.
    ///
    /// # Errors
    /// Returns an error on SQL failure or a poisoned mutex.
    pub fn events(&self, session_id: &str) -> Result<Vec<LabelEvent>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT event_id, session_id, from_label, to_label, action, reason,
                    decided_by, created_at_ns
             FROM session_label_events WHERE session_id = ?1
             ORDER BY created_at_ns ASC, event_id ASC",
        )?;
        let rows = stmt.query_map(params![session_id], |row| {
            Ok(LabelEvent {
                event_id: row.get(0)?,
                session_id: row.get(1)?,
                from_label: row.get(2)?,
                to_label: row.get(3)?,
                action: row.get(4)?,
                reason: row.get(5)?,
                decided_by: row.get(6)?,
                created_at_ns: row.get(7)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Counts, per rule, how often a person accepted or replaced the verdict it
    /// contributed to.
    ///
    /// A trajectory-level signal: it says at least one of the rules behind the
    /// label was wrong, not which. That is enough to rank rules for
    /// investigation, which is what it is for.
    ///
    /// # Errors
    /// Returns an error on SQL failure or unreadable stored enum values.
    pub fn rule_override_stats(&self) -> Result<Vec<RuleOverrideStat>> {
        let mut tally: std::collections::BTreeMap<String, (usize, usize)> =
            std::collections::BTreeMap::new();
        for label in self.list_labels(&LabelFilter::default())? {
            let bucket = match label.confirm_state {
                ConfirmState::Overridden => 0,
                ConfirmState::Confirmed => 1,
                ConfirmState::Unconfirmed => continue,
            };
            for rule in &label.auto_rules {
                let entry = tally.entry(rule.clone()).or_insert((0, 0));
                if bucket == 0 {
                    entry.0 += 1;
                } else {
                    entry.1 += 1;
                }
            }
        }
        let mut out: Vec<_> = tally
            .into_iter()
            .map(|(rule, (overridden, confirmed))| RuleOverrideStat {
                rule,
                overridden,
                confirmed,
            })
            .collect();
        // Most-overturned first: that is the order someone fixing rules wants.
        out.sort_by(|a, b| b.overridden.cmp(&a.overridden).then(a.rule.cmp(&b.rule)));
        Ok(out)
    }
}

// ─── Schema ──────────────────────────────────────────────────────────────────

/// Brings `conn` to [`SCHEMA_VERSION`], one step at a time.
///
/// Each step runs in its own transaction, so an interrupted upgrade leaves the
/// database at either the previous version or the next, never in between.
fn ensure_schema(conn: &mut Connection) -> Result<()> {
    let current: i64 = conn
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap_or(0);
    if current > SCHEMA_VERSION {
        return Err(ReuseStoreError::SchemaTooNew {
            found: current,
            supported: SCHEMA_VERSION,
        });
    }
    let mut at = current;
    while at < SCHEMA_VERSION {
        let tx = conn.transaction()?;
        match at {
            0 => migrate_0_to_1(&tx)?,
            1 => migrate_1_to_2(&tx)?,
            2 => migrate_2_to_3(&tx)?,
            // v4 created the fragment store, v5 drops it again; both steps
            // funnel here so a v3 database never materialises the table.
            3 | 4 => migrate_4_to_5(&tx)?,
            // Future steps insert here, each bumping `at` by one.
            n => return Err(ReuseStoreError::MissingMigration(n)),
        }
        at += 1;
        tx.pragma_update(None, "user_version", at)?;
        tx.commit()?;
    }
    Ok(())
}

/// Initial schema: the label row plus its append-only audit trail.
fn migrate_0_to_1(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    tx.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS session_labels (
            session_id TEXT PRIMARY KEY,
            auto_label TEXT NOT NULL,
            auto_reason TEXT NOT NULL,
            auto_rules_json TEXT NOT NULL,
            human_label TEXT,
            human_reason TEXT,
            confirm_state TEXT NOT NULL,
            decided_by TEXT,
            decided_at_ns INTEGER,
            auto_changed_since_decision INTEGER NOT NULL DEFAULT 0,
            n_steps INTEGER NOT NULL,
            n_user_turns INTEGER NOT NULL,
            n_tool_calls INTEGER NOT NULL,
            max_agent_len INTEGER NOT NULL,
            n_findings INTEGER NOT NULL,
            source_content_hash TEXT NOT NULL,
            triage_version TEXT NOT NULL,
            created_at_ns INTEGER NOT NULL,
            updated_at_ns INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_session_labels_auto
            ON session_labels(auto_label);
        CREATE INDEX IF NOT EXISTS idx_session_labels_pending
            ON session_labels(confirm_state) WHERE confirm_state = 'unconfirmed';

        CREATE TABLE IF NOT EXISTS session_label_events (
            event_id INTEGER PRIMARY KEY AUTOINCREMENT,
            session_id TEXT NOT NULL,
            from_label TEXT,
            to_label TEXT NOT NULL,
            action TEXT NOT NULL,
            reason TEXT,
            decided_by TEXT NOT NULL,
            created_at_ns INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_label_events_session
            ON session_label_events(session_id, created_at_ns);
        "#,
    )?;
    Ok(())
}

// ─── Row mapping ─────────────────────────────────────────────────────────────

/// Column list shared by every read, so the indices in [`row_to_label`] stay
/// valid.
/// Adds the model judge's verdict alongside the rules' own.
///
/// Separate columns rather than a `source` marker on `auto_label`: both verdicts
/// have to survive, since comparing them is how rule misfires get measured.
fn migrate_1_to_2(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    tx.execute_batch(
        r#"
        ALTER TABLE session_labels ADD COLUMN llm_label TEXT;
        ALTER TABLE session_labels ADD COLUMN llm_reason TEXT;
        ALTER TABLE session_labels ADD COLUMN llm_cited_steps_json TEXT;
        ALTER TABLE session_labels ADD COLUMN llm_downgraded INTEGER NOT NULL DEFAULT 0;
        ALTER TABLE session_labels ADD COLUMN llm_at_ns INTEGER;
        "#,
    )?;
    Ok(())
}

/// Drops the extracted-fragment store. Agents read whole trajectories through
/// `/api/trajectories?label=…`; the fragment table only ever held a lossy
/// projection of that. `IF EXISTS` covers fresh v5 databases that never had it.
fn migrate_4_to_5(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    tx.execute_batch(
        r#"
        DROP TABLE IF EXISTS artifacts;
        DROP INDEX IF EXISTS idx_artifacts_session;
        DROP INDEX IF EXISTS idx_artifacts_role_status;
        "#,
    )?;
    Ok(())
}

/// Adds the identity columns so a reviewer can recognise a trajectory without
/// opening it.
///
/// All nullable: existing rows predate the columns and the next triage pass
/// fills them from the trajectory store. Nothing reads them before then, and an
/// empty title simply renders as the session id.
fn migrate_2_to_3(tx: &rusqlite::Transaction<'_>) -> Result<()> {
    tx.execute_batch(
        r#"
        ALTER TABLE session_labels ADD COLUMN title TEXT;
        ALTER TABLE session_labels ADD COLUMN project TEXT;
        ALTER TABLE session_labels ADD COLUMN source TEXT;
        ALTER TABLE session_labels ADD COLUMN agent_name TEXT;
        ALTER TABLE session_labels ADD COLUMN started_at TEXT;
        ALTER TABLE session_labels ADD COLUMN is_subagent INTEGER;
        "#,
    )?;
    Ok(())
}

const SELECT_COLUMNS: &str = "session_id, auto_label, auto_reason, auto_rules_json,
     human_label, human_reason, confirm_state, decided_by, decided_at_ns,
     auto_changed_since_decision, n_steps, n_user_turns, n_tool_calls,
     max_agent_len, n_findings, source_content_hash, triage_version,
     created_at_ns, updated_at_ns, llm_label, llm_reason, llm_cited_steps_json,
     llm_downgraded, llm_at_ns, title, project, source, agent_name, started_at,
     is_subagent";

fn read_label(conn: &Connection, session_id: &str) -> Result<Option<SessionLabel>> {
    let found = conn
        .query_row(
            &format!("SELECT {SELECT_COLUMNS} FROM session_labels WHERE session_id = ?1"),
            params![session_id],
            row_to_label,
        )
        .optional()?;
    match found {
        Some(inner) => Ok(Some(inner?)),
        None => Ok(None),
    }
}

/// Maps a row, deferring domain-level decoding errors to the caller.
///
/// The nested `Result` exists because `rusqlite` only lets the closure fail with
/// its own error type, and a bad enum token is not a SQL error.
fn row_to_label(row: &Row<'_>) -> rusqlite::Result<Result<SessionLabel>> {
    Ok(build_label(row))
}

fn build_label(row: &Row<'_>) -> Result<SessionLabel> {
    let auto_raw: String = row.get(1)?;
    let rules_json: String = row.get(3)?;
    let human_raw: Option<String> = row.get(4)?;
    let state_raw: String = row.get(6)?;
    Ok(SessionLabel {
        session_id: row.get(0)?,
        auto_label: parse_label(&auto_raw)?,
        auto_reason: row.get(2)?,
        auto_rules: serde_json::from_str(&rules_json)?,
        human_label: human_raw.as_deref().map(parse_label).transpose()?,
        human_reason: row.get(5)?,
        confirm_state: ConfirmState::parse(&state_raw).ok_or_else(|| ReuseStoreError::Corrupt {
            field: "confirm_state",
            value: state_raw.clone(),
        })?,
        decided_by: row.get(7)?,
        decided_at_ns: row.get(8)?,
        auto_changed_since_decision: row.get::<_, i64>(9)? != 0,
        metrics: TriageMetrics {
            n_steps: row.get::<_, i64>(10)?.max(0) as usize,
            n_user_turns: row.get::<_, i64>(11)?.max(0) as usize,
            n_tool_calls: row.get::<_, i64>(12)?.max(0) as usize,
            max_agent_len: row.get::<_, i64>(13)?.max(0) as usize,
        },
        n_findings: row.get::<_, i64>(14)?.max(0) as usize,
        source_content_hash: row.get(15)?,
        triage_version: row.get(16)?,
        created_at_ns: row.get(17)?,
        updated_at_ns: row.get(18)?,
        llm_label: row
            .get::<_, Option<String>>(19)?
            .as_deref()
            .map(parse_label)
            .transpose()?,
        llm_reason: row.get(20)?,
        llm_cited_steps: match row.get::<_, Option<String>>(21)? {
            Some(json) => serde_json::from_str(&json)?,
            None => Vec::new(),
        },
        llm_downgraded: row.get::<_, i64>(22)? != 0,
        llm_at_ns: row.get(23)?,
        identity: TrajectoryIdentity {
            title: row.get(24)?,
            project: row.get::<_, Option<String>>(25)?.unwrap_or_default(),
            source: row.get::<_, Option<String>>(26)?.unwrap_or_default(),
            agent_name: row.get::<_, Option<String>>(27)?.unwrap_or_default(),
            started_at: row.get(28)?,
            is_subagent: row.get::<_, Option<i64>>(29)?.unwrap_or(0) != 0,
        },
    })
}

fn parse_label(raw: &str) -> Result<TrajectoryLabel> {
    TrajectoryLabel::parse(raw).ok_or_else(|| ReuseStoreError::Corrupt {
        field: "trajectory label",
        value: raw.to_string(),
    })
}

fn write_label(conn: &Connection, label: &SessionLabel) -> Result<()> {
    conn.execute(
        "INSERT INTO session_labels (
            session_id, auto_label, auto_reason, auto_rules_json, human_label,
            human_reason, confirm_state, decided_by, decided_at_ns,
            auto_changed_since_decision, n_steps, n_user_turns, n_tool_calls,
            max_agent_len, n_findings, source_content_hash, triage_version,
            created_at_ns, updated_at_ns, llm_label, llm_reason,
            llm_cited_steps_json, llm_downgraded, llm_at_ns, title, project,
            source, agent_name, started_at, is_subagent
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                   ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26,
                   ?27, ?28, ?29, ?30)
         ON CONFLICT(session_id) DO UPDATE SET
            auto_label = ?2, auto_reason = ?3, auto_rules_json = ?4,
            human_label = ?5, human_reason = ?6, confirm_state = ?7,
            decided_by = ?8, decided_at_ns = ?9,
            auto_changed_since_decision = ?10, n_steps = ?11, n_user_turns = ?12,
            n_tool_calls = ?13, max_agent_len = ?14, n_findings = ?15,
            source_content_hash = ?16, triage_version = ?17, updated_at_ns = ?19,
            llm_label = ?20, llm_reason = ?21, llm_cited_steps_json = ?22,
            llm_downgraded = ?23, llm_at_ns = ?24, title = ?25, project = ?26,
            source = ?27, agent_name = ?28, started_at = ?29, is_subagent = ?30",
        params![
            label.session_id,
            label.auto_label.as_str(),
            label.auto_reason,
            serde_json::to_string(&label.auto_rules)?,
            label.human_label.map(|l| l.as_str()),
            label.human_reason,
            label.confirm_state.as_str(),
            label.decided_by,
            label.decided_at_ns,
            label.auto_changed_since_decision as i64,
            label.metrics.n_steps as i64,
            label.metrics.n_user_turns as i64,
            label.metrics.n_tool_calls as i64,
            label.metrics.max_agent_len as i64,
            label.n_findings as i64,
            label.source_content_hash,
            label.triage_version,
            label.created_at_ns,
            label.updated_at_ns,
            label.llm_label.map(|l| l.as_str()),
            label.llm_reason,
            serde_json::to_string(&label.llm_cited_steps)?,
            label.llm_downgraded as i64,
            label.llm_at_ns,
            label.identity.title,
            label.identity.project,
            label.identity.source,
            label.identity.agent_name,
            label.identity.started_at,
            label.identity.is_subagent as i64,
        ],
    )?;
    Ok(())
}

#[allow(clippy::too_many_arguments)] // One append-only audit row; splitting it
// would only move the argument list.
fn append_event(
    conn: &Connection,
    session_id: &str,
    from_label: Option<&str>,
    to_label: &str,
    kind: LabelEventKind,
    reason: Option<&str>,
    decided_by: &str,
    now_ns: i64,
) -> Result<()> {
    conn.execute(
        "INSERT INTO session_label_events (
            session_id, from_label, to_label, action, reason, decided_by, created_at_ns
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            session_id,
            from_label,
            to_label,
            kind.as_str(),
            reason,
            decided_by,
            now_ns
        ],
    )?;
    Ok(())
}

fn fractional_count(count: i64, fraction: f64) -> i64 {
    if count > 0 && fraction > 0.0 {
        ((count as f64 * fraction.clamp(0.0, 1.0)) as i64).max(1)
    } else {
        0
    }
}

/// Wall-clock nanoseconds, saturating rather than panicking past 2262.
fn now_ns() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_nanos()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests;
