//! Typed reads and writes for the `security_events` table.
//!
//! Migrated from v1 `security_events/repositories.py`. Two v1 properties are
//! preserved deliberately:
//!
//! * **Every filter, including `verdict`, is applied in SQL.** v1 stopped
//!   post-filtering in Python precisely so `LIMIT`/`OFFSET` stay correct.
//! * **A malformed stored row is skipped, not fatal.** One unparseable `details`
//!   blob must not blank out a whole query.
//!
//! Unlike v1 the repository does not own the store: the kernel owns connection
//! lifecycle, so every method takes a `&Connection`.

use std::fmt::Write as _;

use asc_security_events::timestamp::utc_iso_to_epoch;
use asc_security_events::{
    CorrelationCandidate, EventResult, SecurityEvent, SecurityEventsSummary, TimestampError,
    extract_verdict,
};
use asc_sqlite_kernel::{KernelError, RecordRepository, TableSpec};
use rusqlite::types::Value as SqlValue;
use rusqlite::{Connection, Row};
use serde_json::Value;

use crate::security_events::table::SECURITY_EVENTS_TABLES;

/// Hard cap on rows returned to observability correlation (v1
/// `_CORRELATION_CANDIDATE_LIMIT`).
pub const CORRELATION_CANDIDATE_LIMIT: u32 = 1000;

/// Group fields accepted by [`SecurityEventRepository::count_by`].
///
/// Listed in v1's `_COUNT_BY_COLUMNS` order.
pub const VALID_GROUP_FIELDS: &[&str] = &[
    "category",
    "event_type",
    "result",
    "trace_id",
    "session_id",
    "run_id",
    "call_id",
    "tool_call_id",
    "verdict",
];

/// Group fields aggregated by [`SecurityEventRepository::summary`] in one query.
const SUMMARY_GROUP_FIELDS: &[&str] = &["category", "event_type", "result", "session_id", "run_id"];

/// The columns every row read selects, in table order.
const SELECT_COLUMNS: &str = "event_id, event_type, category, result, timestamp, timestamp_epoch, \
                              trace_id, pid, uid, session_id, run_id, call_id, tool_call_id, details";

/// Optional query filters, all combined with `AND`.
///
/// Time bounds are stored as epochs because that is what the indexed column
/// holds; use [`EventFilters::since`] / [`EventFilters::until`] to supply the ISO
/// strings the v1 API takes.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct EventFilters {
    /// Exact `event_type`.
    pub event_type: Option<String>,
    /// Exact `category`.
    pub category: Option<String>,
    /// Exact `result`.
    pub result: Option<String>,
    /// Exact `trace_id`.
    pub trace_id: Option<String>,
    /// Exact `session_id`.
    pub session_id: Option<String>,
    /// Exact `run_id`.
    pub run_id: Option<String>,
    /// Exact `call_id`.
    pub call_id: Option<String>,
    /// Exact `tool_call_id`.
    pub tool_call_id: Option<String>,
    /// Exact `verdict`.
    pub verdict: Option<String>,
    /// Inclusive lower bound on `timestamp_epoch`.
    pub since_epoch: Option<f64>,
    /// Exclusive upper bound on `timestamp_epoch`.
    pub until_epoch: Option<f64>,
}

impl EventFilters {
    /// Sets the inclusive lower bound from a UTC ISO timestamp.
    ///
    /// # Errors
    ///
    /// Returns [`TimestampError`] when `value` is not ISO-8601, matching v1,
    /// where `utc_iso_to_epoch` raises out of `query()`.
    pub fn since(mut self, value: &str) -> Result<Self, TimestampError> {
        self.since_epoch = Some(utc_iso_to_epoch(value, "since")?);
        Ok(self)
    }

    /// Sets the exclusive upper bound from a UTC ISO timestamp.
    ///
    /// # Errors
    ///
    /// Returns [`TimestampError`] when `value` is not ISO-8601.
    pub fn until(mut self, value: &str) -> Result<Self, TimestampError> {
        self.until_epoch = Some(utc_iso_to_epoch(value, "until")?);
        Ok(self)
    }

    /// Renders the `WHERE` clause and its bound parameters.
    ///
    /// Returns an empty string when nothing is filtered, so callers can splice
    /// the result unconditionally.
    fn build(&self) -> (String, Vec<SqlValue>) {
        let mut clauses: Vec<String> = Vec::new();
        let mut params: Vec<SqlValue> = Vec::new();

        let equality = [
            ("event_type", self.event_type.as_ref()),
            ("category", self.category.as_ref()),
            ("result", self.result.as_ref()),
            ("trace_id", self.trace_id.as_ref()),
            ("session_id", self.session_id.as_ref()),
            ("run_id", self.run_id.as_ref()),
            ("call_id", self.call_id.as_ref()),
            ("tool_call_id", self.tool_call_id.as_ref()),
            ("verdict", self.verdict.as_ref()),
        ];
        for (column, value) in equality {
            if let Some(value) = value {
                params.push(SqlValue::Text(value.clone()));
                clauses.push(format!("{column} = ?{}", params.len()));
            }
        }
        if let Some(since) = self.since_epoch {
            params.push(SqlValue::Real(since));
            clauses.push(format!("timestamp_epoch >= ?{}", params.len()));
        }
        if let Some(until) = self.until_epoch {
            params.push(SqlValue::Real(until));
            clauses.push(format!("timestamp_epoch < ?{}", params.len()));
        }

        if clauses.is_empty() {
            (String::new(), params)
        } else {
            (format!(" WHERE {}", clauses.join(" AND ")), params)
        }
    }
}

/// Counts grouped by one column; `None` is the SQL `NULL` bucket.
pub type GroupCounts = Vec<(Option<String>, u64)>;

/// The filters of one correlation-candidate query.
///
/// Grouped into a struct because v1 takes seven keyword arguments here, and a
/// seven-parameter Rust function is both unreadable and easy to mis-call.
#[derive(Debug, Clone, Copy, Default)]
pub struct CorrelationRequest<'a> {
    /// Required session correlation.
    pub session_id: &'a str,
    /// Categories to include; an empty slice yields no candidates.
    pub categories: &'a [String],
    /// Optional run correlation.
    pub run_id: Option<&'a str>,
    /// Optional tool-call allowlist; an all-empty list yields no candidates.
    pub tool_call_ids: Option<&'a [String]>,
    /// Inclusive lower bound on `timestamp_epoch`.
    pub since_epoch: Option<f64>,
    /// Inclusive upper bound on `timestamp_epoch`.
    pub until_epoch: Option<f64>,
}

/// Reads and writes security events.
#[derive(Debug, Default, Clone, Copy)]
pub struct SecurityEventRepository;

impl RecordRepository for SecurityEventRepository {
    type Record = SecurityEvent;

    fn tables(&self) -> &'static [TableSpec] {
        SECURITY_EVENTS_TABLES
    }

    /// Inserts one event, ignoring a duplicate `event_id`.
    ///
    /// A record the writer cannot serialize reports `Ok(false)` rather than an
    /// error. That is v1's asymmetry: `SecurityEventRepository.insert` catches
    /// `ValueError` / `TypeError` itself, so a malformed event looks exactly like
    /// a skipped write and never reaches the corruption ladder.
    fn insert_or_raise(
        &self,
        conn: &Connection,
        record: &SecurityEvent,
    ) -> Result<bool, KernelError> {
        let Some(values) = event_values(record) else {
            return Ok(false);
        };
        // v1 reports success for a conflict too: the row already exists, so the
        // write is not a drop. Only an unserializable event returns false.
        conn.execute(
            "INSERT INTO security_events (event_id, event_type, category, result, timestamp, \
             timestamp_epoch, trace_id, pid, uid, session_id, run_id, call_id, tool_call_id, \
             verdict, details) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15) \
             ON CONFLICT(event_id) DO NOTHING",
            rusqlite::params_from_iter(values.iter()),
        )?;
        Ok(true)
    }

    /// Deletes rows older than `max_age_days` relative to `now`.
    fn prune(&self, conn: &Connection, max_age_days: u32, now: f64) -> Result<usize, KernelError> {
        let cutoff = now - f64::from(max_age_days) * 86_400.0;
        Ok(conn.execute(
            "DELETE FROM security_events WHERE timestamp_epoch < ?1",
            [cutoff],
        )?)
    }
}

impl SecurityEventRepository {
    /// Returns matching events, newest first.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Sqlite`] when the query cannot run.
    pub fn query(
        &self,
        conn: &Connection,
        filters: &EventFilters,
        limit: u32,
        offset: u32,
    ) -> Result<Vec<SecurityEvent>, KernelError> {
        let (where_clause, mut params) = filters.build();
        params.push(SqlValue::Integer(i64::from(limit)));
        params.push(SqlValue::Integer(i64::from(offset)));
        let sql = format!(
            "SELECT {SELECT_COLUMNS} FROM security_events{where_clause} \
             ORDER BY timestamp_epoch DESC LIMIT ?{} OFFSET ?{}",
            params.len() - 1,
            params.len()
        );
        collect_events(conn, &sql, &params)
    }

    /// Returns one event by id.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Sqlite`] when the query cannot run.
    pub fn get(
        &self,
        conn: &Connection,
        event_id: &str,
    ) -> Result<Option<SecurityEvent>, KernelError> {
        let sql = format!("SELECT {SELECT_COLUMNS} FROM security_events WHERE event_id = ?1");
        let events = collect_events(conn, &sql, &[SqlValue::Text(event_id.to_owned())])?;
        Ok(events.into_iter().next())
    }

    /// Returns up to [`CORRELATION_CANDIDATE_LIMIT`] rows for correlation.
    ///
    /// Ordered ascending by `(timestamp_epoch, event_id)` so the caller sees a
    /// stable chronological sequence. An empty `categories` or an all-empty
    /// `tool_call_ids` short-circuits to no candidates, as in v1.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Sqlite`] when the query cannot run.
    pub fn query_correlation_candidates(
        &self,
        conn: &Connection,
        request: &CorrelationRequest<'_>,
    ) -> Result<Vec<CorrelationCandidate>, KernelError> {
        if request.categories.is_empty() {
            return Ok(Vec::new());
        }

        let mut params: Vec<SqlValue> = vec![SqlValue::Text(request.session_id.to_owned())];
        let mut clauses = vec!["session_id = ?1".to_owned()];

        let placeholders = push_in_list(&mut params, request.categories);
        clauses.push(format!("category IN ({placeholders})"));

        if let Some(run_id) = request.run_id {
            params.push(SqlValue::Text(run_id.to_owned()));
            clauses.push(format!("run_id = ?{}", params.len()));
        }
        if let Some(ids) = request.tool_call_ids {
            let non_empty: Vec<String> = ids.iter().filter(|id| !id.is_empty()).cloned().collect();
            if non_empty.is_empty() {
                return Ok(Vec::new());
            }
            let placeholders = push_in_list(&mut params, &non_empty);
            clauses.push(format!("tool_call_id IN ({placeholders})"));
        }
        if let Some(since) = request.since_epoch {
            params.push(SqlValue::Real(since));
            clauses.push(format!("timestamp_epoch >= ?{}", params.len()));
        }
        if let Some(until) = request.until_epoch {
            params.push(SqlValue::Real(until));
            clauses.push(format!("timestamp_epoch <= ?{}", params.len()));
        }

        let sql = format!(
            "SELECT {SELECT_COLUMNS} FROM security_events WHERE {} \
             ORDER BY timestamp_epoch ASC, event_id ASC LIMIT {CORRELATION_CANDIDATE_LIMIT}",
            clauses.join(" AND ")
        );

        let mut statement = conn.prepare(&sql)?;
        let mut rows = statement.query(rusqlite::params_from_iter(params.iter()))?;
        let mut candidates = Vec::new();
        while let Some(row) = rows.next()? {
            let epoch: f64 = row.get("timestamp_epoch")?;
            if let Some(event) = row_to_event(row)? {
                candidates.push(CorrelationCandidate {
                    event,
                    timestamp_epoch: epoch,
                });
            }
        }
        Ok(candidates)
    }

    /// Counts matching events.
    ///
    /// A non-zero `offset` counts what remains *after* skipping that many rows,
    /// which is how v1 keeps pagination counters honest.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Sqlite`] when the query cannot run.
    pub fn count(
        &self,
        conn: &Connection,
        filters: &EventFilters,
        offset: u32,
    ) -> Result<u64, KernelError> {
        let (where_clause, mut params) = filters.build();
        let sql = if offset > 0 {
            params.push(SqlValue::Integer(i64::from(offset)));
            format!(
                "SELECT COUNT(*) FROM (SELECT event_id FROM security_events{where_clause} \
                 ORDER BY timestamp_epoch DESC LIMIT -1 OFFSET ?{})",
                params.len()
            )
        } else {
            format!("SELECT COUNT(*) FROM security_events{where_clause}")
        };
        let count: i64 =
            conn.query_row(&sql, rusqlite::params_from_iter(params.iter()), |row| {
                row.get(0)
            })?;
        Ok(count.try_into().unwrap_or_default())
    }

    /// Counts matching events grouped by one allowlisted field.
    ///
    /// When grouping by `verdict` without also filtering on it, NULL and empty
    /// verdicts are excluded — otherwise the dashboard would show a bucket for
    /// "no verdict recorded".
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Malformed`] for a field outside
    /// [`VALID_GROUP_FIELDS`], and [`KernelError::Sqlite`] when the query cannot
    /// run.
    pub fn count_by(
        &self,
        conn: &Connection,
        group_field: &str,
        filters: &EventFilters,
        offset: u32,
    ) -> Result<GroupCounts, KernelError> {
        let column = validate_group_field(group_field)?;

        let (mut where_clause, mut params) = filters.build();
        if column == "verdict" && filters.verdict.is_none() {
            let extra = "verdict IS NOT NULL AND verdict != ''";
            where_clause = if where_clause.is_empty() {
                format!(" WHERE {extra}")
            } else {
                format!("{where_clause} AND {extra}")
            };
        }

        let sql = if offset > 0 {
            params.push(SqlValue::Integer(i64::from(offset)));
            format!(
                "SELECT group_value, COUNT(*) FROM \
                 (SELECT {column} AS group_value FROM security_events{where_clause} \
                 ORDER BY timestamp_epoch DESC LIMIT -1 OFFSET ?{}) \
                 GROUP BY group_value",
                params.len()
            )
        } else {
            format!(
                "SELECT {column}, COUNT(*) FROM security_events{where_clause} GROUP BY {column}"
            )
        };

        let mut statement = conn.prepare(&sql)?;
        let mut rows = statement.query(rusqlite::params_from_iter(params.iter()))?;
        let mut counts = Vec::new();
        while let Some(row) = rows.next()? {
            let count: i64 = row.get(1)?;
            counts.push((row.get(0)?, count.try_into().unwrap_or_default()));
        }
        Ok(counts)
    }

    /// Returns the dashboard aggregates and the newest rows.
    ///
    /// The five group counts come back in one `UNION ALL` query, as in v1, so a
    /// dashboard render costs two statements rather than six.
    ///
    /// # Errors
    ///
    /// Returns [`KernelError::Sqlite`] when either query cannot run.
    pub fn summary(
        &self,
        conn: &Connection,
        filters: &EventFilters,
        latest_limit: u32,
    ) -> Result<SecurityEventsSummary, KernelError> {
        let (where_clause, params) = filters.build();
        let branches: Vec<String> = SUMMARY_GROUP_FIELDS
            .iter()
            .map(|field| {
                format!(
                    "SELECT '{field}' AS group_field, {field} AS group_value, COUNT(*) AS count \
                     FROM security_events{where_clause} GROUP BY {field}"
                )
            })
            .collect();

        // Every branch repeats the same filters, so the bound parameters repeat too.
        let mut group_params: Vec<SqlValue> = Vec::new();
        for _ in 0..branches.len() {
            group_params.extend(params.iter().cloned());
        }
        let group_sql = renumber_placeholders(&branches.join(" UNION ALL "), params.len());

        let mut groups: Vec<(&str, GroupCounts)> = SUMMARY_GROUP_FIELDS
            .iter()
            .map(|field| (*field, Vec::new()))
            .collect();

        let mut statement = conn.prepare(&group_sql)?;
        let mut rows = statement.query(rusqlite::params_from_iter(group_params.iter()))?;
        while let Some(row) = rows.next()? {
            let field: String = row.get(0)?;
            let count: i64 = row.get(2)?;
            if let Some(bucket) = groups.iter_mut().find(|(name, _)| *name == field.as_str()) {
                bucket
                    .1
                    .push((row.get(1)?, count.try_into().unwrap_or_default()));
            }
        }
        drop(rows);
        drop(statement);

        let latest = self.query(conn, filters, latest_limit, 0)?;
        let take = |name: &str| -> GroupCounts {
            groups
                .iter()
                .find(|(field, _)| *field == name)
                .map(|(_, counts)| counts.clone())
                .unwrap_or_default()
        };
        let by_category = take("category");
        Ok(SecurityEventsSummary {
            total: by_category.iter().map(|(_, count)| count).sum(),
            by_category,
            by_event_type: take("event_type"),
            by_result: take("result"),
            by_session: take("session_id"),
            by_run: take("run_id"),
            latest_events: latest,
        })
    }
}

/// Runs `sql` and rebuilds every row that parses.
fn collect_events(
    conn: &Connection,
    sql: &str,
    params: &[SqlValue],
) -> Result<Vec<SecurityEvent>, KernelError> {
    let mut statement = conn.prepare(sql)?;
    let mut rows = statement.query(rusqlite::params_from_iter(params.iter()))?;
    let mut events = Vec::new();
    while let Some(row) = rows.next()? {
        if let Some(event) = row_to_event(row)? {
            events.push(event);
        }
    }
    Ok(events)
}

/// Appends `values` as bound parameters and returns their placeholder list.
fn push_in_list(params: &mut Vec<SqlValue>, values: &[String]) -> String {
    values
        .iter()
        .map(|value| {
            params.push(SqlValue::Text(value.clone()));
            format!("?{}", params.len())
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Rewrites `?1..?n` per branch into a single ascending sequence.
///
/// The `UNION ALL` branches are generated from one template, so they all carry
/// `?1..?n`; `SQLite` numbers parameters per statement, not per branch.
///
/// Only `?` followed by at least one digit is treated as a placeholder. A bare
/// `?` — or one inside a string literal such as `LIKE '%?%'` — is copied through
/// untouched, so this stays correct if it is ever reused for another template.
fn renumber_placeholders(sql: &str, per_branch: usize) -> String {
    if per_branch == 0 {
        return sql.to_owned();
    }
    let mut out = String::with_capacity(sql.len());
    let mut rest = sql;
    let mut next = 1;
    while let Some(index) = rest.find('?') {
        out.push_str(&rest[..index]);
        let digits: String = rest[index + 1..]
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        if digits.is_empty() {
            out.push('?');
            rest = &rest[index + 1..];
            continue;
        }
        let _ = write!(out, "?{next}");
        next += 1;
        rest = &rest[index + 1 + digits.len()..];
    }
    out.push_str(rest);
    out
}

/// Validates a `count_by` group field against the V1 allowlist.
///
/// # Errors
///
/// Returns [`KernelError::Malformed`] when `group_field` is not supported.
pub fn validate_group_field(group_field: &str) -> Result<&'static str, KernelError> {
    VALID_GROUP_FIELDS
        .iter()
        .copied()
        .find(|field| *field == group_field)
        .ok_or_else(|| {
            let mut sorted = VALID_GROUP_FIELDS.to_vec();
            sorted.sort_unstable();
            KernelError::Malformed(format!(
                "Invalid group_field: '{group_field}'. Must be one of: {}",
                sorted.join(", ")
            ))
        })
}

/// Builds the bound column values for one insert, in table column order.
///
/// Returns `None` when the event cannot be rendered, which is what makes a
/// malformed event indistinguishable from a skipped write.
fn event_values(event: &SecurityEvent) -> Option<Vec<SqlValue>> {
    let epoch = utc_iso_to_epoch(&event.timestamp, "timestamp").ok()?;
    let details = serde_json::to_string(&event.details).ok()?;
    let result = match event.result {
        EventResult::Succeeded => "succeeded",
        EventResult::Failed => "failed",
    };
    Some(vec![
        SqlValue::Text(event.event_id.clone()),
        SqlValue::Text(event.event_type.clone()),
        SqlValue::Text(event.category.clone()),
        SqlValue::Text(result.to_owned()),
        SqlValue::Text(event.timestamp.clone()),
        SqlValue::Real(epoch),
        SqlValue::Text(event.trace_id.clone()),
        SqlValue::Integer(i64::from(event.pid)),
        SqlValue::Integer(i64::from(event.uid)),
        optional_text(event.session_id.as_ref()),
        optional_text(event.run_id.as_ref()),
        optional_text(event.call_id.as_ref()),
        optional_text(event.tool_call_id.as_ref()),
        extract_verdict(&event.details).map_or(SqlValue::Null, SqlValue::Text),
        SqlValue::Text(details),
    ])
}

fn optional_text(value: Option<&String>) -> SqlValue {
    value.map_or(SqlValue::Null, |value| SqlValue::Text(value.clone()))
}

/// Rebuilds one event from a row, or reports a skip.
///
/// A row whose `details` is not a JSON object, or whose `result` is outside the
/// enum, is skipped with the same stderr line v1 prints. Column read failures
/// still propagate: those indicate a schema problem, not one bad row.
fn row_to_event(row: &Row<'_>) -> Result<Option<SecurityEvent>, KernelError> {
    let details_raw: String = row.get("details")?;
    let details = match serde_json::from_str::<Value>(&details_raw) {
        Ok(Value::Object(map)) => map,
        Ok(_) => return Ok(skip("details is not an object")),
        Err(err) => return Ok(skip(&err.to_string())),
    };
    let result_raw: String = row.get("result")?;
    let result = match result_raw.as_str() {
        "succeeded" => EventResult::Succeeded,
        "failed" => EventResult::Failed,
        other => return Ok(skip(&format!("unexpected result {other:?}"))),
    };

    Ok(Some(SecurityEvent {
        event_id: row.get("event_id")?,
        event_type: row.get("event_type")?,
        category: row.get("category")?,
        result,
        timestamp: row.get("timestamp")?,
        trace_id: row.get("trace_id")?,
        pid: row.get::<_, i64>("pid")?.try_into().unwrap_or_default(),
        uid: row.get::<_, i64>("uid")?.try_into().unwrap_or_default(),
        session_id: row.get("session_id")?,
        run_id: row.get("run_id")?,
        call_id: row.get("call_id")?,
        tool_call_id: row.get("tool_call_id")?,
        details,
    }))
}

fn skip(reason: &str) -> Option<SecurityEvent> {
    eprintln!("[security_events] malformed row skipped: {reason}");
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placeholders_are_renumbered_across_branches() {
        let template =
            "SELECT a WHERE x = ?1 AND y = ?2 UNION ALL SELECT b WHERE x = ?1 AND y = ?2";
        assert_eq!(
            renumber_placeholders(template, 2),
            "SELECT a WHERE x = ?1 AND y = ?2 UNION ALL SELECT b WHERE x = ?3 AND y = ?4"
        );
    }

    #[test]
    fn a_template_without_parameters_is_untouched() {
        let template = "SELECT a UNION ALL SELECT b";
        assert_eq!(renumber_placeholders(template, 0), template);
    }

    #[test]
    fn a_question_mark_that_is_not_a_placeholder_is_preserved() {
        // A bare `?` and one inside a literal must survive: only `?<digits>` is a
        // numbered placeholder.
        let template = "SELECT a WHERE x = ?1 AND note LIKE '%?%' AND y = ?";
        assert_eq!(
            renumber_placeholders(template, 1),
            "SELECT a WHERE x = ?1 AND note LIKE '%?%' AND y = ?"
        );
    }

    #[test]
    fn double_digit_placeholders_are_consumed_whole() {
        let template = "VALUES (?10, ?11)";
        assert_eq!(renumber_placeholders(template, 2), "VALUES (?1, ?2)");
    }

    #[test]
    fn the_group_field_allowlist_is_reported_alphabetically() {
        let error = validate_group_field("details").expect_err("not groupable");
        let message = error.to_string();
        // The `malformed record: ` prefix comes from `KernelError`'s Display; the
        // v1 wording is what follows it.
        assert!(
            message.contains("Invalid group_field: 'details'."),
            "{message}"
        );
        assert!(
            message.ends_with(
                "Must be one of: call_id, category, event_type, result, run_id, session_id, tool_call_id, trace_id, verdict"
            ),
            "{message}"
        );
    }

    #[test]
    fn an_empty_filter_set_renders_no_where_clause() {
        let (clause, params) = EventFilters::default().build();
        assert!(clause.is_empty());
        assert!(params.is_empty());
    }

    #[test]
    fn filters_are_numbered_in_declaration_order() {
        let filters = EventFilters {
            category: Some("exec".to_owned()),
            verdict: Some("deny".to_owned()),
            since_epoch: Some(1.0),
            ..EventFilters::default()
        };
        let (clause, params) = filters.build();
        assert_eq!(
            clause,
            " WHERE category = ?1 AND verdict = ?2 AND timestamp_epoch >= ?3"
        );
        assert_eq!(params.len(), 3);
    }
}
