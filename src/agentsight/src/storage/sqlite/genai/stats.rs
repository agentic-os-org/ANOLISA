//! Token usage statistics and time-series query methods for GenAI SQLite store.

use rusqlite::params;

use super::{GenAISqliteStore, billed_input_col};

// ─── Query result types ────────────────────────────────────────────────────────

/// One data-point in a token time-series response
#[derive(Debug, serde::Serialize)]
pub struct TimeseriesBucket {
    pub bucket_start_ns: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub total_tokens: i64,
}

#[cfg(test)]
mod latency_tests {
    use super::{GenAISqliteStore, percentile, percentiles};
    use rusqlite::params;

    #[test]
    fn interpolates_latency_percentiles() {
        let values = vec![1.0, 2.0, 3.0, 4.0, 5.0];
        assert_eq!(percentile(&values, 50.0), Some(3.0));
        let summary = percentiles(vec![10.0, 20.0]).unwrap();
        assert_eq!(summary.p50, 15.0);
        assert_eq!(summary.p95, 19.5);
        assert!((summary.p99 - 19.9).abs() < 1e-12);
    }

    #[test]
    fn returns_none_for_empty_metric_samples() {
        assert!(percentiles(Vec::new()).is_none());
    }

    #[test]
    fn filters_latency_metrics_by_time_and_agent() {
        let path = std::env::temp_dir().join(format!(
            "agentsight_latency_metrics_{}.db",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let store =
            GenAISqliteStore::new_with_path(&path, crate::config::PeriodicStoragePolicy::default())
                .unwrap();
        {
            let conn = store.conn.lock().unwrap();
            for (call_id, agent, start) in [
                ("a1", "agent-a", 100_i64),
                ("b1", "agent-b", 200),
                ("a2", "agent-a", 300),
            ] {
                conn.execute(
                    "INSERT INTO genai_events
                     (event_type, status, call_id, start_timestamp_ns, end_timestamp_ns,
                      first_output_timestamp_ns, output_tokens, is_sse, agent_name, event_json)
                     VALUES ('llm_call', 'complete', ?1, ?2, ?3, ?4, 10, 1, ?5, '{}')",
                    params![call_id, start, start + 30, start + 10, agent],
                )
                .unwrap();
            }
            conn.execute(
                "INSERT INTO genai_events
                 (event_type, status, call_id, start_timestamp_ns, end_timestamp_ns,
                  first_output_timestamp_ns, output_tokens, is_sse, agent_name, event_json)
                 VALUES ('llm_call', 'complete', 'zero', 400, 430, 410, 0, 1, 'agent-zero', '{}')",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO genai_events
                 (event_type, status, call_id, start_timestamp_ns, end_timestamp_ns,
                  first_output_timestamp_ns, output_tokens, is_sse, process_name, event_json)
                 VALUES ('llm_call', 'complete', 'fallback-agent-call', 500, 550, 520, 10, 1,
                         'fallback-agent', '{}')",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO genai_events
                 (event_type, status, call_id, start_timestamp_ns, end_timestamp_ns,
                  first_output_timestamp_ns, output_tokens, is_sse, agent_name, event_json)
                 VALUES ('llm_call', 'complete', 'invalid-ttft', 700, 710, 720, 10, 1,
                         'agent-invalid', '{}')",
                [],
            )
            .unwrap();
        }
        let result = store.get_latency_metrics(50, 250, Some("agent-a")).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].agent_name.as_deref(), Some("agent-a"));
        assert_eq!(result[0].call_count, 1);
        assert_eq!(
            result[0].ttft_ms.as_ref().map(|value| value.p50),
            Some(0.00001)
        );
        let zero = store
            .get_latency_metrics(350, 450, Some("agent-zero"))
            .unwrap();
        assert_eq!(zero[0].streaming_call_count, 1);
        assert!(zero[0].tps_tokens_per_second.is_none());
        assert!(zero[0].tpot_ms_per_token.is_none());

        let fallback = store
            .get_latency_metrics(450, 600, Some("fallback-agent"))
            .unwrap();
        assert_eq!(fallback.len(), 1);
        assert_eq!(fallback[0].agent_name.as_deref(), Some("fallback-agent"));
        assert_eq!(
            fallback[0].ttft_ms.as_ref().map(|value| value.p50),
            Some(0.00002)
        );

        let invalid = store
            .get_latency_metrics(650, 750, Some("agent-invalid"))
            .unwrap();
        assert_eq!(invalid.len(), 1);
        assert!(invalid[0].ttft_ms.is_none());

        drop(store);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn filters_latency_metrics_case_insensitively_and_combines_variants() {
        let path = std::env::temp_dir().join(format!(
            "agentsight_latency_case_insensitive_{}.db",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let store =
            GenAISqliteStore::new_with_path(&path, crate::config::PeriodicStoragePolicy::default())
                .unwrap();
        {
            let conn = store.conn.lock().unwrap();
            for (call_id, agent, start, first, is_sse) in [
                ("upper", "Qoder", 100_i64, 110_i64, 1_i64),
                ("lower", "qoder", 200_i64, 220_i64, 0_i64),
            ] {
                conn.execute(
                    "INSERT INTO genai_events
                     (event_type, status, call_id, start_timestamp_ns, end_timestamp_ns,
                      first_output_timestamp_ns, output_tokens, is_sse, agent_name, event_json)
                     VALUES ('llm_call', 'complete', ?1, ?2, ?3, ?4, 10, ?5, ?6, '{}')",
                    params![call_id, start, start + 30, first, is_sse, agent],
                )
                .unwrap();
            }
        }

        for query_name in ["Qoder", "qoder"] {
            let summary = store
                .get_latency_metrics(50, 250, Some(query_name))
                .unwrap();
            assert_eq!(summary.len(), 1);
            assert_eq!(summary[0].agent_name.as_deref(), Some(query_name));
            assert_eq!(summary[0].call_count, 2);
            assert_eq!(summary[0].streaming_call_count, 1);
            let ttft_p50 = summary[0].ttft_ms.as_ref().unwrap().p50;
            assert!((ttft_p50 - 0.000015).abs() < 1e-12);
        }

        // The unfiltered view must agree: one agent, one summary.
        let all = store.get_latency_metrics(50, 250, None).unwrap();
        assert_eq!(all.len(), 1, "case variants split into {} rows", all.len());
        assert_eq!(all[0].call_count, 2);
        assert_eq!(all[0].streaming_call_count, 1);

        drop(store);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn streaming_call_count_uses_is_sse_when_ttft_is_missing() {
        let path = std::env::temp_dir().join(format!(
            "agentsight_streaming_count_{}.db",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let store =
            GenAISqliteStore::new_with_path(&path, crate::config::PeriodicStoragePolicy::default())
                .unwrap();
        {
            let conn = store.conn.lock().unwrap();
            conn.execute(
                "INSERT INTO genai_events
                 (event_type, status, call_id, start_timestamp_ns, end_timestamp_ns,
                  first_output_timestamp_ns, output_tokens, is_sse, agent_name, event_json)
                 VALUES ('llm_call', 'complete', 'sse-null', 100, 200, NULL, NULL, 1,
                         'agent-count', '{}')",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO genai_events
                 (event_type, status, call_id, start_timestamp_ns, end_timestamp_ns,
                  first_output_timestamp_ns, output_tokens, is_sse, agent_name, event_json)
                 VALUES ('llm_call', 'complete', 'non-sse', 100, 200, NULL, NULL, 0,
                         'agent-count', '{}')",
                [],
            )
            .unwrap();
        }

        let summary = store
            .get_latency_metrics(0, 300, Some("agent-count"))
            .unwrap();
        assert_eq!(summary[0].call_count, 2);
        assert_eq!(summary[0].streaming_call_count, 1);
        drop(store);
        let _ = std::fs::remove_file(path);
    }
}

/// One data-point in a per-model token time-series response
#[derive(Debug, serde::Serialize)]
pub struct ModelTimeseriesBucket {
    pub bucket_start_ns: i64,
    pub model: String,
    pub total_tokens: i64,
}

/// Per-agent token usage summary (all-time aggregation)
#[derive(Debug, serde::Serialize)]
pub struct AgentTokenSummary {
    pub agent_name: String,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub total_tokens: i64,
    pub request_count: i64,
}

/// Per-agent activity aggregated from recorded LLM calls.
#[derive(Debug, Clone, serde::Serialize)]
pub struct AgentActivitySummary {
    /// Canonical display name observed in stored events, falling back to the process name.
    pub agent_name: String,
    /// Most recent call start or completion timestamp in nanoseconds.
    pub last_seen_ns: i64,
    /// Number of recorded LLM calls.
    pub total_calls: i64,
    /// Input, output, and cache tokens across all calls.
    pub total_tokens: i64,
}

/// Percentiles for one latency or throughput metric.
#[derive(Debug, serde::Serialize)]
pub struct MetricPercentiles {
    pub p50: f64,
    pub p95: f64,
    pub p99: f64,
}

/// Aggregated LLM latency metrics for one agent or the requested filter.
#[derive(Debug, serde::Serialize)]
pub struct LatencyMetricsSummary {
    pub agent_name: Option<String>,
    pub call_count: usize,
    pub streaming_call_count: usize,
    pub ttft_ms: Option<MetricPercentiles>,
    pub tps_tokens_per_second: Option<MetricPercentiles>,
    pub tpot_ms_per_token: Option<MetricPercentiles>,
    pub e2e_latency_ms: Option<MetricPercentiles>,
}

#[derive(Debug)]
struct CallMetrics {
    agent_name: Option<String>,
    is_sse: bool,
    latency: CallLatency,
}

/// Latency and throughput of one recorded call.
///
/// Produced by [`call_latency`] alone, so the agent-grouped latency view and
/// the pid/session-grouped `top` rows cannot drift apart.
#[derive(Debug, Default)]
struct CallLatency {
    ttft_ms: Option<f64>,
    tps_tokens_per_second: Option<f64>,
    tpot_ms_per_token: Option<f64>,
    e2e_latency_ms: Option<f64>,
}

/// Derives TTFT / TPS / TPOT / end-to-end latency for one recorded call.
///
/// A sample is left `None` rather than reported when its timestamps are
/// inconsistent (first token before the call started, or an end at or before
/// the first token); TPS and TPOT additionally need positive `output_tokens`.
fn call_latency(
    start: i64,
    end: Option<i64>,
    first: Option<i64>,
    output_tokens: Option<i64>,
) -> CallLatency {
    let e2e_ns = end.filter(|end| *end > start).map(|end| end - start);
    let stream_ns = match (first, end) {
        (Some(first), Some(end)) if first >= start && end > first => Some(end - first),
        _ => None,
    };
    let tokens = output_tokens.filter(|tokens| *tokens > 0);
    CallLatency {
        ttft_ms: first
            .filter(|first| {
                *first >= start && (end.is_none() || end.is_some_and(|end| *first <= end))
            })
            .map(|first| (first - start) as f64 / 1_000_000.0),
        // By #2339 convention, TPS/TPOT use total output_tokens N; do not use N - 1.
        tps_tokens_per_second: stream_ns
            .zip(tokens)
            .map(|(duration, tokens)| tokens as f64 * 1_000_000_000.0 / duration as f64),
        tpot_ms_per_token: stream_ns
            .zip(tokens)
            .map(|(duration, tokens)| duration as f64 / 1_000_000.0 / tokens as f64),
        e2e_latency_ms: e2e_ns.map(|duration| duration as f64 / 1_000_000.0),
    }
}

/// Percentile summaries of every metric over one group of calls.
struct LatencyPercentiles {
    ttft_ms: Option<MetricPercentiles>,
    tps_tokens_per_second: Option<MetricPercentiles>,
    tpot_ms_per_token: Option<MetricPercentiles>,
    e2e_latency_ms: Option<MetricPercentiles>,
}

fn latency_percentiles(latencies: &[CallLatency]) -> LatencyPercentiles {
    LatencyPercentiles {
        ttft_ms: percentiles(latencies.iter().filter_map(|call| call.ttft_ms).collect()),
        tps_tokens_per_second: percentiles(
            latencies
                .iter()
                .filter_map(|call| call.tps_tokens_per_second)
                .collect(),
        ),
        tpot_ms_per_token: percentiles(
            latencies
                .iter()
                .filter_map(|call| call.tpot_ms_per_token)
                .collect(),
        ),
        e2e_latency_ms: percentiles(
            latencies
                .iter()
                .filter_map(|call| call.e2e_latency_ms)
                .collect(),
        ),
    }
}

/// Label for a group whose calls carry no agent name, process name, PID or
/// session ID — reachable only for hand-written rows, but the table must not
/// render a blank name.
const UNKNOWN_LABEL: &str = "(unknown)";

/// Which column [`GenAISqliteStore::get_top_rows`] groups recorded calls by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TopGrouping {
    /// One row per agent process (PID).
    Process,
    /// One row per session ID.
    Session,
}

/// One aggregated row of the live `top` view.
#[derive(Debug, serde::Serialize)]
pub struct TopRow {
    /// Agent process ID, or `None` when the grouped calls carried no PID.
    pub pid: Option<i64>,
    /// Session ID, or `None` when grouping by process or the calls carried none.
    pub session_id: Option<String>,
    /// Display name: stored agent name, else process name, else the grouping key.
    pub label: String,
    /// Process name of the row's most recent call.
    pub process_name: Option<String>,
    /// Model of the row's most recent call.
    pub model: Option<String>,
    /// Completed LLM calls inside the window.
    pub call_count: usize,
    /// Subset of those calls that streamed over SSE.
    pub streaming_call_count: usize,
    /// Billed input tokens, following the same cache-aware rule as token views.
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub total_tokens: i64,
    /// Start of the row's earliest call, in nanoseconds since the Unix epoch.
    pub first_seen_ns: i64,
    /// Completion (or start, when unfinished) of the row's latest call, in nanoseconds.
    pub last_seen_ns: i64,
    pub ttft_ms: Option<MetricPercentiles>,
    pub tps_tokens_per_second: Option<MetricPercentiles>,
    pub tpot_ms_per_token: Option<MetricPercentiles>,
    pub e2e_latency_ms: Option<MetricPercentiles>,
}

/// Window totals for the `top` header, independent of how rows are grouped.
#[derive(Debug, serde::Serialize)]
pub struct TopWindowSummary {
    /// Distinct agent processes that recorded a call in the window.
    pub process_count: i64,
    /// Distinct sessions that recorded a call in the window.
    pub session_count: i64,
    /// Completed LLM calls in the window.
    pub call_count: i64,
    /// Billed input tokens, following the same cache-aware rule as [`TopRow::input_tokens`].
    pub input_tokens: i64,
    pub output_tokens: i64,
    /// Start of the earliest call in the window, in nanoseconds since the Unix
    /// epoch; `0` when the window holds no call.
    pub first_seen_ns: i64,
    /// Completion (or start, when unfinished) of the latest call in the window,
    /// in nanoseconds since the Unix epoch; `0` when the window holds no call.
    pub last_seen_ns: i64,
}

/// Grouping key of one `top` row.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum TopKey {
    /// Fallback for calls whose grouping column is NULL or blank.
    Agent(String),
    Pid(i64),
    Session(String),
}

/// One recorded call attributed to a `top` group.
struct TopCall {
    key: TopKey,
    pid: Option<i64>,
    session_id: Option<String>,
    agent_label: Option<String>,
    process_name: Option<String>,
    model: Option<String>,
    start: i64,
    seen: i64,
    input_tokens: i64,
    output_tokens: i64,
    is_sse: bool,
    latency: CallLatency,
}

/// Accumulator over the calls of one `top` group.
#[derive(Default)]
struct TopGroup {
    call_count: usize,
    streaming_call_count: usize,
    input_tokens: i64,
    output_tokens: i64,
    first_seen_ns: Option<i64>,
    last_seen_ns: Option<i64>,
    pid: Option<i64>,
    session_id: Option<String>,
    label: String,
    process_name: Option<String>,
    model: Option<String>,
    latencies: Vec<CallLatency>,
}

impl TopGroup {
    fn add(&mut self, call: TopCall) {
        self.call_count += 1;
        self.streaming_call_count += usize::from(call.is_sse);
        self.input_tokens += call.input_tokens;
        self.output_tokens += call.output_tokens;
        let first_seen = self
            .first_seen_ns
            .map_or(call.start, |seen| seen.min(call.start));
        self.first_seen_ns = Some(first_seen);
        // Identity follows the most recent call, so a process that changed its
        // reported name shows the current one rather than the first one seen.
        if call.seen >= self.last_seen_ns.unwrap_or(i64::MIN) {
            self.last_seen_ns = Some(call.seen);
            self.pid = call.pid;
            self.session_id = call.session_id.clone();
            self.process_name = call.process_name.clone();
            self.model = call.model.clone();
            if let Some(label) = call.agent_label {
                self.label = label;
            }
        }
        self.latencies.push(call.latency);
    }

    fn into_row(self, key: TopKey) -> TopRow {
        // A group keyed by the agent fallback may have no name at all; fall back
        // to the key so the row is never blank in the table.
        let fallback = match key {
            TopKey::Agent(name) if !name.is_empty() => name,
            TopKey::Pid(pid) => pid.to_string(),
            TopKey::Session(session) => session,
            TopKey::Agent(_) => UNKNOWN_LABEL.to_string(),
        };
        let label = if self.label.is_empty() {
            fallback
        } else {
            self.label
        };
        let percentiles = latency_percentiles(&self.latencies);
        TopRow {
            pid: self.pid,
            session_id: self.session_id,
            label,
            process_name: self.process_name,
            model: self.model,
            call_count: self.call_count,
            streaming_call_count: self.streaming_call_count,
            input_tokens: self.input_tokens,
            output_tokens: self.output_tokens,
            total_tokens: self.input_tokens + self.output_tokens,
            first_seen_ns: self.first_seen_ns.unwrap_or_default(),
            last_seen_ns: self.last_seen_ns.unwrap_or_default(),
            ttft_ms: percentiles.ttft_ms,
            tps_tokens_per_second: percentiles.tps_tokens_per_second,
            tpot_ms_per_token: percentiles.tpot_ms_per_token,
            e2e_latency_ms: percentiles.e2e_latency_ms,
        }
    }
}

fn percentile(sorted: &[f64], pct: f64) -> Option<f64> {
    if sorted.is_empty() {
        return None;
    }
    let position = (pct / 100.0) * (sorted.len() - 1) as f64;
    let lower = position.floor() as usize;
    let upper = position.ceil() as usize;
    let fraction = position - lower as f64;
    Some(sorted[lower] * (1.0 - fraction) + sorted[upper] * fraction)
}

fn percentiles(mut values: Vec<f64>) -> Option<MetricPercentiles> {
    values.sort_by(f64::total_cmp);
    Some(MetricPercentiles {
        p50: percentile(&values, 50.0)?,
        p95: percentile(&values, 95.0)?,
        p99: percentile(&values, 99.0)?,
    })
}

impl GenAISqliteStore {
    /// Returns percentile latency metrics grouped by agent.
    pub fn get_latency_metrics(
        &self,
        start_ns: i64,
        end_ns: i64,
        agent_name: Option<&str>,
    ) -> Result<Vec<LatencyMetricsSummary>, Box<dyn std::error::Error>> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let sql = if agent_name.is_some() {
            "SELECT ?3 AS agent_name,
                    start_timestamp_ns, end_timestamp_ns,
                    first_output_timestamp_ns, output_tokens, is_sse
             FROM genai_events
             WHERE event_type = 'llm_call' AND status = 'complete'
               AND start_timestamp_ns BETWEEN ?1 AND ?2
               AND COALESCE(agent_name, process_name) COLLATE NOCASE = ?3 COLLATE NOCASE"
        } else {
            "SELECT COALESCE(agent_name, process_name) AS agent_name,
                    start_timestamp_ns, end_timestamp_ns,
                    first_output_timestamp_ns, output_tokens, is_sse
             FROM genai_events
             WHERE event_type = 'llm_call' AND status = 'complete'
               AND start_timestamp_ns BETWEEN ?1 AND ?2"
        };
        let mut stmt = conn.prepare(sql)?;
        let map_row = |row: &rusqlite::Row<'_>| -> rusqlite::Result<CallMetrics> {
            let start: i64 = row.get(1)?;
            let end: Option<i64> = row.get(2)?;
            let first: Option<i64> = row.get(3)?;
            let output_tokens: Option<i64> = row.get(4)?;
            let is_sse: Option<i64> = row.get(5)?;
            Ok(CallMetrics {
                agent_name: row.get(0)?,
                is_sse: is_sse == Some(1),
                latency: call_latency(start, end, first, output_tokens),
            })
        };
        let calls = if let Some(name) = agent_name {
            stmt.query_map(params![start_ns, end_ns, name], map_row)?
                .collect::<Result<Vec<_>, _>>()?
        } else {
            stmt.query_map(params![start_ns, end_ns], map_row)?
                .collect::<Result<Vec<_>, _>>()?
        };

        // Group case-insensitively, like the filtered query (COLLATE NOCASE)
        // and every other agent-attributed view: "Qoder" and "qoder" are one
        // agent, not two. The first name seen labels the group.
        type AgentGroup = (Option<String>, usize, Vec<CallLatency>);
        let mut grouped: std::collections::BTreeMap<Option<String>, AgentGroup> =
            std::collections::BTreeMap::new();
        for call in calls {
            let key = call.agent_name.as_deref().map(|name| name.to_lowercase());
            let group = grouped
                .entry(key)
                .or_insert_with(|| (call.agent_name.clone(), 0, Vec::new()));
            group.1 += usize::from(call.is_sse);
            group.2.push(call.latency);
        }
        Ok(grouped
            .into_iter()
            .map(|(_, (agent_name, streaming_call_count, latencies))| {
                let call_count = latencies.len();
                let percentiles = latency_percentiles(&latencies);
                LatencyMetricsSummary {
                    agent_name,
                    call_count,
                    streaming_call_count,
                    ttft_ms: percentiles.ttft_ms,
                    tps_tokens_per_second: percentiles.tps_tokens_per_second,
                    tpot_ms_per_token: percentiles.tpot_ms_per_token,
                    e2e_latency_ms: percentiles.e2e_latency_ms,
                }
            })
            .collect())
    }

    /// Returns one row per agent process or per session for a live `top` view.
    ///
    /// Metrics use the same per-call derivation as [`Self::get_latency_metrics`]
    /// and are reported as p50/p95/p99 over the window. Calls whose grouping
    /// column is NULL or blank are bucketed by agent label, so unattributed
    /// traffic stays visible instead of merging into one anonymous row; `pid`
    /// and `session_id` on each row come from its most recent call.
    ///
    /// Rows come back in grouping-key order; callers sort them for display.
    ///
    /// # Errors
    ///
    /// Propagates SQLite errors. `agent_name` matches case-insensitively, like
    /// every other agent-attributed view.
    pub fn get_top_rows(
        &self,
        start_ns: i64,
        end_ns: i64,
        grouping: TopGrouping,
        agent_name: Option<&str>,
        pid: Option<i64>,
    ) -> Result<Vec<TopRow>, Box<dyn std::error::Error>> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        // One statement serves both groupings: the key is chosen in Rust, and
        // the optional filters stay parameterized (`? IS NULL OR …`) instead of
        // being concatenated into the SQL.
        let mut stmt = conn.prepare(concat!(
            "SELECT pid, session_id,
                    COALESCE(NULLIF(TRIM(agent_name), ''),
                             NULLIF(TRIM(process_name), '')) AS agent_label,
                    process_name, model,
                    start_timestamp_ns, end_timestamp_ns, first_output_timestamp_ns,
                    output_tokens, is_sse,
                    COALESCE(",
            billed_input_col!(),
            ", 0) AS billed_input_tokens
             FROM genai_events
             WHERE event_type = 'llm_call' AND status = 'complete'
               AND start_timestamp_ns BETWEEN ?1 AND ?2
               AND (?3 IS NULL
                    OR COALESCE(agent_name, process_name) COLLATE NOCASE = ?3 COLLATE NOCASE)
               AND (?4 IS NULL OR pid = ?4)"
        ))?;

        let mut groups: std::collections::BTreeMap<TopKey, TopGroup> =
            std::collections::BTreeMap::new();
        let calls = stmt.query_map(params![start_ns, end_ns, agent_name, pid], |row| {
            let pid: Option<i64> = row.get(0)?;
            // Normalized the same way the window summary counts sessions
            // (`NULLIF(TRIM(session_id), '')`), so a blank session groups under
            // the agent label instead of becoming its own unnamed row.
            let session_id: Option<String> = row
                .get::<_, Option<String>>(1)?
                .map(|session| session.trim().to_string())
                .filter(|session| !session.is_empty());
            let agent_label: Option<String> = row.get(2)?;
            let process_name: Option<String> = row.get(3)?;
            let model: Option<String> = row.get(4)?;
            let start: i64 = row.get(5)?;
            let end: Option<i64> = row.get(6)?;
            let first: Option<i64> = row.get(7)?;
            let output_tokens: Option<i64> = row.get(8)?;
            let is_sse: Option<i64> = row.get(9)?;
            let input_tokens: i64 = row.get(10)?;
            Ok(TopCall {
                key: match grouping {
                    TopGrouping::Process => match pid {
                        Some(pid) => TopKey::Pid(pid),
                        None => TopKey::Agent(agent_label.clone().unwrap_or_default()),
                    },
                    TopGrouping::Session => match &session_id {
                        Some(session) => TopKey::Session(session.clone()),
                        None => TopKey::Agent(agent_label.clone().unwrap_or_default()),
                    },
                },
                pid,
                session_id,
                agent_label,
                process_name,
                model,
                start,
                seen: end.filter(|end| *end > start).unwrap_or(start),
                input_tokens,
                output_tokens: output_tokens.unwrap_or_default().max(0),
                is_sse: is_sse == Some(1),
                latency: call_latency(start, end, first, output_tokens),
            })
        })?;
        for call in calls {
            let call = call?;
            groups.entry(call.key.clone()).or_default().add(call);
        }
        Ok(groups
            .into_iter()
            .map(|(key, group)| group.into_row(key))
            .collect())
    }

    /// Returns the window totals shown in the `top` header.
    ///
    /// Counted independently of [`Self::get_top_rows`] so the header stays
    /// correct whichever grouping the rows use: process and session counts are
    /// both distinct counts over the same filtered window.
    ///
    /// # Errors
    ///
    /// Propagates SQLite errors.
    pub fn get_top_window_summary(
        &self,
        start_ns: i64,
        end_ns: i64,
        agent_name: Option<&str>,
        pid: Option<i64>,
    ) -> Result<TopWindowSummary, Box<dyn std::error::Error>> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let mut stmt = conn.prepare(concat!(
            "SELECT COUNT(DISTINCT pid),
                    COUNT(DISTINCT NULLIF(TRIM(session_id), '')),
                    COUNT(*),
                    COALESCE(SUM(",
            billed_input_col!(),
            "), 0),
                    COALESCE(SUM(COALESCE(output_tokens, 0)), 0),
                    COALESCE(MAX(CASE WHEN end_timestamp_ns > start_timestamp_ns
                                      THEN end_timestamp_ns
                                      ELSE start_timestamp_ns END), 0),
                    COALESCE(MIN(start_timestamp_ns), 0)
             FROM genai_events
             WHERE event_type = 'llm_call' AND status = 'complete'
               AND start_timestamp_ns BETWEEN ?1 AND ?2
               AND (?3 IS NULL
                    OR COALESCE(agent_name, process_name) COLLATE NOCASE = ?3 COLLATE NOCASE)
               AND (?4 IS NULL OR pid = ?4)"
        ))?;
        stmt.query_row(params![start_ns, end_ns, agent_name, pid], |row| {
            Ok(TopWindowSummary {
                process_count: row.get(0)?,
                session_count: row.get(1)?,
                call_count: row.get(2)?,
                input_tokens: row.get(3)?,
                output_tokens: row.get(4)?,
                last_seen_ns: row.get(5)?,
                first_seen_ns: row.get(6)?,
            })
        })
        .map_err(Into::into)
    }

    /// One bucket in a token time-series query.
    pub fn get_token_timeseries(
        &self,
        start_ns: i64,
        end_ns: i64,
        agent_name: Option<&str>,
        bucket_count: u32,
    ) -> Result<Vec<TimeseriesBucket>, Box<dyn std::error::Error>> {
        let bucket_count = bucket_count.max(1);
        let range_ns = end_ns.saturating_sub(start_ns).max(1);
        // SQLite evaluates x/0 as NULL, which would collapse every row into
        // one NULL bucket and make bucket_start_ns unreadable as an integer.
        // Round the width UP: floor division lets the last bucket index reach
        // range_ns / bucket_ns >= bucket_count (up to ~2x when the span is
        // just under twice the requested count), so callers got more buckets
        // than they asked for. With ceil, every index stays < bucket_count —
        // except when the range is an exact multiple of the count: the width
        // is then exact, and the inclusive BETWEEN puts a row at end_ns in
        // index == bucket_count, a phantom bucket starting at end_ns itself.
        // Clamp the index in SQL so that row merges into the last bucket.
        let bucket_ns = range_ns.saturating_add(bucket_count as i64 - 1) / bucket_count as i64;
        let bucket_ns = bucket_ns.max(1);

        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());

        // Build query with optional agent_name filter
        let sql: &str = if agent_name.is_some() {
            concat!(
                "SELECT
                MIN((start_timestamp_ns - ?1) / ?3, ?5 - 1)            AS bucket_idx,
                ?1 + MIN((start_timestamp_ns - ?1) / ?3, ?5 - 1) * ?3  AS bucket_start_ns,
                COALESCE(SUM(",
                billed_input_col!(),
                "), 0)                AS input_tokens,
                COALESCE(SUM(output_tokens), 0)           AS output_tokens,
                COALESCE(SUM(COALESCE((",
                billed_input_col!(),
                "), 0) + COALESCE(output_tokens, 0)), 0) AS total_tokens
             FROM genai_events
             WHERE event_type = 'llm_call'
               AND start_timestamp_ns BETWEEN ?1 AND ?2
               AND COALESCE(agent_name, process_name) COLLATE NOCASE = ?4 COLLATE NOCASE
             GROUP BY bucket_idx
             ORDER BY bucket_idx ASC"
            )
        } else {
            concat!(
                "SELECT
                MIN((start_timestamp_ns - ?1) / ?3, ?4 - 1)            AS bucket_idx,
                ?1 + MIN((start_timestamp_ns - ?1) / ?3, ?4 - 1) * ?3  AS bucket_start_ns,
                COALESCE(SUM(",
                billed_input_col!(),
                "), 0)                AS input_tokens,
                COALESCE(SUM(output_tokens), 0)           AS output_tokens,
                COALESCE(SUM(COALESCE((",
                billed_input_col!(),
                "), 0) + COALESCE(output_tokens, 0)), 0) AS total_tokens
             FROM genai_events
             WHERE event_type = 'llm_call'
               AND start_timestamp_ns BETWEEN ?1 AND ?2
             GROUP BY bucket_idx
             ORDER BY bucket_idx ASC"
            )
        };

        let rows: Vec<TimeseriesBucket> = if let Some(name) = agent_name {
            let mut stmt = conn.prepare(sql)?;
            stmt.query_map(
                params![start_ns, end_ns, bucket_ns, name, bucket_count as i64],
                |row| {
                    Ok(TimeseriesBucket {
                        bucket_start_ns: row.get(1)?,
                        input_tokens: row.get(2)?,
                        output_tokens: row.get(3)?,
                        total_tokens: row.get(4)?,
                    })
                },
            )?
            .collect::<Result<Vec<_>, _>>()?
        } else {
            let mut stmt = conn.prepare(sql)?;
            stmt.query_map(
                params![start_ns, end_ns, bucket_ns, bucket_count as i64],
                |row| {
                    Ok(TimeseriesBucket {
                        bucket_start_ns: row.get(1)?,
                        input_tokens: row.get(2)?,
                        output_tokens: row.get(3)?,
                        total_tokens: row.get(4)?,
                    })
                },
            )?
            .collect::<Result<Vec<_>, _>>()?
        };

        Ok(rows)
    }

    /// Model-level token breakdown time-series.
    pub fn get_model_timeseries(
        &self,
        start_ns: i64,
        end_ns: i64,
        agent_name: Option<&str>,
        bucket_count: u32,
    ) -> Result<Vec<ModelTimeseriesBucket>, Box<dyn std::error::Error>> {
        let bucket_count = bucket_count.max(1);
        let range_ns = end_ns.saturating_sub(start_ns).max(1);
        // SQLite evaluates x/0 as NULL, which would collapse every row into
        // one NULL bucket and make bucket_start_ns unreadable as an integer.
        // Round the width UP: floor division lets the last bucket index reach
        // range_ns / bucket_ns >= bucket_count (up to ~2x when the span is
        // just under twice the requested count), so callers got more buckets
        // than they asked for. With ceil, every index stays < bucket_count —
        // except when the range is an exact multiple of the count: the width
        // is then exact, and the inclusive BETWEEN puts a row at end_ns in
        // index == bucket_count, a phantom bucket starting at end_ns itself.
        // Clamp the index in SQL so that row merges into the last bucket.
        let bucket_ns = range_ns.saturating_add(bucket_count as i64 - 1) / bucket_count as i64;
        let bucket_ns = bucket_ns.max(1);

        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());

        // Group on the *labelled* model, not the raw column: a NULL model and
        // the literal `'unknown'` are the same series to every consumer (the
        // dashboard keys the rows by this label and overwrites on collision),
        // so grouping on the raw column returned two rows with the same
        // `(bucket_start_ns, model)` key and one total was silently dropped.
        // SQLite resolves the bare column here rather than the output alias,
        // so the `COALESCE` has to be repeated in the `GROUP BY`. The group
        // key also collates NOCASE, like every agent-attributed view (the
        // filter one line up, `list_agent_names`, the latency grouping):
        // different clients spell the same model with different case
        // ("gpt-4o" / "GPT-4o"), and case-sensitive grouping split one model
        // into two series that the dashboard keyed and rendered separately.
        let sql: &str = if agent_name.is_some() {
            concat!(
                "SELECT
                MIN((start_timestamp_ns - ?1) / ?3, ?5 - 1)            AS bucket_idx,
                ?1 + MIN((start_timestamp_ns - ?1) / ?3, ?5 - 1) * ?3  AS bucket_start_ns,
                COALESCE(model, 'unknown')                 AS model,
                COALESCE(SUM(COALESCE((",
                billed_input_col!(),
                "), 0) + COALESCE(output_tokens, 0)), 0) AS total_tokens
             FROM genai_events
             WHERE event_type = 'llm_call'
               AND start_timestamp_ns BETWEEN ?1 AND ?2
               AND COALESCE(agent_name, process_name) COLLATE NOCASE = ?4 COLLATE NOCASE
             GROUP BY bucket_idx, COALESCE(model, 'unknown') COLLATE NOCASE
             ORDER BY bucket_idx ASC"
            )
        } else {
            concat!(
                "SELECT
                MIN((start_timestamp_ns - ?1) / ?3, ?4 - 1)            AS bucket_idx,
                ?1 + MIN((start_timestamp_ns - ?1) / ?3, ?4 - 1) * ?3  AS bucket_start_ns,
                COALESCE(model, 'unknown')                 AS model,
                COALESCE(SUM(COALESCE((",
                billed_input_col!(),
                "), 0) + COALESCE(output_tokens, 0)), 0) AS total_tokens
             FROM genai_events
             WHERE event_type = 'llm_call'
               AND start_timestamp_ns BETWEEN ?1 AND ?2
             GROUP BY bucket_idx, COALESCE(model, 'unknown') COLLATE NOCASE
             ORDER BY bucket_idx ASC"
            )
        };

        let rows: Vec<ModelTimeseriesBucket> = if let Some(name) = agent_name {
            let mut stmt = conn.prepare(sql)?;
            stmt.query_map(
                params![start_ns, end_ns, bucket_ns, name, bucket_count as i64],
                |row| {
                    Ok(ModelTimeseriesBucket {
                        bucket_start_ns: row.get(1)?,
                        model: row.get(2)?,
                        total_tokens: row.get(3)?,
                    })
                },
            )?
            .collect::<Result<Vec<_>, _>>()?
        } else {
            let mut stmt = conn.prepare(sql)?;
            stmt.query_map(
                params![start_ns, end_ns, bucket_ns, bucket_count as i64],
                |row| {
                    Ok(ModelTimeseriesBucket {
                        bucket_start_ns: row.get(1)?,
                        model: row.get(2)?,
                        total_tokens: row.get(3)?,
                    })
                },
            )?
            .collect::<Result<Vec<_>, _>>()?
        };

        Ok(rows)
    }

    /// Returns all historical LLM activity grouped by Agent or process name.
    pub fn list_agent_activity_summaries(
        &self,
    ) -> Result<Vec<AgentActivitySummary>, Box<dyn std::error::Error>> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let mut stmt = conn.prepare(concat!(
            "SELECT MIN(COALESCE(NULLIF(TRIM(agent_name), ''),
                                NULLIF(TRIM(process_name), ''))) AS display_name,
                    MAX(CASE WHEN end_timestamp_ns IS NOT NULL AND end_timestamp_ns > 0
                             THEN end_timestamp_ns ELSE start_timestamp_ns END) AS last_seen_ns,
                    COUNT(*) AS total_calls,
                    COALESCE(SUM(COALESCE(",
            billed_input_col!(),
            ", 0)
                               + COALESCE(output_tokens, 0)), 0) AS total_tokens
             FROM genai_events
             WHERE event_type = 'llm_call'
               AND COALESCE(NULLIF(TRIM(agent_name), ''),
                            NULLIF(TRIM(process_name), '')) IS NOT NULL
             GROUP BY COALESCE(NULLIF(TRIM(agent_name), ''),
                               NULLIF(TRIM(process_name), '')) COLLATE NOCASE
             ORDER BY last_seen_ns DESC, display_name ASC"
        ))?;
        let rows = stmt.query_map([], |row| {
            Ok(AgentActivitySummary {
                agent_name: row.get(0)?,
                last_seen_ns: row.get(1)?,
                total_calls: row.get(2)?,
                total_tokens: row.get(3)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    /// Return per-agent token usage aggregated over all recorded history.
    ///
    /// Groups by `COALESCE(agent_name, process_name, 'unknown') COLLATE NOCASE`
    /// so that every LLM call is attributed to some label even when agent_name
    /// is NULL, and case variants of one agent stay a single series — the same
    /// rule `list_agent_activity_summaries` and the latency metrics use. The
    /// reported label is the group's `MIN` for a stable spelling.
    pub fn get_agent_token_summary(
        &self,
    ) -> Result<Vec<AgentTokenSummary>, Box<dyn std::error::Error>> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let mut stmt = conn.prepare(concat!(
            "SELECT MIN(COALESCE(agent_name, process_name, 'unknown')) AS agent,
                    COALESCE(SUM(",
            billed_input_col!(),
            "), 0)      AS input_tokens,
                    COALESCE(SUM(output_tokens), 0) AS output_tokens,
                    COALESCE(SUM(COALESCE((",
            billed_input_col!(),
            "), 0) + COALESCE(output_tokens, 0)), 0) AS total_tokens,
                    COUNT(*)                        AS request_count
             FROM genai_events
             WHERE event_type = 'llm_call'
             GROUP BY COALESCE(agent_name, process_name, 'unknown') COLLATE NOCASE
             ORDER BY total_tokens DESC"
        ))?;
        let rows = stmt.query_map([], |row| {
            Ok(AgentTokenSummary {
                agent_name: row.get(0)?,
                input_tokens: row.get(1)?,
                output_tokens: row.get(2)?,
                total_tokens: row.get(3)?,
                request_count: row.get(4)?,
            })
        })?;
        let mut result = Vec::new();
        for row in rows {
            result.push(row?);
        }
        Ok(result)
    }
}

#[cfg(test)]
mod top_tests {
    use super::{GenAISqliteStore, TopGrouping};
    use rusqlite::params;

    /// One stored `llm_call`; every field maps to a `genai_events` column.
    #[derive(Default)]
    struct Call {
        pid: Option<i64>,
        session_id: Option<&'static str>,
        agent_name: Option<&'static str>,
        process_name: Option<&'static str>,
        model: Option<&'static str>,
        provider: Option<&'static str>,
        start_ns: i64,
        end_ns: Option<i64>,
        first_output_ns: Option<i64>,
        input_tokens: i64,
        cache_creation_tokens: i64,
        cache_read_tokens: i64,
        includes_cache: bool,
        output_tokens: i64,
        is_sse: bool,
        /// `None` means `'complete'`, the default every view counts.
        status: Option<&'static str>,
    }

    impl Call {
        fn insert(&self, conn: &rusqlite::Connection, call_id: &str) {
            conn.execute(
                "INSERT INTO genai_events
                 (event_type, status, call_id, pid, session_id, agent_name, process_name,
                  model, provider, start_timestamp_ns, end_timestamp_ns,
                  first_output_timestamp_ns, input_tokens, cache_creation_tokens,
                  cache_read_tokens, input_tokens_includes_cache, output_tokens, is_sse,
                  event_json)
                 VALUES ('llm_call', ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12,
                         ?13, ?14, ?15, ?16, ?17, '{}')",
                params![
                    self.status.unwrap_or("complete"),
                    call_id,
                    self.pid,
                    self.session_id,
                    self.agent_name,
                    self.process_name,
                    self.model,
                    self.provider,
                    self.start_ns,
                    self.end_ns,
                    self.first_output_ns,
                    self.input_tokens,
                    self.cache_creation_tokens,
                    self.cache_read_tokens,
                    self.includes_cache as i64,
                    self.output_tokens,
                    self.is_sse as i64,
                ],
            )
            .unwrap();
        }
    }

    fn store_with(name: &str, calls: &[Call]) -> (GenAISqliteStore, std::path::PathBuf) {
        let path =
            std::env::temp_dir().join(format!("agentsight_top_{name}_{}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let store =
            GenAISqliteStore::new_with_path(&path, crate::config::PeriodicStoragePolicy::default())
                .unwrap();
        {
            let conn = store.conn.lock().unwrap();
            for (index, call) in calls.iter().enumerate() {
                call.insert(&conn, &format!("call-{index}"));
            }
        }
        (store, path)
    }

    fn finish(store: GenAISqliteStore, path: std::path::PathBuf) {
        drop(store);
        let _ = std::fs::remove_file(path);
    }

    /// Two streamed calls of one process, one call of a second process, and an
    /// interrupted call that no view may count.
    fn two_process_fixture() -> Vec<Call> {
        vec![
            Call {
                pid: Some(111),
                session_id: Some("s-1"),
                agent_name: Some("claude-code"),
                process_name: Some("claude"),
                model: Some("claude-4"),
                start_ns: 1_000_000_000,
                end_ns: Some(1_500_000_000),
                first_output_ns: Some(1_100_000_000),
                input_tokens: 100,
                output_tokens: 20,
                is_sse: true,
                ..Default::default()
            },
            Call {
                pid: Some(111),
                session_id: Some("s-1"),
                agent_name: Some("claude-code"),
                process_name: Some("claude"),
                model: Some("claude-4"),
                start_ns: 2_000_000_000,
                end_ns: Some(2_300_000_000),
                first_output_ns: Some(2_050_000_000),
                input_tokens: 50,
                output_tokens: 10,
                is_sse: true,
                ..Default::default()
            },
            Call {
                pid: Some(222),
                session_id: Some("s-2"),
                agent_name: Some("qoder"),
                process_name: Some("qoder-cli"),
                model: Some("gpt-x"),
                start_ns: 3_000_000_000,
                // end == start: no end-to-end or streaming sample may be derived
                end_ns: Some(3_000_000_000),
                input_tokens: 10,
                ..Default::default()
            },
            Call {
                pid: Some(111),
                session_id: Some("s-1"),
                agent_name: Some("claude-code"),
                start_ns: 4_000_000_000,
                end_ns: Some(4_500_000_000),
                output_tokens: 99,
                status: Some("interrupted"),
                ..Default::default()
            },
        ]
    }

    #[test]
    fn groups_rows_by_process_and_derives_metrics_per_call() {
        let (store, path) = store_with("process", &two_process_fixture());
        let rows = store
            .get_top_rows(0, 10_000_000_000, TopGrouping::Process, None, None)
            .unwrap();
        assert_eq!(rows.len(), 2, "the interrupted call must not add a row");

        let claude = &rows[0];
        assert_eq!(claude.pid, Some(111));
        assert_eq!(claude.label, "claude-code");
        assert_eq!(claude.process_name.as_deref(), Some("claude"));
        assert_eq!(claude.model.as_deref(), Some("claude-4"));
        assert_eq!(claude.session_id.as_deref(), Some("s-1"));
        assert_eq!(claude.call_count, 2);
        assert_eq!(claude.streaming_call_count, 2);
        assert_eq!(claude.input_tokens, 150);
        assert_eq!(claude.output_tokens, 30);
        assert_eq!(claude.total_tokens, 180);
        assert_eq!(claude.first_seen_ns, 1_000_000_000);
        assert_eq!(claude.last_seen_ns, 2_300_000_000);
        assert_eq!(claude.ttft_ms.as_ref().map(|p| p.p50), Some(75.0));
        assert_eq!(
            claude.tps_tokens_per_second.as_ref().map(|p| p.p50),
            Some(45.0)
        );
        assert_eq!(claude.tpot_ms_per_token.as_ref().map(|p| p.p50), Some(22.5));
        assert_eq!(claude.e2e_latency_ms.as_ref().map(|p| p.p50), Some(400.0));

        let qoder = &rows[1];
        assert_eq!(qoder.pid, Some(222));
        assert_eq!(qoder.label, "qoder");
        assert_eq!(qoder.call_count, 1);
        assert_eq!(qoder.streaming_call_count, 0);
        assert_eq!(qoder.first_seen_ns, 3_000_000_000);
        assert_eq!(qoder.last_seen_ns, 3_000_000_000);
        assert!(qoder.ttft_ms.is_none());
        assert!(qoder.tps_tokens_per_second.is_none());
        assert!(qoder.tpot_ms_per_token.is_none());
        assert!(qoder.e2e_latency_ms.is_none());
        finish(store, path);
    }

    #[test]
    fn groups_rows_by_session_and_falls_back_to_the_agent_label() {
        let calls = vec![
            Call {
                pid: Some(111),
                session_id: Some("s-1"),
                agent_name: Some("claude-code"),
                start_ns: 1_000_000_000,
                end_ns: Some(1_500_000_000),
                output_tokens: 20,
                ..Default::default()
            },
            // Whitespace around the ID is the same session, not a second one.
            Call {
                pid: Some(111),
                session_id: Some(" s-1 "),
                agent_name: Some("claude-code"),
                start_ns: 2_000_000_000,
                end_ns: Some(2_200_000_000),
                output_tokens: 10,
                ..Default::default()
            },
            Call {
                pid: Some(222),
                session_id: Some("   "),
                agent_name: Some("blank-session"),
                start_ns: 3_000_000_000,
                end_ns: Some(3_100_000_000),
                output_tokens: 5,
                ..Default::default()
            },
            Call {
                pid: Some(333),
                process_name: Some("mystery"),
                start_ns: 4_000_000_000,
                end_ns: Some(4_400_000_000),
                output_tokens: 5,
                ..Default::default()
            },
            Call {
                start_ns: 5_000_000_000,
                end_ns: Some(5_500_000_000),
                output_tokens: 5,
                ..Default::default()
            },
        ];
        let (store, path) = store_with("session", &calls);
        let rows = store
            .get_top_rows(0, 10_000_000_000, TopGrouping::Session, None, None)
            .unwrap();
        // Agent-fallback groups sort before session groups; the unnamed one first.
        assert_eq!(rows.len(), 4);
        assert_eq!(rows[0].label, "(unknown)");
        assert!(rows[0].pid.is_none() && rows[0].session_id.is_none());
        assert_eq!(rows[1].label, "blank-session");
        assert_eq!(rows[1].pid, Some(222));
        assert!(
            rows[1].session_id.is_none(),
            "a blank session is no session"
        );
        assert_eq!(rows[2].label, "mystery");

        let session = &rows[3];
        assert_eq!(session.label, "claude-code");
        assert_eq!(session.session_id.as_deref(), Some("s-1"));
        assert_eq!(session.pid, Some(111));
        assert_eq!(session.call_count, 2);

        let summary = store
            .get_top_window_summary(0, 10_000_000_000, None, None)
            .unwrap();
        assert_eq!(
            summary.process_count, 3,
            "the PID-less call counts no process"
        );
        assert_eq!(
            summary.session_count, 1,
            "only the trimmed s-1 is a session"
        );
        assert_eq!(summary.call_count, 5);
        finish(store, path);
    }

    #[test]
    fn filters_rows_by_pid_and_matches_agent_names_case_insensitively() {
        let (store, path) = store_with("filters", &two_process_fixture());
        let rows = store
            .get_top_rows(0, 10_000_000_000, TopGrouping::Process, None, Some(222))
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].pid, Some(222));

        let rows = store
            .get_top_rows(
                0,
                10_000_000_000,
                TopGrouping::Process,
                Some("CLAUDE-CODE"),
                None,
            )
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].call_count, 2);

        let summary = store
            .get_top_window_summary(0, 10_000_000_000, Some("claude-code"), None)
            .unwrap();
        assert_eq!(summary.call_count, 2);
        assert_eq!(summary.process_count, 1);
        assert_eq!(summary.session_count, 1);
        finish(store, path);
    }

    #[test]
    fn process_rows_agree_with_the_agent_latency_view() {
        let (store, path) = store_with("agreement", &two_process_fixture());
        let rows = store
            .get_top_rows(
                0,
                10_000_000_000,
                TopGrouping::Process,
                Some("claude-code"),
                None,
            )
            .unwrap();
        let agents = store
            .get_latency_metrics(0, 10_000_000_000, Some("claude-code"))
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(agents.len(), 1);
        let (row, agent) = (&rows[0], &agents[0]);
        assert_eq!(row.call_count, agent.call_count);
        assert_eq!(row.streaming_call_count, agent.streaming_call_count);
        for (top, latency) in [
            (row.ttft_ms.as_ref(), agent.ttft_ms.as_ref()),
            (
                row.tps_tokens_per_second.as_ref(),
                agent.tps_tokens_per_second.as_ref(),
            ),
            (
                row.tpot_ms_per_token.as_ref(),
                agent.tpot_ms_per_token.as_ref(),
            ),
            (row.e2e_latency_ms.as_ref(), agent.e2e_latency_ms.as_ref()),
        ] {
            assert_eq!(
                top.map(|p| (p.p50, p.p95, p.p99)),
                latency.map(|p| (p.p50, p.p95, p.p99))
            );
        }
        finish(store, path);
    }

    #[test]
    fn bills_input_tokens_with_the_cache_aware_rule() {
        let calls = vec![
            Call {
                pid: Some(1),
                agent_name: Some("a"),
                provider: Some("anthropic"),
                start_ns: 1_000_000_000,
                end_ns: Some(2_000_000_000),
                input_tokens: 100,
                cache_creation_tokens: 5,
                cache_read_tokens: 7,
                output_tokens: 1,
                ..Default::default()
            },
            Call {
                pid: Some(1),
                agent_name: Some("a"),
                provider: Some("openai"),
                start_ns: 2_000_000_000,
                end_ns: Some(3_000_000_000),
                input_tokens: 100,
                cache_read_tokens: 50,
                includes_cache: true,
                output_tokens: 1,
                ..Default::default()
            },
            Call {
                pid: Some(1),
                agent_name: Some("a"),
                start_ns: 3_000_000_000,
                end_ns: Some(4_000_000_000),
                input_tokens: 10,
                output_tokens: 1,
                ..Default::default()
            },
        ];
        let (store, path) = store_with("billed", &calls);
        let rows = store
            .get_top_rows(0, 10_000_000_000, TopGrouping::Process, None, None)
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].input_tokens, 222, "100+5+7, 100 already cached, 10");
        assert_eq!(rows[0].output_tokens, 3);
        assert_eq!(rows[0].total_tokens, 225);

        let summary = store
            .get_top_window_summary(0, 10_000_000_000, None, None)
            .unwrap();
        assert_eq!(summary.input_tokens, 222);
        assert_eq!(summary.output_tokens, 3);
        assert_eq!(summary.first_seen_ns, 1_000_000_000);
        assert_eq!(summary.last_seen_ns, 4_000_000_000);
        finish(store, path);
    }

    #[test]
    fn empty_window_reports_zero_summary_and_no_rows() {
        let (store, path) = store_with("empty", &two_process_fixture());
        let rows = store
            .get_top_rows(
                20_000_000_000,
                30_000_000_000,
                TopGrouping::Process,
                None,
                None,
            )
            .unwrap();
        assert!(rows.is_empty());
        let summary = store
            .get_top_window_summary(20_000_000_000, 30_000_000_000, None, None)
            .unwrap();
        assert_eq!(summary.call_count, 0);
        assert_eq!(summary.process_count, 0);
        assert_eq!(summary.session_count, 0);
        assert_eq!(summary.input_tokens, 0);
        assert_eq!(summary.output_tokens, 0);
        assert_eq!(summary.first_seen_ns, 0);
        assert_eq!(summary.last_seen_ns, 0);
        finish(store, path);
    }
}
