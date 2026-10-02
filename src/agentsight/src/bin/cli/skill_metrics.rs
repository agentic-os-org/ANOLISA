//! Skill Metrics subcommand — compute and display skill usage metrics.
//!
//! # Overview
//!
//! This subcommand provides CLI access to skill metrics computed from
//! GenAI events stored in the AgentSight SQLite database. Metrics are
//! computed on-demand from raw event data.
//!
//! # Examples
//!
//! ```bash
//! # Show all skill metrics for the last 7 days
//! agentsight skill-metrics all --last 168
//!
//! # Show skill load counts
//! agentsight skill-metrics loads --last 24
//!
//! # Show skill hotness ranking (JSON output)
//! agentsight skill-metrics hotness --last 168 --json
//!
//! # Filter by agent
//! agentsight skill-metrics all --last 168 --agent Cosh
//! ```

use agentsight::database::{DatabaseCoverage, DatabaseId, DatabaseManager};
use agentsight::skill_metrics::{MetricOptions, compute_skill_metrics};
use agentsight::storage::sqlite::GenAISqliteStore;
use structopt::StructOpt;

use super::hours_ago_ns;

/// Compute and display skill usage metrics from GenAI event data.
///
/// Metrics are computed on-demand by scanning genai_events within the
/// specified time range. Available metrics include download count, load count,
/// usage ratio, distribution, hotness ranking, frequency, intervals,
/// co-occurrence, and cross-agent overlap.
#[derive(Debug, StructOpt, Clone)]
pub struct SkillMetricsCommand {
    #[structopt(subcommand)]
    pub action: SkillMetricsAction,
}

#[derive(Debug, StructOpt, Clone)]
pub enum SkillMetricsAction {
    /// Compute all skill metrics.
    All {
        /// Query last N hours (default: 168 = 7 days)
        #[structopt(long, default_value = "168")]
        last: u64,

        /// Filter by agent name
        #[structopt(long)]
        agent: Option<String>,

        /// Output as JSON
        #[structopt(long)]
        json: bool,

        /// Override database path
        #[structopt(long)]
        db: Option<String>,
    },

    /// Show skill download tracking (first appearance in available_skills).
    Downloads {
        #[structopt(long, default_value = "168")]
        last: u64,
        #[structopt(long)]
        agent: Option<String>,
        #[structopt(long)]
        json: bool,
        #[structopt(long)]
        db: Option<String>,
    },

    /// Show skill load counts (SKILL.md reads via tool_calls).
    Loads {
        #[structopt(long, default_value = "168")]
        last: u64,
        #[structopt(long)]
        agent: Option<String>,
        #[structopt(long)]
        json: bool,
        #[structopt(long)]
        db: Option<String>,
    },

    /// Show skill usage ratio (tasks with/without skills).
    UsageRatio {
        #[structopt(long, default_value = "168")]
        last: u64,
        #[structopt(long)]
        agent: Option<String>,
        #[structopt(long)]
        json: bool,
        #[structopt(long)]
        db: Option<String>,
    },

    /// Show per-task skill count distribution.
    Distribution {
        #[structopt(long, default_value = "168")]
        last: u64,
        #[structopt(long)]
        agent: Option<String>,
        #[structopt(long)]
        json: bool,
        #[structopt(long)]
        db: Option<String>,
    },

    /// Show skill hotness ranking by week.
    Hotness {
        #[structopt(long, default_value = "168")]
        last: u64,
        #[structopt(long)]
        agent: Option<String>,
        #[structopt(long)]
        json: bool,
        #[structopt(long)]
        db: Option<String>,
    },
}

impl SkillMetricsCommand {
    pub fn execute(&self) {
        if let Err(e) = self.run() {
            eprintln!("Error: {e}");
            std::process::exit(1);
        }
    }

    fn run(&self) -> Result<(), Box<dyn std::error::Error>> {
        let (last, agent, json, db, options) = match &self.action {
            SkillMetricsAction::All {
                last,
                agent,
                json,
                db,
            } => (
                *last,
                agent.as_deref(),
                *json,
                db.as_deref(),
                MetricOptions::all(),
            ),
            SkillMetricsAction::Downloads {
                last,
                agent,
                json,
                db,
            } => (
                *last,
                agent.as_deref(),
                *json,
                db.as_deref(),
                MetricOptions {
                    downloads: true,
                    ..Default::default()
                },
            ),
            SkillMetricsAction::Loads {
                last,
                agent,
                json,
                db,
            } => (
                *last,
                agent.as_deref(),
                *json,
                db.as_deref(),
                MetricOptions {
                    loads: true,
                    ..Default::default()
                },
            ),
            SkillMetricsAction::UsageRatio {
                last,
                agent,
                json,
                db,
            } => (
                *last,
                agent.as_deref(),
                *json,
                db.as_deref(),
                MetricOptions {
                    usage_ratio: true,
                    ..Default::default()
                },
            ),
            SkillMetricsAction::Distribution {
                last,
                agent,
                json,
                db,
            } => (
                *last,
                agent.as_deref(),
                *json,
                db.as_deref(),
                MetricOptions {
                    distribution: true,
                    ..Default::default()
                },
            ),
            SkillMetricsAction::Hotness {
                last,
                agent,
                json,
                db,
            } => (
                *last,
                agent.as_deref(),
                *json,
                db.as_deref(),
                MetricOptions {
                    hotness: true,
                    ..Default::default()
                },
            ),
        };

        // Resolve database path
        let db_path = match db {
            Some(p) => std::path::PathBuf::from(p),
            None => GenAISqliteStore::default_path(),
        };

        // Open store
        let store = DatabaseManager::open_query(
            DatabaseId::GenAi,
            &db_path,
            DatabaseCoverage::Full,
            GenAISqliteStore::open_read_only_existing,
        )?;

        // Compute time range
        let start_ns = hours_ago_ns(last) as i64;
        let end_ns = hours_ago_ns(0) as i64;

        // Query events
        let events = store.get_events_in_time_range(start_ns, end_ns, agent)?;

        if events.is_empty() {
            if json {
                println!("{{\"message\": \"No events found in the specified time range\"}}");
            } else {
                eprintln!("No events found in the last {last} hours.");
            }
            return Ok(());
        }

        // Compute metrics
        let report = compute_skill_metrics(&events, &options);

        if json {
            println!("{}", serde_json::to_string_pretty(&report)?);
        } else {
            print_report(&report, &options);
        }

        Ok(())
    }
}

// --- Human-readable output ---

fn print_report(report: &agentsight::skill_metrics::SkillMetricsReport, options: &MetricOptions) {
    println!("=== Skill Metrics Report ===");
    println!("Computed at: {}", report.computed_at);
    println!("Events analyzed: {}", report.event_count);
    println!();

    if options.downloads
        && let Some(ref d) = report.downloads
    {
        println!("--- Skill Downloads ---");
        if d.downloads.is_empty() {
            println!("  (no downloads detected)");
        } else {
            println!("  {:30} {:>12} {:>10}", "Skill", "First Seen", "Sessions");
            for (name, info) in &d.downloads {
                println!(
                    "  {:30} {:>12} {:>10}",
                    name,
                    format_timestamp_ns(info.first_seen_timestamp_ns),
                    info.total_sessions
                );
            }
        }
        println!();
    }

    if options.loads
        && let Some(ref l) = report.loads
    {
        println!("--- Skill Load Counts ---");
        println!("  Total loads: {}", l.total_loads);
        if !l.loads.is_empty() {
            let mut sorted: Vec<_> = l.loads.iter().collect();
            sorted.sort_by(|a, b| b.1.cmp(a.1));
            println!("  {:30} {:>8}", "Skill", "Count");
            for (name, count) in sorted {
                println!("  {name:30} {count:>8}");
            }
        }
        println!();
    }

    if options.usage_ratio
        && let Some(ref u) = report.usage_ratio
    {
        println!("--- Skill Usage Ratio ---");
        println!("  Ratio: {:.1}%", u.ratio * 100.0);
        println!("  Sessions with skill: {}", u.with_skill_count);
        println!("  Sessions without skill: {}", u.without_skill_count);
        println!("  Total sessions: {}", u.total_sessions);
        println!();
    }

    if options.distribution
        && let Some(ref d) = report.distribution
    {
        println!("--- Per-task Skill Count Distribution ---");
        println!(
            "  Min: {}, Max: {}, Mean: {:.1}, Median: {:.1}, P90: {:.1}",
            d.min, d.max, d.mean, d.median, d.p90
        );
        println!(
            "  Histogram: [0]={} [1]={} [2]={} [3]={} [4]={} [5+]={}",
            d.histogram[0],
            d.histogram[1],
            d.histogram[2],
            d.histogram[3],
            d.histogram[4],
            d.histogram[5]
        );
        println!();
    }

    if options.hotness
        && let Some(ref h) = report.hotness
    {
        println!("--- Skill Hotness Ranking ---");
        if h.rankings.is_empty() {
            println!("  (no data)");
        } else {
            println!(
                "  {:>4} {:30} {:>8} {:>8}",
                "Rank", "Skill", "Loads", "Delta"
            );
            for entry in &h.rankings {
                let delta = entry
                    .rank_delta
                    .map(|d| format!("{d:+}"))
                    .unwrap_or_else(|| "-".to_string());
                println!(
                    "  {:>4} {:30} {:>8} {:>8}",
                    entry.total_rank, entry.skill_name, entry.total_loads, delta
                );
            }
        }
        println!();
    }
}

fn format_timestamp_ns(ns: i64) -> String {
    let secs = ns / 1_000_000_000;
    let dt = chrono::DateTime::from_timestamp(secs, 0)
        .unwrap_or_default()
        .naive_utc();
    dt.format("%m-%d %H:%M").to_string()
}
