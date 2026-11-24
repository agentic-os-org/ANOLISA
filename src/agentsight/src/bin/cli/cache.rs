//! Cache subcommand — prompt-cache hit rate, low-hit causes and their cost

use agentsight::database::{DatabaseCoverage, DatabaseId, DatabaseManager};
use agentsight::format_tokens_with_commas;
use agentsight::health::cache::{
    CacheDiagnosis, CacheDiagnosisOptions, CacheDiagnosisRequest, CacheGroupDiagnosis, CachePrices,
    CacheStatus, diagnose_store,
};
use agentsight::storage::sqlite::GenAISqliteStore;
use structopt::StructOpt;

const HOUR_NS: i64 = 3_600_000_000_000;

/// Diagnose prompt-cache hit rates of recorded LLM calls
///
/// Rates are token-weighted per agent, provider and model. A call without a
/// cache-read counter is not counted as a miss: the conservative rate treats
/// it as zero, the complete rate leaves it out.
///
/// Examples:
///   agentsight cache --last 24
///   agentsight cache --agent hermes --compare
///   agentsight cache --until 2026-11-03T12:00:00+08:00 --baseline-until 2026-11-01T12:00:00+08:00
///   agentsight cache --price-input 12 --price-cache-read 1 --price-cache-write 15 --price-output 36 --json
#[derive(Debug, StructOpt, Clone)]
#[structopt(verbatim_doc_comment)]
pub struct CacheCommand {
    /// Window length in hours
    #[structopt(long, default_value = "24")]
    pub last: u64,

    /// End of the window as RFC 3339 (default: now)
    #[structopt(long, parse(try_from_str = parse_time))]
    pub until: Option<i64>,

    /// Compare with the preceding window of the same length
    #[structopt(long, conflicts_with = "baseline-until")]
    pub compare: bool,

    /// Compare with the window of the same length ending at this RFC 3339 time
    #[structopt(long, parse(try_from_str = parse_time))]
    pub baseline_until: Option<i64>,

    /// Only calls of this agent (case-insensitive)
    #[structopt(long)]
    pub agent: Option<String>,

    /// Only calls of this session
    #[structopt(long)]
    pub session: Option<String>,

    /// Conservative hit rate below which a well-sampled group needs review
    #[structopt(long, default_value = "0.5", parse(try_from_str = parse_rate))]
    pub low_hit_rate: f64,

    /// Gap in seconds after which a cache entry is assumed expired
    #[structopt(long, default_value = "300")]
    pub ttl_secs: u64,

    /// Price per million uncached input tokens (all four prices go together)
    #[structopt(
        long,
        requires_all = &["price-cache-read", "price-cache-write", "price-output"],
        parse(try_from_str = parse_price)
    )]
    pub price_input: Option<f64>,

    /// Price per million tokens read from the cache
    #[structopt(long, requires = "price-input", parse(try_from_str = parse_price))]
    pub price_cache_read: Option<f64>,

    /// Price per million tokens written to the cache
    #[structopt(long, requires = "price-input", parse(try_from_str = parse_price))]
    pub price_cache_write: Option<f64>,

    /// Price per million output tokens
    #[structopt(long, requires = "price-input", parse(try_from_str = parse_price))]
    pub price_output: Option<f64>,

    /// Override the database path
    #[structopt(long)]
    pub db: Option<String>,

    /// Output as JSON
    #[structopt(long)]
    pub json: bool,
}

fn parse_time(value: &str) -> Result<i64, String> {
    let time = chrono::DateTime::parse_from_rfc3339(value).map_err(|error| {
        format!("expected an RFC 3339 time such as 2026-11-03T12:00:00+08:00: {error}")
    })?;
    time.timestamp_nanos_opt()
        .ok_or_else(|| format!("{value} is outside the representable range"))
}

fn parse_price(value: &str) -> Result<f64, String> {
    let price: f64 = value.parse().map_err(|error| format!("{error}"))?;
    if price.is_finite() && price >= 0.0 {
        Ok(price)
    } else {
        Err("expected a non-negative price per million tokens".into())
    }
}

fn parse_rate(value: &str) -> Result<f64, String> {
    let rate: f64 = value.parse().map_err(|error| format!("{error}"))?;
    if (0.0..=1.0).contains(&rate) {
        Ok(rate)
    } else {
        Err("expected a ratio between 0 and 1".into())
    }
}

impl CacheCommand {
    fn request(&self, now_ns: i64) -> Result<CacheDiagnosisRequest, String> {
        let length = i64::try_from(self.last)
            .ok()
            .and_then(|hours| hours.checked_mul(HOUR_NS))
            .ok_or("--last is too large")?;
        let window = |end_ns: i64| {
            end_ns
                .checked_sub(length)
                .map(|start_ns| (start_ns, end_ns))
                .ok_or("the window starts before the representable range")
        };
        let (start_ns, end_ns) = window(self.until.unwrap_or(now_ns))?;
        let baseline = match (self.compare, self.baseline_until) {
            (true, _) => Some(window(start_ns)?),
            (false, Some(until)) => Some(window(until)?),
            (false, None) => None,
        };
        Ok(CacheDiagnosisRequest {
            start_ns,
            end_ns,
            agent_name: self.agent.clone().filter(|name| !name.is_empty()),
            session_id: self.session.clone().filter(|id| !id.is_empty()),
            baseline,
        })
    }

    fn options(&self) -> CacheDiagnosisOptions {
        let prices = match (
            self.price_input,
            self.price_cache_read,
            self.price_cache_write,
            self.price_output,
        ) {
            (Some(input), Some(cache_read), Some(cache_write), Some(output)) => Some(CachePrices {
                input,
                cache_read,
                cache_write,
                output,
            }),
            _ => None,
        };
        CacheDiagnosisOptions {
            low_hit_rate: self.low_hit_rate,
            cache_ttl_secs: self.ttl_secs,
            prices,
        }
    }

    pub fn execute(&self) {
        let now_ns = i64::try_from(agentsight::utils::epoch_nanos(std::time::SystemTime::now()))
            .unwrap_or(i64::MAX);
        let request = match self.request(now_ns) {
            Ok(request) => request,
            Err(error) => {
                eprintln!("{error}");
                std::process::exit(2);
            }
        };
        let db_path = self
            .db
            .as_deref()
            .map(std::path::PathBuf::from)
            .unwrap_or_else(GenAISqliteStore::default_path);
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
        let diagnosis = match diagnose_store(&store, &request, &self.options()) {
            Ok(diagnosis) => diagnosis,
            Err(error) => {
                eprintln!("Error diagnosing cache hit rates: {error}");
                std::process::exit(1);
            }
        };
        if self.json {
            super::print_json(&diagnosis);
        } else {
            print!("{}", render(&diagnosis));
        }
    }
}

fn tokens(count: i64) -> String {
    format_tokens_with_commas(u64::try_from(count).unwrap_or(0))
}

fn percent(rate: Option<f64>) -> String {
    rate.map_or_else(|| "n/a".into(), |rate| format!("{:.1}%", rate * 100.0))
}

/// The snake_case name the JSON output uses for `value`.
fn wire_name<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_default()
}

fn status_label(status: CacheStatus) -> &'static str {
    match status {
        CacheStatus::NoUsage => "no usage reported",
        CacheStatus::Undeterminable => "cannot judge: no cache-read counter",
        CacheStatus::InsufficientSamples => "too few samples to judge",
        CacheStatus::NeedsReview => "needs review",
        CacheStatus::Healthy => "healthy",
    }
}

fn milliseconds(value: Option<f64>) -> String {
    value.map_or_else(|| "n/a".into(), |ms| format!("{ms:.0} ms"))
}

fn p50(metric: &Option<agentsight::storage::sqlite::genai::MetricPercentiles>) -> String {
    milliseconds(metric.as_ref().map(|metric| metric.p50))
}

fn local_time(ns: i64) -> String {
    use chrono::TimeZone;
    chrono::Local
        .timestamp_nanos(ns)
        .format("%Y-%m-%d %H:%M")
        .to_string()
}

fn render_group(out: &mut String, group: &CacheGroupDiagnosis) {
    use std::fmt::Write;
    let label = |value: &Option<String>| value.clone().unwrap_or_else(|| "unknown".into());
    let _ = writeln!(
        out,
        "\n{} · {} · {} — {}",
        label(&group.agent_name),
        label(&group.provider),
        label(&group.model),
        status_label(group.status)
    );
    let mut rate = format!(
        "  Hit rate: {} conservative, {} with complete fields",
        percent(group.conservative_hit_rate),
        percent(group.complete_hit_rate)
    );
    if let Some(baseline) = &group.baseline {
        let change = baseline
            .hit_rate_change_pp
            .map_or_else(|| "n/a".into(), |pp| format!("{pp:+.1} pp"));
        let _ = write!(
            rate,
            " (baseline {}, {change}, {}{})",
            percent(baseline.conservative_hit_rate),
            wire_name(&baseline.trend),
            if baseline.sufficient_samples {
                ""
            } else {
                ", few samples"
            }
        );
    }
    let _ = writeln!(out, "{rate}");
    let coverage = &group.coverage;
    let _ = writeln!(
        out,
        "  Calls: {} ({} with usage, {} with cache-read counter, {} explicit zero, {} duplicates, {} failed)",
        coverage.calls,
        coverage.usage_calls,
        coverage.cache_read_field_calls,
        coverage.explicit_zero_read_calls,
        coverage.duplicate_calls,
        coverage.failed_calls
    );
    let t = &group.tokens;
    let _ = writeln!(
        out,
        "  Tokens: input {} · read {} · write {} · uncached {} · output {}",
        tokens(t.input),
        tokens(t.cache_read),
        tokens(t.cache_creation),
        tokens(t.uncached_input),
        tokens(t.output)
    );
    let bucket = |name: &str, bucket: &agentsight::health::cache::CacheLatencyBucket| {
        format!(
            "{name} {} / {} ({} calls, median input {})",
            p50(&bucket.ttft_ms),
            p50(&bucket.e2e_latency_ms),
            bucket.calls,
            bucket
                .median_input_tokens
                .map_or_else(|| "n/a".into(), tokens)
        )
    };
    let _ = writeln!(
        out,
        "  Latency p50, TTFT / end-to-end: {} · {}",
        bucket("hit", &group.latency.warm),
        bucket("miss", &group.latency.cold)
    );
    if let Some(cost) = &group.cost {
        let _ = writeln!(
            out,
            "  Cost: input {:.4} · output {:.4} · saved {:.4} against no cache · missed {:.4}{}",
            cost.input_cost,
            cost.output_cost,
            cost.cache_savings,
            cost.missed_savings,
            if cost.incompatible_calls > 0 {
                format!(
                    " · {} calls with incompatible counters skipped",
                    cost.incompatible_calls
                )
            } else {
                String::new()
            }
        );
    }
    for finding in &group.findings {
        let _ = writeln!(
            out,
            "  [{}] {}: calls {}, missed {} of {} unread tokens, latency p50 {} / {}",
            wire_name(&finding.kind),
            wire_name(&finding.reason),
            finding.calls,
            tokens(finding.missed_tokens),
            tokens(finding.unread_tokens),
            milliseconds(finding.ttft_ms_p50),
            milliseconds(finding.e2e_latency_ms_p50)
        );
        let _ = writeln!(out, "      {}", finding.recommendation);
        if let Some(example) = finding.examples.first() {
            let _ = writeln!(
                out,
                "      e.g. trace {} (session {}): input {}, read {}, previous input {}",
                example.trace_id.as_deref().unwrap_or("-"),
                example.session_id.as_deref().unwrap_or("-"),
                tokens(example.input_tokens),
                example
                    .cache_read_tokens
                    .map_or_else(|| "missing".into(), tokens),
                example
                    .previous_input_tokens
                    .map_or_else(|| "-".into(), tokens)
            );
        }
    }
}

fn render(diagnosis: &CacheDiagnosis) -> String {
    let mut out = format!(
        "Prompt cache, {} → {}",
        local_time(diagnosis.start_ns),
        local_time(diagnosis.end_ns)
    );
    if diagnosis.truncated {
        out.push_str(" (only the newest calls were read)");
    }
    out.push('\n');
    if diagnosis.groups.is_empty() {
        out.push_str("No completed LLM calls in this window.\n");
    }
    for group in &diagnosis.groups {
        render_group(&mut out, group);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<CacheCommand, structopt::clap::Error> {
        CacheCommand::from_iter_safe(std::iter::once("cache").chain(args.iter().copied()))
    }

    #[test]
    fn windows_follow_last_until_and_baseline_flags() {
        let hour = HOUR_NS;
        let plain = parse(&[]).unwrap().request(48 * hour).unwrap();
        assert_eq!((plain.start_ns, plain.end_ns), (24 * hour, 48 * hour));
        assert_eq!(plain.baseline, None);

        let compared = parse(&["--last", "2", "--compare"]).unwrap();
        let request = compared.request(10 * hour).unwrap();
        assert_eq!(request.baseline, Some((6 * hour, 8 * hour)));

        let pinned = parse(&[
            "--last",
            "1",
            "--until",
            "1970-01-01T10:00:00Z",
            "--baseline-until",
            "1970-01-01T05:00:00Z",
        ])
        .unwrap()
        .request(0)
        .unwrap();
        assert_eq!((pinned.start_ns, pinned.end_ns), (9 * hour, 10 * hour));
        assert_eq!(pinned.baseline, Some((4 * hour, 5 * hour)));
    }

    #[test]
    fn empty_agent_and_session_filters_are_unset() {
        let empty = parse(&["--agent", "", "--session", ""])
            .unwrap()
            .request(HOUR_NS)
            .unwrap();
        assert_eq!(empty.agent_name, None);
        assert_eq!(empty.session_id, None);

        let filtered = parse(&["--agent", "hermes", "--session", "session-1"])
            .unwrap()
            .request(HOUR_NS)
            .unwrap();
        assert_eq!(filtered.agent_name.as_deref(), Some("hermes"));
        assert_eq!(filtered.session_id.as_deref(), Some("session-1"));
    }

    #[test]
    fn rejects_conflicting_or_partial_flags() {
        assert!(parse(&["--compare", "--baseline-until", "1970-01-01T05:00:00Z"]).is_err());
        assert!(parse(&["--price-input", "12"]).is_err());
        assert!(parse(&["--price-output", "36"]).is_err());
        assert!(parse(&["--low-hit-rate", "2"]).is_err());
        assert!(parse(&["--until", "yesterday"]).is_err());
        for bad in ["NaN", "-1", "inf"] {
            let flag = format!("--price-input={bad}");
            let args = [
                flag.as_str(),
                "--price-cache-read",
                "1",
                "--price-cache-write",
                "15",
                "--price-output",
                "36",
            ];
            assert!(parse(&args).is_err(), "--price-input={bad} was accepted");
        }
        let priced = parse(&[
            "--price-input",
            "12",
            "--price-cache-read",
            "1",
            "--price-cache-write",
            "15",
            "--price-output",
            "36",
        ])
        .unwrap();
        assert_eq!(
            priced.options().prices.map(|prices| prices.input),
            Some(12.0)
        );
    }

    #[test]
    fn renders_an_empty_window() {
        let diagnosis = agentsight::health::cache::diagnose(
            Default::default(),
            0,
            HOUR_NS,
            &CacheDiagnosisOptions::default(),
        );
        assert!(render(&diagnosis).contains("No completed LLM calls"));
    }
}
