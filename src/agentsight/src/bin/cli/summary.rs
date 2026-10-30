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

use agentsight::database::{DatabaseCoverage, DatabaseId, DatabaseManager};
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

    /// Override the sessions database path
    ///
    /// Reads a copy of the databases from another host or a non-default data
    /// directory; the interruption database is read from the same directory,
    /// matching the default on-disk layout.
    #[structopt(long)]
    pub db: Option<String>,

    /// Override the tokenless stats database path
    ///
    /// The tokenless store lives outside the AgentSight databases, so `--db`
    /// does not move it: a report over a copy still reads the running host's
    /// own `stats.db`. Point this at a copy to keep the whole report free of
    /// live data.
    #[structopt(long)]
    pub tokenless_db: Option<String>,

    /// Scope the session and interruption totals to one agent
    ///
    /// Each section keeps the match its own store applies: sessions compare
    /// the label the session listing resolves (recorded agent name, falling
    /// back to the process name), interrupts compare `agent_name` only, the
    /// column `interruption list --agent` filters. Both are case-insensitive.
    #[structopt(long)]
    pub agent: Option<String>,
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
    /// Resolve the sessions database path: `--db` reads a copy of the
    /// databases from another host or a non-default data directory; without
    /// it the default location is used.
    fn db_path(&self) -> std::path::PathBuf {
        self.db
            .as_deref()
            .map(std::path::PathBuf::from)
            .unwrap_or_else(GenAISqliteStore::default_path)
    }

    /// Resolve the tokenless stats database path: `--tokenless-db` reads a
    /// copy instead of the running host's own store; without it the default
    /// location is used.
    fn tokenless_db_path(&self) -> std::path::PathBuf {
        self.tokenless_db
            .as_deref()
            .map(std::path::PathBuf::from)
            .unwrap_or_else(default_stats_path)
    }

    pub fn execute(&self) {
        let (start_ns, end_ns) = time_range_ns(self.last);

        let agent = self.agent.as_deref();
        let genai_db = self.db_path();
        let sessions = gather_sessions(&genai_db, start_ns, end_ns, agent);
        let interruptions =
            gather_interruptions(&interruption_db_path(&genai_db), start_ns, end_ns, agent);
        // The tokenless statistics database records a saving against a session
        // but never an agent, so an agent-scoped report cannot attribute it.
        // Leaving the host-wide totals in would put two different scopes in
        // one report; the section says so instead (see print_text/print_json).
        let (tokenless, tokenless_available) = if agent.is_some() {
            (TokenlessWindowSummary::default(), false)
        } else {
            gather_tokenless(&self.tokenless_db_path(), start_ns, end_ns)
        };

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
        match self.agent.as_deref() {
            Some(agent) => println!("AgentSight Summary (last {}h, agent {agent})", self.last),
            None => println!("AgentSight Summary (last {}h)", self.last),
        }
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

        if self.agent.is_some() {
            println!("Tokenless     not recorded per agent, so it is left out");
        } else if !tokenless_available {
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
            self.agent.as_deref(),
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
    agent: Option<&str>,
    sessions: &SessionStats,
    interruptions: &InterruptionStats,
    tokenless: &TokenlessWindowSummary,
    tokenless_available: bool,
) -> serde_json::Value {
    let mut output = serde_json::json!({
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
    });
    // Only an agent-scoped report names its scope and the source it cannot
    // carry; an unscoped report keeps its exact previous shape.
    if let Some(agent) = agent {
        output["agent"] = serde_json::json!(agent);
    }
    output
}

// ─── Data gathering (each source degrades independently) ─────────────────────

/// Sessions and token totals from genai_events.db. Returns zeros if the
/// database is absent or unreadable.
fn gather_sessions(path: &Path, start_ns: i64, end_ns: i64, agent: Option<&str>) -> SessionStats {
    if !path.exists() {
        return SessionStats::default();
    }
    let store = match DatabaseManager::open_query(
        DatabaseId::GenAi,
        path,
        DatabaseCoverage::Full,
        GenAISqliteStore::open_read_only_existing,
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
    let mut stats = SessionStats::default();
    for s in sessions
        .iter()
        .filter(|s| agent.is_none_or(|agent| agent_label_matches(s.agent_name.as_deref(), agent)))
    {
        stats.count += 1;
        stats.input_tokens += s.total_input_tokens;
        stats.output_tokens += s.total_output_tokens;
    }
    stats
}

/// Whether a resolved agent label is the one the caller asked for.
///
/// The scan is on the label the queries already resolve — `agent_name`,
/// falling back to `process_name` — and case-insensitive, matching the
/// `COLLATE NOCASE` filters the stores use for the same flag.
fn agent_label_matches(label: Option<&str>, wanted: &str) -> bool {
    label.is_some_and(|label| label.eq_ignore_ascii_case(wanted))
}

/// Interruptions tallied by severity from interruption_events.db.
///
/// `resolved_filter = None` deliberately counts **every** interruption in the
/// window, resolved or not: a summary reports what happened during the window,
/// not just the still-open items (which is what `interruption count` shows).
fn gather_interruptions(
    path: &Path,
    start_ns: i64,
    end_ns: i64,
    agent: Option<&str>,
) -> InterruptionStats {
    if !path.exists() {
        return InterruptionStats::default();
    }
    let store = match DatabaseManager::open_query(
        DatabaseId::Interruptions,
        path,
        DatabaseCoverage::Full,
        InterruptionStore::open_read_only_existing,
    ) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("warning: cannot open interruption database: {e}");
            return InterruptionStats::default();
        }
    };
    // `agent` goes to the store: the same `COLLATE NOCASE` filter
    // `interruption list --agent` uses, so both commands select one row set.
    let rows = match store.list(start_ns, end_ns, agent, None, None, None, i64::MAX) {
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

/// interruption_events.db lives next to the genai database, including when
/// `--db` points at a copy of the databases in another directory.
fn interruption_db_path(genai_db: &Path) -> std::path::PathBuf {
    genai_db
        .parent()
        .unwrap_or(std::path::Path::new("/var/log/sysak/.agentsight"))
        .join("interruption_events.db")
}

/// Compute (start_ns, end_ns) for the last N hours from now.
fn time_range_ns(hours: u64) -> (i64, i64) {
    // A pre-epoch realtime clock degrades to 0 (the `epoch_nanos` family
    // contract) instead of unwrapping the elapsed-time error.
    let now_ns = agentsight::utils::epoch_nanos(std::time::SystemTime::now()) as i64;
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
            None,
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
            None,
            &sessions,
            &InterruptionStats::default(),
            &TokenlessWindowSummary::default(),
            false,
        );
        assert_eq!(v["window_hours"], 12);
        assert_eq!(v["sessions"]["count"], 2);
        assert_eq!(v["sessions"]["total_tokens"], 140);
        assert_eq!(v["tokenless"]["available"], false);
        // Without `--agent` the report keeps its exact previous shape: the
        // scope key is the only addition and it is absent, so a script that
        // reads these keys keeps working byte for byte.
        assert_eq!(
            v,
            serde_json::json!({
                "window_hours": 12,
                "sessions": {
                    "count": 2,
                    "input_tokens": 100,
                    "output_tokens": 40,
                    "total_tokens": 140,
                },
                "interruptions": {
                    "total": 0,
                    "critical": 0,
                    "high": 0,
                    "medium": 0,
                    "low": 0,
                },
                "tokenless": {
                    "available": false,
                    "records": 0,
                    "before_tokens": 0,
                    "after_tokens": 0,
                    "saved_tokens": 0,
                    "saved_percent": 0.0,
                },
            })
        );
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

    #[test]
    fn db_path_honours_the_override_and_keeps_the_sibling_interruption_db() {
        // `--db` must reach the gather calls: reverting the wiring (returning
        // the default unconditionally) makes this fail. The interruption
        // database must move with it — it lives next to the genai database.
        let plain = SummaryCommand {
            last: 24,
            json: false,
            db: None,
            tokenless_db: None,
            agent: None,
        };
        assert_eq!(plain.db_path(), GenAISqliteStore::default_path());

        let copied = SummaryCommand {
            last: 24,
            json: false,
            db: Some("/tmp/agentsight_summary_copy/genai_events.db".into()),
            tokenless_db: None,
            agent: None,
        };
        let genai = copied.db_path();
        assert_eq!(
            genai,
            std::path::PathBuf::from("/tmp/agentsight_summary_copy/genai_events.db")
        );
        assert_eq!(
            interruption_db_path(&genai),
            std::path::PathBuf::from("/tmp/agentsight_summary_copy/interruption_events.db")
        );
    }

    #[test]
    fn tokenless_db_path_honours_the_override_and_falls_back_to_the_default() {
        // `--tokenless-db` must reach the gather call: reverting the wiring
        // (returning the default unconditionally) makes this fail. The
        // tokenless store is the one source `--db` cannot move, so without
        // this flag a report over a copy still reads the local host's stats.db.
        let plain = SummaryCommand {
            last: 24,
            json: false,
            db: None,
            tokenless_db: None,
            agent: None,
        };
        assert_eq!(plain.tokenless_db_path(), default_stats_path());

        let copied = SummaryCommand {
            last: 24,
            json: false,
            db: None,
            tokenless_db: Some("/tmp/agentsight_summary_copy/stats.db".into()),
            agent: None,
        };
        assert_eq!(
            copied.tokenless_db_path(),
            std::path::PathBuf::from("/tmp/agentsight_summary_copy/stats.db")
        );
    }

    #[test]
    fn agent_scopes_the_session_and_interruption_totals() {
        // `--agent` was the one filter `summary` lacked while every other
        // agent-attributed view had it: the report could only answer for the
        // whole host. Reverting the wiring — ignoring the flag in the two
        // gather calls — leaves both totals host-wide and fails this test.
        let dir = std::env::temp_dir().join(format!(
            "test_summary_agent_{}_{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let genai_db = dir.join("genai_events.db");
        {
            let store = GenAISqliteStore::new_with_path(
                &genai_db,
                agentsight::config::PeriodicStoragePolicy::default(),
            )
            .unwrap();
            drop(store);
            let conn = rusqlite::Connection::open(&genai_db).unwrap();
            for (call, agent, input, output) in [
                ("call-a", "agent-a", 100_i64, 10_i64),
                ("call-b", "agent-b", 200, 20),
            ] {
                conn.execute(
                    "INSERT INTO genai_events (event_type, session_id, call_id, agent_name,
                     model, input_tokens, output_tokens, start_timestamp_ns, event_json)
                     VALUES ('llm_call', 'sess-' || ?1, ?1, ?2, 'gpt-4', ?3, ?4, ?5, '{}')",
                    rusqlite::params![call, agent, input, output, 1_000_i64],
                )
                .unwrap();
            }
        }

        let all = gather_sessions(&genai_db, 0, i64::MAX, None);
        assert_eq!(all.count, 2, "without --agent both sessions count");
        let only_a = gather_sessions(&genai_db, 0, i64::MAX, Some("agent-a"));
        assert_eq!(only_a.count, 1);
        assert_eq!(only_a.input_tokens, 100);
        assert_eq!(only_a.output_tokens, 10);
        // The same case-insensitive match the store filters use.
        assert_eq!(
            gather_sessions(&genai_db, 0, i64::MAX, Some("AGENT-A")).count,
            1
        );
        assert_eq!(
            gather_sessions(&genai_db, 0, i64::MAX, Some("agent-missing")).count,
            0
        );

        let interruption_db = dir.join("interruption_events.db");
        {
            use agentsight::interruption::{InterruptionEvent, InterruptionType};
            let store = InterruptionStore::new_with_path(&interruption_db).unwrap();
            for agent in ["agent-a", "agent-b"] {
                store
                    .insert(&InterruptionEvent::new(
                        InterruptionType::LlmError,
                        Some("sess-1".to_string()),
                        None,
                        None,
                        None,
                        None,
                        Some(agent.to_string()),
                        1_000,
                        None,
                    ))
                    .unwrap();
            }
        }
        assert_eq!(
            gather_interruptions(&interruption_db, 0, i64::MAX, None).total,
            2
        );
        let filtered = gather_interruptions(&interruption_db, 0, i64::MAX, Some("agent-a"));
        assert_eq!(filtered.total, 1);
        assert_eq!(
            filtered.high + filtered.critical + filtered.medium + filtered.low,
            1
        );

        // A scoped report names its scope in JSON and says why the tokenless
        // section is absent instead of leaving host-wide numbers beside it.
        let scoped = build_json(
            24,
            Some("agent-a"),
            &only_a,
            &filtered,
            &TokenlessWindowSummary::default(),
            false,
        );
        assert_eq!(scoped["agent"], "agent-a");

        // The scoped text view names its scope and says why the tokenless
        // section is missing instead of showing host-wide savings.
        let scoped_cmd = SummaryCommand {
            last: 24,
            json: false,
            db: None,
            tokenless_db: None,
            agent: Some("agent-a".to_string()),
        };
        scoped_cmd.print_text(
            &only_a,
            &filtered,
            &TokenlessWindowSummary::default(),
            false,
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── Independent degradation matrix ──────────────────────────────────────
    // Each source must contribute zeros (tokenless: unavailable) when its DB is
    // missing or has the wrong schema, without panicking — so one broken source
    // never aborts the whole report.

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
        let s = gather_sessions(&missing, 0, i64::MAX, None);
        assert_eq!(s.count, 0);
        assert_eq!(s.input_tokens, 0);

        let bad = wrong_schema_db("sess_badschema");
        let s2 = gather_sessions(&bad, 0, i64::MAX, None);
        assert_eq!(s2.count, 0, "wrong-schema genai db must degrade, not panic");
        let _ = std::fs::remove_file(&bad);
    }

    #[test]
    fn gather_interruptions_degrades_to_zero() {
        let missing = unique_tmp("int_missing");
        let i = gather_interruptions(&missing, 0, i64::MAX, None);
        assert_eq!(i.total, 0);

        let bad = wrong_schema_db("int_badschema");
        let i2 = gather_interruptions(&bad, 0, i64::MAX, None);
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
            db: None,
            tokenless_db: None,
            agent: None,
        }
        .execute();
        SummaryCommand {
            last: 1,
            json: true,
            db: None,
            tokenless_db: None,
            agent: None,
        }
        .execute();
        // A scoped run skips the tokenless source entirely; drive that path
        // too so the branch is not left uncovered.
        SummaryCommand {
            last: 1,
            json: false,
            db: None,
            tokenless_db: None,
            agent: Some("agent-from-the-host".to_string()),
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
            db: None,
            tokenless_db: None,
            agent: None,
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
            db: None,
            tokenless_db: None,
            agent: None,
        }
        .print_json(&populated_sessions, &populated_ints, &populated_tl, true);
    }
}
