//! Session resource timeline queries and activity phase derivation.

use std::collections::HashSet;

use rusqlite::params;
use serde::Serialize;

use super::GenAISqliteStore;
use super::resource::ResourceSample;

/// One classified interval on a Session resource timeline.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SessionPhase {
    /// `llm`, `tool_call`, or `idle`.
    pub kind: String,
    /// Inclusive interval start in Unix epoch nanoseconds.
    pub start_timestamp_ns: i64,
    /// Inclusive interval end in Unix epoch nanoseconds.
    pub end_timestamp_ns: i64,
    /// Provider tool-call identifier for inferred Tool Call intervals.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

/// Raw process samples and derived LLM/Tool Call intervals for one Session.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SessionResourceTimeline {
    /// Session whose calls determine the time range and associated PIDs.
    pub session_id: String,
    /// Process-level observations in ascending timestamp order.
    pub samples: Vec<ResourceSample>,
    /// LLM and inferred Tool Call intervals clipped to the requested range.
    pub phases: Vec<SessionPhase>,
}

#[derive(Debug)]
struct SessionCall {
    start_ns: i64,
    end_ns: i64,
    pid: i32,
    tool_call_ids: Vec<String>,
    tool_response_ids: HashSet<String>,
}

impl GenAISqliteStore {
    /// Returns process samples and derived activity intervals for a Session.
    pub fn get_session_resource_timeline(
        &self,
        session_id: &str,
        start_ns: Option<i64>,
        end_ns: Option<i64>,
        max_points: usize,
    ) -> Result<Option<SessionResourceTimeline>, Box<dyn std::error::Error>> {
        let conn = self
            .conn
            .lock()
            .map_err(|error| format!("GenAI SQLite connection mutex poisoned: {error}"))?;
        let mut statement = conn.prepare(
            "SELECT start_timestamp_ns,
                    COALESCE(end_timestamp_ns, start_timestamp_ns),
                    pid, tool_call_ids, input_messages
             FROM genai_events
             WHERE event_type = 'llm_call' AND session_id = ?1
             ORDER BY start_timestamp_ns ASC",
        )?;
        let rows = statement.query_map(params![session_id], |row| {
            let tool_ids_json: Option<String> = row.get(3)?;
            let input_messages_json: Option<String> = row.get(4)?;
            Ok(SessionCall {
                start_ns: row.get(0)?,
                end_ns: row.get(1)?,
                pid: row.get(2)?,
                tool_call_ids: parse_string_array(tool_ids_json.as_deref()),
                tool_response_ids: parse_tool_response_ids(input_messages_json.as_deref()),
            })
        })?;
        let calls: Vec<SessionCall> = rows.collect::<Result<_, _>>()?;
        if calls.is_empty() {
            return Ok(None);
        }

        let default_start = calls.first().map(|call| call.start_ns).unwrap_or(0);
        let default_end = calls
            .iter()
            .map(|call| call.end_ns.max(call.start_ns))
            .max()
            .unwrap_or(default_start);
        let range_start = start_ns.unwrap_or(default_start).max(0);
        let range_end = end_ns.unwrap_or(default_end).max(range_start);
        let pids: Vec<i32> = calls
            .iter()
            .map(|call| call.pid)
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();

        let samples = query_samples(&conn, &pids, range_start, range_end, max_points.max(1))?;
        let phases = derive_phases(&calls, range_start, range_end);
        Ok(Some(SessionResourceTimeline {
            session_id: session_id.to_string(),
            samples,
            phases,
        }))
    }
}

fn query_samples(
    conn: &rusqlite::Connection,
    pids: &[i32],
    start_ns: i64,
    end_ns: i64,
    max_points: usize,
) -> Result<Vec<ResourceSample>, rusqlite::Error> {
    if pids.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders = (1..=pids.len())
        .map(|index| format!("?{index}"))
        .collect::<Vec<_>>()
        .join(",");
    let start_index = pids.len() + 1;
    let end_index = pids.len() + 2;
    let stride_index = pids.len() + 3;
    let limit_index = pids.len() + 4;
    let filter =
        format!("pid IN ({placeholders}) AND timestamp_ns BETWEEN ?{start_index} AND ?{end_index}");
    let values: Vec<rusqlite::types::Value> = pids
        .iter()
        .map(|pid| rusqlite::types::Value::Integer(*pid as i64))
        .collect();
    let mut count_values = values.clone();
    count_values.push(rusqlite::types::Value::Integer(start_ns));
    count_values.push(rusqlite::types::Value::Integer(end_ns));
    let total: usize = conn.query_row(
        &format!("SELECT COUNT(*) FROM agent_resource_samples WHERE {filter}"),
        rusqlite::params_from_iter(count_values),
        |row| row.get::<_, i64>(0).map(|count| count.max(0) as usize),
    )?;
    let stride = total.max(1).div_ceil(max_points.max(1));
    let sql = format!(
        "SELECT timestamp_ns, pid, agent_name, cpu_percent, memory_bytes
         FROM (
             SELECT timestamp_ns, pid, agent_name, cpu_percent, memory_bytes,
                    ROW_NUMBER() OVER (ORDER BY timestamp_ns ASC, pid ASC) AS row_num
             FROM agent_resource_samples
             WHERE {filter}
         )
         WHERE (row_num - 1) % ?{stride_index} = 0
         ORDER BY timestamp_ns ASC, pid ASC
         LIMIT ?{limit_index}"
    );
    let mut query_values = values;
    query_values.push(rusqlite::types::Value::Integer(start_ns));
    query_values.push(rusqlite::types::Value::Integer(end_ns));
    query_values.push(rusqlite::types::Value::Integer(stride as i64));
    query_values.push(rusqlite::types::Value::Integer(max_points.max(1) as i64));
    let mut statement = conn.prepare(&sql)?;
    let rows = statement.query_map(rusqlite::params_from_iter(query_values), |row| {
        Ok(ResourceSample {
            timestamp_ns: row.get(0)?,
            pid: row.get(1)?,
            agent_name: row.get(2)?,
            cpu_percent: row.get(3)?,
            memory_bytes: row.get(4)?,
        })
    })?;
    rows.collect::<Result<_, _>>()
}

fn derive_phases(calls: &[SessionCall], range_start: i64, range_end: i64) -> Vec<SessionPhase> {
    let mut phases = Vec::new();
    for (index, call) in calls.iter().enumerate() {
        push_clipped_phase(
            &mut phases,
            "llm",
            call.start_ns,
            call.end_ns,
            None,
            range_start,
            range_end,
        );
        for tool_call_id in &call.tool_call_ids {
            let Some(tool_end) = calls[index + 1..]
                .iter()
                .find(|next| next.tool_response_ids.contains(tool_call_id))
                .map(|next| next.start_ns)
            else {
                continue;
            };
            push_clipped_phase(
                &mut phases,
                "tool_call",
                call.end_ns,
                tool_end,
                Some(tool_call_id.clone()),
                range_start,
                range_end,
            );
        }
    }
    phases.sort_by_key(|phase| phase.start_timestamp_ns);
    let mut idle_phases = Vec::new();
    let mut occupied_until = range_start;
    for phase in &phases {
        if phase.start_timestamp_ns > occupied_until {
            push_clipped_phase(
                &mut idle_phases,
                "idle",
                occupied_until,
                phase.start_timestamp_ns,
                None,
                range_start,
                range_end,
            );
        }
        occupied_until = occupied_until.max(phase.end_timestamp_ns);
    }
    if occupied_until < range_end {
        push_clipped_phase(
            &mut idle_phases,
            "idle",
            occupied_until,
            range_end,
            None,
            range_start,
            range_end,
        );
    }
    phases.extend(idle_phases);
    phases.sort_by_key(|phase| phase.start_timestamp_ns);
    phases
}

fn push_clipped_phase(
    phases: &mut Vec<SessionPhase>,
    kind: &str,
    start_ns: i64,
    end_ns: i64,
    tool_call_id: Option<String>,
    range_start: i64,
    range_end: i64,
) {
    let start = start_ns.max(range_start);
    let end = end_ns.min(range_end).max(start);
    if end > start {
        phases.push(SessionPhase {
            kind: kind.to_string(),
            start_timestamp_ns: start,
            end_timestamp_ns: end,
            tool_call_id,
        });
    }
}

fn parse_string_array(value: Option<&str>) -> Vec<String> {
    value
        .and_then(|json| serde_json::from_str::<Vec<String>>(json).ok())
        .unwrap_or_default()
}

fn parse_tool_response_ids(value: Option<&str>) -> HashSet<String> {
    let Some(json) = value else {
        return HashSet::new();
    };
    let Ok(messages) = serde_json::from_str::<serde_json::Value>(json) else {
        return HashSet::new();
    };
    let mut ids = HashSet::new();
    collect_tool_response_ids(&messages, &mut ids);
    ids
}

fn collect_tool_response_ids(value: &serde_json::Value, ids: &mut HashSet<String>) {
    match value {
        serde_json::Value::Array(values) => {
            for value in values {
                collect_tool_response_ids(value, ids);
            }
        }
        serde_json::Value::Object(object) => {
            let is_response = object
                .get("type")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|kind| matches!(kind, "tool_call_response" | "tool_result"));
            if is_response {
                for key in ["id", "tool_call_id", "tool_use_id"] {
                    if let Some(id) = object.get(key).and_then(serde_json::Value::as_str) {
                        ids.insert(id.to_string());
                    }
                }
            }
            for child in object.values() {
                collect_tool_response_ids(child, ids);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
#[path = "resource_timeline_tests.rs"]
mod tests;
