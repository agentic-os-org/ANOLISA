//! Cancellable read adapters with V1 correlation tolerance; never create or migrate stores.

use std::path::{Path, PathBuf};

use asc_daemon_core::query::{
    ObservabilityQueries, QueryControl, QueryError, QueryObservation, QueryPage, QueryResult,
    QueryRun, QueryScope, QuerySecurityCounts, QuerySession, QueryWindow, SecurityQueries,
};
use asc_security_events::CorrelationCandidate;
use asc_sqlite_kernel::KernelError;
use rusqlite::{Connection, OptionalExtension, Row, params_from_iter, types::Value as SqlValue};
use serde_json::Value;

use crate::security_events::repository::{CorrelationRequest, SecurityEventRepository};

const ROW_BUDGET: i32 = 4 * 1024 * 1024;
const PAGE_BUDGET: usize = 4 * 1024 * 1024;

/// Read-only observability adapter for the owner-aware system schema.
pub struct SqliteObservabilityQueries {
    path: PathBuf,
}

/// Read-only security adapter shared by obs correlation and future sec query services.
pub struct SqliteSecurityQueries {
    path: PathBuf,
}

impl SqliteObservabilityQueries {
    /// Uses the exact path supplied to the daemon writer, without I/O at construction.
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }
}

impl SqliteSecurityQueries {
    /// Uses the daemon security-event database; no environment or HOME fallback.
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }
}

fn map_error(error: &rusqlite::Error) -> QueryError {
    match error.sqlite_error_code() {
        Some(rusqlite::ErrorCode::OperationInterrupted) => QueryError::DeadlineExceeded,
        Some(rusqlite::ErrorCode::TooBig) => QueryError::ResourceExhausted,
        Some(rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked) => {
            QueryError::Unavailable
        }
        _ => QueryError::Internal,
    }
}

fn open(path: &Path, version: u32, control: &QueryControl) -> Result<Connection, QueryError> {
    control.check()?;
    match std::fs::metadata(path) {
        Ok(metadata) if metadata.is_file() => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(QueryError::Unavailable);
        }
        _ => return Err(QueryError::Internal),
    }
    let conn = asc_sqlite_kernel::open_connection(path, true).map_err(|_| QueryError::Internal)?;
    let cancellation = control.clone();
    conn.progress_handler(1000, Some(move || cancellation.is_cancelled()));
    conn.set_limit(rusqlite::limits::Limit::SQLITE_LIMIT_LENGTH, ROW_BUDGET);
    let found: u32 = conn
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .map_err(|e| map_error(&e))?;
    if found < version {
        return Err(QueryError::Unavailable);
    }
    if found > version {
        return Err(QueryError::Internal);
    }
    conn.execute_batch("BEGIN DEFERRED")
        .map_err(|e| map_error(&e))?;
    control.check()?;
    Ok(conn)
}

struct Filter {
    clauses: Vec<String>,
    values: Vec<SqlValue>,
}

impl Filter {
    fn new(scope: QueryScope, column: &str, window: QueryWindow, time: &str) -> Self {
        let mut filter = Self {
            clauses: vec!["1=1".into()],
            values: Vec::new(),
        };
        match scope {
            QueryScope::All => {}
            QueryScope::Own(uid) => filter.add(column, "=", SqlValue::Integer(i64::from(uid))),
            QueryScope::Unknown => filter.clauses.push(format!("{column} IS NULL")),
        }
        if let Some(value) = window.since {
            filter.add(time, ">=", SqlValue::Real(value));
        }
        if let Some(value) = window.until {
            filter.add(time, "<", SqlValue::Real(value));
        }
        filter
    }

    fn add(&mut self, column: &str, operator: &str, value: SqlValue) {
        self.values.push(value);
        self.clauses
            .push(format!("{column} {operator} ?{}", self.values.len()));
    }

    fn text(&mut self, column: &str, text: &str) {
        self.add(column, "=", SqlValue::Text(text.to_owned()));
    }
    fn sql(&self) -> String {
        format!(" WHERE {}", self.clauses.join(" AND "))
    }
    fn pagination(&mut self, page: QueryPage) -> String {
        self.values.push(SqlValue::Integer(i64::from(page.limit)));
        self.values.push(SqlValue::Integer(page.offset));
        format!(
            " LIMIT ?{} OFFSET ?{}",
            self.values.len() - 1,
            self.values.len()
        )
    }
}

fn count(conn: &Connection, sql: &str, filter: &Filter) -> Result<u64, QueryError> {
    conn.query_row(sql, params_from_iter(&filter.values), |row| row.get(0))
        .map_err(|e| map_error(&e))
}

fn collect<T: serde::Serialize>(
    conn: &Connection,
    sql: &str,
    filter: &Filter,
    decode: impl Fn(&Row<'_>) -> rusqlite::Result<T>,
) -> Result<Vec<T>, QueryError> {
    let mut statement = conn.prepare(sql).map_err(|e| map_error(&e))?;
    let mut rows = statement
        .query(params_from_iter(&filter.values))
        .map_err(|e| map_error(&e))?;
    let mut result = Vec::new();
    let mut size = 0;
    while let Some(row) = rows.next().map_err(|e| map_error(&e))? {
        let item = decode(row).map_err(|e| map_error(&e))?;
        size += serde_json::to_vec(&item)
            .map_err(|_| QueryError::Internal)?
            .len();
        if size > PAGE_BUDGET {
            return Err(QueryError::ResourceExhausted);
        }
        result.push(item);
    }
    Ok(result)
}

impl ObservabilityQueries for SqliteObservabilityQueries {
    fn sessions(
        &self,
        scope: QueryScope,
        session: Option<&str>,
        window: QueryWindow,
        page: QueryPage,
        control: &QueryControl,
    ) -> Result<QueryResult<QuerySession>, QueryError> {
        let conn = open(
            &self.path,
            crate::observability::owned::OWNED_OBSERVABILITY_VERSION,
            control,
        )?;
        let resolved = if scope == QueryScope::All
            && session.is_some_and(|id| qualified_session(id).is_some())
        {
            resolve_root_session(&conn, session.unwrap_or_default(), "observability_events")?
        } else {
            None
        };
        let filtered_scope = resolved.as_ref().map_or(scope, |(scope, _)| *scope);
        let mut filter = Filter::new(filtered_scope, "uid", window, "observed_at_epoch");
        let session = resolved.as_ref().map(|(_, id)| id.as_str()).or(session);
        if let Some(session) = session {
            filter.text("session_id", session);
        }
        let grouped = format!(
            " FROM observability_events AS e{} GROUP BY uid, session_id",
            filter.sql()
        );
        let total = count(
            &conn,
            &format!("SELECT COUNT(*) FROM (SELECT 1{grouped})"),
            &filter,
        )?;
        // Check the full store, not just this page/window, so root labels remain consistent.
        let collision = if scope == QueryScope::All {
            "EXISTS(SELECT 1 FROM observability_events AS other WHERE other.session_id=e.session_id AND other.uid IS NOT e.uid)"
        } else {
            "0"
        };
        let sql = format!(
            "SELECT uid,session_id,MIN(observed_at_epoch),MAX(observed_at_epoch),COUNT(DISTINCT run_id),COUNT(*),{collision}{grouped} ORDER BY MAX(observed_at_epoch) DESC, uid, session_id{}",
            filter.pagination(page)
        );
        let items = collect(&conn, &sql, &filter, |row| {
            let uid: Option<u32> = row.get(0)?;
            let session_id: String = row.get(1)?;
            let qualified_id = row.get::<_, bool>(6)?.then(|| {
                format!(
                    "{}_{}",
                    uid.map_or_else(|| "unknown".into(), |uid| uid.to_string()),
                    session_id
                )
            });
            Ok(QuerySession {
                uid,
                session_id,
                qualified_id,
                first_seen_epoch: row.get(2)?,
                last_seen_epoch: row.get(3)?,
                turn_count: row.get(4)?,
                observability_event_count: row.get(5)?,
            })
        })?;
        control.check()?;
        Ok(QueryResult { items, total })
    }

    fn resolve_session(
        &self,
        scope: QueryScope,
        session: &str,
        control: &QueryControl,
    ) -> Result<Option<(QueryScope, String)>, QueryError> {
        if scope != QueryScope::All {
            return Ok(Some((scope, session.to_owned())));
        }
        let conn = open(
            &self.path,
            crate::observability::owned::OWNED_OBSERVABILITY_VERSION,
            control,
        )?;
        resolve_root_session(&conn, session, "observability_events")
    }

    fn runs(
        &self,
        scope: QueryScope,
        session: &str,
        window: QueryWindow,
        page: QueryPage,
        control: &QueryControl,
    ) -> Result<QueryResult<QueryRun>, QueryError> {
        let conn = open(
            &self.path,
            crate::observability::owned::OWNED_OBSERVABILITY_VERSION,
            control,
        )?;
        let mut filter = Filter::new(scope, "uid", window, "observed_at_epoch");
        filter.text("session_id", session);
        let grouped = format!(
            " FROM observability_events{} GROUP BY uid,session_id,run_id",
            filter.sql()
        );
        let total = count(
            &conn,
            &format!("SELECT COUNT(*) FROM (SELECT 1{grouped})"),
            &filter,
        )?;
        let sql = format!(
            "SELECT uid,run_id,MIN(observed_at_epoch),MAX(observed_at_epoch),COUNT(*){grouped} ORDER BY MIN(observed_at_epoch),run_id{}",
            filter.pagination(page)
        );
        let mut items = collect(&conn, &sql, &filter, |row| {
            Ok(QueryRun {
                uid: row.get(0)?,
                run_id: row.get(1)?,
                started_at_epoch: row.get(2)?,
                ended_at_epoch: row.get(3)?,
                observability_event_count: row.get(4)?,
                user_input_preview: None,
            })
        })?;
        for item in &mut items {
            control.check()?;
            let mut preview = Filter::new(
                QueryScope::for_owner(item.uid),
                "uid",
                window,
                "observed_at_epoch",
            );
            preview.text("session_id", session);
            preview.text("run_id", &item.run_id);
            preview.text("hook", "before_agent_run");
            let sql = format!(
                "SELECT metrics_json FROM observability_events{} ORDER BY observed_at_epoch,id LIMIT 1",
                preview.sql()
            );
            let raw: Option<String> = conn
                .query_row(&sql, params_from_iter(&preview.values), |r| r.get(0))
                .optional()
                .map_err(|e| map_error(&e))?;
            if let Some(raw) = raw {
                let metrics: Value =
                    serde_json::from_str(&raw).map_err(|_| QueryError::Internal)?;
                if !metrics.is_object() {
                    return Err(QueryError::Internal);
                }
                item.user_input_preview =
                    super::observability::repository::extract_user_input_preview(&raw);
            }
        }
        Ok(QueryResult { items, total })
    }

    fn observations(
        &self,
        scope: QueryScope,
        session: &str,
        run: &str,
        window: QueryWindow,
        page: QueryPage,
        control: &QueryControl,
    ) -> Result<QueryResult<QueryObservation>, QueryError> {
        let conn = open(
            &self.path,
            crate::observability::owned::OWNED_OBSERVABILITY_VERSION,
            control,
        )?;
        let mut filter = Filter::new(scope, "uid", window, "observed_at_epoch");
        filter.text("session_id", session);
        filter.text("run_id", run);
        let total = count(
            &conn,
            &format!("SELECT COUNT(*) FROM observability_events{}", filter.sql()),
            &filter,
        )?;
        let sql = format!(
            "SELECT id,uid,hook,observed_at,observed_at_epoch,session_id,run_id,call_id,tool_call_id,metadata_json,metrics_json FROM observability_events{} ORDER BY observed_at_epoch,id{}",
            filter.sql(),
            filter.pagination(page)
        );
        let items = collect(&conn, &sql, &filter, |row| {
            Ok(QueryObservation {
                id: row.get(0)?,
                uid: row.get(1)?,
                hook: row.get(2)?,
                timestamp: row.get(3)?,
                timestamp_epoch: row.get(4)?,
                session_id: row.get(5)?,
                run_id: row.get(6)?,
                call_id: row.get(7)?,
                tool_call_id: row.get(8)?,
                metadata: object(row, 9)?,
                metrics: object(row, 10)?,
            })
        })?;
        control.check()?;
        Ok(QueryResult { items, total })
    }
}

fn object(row: &Row<'_>, index: usize) -> rusqlite::Result<Value> {
    let raw: String = row.get(index)?;
    let value: Value = serde_json::from_str(&raw).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(index, rusqlite::types::Type::Text, Box::new(e))
    })?;
    if !value.is_object() {
        return Err(rusqlite::Error::InvalidQuery);
    }
    Ok(value)
}

impl SecurityQueries for SqliteSecurityQueries {
    fn counts(
        &self,
        scope: QueryScope,
        session: &str,
        run: Option<&str>,
        window: QueryWindow,
        control: &QueryControl,
    ) -> Result<QuerySecurityCounts, QueryError> {
        control.check()?;
        if scope == QueryScope::Unknown {
            return Ok(QuerySecurityCounts::default());
        }
        let conn = open(
            &self.path,
            asc_security_events::SECURITY_EVENTS_SQLITE_SCHEMA_VERSION,
            control,
        )?;
        let mut filter = Filter::new(scope, "uid", window, "timestamp_epoch");
        filter.text("session_id", session);
        if let Some(run) = run {
            filter.text("run_id", run);
        }
        let total = count(
            &conn,
            &format!("SELECT COUNT(*) FROM security_events{}", filter.sql()),
            &filter,
        )?;
        let sql = format!(
            "SELECT category,result,COUNT(*) FROM security_events{} AND category IN ('code_scan','prompt_scan','pii_scan','skill_ledger','sandbox','hardening') GROUP BY category,result",
            filter.sql()
        );
        let groups = collect(&conn, &sql, &filter, |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, u64>(2)?,
            ))
        })?;
        let mut result = QuerySecurityCounts {
            total,
            ..QuerySecurityCounts::default()
        };
        for (category, outcome, count) in groups {
            if outcome != "succeeded" && outcome != "failed" {
                return Err(QueryError::Internal);
            }
            result
                .by_category_result
                .entry(category)
                .or_default()
                .insert(outcome, count);
        }
        Ok(result)
    }

    fn candidates(
        &self,
        scope: QueryScope,
        record: &QueryObservation,
        control: &QueryControl,
    ) -> Result<Vec<CorrelationCandidate>, QueryError> {
        control.check()?;
        let Some(owner) = record.uid else {
            return Ok(Vec::new());
        };
        if scope != QueryScope::Own(owner) {
            return Err(QueryError::PermissionDenied);
        }
        let categories: &[&str] = match record.hook.as_str() {
            "before_tool_call" => &["code_scan", "skill_ledger", "pii_scan"],
            "before_agent_run" => &["prompt_scan", "pii_scan"],
            "after_tool_call" => &["pii_scan"],
            _ => return Ok(Vec::new()),
        };
        let categories: Vec<String> = categories
            .iter()
            .map(|category| (*category).into())
            .collect();
        let result = (|| {
            let conn = open(
                &self.path,
                asc_security_events::SECURITY_EVENTS_SQLITE_SCHEMA_VERSION,
                control,
            )?;
            let real_run = !record.run_id.trim().is_empty()
                && record.run_id != asc_daemon_core::query::ZERO_RUN_ID;
            let exact = real_run
                && record
                    .tool_call_id
                    .as_deref()
                    .is_some_and(|s| !s.trim().is_empty());
            let tool_call_ids = record
                .tool_call_id
                .as_ref()
                .filter(|_| exact)
                .map(|tool| vec![tool.clone()]);
            let timed = !(exact || real_run && record.hook == "before_agent_run");
            let request = CorrelationRequest {
                session_id: &record.session_id,
                categories: &categories,
                run_id: real_run.then_some(record.run_id.as_str()),
                tool_call_ids: tool_call_ids.as_deref(),
                since_epoch: timed.then_some(record.timestamp_epoch - 10.0),
                until_epoch: timed.then_some(record.timestamp_epoch + 10.0),
            };
            let candidates = SecurityEventRepository
                .query_correlation_candidates(
                    &conn,
                    &request,
                    &asc_security_events::query::QueryScope::Owner(owner),
                )
                .map_err(|error| match error {
                    KernelError::Sqlite(error) => map_error(&error),
                    _ => QueryError::Internal,
                })?;
            control.check()?;
            Ok(candidates)
        })();
        match result {
            Err(QueryError::Unavailable | QueryError::Internal | QueryError::ResourceExhausted) => {
                tracing::warn!(target: "asc_process_diagnostic",
                    "security correlation candidate query failed; showing observations without correlations");
                control.check()?;
                Ok(Vec::new())
            }
            result => result,
        }
    }
}

// A qualified session is a root resource locator, never a caller identity.
fn qualified_session(session: &str) -> Option<(Option<u32>, &str)> {
    let (owner, raw) = session.split_once('_')?;
    let uid = if owner == "unknown" {
        None
    } else {
        let uid: u32 = owner.parse().ok()?;
        if owner != uid.to_string() {
            return None;
        }
        Some(uid)
    };
    Some((uid, raw))
}

fn session_uids(
    conn: &Connection,
    session: &str,
    table: &str,
) -> Result<Vec<Option<u32>>, QueryError> {
    collect(
        conn,
        &format!("SELECT DISTINCT uid FROM {table} WHERE session_id=?1 LIMIT 2"),
        &Filter {
            clauses: Vec::new(),
            values: vec![SqlValue::Text(session.into())],
        },
        |row| row.get(0),
    )
}

// Table names are trusted store constants, never request values.
pub(crate) fn resolve_root_session(
    conn: &Connection,
    session: &str,
    table: &str,
) -> Result<Option<(QueryScope, String)>, QueryError> {
    let uids = session_uids(conn, session, table)?;
    let qualified = if let Some((uid, raw)) = qualified_session(session) {
        let shared = session_uids(conn, raw, table)?.len() > 1;
        let exists: bool = conn
            .query_row(
                &format!("SELECT EXISTS(SELECT 1 FROM {table} WHERE session_id=?1 AND uid IS ?2)"),
                rusqlite::params![raw, uid],
                |row| row.get(0),
            )
            .map_err(|e| map_error(&e))?;
        (shared && exists).then(|| (QueryScope::for_owner(uid), raw.to_owned()))
    } else {
        None
    };
    match (uids.as_slice(), qualified) {
        ([], selected) => Ok(selected),
        ([owner], None) => Ok(Some((QueryScope::for_owner(*owner), session.to_owned()))),
        _ => Err(QueryError::InvalidArgument),
    }
}
