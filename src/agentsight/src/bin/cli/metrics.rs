//! Metrics subcommand — print per-agent token usage in Prometheus text format

use agentsight::database::{DatabaseCoverage, DatabaseId, DatabaseManager};
use agentsight::storage::sqlite::GenAISqliteStore;
use structopt::StructOpt;

/// Print per-agent token usage metrics in Prometheus text format
#[derive(Debug, StructOpt, Clone)]
pub struct MetricsCommand {
    /// Override the database path
    ///
    /// Reads a copy of the database instead of the default location — for
    /// example a database copied from another host for offline analysis.
    #[structopt(long)]
    pub db: Option<String>,
}

impl MetricsCommand {
    /// Resolve the database path: `--db` reads a copy of the database;
    /// without it the default location is used.
    fn db_path(&self) -> std::path::PathBuf {
        self.db
            .as_deref()
            .map(std::path::PathBuf::from)
            .unwrap_or_else(GenAISqliteStore::default_path)
    }

    pub fn execute(&self) {
        let db_path = self.db_path();

        if let Err(error) = agentsight::check_data_file(&db_path) {
            eprintln!("{error}");
            std::process::exit(1);
        }

        let store = match DatabaseManager::open_query(
            DatabaseId::GenAi,
            &db_path,
            DatabaseCoverage::Full,
            GenAISqliteStore::open_read_only_existing,
        ) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("Error opening database {db_path:?}: {e}");
                std::process::exit(1);
            }
        };

        let summaries = match store.get_agent_token_summary() {
            Ok(v) => v,
            Err(e) => {
                eprintln!("Error querying metrics: {e}");
                std::process::exit(1);
            }
        };

        // ── Prometheus text format output ──────────────────────────────────

        println!(
            "# HELP agentsight_token_input_total Total input tokens consumed by agent (all-time)"
        );
        println!("# TYPE agentsight_token_input_total counter");
        for s in &summaries {
            println!(
                "agentsight_token_input_total{{agent=\"{}\"}} {}",
                escape_label(&s.agent_name),
                s.input_tokens
            );
        }

        println!();
        println!(
            "# HELP agentsight_token_output_total Total output tokens consumed by agent (all-time)"
        );
        println!("# TYPE agentsight_token_output_total counter");
        for s in &summaries {
            println!(
                "agentsight_token_output_total{{agent=\"{}\"}} {}",
                escape_label(&s.agent_name),
                s.output_tokens
            );
        }

        println!();
        println!(
            "# HELP agentsight_token_total_total Total tokens (input+output) consumed by agent (all-time)"
        );
        println!("# TYPE agentsight_token_total_total counter");
        for s in &summaries {
            println!(
                "agentsight_token_total_total{{agent=\"{}\"}} {}",
                escape_label(&s.agent_name),
                s.total_tokens
            );
        }

        println!();
        println!(
            "# HELP agentsight_llm_requests_total Total LLM requests made by agent (all-time)"
        );
        println!("# TYPE agentsight_llm_requests_total counter");
        for s in &summaries {
            println!(
                "agentsight_llm_requests_total{{agent=\"{}\"}} {}",
                escape_label(&s.agent_name),
                s.request_count
            );
        }
    }
}

/// Escape Prometheus label value: backslash → \\, double-quote → \", newline → \n
fn escape_label(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn db_path_honours_the_override_and_falls_back_to_the_default() {
        // `--db` must reach the query path: reverting the wiring (returning
        // the default unconditionally) makes this fail.
        let mut c = MetricsCommand { db: None };
        assert_eq!(c.db_path(), GenAISqliteStore::default_path());
        c.db = Some("/tmp/agentsight_metrics_override.db".into());
        assert_eq!(
            c.db_path(),
            std::path::PathBuf::from("/tmp/agentsight_metrics_override.db")
        );
    }
}
