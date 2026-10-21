//! Interruption subcommand — query and manage session interruption events.
//!
//! # Overview
//!
//! This subcommand provides direct CLI access to the interruption_events SQLite
//! database used by AgentSight to track conversation interruptions detected
//! during AI agent sessions.
//!
//! # Database
//!
//! Default path: `/var/log/sysak/.agentsight/interruption_events.db`
//!
//! # Interruption Types
//!
//! `--type` accepts every `InterruptionType` variant (see
//! `src/interruption/types.rs`); the values are derived from
//! `InterruptionType::ALL` so the filter cannot fall behind the detector.
//! The most frequently filtered ones:
//!
//! | Type              | Description                                           | Default Severity |
//! |-------------------|-------------------------------------------------------|-----------------|
//! | llm_error         | HTTP status >= 400 or SSE body contains {"error":...} | high            |
//! | sse_truncated     | SSE stream ended without finish_reason=stop/[DONE]    | high            |
//! | context_overflow  | finish_reason=content_filter or context_length_exceeded| high            |
//! | agent_crash       | Agent process disappeared mid-session (OOM/signal)    | critical        |
//! | token_limit       | finish_reason=length and output_tokens >= max * 0.95  | medium          |
//! | dead_loop         | Repeated tool sequences or similar outputs, no progress| critical        |
//! | retry_storm       | Same error repeated past the retry threshold          | critical        |
//!
//! # Severity Levels
//!
//! critical > high > medium > low
//!
//! # Examples
//!
//! ```bash
//! # List all interruptions in the last 24 hours
//! agentsight interruption list --last 24
//!
//! # List only unresolved critical/high interruptions
//! agentsight interruption list --last 48 --severity high --unresolved
//!
//! # Show per-type statistics for the last 7 days
//! agentsight interruption stats --last 168
//!
//! # Count interruptions grouped by severity
//! agentsight interruption count --last 24
//!
//! # Get a specific interruption event by ID
//! agentsight interruption get <INTERRUPTION_ID>
//!
//! # List all interruptions for a specific session
//! agentsight interruption session <SESSION_ID>
//!
//! # List all interruptions for a specific conversation
//! agentsight interruption conversation <CONVERSATION_ID>
//!
//! # Mark an interruption as resolved
//! agentsight interruption resolve <INTERRUPTION_ID>
//!
//! # Output as JSON (for programmatic consumption)
//! agentsight interruption list --last 24 --json
//! ```

use agentsight::database::{
    DatabaseAccess, DatabaseCoverage, DatabaseId, DatabaseManager, DatabaseRole, DatabaseSpec,
};
use agentsight::interruption::InterruptionType;
use agentsight::storage::sqlite::{GenAISqliteStore, InterruptionRecord, InterruptionStore};
use std::sync::OnceLock;
use structopt::StructOpt;

/// Type identifiers accepted by `--type`, derived from `InterruptionType::ALL`.
///
/// Deriving them keeps the filter from covering fewer types than the detector
/// produces, which is what happened while the list was hand-written.
fn interruption_type_values() -> &'static [&'static str] {
    static VALUES: OnceLock<Vec<&'static str>> = OnceLock::new();
    VALUES.get_or_init(|| InterruptionType::ALL.iter().map(|t| t.as_str()).collect())
}

/// Query and manage AI agent session interruption events.
///
/// Accesses the interruption_events SQLite database to list, filter, count,
/// and resolve interruption events detected during agent conversations.
///
/// Default database: /var/log/sysak/.agentsight/interruption_events.db
///
/// Interruption types: every `InterruptionType` variant, listed by
/// `agentsight interruption list --help`
/// Severity levels: critical, high, medium, low
#[derive(Debug, StructOpt, Clone)]
pub struct InterruptionCommand {
    /// Override the database path
    ///
    /// Reads a copy of the database instead of the default location — for
    /// example a database copied from another host for offline analysis.
    #[structopt(long)]
    pub db: Option<String>,

    #[structopt(subcommand)]
    pub action: InterruptionAction,
}

#[derive(Debug, StructOpt, Clone)]
pub enum InterruptionAction {
    /// List interruption events with optional filters.
    ///
    /// Filters by time range, type, severity, agent, and resolved status.
    /// Default: last 24 hours, all types, limit 100.
    ///
    /// Examples:
    ///   agentsight interruption list --last 24
    ///   agentsight interruption list --last 48 --type llm_error --severity high --unresolved
    ///   agentsight interruption list --last 168 --agent qoder --json --limit 50
    List {
        /// Query last N hours (default: 24)
        #[structopt(long, default_value = "24")]
        last: u64,

        /// Filter by interruption type.
        /// Accepts every type the detector can produce; see the values listed below.
        #[structopt(long = "type", possible_values = interruption_type_values())]
        itype: Option<String>,

        /// Filter by severity level.
        /// Values: critical, high, medium, low
        #[structopt(long, possible_values = &["critical", "high", "medium", "low"])]
        severity: Option<String>,

        /// Filter by agent name (exact match)
        #[structopt(long)]
        agent: Option<String>,

        /// Show only unresolved events
        #[structopt(long, conflicts_with = "resolved")]
        unresolved: bool,

        /// Show only resolved events
        #[structopt(long, conflicts_with = "unresolved")]
        resolved: bool,

        /// Maximum number of results (default: 100, max: 1000)
        #[structopt(long, default_value = "100", parse(try_from_str = parse_list_limit))]
        limit: i64,

        /// Output as JSON (one JSON array)
        #[structopt(long)]
        json: bool,
    },

    /// Get a single interruption event by its ID.
    ///
    /// The interruption_id is a 32-character hex string.
    ///
    /// Example:
    ///   agentsight interruption get 0192a3b4c5d6e7f80192a3b4c5d6e7f8
    Get {
        /// The interruption_id (32-char hex)
        interruption_id: String,

        /// Output as JSON
        #[structopt(long)]
        json: bool,
    },

    /// Show per-type count statistics within a time range.
    ///
    /// Groups interruption events by (interruption_type, severity) and
    /// returns the count for each group.
    ///
    /// Example:
    ///   agentsight interruption stats --last 168
    ///   agentsight interruption stats --last 168 --agent qoder
    ///   agentsight interruption stats --last 168 --unresolved
    Stats {
        /// Query last N hours (default: 24)
        #[structopt(long, default_value = "24")]
        last: u64,

        /// Filter by agent name (exact match)
        #[structopt(long)]
        agent: Option<String>,

        /// Count only unresolved events
        #[structopt(long, conflicts_with = "resolved")]
        unresolved: bool,

        /// Count only resolved events
        #[structopt(long, conflicts_with = "unresolved")]
        resolved: bool,

        /// Output as JSON
        #[structopt(long)]
        json: bool,
    },

    /// Count unresolved interruptions grouped by severity.
    ///
    /// Returns total count and per-severity breakdown (critical/high/medium/low).
    ///
    /// Example:
    ///   agentsight interruption count --last 24
    ///   agentsight interruption count --last 24 --agent qoder
    Count {
        /// Query last N hours (default: 24)
        #[structopt(long, default_value = "24")]
        last: u64,

        /// Filter by agent name (exact match)
        #[structopt(long)]
        agent: Option<String>,

        /// Filter by interruption type.
        /// Accepts every type the detector can produce; see the values listed below.
        #[structopt(long = "type", possible_values = interruption_type_values())]
        itype: Option<String>,

        /// Output as JSON
        #[structopt(long)]
        json: bool,
    },

    /// List all interruption events for a specific session.
    ///
    /// Returns events ordered by occurred_at_ns ascending. Takes the same
    /// `--type` and `--severity` row filters `list` does, so one session can be
    /// narrowed without post-processing its `--json` array. There is no
    /// `--agent` here: the resolver derives a session from `(agent, pid,
    /// prompt)`, so one session already belongs to one agent.
    ///
    /// Example:
    ///   agentsight interruption session abc123-session-id
    ///   agentsight interruption session abc123-session-id --unresolved
    ///   agentsight interruption session abc123-session-id --type token_limit
    Session {
        /// The session_id to query
        session_id: String,

        /// Filter by interruption type.
        /// Accepts every type the detector can produce; see the values listed below.
        #[structopt(long = "type", possible_values = interruption_type_values())]
        itype: Option<String>,

        /// Filter by severity level.
        /// Values: critical, high, medium, low
        #[structopt(long, possible_values = &["critical", "high", "medium", "low"])]
        severity: Option<String>,

        /// Show only unresolved events
        #[structopt(long, conflicts_with = "resolved")]
        unresolved: bool,

        /// Show only resolved events
        #[structopt(long, conflicts_with = "unresolved")]
        resolved: bool,

        /// Output as JSON
        #[structopt(long)]
        json: bool,
    },

    /// List all interruption events for a specific conversation.
    ///
    /// Returns events ordered by occurred_at_ns ascending. The conversation_id
    /// corresponds to a single agent conversation, and the view takes the same
    /// `--type` and `--severity` row filters `list` does.
    ///
    /// Example:
    ///   agentsight interruption conversation abc123-conversation-id
    ///   agentsight interruption conversation abc123-conversation-id --severity high
    Conversation {
        /// The conversation_id to query
        conversation_id: String,

        /// Filter by interruption type.
        /// Accepts every type the detector can produce; see the values listed below.
        #[structopt(long = "type", possible_values = interruption_type_values())]
        itype: Option<String>,

        /// Filter by severity level.
        /// Values: critical, high, medium, low
        #[structopt(long, possible_values = &["critical", "high", "medium", "low"])]
        severity: Option<String>,

        /// Show only unresolved events
        #[structopt(long, conflicts_with = "resolved")]
        unresolved: bool,

        /// Show only resolved events
        #[structopt(long, conflicts_with = "unresolved")]
        resolved: bool,

        /// Output as JSON
        #[structopt(long)]
        json: bool,
    },

    /// Mark an interruption event as resolved.
    ///
    /// Sets resolved=1 in the database for the given interruption_id.
    ///
    /// Example:
    ///   agentsight interruption resolve 0192a3b4c5d6e7f80192a3b4c5d6e7f8
    Resolve {
        /// The interruption_id to resolve (32-char hex)
        interruption_id: String,
    },
}

impl InterruptionCommand {
    /// Resolve the database path: `--db` reads a copy of the database (a
    /// database copied from another host, or one in a non-default data
    /// directory); without it the default location is used.
    fn db_path(&self) -> std::path::PathBuf {
        self.db
            .as_deref()
            .map(std::path::PathBuf::from)
            .unwrap_or_else(default_db_path)
    }

    pub fn execute(&self) {
        let db_path = self.db_path();

        if let Err(error) = agentsight::check_data_file(&db_path) {
            eprintln!("{error}");
            std::process::exit(1);
        }

        let access = if matches!(&self.action, InterruptionAction::Resolve { .. }) {
            DatabaseAccess::ReadWrite
        } else {
            DatabaseAccess::ReadOnly
        };
        let manager = match DatabaseManager::new(
            DatabaseRole::Query,
            [DatabaseSpec::new(
                DatabaseId::Interruptions,
                &db_path,
                access,
                DatabaseCoverage::Full,
            )],
        ) {
            Ok(manager) => manager,
            Err(error) => {
                eprintln!("Error registering interruption database: {error}");
                std::process::exit(1);
            }
        };
        let opened = if access == DatabaseAccess::ReadWrite {
            manager.open_read_write(DatabaseId::Interruptions, InterruptionStore::new_with_path)
        } else {
            manager.open_read_only(
                DatabaseId::Interruptions,
                InterruptionStore::open_read_only_existing,
            )
        };
        let store = match opened {
            Ok(store) => store,
            Err(error) => {
                eprintln!("Error opening interruption database {db_path:?}: {error}");
                std::process::exit(1);
            }
        };

        match &self.action {
            InterruptionAction::List {
                last,
                itype,
                severity,
                agent,
                unresolved,
                resolved,
                limit,
                json,
            } => {
                let (start_ns, end_ns) = time_range_ns(*last);
                let resolved_filter = resolution_filter(*unresolved, *resolved);

                match store.list(
                    start_ns,
                    end_ns,
                    agent.as_deref(),
                    itype.as_deref(),
                    severity.as_deref(),
                    resolved_filter,
                    *limit,
                ) {
                    Ok(rows) => {
                        if *json {
                            print_json(&rows);
                        } else {
                            print_records_table(&rows);
                        }
                    }
                    Err(e) => {
                        eprintln!("Query error: {e}");
                        std::process::exit(1);
                    }
                }
            }

            InterruptionAction::Get {
                interruption_id,
                json,
            } => match store.get_by_id(interruption_id) {
                Ok(Some(record)) => {
                    if *json {
                        print_json(&record);
                    } else {
                        print_record_detail(&record);
                    }
                }
                Ok(None) => {
                    eprintln!("No interruption found with id: {interruption_id}");
                    std::process::exit(1);
                }
                Err(e) => {
                    eprintln!("Query error: {e}");
                    std::process::exit(1);
                }
            },

            InterruptionAction::Stats {
                last,
                agent,
                unresolved,
                resolved,
                json,
            } => {
                let (start_ns, end_ns) = time_range_ns(*last);
                let resolved_filter = resolution_filter(*unresolved, *resolved);
                match interruption_stats(
                    &store,
                    start_ns,
                    end_ns,
                    agent.as_deref(),
                    resolved_filter,
                ) {
                    Ok(stats) => {
                        if *json {
                            print_json(&stats);
                        } else {
                            if stats.is_empty() {
                                println!("No interruption events in the last {last} hour(s).");
                                return;
                            }
                            println!("{:<20} {:<10} {:>6}", "TYPE", "SEVERITY", "COUNT");
                            println!("{}", "-".repeat(40));
                            for s in &stats {
                                println!(
                                    "{:<20} {:<10} {:>6}",
                                    s.interruption_type, s.severity, s.count
                                );
                            }
                        }
                    }
                    Err(e) => {
                        eprintln!("Query error: {e}");
                        std::process::exit(1);
                    }
                }
            }

            InterruptionAction::Count {
                last,
                agent,
                itype,
                json,
            } => {
                let (start_ns, end_ns) = time_range_ns(*last);
                let (critical, high, medium, low) = match interruption_severity_counts(
                    &store,
                    start_ns,
                    end_ns,
                    agent.as_deref(),
                    itype.as_deref(),
                ) {
                    Ok(counts) => counts,
                    Err(e) => {
                        eprintln!("Query error: {e}");
                        std::process::exit(1);
                    }
                };
                let total = critical + high + medium + low;

                if *json {
                    let output = serde_json::json!({
                        "total": total,
                        "by_severity": {
                            "critical": critical,
                            "high": high,
                            "medium": medium,
                            "low": low,
                        }
                    });
                    println!("{}", serde_json::to_string_pretty(&output).unwrap());
                } else {
                    println!("Unresolved interruptions (last {last} hour(s)):");
                    println!();
                    println!("  Total:    {total}");
                    println!("  Critical: {critical}");
                    println!("  High:     {high}");
                    println!("  Medium:   {medium}");
                    println!("  Low:      {low}");
                }
            }

            InterruptionAction::Session {
                session_id,
                itype,
                severity,
                unresolved,
                resolved,
                json,
            } => {
                let resolved_filter = resolution_filter(*unresolved, *resolved);
                let row_filter = RowFilter {
                    itype: itype.as_deref(),
                    severity: severity.as_deref(),
                    resolved: resolved_filter,
                };
                match store.list_by_session(session_id) {
                    Ok(rows) => {
                        let rows: Vec<_> =
                            rows.into_iter().filter(|r| row_filter.keeps(r)).collect();
                        if *json {
                            print_json(&rows);
                        } else {
                            if rows.is_empty() {
                                println!("No interruptions for session: {session_id}");
                                return;
                            }
                            println!("Interruptions for session {session_id}:");
                            println!();
                            print_records_table(&rows);
                        }
                    }
                    Err(e) => {
                        eprintln!("Query error: {e}");
                        std::process::exit(1);
                    }
                }
            }

            InterruptionAction::Conversation {
                conversation_id,
                itype,
                severity,
                unresolved,
                resolved,
                json,
            } => {
                let resolved_filter = resolution_filter(*unresolved, *resolved);
                let row_filter = RowFilter {
                    itype: itype.as_deref(),
                    severity: severity.as_deref(),
                    resolved: resolved_filter,
                };
                match store.list_by_conversation(conversation_id) {
                    Ok(rows) => {
                        let rows: Vec<_> =
                            rows.into_iter().filter(|r| row_filter.keeps(r)).collect();
                        if *json {
                            print_json(&rows);
                        } else {
                            if rows.is_empty() {
                                println!("No interruptions for conversation: {conversation_id}");
                                return;
                            }
                            println!("Interruptions for conversation {conversation_id}:");
                            println!();
                            print_records_table(&rows);
                        }
                    }
                    Err(e) => {
                        eprintln!("Query error: {e}");
                        std::process::exit(1);
                    }
                }
            }

            InterruptionAction::Resolve { interruption_id } => {
                match store.resolve(interruption_id) {
                    Ok(true) => {
                        println!("Resolved: {interruption_id}");
                    }
                    Ok(false) => {
                        eprintln!("No interruption found with id: {interruption_id}");
                        std::process::exit(1);
                    }
                    Err(e) => {
                        eprintln!("Error resolving interruption: {e}");
                        std::process::exit(1);
                    }
                }
            }
        }
    }
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

/// Resolve the `--unresolved` / `--resolved` pair into a store filter.
///
/// `None` means "every event in the window", which stays the default for
/// `list` and `stats`; the flags are declared mutually exclusive, so at most
/// one of them is set.
fn resolution_filter(unresolved: bool, resolved: bool) -> Option<bool> {
    if unresolved {
        Some(false)
    } else if resolved {
        Some(true)
    } else {
        None
    }
}

/// The row filters the per-id views apply on top of their store call.
///
/// `list_by_session` / `list_by_conversation` take an id and nothing else, so
/// the two views narrow the rows the store already returned. `list` sends the
/// same three filters to SQL; keeping them in one predicate here is what makes
/// the per-id views agree with it instead of each re-deriving the rule (the
/// separation of the two arms is how `--resolved` first drifted from `list`).
#[derive(Clone, Copy)]
struct RowFilter<'a> {
    /// `--type`; the concrete type string the store column holds.
    itype: Option<&'a str>,
    /// `--severity`; one of critical/high/medium/low.
    severity: Option<&'a str>,
    /// `--unresolved` / `--resolved`; `None` keeps both.
    resolved: Option<bool>,
}

impl RowFilter<'_> {
    /// Whether one record survives every filter that was asked for.
    fn keeps(&self, record: &InterruptionRecord) -> bool {
        self.itype
            .is_none_or(|itype| record.interruption_type == itype)
            && self
                .severity
                .is_none_or(|severity| record.severity == severity)
            && self.resolved.is_none_or(|state| record.resolved == state)
    }

    /// How many of `records` survive; the arms and their tests share it so the
    /// two cannot disagree about what "kept" means.
    #[cfg(test)]
    fn keeps_each(&self, records: &[InterruptionRecord]) -> usize {
        records.iter().filter(|record| self.keeps(record)).count()
    }
}

/// Per-(type, severity) counts for a window, optionally scoped to one agent.
///
/// `resolved` stays `None` unless `--unresolved` / `--resolved` asks
/// otherwise: the CLI reports what happened in the window, resolved or not,
/// unlike the dashboard's unresolved-only card.
fn interruption_stats(
    store: &InterruptionStore,
    start_ns: i64,
    end_ns: i64,
    agent: Option<&str>,
    resolved: Option<bool>,
) -> Result<Vec<agentsight::storage::sqlite::InterruptionTypeStat>, Box<dyn std::error::Error>> {
    store.stats(start_ns, end_ns, resolved, agent)
}

/// Unresolved per-severity counts for a window, optionally scoped to one agent
/// and one interruption type.
///
/// `Some(false)` keeps the documented unresolved-only view; the agent filter
/// is the one `list --agent` already applies, and `itype` is the same column
/// `list --type` sends to SQL — the count had no way to ask it, so every
/// severity total covered all types no matter what the caller named.
fn interruption_severity_counts(
    store: &InterruptionStore,
    start_ns: i64,
    end_ns: i64,
    agent: Option<&str>,
    itype: Option<&str>,
) -> Result<(i64, i64, i64, i64), Box<dyn std::error::Error>> {
    let rows = store.list(start_ns, end_ns, agent, itype, None, Some(false), i64::MAX)?;
    let mut critical = 0i64;
    let mut high = 0i64;
    let mut medium = 0i64;
    let mut low = 0i64;
    for r in &rows {
        match r.severity.as_str() {
            "critical" => critical += 1,
            "high" => high += 1,
            "medium" => medium += 1,
            "low" => low += 1,
            _ => {}
        }
    }
    Ok((critical, high, medium, low))
}

/// Upper bound for `--limit`, mirroring `AuditStore::list_events`' documented
/// `1..=1000` range. The store itself accepts any positive LIMIT (the `count`
/// subcommand deliberately passes `i64::MAX`), so the bound lives on the flag.
const MAX_LIST_LIMIT: i64 = 1000;

/// Parse `--limit`: reject values below 1 with a clear error — SQLite reads a
/// negative LIMIT as "no limit", which would dump every matching event — and
/// cap oversized values at [`MAX_LIST_LIMIT`].
fn parse_list_limit(raw: &str) -> Result<i64, String> {
    let value: i64 = raw
        .parse()
        .map_err(|_| format!("--limit must be an integer, got {raw:?}"))?;
    if value < 1 {
        return Err(format!("--limit must be at least 1, got {value}"));
    }
    Ok(value.min(MAX_LIST_LIMIT))
}

/// Default database path for interruption events.
fn default_db_path() -> std::path::PathBuf {
    GenAISqliteStore::default_path()
        .parent()
        .unwrap_or(std::path::Path::new("/var/log/sysak/.agentsight"))
        .join("interruption_events.db")
}

/// Compute (start_ns, end_ns) for the last N hours from now.
fn time_range_ns(hours: u64) -> (i64, i64) {
    // A pre-epoch realtime clock degrades to 0 (the `epoch_nanos` family
    // contract) instead of unwrapping the elapsed-time error.
    let now_ns = agentsight::utils::epoch_nanos(std::time::SystemTime::now()) as i64;
    let start_ns = now_ns.saturating_sub(
        // Saturate so an absurd --last (up to u64::MAX) degrades to
        // "everything" (start far in the past) instead of overflowing into an
        // inverted or future-start window. Guard the u64->i64 cast first so a
        // value >= 2^63 cannot flip sign.
        (hours.min(i64::MAX as u64) as i64).saturating_mul(3_600_000_000_000),
    );
    (start_ns, now_ns)
}

/// Format nanosecond timestamp to human-readable datetime string.
fn format_ns(ns: i64) -> String {
    let secs = ns / 1_000_000_000;
    let nanos_rem = (ns % 1_000_000_000) as u32;
    // Simple UTC formatting without external crate
    let total_mins = secs / 60;
    let sec = secs % 60;
    let total_hours = total_mins / 60;
    let min = total_mins % 60;
    let total_days = total_hours / 24;
    let hour = total_hours % 24;

    // Days since epoch to Y-M-D (simplified)
    let (year, month, day) = days_to_ymd(total_days);
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03}",
        year,
        month,
        day,
        hour,
        min,
        sec,
        nanos_rem / 1_000_000
    )
}

/// Convert days since Unix epoch to (year, month, day).
fn days_to_ymd(days: i64) -> (i64, u32, u32) {
    // Algorithm from http://howardhinnant.github.io/date_algorithms.html
    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };
    (year, m as u32, d as u32)
}

/// Truncate a string ID for table display, appending "..." if needed.
/// Counts and cuts by character, not byte: stored ids are free text and a
/// byte-index cut can land inside a multi-byte character and panic.
fn truncate_id(s: &str, max_len: usize) -> String {
    if s.chars().count() > max_len {
        let kept: String = s.chars().take(max_len.saturating_sub(3)).collect();
        format!("{kept}...")
    } else {
        s.to_string()
    }
}

/// Print a slice of InterruptionRecord as a formatted table.
fn print_records_table(records: &[InterruptionRecord]) {
    if records.is_empty() {
        println!("No interruption events found.");
        return;
    }

    println!(
        "{:<34} {:<18} {:<10} {:<22} {:<10} {:<14} {:<16} CONVERSATION_ID",
        "INTERRUPTION_ID", "TYPE", "SEVERITY", "OCCURRED_AT", "RESOLVED", "AGENT", "SESSION_ID"
    );
    println!("{}", "-".repeat(140));

    for r in records {
        let resolved_str = if r.resolved { "yes" } else { "no" };
        let agent = r.agent_name.as_deref().unwrap_or("-");
        let session = truncate_id(r.session_id.as_deref().unwrap_or("-"), 14);
        let conversation = truncate_id(r.conversation_id.as_deref().unwrap_or("-"), 16);

        println!(
            "{:<34} {:<18} {:<10} {:<22} {:<10} {:<14} {:<16} {}",
            r.interruption_id,
            r.interruption_type,
            r.severity,
            format_ns(r.occurred_at_ns),
            resolved_str,
            agent,
            session,
            conversation,
        );
    }

    println!();
    println!("Total: {} event(s)", records.len());
}

/// Print a single InterruptionRecord in detail.
fn print_record_detail(r: &InterruptionRecord) {
    println!("Interruption Event Detail");
    println!("{}", "=".repeat(60));
    println!("  ID:           {}", r.interruption_id);
    println!("  Type:         {}", r.interruption_type);
    println!("  Severity:     {}", r.severity);
    println!(
        "  Occurred At:  {} ({}ns)",
        format_ns(r.occurred_at_ns),
        r.occurred_at_ns
    );
    println!("  Resolved:     {}", if r.resolved { "yes" } else { "no" });
    println!("  Session ID:   {}", r.session_id.as_deref().unwrap_or("-"));
    println!(
        "  Conversation: {}",
        r.conversation_id.as_deref().unwrap_or("-")
    );
    println!("  Trace ID:     {}", r.trace_id.as_deref().unwrap_or("-"));
    println!("  Call ID:      {}", r.call_id.as_deref().unwrap_or("-"));
    println!(
        "  PID:          {}",
        r.pid
            .map(|p| p.to_string())
            .unwrap_or_else(|| "-".to_string())
    );
    println!("  Agent:        {}", r.agent_name.as_deref().unwrap_or("-"));
    if let Some(ref detail) = r.detail {
        // Pretty-print JSON detail
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(detail) {
            println!("  Detail:");
            println!(
                "{}",
                serde_json::to_string_pretty(&v).unwrap_or_else(|_| detail.clone())
            );
        } else {
            println!("  Detail:       {detail}");
        }
    }
}

/// Print any Serialize value as JSON.
fn print_json<T: serde::Serialize>(value: &T) {
    match serde_json::to_string_pretty(value) {
        Ok(s) => println!("{s}"),
        Err(e) => {
            eprintln!("JSON serialization error: {e}");
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_id_cuts_on_char_boundaries() {
        // 16 chars / 48 bytes: a byte-index cut at 11 lands inside a character
        // and panics. The limit is a character budget.
        let id = "会话会话会话会话会话会话会话会话";
        let out = truncate_id(id, 14);
        assert!(out.ends_with("..."));
        assert_eq!(out.chars().count(), 14);

        // Short multi-byte ids pass through untouched.
        assert_eq!(truncate_id("会话", 14), "会话");

        // ASCII behaviour is unchanged.
        assert_eq!(truncate_id("abcdefghij", 14), "abcdefghij");
        assert_eq!(truncate_id("abcdefghijklmnop", 14), "abcdefghijk...");
    }

    #[test]
    fn list_limit_rejects_negative_and_caps_oversized() {
        // SQLite reads a negative LIMIT as "no limit", so `--limit=-1` used
        // to dump every matching event; the flag must not reach the store
        // as-is (the sibling AuditStore::list_events clamps to 1..=1000).
        let negative = InterruptionCommand::from_iter_safe(["interruption", "list", "--limit=-1"]);
        let err = negative.expect_err("a negative limit must be rejected");
        assert!(
            err.to_string().contains("--limit"),
            "the error must name the flag: {err}"
        );

        let oversized =
            InterruptionCommand::from_iter_safe(["interruption", "list", "--limit", "100000"])
                .expect("an oversized limit still parses");
        match oversized.action {
            InterruptionAction::List { limit, .. } => {
                assert_eq!(limit, 1000, "an oversized limit must be capped")
            }
            other => panic!("expected List, got {other:?}"),
        }
    }

    #[test]
    fn db_path_honours_the_override_and_falls_back_to_the_default() {
        // `--db` must be accepted in the documented position (before the
        // subcommand) and reach the query path: reverting the wiring
        // (returning the default unconditionally) makes this fail.
        let plain = InterruptionCommand::from_iter_safe(["interruption", "list"])
            .expect("a plain invocation still parses");
        assert_eq!(plain.db_path(), default_db_path());

        let overridden = InterruptionCommand::from_iter_safe([
            "interruption",
            "--db",
            "/tmp/agentsight_interruption_override.db",
            "list",
        ])
        .expect("--db must parse before the subcommand, as documented");
        assert_eq!(
            overridden.db_path(),
            std::path::PathBuf::from("/tmp/agentsight_interruption_override.db")
        );
    }

    #[test]
    fn time_range_never_inverts_for_absurd_last() {
        // A requested window far wider than the recorded history must degrade
        // to "everything", not overflow into an inverted or future-start
        // window. Without the saturating guard `u64::MAX as i64 == -1` pushes
        // start one hour into the future.
        let (start, end) = time_range_ns(u64::MAX);
        assert!(start <= end, "start {start} must not exceed end {end}");

        // 3_000_000 hours (~342 years) overflows i64 nanoseconds; the window
        // must still be ordered (debug builds abort on the multiply).
        let (start, end) = time_range_ns(3_000_000);
        assert!(start <= end, "start {start} must not exceed end {end}");

        // A normal window is unaffected.
        let (s2, e2) = time_range_ns(24);
        assert!(s2 < e2);
        assert_eq!(e2 - s2, 24 * 3600 * 1_000_000_000);
    }

    /// A read-only store holding one resolved and one unresolved interruption.
    fn stats_store(name: &str) -> (InterruptionStore, std::path::PathBuf) {
        use agentsight::interruption::{InterruptionEvent, InterruptionType, Severity};

        let path = std::env::temp_dir().join(format!(
            "agentsight_interruption_stats_{name}_{}.db",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let store = InterruptionStore::new_with_path(&path).expect("create store");
        for (index, resolved) in [false, true].into_iter().enumerate() {
            let mut event = InterruptionEvent::new(
                InterruptionType::LlmError,
                Some(format!("sess-{index}")),
                None,
                Some(format!("conv-{index}")),
                None,
                Some(4242),
                Some("TestAgent".to_string()),
                1_000_000_000,
                None,
            );
            event.severity = Severity::High;
            event.resolved = resolved;
            store.insert(&event).expect("insert event");
        }
        let read_only =
            InterruptionStore::open_read_only_existing(&path).expect("reopen read-only");
        (read_only, path)
    }

    /// One unresolved `token_limit` (medium) and one unresolved `agent_crash`
    /// (critical), so a per-severity count can tell the two types apart.
    fn mixed_type_store(name: &str) -> (InterruptionStore, std::path::PathBuf) {
        use agentsight::interruption::{InterruptionEvent, InterruptionType, Severity};

        let path = std::env::temp_dir().join(format!(
            "agentsight_interruption_mixed_{name}_{}.db",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let store = InterruptionStore::new_with_path(&path).expect("create store");
        for (index, (itype, severity)) in [
            (InterruptionType::TokenLimit, Severity::Medium),
            (InterruptionType::AgentCrash, Severity::Critical),
        ]
        .into_iter()
        .enumerate()
        {
            let mut event = InterruptionEvent::new(
                itype,
                Some(format!("sess-mixed-{index}")),
                None,
                Some(format!("conv-mixed-{index}")),
                None,
                Some(4242),
                Some("TestAgent".to_string()),
                1_000_000_000,
                None,
            );
            event.severity = severity;
            event.interruption_id = format!("int-mixed-{index}");
            store.insert(&event).expect("insert event");
        }
        let read_only =
            InterruptionStore::open_read_only_existing(&path).expect("reopen read-only");
        (read_only, path)
    }

    /// `--unresolved` / `--resolved` on `stats` used to be unexpressible: the
    /// helper passed `None` for the resolution filter no matter what, so the
    /// per-type table always mixed solved and unsolved events. The store has
    /// accepted the filter all along; only the CLI wiring was missing.
    #[test]
    fn stats_honours_the_resolution_filters() {
        let (store, path) = stats_store("resolution");
        let (start_ns, end_ns) = (0, i64::MAX);

        let unresolved = interruption_stats(&store, start_ns, end_ns, None, Some(false))
            .expect("unresolved stats");
        assert_eq!(
            unresolved.iter().map(|s| s.count).sum::<i64>(),
            1,
            "--unresolved must leave out the resolved event: {unresolved:?}"
        );

        let resolved =
            interruption_stats(&store, start_ns, end_ns, None, Some(true)).expect("resolved stats");
        assert_eq!(
            resolved.iter().map(|s| s.count).sum::<i64>(),
            1,
            "--resolved must leave out the unresolved event: {resolved:?}"
        );

        // Without either flag both events still count, as before.
        let all = interruption_stats(&store, start_ns, end_ns, None, None).expect("all stats");
        assert_eq!(all.iter().map(|s| s.count).sum::<i64>(), 2);

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn stats_parses_the_resolution_flags_exclusively() {
        let unresolved =
            InterruptionCommand::from_iter_safe(["interruption", "stats", "--unresolved"])
                .expect("--unresolved parses");
        match unresolved.action {
            InterruptionAction::Stats {
                unresolved,
                resolved,
                ..
            } => {
                assert!(unresolved);
                assert!(!resolved);
            }
            other => panic!("expected Stats, got {other:?}"),
        }

        let resolved = InterruptionCommand::from_iter_safe(["interruption", "stats", "--resolved"])
            .expect("--resolved parses");
        match resolved.action {
            InterruptionAction::Stats {
                unresolved,
                resolved,
                ..
            } => {
                assert!(!unresolved);
                assert!(resolved);
            }
            other => panic!("expected Stats, got {other:?}"),
        }

        // The siblings `list` already rejects both flags together; `stats`
        // must not silently pick one.
        let both = InterruptionCommand::from_iter_safe([
            "interruption",
            "stats",
            "--unresolved",
            "--resolved",
        ]);
        assert!(both.is_err(), "the two flags are mutually exclusive");
    }

    /// `session` and `conversation` are the two per-id views, and like `list`
    /// and `stats` they used to print every event for the id — a reader
    /// checking whether a session still had anything open had to eyeball the
    /// RESOLVED column. The store's `list_by_session` / `list_by_conversation`
    /// take no filter, so the CLI narrows the rows it already returned.
    #[test]
    fn session_and_conversation_filter_by_resolution_state() {
        use agentsight::interruption::{InterruptionEvent, InterruptionType, Severity};

        let path = std::env::temp_dir().join(format!(
            "agentsight_interruption_session_filter_{}.db",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let store = InterruptionStore::new_with_path(&path).expect("create store");
        for (index, resolved) in [false, true].into_iter().enumerate() {
            let mut event = InterruptionEvent::new(
                InterruptionType::LlmError,
                Some("sess-shared".to_string()),
                None,
                Some("conv-shared".to_string()),
                None,
                Some(4242),
                Some("TestAgent".to_string()),
                1_000_000_000,
                None,
            );
            event.severity = Severity::High;
            event.resolved = resolved;
            event.interruption_id = format!("int-session-filter-{index}");
            store.insert(&event).expect("insert event");
        }

        let by_session = store.list_by_session("sess-shared").expect("session rows");
        assert_eq!(by_session.len(), 2, "the store returns both states");

        // The arms narrow with `RowFilter`; reverting it to an
        // unconditional keep makes these counts 2.
        let keep = |rows: &[InterruptionRecord], want: Option<bool>| -> usize {
            RowFilter {
                itype: None,
                severity: None,
                resolved: want,
            }
            .keeps_each(rows)
        };
        assert_eq!(keep(&by_session, Some(false)), 1, "--unresolved");
        assert_eq!(keep(&by_session, Some(true)), 1, "--resolved");

        let by_conversation = store
            .list_by_conversation("conv-shared")
            .expect("conversation rows");
        assert_eq!(by_conversation.len(), 2);
        assert_eq!(keep(&by_conversation, Some(false)), 1);

        let _ = std::fs::remove_file(&path);
    }

    /// `--type` and `--severity` are the other two row filters `list` applies,
    /// and the per-id views rejected them: `session <id> --severity high` was a
    /// usage error, so narrowing one session to its critical events meant
    /// dumping `--json` and filtering by hand. Each filter is asserted on its
    /// own and together, so a dropped dimension cannot pass on the other's
    /// evidence.
    #[test]
    fn session_and_conversation_filter_by_type_and_severity() {
        use agentsight::interruption::{InterruptionEvent, InterruptionType, Severity};

        let path = std::env::temp_dir().join(format!(
            "agentsight_interruption_session_row_filter_{}.db",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let store = InterruptionStore::new_with_path(&path).expect("create store");
        for (index, (itype, severity)) in [
            (InterruptionType::LlmError, Severity::High),
            (InterruptionType::TokenLimit, Severity::Medium),
            (InterruptionType::AgentCrash, Severity::Critical),
        ]
        .into_iter()
        .enumerate()
        {
            let mut event = InterruptionEvent::new(
                itype,
                Some("sess-rows".to_string()),
                None,
                Some("conv-rows".to_string()),
                None,
                Some(4242),
                Some("TestAgent".to_string()),
                1_000_000_000,
                None,
            );
            event.severity = severity;
            event.interruption_id = format!("int-row-filter-{index}");
            store.insert(&event).expect("insert event");
        }

        let rows = store.list_by_session("sess-rows").expect("session rows");
        assert_eq!(rows.len(), 3, "the store returns every type and severity");
        let kept = |itype: Option<&str>, severity: Option<&str>| -> usize {
            RowFilter {
                itype,
                severity,
                resolved: None,
            }
            .keeps_each(&rows)
        };

        // Each filter alone.
        assert_eq!(kept(Some("token_limit"), None), 1, "--type");
        assert_eq!(kept(None, Some("high")), 1, "--severity");
        // An unmatched value yields nothing instead of falling back to all
        // rows, the same contract `list` has.
        assert_eq!(kept(Some("dead_loop"), None), 0, "unknown type");
        assert_eq!(kept(None, Some("low")), 0, "absent severity");
        // Both together intersect rather than union.
        assert_eq!(kept(Some("llm_error"), Some("high")), 1);
        assert_eq!(
            kept(Some("llm_error"), Some("critical")),
            0,
            "a row must satisfy every filter that was asked for"
        );
        // No filter still keeps every row.
        assert_eq!(kept(None, None), 3);

        let by_conversation = store.list_by_conversation("conv-rows").expect("conv rows");
        assert_eq!(
            RowFilter {
                itype: Some("agent_crash"),
                severity: Some("critical"),
                resolved: None,
            }
            .keeps_each(&by_conversation),
            1,
            "the conversation arm shares the predicate"
        );

        let _ = std::fs::remove_file(&path);
    }

    /// Both flags parse on the two per-id subcommands and stay exclusive.
    #[test]
    fn session_and_conversation_parse_the_resolution_flags() {
        for command in ["session", "conversation"] {
            let one = InterruptionCommand::from_iter_safe([
                "interruption",
                command,
                "abc",
                "--unresolved",
            ])
            .unwrap_or_else(|e| panic!("{command} --unresolved must parse: {e}"));
            match one.action {
                InterruptionAction::Session { unresolved, .. }
                | InterruptionAction::Conversation { unresolved, .. } => assert!(unresolved),
                other => panic!("unexpected action {other:?}"),
            }

            let both = InterruptionCommand::from_iter_safe([
                "interruption",
                command,
                "abc",
                "--unresolved",
                "--resolved",
            ]);
            assert!(both.is_err(), "{command} must reject both flags together");
        }
    }

    /// `count` publishes per-severity totals, but its store call hardcoded the
    /// type filter to `None`: every severity total covered all types, so a
    /// caller asking "how many token_limit hits are open" had no way to ask it
    /// and had to pull `list --type token_limit --json` and sum by hand.
    #[test]
    fn count_filters_by_type() {
        use agentsight::interruption::{InterruptionType, Severity};

        let (store, path) = mixed_type_store("count_type");

        let all = interruption_severity_counts(&store, 0, i64::MAX, None, None)
            .expect("count every type");
        assert_eq!(all, (1, 0, 1, 0), "both types are counted without --type");

        // Reverting the wiring (back to a hardcoded `None`) makes this the
        // `all` tuple instead: (1, 0, 1, 0) rather than (0, 0, 1, 0).
        let token_limit =
            interruption_severity_counts(&store, 0, i64::MAX, None, Some("token_limit"))
                .expect("count token_limit");
        assert_eq!(
            token_limit,
            (0, 0, 1, 0),
            "only the token_limit row survives the type filter"
        );

        let crash = interruption_severity_counts(&store, 0, i64::MAX, None, Some("agent_crash"))
            .expect("count agent_crash");
        assert_eq!(crash, (1, 0, 0, 0), "only the agent_crash row survives");

        // A type nobody recorded is zero everywhere, not the unfiltered total.
        let absent = interruption_severity_counts(&store, 0, i64::MAX, None, Some("dead_loop"))
            .expect("count an absent type");
        assert_eq!(absent, (0, 0, 0, 0), "an absent type counts zero, not all");

        // The store spells the type the way `InterruptionType::as_str` does;
        // keep the fixture honest about that so the values above are not
        // passing by accident on a different spelling.
        assert_eq!(InterruptionType::TokenLimit.as_str(), "token_limit");
        assert_eq!(InterruptionType::AgentCrash.as_str(), "agent_crash");
        assert_eq!(
            store
                .list(0, i64::MAX, None, Some("token_limit"), None, None, i64::MAX)
                .expect("list the token_limit rows")
                .len(),
            1,
            "the fixture holds one token_limit row"
        );
        let _ = Severity::Medium;

        let _ = std::fs::remove_file(&path);
    }

    /// `count --type` parses with the same closed value set `list --type` uses,
    /// so a typo is a usage error rather than a filter that silently keeps
    /// everything.
    #[test]
    fn count_parses_the_type_filter() {
        let parsed =
            InterruptionCommand::from_iter_safe(["interruption", "count", "--type", "token_limit"])
                .expect("count --type must parse");
        match parsed.action {
            InterruptionAction::Count { itype, .. } => {
                assert_eq!(itype.as_deref(), Some("token_limit"));
            }
            other => panic!("expected Count, got {other:?}"),
        }

        let bare = InterruptionCommand::from_iter_safe(["interruption", "count"])
            .expect("bare count must parse");
        match bare.action {
            InterruptionAction::Count { itype, .. } => assert!(itype.is_none()),
            other => panic!("expected Count, got {other:?}"),
        }

        assert!(
            InterruptionCommand::from_iter_safe(["interruption", "count", "--type", "not_a_type"])
                .is_err(),
            "count --type must reject an unknown type, like list's"
        );
    }

    /// The per-id views accept the `--type` / `--severity` pair `list` does,
    /// with the same closed value sets. A typo has to be a usage error rather
    /// than a silently ignored filter, which is the contract `list` already
    /// enforces through `possible_values`.
    #[test]
    fn session_and_conversation_parse_the_type_and_severity_filters() {
        for command in ["session", "conversation"] {
            let parsed = InterruptionCommand::from_iter_safe([
                "interruption",
                command,
                "abc",
                "--type",
                "token_limit",
                "--severity",
                "medium",
            ])
            .unwrap_or_else(|e| panic!("{command} --type/--severity must parse: {e}"));
            match parsed.action {
                InterruptionAction::Session {
                    itype, severity, ..
                }
                | InterruptionAction::Conversation {
                    itype, severity, ..
                } => {
                    assert_eq!(itype.as_deref(), Some("token_limit"));
                    assert_eq!(severity.as_deref(), Some("medium"));
                }
                other => panic!("unexpected action {other:?}"),
            }

            for (flag, bad) in [("--type", "not_a_type"), ("--severity", "urgent")] {
                let rejected = InterruptionCommand::from_iter_safe([
                    "interruption",
                    command,
                    "abc",
                    flag,
                    bad,
                ]);
                assert!(
                    rejected.is_err(),
                    "{command} {flag} {bad} must be rejected, like list's"
                );
            }
        }
    }
}
