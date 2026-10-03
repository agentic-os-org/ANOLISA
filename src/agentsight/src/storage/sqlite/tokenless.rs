//! Read-only store for ~/.tokenless/stats.db
//!
//! This database is created and maintained by an external component (tokenless).
//! AgentSight only reads from it to display token savings data.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{Connection, OpenFlags};

/// Default path to the tokenless stats database
pub fn default_stats_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
    PathBuf::from(home).join(".tokenless").join("stats.db")
}

/// A single optimization record from stats.db
#[derive(Debug, Clone)]
pub struct TokenlessStatRow {
    pub session_id: String,
    pub tool_use_id: String,
    pub before_tokens: i64,
    pub after_tokens: i64,
    pub before_text: Option<String>,
    pub after_text: Option<String>,
    pub operation: String,
}

/// Read-only store for the tokenless stats database
pub struct TokenlessStatsStore {
    conn: Connection,
}

impl TokenlessStatsStore {
    /// Open stats.db if it exists. Returns None if the file is missing.
    pub fn open_if_exists(path: &Path) -> Option<Self> {
        if !path.exists() {
            return None;
        }

        let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
        match Connection::open_with_flags(path, flags) {
            Ok(conn) => {
                if let Err(e) = conn.busy_timeout(Duration::from_millis(500)) {
                    log::warn!("Failed to set busy_timeout on stats.db: {e}");
                }
                Some(TokenlessStatsStore { conn })
            }
            Err(e) => {
                log::warn!("Failed to open stats.db at {path:?}: {e}");
                None
            }
        }
    }

    /// Query optimization records for the given session IDs.
    ///
    /// Batches queries in groups of 500 to stay within SQLite variable limits.
    /// Returns an empty Vec on SQLITE_BUSY or other transient errors.
    pub fn get_stats_by_session_ids(&self, ids: &[&str]) -> Vec<TokenlessStatRow> {
        let mut results = Vec::new();

        for chunk in ids.chunks(500) {
            let placeholders: String = chunk.iter().map(|_| "?").collect::<Vec<_>>().join(",");
            let sql = format!(
                "SELECT session_id, tool_use_id, before_tokens, after_tokens, before_text, after_text, operation \
                 FROM stats WHERE session_id IN ({placeholders})"
            );

            let mut stmt = match self.conn.prepare(&sql) {
                Ok(s) => s,
                Err(e) => {
                    log::warn!("Failed to prepare stats query: {e}");
                    return Vec::new();
                }
            };

            let params: Vec<&dyn rusqlite::types::ToSql> = chunk
                .iter()
                .map(|s| s as &dyn rusqlite::types::ToSql)
                .collect();

            let rows = match stmt.query_map(params.as_slice(), |row| {
                Ok(TokenlessStatRow {
                    session_id: row.get(0)?,
                    tool_use_id: row.get(1)?,
                    before_tokens: row.get(2)?,
                    after_tokens: row.get(3)?,
                    before_text: row.get(4)?,
                    after_text: row.get(5)?,
                    operation: row.get(6)?,
                })
            }) {
                Ok(rows) => rows,
                Err(e) => {
                    log::warn!("Failed to query stats.db: {e}");
                    return Vec::new();
                }
            };

            for row in rows {
                match row {
                    Ok(r) => results.push(r),
                    Err(e) => {
                        log::warn!("Error reading stats row: {e}");
                    }
                }
            }
        }

        results
    }

    /// Query optimization records for the given tool_use_ids.
    ///
    /// Batches queries in groups of 500 to stay within SQLite variable limits.
    /// Returns an empty Vec on SQLITE_BUSY or other transient errors.
    pub fn get_stats_by_tool_use_ids(&self, ids: &[&str]) -> Vec<TokenlessStatRow> {
        let mut results = Vec::new();

        for chunk in ids.chunks(500) {
            let placeholders: String = chunk.iter().map(|_| "?").collect::<Vec<_>>().join(",");
            let sql = format!(
                "SELECT session_id, tool_use_id, before_tokens, after_tokens, before_text, after_text, operation \
                 FROM stats WHERE tool_use_id IN ({placeholders})"
            );

            let mut stmt = match self.conn.prepare(&sql) {
                Ok(s) => s,
                Err(e) => {
                    log::warn!("Failed to prepare stats query by tool_use_id: {e}");
                    return Vec::new();
                }
            };

            let params: Vec<&dyn rusqlite::types::ToSql> = chunk
                .iter()
                .map(|s| s as &dyn rusqlite::types::ToSql)
                .collect();

            let rows = match stmt.query_map(params.as_slice(), |row| {
                Ok(TokenlessStatRow {
                    session_id: row.get(0)?,
                    tool_use_id: row.get(1)?,
                    before_tokens: row.get(2)?,
                    after_tokens: row.get(3)?,
                    before_text: row.get(4)?,
                    after_text: row.get(5)?,
                    operation: row.get(6)?,
                })
            }) {
                Ok(rows) => rows,
                Err(e) => {
                    log::warn!("Failed to query stats.db by tool_use_id: {e}");
                    return Vec::new();
                }
            };

            for row in rows {
                match row {
                    Ok(r) => results.push(r),
                    Err(e) => {
                        log::warn!("Error reading stats row: {e}");
                    }
                }
            }
        }

        results
    }

    /// Group stat rows by session_id for efficient lookup.
    pub fn group_by_session(rows: Vec<TokenlessStatRow>) -> HashMap<String, Vec<TokenlessStatRow>> {
        let mut map: HashMap<String, Vec<TokenlessStatRow>> = HashMap::new();
        for row in rows {
            map.entry(row.session_id.clone()).or_default().push(row);
        }
        map
    }

    /// Aggregate token savings for records whose timestamp falls within
    /// `[start_ns, end_ns]` (inclusive), used by the `summary` command.
    ///
    /// The `stats` table stores timestamps as RFC3339 text (e.g.
    /// `2026-06-09T14:30:00.123+08:00`), so we normalise each row to UTC
    /// epoch seconds inside SQLite via `strftime('%s', ...)`. This is robust
    /// to whatever timezone offset the recorder wrote — unlike a lexicographic
    /// string comparison, which only holds when every row shares one offset.
    ///
    /// Returns a zeroed summary on a query error (e.g. a missing `stats` table
    /// or SQLITE_BUSY) so the caller degrades gracefully. Rows whose `timestamp`
    /// is not RFC3339-parseable yield NULL from `strftime` and are silently
    /// excluded from the aggregate — they do not zero the window; all valid rows
    /// are still counted.
    pub fn summary_in_window(&self, start_ns: i64, end_ns: i64) -> TokenlessWindowSummary {
        let start_secs = start_ns.div_euclid(1_000_000_000);
        let end_secs = end_ns.div_euclid(1_000_000_000);

        let sql = "SELECT COUNT(*), \
                          COALESCE(SUM(before_tokens), 0), \
                          COALESCE(SUM(after_tokens), 0) \
                   FROM stats \
                   WHERE CAST(strftime('%s', timestamp) AS INTEGER) BETWEEN ?1 AND ?2";

        let result = self.conn.query_row(sql, [start_secs, end_secs], |row| {
            Ok(TokenlessWindowSummary {
                records: row.get(0)?,
                before_tokens: row.get(1)?,
                after_tokens: row.get(2)?,
            })
        });

        match result {
            Ok(summary) => summary,
            Err(e) => {
                log::warn!("Failed to aggregate tokenless window summary: {e}");
                TokenlessWindowSummary::default()
            }
        }
    }
}

/// Aggregated tokenless token savings over a time window.
#[derive(Debug, Clone, Default)]
pub struct TokenlessWindowSummary {
    /// Number of optimization records in the window.
    pub records: i64,
    /// Sum of input (pre-compression) tokens.
    pub before_tokens: i64,
    /// Sum of output (post-compression) tokens.
    pub after_tokens: i64,
}

impl TokenlessWindowSummary {
    /// Tokens saved (clamped at 0 — a record can never legitimately expand,
    /// but guard against it rather than report a negative saving).
    pub fn saved_tokens(&self) -> i64 {
        (self.before_tokens - self.after_tokens).max(0)
    }

    /// Percentage of input tokens saved (0.0 when there is no input).
    pub fn saved_percent(&self) -> f64 {
        if self.before_tokens > 0 {
            (self.saved_tokens() as f64 / self.before_tokens as f64) * 100.0
        } else {
            0.0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{FixedOffset, TimeZone, Utc};
    use std::time::{SystemTime, UNIX_EPOCH};

    /// Create a stats.db with the queried columns and the given
    /// `(timestamp_text, before_tokens, after_tokens)` rows, then reopen it
    /// through the read-only store.
    fn store_with_rows(rows: &[(&str, i64, i64)]) -> (TokenlessStatsStore, PathBuf) {
        let path = std::env::temp_dir().join(format!(
            "test_tokenless_stats_{}.db",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute(
                "CREATE TABLE stats (
                    timestamp TEXT NOT NULL,
                    before_tokens INTEGER NOT NULL,
                    after_tokens INTEGER NOT NULL
                )",
                [],
            )
            .unwrap();
            for (ts, before, after) in rows {
                conn.execute(
                    "INSERT INTO stats (timestamp, before_tokens, after_tokens) VALUES (?1, ?2, ?3)",
                    rusqlite::params![ts, before, after],
                )
                .unwrap();
            }
        }
        let store = TokenlessStatsStore::open_if_exists(&path).unwrap();
        (store, path)
    }

    #[test]
    fn test_summary_in_window_filters_and_normalises_timezone() {
        // Fixed reference instant (UTC seconds).
        let base_secs: i64 = 1_700_000_000;
        let base = Utc.timestamp_opt(base_secs, 0).unwrap();
        // Same instant, but written with a +08:00 offset — must still be
        // counted at the correct UTC time (proves strftime normalisation).
        let plus8 = base.with_timezone(&FixedOffset::east_opt(8 * 3600).unwrap());
        // 48h before the window start — must be excluded.
        let older = Utc.timestamp_opt(base_secs - 48 * 3600, 0).unwrap();

        let (store, path) = store_with_rows(&[
            (&base.to_rfc3339(), 1000, 200),
            (&plus8.to_rfc3339(), 500, 100),
            (&older.to_rfc3339(), 9999, 9999),
        ]);

        // Window: [base - 24h, base + 1h].
        let start_ns = (base_secs - 24 * 3600) * 1_000_000_000;
        let end_ns = (base_secs + 3600) * 1_000_000_000;
        let summary = store.summary_in_window(start_ns, end_ns);

        assert_eq!(summary.records, 2, "old row must be excluded");
        assert_eq!(summary.before_tokens, 1500);
        assert_eq!(summary.after_tokens, 300);
        assert_eq!(summary.saved_tokens(), 1200);
        assert_eq!(summary.saved_percent(), 80.0);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_summary_in_window_empty_window() {
        let base_secs: i64 = 1_700_000_000;
        let base = Utc.timestamp_opt(base_secs, 0).unwrap();
        let (store, path) = store_with_rows(&[(&base.to_rfc3339(), 1000, 200)]);

        // Window far in the future — no rows match.
        let start_ns = (base_secs + 100 * 3600) * 1_000_000_000;
        let end_ns = (base_secs + 200 * 3600) * 1_000_000_000;
        let summary = store.summary_in_window(start_ns, end_ns);

        assert_eq!(summary.records, 0);
        assert_eq!(summary.before_tokens, 0);
        assert_eq!(summary.after_tokens, 0);
        assert_eq!(summary.saved_tokens(), 0);
        assert_eq!(summary.saved_percent(), 0.0);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_summary_in_window_missing_table_degrades() {
        // A db file with no `stats` table → query errors → zeroed summary.
        let path = std::env::temp_dir().join(format!(
            "test_tokenless_notable_{}.db",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute("CREATE TABLE other (x INTEGER)", []).unwrap();
        }
        let store = TokenlessStatsStore::open_if_exists(&path).unwrap();
        let summary = store.summary_in_window(0, i64::MAX);
        assert_eq!(summary.records, 0);
        assert_eq!(summary.before_tokens, 0);

        let _ = std::fs::remove_file(&path);
    }
}
