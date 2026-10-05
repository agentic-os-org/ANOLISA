//! Audit query subcommand

use agentsight::database::{DatabaseCoverage, DatabaseId, DatabaseManager};
use agentsight::{AuditEventType, AuditStore, SqliteConfig};
use anyhow::Context;
use structopt::StructOpt;

/// Audit query subcommand
#[derive(Debug, StructOpt, Clone)]
pub struct AuditCommand {
    /// Query last N hours (e.g. 24)
    #[structopt(long)]
    pub last: Option<u64>,

    /// Filter by PID
    #[structopt(long)]
    pub pid: Option<u32>,

    /// Filter by event type: "llm" or "process"
    #[structopt(long = "type")]
    pub event_type: Option<String>,

    /// Output as JSON
    #[structopt(long)]
    pub json: bool,

    /// Show summary statistics
    #[structopt(long)]
    pub summary: bool,

    /// Hide process_action events whose command/args contain any of these
    /// substrings. Repeatable. Useful for filtering shell-startup noise, e.g.
    /// `--exclude "command -v" --exclude grepconf`. The hidden count is reported.
    #[structopt(long)]
    pub exclude: Vec<String>,
}

/// Resolve the optional `--type` filter into an [`AuditEventType`].
///
/// Unknown values are rejected instead of silently ignored: a typo like
/// `--type llm_calls` used to fall through to "no filter" and return every
/// audit event with exit code 0, which is worse than an error for a
/// machine-facing command (`token --period` and `interruption --type` reject
/// invalid values the same way, via structopt possible_values).
fn resolve_event_type(raw: Option<&str>) -> Result<Option<AuditEventType>, String> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    raw.parse::<AuditEventType>().map(Some).map_err(|_| {
        format!(
            "invalid --type '{raw}': valid values are 'llm' (llm_call) and \
             'process' (process_action)"
        )
    })
}

impl AuditCommand {
    pub fn execute(&self) {
        let db_path = SqliteConfig::default().db_path();

        if !db_path.exists() {
            eprintln!("Database file not found: {db_path:?}");
            std::process::exit(1);
        }

        let store = match DatabaseManager::open_query(
            DatabaseId::Primary,
            &db_path,
            DatabaseCoverage::Full,
            |path| AuditStore::open_read_only_existing(path, "audit_events"),
        ) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("Failed to open audit database {db_path:?}: {e}");
                std::process::exit(1);
            }
        };

        let event_type = match resolve_event_type(self.event_type.as_deref()) {
            Ok(event_type) => event_type,
            Err(message) => {
                eprintln!("{message}");
                std::process::exit(1);
            }
        };

        if let Err(error) = self.run(&store, event_type) {
            eprintln!("{error:#}");
            std::process::exit(1);
        }
    }

    /// Run the requested query and report whether it succeeded.
    ///
    /// A query that fails used to print to stderr and exit 0, so a script (or
    /// `--json`) could not tell it apart from "no events in the window": the
    /// output was empty either way. `execute` maps the error to a non-zero
    /// exit, the same way the open and `--type` failures above do.
    fn run(&self, store: &AuditStore, event_type: Option<AuditEventType>) -> anyhow::Result<()> {
        if self.summary {
            if self.pid.is_some() {
                // The summary aggregates the whole window and has no pid
                // filter; saying so beats returning unfiltered totals that
                // look like they came from `--pid`. Mirrors the `--exclude`
                // note below.
                eprintln!("Note: --pid is not applied to --summary.");
            }
            if !self.exclude.is_empty() {
                eprintln!(
                    "Note: --exclude is not applied to --summary (summary always reflects the full dataset)."
                );
            }
            return self.print_summary(store);
        }

        if let Some(pid) = self.pid {
            self.query_by_pid(store, pid, event_type)
        } else {
            self.query_by_time(store, event_type)
        }
    }

    fn query_by_time(
        &self,
        store: &AuditStore,
        event_type: Option<AuditEventType>,
    ) -> anyhow::Result<()> {
        let hours = self.last.unwrap_or(24);
        let since_ns = super::hours_ago_ns(hours);

        match store.query_since(since_ns, event_type) {
            Ok(records) => {
                self.output_records(&records, &format!("Last {hours} hours"));
                Ok(())
            }
            Err(e) => Err(e).context("Query failed"),
        }
    }

    fn query_by_pid(
        &self,
        store: &AuditStore,
        pid: u32,
        event_type: Option<AuditEventType>,
    ) -> anyhow::Result<()> {
        let records = self
            .query_pid_records(store, pid, event_type)
            .context("Query failed")?;
        let hours = self.last.unwrap_or(24);
        self.output_records(&records, &format!("PID {pid}, last {hours} hours"));
        Ok(())
    }

    /// Resolve the `--last` window and fetch the pid's records.
    ///
    /// Split out of [`Self::query_by_pid`] so tests can assert on the rows the
    /// command would print: the pid path used to drop `--last` entirely and
    /// return every row ever recorded for the pid.
    fn query_pid_records(
        &self,
        store: &AuditStore,
        pid: u32,
        event_type: Option<AuditEventType>,
    ) -> anyhow::Result<Vec<agentsight::AuditRecord>> {
        let hours = self.last.unwrap_or(24);
        let since_ns = super::hours_ago_ns(hours);
        store.query_by_pid(pid, since_ns, event_type)
    }

    fn is_excluded(&self, record: &agentsight::AuditRecord) -> bool {
        use agentsight::AuditExtra;
        if self.exclude.is_empty() {
            return false;
        }
        if let AuditExtra::ProcessAction { filename, args, .. } = &record.extra {
            let fname = filename.as_deref().unwrap_or("");
            let a = args.as_deref().unwrap_or("");
            return self
                .exclude
                .iter()
                .filter(|p| !p.trim().is_empty())
                .any(|p| fname.contains(p.as_str()) || a.contains(p.as_str()));
        }
        false
    }

    fn output_records(&self, records: &[agentsight::AuditRecord], scope: &str) {
        let total = records.len();
        let filtered: Vec<&agentsight::AuditRecord> =
            records.iter().filter(|r| !self.is_excluded(r)).collect();
        let hidden = total - filtered.len();

        if self.json {
            let json_records: Vec<serde_json::Value> = filtered
                .iter()
                .map(|r| {
                    serde_json::json!({
                        "id": r.id,
                        "event_type": r.event_type.to_string(),
                        "timestamp_ns": r.timestamp_ns,
                        "pid": r.pid,
                        "ppid": r.ppid,
                        "comm": r.comm,
                        "duration_ns": r.duration_ns,
                        "extra": r.extra,
                        "session_id": r.session_id,
                    })
                })
                .collect();
            println!("{}", serde_json::to_string_pretty(&json_records).unwrap());
            if hidden > 0 {
                eprintln!(
                    "{} events hidden by --exclude ({} shown, {} total)",
                    hidden,
                    filtered.len(),
                    total
                );
            }
        } else {
            if hidden > 0 {
                println!(
                    "{}: {} audit events ({} hidden by --exclude)",
                    scope,
                    filtered.len(),
                    hidden
                );
            } else {
                println!("{}: {} audit events", scope, filtered.len());
            }
            println!();
            for record in &filtered {
                let json_record = serde_json::json!({
                    "id": record.id,
                    "event_type": record.event_type.to_string(),
                    "timestamp_ns": record.timestamp_ns,
                    "pid": record.pid,
                    "ppid": record.ppid,
                    "comm": record.comm,
                    "duration_ns": record.duration_ns,
                    "extra": record.extra,
                    "session_id": record.session_id,
                });
                println!("{}", serde_json::to_string(&json_record).unwrap());
            }
        }
    }

    fn print_summary(&self, store: &AuditStore) -> anyhow::Result<()> {
        let hours = self.last.unwrap_or(24);
        let since_ns = super::hours_ago_ns(hours);

        match store.summary(since_ns) {
            Ok(summary) => {
                if self.json {
                    println!("{}", serde_json::to_string_pretty(&summary).unwrap());
                } else {
                    println!("=== Audit Summary (last {hours} hours) ===");
                    println!();
                    println!("LLM calls:        {}", summary.total_llm_calls);
                    println!("Process actions:  {}", summary.total_process_actions);

                    if !summary.providers.is_empty() {
                        println!();
                        println!("Providers:");
                        for (provider, count) in &summary.providers {
                            println!("  {provider}: {count} calls");
                        }
                    }

                    if !summary.top_commands.is_empty() {
                        println!();
                        println!("Top commands:");
                        for (cmd, count) in &summary.top_commands {
                            println!("  {cmd}: {count} times");
                        }
                    }
                }
                Ok(())
            }
            Err(e) => Err(e).context("Summary query failed"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentsight::{AuditEventType, AuditExtra, AuditRecord};

    fn proc_record(filename: &str, args: &str) -> AuditRecord {
        AuditRecord {
            id: None,
            event_type: AuditEventType::ProcessAction,
            timestamp_ns: 0,
            pid: 1,
            ppid: None,
            comm: "test".into(),
            duration_ns: 0,
            extra: AuditExtra::ProcessAction {
                filename: Some(filename.into()),
                args: Some(args.into()),
                exit_code: None,
            },
            session_id: None,
        }
    }

    fn cmd(exclude: Vec<String>, json: bool) -> AuditCommand {
        AuditCommand {
            last: None,
            pid: None,
            event_type: None,
            json,
            summary: false,
            exclude,
        }
    }

    #[test]
    fn is_excluded_matches_filename_or_args() {
        let c = cmd(vec!["grepconf".to_string()], false);
        // filename contains the pattern -> excluded
        assert!(c.is_excluded(&proc_record("/usr/bin/grepconf", "")));
        // args contain the pattern -> excluded
        assert!(c.is_excluded(&proc_record("/bin/sh", "-c grepconf -V")));
        // neither contains the pattern -> NOT excluded
        assert!(!c.is_excluded(&proc_record("/usr/bin/node", "server.js")));
    }

    #[test]
    fn is_excluded_is_false_without_patterns() {
        let c = cmd(vec![], false);
        assert!(!c.is_excluded(&proc_record("/usr/bin/grepconf", "")));
    }

    #[test]
    fn output_records_runs_filter_both_formats() {
        let records = vec![
            proc_record("/usr/bin/grepconf", ""),   // excluded
            proc_record("/usr/bin/node", "app.js"), // kept
        ];
        // Exercise both branches (text + json); each hides 1 record, covering
        // the filtered / hidden-count paths in output_records.
        cmd(vec!["grepconf".to_string()], false).output_records(&records, "test");
        cmd(vec!["grepconf".to_string()], true).output_records(&records, "test");
    }

    #[test]
    fn resolve_event_type_accepts_absence_and_both_aliases() {
        assert!(matches!(resolve_event_type(None), Ok(None)));
        for raw in ["llm", "llm_call"] {
            assert!(
                matches!(
                    resolve_event_type(Some(raw)),
                    Ok(Some(AuditEventType::LlmCall))
                ),
                "{raw} must resolve to LlmCall"
            );
        }
        for raw in ["process", "process_action"] {
            assert!(
                matches!(
                    resolve_event_type(Some(raw)),
                    Ok(Some(AuditEventType::ProcessAction))
                ),
                "{raw} must resolve to ProcessAction"
            );
        }
    }

    #[test]
    fn resolve_event_type_rejects_unknown_values() {
        // Before this guard a typo like "llm_calls" was silently dropped and
        // the query returned BOTH event types with exit code 0 — worse than an
        // error for a machine-facing command.
        for raw in ["llm_calls", "processes", ""] {
            let Err(error) = resolve_event_type(Some(raw)) else {
                panic!("{raw:?} must be rejected, not silently treated as no filter");
            };
            assert!(
                error.contains("--type"),
                "{raw:?}: error must name the flag: {error}"
            );
            assert!(
                error.contains("llm") && error.contains("process"),
                "{raw:?}: error must list the valid values: {error}"
            );
        }
    }

    /// A read-only store whose `audit_events` table holds two `llm_call` rows
    /// for pid 4242: one a minute old and one 30 days old. Returns the recent
    /// timestamp so tests can assert which row survived the window.
    fn pid_window_store(name: &str) -> (AuditStore, std::path::PathBuf, u64) {
        use std::time::{SystemTime, UNIX_EPOCH};

        let now_ns = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock after Unix epoch")
            .as_nanos() as u64;
        let recent_ns = now_ns - 60 * 1_000_000_000;
        let old_ns = now_ns - 30 * 24 * 3_600_000_000_000;

        let path = std::env::temp_dir().join(format!(
            "agentsight_audit_pid_window_{name}_{}.db",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        {
            let conn = rusqlite::Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE audit_events (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    event_type TEXT NOT NULL,
                    timestamp_ns INTEGER NOT NULL,
                    pid INTEGER NOT NULL,
                    ppid INTEGER,
                    comm TEXT NOT NULL,
                    duration_ns INTEGER DEFAULT 0,
                    extra TEXT,
                    session_id TEXT
                );",
            )
            .unwrap();
            let extra = serde_json::json!({
                "provider": "openai",
                "input_tokens": 1,
                "output_tokens": 1,
                "cache_creation_tokens": 0,
                "cache_read_tokens": 0,
                "is_sse": true,
            })
            .to_string();
            for timestamp_ns in [recent_ns, old_ns] {
                conn.execute(
                    "INSERT INTO audit_events
                     (event_type, timestamp_ns, pid, comm, duration_ns, extra)
                     VALUES ('llm_call', ?1, 4242, 'agent', 0, ?2)",
                    rusqlite::params![timestamp_ns as i64, extra],
                )
                .unwrap();
            }
        }
        let store = AuditStore::open_read_only_existing(&path, "audit_events").unwrap();
        (store, path, recent_ns)
    }

    #[test]
    fn pid_query_honours_last_window() {
        let (store, path, recent_ns) = pid_window_store("window");
        // The CLI dropped `self.last` on the pid path and the store query had
        // no timestamp bound, so `--pid 4242 --last 1` returned the 30-day-old
        // row too.
        let mut c = cmd(vec![], false);
        c.pid = Some(4242);
        c.last = Some(1);
        let records = c.query_pid_records(&store, 4242, None).unwrap();
        let _ = std::fs::remove_file(&path);
        assert_eq!(
            records.len(),
            1,
            "--pid must only return rows inside the --last window"
        );
        assert_eq!(records[0].timestamp_ns, recent_ns);
    }

    /// An `audit_events` table that exists but lacks the columns the queries
    /// select: opening succeeds (the store only asserts the table exists) and
    /// every query fails, like an old or damaged database does.
    fn stale_schema_store(name: &str) -> (AuditStore, std::path::PathBuf) {
        let path = std::env::temp_dir().join(format!(
            "agentsight_stale_audit_{name}_{}.db",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        {
            let conn = rusqlite::Connection::open(&path).unwrap();
            conn.execute_batch("CREATE TABLE audit_events (id INTEGER PRIMARY KEY);")
                .unwrap();
        }
        let store = AuditStore::open_read_only_existing(&path, "audit_events").unwrap();
        (store, path)
    }

    #[test]
    fn query_failures_surface_instead_of_exiting_zero() {
        // A failed query used to print to stderr and exit 0, so a script (or
        // `--json`) could not tell it apart from "no events in the window".
        let c = cmd(vec![], true);
        let (store, path) = stale_schema_store("query");
        let result = c.run(&store, None);
        let _ = std::fs::remove_file(&path);
        assert!(
            result.is_err(),
            "a failed audit query must be reported as an error"
        );
    }

    #[test]
    fn summary_failures_surface_instead_of_exiting_zero() {
        let mut c = cmd(vec![], true);
        c.summary = true;
        let (store, path) = stale_schema_store("summary");
        let result = c.run(&store, None);
        let _ = std::fs::remove_file(&path);
        assert!(
            result.is_err(),
            "a failed audit summary must be reported as an error"
        );
    }
}
