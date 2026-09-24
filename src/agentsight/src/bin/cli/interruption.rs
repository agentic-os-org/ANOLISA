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

        /// Maximum number of results (default: 100)
        #[structopt(long, default_value = "100")]
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
    Stats {
        /// Query last N hours (default: 24)
        #[structopt(long, default_value = "24")]
        last: u64,

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
    Count {
        /// Query last N hours (default: 24)
        #[structopt(long, default_value = "24")]
        last: u64,

        /// Output as JSON
        #[structopt(long)]
        json: bool,
    },

    /// List all interruption events for a specific session.
    ///
    /// Returns events ordered by occurred_at_ns ascending.
    ///
    /// Example:
    ///   agentsight interruption session abc123-session-id
    Session {
        /// The session_id to query
        session_id: String,

        /// Output as JSON
        #[structopt(long)]
        json: bool,
    },

    /// List all interruption events for a specific conversation.
    ///
    /// Returns events ordered by occurred_at_ns ascending.
    /// The conversation_id corresponds to a single agent conversation.
    ///
    /// Example:
    ///   agentsight interruption conversation abc123-conversation-id
    Conversation {
        /// The conversation_id to query
        conversation_id: String,

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
    pub fn execute(&self) {
        let db_path = default_db_path();

        if !db_path.exists() {
            eprintln!("Database file not found: {db_path:?}");
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
                let resolved_filter = if *unresolved {
                    Some(false)
                } else if *resolved {
                    Some(true)
                } else {
                    None
                };

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

            InterruptionAction::Stats { last, json } => {
                let (start_ns, end_ns) = time_range_ns(*last);
                // `None`: the CLI reports what happened in the window,
                // resolved or not, unlike the dashboard's unresolved-only card.
                match store.stats(start_ns, end_ns, None) {
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

            InterruptionAction::Count { last, json } => {
                let (start_ns, end_ns) = time_range_ns(*last);
                // Use the detailed session-level query to compute total by severity
                match store.list(start_ns, end_ns, None, None, None, Some(false), i64::MAX) {
                    Ok(rows) => {
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
                    Err(e) => {
                        eprintln!("Query error: {e}");
                        std::process::exit(1);
                    }
                }
            }

            InterruptionAction::Session { session_id, json } => {
                match store.list_by_session(session_id) {
                    Ok(rows) => {
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
                json,
            } => match store.list_by_conversation(conversation_id) {
                Ok(rows) => {
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
            },

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

/// Default database path for interruption events.
fn default_db_path() -> std::path::PathBuf {
    GenAISqliteStore::default_path()
        .parent()
        .unwrap_or(std::path::Path::new("/var/log/sysak/.agentsight"))
        .join("interruption_events.db")
}

/// Compute (start_ns, end_ns) for the last N hours from now.
fn time_range_ns(hours: u64) -> (i64, i64) {
    use std::time::{SystemTime, UNIX_EPOCH};
    let now_ns = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos() as i64;
    let start_ns = now_ns - (hours as i64) * 3600 * 1_000_000_000;
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
fn truncate_id(s: &str, max_len: usize) -> String {
    if s.len() > max_len {
        format!("{}...", &s[..max_len.saturating_sub(3)])
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
