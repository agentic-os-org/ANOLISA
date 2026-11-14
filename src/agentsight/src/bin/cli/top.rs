//! `top` subcommand — live per-process or per-session LLM throughput view

use std::io::IsTerminal;
use std::time::Duration;

use agentsight::database::{DatabaseCoverage, DatabaseId, DatabaseManager};
use agentsight::format_tokens;
use agentsight::storage::sqlite::{
    GenAISqliteStore, MetricPercentiles, TopGrouping, TopRow, TopWindowSummary,
};
use structopt::StructOpt;

/// One refresh of the live view, as emitted by `--json`.
#[derive(Debug, serde::Serialize)]
struct TopSnapshot<'a> {
    window_start_ns: i64,
    window_end_ns: i64,
    grouping: &'static str,
    summary: &'a TopWindowSummary,
    rows: &'a [TopRow],
}

/// Live view of LLM throughput and latency per agent process or per session
#[derive(Debug, StructOpt, Clone)]
pub struct TopCommand {
    /// Override the database path
    ///
    /// Reads a copy of the database instead of the default location — for
    /// example a database copied from another host for offline analysis.
    #[structopt(long)]
    pub db: Option<String>,

    /// Seconds between refreshes (clamped to 0.1-86400)
    #[structopt(short = "d", long, default_value = "2")]
    pub delay: f64,

    /// Seconds of history each refresh aggregates over; 0 means all recorded history
    #[structopt(long, default_value = "60")]
    pub window: u64,

    /// Group rows by session ID instead of agent process
    #[structopt(long)]
    pub session: bool,

    /// Column to sort by; metric columns compare p50
    #[structopt(
        long,
        default_value = "tps",
        possible_values = &[
            "tps", "ttft", "tpot", "reqs", "in", "out", "total", "agent", "pid", "session", "last"
        ]
    )]
    pub sort: String,

    /// Sort ascending instead of descending
    #[structopt(long)]
    pub asc: bool,

    /// Only rows recorded by this agent process ID
    #[structopt(long)]
    pub pid: Option<i64>,

    /// Only rows of this agent name (case-insensitive)
    #[structopt(long)]
    pub agent: Option<String>,

    /// Maximum number of rows to display; 0 shows every row
    #[structopt(long, default_value = "20")]
    pub limit: usize,

    /// Number of refreshes before exiting; defaults to one when stdout is not a terminal
    #[structopt(short = "n", long)]
    pub iterations: Option<u64>,

    /// Print plain text without screen clearing, so the output can be piped or logged
    #[structopt(long)]
    pub batch: bool,

    /// Print each refresh as one JSON object instead of a table
    #[structopt(long)]
    pub json: bool,
}

impl TopCommand {
    /// Resolve the database path: `--db` reads a copy of the database;
    /// without it the default location is used.
    fn db_path(&self) -> std::path::PathBuf {
        self.db
            .as_deref()
            .map(std::path::PathBuf::from)
            .unwrap_or_else(GenAISqliteStore::default_path)
    }

    /// Nanosecond window each refresh aggregates over, as `(start, end)`.
    ///
    /// `--window 0` reaches back to the oldest recorded call, matching the
    /// all-time scope of `agentsight token` and `agentsight metrics`.
    fn window_ns(&self) -> (i64, i64) {
        // A pre-epoch realtime clock degrades to 0 (the `epoch_nanos` family
        // contract) instead of unwrapping the elapsed-time error.
        let now_ns = agentsight::utils::epoch_nanos(std::time::SystemTime::now()) as i64;
        if self.window == 0 {
            return (0, now_ns);
        }
        // Saturate so an absurd --window degrades to "everything" instead of
        // overflowing; guard the u64->i64 cast first so a value >= 2^63 cannot
        // flip sign. Recorded timestamps are epoch nanos, so the lower bound is
        // clamped at 0 rather than left negative.
        let span_ns = (self.window.min(i64::MAX as u64) as i64).saturating_mul(1_000_000_000);
        (now_ns.saturating_sub(span_ns).max(0), now_ns)
    }

    /// Delay between refreshes, clamped to 0.1..=86_400 seconds. The floor keeps
    /// a fractional `--delay` from turning the view into a busy loop re-reading
    /// SQLite; the ceiling keeps `Duration::from_secs_f64` total, since it
    /// panics on a value too large to represent. A non-finite `--delay` falls
    /// back to the 2 second default.
    fn refresh_interval(&self) -> Duration {
        Duration::from_secs_f64(if self.delay.is_finite() {
            self.delay.clamp(0.1, 86_400.0)
        } else {
            2.0
        })
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
            Ok(store) => store,
            Err(error) => {
                eprintln!("Error opening database {db_path:?}: {error}");
                std::process::exit(1);
            }
        };

        // A pipe or a log file gets one refresh and no control codes; only a
        // terminal keeps redrawing in place until Ctrl+C.
        let interactive = !self.batch && !self.json && std::io::stdout().is_terminal();
        let refreshes = self
            .iterations
            .unwrap_or(if interactive { u64::MAX } else { 1 });

        for iteration in 0..refreshes {
            if iteration > 0 {
                std::thread::sleep(self.refresh_interval());
            }
            if let Err(error) = self.render(&store, interactive) {
                eprintln!("Error querying top rows: {error}");
                std::process::exit(1);
            }
        }
    }

    /// Queries one window and prints it as JSON or as a table.
    fn render(
        &self,
        store: &GenAISqliteStore,
        interactive: bool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let (start_ns, end_ns) = self.window_ns();
        let grouping = if self.session {
            TopGrouping::Session
        } else {
            TopGrouping::Process
        };
        let summary =
            store.get_top_window_summary(start_ns, end_ns, self.agent.as_deref(), self.pid)?;
        let mut rows =
            store.get_top_rows(start_ns, end_ns, grouping, self.agent.as_deref(), self.pid)?;
        sort_rows(&mut rows, &self.sort, self.asc);
        if self.limit > 0 {
            rows.truncate(self.limit);
        }

        if self.json {
            super::print_json(&TopSnapshot {
                window_start_ns: start_ns,
                window_end_ns: end_ns,
                grouping: if self.session { "session" } else { "process" },
                summary: &summary,
                rows: &rows,
            });
            return Ok(());
        }

        if interactive {
            // Home + clear keeps the redraw in place; the cursor is never
            // hidden, so Ctrl+C leaves a usable terminal without a handler.
            print!("\x1b[H\x1b[2J");
        }
        print_header(self, &summary, end_ns);
        print_table(&rows, self.session);
        if interactive {
            // Printed from the helper the sleep itself uses, so the footer can
            // never disagree with the real interval: NaN, infinity and values
            // outside the clamp range all render as what the view will do.
            println!(
                "\nPress Ctrl+C to stop. Refresh every {:.1}s.",
                self.refresh_interval().as_secs_f64()
            );
        }
        Ok(())
    }
}

/// Prints the window totals line above the table.
fn print_header(command: &TopCommand, summary: &TopWindowSummary, end_ns: i64) {
    let window = if command.window == 0 {
        "all".to_string()
    } else {
        format!("{}s", command.window)
    };
    // All-history mode has no window to divide by, so the rate covers the span
    // the recorded calls actually span instead of the whole epoch.
    let span_secs = if command.window == 0 {
        (summary.last_seen_ns - summary.first_seen_ns) as f64 / 1_000_000_000.0
    } else {
        command.window as f64
    };
    let tps = if span_secs > 0.0 {
        format!("{:.1}", summary.output_tokens as f64 / span_secs)
    } else {
        "-".to_string()
    };
    println!(
        "agentsight top - {}  window {}  procs {}  sessions {}  reqs {}  in {}  out {}  tps {}",
        format_clock(end_ns),
        window,
        summary.process_count,
        summary.session_count,
        summary.call_count,
        format_tokens(summary.input_tokens.max(0) as u64),
        format_tokens(summary.output_tokens.max(0) as u64),
        tps,
    );
}

/// Prints the row table; the first column follows the active grouping.
fn print_table(rows: &[TopRow], session_view: bool) {
    if rows.is_empty() {
        println!("No completed LLM calls recorded in this window.");
        return;
    }

    println!(
        "{:>20} {:<18} {:<22} {:>5} {:>5} {:>8} {:>8} {:>8} {:>8} {:>8} {:>8} {:>8}",
        if session_view { "SESSION" } else { "PID" },
        "AGENT",
        "MODEL",
        "REQS",
        "SSE",
        "IN",
        "OUT",
        "TOTAL",
        "TPS",
        "TTFT",
        "TPOT",
        "LAST",
    );
    for row in rows {
        let id = match (session_view, &row.session_id) {
            (true, Some(session)) => fit_id(session, 20),
            _ => row
                .pid
                .map(|pid| pid.to_string())
                .unwrap_or_else(|| "-".to_string()),
        };
        println!(
            "{:>20} {:<18} {:<22} {:>5} {:>5} {:>8} {:>8} {:>8} {:>8} {:>8} {:>8} {:>8}",
            id,
            fit(&row.label, 18),
            fit(row.model.as_deref().unwrap_or("-"), 22),
            row.call_count,
            row.streaming_call_count,
            format_tokens(row.input_tokens.max(0) as u64),
            format_tokens(row.output_tokens.max(0) as u64),
            format_tokens(row.total_tokens.max(0) as u64),
            format_metric(row.tps_tokens_per_second.as_ref(), format_tps),
            format_metric(row.ttft_ms.as_ref(), format_ms),
            format_metric(row.tpot_ms_per_token.as_ref(), format_ms),
            format_clock(row.last_seen_ns),
        );
    }
}

/// Orders rows by the requested column.
///
/// Rows without a sample for a metric column keep their relative order and stay
/// below every row that has one in both directions, so `--asc` still leads with
/// the fastest measured call instead of with rows that measured nothing.
fn sort_rows(rows: &mut [TopRow], sort: &str, ascending: bool) {
    rows.sort_by(|a, b| match sort {
        "ttft" => compare_metric(a.ttft_ms.as_ref(), b.ttft_ms.as_ref(), ascending),
        "tpot" => compare_metric(
            a.tpot_ms_per_token.as_ref(),
            b.tpot_ms_per_token.as_ref(),
            ascending,
        ),
        "reqs" => directed(a.call_count.cmp(&b.call_count), ascending),
        "in" => directed(a.input_tokens.cmp(&b.input_tokens), ascending),
        "out" => directed(a.output_tokens.cmp(&b.output_tokens), ascending),
        "total" => directed(a.total_tokens.cmp(&b.total_tokens), ascending),
        "pid" => directed(a.pid.cmp(&b.pid), ascending),
        "session" => directed(a.session_id.cmp(&b.session_id), ascending),
        "agent" => directed(a.label.cmp(&b.label), ascending),
        "last" => directed(a.last_seen_ns.cmp(&b.last_seen_ns), ascending),
        // "tps" and, defensively, any unrecognized column: throughput is
        // what the view is for.
        _ => compare_metric(
            a.tps_tokens_per_second.as_ref(),
            b.tps_tokens_per_second.as_ref(),
            ascending,
        ),
    });
}

/// Applies the requested direction to a total ordering.
fn directed(ordering: std::cmp::Ordering, ascending: bool) -> std::cmp::Ordering {
    if ascending {
        ordering
    } else {
        ordering.reverse()
    }
}

/// Compares one metric by p50, keeping rows without a sample last in both
/// directions.
fn compare_metric(
    a: Option<&MetricPercentiles>,
    b: Option<&MetricPercentiles>,
    ascending: bool,
) -> std::cmp::Ordering {
    match (a, b) {
        (Some(a), Some(b)) => directed(a.p50.total_cmp(&b.p50), ascending),
        // "Sorts before" is direction-independent here: a row without a sample
        // is never a candidate for the top of either ordering.
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    }
}

/// Renders one metric's p50, or `-` when the row has no sample for it.
fn format_metric(
    percentiles: Option<&MetricPercentiles>,
    render: impl Fn(f64) -> String,
) -> String {
    percentiles
        .map(|percentiles| render(percentiles.p50))
        .unwrap_or_else(|| "-".to_string())
}

/// Formats tokens per second with one decimal, the precision the column shows.
fn format_tps(tps: f64) -> String {
    format!("{tps:.1}")
}

/// Formats a millisecond duration: sub-second values stay in milliseconds so
/// TPOT does not read as `0.0s`.
fn format_ms(ms: f64) -> String {
    if ms >= 1000.0 {
        format!("{:.2}s", ms / 1000.0)
    } else {
        format!("{ms:.1}ms")
    }
}

/// Formats a wall-clock timestamp in local time for the LAST column.
fn format_clock(ns: i64) -> String {
    chrono::DateTime::from_timestamp(ns / 1_000_000_000, 0)
        .map(|dt| {
            dt.with_timezone(&chrono::Local)
                .format("%H:%M:%S")
                .to_string()
        })
        .unwrap_or_else(|| "-".to_string())
}

/// Pads a display value to the column width, marking a cut with an ellipsis.
///
/// Counts characters rather than bytes so a multibyte agent name is truncated
/// cleanly instead of panicking the formatter.
fn fit(value: &str, width: usize) -> String {
    let trimmed = value.trim();
    if trimmed.chars().count() <= width {
        return format!("{trimmed:<width$}");
    }
    let mut cut: String = trimmed.chars().take(width.saturating_sub(1)).collect();
    cut.push('…');
    format!("{cut:<width$}")
}

/// Pads an identifier to the column width, keeping both ends when it is cut.
///
/// Session IDs are UUIDs whose leading characters often match, so a plain
/// prefix cut would render distinct sessions identically; `--json` still
/// reports the full value.
fn fit_id(value: &str, width: usize) -> String {
    let trimmed = value.trim();
    let chars = trimmed.chars().count();
    if chars <= width {
        return format!("{trimmed:<width$}");
    }
    let head = width.saturating_sub(1) / 2;
    let tail = width.saturating_sub(1) - head;
    let mut cut: String = trimmed.chars().take(head).collect();
    cut.push('…');
    cut.extend(trimmed.chars().skip(chars - tail));
    format!("{cut:<width$}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(label: &str, pid: Option<i64>, tps: Option<f64>) -> TopRow {
        TopRow {
            pid,
            session_id: None,
            label: label.to_string(),
            process_name: None,
            model: None,
            call_count: 1,
            streaming_call_count: 0,
            input_tokens: 0,
            output_tokens: 0,
            total_tokens: 0,
            first_seen_ns: 0,
            last_seen_ns: 0,
            ttft_ms: None,
            tps_tokens_per_second: tps.map(|p50| MetricPercentiles {
                p50,
                p95: p50,
                p99: p50,
            }),
            tpot_ms_per_token: None,
            e2e_latency_ms: None,
        }
    }

    #[test]
    fn db_path_honours_the_override_and_falls_back_to_the_default() {
        let mut command = TopCommand::from_iter_safe(["agentsight"]).unwrap();
        assert_eq!(command.db_path(), GenAISqliteStore::default_path());
        command.db = Some("/tmp/agentsight_top_override.db".into());
        assert_eq!(
            command.db_path(),
            std::path::PathBuf::from("/tmp/agentsight_top_override.db")
        );
    }

    #[test]
    fn flags_parse_with_their_documented_defaults() {
        let command = TopCommand::from_iter_safe(["agentsight"]).unwrap();
        assert_eq!(command.delay, 2.0);
        assert_eq!(command.window, 60);
        assert_eq!(command.sort, "tps");
        assert_eq!(command.limit, 20);
        assert!(!command.session && !command.asc && !command.batch && !command.json);

        let command = TopCommand::from_iter_safe([
            "agentsight",
            "-d",
            "0.5",
            "--window",
            "0",
            "--session",
            "--sort",
            "ttft",
            "--asc",
            "-n",
            "3",
            "--batch",
        ])
        .unwrap();
        assert_eq!(command.delay, 0.5);
        assert_eq!(command.window, 0);
        assert!(command.session && command.asc && command.batch);
        assert_eq!(command.iterations, Some(3));
    }

    #[test]
    fn an_unparsable_sort_column_is_rejected() {
        // `possible_values` is the contract: a typo must fail loudly instead of
        // silently falling back to the default column.
        assert!(TopCommand::from_iter_safe(["agentsight", "--sort", "bogus"]).is_err());
    }

    #[test]
    fn window_zero_covers_all_recorded_history() {
        let mut command = TopCommand::from_iter_safe(["agentsight"]).unwrap();
        command.window = 0;
        let (start_ns, end_ns) = command.window_ns();
        assert_eq!(start_ns, 0);
        assert!(end_ns > 0);

        // An absurd window saturates to "everything" instead of overflowing
        // into an inverted window.
        command.window = u64::MAX;
        let (start_ns, end_ns) = command.window_ns();
        assert_eq!(start_ns, 0);
        assert!(end_ns > 0);
    }

    #[test]
    fn a_bounded_window_is_exactly_as_wide_as_requested() {
        let mut command = TopCommand::from_iter_safe(["agentsight"]).unwrap();
        command.window = 60;
        let (start_ns, end_ns) = command.window_ns();
        assert_eq!(end_ns - start_ns, 60_000_000_000);
    }

    #[test]
    fn refresh_interval_is_clamped_and_never_nan() {
        let mut command = TopCommand::from_iter_safe(["agentsight"]).unwrap();
        command.delay = 0.0;
        assert_eq!(command.refresh_interval(), Duration::from_millis(100));
        command.delay = f64::NAN;
        assert_eq!(command.refresh_interval(), Duration::from_secs(2));
        command.delay = 5.0;
        assert_eq!(command.refresh_interval(), Duration::from_secs(5));
        // A finite value beyond Duration's range would panic inside
        // from_secs_f64; the ceiling keeps the view alive instead.
        command.delay = 1e20;
        assert_eq!(command.refresh_interval(), Duration::from_secs(86_400));
        command.delay = f64::INFINITY;
        assert_eq!(command.refresh_interval(), Duration::from_secs(2));
    }

    #[test]
    fn sorting_keeps_rows_without_a_metric_sample_last_in_both_directions() {
        let mut rows = vec![
            row("none", Some(1), None),
            row("slow", Some(2), Some(10.0)),
            row("fast", Some(3), Some(90.0)),
        ];
        sort_rows(&mut rows, "tps", false);
        let labels: Vec<&str> = rows.iter().map(|row| row.label.as_str()).collect();
        assert_eq!(labels, ["fast", "slow", "none"]);

        sort_rows(&mut rows, "tps", true);
        let labels: Vec<&str> = rows.iter().map(|row| row.label.as_str()).collect();
        assert_eq!(labels, ["slow", "fast", "none"]);
    }

    #[test]
    fn sorting_by_agent_name_and_request_count_uses_the_requested_column() {
        let mut rows = vec![
            row("b-agent", Some(2), Some(1.0)),
            row("a-agent", Some(1), Some(9.0)),
        ];
        sort_rows(&mut rows, "agent", true);
        assert_eq!(rows[0].label, "a-agent");

        let mut rows = vec![
            row("one", Some(1), Some(1.0)),
            row("many", Some(2), Some(1.0)),
        ];
        rows[1].call_count = 7;
        sort_rows(&mut rows, "reqs", false);
        assert_eq!(rows[0].label, "many");
    }

    #[test]
    fn durations_switch_to_seconds_only_above_one_second() {
        assert_eq!(format_ms(18.34), "18.3ms");
        assert_eq!(format_ms(999.9), "999.9ms");
        assert_eq!(format_ms(1240.0), "1.24s");
    }

    #[test]
    fn long_labels_are_cut_on_a_character_boundary() {
        assert_eq!(fit("claude", 18), "claude            ");
        assert_eq!(fit("  padded  ", 6), "padded");
        assert_eq!(fit("0123456789", 4), "012…");
        // A multibyte name must not be split mid-character.
        assert_eq!(fit("智能体名称", 3).chars().count(), 3);
    }

    #[test]
    fn cut_session_ids_keep_both_ends() {
        let first = fit_id("00000000-0000-0000-0000-000000000001", 20);
        let second = fit_id("00000000-0000-0000-0000-000000000002", 20);
        assert_eq!(first.chars().count(), 20);
        assert_eq!(first, "00000000-…0000000001");
        assert_ne!(
            first, second,
            "a shared prefix must not hide the difference"
        );
        // A short identifier is only padded.
        assert_eq!(fit_id("s-1", 20), "s-1                 ");
    }
}
