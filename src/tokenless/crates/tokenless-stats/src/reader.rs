//! Read existing statistics without initialization, migration, or inferred values.
//! Record IDs order pages within the current database; they are not recovery checkpoints.

use std::collections::HashSet;
use std::path::Path;
use std::time::Duration;

use rusqlite::{Connection, OpenFlags, params, types::ValueRef};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

const FIELDS: &[&str] = &[
    "id",
    "timestamp",
    "operation",
    "agent_id",
    "source_pid",
    "session_id",
    "tool_use_id",
    "before_chars",
    "before_tokens",
    "after_chars",
    "after_tokens",
    "mode",
    "stash_writes",
    "stash_errors",
    "stash_size",
    "content_type",
    "content_origin",
    "applied_operations",
    "recoverability",
    "tokenizer_id",
    "unrecoverable_truncations",
    "before_text",
    "after_text",
    "before_output",
    "after_output",
];
const REQUIRED: &[&str] = &[
    "id",
    "timestamp",
    "operation",
    "agent_id",
    "before_chars",
    "before_tokens",
    "after_chars",
    "after_tokens",
];
const CONTENT: &[&str] = &["before_text", "after_text", "before_output", "after_output"];

/// Maximum metadata records returned by one query.
pub const MAX_PAGE_SIZE: usize = 100;

/// Errors in opening or querying an existing statistics source.
#[derive(Debug, thiserror::Error)]
pub enum ReadError {
    /// SQLite access failed; this is not an empty query result.
    #[error("statistics database read failed: {0}")]
    Database(#[from] rusqlite::Error),
    /// Required columns are absent or the database layout is unsupported.
    #[error("unsupported statistics layout: missing {0}")]
    Unsupported(String),
    /// Query arguments violate the bounded query contract.
    #[error("invalid statistics query: {0}")]
    InvalidQuery(String),
    /// A stored value cannot be represented by the query contract.
    #[error("invalid statistics record: {0}")]
    InvalidRecord(String),
}

/// Filters for metadata pages, ordered by descending source record ID.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RecordQuery {
    /// Page size in `1..=MAX_PAGE_SIZE`.
    pub limit: usize,
    /// Exclusive upper ID bound for the next page of this query.
    pub before_id: Option<i64>,
    /// Exact session match; absent identifiers are not inferred.
    pub session_id: Option<String>,
    /// Exact tool-call match, optionally combined with the session filter.
    pub tool_use_id: Option<String>,
}

impl Default for RecordQuery {
    fn default() -> Self {
        Self {
            limit: 50,
            before_id: None,
            session_id: None,
            tool_use_id: None,
        }
    }
}

/// One metadata page; optional historical fields remain JSON null.
#[derive(Debug, Serialize, Deserialize)]
pub struct RecordPage {
    /// Source records with `content_included=false` and omitted content fields.
    pub records: Vec<Value>,
    /// Continue using the same filters and this exclusive ID bound.
    pub next_before_id: Option<i64>,
}

/// Query connection that never initializes or migrates a statistics database.
pub struct StatsReader {
    conn: Connection,
    columns: HashSet<String>,
}

impl StatsReader {
    /// Open an existing database, including a live WAL database.
    ///
    /// # Errors
    /// Returns access errors or an unsupported layout; missing files are not created.
    pub fn open(path: &Path) -> Result<Self, ReadError> {
        let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        conn.busy_timeout(Duration::from_millis(500))?;
        let columns = conn
            .prepare("PRAGMA table_info(stats)")?
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<Result<HashSet<_>, _>>()?;
        for name in REQUIRED {
            if !columns.contains(*name) {
                return Err(ReadError::Unsupported((*name).to_owned()));
            }
        }
        Ok(Self { conn, columns })
    }

    /// Query a bounded metadata page without loading stored content.
    ///
    /// # Errors
    /// Returns invalid query, stored-data, or SQLite errors without skipping rows.
    pub fn list(&self, query: &RecordQuery) -> Result<RecordPage, ReadError> {
        if query.limit == 0
            || query.limit > MAX_PAGE_SIZE
            || query.before_id.is_some_and(|id| id <= 0)
        {
            return Err(ReadError::InvalidQuery(
                "limit must be 1..=100 and before_id positive".into(),
            ));
        }
        for (field, filter) in [
            ("session_id", &query.session_id),
            ("tool_use_id", &query.tool_use_id),
        ] {
            if filter.is_some() && !self.columns.contains(field) {
                return Err(ReadError::Unsupported(field.into()));
            }
        }
        // Identifiers come only from the fixed field allowlist; all input values are bound.
        let sql = format!(
            "SELECT {} FROM stats WHERE (?1 IS NULL OR id < ?1)
            AND (?2 IS NULL OR session_id = ?2) AND (?3 IS NULL OR tool_use_id = ?3)
            ORDER BY id DESC LIMIT ?4",
            self.select(false)
        );
        // Legacy layouts may omit optional attribution columns entirely.
        let sql = sql
            .replace(
                "session_id = ?2",
                if self.columns.contains("session_id") {
                    "session_id = ?2"
                } else {
                    "NULL = ?2"
                },
            )
            .replace(
                "tool_use_id = ?3",
                if self.columns.contains("tool_use_id") {
                    "tool_use_id = ?3"
                } else {
                    "NULL = ?3"
                },
            );
        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt.query(params![
            query.before_id,
            query.session_id,
            query.tool_use_id,
            query.limit + 1
        ])?;
        let mut records = Vec::new();
        while let Some(row) = rows.next()? {
            records.push(read_record(row, false)?);
        }
        let next_before_id = if records.len() > query.limit {
            records.truncate(query.limit);
            records.last().and_then(|record| record["id"].as_i64())
        } else {
            None
        };
        Ok(RecordPage {
            records,
            next_before_id,
        })
    }

    /// Read one record, optionally including all stored before/after content.
    ///
    /// `content_included=true` with a null content field means it was not stored.
    /// # Errors
    /// Returns invalid query, stored-data, or SQLite errors. Absent IDs return `None`.
    pub fn get(&self, id: i64, include_content: bool) -> Result<Option<Value>, ReadError> {
        if id <= 0 {
            return Err(ReadError::InvalidQuery("record ID must be positive".into()));
        }
        let sql = format!(
            "SELECT {} FROM stats WHERE id = ?1",
            self.select(include_content)
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt.query([id])?;
        rows.next()?
            .map(|row| read_record(row, include_content))
            .transpose()
    }

    fn select(&self, include_content: bool) -> String {
        FIELDS
            .iter()
            .map(|name| {
                if self.columns.contains(*name) && (include_content || !CONTENT.contains(name)) {
                    (*name).to_owned()
                } else {
                    format!("NULL AS {name}")
                }
            })
            .collect::<Vec<_>>()
            .join(", ")
    }
}

fn read_record(row: &rusqlite::Row<'_>, include_content: bool) -> Result<Value, ReadError> {
    let mut values = Map::new();
    for (index, name) in FIELDS.iter().enumerate() {
        let value = match row.get_ref(index)? {
            ValueRef::Null => Value::Null,
            ValueRef::Integer(number) => Value::from(number),
            ValueRef::Text(bytes) => Value::String(
                std::str::from_utf8(bytes)
                    .map_err(|_| ReadError::InvalidRecord(format!("{name} is not UTF-8")))?
                    .into(),
            ),
            _ => {
                return Err(ReadError::InvalidRecord(format!(
                    "unexpected value type for {name}"
                )));
            }
        };
        values.insert((*name).into(), value);
    }
    for name in [
        "id",
        "before_chars",
        "before_tokens",
        "after_chars",
        "after_tokens",
    ] {
        if !values[name]
            .as_i64()
            .is_some_and(|value| value >= 0 && (name != "id" || value > 0))
        {
            return Err(ReadError::InvalidRecord(format!("invalid {name}")));
        }
    }
    for name in ["timestamp", "operation", "agent_id"] {
        if !values[name].is_string() {
            return Err(ReadError::InvalidRecord(format!("invalid {name}")));
        }
    }
    let timestamp = values["timestamp"]
        .as_str()
        .ok_or_else(|| ReadError::InvalidRecord("timestamp is not text".into()))?;
    chrono::DateTime::parse_from_rfc3339(timestamp)
        .map_err(|_| ReadError::InvalidRecord("timestamp is not RFC3339".into()))?;
    for name in CONTENT
        .iter()
        .copied()
        .chain(["session_id", "tool_use_id", "mode", "tokenizer_id"])
    {
        if !values[name].is_null() && !values[name].is_string() {
            return Err(ReadError::InvalidRecord(format!("invalid {name}")));
        }
    }
    values.insert("content_included".into(), Value::Bool(include_content));
    Ok(Value::Object(values))
}

#[cfg(test)]
#[path = "reader_tests.rs"]
mod tests;
