//! Summary subcommand — one unified, glanceable cross-component overview.
//!
//! AgentSight and tokenless already record three independent observability
//! streams, each behind its own subcommand / database. `summary` rolls up the
//! headline numbers from all of them into a single report:
//!
//!   * Sessions & token usage    (genai_events.db)
//!   * Interruptions by severity (interruption_events.db)
//!   * Tokenless savings         (~/.tokenless/stats.db)
//!
//! Each data source degrades **independently**: a missing or unreadable
//! database contributes zeros (and, for tokenless, an explicit "not
//! available" note) rather than aborting the whole report. `summary` always
//! exits 0 with whatever is available, so it is safe to run on a fresh box.

use agentsight::storage::sqlite::tokenless::default_stats_path;
use agentsight::storage::sqlite::{
    GenAISqliteStore, InterruptionStore, TokenlessStatsStore, TokenlessWindowSummary, format_tokens,
};
use std::path::Path;
use structopt::StructOpt;

/// Unified summary of agent activity over a recent time window.
#[derive(Debug, StructOpt, Clone)]
pub struct SummaryCommand {
    /// Query the last N hours (default: 24)
    #[structopt(long, default_value = "24")]
    pub last: u64,

    /// Output as JSON
    #[structopt(long)]
    pub json: bool,
}

/// Sessions + token totals from genai_events.db.
#[derive(Default)]
struct SessionStats {
    count: usize,
    input_tokens: i64,
    output_tokens: i64,
}

impl SessionStats {
    fn total_tokens(&self) -> i64 {
        self.input_tokens + self.output_tokens
    }
}

/// Interruption tallies grouped by severity.
#[derive(Default)]
struct InterruptionStats {
    total: usize,
    critical: usize,
    high: usize,
    medium: usize,
    low: usize,
}

impl SummaryCommand {
    pub fn execute(&self) {
        let (start_ns, end_ns) = time_range_ns(self.last);

        let sessions = gather_sessions(&GenAISqliteStore::default_path(), start_ns, end_ns);
        let interruptions = gather_interruptions(&interruption_db_path(), start_ns, end_ns);
        let (tokenless, tokenless_available) =
            gather_tokenless(&default_stats_path(), start_ns, end_ns);

        if self.json {
            self.print_json(&sessions, &interruptions, &tokenless, tokenless_available);
        } else {
            self.print_text(&sessions, &interruptions, &tokenless, tokenless_available);
        }
    }

    fn print_text(
        &self,
        sessions: &SessionStats,
        interruptions: &InterruptionStats,
        tokenless: &TokenlessWindowSummary,
        tokenless_available: bool,
    ) {
        println!("AgentSight Summary (last {}h)", self.last);
        println!();

        println!("Sessions      {}", sessions.count);
        if sessions.count > 0 {
            println!(
                "  Tokens      {} in / {} out / {} total",
                format_tokens(sessions.input_tokens.max(0) as u64),
                format_tokens(sessions.output_tokens.max(0) as u64),
                format_tokens(sessions.total_tokens().max(0) as u64),
            );
        }
        println!();

        println!("Interruptions {}", interruptions.total);
        if interruptions.total > 0 {
            println!("  critical    {}", interruptions.critical);
            println!("  high        {}", interruptions.high);
            println!("  medium      {}", interruptions.medium);
            println!("  low         {}", interruptions.low);
        }
        println!();

        if !tokenless_available {
            println!("Tokenless     not available (stats.db not found)");
        } else if tokenless.records > 0 {
            println!(
                "Tokenless     {:.0}% saved ({} -> {}, {} ops)",
                tokenless.saved_percent(),
                format_tokens(tokenless.before_tokens.max(0) as u64),
                format_tokens(tokenless.after_tokens.max(0) as u64),
                tokenless.records,
            );
        } else {
            println!("Tokenless     no activity in window");
        }
    }

    fn print_json(
        &self,
        sessions: &SessionStats,
        interruptions: &InterruptionStats,
        tokenless: &TokenlessWindowSummary,
        tokenless_available: bool,
    ) {
        let output = build_json(
            self.last,
            sessions,
            interruptions,
            tokenless,
            tokenless_available,
        );
        println!("{}", serde_json::to_string_pretty(&output).unwrap());
    }
}

/// Build the JSON report value. Kept separate from `print_json` so the output
/// contract — in particular the clamp policy that keeps it identical to the
/// text view — is unit-testable without spawning the binary.
fn build_json(
    window_hours: u64,
    sessions: &SessionStats,
    interruptions: &InterruptionStats,
    tokenless: &TokenlessWindowSummary,
    tokenless_available: bool,
) -> serde_json::Value {
    serde_json::json!({
        "window_hours": window_hours,
        "sessions": {
            "count": sessions.count,
            // Clamp to match the text path (and the saved_tokens policy) so the
            // two output modes never disagree on the same data.
            "input_tokens": sessions.input_tokens.max(0),
            "output_tokens": sessions.output_tokens.max(0),
            "total_tokens": sessions.total_tokens().max(0),
        },
        "interruptions": {
            "total": interruptions.total,
            "critical": interruptions.critical,
            "high": interruptions.high,
            "medium": interruptions.medium,
            "low": interruptions.low,
        },
        "tokenless": {
            "available": tokenless_available,
            "records": tokenless.records,
            "before_tokens": tokenless.before_tokens.max(0),
            "after_tokens": tokenless.after_tokens.max(0),
            "saved_tokens": tokenless.saved_tokens(),
            "saved_percent": tokenless.saved_percent(),
        },
    })
}

// ─── Data gathering (each source degrades independently) ─────────────────────

/// Sessions and token totals from genai_events.db. Returns zeros if the
/// database is absent or unreadable.
fn gather_sessions(path: &Path, start_ns: i64, end_ns: i64) -> SessionStats {
    if !path.exists() {
        return SessionStats::default();
    }
    let store = match GenAISqliteStore::new_with_path(
        path,
        agentsight::config::InsertStoragePolicy::default(),
    ) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("warning: cannot open genai database: {e}");
            return SessionStats::default();
        }
    };
    let sessions = match store.list_sessions(start_ns, end_ns, true) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("warning: cannot query sessions: {e}");
            return SessionStats::default();
        }
    };
    let mut stats = SessionStats {
        count: sessions.len(),
        ..Default::default()
    };
    for s in &sessions {
        stats.input_tokens += s.total_input_tokens;
        stats.output_tokens += s.total_output_tokens;
    }
    stats
}

/// Interruptions tallied by severity from interruption_events.db.
///
/// `resolved_filter = None` deliberately counts **every** interruption in the
/// window, resolved or not: a summary reports what happened during the window,
/// not just the still-open items (which is what `interruption count` shows).
fn gather_interruptions(path: &Path, start_ns: i64, end_ns: i64) -> InterruptionStats {
    if !path.exists() {
        return InterruptionStats::default();
    }
    let store = match InterruptionStore::new_with_path(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("warning: cannot open interruption database: {e}");
            return InterruptionStats::default();
        }
    };
    let rows = match store.list(start_ns, end_ns, None, None, None, None, i64::MAX) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("warning: cannot query interruptions: {e}");
            return InterruptionStats::default();
        }
    };
    let mut stats = InterruptionStats {
        total: rows.len(),
        ..Default::default()
    };
    for r in &rows {
        match r.severity.as_str() {
            "critical" => stats.critical += 1,
            "high" => stats.high += 1,
            "medium" => stats.medium += 1,
            "low" => stats.low += 1,
            _ => {}
        }
    }
    stats
}

/// Tokenless savings within the window, plus whether stats.db was found.
fn gather_tokenless(path: &Path, start_ns: i64, end_ns: i64) -> (TokenlessWindowSummary, bool) {
    match TokenlessStatsStore::open_if_exists(path) {
        Some(store) => (store.summary_in_window(start_ns, end_ns), true),
        None => (TokenlessWindowSummary::default(), false),
    }
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

/// interruption_events.db lives next to the genai database.
fn interruption_db_path() -> std::path::PathBuf {
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
    // Saturate so an absurd --last (up to u64::MAX) degrades to "everything"
    // (start far in the past) instead of overflowing into an inverted/garbage
    // window. Guard the u64->i64 cast first so a value >= 2^63 cannot flip sign.
    let hours_ns = (hours.min(i64::MAX as u64) as i64).saturating_mul(3_600_000_000_000);
    let start_ns = now_ns.saturating_sub(hours_ns);
    (start_ns, now_ns)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn json_clamps_negative_tokens_to_match_text() {
        // Corrupt/negative external data must render identically in JSON and
        // text (both clamp to 0). Reverting the clamp makes this fail.
        let sessions = SessionStats {
            count: 1,
            input_tokens: -5,
            output_tokens: 3,
        };
        let tokenless = TokenlessWindowSummary {
            records: 1,
            before_tokens: -10,
            after_tokens: 2,
        };
        let v = build_json(
            24,
            &sessions,
            &InterruptionStats::default(),
            &tokenless,
            true,
        );

        assert_eq!(v["sessions"]["input_tokens"], 0);
        assert_eq!(v["sessions"]["output_tokens"], 3);
        assert_eq!(v["sessions"]["total_tokens"], 0); // (-5 + 3) -> -2 -> 0
        assert_eq!(v["tokenless"]["before_tokens"], 0);
        assert_eq!(v["tokenless"]["after_tokens"], 2);
    }

    #[test]
    fn json_preserves_normal_values() {
        let sessions = SessionStats {
            count: 2,
            input_tokens: 100,
            output_tokens: 40,
        };
        let v = build_json(
            12,
            &sessions,
            &InterruptionStats::default(),
            &TokenlessWindowSummary::default(),
            false,
        );
        assert_eq!(v["window_hours"], 12);
        assert_eq!(v["sessions"]["count"], 2);
        assert_eq!(v["sessions"]["total_tokens"], 140);
        assert_eq!(v["tokenless"]["available"], false);
    }

    #[test]
    fn time_range_never_inverts_for_absurd_last() {
        // u64::MAX hours must not overflow into an inverted (start > end) or
        // future-start window. Without the saturating guard this regresses:
        // `u64::MAX as i64 == -1` would push start one hour into the future.
        let (start, end) = time_range_ns(u64::MAX);
        assert!(start <= end, "start {start} must not exceed end {end}");
        // A normal window is strictly ordered and non-empty.
        let (s2, e2) = time_range_ns(24);
        assert!(s2 < e2);
    }

    // ── Independent degradation matrix ──────────────────────────────────────
    // Each source must contribute zeros (tokenless: unavailable) when its DB is
    // missing or has the wrong schema, without panicking — so one broken source
    // never aborts the whole report. The genai/interruption DB paths are
    // hardcoded in production, so these inject paths directly (the live ECS
    // collector holds the real DBs open, so they cannot be moved to E2E this).

    fn unique_tmp(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "test_summary_{tag}_{}.db",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    /// A db file that exists but has the wrong schema (no expected table).
    fn wrong_schema_db(tag: &str) -> std::path::PathBuf {
        let path = unique_tmp(tag);
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute("CREATE TABLE unrelated (x INTEGER)", [])
            .unwrap();
        path
    }

    #[test]
    fn gather_sessions_degrades_to_zero() {
        let missing = unique_tmp("sess_missing");
        let s = gather_sessions(&missing, 0, i64::MAX);
        assert_eq!(s.count, 0);
        assert_eq!(s.input_tokens, 0);

        let bad = wrong_schema_db("sess_badschema");
        let s2 = gather_sessions(&bad, 0, i64::MAX);
        assert_eq!(s2.count, 0, "wrong-schema genai db must degrade, not panic");
        let _ = std::fs::remove_file(&bad);
    }

    #[test]
    fn gather_interruptions_degrades_to_zero() {
        let missing = unique_tmp("int_missing");
        let i = gather_interruptions(&missing, 0, i64::MAX);
        assert_eq!(i.total, 0);

        let bad = wrong_schema_db("int_badschema");
        let i2 = gather_interruptions(&bad, 0, i64::MAX);
        assert_eq!(i2.total, 0, "wrong-schema interruption db must degrade");
        let _ = std::fs::remove_file(&bad);
    }

    #[test]
    fn gather_tokenless_reports_availability() {
        // Missing file -> not available.
        let missing = unique_tmp("tl_missing");
        let (sum, avail) = gather_tokenless(&missing, 0, i64::MAX);
        assert!(!avail, "missing stats.db must be reported unavailable");
        assert_eq!(sum.records, 0);

        // Present but wrong schema -> available (file exists) but zeroed.
        let bad = wrong_schema_db("tl_badschema");
        let (sum2, avail2) = gather_tokenless(&bad, 0, i64::MAX);
        assert!(
            avail2,
            "an existing stats.db file is 'available' even if unreadable"
        );
        assert_eq!(
            sum2.records, 0,
            "wrong-schema stats db must degrade to zero"
        );
        let _ = std::fs::remove_file(&bad);
    }

    #[test]
    fn execute_runs_both_output_modes_without_panicking() {
        // execute() wires gather_* + print_* against the hardcoded default DB
        // paths. On a box without those DBs every source degrades to zero and
        // the report still renders (exit 0). This drives the execute() dispatch
        // and the interruption_db_path() helper end to end.
        SummaryCommand {
            last: 24,
            json: false,
        }
        .execute();
        SummaryCommand {
            last: 1,
            json: true,
        }
        .execute();
    }

    #[test]
    fn print_paths_render_every_branch() {
        // Drive the populated branches of print_text/print_json that the zero
        // path can't reach: the token line, the per-severity lines, and the
        // tokenless saved/unavailable/no-activity variants. The numeric
        // contract itself is asserted in the build_json tests above; here we
        // exercise the formatting branches so a regression that panics or drops
        // a branch is caught.
        let populated_sessions = SessionStats {
            count: 3,
            input_tokens: 1000,
            output_tokens: 500,
        };
        let populated_ints = InterruptionStats {
            total: 4,
            critical: 1,
            high: 1,
            medium: 1,
            low: 1,
        };
        let populated_tl = TokenlessWindowSummary {
            records: 5,
            before_tokens: 2000,
            after_tokens: 800,
        };

        let text_cmd = SummaryCommand {
            last: 24,
            json: false,
        };
        // Saved-activity + non-zero sessions/interruptions branches.
        text_cmd.print_text(&populated_sessions, &populated_ints, &populated_tl, true);
        // tokenless-unavailable branch.
        text_cmd.print_text(
            &SessionStats::default(),
            &InterruptionStats::default(),
            &TokenlessWindowSummary::default(),
            false,
        );
        // tokenless-available-but-no-activity branch.
        text_cmd.print_text(
            &SessionStats::default(),
            &InterruptionStats::default(),
            &TokenlessWindowSummary::default(),
            true,
        );

        SummaryCommand {
            last: 24,
            json: true,
        }
        .print_json(&populated_sessions, &populated_ints, &populated_tl, true);
    }
}
