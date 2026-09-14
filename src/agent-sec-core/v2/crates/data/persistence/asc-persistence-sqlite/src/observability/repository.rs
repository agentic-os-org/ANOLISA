//! Typed reads and writes for the `observability_events` table.
//!
//! Migrated from v1 `observability/repositories.py`. The `list_runs` window
//! function is kept as-is rather than rewritten to `GROUP BY`: picking the
//! *first* `before_agent_run` row per run needs `ROW_NUMBER()`, and that is the
//! direct reason this crate links a bundled `SQLite` instead of relying on the
//! host library.

use std::collections::HashMap;

use asc_observability::{
    ObservabilityRecord, RunSummary, SessionSummary, USER_INPUT_PREVIEW_LIMIT,
};
use asc_sqlite_kernel::{KernelError, RecordRepository, TableSpec};
use rusqlite::types::Value as SqlValue;
use rusqlite::{Connection, Row};
use serde_json::Value;

use crate::observability::table::OBSERVABILITY_TABLES;

/// One stored observability row.
///
/// The JSON blobs stay as strings: v1 hands them to the review UI unparsed, and
/// re-encoding them here would risk changing the bytes callers already compare.
#[derive(Debug, Clone, PartialEq)]
pub struct ObservabilityEventRow {
    /// Autoincrementing row id, used to break `observed_at_epoch` ties.
    pub id: i64,
    /// Hook name as stored.
    pub hook: String,
    /// Wire-format observation timestamp.
    pub observed_at: String,
    /// Observation timestamp as epoch seconds.
    pub observed_at_epoch: f64,
    /// Session correlation.
    pub session_id: String,
    /// Run correlation.
    pub run_id: String,
    /// Serialized metrics object.
    pub metrics_json: String,
    /// Serialized metadata object.
    pub metadata_json: String,
    /// Optional LLM call correlation.
    pub call_id: Option<String>,
    /// Optional tool call correlation.
    pub tool_call_id: Option<String>,
}

/// An optional epoch window; the lower bound is inclusive, the upper exclusive.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct EpochWindow {
    /// Inclusive lower bound.
    pub start_epoch: Option<f64>,
    /// Exclusive upper bound.
    pub end_epoch: Option<f64>,
}

impl EpochWindow {
    /// Appends the window's clauses to `clauses` and its values to `params`.
    fn apply(self, clauses: &mut Vec<String>, params: &mut Vec<SqlValue>) {
        if let Some(start) = self.start_epoch {
            params.push(SqlValue::Real(start));
            clauses.push(format!("observed_at_epoch >= ?{}", params.len()));
        }
        if let Some(end) = self.end_epoch {
            params.push(SqlValue::Real(end));
            clauses.push(format!("observed_at_epoch < ?{}", params.len()));
        }
    }
}

/// Optional `LIMIT` / `OFFSET`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Page {
    /// Maximum rows to return; `None` means unbounded.
    pub limit: Option<u32>,
    /// Rows to skip.
    pub offset: u32,
}

impl Page {
    /// Renders the clause, using `LIMIT -1` when only an offset is set.
    fn render(self) -> String {
        match (self.limit, self.offset) {
            (None, 0) => String::new(),
            (None, offset) => format!(" LIMIT -1 OFFSET {offset}"),
            (Some(limit), 0) => format!(" LIMIT {limit}"),
            (Some(limit), offset) => format!(" LIMIT {limit} OFFSET {offset}"),
        }
    }
}

/// The columns [`ObservabilityEventRow`] reads, in table order.
const SELECT_COLUMNS: &str = "id, hook, observed_at, observed_at_epoch, session_id, run_id, \
                              metrics_json, metadata_json, call_id, tool_call_id";

/// Reads and writes observability records.
#[derive(Debug, Default, Clone, Copy)]
pub struct ObservabilityEventRepository;

impl RecordRepository for ObservabilityEventRepository {
    type Record = ObservabilityRecord;

    fn tables(&self) -> &'static [TableSpec] {
        OBSERVABILITY_TABLES
    }

    /// Inserts one record.
    ///
    /// A record that cannot be serialized reports [`KernelError::Malformed`]
    /// rather than `Ok(false)`. That is the mirror image of the security-event
    /// repository and is what lets the observability policy propagate a caller
    /// bug without disposing the connection.
    fn insert_or_raise(
        &self,
        conn: &Connection,
        record: &ObservabilityRecord,
    ) -> Result<bool, KernelError> {
        let metrics_json = record
            .metrics()
            .to_json_string()
            .map_err(|err| KernelError::Malformed(format!("metrics must be an object: {err}")))?;
        let metadata_json = record
            .metadata()
            .to_json_string()
            .map_err(|err| KernelError::Malformed(format!("metadata must be an object: {err}")))?;
        let metadata = record.metadata();

        conn.execute(
            "INSERT INTO observability_events (hook, observed_at, observed_at_epoch, session_id, \
             run_id, metrics_json, metadata_json, call_id, tool_call_id) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            rusqlite::params![
                record.hook().as_str(),
                record.observed_at_iso(),
                record.observed_at_epoch(),
                metadata.session_id,
                metadata.run_id,
                metrics_json,
                metadata_json,
                metadata.call_id,
                metadata.tool_call_id,
            ],
        )?;
        Ok(true)
    }

    /// Deletes rows older than `max_age_days` relative to `now`.
    fn prune(&self, conn: &Connection, max_age_days: u32, now: f64) -> Result<usize, KernelError> {
        let cutoff = now - f64::from(max_age_days) * 86_400.0;
        Ok(conn.execute(
            "DELETE FROM observability_events WHERE observed_at_epoch < ?1",
            [cutoff],
        )?)
    }
}

impl ObservabilityEventRepository {
    /// Returns the number of indexed records.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Sqlite`] when the query cannot run.
    pub fn count(&self, conn: &Connection) -> Result<u64, KernelError> {
        let count: i64 =
            conn.query_row("SELECT COUNT(*) FROM observability_events", [], |row| {
                row.get(0)
            })?;
        Ok(count.try_into().unwrap_or_default())
    }

    /// Returns the number of distinct sessions in `window`.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Sqlite`] when the query cannot run.
    pub fn count_sessions(
        &self,
        conn: &Connection,
        window: EpochWindow,
    ) -> Result<u64, KernelError> {
        let mut clauses = Vec::new();
        let mut params = Vec::new();
        window.apply(&mut clauses, &mut params);
        let sql = format!(
            "SELECT COUNT(DISTINCT session_id) FROM observability_events{}",
            where_clause(&clauses)
        );
        let count: i64 =
            conn.query_row(&sql, rusqlite::params_from_iter(params.iter()), |row| {
                row.get(0)
            })?;
        Ok(count.try_into().unwrap_or_default())
    }

    /// Returns the number of distinct runs of `session_id` in `window`.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Sqlite`] when the query cannot run.
    pub fn count_runs(
        &self,
        conn: &Connection,
        session_id: &str,
        window: EpochWindow,
    ) -> Result<u64, KernelError> {
        let mut params = vec![SqlValue::Text(session_id.to_owned())];
        let mut clauses = vec!["session_id = ?1".to_owned()];
        window.apply(&mut clauses, &mut params);
        let sql = format!(
            "SELECT COUNT(DISTINCT run_id) FROM observability_events{}",
            where_clause(&clauses)
        );
        let count: i64 =
            conn.query_row(&sql, rusqlite::params_from_iter(params.iter()), |row| {
                row.get(0)
            })?;
        Ok(count.try_into().unwrap_or_default())
    }

    /// Returns sessions ordered by most recent activity first.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Sqlite`] when the query cannot run.
    pub fn list_sessions(
        &self,
        conn: &Connection,
        window: EpochWindow,
        page: Page,
    ) -> Result<Vec<SessionSummary>, KernelError> {
        let mut clauses = Vec::new();
        let mut params = Vec::new();
        window.apply(&mut clauses, &mut params);
        let sql = format!(
            "SELECT session_id, MIN(observed_at_epoch) AS first_seen, \
             MAX(observed_at_epoch) AS last_seen, COUNT(DISTINCT run_id) AS turn_count, \
             COUNT(*) AS event_count FROM observability_events{} \
             GROUP BY session_id ORDER BY MAX(observed_at_epoch) DESC{}",
            where_clause(&clauses),
            page.render()
        );

        let mut statement = conn.prepare(&sql)?;
        let mut rows = statement.query(rusqlite::params_from_iter(params.iter()))?;
        let mut sessions = Vec::new();
        while let Some(row) = rows.next()? {
            sessions.push(SessionSummary {
                session_id: row.get(0)?,
                first_seen_epoch: row.get(1)?,
                last_seen_epoch: row.get(2)?,
                turn_count: count_of(row, 3)?,
                event_count: count_of(row, 4)?,
            });
        }
        Ok(sessions)
    }

    /// Returns the runs of `session_id` in chronological order.
    ///
    /// Two statements, constant regardless of run count: one `GROUP BY` for the
    /// stats and one window query for each run's first `before_agent_run`
    /// metrics blob, from which the user-input preview is taken.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Sqlite`] when either query cannot run.
    pub fn list_runs(
        &self,
        conn: &Connection,
        session_id: &str,
        window: EpochWindow,
        page: Page,
    ) -> Result<Vec<RunSummary>, KernelError> {
        let mut params = vec![SqlValue::Text(session_id.to_owned())];
        let mut clauses = vec!["session_id = ?1".to_owned()];
        window.apply(&mut clauses, &mut params);
        let filter = where_clause(&clauses);

        let stats_sql = format!(
            "SELECT run_id, MIN(observed_at_epoch) AS started_at, \
             MAX(observed_at_epoch) AS ended_at, COUNT(*) AS event_count \
             FROM observability_events{filter} \
             GROUP BY run_id ORDER BY MIN(observed_at_epoch) ASC{}",
            page.render()
        );
        let preview_sql = format!(
            "SELECT run_id, metrics_json FROM (\
             SELECT run_id, metrics_json, ROW_NUMBER() OVER (\
             PARTITION BY run_id ORDER BY observed_at_epoch ASC, id ASC) AS rn \
             FROM observability_events{filter} AND hook = 'before_agent_run') WHERE rn = 1"
        );

        let mut previews: HashMap<String, Option<String>> = HashMap::new();
        {
            let mut statement = conn.prepare(&preview_sql)?;
            let mut rows = statement.query(rusqlite::params_from_iter(params.iter()))?;
            while let Some(row) = rows.next()? {
                previews.insert(row.get(0)?, row.get(1)?);
            }
        }

        let mut statement = conn.prepare(&stats_sql)?;
        let mut rows = statement.query(rusqlite::params_from_iter(params.iter()))?;
        let mut runs = Vec::new();
        while let Some(row) = rows.next()? {
            let run_id: String = row.get(0)?;
            let preview = previews
                .get(&run_id)
                .and_then(|metrics| metrics.as_deref())
                .and_then(extract_user_input_preview);
            runs.push(RunSummary {
                run_id,
                started_at_epoch: row.get(1)?,
                ended_at_epoch: row.get(2)?,
                user_input_preview: preview,
                event_count: count_of(row, 3)?,
            });
        }
        Ok(runs)
    }

    /// Returns the rows of one run, oldest first.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Sqlite`] when the query cannot run.
    pub fn list_events(
        &self,
        conn: &Connection,
        session_id: &str,
        run_id: &str,
        window: EpochWindow,
        page: Page,
    ) -> Result<Vec<ObservabilityEventRow>, KernelError> {
        let mut params = vec![
            SqlValue::Text(session_id.to_owned()),
            SqlValue::Text(run_id.to_owned()),
        ];
        let mut clauses = vec!["session_id = ?1".to_owned(), "run_id = ?2".to_owned()];
        window.apply(&mut clauses, &mut params);
        let sql = format!(
            "SELECT {SELECT_COLUMNS} FROM observability_events{} \
             ORDER BY observed_at_epoch ASC{}",
            where_clause(&clauses),
            page.render()
        );

        let mut statement = conn.prepare(&sql)?;
        let mut rows = statement.query(rusqlite::params_from_iter(params.iter()))?;
        let mut events = Vec::new();
        while let Some(row) = rows.next()? {
            events.push(ObservabilityEventRow {
                id: row.get(0)?,
                hook: row.get(1)?,
                observed_at: row.get(2)?,
                observed_at_epoch: row.get(3)?,
                session_id: row.get(4)?,
                run_id: row.get(5)?,
                metrics_json: row.get(6)?,
                metadata_json: row.get(7)?,
                call_id: row.get(8)?,
                tool_call_id: row.get(9)?,
            });
        }
        Ok(events)
    }
}

fn where_clause(clauses: &[String]) -> String {
    if clauses.is_empty() {
        String::new()
    } else {
        format!(" WHERE {}", clauses.join(" AND "))
    }
}

fn count_of(row: &Row<'_>, index: usize) -> Result<u64, KernelError> {
    let value: i64 = row.get(index)?;
    Ok(value.try_into().unwrap_or_default())
}

/// Extracts a short user-input preview from a `before_agent_run` metrics blob.
///
/// Falls back `user_input` → `prompt` → `None`, and truncates to
/// [`USER_INPUT_PREVIEW_LIMIT`]. Unparseable JSON yields `None` so the UI can
/// render a placeholder instead of failing the whole list.
fn extract_user_input_preview(metrics_json: &str) -> Option<String> {
    let Ok(Value::Object(metrics)) = serde_json::from_str::<Value>(metrics_json) else {
        return None;
    };
    let candidate = ["user_input", "prompt"]
        .into_iter()
        .filter_map(|key| metrics.get(key))
        .find_map(preview_text)?;
    // Truncate on a character boundary; a byte slice could split a multi-byte
    // character and produce invalid UTF-8.
    Some(candidate.chars().take(USER_INPUT_PREVIEW_LIMIT).collect())
}

/// Renders a metrics value the way v1's truthiness check does.
///
/// v1 uses `metrics.get("user_input") or metrics.get("prompt")`, so falsy values
/// — empty string, `0`, `false`, `null` — fall through to the next candidate.
fn preview_text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) if !text.is_empty() => Some(text.clone()),
        Value::Number(number) if number.as_f64() != Some(0.0) => Some(number.to_string()),
        Value::Bool(true) => Some("True".to_owned()),
        Value::Array(items) if !items.is_empty() => Some(value.to_string()),
        Value::Object(map) if !map.is_empty() => Some(value.to_string()),
        _ => None,
    }
}
