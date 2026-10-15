//! Token query subcommand

use agentsight::database::{DatabaseCoverage, DatabaseId, DatabaseManager};
use agentsight::{
    SqliteConfig, TimePeriod, TokenQueryResult, TokenStore, Trend, format_tokens_with_commas,
};
use structopt::StructOpt;

/// Token query subcommand
#[derive(Debug, StructOpt, Clone)]
pub struct TokenCommand {
    /// Query by fixed time period
    #[structopt(long, possible_values = &["today", "yesterday", "week", "last_week", "month", "last_month"])]
    pub period: Option<String>,

    /// Query last N hours (cannot be combined with --period)
    #[structopt(long, conflicts_with = "period")]
    pub hours: Option<u64>,

    /// Compare with previous period
    #[structopt(long)]
    pub compare: bool,

    /// Output as JSON
    #[structopt(long)]
    pub json: bool,

    /// Custom data file path
    #[structopt(long)]
    pub data_file: Option<String>,
}

impl TokenCommand {
    pub fn execute(&self) {
        // Determine data file path
        // Use the unified database path (agentsight.db) as default,
        // which is where Storage writes all tables.
        let data_path = self
            .data_file
            .as_ref()
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| SqliteConfig::default().db_path());

        self.execute_summary(&data_path);
    }

    fn execute_summary(&self, data_path: &std::path::Path) {
        if self.data_file.is_some()
            && let Err(error) = agentsight::check_data_file(data_path)
        {
            eprintln!("{error}");
            std::process::exit(1);
        }

        let store = match DatabaseManager::open_query(
            DatabaseId::Primary,
            data_path,
            DatabaseCoverage::Full,
            |path| TokenStore::open_read_only_existing(path, "token_records"),
        ) {
            Ok(store) => store,
            Err(error) => {
                eprintln!("Failed to open token database: {error}");
                std::process::exit(1);
            }
        };
        let query = agentsight::TokenQuery::new(&store);

        // Execute query
        let result = if let Some(hours) = self.hours {
            if self.compare {
                query.by_hours_with_compare(hours)
            } else {
                query.by_hours(hours)
            }
        } else if let Some(ref period_str) = self.period {
            let period = super::parse_period(period_str);
            if self.compare {
                query.by_period_with_compare(period)
            } else {
                query.by_period(period)
            }
        } else if self.compare {
            query.by_period_with_compare(TimePeriod::Today)
        } else {
            query.by_period(TimePeriod::Today)
        };

        // Output result
        if self.json {
            super::print_json(&result);
        } else {
            print_human_readable(&result, self.compare);
        }
    }
}

/// Print human-readable summary output
fn print_human_readable(result: &TokenQueryResult, show_compare: bool) {
    // Main result
    println!(
        "{}共消耗 {} tokens。",
        result.period,
        format_tokens_with_commas(result.total_tokens)
    );

    // Comparison
    #[allow(clippy::collapsible_if)]
    if show_compare {
        if let Some(ref comp) = result.comparison {
            let trend = match comp.trend {
                Trend::Up => "增长",
                Trend::Down => "下降",
                Trend::Flat => "持平",
            };

            println!(
                "比上一时段（{}）{}了 {}。",
                format_tokens_with_commas(comp.previous_total),
                trend,
                comp.formatted_change()
            );
        }
    }

    // Additional details
    if result.request_count > 0 {
        println!();
        println!(
            "共 {} 次请求，输入 {} tokens，输出 {} tokens。",
            result.request_count,
            format_tokens_with_commas(result.input_tokens),
            format_tokens_with_commas(result.output_tokens)
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `--hours` used to win unconditionally and `--period` was then never
    /// read: `token --period week --hours 24` answered the 24-hour window while
    /// the caller believed it asked for the calendar week — with exit 0 and a
    /// header naming the window actually used, so the mismatch was invisible.
    /// The sibling `interruption list --unresolved/--resolved` flags are
    /// declared mutually exclusive for the same reason.
    #[test]
    fn hours_and_period_are_rejected_together() {
        let both = TokenCommand::from_iter_safe(["token", "--period", "week", "--hours", "24"]);
        let err = both.expect_err("two window selectors must not be silently reduced to one");
        let message = err.to_string();
        assert!(
            message.contains("--hours") && message.contains("--period"),
            "the error must name both flags: {message}"
        );

        // Either selector alone keeps working, in both flag orders.
        let hours =
            TokenCommand::from_iter_safe(["token", "--hours", "24"]).expect("--hours alone");
        assert_eq!(hours.hours, Some(24));
        let period =
            TokenCommand::from_iter_safe(["token", "--period", "week"]).expect("--period alone");
        assert_eq!(period.period.as_deref(), Some("week"));
        let compared = TokenCommand::from_iter_safe(["token", "--hours", "24", "--compare"])
            .expect("--hours with --compare");
        assert!(compared.compare);
        let reversed = TokenCommand::from_iter_safe(["token", "--hours", "24", "--period", "week"]);
        assert!(reversed.is_err(), "the order of the flags must not matter");
    }
}
