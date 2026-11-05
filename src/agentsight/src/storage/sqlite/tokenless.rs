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

    /// SQL predicate that keeps applied savings only, when `mode` exists.
    ///
    /// Tokenless records predicted `before`/`after` values in dry-run mode
    /// without emitting the compressed text, so those rows are not savings the
    /// agent actually received. The column was added after the first stats.db
    /// releases; an old database without it keeps its historical behaviour.
    fn active_mode_filter(&self) -> &'static str {
        let has_mode = self
            .conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('stats') WHERE name = 'mode'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .map(|count| count > 0)
            .unwrap_or(false);
        if has_mode {
            " AND COALESCE(LOWER(mode), 'active') NOT IN ('dry-run', 'dryrun')"
        } else {
            ""
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
            let mode_filter = self.active_mode_filter();
            let sql = format!(
                "SELECT session_id, tool_use_id, before_tokens, after_tokens, before_text, after_text, operation \
                 FROM stats WHERE session_id IN ({placeholders}){mode_filter}"
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
            let mode_filter = self.active_mode_filter();
            let sql = format!(
                "SELECT session_id, tool_use_id, before_tokens, after_tokens, before_text, after_text, operation \
                 FROM stats WHERE tool_use_id IN ({placeholders}){mode_filter}"
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

    /// Whether the `stats` table carries the RFC3339 `timestamp` column.
    ///
    /// The recorder has written it from the first stats.db releases, but a
    /// foreign or stripped database may lack it; the windowed readers then
    /// keep their historical unbounded behaviour instead of failing.
    fn has_timestamp_column(&self) -> bool {
        self.conn
            .query_row(
                "SELECT COUNT(*) FROM pragma_table_info('stats') WHERE name = 'timestamp'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .map(|count| count > 0)
            .unwrap_or(false)
    }

    /// Query optimization records for the given tool_use_ids whose recorded
    /// timestamp falls within `[start_ns, end_ns]` (inclusive).
    ///
    /// Bounds the token-savings numerator to the same request window as the
    /// denominator: without it, narrowing the dashboard window kept older
    /// records in the reported savings and the rate could inflate past 100%.
    /// Like `summary_in_window`, the RFC3339 `timestamp` column is normalised
    /// to UTC epoch seconds inside SQLite as a prefilter (widened by a second
    /// on each side because `strftime('%s', …)` truncates to the enclosing
    /// second) and each surviving row is then compared against the nanosecond
    /// bounds exactly. Legacy databases without the column keep every
    /// matching row.
    ///
    /// Batches queries in groups of 500 to stay within SQLite variable limits.
    /// Returns an empty Vec on SQLITE_BUSY or other transient errors.
    pub fn get_stats_by_tool_use_ids_in_window(
        &self,
        ids: &[&str],
        start_ns: i64,
        end_ns: i64,
    ) -> Vec<TokenlessStatRow> {
        if !self.has_timestamp_column() {
            return self.get_stats_by_tool_use_ids(ids);
        }
        let start_secs = start_ns.div_euclid(1_000_000_000);
        let end_secs = end_ns.div_euclid(1_000_000_000);

        let mut results = Vec::new();
        for chunk in ids.chunks(500) {
            let placeholders: String = chunk.iter().map(|_| "?").collect::<Vec<_>>().join(",");
            let mode_filter = self.active_mode_filter();
            let sql = format!(
                "SELECT timestamp, session_id, tool_use_id, before_tokens, after_tokens, before_text, after_text, operation \
                 FROM stats WHERE tool_use_id IN ({placeholders}){mode_filter} \
                 AND CAST(strftime('%s', timestamp) AS INTEGER) BETWEEN ? AND ?"
            );

            let mut stmt = match self.conn.prepare(&sql) {
                Ok(s) => s,
                Err(e) => {
                    log::warn!("Failed to prepare windowed stats query by tool_use_id: {e}");
                    return Vec::new();
                }
            };

            let mut params: Vec<&dyn rusqlite::types::ToSql> = chunk
                .iter()
                .map(|s| s as &dyn rusqlite::types::ToSql)
                .collect();
            let lower = start_secs.saturating_sub(1);
            let upper = end_secs.saturating_add(1);
            params.push(&lower);
            params.push(&upper);
            let rows = match stmt.query_map(params.as_slice(), |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    TokenlessStatRow {
                        session_id: row.get(1)?,
                        tool_use_id: row.get(2)?,
                        before_tokens: row.get(3)?,
                        after_tokens: row.get(4)?,
                        before_text: row.get(5)?,
                        after_text: row.get(6)?,
                        operation: row.get(7)?,
                    },
                ))
            }) {
                Ok(rows) => rows,
                Err(e) => {
                    log::warn!("Failed to query stats.db by tool_use_id in a window: {e}");
                    return Vec::new();
                }
            };

            for row in rows {
                match row {
                    // Rows inside the widened edge seconds are compared against
                    // the nanosecond bounds exactly, as in summary_in_window.
                    Ok((timestamp, record))
                        if tokenless_timestamp_in_window(&timestamp, start_ns, end_ns) =>
                    {
                        results.push(record);
                    }
                    Ok(_) => {}
                    Err(e) => log::warn!("Error reading stats row: {e}"),
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

        // The second range is only a prefilter, widened by a second on each side
        // because `strftime('%s', …)` truncates every row to its enclosing
        // second. The documented bounds are nanoseconds and inclusive, so each
        // candidate row is compared against them exactly below — without that
        // second pass a record written just before the start (or just after the
        // end) inside the same second was counted, silently growing the window.
        let mode_filter = self.active_mode_filter();
        let sql = format!(
            "SELECT timestamp, before_tokens, after_tokens \
             FROM stats \
             WHERE CAST(strftime('%s', timestamp) AS INTEGER) BETWEEN ?1 AND ?2{mode_filter}"
        );

        let mut statement = match self.conn.prepare(&sql) {
            Ok(statement) => statement,
            Err(e) => {
                log::warn!("Failed to prepare tokenless window summary: {e}");
                return TokenlessWindowSummary::default();
            }
        };
        let rows = match statement.query_map(
            [start_secs.saturating_sub(1), end_secs.saturating_add(1)],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            },
        ) {
            Ok(rows) => rows,
            Err(e) => {
                log::warn!("Failed to aggregate tokenless window summary: {e}");
                return TokenlessWindowSummary::default();
            }
        };

        let mut summary = TokenlessWindowSummary::default();
        for (timestamp, before_tokens, after_tokens) in rows.flatten() {
            // Rows whose timestamp is not parseable stay excluded, as the
            // module documents.
            if !tokenless_timestamp_in_window(&timestamp, start_ns, end_ns) {
                continue;
            }
            summary.records += 1;
            summary.before_tokens += before_tokens;
            summary.after_tokens += after_tokens;
        }
        summary
    }
}

/// Whether a `stats.timestamp` value falls inside `[start_ns, end_ns]`.
///
/// The recorder writes RFC3339 with whatever offset it ran under, so the value
/// is resolved to an instant rather than compared as text; an unparseable value
/// is simply not in the window.
fn tokenless_timestamp_in_window(timestamp: &str, start_ns: i64, end_ns: i64) -> bool {
    chrono::DateTime::parse_from_rfc3339(timestamp)
        .ok()
        .and_then(|parsed| parsed.timestamp_nanos_opt())
        .is_some_and(|ns| ns >= start_ns && ns <= end_ns)
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

    /// The documented window is `[start_ns, end_ns]`, inclusive and in
    /// nanoseconds, but the aggregate compared whole seconds: every row inside
    /// the window's second was counted, including ones written before the start
    /// and after the end.
    #[test]
    fn summary_in_window_honours_the_exact_nanosecond_bounds() {
        let base_secs: i64 = 1_700_000_000;
        let row = |ms: u32, before: i64, after: i64| {
            (
                Utc.timestamp_opt(base_secs, ms * 1_000_000)
                    .unwrap()
                    .to_rfc3339(),
                before,
                after,
            )
        };
        let (store, path) = store_with_rows(&[
            (&row(0, 100, 10).0, 100, 10),
            (&row(500, 200, 20).0, 200, 20),
            (&row(900, 400, 40).0, 400, 40),
        ]);

        // Window: [base + 200 ms, base + 700 ms] — a single row qualifies.
        let start_ns = base_secs * 1_000_000_000 + 200_000_000;
        let end_ns = base_secs * 1_000_000_000 + 700_000_000;
        let summary = store.summary_in_window(start_ns, end_ns);

        assert_eq!(
            summary.records, 1,
            "only the row inside the window may count"
        );
        assert_eq!(summary.before_tokens, 200);
        assert_eq!(summary.after_tokens, 20);

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

    #[test]
    fn readers_count_only_applied_savings() {
        // Tokenless records predicted values in dry-run mode but emits the
        // original text, so those rows never reached the model. The filtered
        // readers must skip them; a legacy database without the `mode` column
        // keeps every row (covered by `store_with_rows` above).
        let base_secs: i64 = 1_700_000_000;
        let timestamp = Utc.timestamp_opt(base_secs, 0).unwrap().to_rfc3339();
        let path = std::env::temp_dir().join(format!(
            "test_tokenless_mode_{}.db",
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
                    session_id TEXT,
                    tool_use_id TEXT,
                    before_tokens INTEGER NOT NULL,
                    after_tokens INTEGER NOT NULL,
                    before_text TEXT,
                    after_text TEXT,
                    operation TEXT NOT NULL,
                    mode TEXT
                )",
                [],
            )
            .unwrap();
            for (session, tool, mode, before, after) in [
                ("s1", "t1", "active", 1000, 100),
                ("s1", "t2", "dry-run", 5000, 50),
                ("s2", "t3", "dryrun", 7000, 70),
            ] {
                conn.execute(
                    "INSERT INTO stats
                     (timestamp, session_id, tool_use_id, before_tokens, after_tokens, operation, mode)
                     VALUES (?1, ?2, ?3, ?4, ?5, 'compress', ?6)",
                    rusqlite::params![timestamp, session, tool, before, after, mode],
                )
                .unwrap();
            }
        }

        let store = TokenlessStatsStore::open_if_exists(&path).unwrap();
        let by_session = store.get_stats_by_session_ids(&["s1", "s2"]);
        assert_eq!(by_session.len(), 1, "dry-run rows are not applied savings");
        assert_eq!(by_session[0].tool_use_id, "t1");

        let by_tool = store.get_stats_by_tool_use_ids(&["t1", "t2", "t3"]);
        assert_eq!(by_tool.len(), 1);
        assert_eq!(by_tool[0].session_id, "s1");

        let summary = store.summary_in_window(0, i64::MAX);
        assert_eq!(summary.records, 1);
        assert_eq!(summary.before_tokens, 1000);
        assert_eq!(summary.after_tokens, 100);
        assert_eq!(summary.saved_tokens(), 900);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn windowed_tool_rows_are_bounded_by_the_record_timestamp() {
        // The savings denominator is windowed by the request, so the numerator
        // must be too: without the timestamp bound, older records kept landing
        // in the reported savings and the rate could inflate past 100%.
        let base_secs: i64 = 1_700_000_000;
        let timestamp = |secs: i64, millis: u32| {
            Utc.timestamp_opt(secs, millis * 1_000_000)
                .unwrap()
                .to_rfc3339()
        };
        let path = std::env::temp_dir().join(format!(
            "test_tokenless_window_{}.db",
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
                    session_id TEXT,
                    tool_use_id TEXT,
                    before_tokens INTEGER NOT NULL,
                    after_tokens INTEGER NOT NULL,
                    before_text TEXT,
                    after_text TEXT,
                    operation TEXT NOT NULL
                )",
                [],
            )
            .unwrap();
            for (tool, secs, millis) in [
                ("t-in", base_secs + 60, 0),
                ("t-before", base_secs - 48 * 3600, 0),
                ("t-after", base_secs + 48 * 3600, 0),
                // Inside the widened edge second, but past the exact
                // nanosecond bound — the second pass must drop it.
                ("t-edge", base_secs + 3600, 500),
            ] {
                conn.execute(
                    "INSERT INTO stats
                     (timestamp, session_id, tool_use_id, before_tokens, after_tokens, operation)
                     VALUES (?1, 's1', ?2, 1000, 100, 'compress')",
                    rusqlite::params![timestamp(secs, millis), tool],
                )
                .unwrap();
            }
        }

        let store = TokenlessStatsStore::open_if_exists(&path).unwrap();
        let ids = ["t-in", "t-before", "t-after", "t-edge"];
        let start_ns = base_secs * 1_000_000_000;
        let end_ns = (base_secs + 3600) * 1_000_000_000;
        let windowed = store.get_stats_by_tool_use_ids_in_window(&ids, start_ns, end_ns);
        assert_eq!(
            windowed
                .iter()
                .map(|row| row.tool_use_id.as_str())
                .collect::<Vec<_>>(),
            ["t-in"],
            "only the in-window record may count"
        );
        // The full range keeps every row, including the edge one.
        assert_eq!(
            store
                .get_stats_by_tool_use_ids_in_window(&ids, 0, i64::MAX)
                .len(),
            4
        );

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn windowed_tool_rows_keep_legacy_databases_unbounded() {
        // A stats table without the `timestamp` column cannot be bounded; the
        // windowed read must keep the historical behaviour of returning every
        // matching row (same degradation as the `mode` filter).
        let path = std::env::temp_dir().join(format!(
            "test_tokenless_window_legacy_{}.db",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute(
                "CREATE TABLE stats (
                    session_id TEXT,
                    tool_use_id TEXT,
                    before_tokens INTEGER NOT NULL,
                    after_tokens INTEGER NOT NULL,
                    before_text TEXT,
                    after_text TEXT,
                    operation TEXT NOT NULL
                )",
                [],
            )
            .unwrap();
            for tool in ["t1", "t2"] {
                conn.execute(
                    "INSERT INTO stats
                     (session_id, tool_use_id, before_tokens, after_tokens, operation)
                     VALUES ('s1', ?1, 1000, 100, 'compress')",
                    [tool],
                )
                .unwrap();
            }
        }

        let store = TokenlessStatsStore::open_if_exists(&path).unwrap();
        // An empty window still returns every matching row: the table cannot
        // be bounded, so the historical behaviour is preserved.
        let windowed = store.get_stats_by_tool_use_ids_in_window(&["t1", "t2"], 0, 0);
        assert_eq!(
            windowed.len(),
            2,
            "a legacy database keeps every matching row"
        );

        let _ = std::fs::remove_file(&path);
    }
}
