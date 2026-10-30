//! Conversion from Hermes session rows to ATIF v1.7.

use std::collections::{hash_map::DefaultHasher, HashMap};
use std::hash::{Hash, Hasher};

use agentsight_atif::{
    Agent, AtifTrajectory, FinalMetrics, Observation, ObservationResult, Step, StepSource,
    ToolCall, ATIF_SCHEMA_VERSION,
};
use chrono::{DateTime, SecondsFormat, Utc};
use serde_json::Value;
use sha2::{Digest, Sha256};

const CONTENT_JSON_PREFIX: &str = "\0json:";
const SOURCE: &str = "hermes";

#[derive(Debug)]
pub(crate) struct HermesSession {
    pub(crate) id: String,
    pub(crate) source: String,
    pub(crate) model: Option<String>,
    pub(crate) parent_session_id: Option<String>,
    pub(crate) cwd: Option<String>,
    pub(crate) started_at: f64,
    pub(crate) ended_at: Option<f64>,
    pub(crate) input_tokens: i64,
    pub(crate) output_tokens: i64,
    pub(crate) cache_read_tokens: i64,
    pub(crate) cache_write_tokens: i64,
    pub(crate) reasoning_tokens: i64,
    pub(crate) estimated_cost_usd: Option<f64>,
    pub(crate) actual_cost_usd: Option<f64>,
    pub(crate) api_call_count: i64,
}

#[derive(Debug)]
pub(crate) struct HermesMessage {
    pub(crate) id: i64,
    pub(crate) role: String,
    pub(crate) content: Option<String>,
    pub(crate) tool_call_id: Option<String>,
    pub(crate) tool_calls: Option<String>,
    pub(crate) tool_name: Option<String>,
    pub(crate) timestamp: f64,
    pub(crate) finish_reason: Option<String>,
    pub(crate) reasoning: Option<String>,
    pub(crate) reasoning_content: Option<String>,
}

/// 用域隔离和固定摘要算法生成采集标识，不暴露库的本地路径。
pub(crate) fn scoped_session_id(database_namespace: &str, original_id: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(b"agentsight-hermes-session-v1\0");
    digest.update(database_namespace.as_bytes());
    digest.update(b"\0");
    digest.update(original_id.as_bytes());
    format!("hermes:{:x}", digest.finalize())
}

pub(crate) fn convert_session(
    database_namespace: &str,
    schema_version: Option<i64>,
    session: &HermesSession,
    messages: &[HermesMessage],
) -> AtifTrajectory {
    let mut steps = Vec::new();
    for message in messages {
        match message.role.as_str() {
            "tool" => attach_tool_result(&mut steps, message),
            "user" | "system" | "assistant" => {
                steps.push(message_step(steps.len() + 1, message));
            }
            _ => {}
        }
    }
    for (index, step) in steps.iter_mut().enumerate() {
        step.step_id = index + 1;
    }
    let total_steps = steps.len();

    let mut agent_extra = HashMap::new();
    if let Some(version) = schema_version {
        agent_extra.insert("hermes_state_schema_version".into(), Value::from(version));
    }
    let mut trajectory_extra = HashMap::new();
    trajectory_extra.insert(
        "hermes_source".into(),
        Value::String(session.source.clone()),
    );
    if let Some(cwd) = &session.cwd {
        trajectory_extra.insert("cwd".into(), Value::String(cwd.clone()));
    }
    trajectory_extra.insert(
        "hermes_session_id".into(),
        Value::String(session.id.clone()),
    );
    if let Some(parent) = &session.parent_session_id {
        trajectory_extra.insert(
            "hermes_parent_session_id".into(),
            Value::String(parent.clone()),
        );
        trajectory_extra.insert(
            "parent_session_id".into(),
            Value::String(scoped_session_id(database_namespace, parent)),
        );
    }

    AtifTrajectory {
        schema_version: ATIF_SCHEMA_VERSION.into(),
        agent: Agent {
            name: SOURCE.into(),
            version: "unknown".into(),
            model_name: session.model.clone(),
            tool_definitions: None,
            extra: (!agent_extra.is_empty()).then_some(agent_extra),
        },
        steps,
        session_id: Some(scoped_session_id(database_namespace, &session.id)),
        trajectory_id: None,
        notes: None,
        final_metrics: Some(final_metrics(session, total_steps)),
        continued_trajectory_ref: None,
        subagent_trajectories: None,
        extra: Some(trajectory_extra),
    }
}

fn message_step(step_id: usize, message: &HermesMessage) -> Step {
    let source = match message.role.as_str() {
        "user" => StepSource::User,
        "system" => StepSource::System,
        _ => StepSource::Agent,
    };
    let mut extra = HashMap::new();
    extra.insert("hermes_message_id".into(), Value::from(message.id));
    if let Some(reason) = &message.finish_reason {
        extra.insert("finish_reason".into(), Value::String(reason.clone()));
    }
    Step {
        step_id,
        source,
        message: content_text(message.content.as_deref()),
        timestamp: format_timestamp(message.timestamp),
        model_name: None,
        reasoning_effort: None,
        reasoning_content: message
            .reasoning_content
            .clone()
            .or_else(|| message.reasoning.clone()),
        tool_calls: parse_tool_calls(message.tool_calls.as_deref(), message.id),
        observation: None,
        metrics: None,
        extra: Some(extra),
        llm_call_count: (message.role == "assistant").then_some(1),
        is_copied_context: None,
    }
}

fn parse_tool_calls(raw: Option<&str>, message_id: i64) -> Option<Vec<ToolCall>> {
    let value = raw.and_then(|raw| serde_json::from_str::<Value>(raw).ok())?;
    let values = value.as_array().cloned().unwrap_or_else(|| vec![value]);
    let calls = values
        .into_iter()
        .enumerate()
        .filter_map(|(index, call)| {
            let function = call.get("function").unwrap_or(&call);
            let name = function
                .get("name")
                .or_else(|| call.get("name"))
                .and_then(Value::as_str)?;
            let id = call
                .get("id")
                .or_else(|| call.get("tool_call_id"))
                .and_then(Value::as_str)
                .map(String::from)
                .unwrap_or_else(|| format!("hermes-{message_id}-{index}"));
            let arguments = function
                .get("arguments")
                .or_else(|| call.get("arguments"))
                .cloned()
                .map(parse_arguments)
                .unwrap_or(Value::Null);
            Some(ToolCall {
                tool_call_id: id,
                function_name: name.to_string(),
                arguments,
                extra: None,
            })
        })
        .collect::<Vec<_>>();
    (!calls.is_empty()).then_some(calls)
}

fn parse_arguments(value: Value) -> Value {
    match value {
        Value::String(text) => serde_json::from_str(&text).unwrap_or(Value::String(text)),
        other => other,
    }
}

fn attach_tool_result(steps: &mut Vec<Step>, message: &HermesMessage) {
    let result_id = message.tool_call_id.clone();
    // An older explicit call outranks a newer call to the same tool. Name
    // fallback remains necessary when Hermes omits the assistant's call ID.
    let exact = result_id.as_deref().and_then(|id| {
        steps.iter().rposition(|step| {
            step.source == StepSource::Agent
                && step
                    .tool_calls
                    .as_ref()
                    .is_some_and(|calls| calls.iter().any(|call| call.tool_call_id == id))
        })
    });
    let target = exact.or_else(|| {
        let name = message.tool_name.as_deref()?;
        steps.iter().rposition(|step| {
            step.source == StepSource::Agent
                && step
                    .tool_calls
                    .as_ref()
                    .is_some_and(|calls| calls.iter().any(|call| call.function_name == name))
        })
    });
    let result = ObservationResult {
        source_call_id: result_id.clone(),
        content: decode_content(message.content.as_deref()),
        subagent_trajectory_ref: None,
        extra: None,
    };
    if let Some(step) = target.map(|index| &mut steps[index]) {
        align_result_id(step, message, result_id.as_deref());
        step.observation
            .get_or_insert_with(|| Observation {
                results: Vec::new(),
            })
            .results
            .push(result);
        return;
    }

    steps.push(Step {
        step_id: steps.len() + 1,
        source: StepSource::Agent,
        message: String::new(),
        timestamp: format_timestamp(message.timestamp),
        model_name: None,
        reasoning_effort: None,
        reasoning_content: None,
        tool_calls: None,
        observation: Some(Observation {
            results: vec![result],
        }),
        metrics: None,
        extra: None,
        llm_call_count: None,
        is_copied_context: None,
    });
}

fn align_result_id(step: &mut Step, message: &HermesMessage, result_id: Option<&str>) {
    let (Some(id), Some(calls)) = (result_id, step.tool_calls.as_mut()) else {
        return;
    };
    let position = calls
        .iter()
        .position(|call| call.tool_call_id == id)
        .or_else(|| {
            calls.iter().position(|call| {
                message
                    .tool_name
                    .as_deref()
                    .is_some_and(|name| call.function_name == name)
            })
        });
    let Some(call) = position.map(|index| &mut calls[index]) else {
        return;
    };
    call.tool_call_id = id.to_string();
    let mut extra = call.extra.take().unwrap_or_default();
    if let Some(timestamp) = format_timestamp(message.timestamp) {
        extra.insert("result_timestamp".into(), Value::String(timestamp));
    }
    call.extra = Some(extra);
}

fn decode_content(raw: Option<&str>) -> Option<Value> {
    let raw = raw?;
    if let Some(json) = raw.strip_prefix(CONTENT_JSON_PREFIX) {
        serde_json::from_str(json).ok()
    } else {
        Some(Value::String(raw.to_string()))
    }
}

fn content_text(raw: Option<&str>) -> String {
    match decode_content(raw) {
        Some(Value::String(text)) => text,
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|part| {
                part.get("text")
                    .or_else(|| part.get("content"))
                    .and_then(Value::as_str)
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Some(value) => value.to_string(),
        None => String::new(),
    }
}

fn final_metrics(session: &HermesSession, total_steps: usize) -> FinalMetrics {
    let mut extra = HashMap::new();
    extra.insert("api_call_count".into(), Value::from(session.api_call_count));
    extra.insert(
        "cache_write_tokens".into(),
        Value::from(session.cache_write_tokens),
    );
    extra.insert(
        "reasoning_tokens".into(),
        Value::from(session.reasoning_tokens),
    );
    FinalMetrics {
        total_prompt_tokens: u64::try_from(session.input_tokens).ok(),
        total_completion_tokens: u64::try_from(session.output_tokens).ok(),
        total_cached_tokens: u64::try_from(session.cache_read_tokens).ok(),
        total_cost_usd: session.actual_cost_usd.or(session.estimated_cost_usd),
        total_steps: Some(total_steps),
        extra: Some(extra),
    }
}

fn format_timestamp(timestamp: f64) -> Option<String> {
    if !timestamp.is_finite() || timestamp < 0.0 {
        return None;
    }
    let seconds = timestamp.trunc() as i64;
    let nanos = ((timestamp.fract() * 1_000_000_000.0).round() as u32).min(999_999_999);
    DateTime::<Utc>::from_timestamp(seconds, nanos)
        .map(|value| value.to_rfc3339_opts(SecondsFormat::Millis, true))
}

pub(crate) fn timestamp_ns(timestamp: f64) -> i64 {
    if !timestamp.is_finite() || timestamp <= 0.0 {
        return 0;
    }
    let value = timestamp * 1_000_000_000.0;
    if value >= i64::MAX as f64 {
        i64::MAX
    } else {
        value.round() as i64
    }
}

pub(crate) fn source_fingerprint(
    schema_version: Option<i64>,
    session: &HermesSession,
    messages: &[HermesMessage],
) -> i64 {
    let mut hasher = DefaultHasher::new();
    schema_version.hash(&mut hasher);
    session.id.hash(&mut hasher);
    session.source.hash(&mut hasher);
    session.model.hash(&mut hasher);
    session.parent_session_id.hash(&mut hasher);
    session.cwd.hash(&mut hasher);
    session.started_at.to_bits().hash(&mut hasher);
    session.input_tokens.hash(&mut hasher);
    session.output_tokens.hash(&mut hasher);
    session.cache_read_tokens.hash(&mut hasher);
    session.cache_write_tokens.hash(&mut hasher);
    session.reasoning_tokens.hash(&mut hasher);
    session.api_call_count.hash(&mut hasher);
    session
        .estimated_cost_usd
        .map(f64::to_bits)
        .hash(&mut hasher);
    session.actual_cost_usd.map(f64::to_bits).hash(&mut hasher);
    session.ended_at.map(f64::to_bits).hash(&mut hasher);
    for message in messages {
        message.id.hash(&mut hasher);
        message.role.hash(&mut hasher);
        message.content.hash(&mut hasher);
        message.tool_call_id.hash(&mut hasher);
        message.tool_calls.hash(&mut hasher);
        message.tool_name.hash(&mut hasher);
        message.timestamp.to_bits().hash(&mut hasher);
        message.finish_reason.hash(&mut hasher);
        message.reasoning.hash(&mut hasher);
        message.reasoning_content.hash(&mut hasher);
    }
    (hasher.finish() & i64::MAX as u64) as i64
}
