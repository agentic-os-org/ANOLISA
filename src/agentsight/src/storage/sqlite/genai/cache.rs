//! Per-call prompt-cache rows read by the cache hit-rate diagnosis.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use rusqlite::params;

use super::stats::call_latency;
use super::{GenAISqliteStore, billed_input_col};

/// Upper bound on calls one diagnosis window reads; the newest calls are kept.
pub const CACHE_DIAGNOSIS_MAX_ROWS: usize = 20_000;

/// One completed LLM call with the counters and request fingerprints that
/// cache diagnosis compares between adjacent calls.
///
/// Fingerprints are process-local hashes: they are only meaningful when
/// compared with other rows read by the same binary.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CacheCallRow {
    pub id: i64,
    pub call_id: Option<String>,
    pub trace_id: Option<String>,
    pub session_id: Option<String>,
    pub conversation_id: Option<String>,
    pub agent_name: Option<String>,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub start_ns: i64,
    pub end_ns: Option<i64>,
    /// Input including cache reads and writes; `None` when that total is zero,
    /// meaning the call reported no usage. Legacy Anthropic rows can store a
    /// zero raw `input_tokens` for a fully cached call, so the test applies to
    /// the normalized total, not the raw column.
    pub input_tokens: Option<i64>,
    /// `None` when the response had no cache-read counter, which is not a confirmed zero.
    pub cache_read_tokens: Option<i64>,
    /// `None` when the response had no cache-write counter.
    pub cache_creation_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub ttft_ms: Option<f64>,
    pub e2e_latency_ms: Option<f64>,
    /// HTTP error status or a recorded interruption.
    pub failed: bool,
    pub system_fingerprint: Option<u64>,
    pub tools_fingerprint: Option<u64>,
    /// Number of request messages, including a synthetic system message.
    pub message_count: Option<i64>,
}

/// Cache rows of one window, oldest first.
#[derive(Debug, Default)]
pub struct CacheCallWindow {
    pub rows: Vec<CacheCallRow>,
    /// The window held more than [`CACHE_DIAGNOSIS_MAX_ROWS`] calls and the oldest were skipped.
    pub truncated: bool,
}

fn fingerprint(text: Option<String>) -> Option<u64> {
    let text = text.filter(|text| !text.is_empty())?;
    let mut hasher = DefaultHasher::new();
    text.hash(&mut hasher);
    Some(hasher.finish())
}

impl GenAISqliteStore {
    /// Reads completed LLM calls in `[start_ns, end_ns]` for cache diagnosis.
    ///
    /// Request tools and the message count come from the stored event JSON;
    /// rows whose JSON is invalid keep their counters and lose only those
    /// fingerprints.
    ///
    /// # Errors
    ///
    /// Returns an error when the connection mutex is poisoned or the query fails.
    pub fn get_cache_call_window(
        &self,
        start_ns: i64,
        end_ns: i64,
        agent_name: Option<&str>,
        session_id: Option<&str>,
    ) -> Result<CacheCallWindow, Box<dyn std::error::Error>> {
        let conn = self
            .conn
            .lock()
            .map_err(|error| format!("GenAI cache diagnosis mutex poisoned: {error}"))?;
        let mut stmt = conn.prepare(concat!(
            "SELECT id, call_id, trace_id, session_id, conversation_id,
                    COALESCE(NULLIF(TRIM(agent_name), ''), NULLIF(TRIM(process_name), '')),
                    provider, model, start_timestamp_ns, end_timestamp_ns,
                    first_output_timestamp_ns,
                    CASE WHEN (",
            billed_input_col!(),
            ") > 0 THEN (",
            billed_input_col!(),
            ") END,
                    cache_read_tokens, cache_creation_tokens, output_tokens,
                    COALESCE(status_code, 0) >= 400 OR interruption_type IS NOT NULL,
                    system_instructions,
                    CASE WHEN json_valid(event_json)
                         THEN json_extract(event_json, '$.LLMCall.request.tools') END,
                    CASE WHEN json_valid(event_json)
                         THEN json_array_length(event_json, '$.LLMCall.request.messages') END
             FROM genai_events
             WHERE event_type = 'llm_call' AND status = 'complete'
               AND start_timestamp_ns BETWEEN ?1 AND ?2
               AND (?3 IS NULL
                    OR COALESCE(NULLIF(TRIM(agent_name), ''), NULLIF(TRIM(process_name), '')) COLLATE NOCASE = ?3 COLLATE NOCASE)
               AND (?4 IS NULL OR session_id = ?4)
             ORDER BY start_timestamp_ns DESC, id DESC
             LIMIT ?5"
        ))?;
        let limit = i64::try_from(CACHE_DIAGNOSIS_MAX_ROWS)?.saturating_add(1);
        let rows = stmt.query_map(
            params![start_ns, end_ns, agent_name, session_id, limit],
            |row| {
                let start_ns: i64 = row.get(8)?;
                let end_ns: Option<i64> = row.get(9)?;
                let output_tokens: Option<i64> = row.get(14)?;
                let latency = call_latency(start_ns, end_ns, row.get(10)?, output_tokens);
                // json_extract returns arrays as JSON text but scalars as
                // SQL values; a non-array `tools` is treated as absent.
                let tools = row.get::<_, Option<String>>(17).unwrap_or(None);
                Ok(CacheCallRow {
                    id: row.get(0)?,
                    call_id: row.get(1)?,
                    trace_id: row.get(2)?,
                    session_id: row.get(3)?,
                    conversation_id: row.get(4)?,
                    agent_name: row.get(5)?,
                    provider: row.get(6)?,
                    model: row.get(7)?,
                    start_ns,
                    end_ns,
                    input_tokens: row.get(11)?,
                    cache_read_tokens: row.get(12)?,
                    cache_creation_tokens: row.get(13)?,
                    output_tokens,
                    ttft_ms: latency.ttft_ms,
                    e2e_latency_ms: latency.e2e_latency_ms,
                    failed: row.get(15)?,
                    system_fingerprint: fingerprint(row.get(16)?),
                    tools_fingerprint: fingerprint(tools),
                    message_count: row.get(18)?,
                })
            },
        )?;
        let mut rows = rows.collect::<Result<Vec<_>, _>>()?;
        let truncated = rows.len() > CACHE_DIAGNOSIS_MAX_ROWS;
        rows.truncate(CACHE_DIAGNOSIS_MAX_ROWS);
        rows.reverse();
        Ok(CacheCallWindow { rows, truncated })
    }
}
