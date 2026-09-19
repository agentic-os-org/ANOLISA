use opentelemetry::{
    Context, KeyValue,
    baggage::BaggageExt as _,
    trace::{Span as _, TraceContextExt as _},
};
use opentelemetry_sdk::{
    error::OTelSdkResult,
    trace::{Span, SpanData, SpanProcessor},
};
use serde::Serialize;
use serde_json::Value;
use std::time::Duration;

pub(crate) const FIELDS: [(&str, &str, &str); 5] = [
    ("agentsec.agent.name", "agent_name", "agentName"),
    ("agentsec.session.id", "session_id", "sessionId"),
    ("agentsec.run.id", "run_id", "runId"),
    ("agentsec.call.id", "call_id", "callId"),
    ("agentsec.tool_call.id", "tool_call_id", "toolCallId"),
];

/// Opaque compatibility labels stored as one typed extension in `OTel` Context.
#[derive(Debug, Default, Clone, Serialize, PartialEq, Eq)]
pub struct CompatibilityCorrelation {
    /// Caller-provided old trace ID, not an `OTel` `TraceId`.
    pub trace_id: Option<String>,
    /// Explicit invocation label; no automatic invocation UUID.
    pub invocation_label: Option<String>,
}

/// Bounds a string using the frozen V1 256 Unicode-code-point convention.
pub fn bounded(value: &str) -> String {
    const SUFFIX: &str = "...[truncated]";
    if value.chars().count() <= 256 {
        value.to_owned()
    } else {
        value.chars().take(256 - SUFFIX.len()).collect::<String>() + SUFFIX
    }
}

/// V1 Python strip semantics, including the four Unicode information separators.
pub fn normalize(value: &str) -> Option<String> {
    let value =
        value.trim_matches(|ch: char| ch.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&ch));
    (!value.is_empty()).then(|| bounded(value))
}
fn trace_context_field(value: &Value, snake: &str, camel: &str) -> Option<String> {
    value
        .get(snake)
        .and_then(Value::as_str)
        .and_then(normalize)
        .or_else(|| value.get(camel).and_then(Value::as_str).and_then(normalize))
}

/// Maps the existing flat trace-context JSON input at ingress. Valid fields override inherited
/// attribution; absent/invalid fields do not erase an existing native carrier.
///
/// # Errors
/// Returns a fixed error when input is not a JSON object.
pub fn bind_trace_context_input(parent: &Context, value: &Value) -> Result<Context, &'static str> {
    if !value.is_object() {
        return Err("trace context must be a JSON object");
    }
    let mut values = agent_values(parent);
    for (key, snake, camel) in FIELDS {
        if let Some(value) = trace_context_field(value, snake, camel) {
            values.retain(|item| item.key.as_str() != key);
            values.push(KeyValue::new(key, value));
        }
    }
    let mut labels = parent
        .get::<CompatibilityCorrelation>()
        .cloned()
        .unwrap_or_default();
    if let Some(trace_id) = trace_context_field(value, "trace_id", "traceId") {
        labels.trace_id = Some(trace_id);
    }
    Ok(parent.with_baggage(values).with_value(labels))
}

/// Original observability metadata ingress: strings are truncated, not trimmed.
/// Validates this record before binding; record-owned fields replace inherited
/// session/run/call/tool attribution, including clearing absent optional fields.
/// Technical parentage, typed extensions and independent agent attribution remain.
/// This is an adapter only; it does not store events or reconstruct trajectories.
///
/// # Errors
/// Returns a fixed schema error or the invalid/missing original field name.
pub fn bind_metadata(
    parent: &Context,
    value: &Value,
    kind: MetadataKind,
) -> Result<Context, &'static str> {
    if !value.is_object() {
        return Err("metadata must be a JSON object");
    }
    let mut values = agent_values(parent);
    values.retain(|item| item.key.as_str() == "agentsec.agent.name");
    for (key, snake, camel) in FIELDS.into_iter().skip(1) {
        let required = matches!(snake, "session_id" | "run_id")
            || snake == "tool_call_id" && matches!(kind, MetadataKind::ToolCall);
        let optional = snake == "call_id" && !matches!(kind, MetadataKind::AgentRun);
        if !required && !optional {
            continue; // V1 schema ignores fields belonging to other hook kinds.
        }
        let value = value.get(camel).or_else(|| value.get(snake));
        if optional && value.is_none_or(Value::is_null) {
            continue;
        }
        let value = value.and_then(Value::as_str).ok_or(snake)?;
        values.push(KeyValue::new(key, bounded(value)));
    }
    // with_baggage replaces the entry; independent agent.name is re-added above.
    Ok(parent.with_baggage(values))
}

pub(crate) fn agent_values(context: &Context) -> Vec<KeyValue> {
    FIELDS
        .into_iter()
        .filter_map(|(key, _, _)| {
            context
                .baggage()
                .get(key)
                .map(|value| KeyValue::new(key, bounded(value.as_ref())))
        })
        .collect()
}

/// Independent, read-only snapshot for logs and security-event attribution.
/// Reading it never depends on recording, a collector, or an exporter.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct CorrelationSnapshot {
    /// SDK technical trace identity; absent without an initialized active span.
    pub trace_id: Option<String>,
    /// SDK current operation identity.
    pub span_id: Option<String>,
    /// Entry SDK span shared by logs from all children of this request.
    pub request_span_id: Option<String>,
    /// Only the five allowlisted Agent attribution fields, in original snake case.
    pub agent: std::collections::BTreeMap<String, String>,
    /// Boundary compatibility labels, distinct from technical IDs.
    pub compatibility: CompatibilityCorrelation,
}

/// Reads all correlation information from the active `OTel` Context.
pub fn snapshot() -> CorrelationSnapshot {
    let context = Context::current();
    let span = context.span();
    let sc = span.span_context();
    CorrelationSnapshot {
        trace_id: sc.is_valid().then(|| sc.trace_id().to_string()),
        span_id: sc.is_valid().then(|| sc.span_id().to_string()),
        request_span_id: context.get::<crate::RequestSpanId>().map(|id| id.0.clone()),
        agent: FIELDS
            .into_iter()
            .filter_map(|(key, snake, _)| {
                context
                    .baggage()
                    .get(key)
                    .map(|value| (snake.to_owned(), bounded(value.as_ref())))
            })
            .collect(),
        compatibility: context
            .get::<CompatibilityCorrelation>()
            .cloned()
            .unwrap_or_default(),
    }
}

/// Required metadata differs by hook; this is a consumer contract, not a span name.
#[derive(Debug, Clone, Copy)]
pub enum MetadataKind {
    /// Agent run hooks require only session/run; other metadata keys are extras.
    AgentRun,
    /// Session/run attribution is required.
    ModelCall,
    /// Tool hooks also require a tool call identity.
    ToolCall,
}

/// Validates the current attribution for a future observability consumer.
/// Ordinary daemon calls do not invoke this validator.
///
/// # Errors
/// Returns the missing original field name, without including caller values.
pub fn validate_metadata(kind: MetadataKind) -> Result<CorrelationSnapshot, &'static str> {
    let snapshot = snapshot();
    for field in ["session_id", "run_id"] {
        if !snapshot.agent.contains_key(field) {
            return Err(field);
        }
    }
    if matches!(kind, MetadataKind::ToolCall) && !snapshot.agent.contains_key("tool_call_id") {
        return Err("tool_call_id");
    }
    Ok(snapshot)
}

/// Projects the Context's allowlisted baggage when any native SDK/bridge span
/// starts. Business callsites need neither metadata parameters nor custom macros.
#[derive(Debug)]
pub struct AgentFieldProcessor;
impl SpanProcessor for AgentFieldProcessor {
    fn on_start(&self, span: &mut Span, parent: &Context) {
        span.set_attributes(agent_values(parent));
    }
    fn on_end(&self, _: SpanData) {}
    fn force_flush(&self) -> OTelSdkResult {
        Ok(())
    }
    fn shutdown_with_timeout(&self, _: Duration) -> OTelSdkResult {
        Ok(())
    }
}
