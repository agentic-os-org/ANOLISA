//! Audit query subcommand

use agentsight::database::{DatabaseCoverage, DatabaseId, DatabaseManager};
use agentsight::{AuditEventType, AuditStore, SqliteConfig};
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

        let event_type = self
            .event_type
            .as_ref()
            .and_then(|t| t.parse::<AuditEventType>().ok());

        if self.summary {
            if !self.exclude.is_empty() {
                eprintln!(
                    "Note: --exclude is not applied to --summary (summary always reflects the full dataset)."
                );
            }
            self.print_summary(&store);
            return;
        }

        if let Some(pid) = self.pid {
            self.query_by_pid(&store, pid, event_type);
        } else {
            self.query_by_time(&store, event_type);
        }
    }

    fn query_by_time(&self, store: &AuditStore, event_type: Option<AuditEventType>) {
        let hours = self.last.unwrap_or(24);
        let since_ns = super::hours_ago_ns(hours);

        match store.query_since(since_ns, event_type) {
            Ok(records) => self.output_records(&records, &format!("Last {hours} hours")),
            Err(e) => eprintln!("Query failed: {e}"),
        }
    }

    fn query_by_pid(&self, store: &AuditStore, pid: u32, event_type: Option<AuditEventType>) {
        match store.query_by_pid(pid, event_type) {
            Ok(records) => self.output_records(&records, &format!("PID {pid}")),
            Err(e) => eprintln!("Query failed: {e}"),
        }
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

    fn print_summary(&self, store: &AuditStore) {
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
            }
            Err(e) => eprintln!("Summary query failed: {e}"),
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
}
