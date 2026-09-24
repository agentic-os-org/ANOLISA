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
    ttft_ms: Option<f64>,
    tps_tokens_per_second: Option<f64>,
    tpot_ms_per_token: Option<f64>,
    e2e_latency_ms: Option<f64>,
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
            let e2e_ns = end.filter(|end| *end > start).map(|end| end - start);
            let is_sse: Option<i64> = row.get(5)?;
            let stream_ns = match (first, end) {
                (Some(first), Some(end)) if first >= start && end > first => Some(end - first),
                _ => None,
            };
            let tokens = output_tokens.filter(|tokens| *tokens > 0);
            Ok(CallMetrics {
                agent_name: row.get(0)?,
                ttft_ms: first
                    .filter(|first| {
                        *first >= start && (end.is_none() || end.is_some_and(|end| *first <= end))
                    })
                    .map(|first| (first - start) as f64 / 1_000_000.0),
                is_sse: is_sse == Some(1),
                // By #2339 convention, TPS/TPOT use total output_tokens N; do not use N - 1.
                tps_tokens_per_second: stream_ns
                    .zip(tokens)
                    .map(|(duration, tokens)| tokens as f64 * 1_000_000_000.0 / duration as f64),
                tpot_ms_per_token: stream_ns
                    .zip(tokens)
                    .map(|(duration, tokens)| duration as f64 / 1_000_000.0 / tokens as f64),
                e2e_latency_ms: e2e_ns.map(|duration| duration as f64 / 1_000_000.0),
            })
        };
        let calls = if let Some(name) = agent_name {
            stmt.query_map(params![start_ns, end_ns, name], map_row)?
                .collect::<Result<Vec<_>, _>>()?
        } else {
            stmt.query_map(params![start_ns, end_ns], map_row)?
                .collect::<Result<Vec<_>, _>>()?
        };

        let mut grouped = std::collections::BTreeMap::<Option<String>, Vec<CallMetrics>>::new();
        for call in calls {
            grouped
                .entry(call.agent_name.clone())
                .or_default()
                .push(call);
        }
        Ok(grouped
            .into_iter()
            .map(|(agent_name, calls)| LatencyMetricsSummary {
                agent_name,
                call_count: calls.len(),
                streaming_call_count: calls.iter().filter(|call| call.is_sse).count(),
                ttft_ms: percentiles(calls.iter().filter_map(|call| call.ttft_ms).collect()),
                tps_tokens_per_second: percentiles(
                    calls
                        .iter()
                        .filter_map(|call| call.tps_tokens_per_second)
                        .collect(),
                ),
                tpot_ms_per_token: percentiles(
                    calls
                        .iter()
                        .filter_map(|call| call.tpot_ms_per_token)
                        .collect(),
                ),
                e2e_latency_ms: percentiles(
                    calls
                        .iter()
                        .filter_map(|call| call.e2e_latency_ms)
                        .collect(),
                ),
            })
            .collect())
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
        let range_ns = (end_ns - start_ns).max(1);
        let bucket_ns = range_ns / bucket_count as i64;

        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());

        // Build query with optional agent_name filter
        let sql: &str = if agent_name.is_some() {
            concat!(
                "SELECT
                (start_timestamp_ns - ?1) / ?3            AS bucket_idx,
                ?1 + ((start_timestamp_ns - ?1) / ?3) * ?3 AS bucket_start_ns,
                COALESCE(SUM(",
                billed_input_col!(),
                "), 0)                AS input_tokens,
                COALESCE(SUM(output_tokens), 0)           AS output_tokens,
                COALESCE(SUM((",
                billed_input_col!(),
                ") + output_tokens), 0) AS total_tokens
             FROM genai_events
             WHERE event_type = 'llm_call'
               AND start_timestamp_ns BETWEEN ?1 AND ?2
               AND agent_name = ?4
             GROUP BY bucket_idx
             ORDER BY bucket_idx ASC"
            )
        } else {
            concat!(
                "SELECT
                (start_timestamp_ns - ?1) / ?3            AS bucket_idx,
                ?1 + ((start_timestamp_ns - ?1) / ?3) * ?3 AS bucket_start_ns,
                COALESCE(SUM(",
                billed_input_col!(),
                "), 0)                AS input_tokens,
                COALESCE(SUM(output_tokens), 0)           AS output_tokens,
                COALESCE(SUM((",
                billed_input_col!(),
                ") + output_tokens), 0) AS total_tokens
             FROM genai_events
             WHERE event_type = 'llm_call'
               AND start_timestamp_ns BETWEEN ?1 AND ?2
             GROUP BY bucket_idx
             ORDER BY bucket_idx ASC"
            )
        };

        let rows: Vec<TimeseriesBucket> = if let Some(name) = agent_name {
            let mut stmt = conn.prepare(sql)?;
            stmt.query_map(params![start_ns, end_ns, bucket_ns, name], |row| {
                Ok(TimeseriesBucket {
                    bucket_start_ns: row.get(1)?,
                    input_tokens: row.get(2)?,
                    output_tokens: row.get(3)?,
                    total_tokens: row.get(4)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?
        } else {
            let mut stmt = conn.prepare(sql)?;
            stmt.query_map(params![start_ns, end_ns, bucket_ns], |row| {
                Ok(TimeseriesBucket {
                    bucket_start_ns: row.get(1)?,
                    input_tokens: row.get(2)?,
                    output_tokens: row.get(3)?,
                    total_tokens: row.get(4)?,
                })
            })?
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
        let range_ns = (end_ns - start_ns).max(1);
        let bucket_ns = range_ns / bucket_count as i64;

        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());

        let sql: &str = if agent_name.is_some() {
            concat!(
                "SELECT
                (start_timestamp_ns - ?1) / ?3            AS bucket_idx,
                ?1 + ((start_timestamp_ns - ?1) / ?3) * ?3 AS bucket_start_ns,
                COALESCE(model, 'unknown')                 AS model,
                COALESCE(SUM((",
                billed_input_col!(),
                ") + output_tokens), 0) AS total_tokens
             FROM genai_events
             WHERE event_type = 'llm_call'
               AND start_timestamp_ns BETWEEN ?1 AND ?2
               AND agent_name = ?4
             GROUP BY bucket_idx, model
             ORDER BY bucket_idx ASC"
            )
        } else {
            concat!(
                "SELECT
                (start_timestamp_ns - ?1) / ?3            AS bucket_idx,
                ?1 + ((start_timestamp_ns - ?1) / ?3) * ?3 AS bucket_start_ns,
                COALESCE(model, 'unknown')                 AS model,
                COALESCE(SUM((",
                billed_input_col!(),
                ") + output_tokens), 0) AS total_tokens
             FROM genai_events
             WHERE event_type = 'llm_call'
               AND start_timestamp_ns BETWEEN ?1 AND ?2
             GROUP BY bucket_idx, model
             ORDER BY bucket_idx ASC"
            )
        };

        let rows: Vec<ModelTimeseriesBucket> = if let Some(name) = agent_name {
            let mut stmt = conn.prepare(sql)?;
            stmt.query_map(params![start_ns, end_ns, bucket_ns, name], |row| {
                Ok(ModelTimeseriesBucket {
                    bucket_start_ns: row.get(1)?,
                    model: row.get(2)?,
                    total_tokens: row.get(3)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?
        } else {
            let mut stmt = conn.prepare(sql)?;
            stmt.query_map(params![start_ns, end_ns, bucket_ns], |row| {
                Ok(ModelTimeseriesBucket {
                    bucket_start_ns: row.get(1)?,
                    model: row.get(2)?,
                    total_tokens: row.get(3)?,
                })
            })?
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
    /// Groups by `COALESCE(agent_name, process_name, 'unknown')` so that every
    /// LLM call is attributed to some label even when agent_name is NULL.
    pub fn get_agent_token_summary(
        &self,
    ) -> Result<Vec<AgentTokenSummary>, Box<dyn std::error::Error>> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let mut stmt = conn.prepare(concat!(
            "SELECT COALESCE(agent_name, process_name, 'unknown') AS agent,
                    COALESCE(SUM(",
            billed_input_col!(),
            "), 0)      AS input_tokens,
                    COALESCE(SUM(output_tokens), 0) AS output_tokens,
                    COALESCE(SUM((",
            billed_input_col!(),
            ") + output_tokens), 0) AS total_tokens,
                    COUNT(*)                        AS request_count
             FROM genai_events
             WHERE event_type = 'llm_call'
             GROUP BY agent
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
