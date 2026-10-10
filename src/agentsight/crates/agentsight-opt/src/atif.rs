//! ATIF trajectory model — the sole analysis input format.
//!
//! A lenient analysis-side reader for the shared ATIF schema (see the
//! `agentsight-atif` crate, which producers write). Kept separate for now
//! because the analyzers rely on the accessors below; collapsing the two models
//! is a follow-up.
//!
//! All analyzers (accuracy / perf / cost) consume [`AtifTrajectory`] directly.
//! See <https://github.com/laude-institute/harbor/blob/main/docs/rfcs/0001-trajectory-format.md>.
//!
//! # Timing model
//!
//! - An agent step's `timestamp` marks the **end** of its LLM call.
//! - Producers may record the request **start** time in `extra.start_timestamp`
//!   (ISO 8601). AgentSight's exporter always does.
//! - Model inference time of a step = `end − start`. When `start_timestamp` is
//!   absent, the previous step's timestamp is used as an approximation —
//!   unless that previous agent step issued tool calls (and no user step
//!   intervenes): the interval is then booked as that step's tool window
//!   instead, so the model/tool split never double-books one gap.
//! - Tool execution time of a step = next agent step's `start` − this step's
//!   `end`, valid only when no user step intervenes (a user step means the
//!   turn ended and the gap is user idle, not tool time).

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Max characters kept per tool observation in [`render_trimmed`].
const OBSERVATION_TRIM_CHARS: usize = 80;

/// Max characters kept per thinking / text block in [`render_trimmed`].
/// `render_trimmed` feeds LLM prompts (the perf experience-library
/// strategy), and reasoning models emit tens of thousands of characters of
/// hidden chain-of-thought per step — uncapped narration is the exact
/// context overflow `summary` caps its payload to avoid. The same head cap
/// bounds user narration: a pasted log or config dump in one user turn is
/// the other routine source of oversized prompt payload.
const NARRATION_TRIM_CHARS: usize = 800;

// ─── Document types ──────────────────────────────────────────────────────────

/// Root ATIF trajectory document (analysis-side mirror of the shared schema).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AtifTrajectory {
    pub schema_version: String,
    /// Informational only (run-scoped); ATIF v1.7 relaxed it to optional, and
    /// nothing in this crate reads it.
    #[serde(default)]
    pub session_id: String,
    #[serde(default)]
    pub agent: Option<AtifAgent>,
    #[serde(default)]
    pub steps: Vec<AtifStep>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_metrics: Option<AtifFinalMetrics>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra: Option<serde_json::Value>,
}

/// Agent system identification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AtifAgent {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_definitions: Option<Vec<serde_json::Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra: Option<serde_json::Value>,
}

/// One interaction step: `source` ∈ system / user / agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AtifStep {
    pub step_id: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
    pub source: String,
    #[serde(
        default,
        deserialize_with = "de_step_message",
        skip_serializing_if = "Option::is_none"
    )]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<AtifToolCall>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observation: Option<AtifObservation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metrics: Option<AtifStepMetrics>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra: Option<serde_json::Value>,
}

/// A structured tool invocation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AtifToolCall {
    #[serde(default)]
    pub tool_call_id: String,
    #[serde(default)]
    pub function_name: String,
    #[serde(default)]
    pub arguments: serde_json::Value,
}

/// Environment feedback after tool calls.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AtifObservation {
    #[serde(default)]
    pub results: Vec<AtifObservationResult>,
}

/// Flatten observation content to the text the analyzers consume.
///
/// The shared ATIF schema types `ObservationResult.content` as any JSON
/// (`agentsight-atif::ObservationResult`), and the in-repo producers flatten
/// structured responses before writing them (`src/atif/converter.rs`). A
/// document from any other producer may keep the structured shape, so the
/// lenient reader accepts it instead of rejecting the whole document with
/// "invalid type: map, expected a string".
fn de_observation_content<'de, D>(deserializer: D) -> std::result::Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(
        Option::<serde_json::Value>::deserialize(deserializer)?.map(|v| match v {
            serde_json::Value::String(s) => s,
            other => other.to_string(),
        }),
    )
}

/// Flatten a step message to the text the analyzers consume.
///
/// ATIF v1.6+ types `StepObject.message` as `String | Array<ContentPart>`, the
/// array form being how a multimodal step carries its text and attachments. The
/// in-repo producers always write the string form, but the format is
/// interoperable, so a document from another producer — or a hand-written one —
/// used to fail the whole parse with "invalid type: sequence, expected a
/// string", losing every step of the trajectory rather than flattening one
/// field. Same reasoning and same result as `de_observation_content` above.
fn de_step_message<'de, D>(deserializer: D) -> std::result::Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    de_observation_content(deserializer)
}

/// One tool result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AtifObservationResult {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_call_id: Option<String>,
    /// Tool output as text, flattened from the schema's any-JSON shape.
    #[serde(
        default,
        deserialize_with = "de_observation_content",
        skip_serializing_if = "Option::is_none"
    )]
    pub content: Option<String>,
    /// Producer extension data; `extra.is_error` carries the provider's
    /// out-of-band tool-failure flag (`EXTRA_IS_ERROR` in the shared
    /// `agentsight-atif` schema, written by both in-repo producers).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra: Option<serde_json::Value>,
}

/// Per-step LLM billing metrics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AtifStepMetrics {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached_tokens: Option<u32>,
    /// Producer extension map (schema-valid per the shared `agentsight-atif`
    /// `Metrics.extra`); the analyzer accepts and ignores it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra: Option<serde_json::Value>,
}

/// Trajectory-level aggregate metrics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AtifFinalMetrics {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_prompt_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_completion_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_cached_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_steps: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra: Option<serde_json::Value>,
}

// ─── Parsing & helpers ───────────────────────────────────────────────────────

impl AtifTrajectory {
    /// Parse an ATIF JSON document.
    ///
    /// # Errors
    /// Returns an error when the input is not valid ATIF JSON.
    pub fn from_json(json: &str) -> Result<Self> {
        serde_json::from_str(json).context("failed to parse ATIF trajectory JSON")
    }

    /// The trajectory's default model name (agent-level, falling back to the
    /// first agent step carrying one).
    pub fn model_name(&self) -> String {
        self.agent
            .as_ref()
            .and_then(|a| a.model_name.clone())
            .or_else(|| self.steps.iter().find_map(|s| s.model_name.clone()))
            .unwrap_or_else(|| "unknown".to_string())
    }

    /// Earliest timestamp in the trajectory (wall-clock origin).
    pub fn origin_ts(&self) -> Option<DateTime<Utc>> {
        self.steps
            .iter()
            .flat_map(|s| [s.start_ts(), s.end_ts()])
            .flatten()
            .min()
    }

    /// Latest timestamp in the trajectory (wall-clock end).
    pub fn last_ts(&self) -> Option<DateTime<Utc>> {
        self.steps
            .iter()
            .flat_map(|s| [s.start_ts(), s.end_ts()])
            .flatten()
            .max()
    }
}

impl AtifStep {
    pub fn is_agent(&self) -> bool {
        self.source == "agent"
    }

    pub fn is_user(&self) -> bool {
        self.source == "user"
    }

    pub fn is_system(&self) -> bool {
        self.source == "system"
    }

    /// Step timestamp (agent steps: LLM call end).
    pub fn end_ts(&self) -> Option<DateTime<Utc>> {
        parse_ts(self.timestamp.as_deref()?)
    }

    /// LLM request start time from `extra.start_timestamp`, if recorded.
    pub fn start_ts(&self) -> Option<DateTime<Utc>> {
        let raw = self.extra.as_ref()?.get("start_timestamp")?.as_str()?;
        parse_ts(raw)
    }

    /// Structured tool calls (empty slice when none).
    pub fn calls(&self) -> &[AtifToolCall] {
        self.tool_calls.as_deref().unwrap_or(&[])
    }

    /// Observation results (empty slice when none).
    pub fn results(&self) -> &[AtifObservationResult] {
        self.observation
            .as_ref()
            .map(|o| o.results.as_slice())
            .unwrap_or(&[])
    }

    /// Whether this agent step produced user-visible text (end of a turn).
    pub fn has_text_output(&self) -> bool {
        self.message.as_deref().is_some_and(|m| !m.is_empty())
    }
}

impl AtifToolCall {
    /// Compact JSON of the tool call arguments, truncated UTF-8 safe.
    pub fn command_summary(&self, max_chars: usize) -> String {
        let json = serde_json::to_string(&self.arguments).unwrap_or_default();
        if json.is_empty() || json == "{}" || json == "null" {
            return String::new();
        }
        truncate_chars(&json, max_chars)
    }

    /// The command this call ran, when it carries one.
    ///
    /// Distinct from [`Self::command_summary`], which is the whole `arguments`
    /// object: matching command keywords against the summary let *arguments*
    /// decide the verdict — a `Grep` whose pattern reads `git stash pop`, an
    /// `Edit` whose replacement text quotes 回退 — and missed a real command
    /// whose text fell outside the summary window.
    pub fn command(&self) -> Option<&str> {
        ["command", "cmd", "script"]
            .iter()
            .find_map(|key| self.arguments.get(key).and_then(|value| value.as_str()))
            .filter(|command| !command.is_empty())
    }

    /// Tool name enriched with subagent type, e.g. `Agent(Explore)`.
    pub fn display_name(&self) -> String {
        let base = if self.function_name.is_empty() {
            "unknown"
        } else {
            self.function_name.as_str()
        };
        if base == "Agent" || base == "Task" {
            if let Some(stype) = self.arguments.get("subagent_type").and_then(|v| v.as_str()) {
                return format!("{base}({stype})");
            }
        }
        base.to_string()
    }
}

fn parse_ts(raw: &str) -> Option<DateTime<Utc>> {
    raw.parse::<DateTime<Utc>>().ok()
}

/// Heuristic error detection for tool observations — fallback for documents
/// that carry no structured `extra.is_error` flag: scan the head of the
/// content for common failure markers. Conservative: prefer false negatives
/// over false positives.
pub(crate) fn observation_looks_like_error(content: &str) -> bool {
    const MARKERS: &[&str] = &[
        "error:",
        "Error:",
        "ERROR",
        "Traceback (most recent call last)",
        "panicked at",
        "command not found",
        "No such file or directory",
        "Permission denied",
        "<tool_use_error>",
    ];
    let head: String = content.trim_start().chars().take(200).collect();
    MARKERS.iter().any(|m| head.contains(m))
}

/// Whether one observation result failed: the producer's structured
/// `extra.is_error` flag wins when recorded (either polarity); the text
/// heuristic is only the fallback for flag-less documents. Single source of
/// truth for every reader that derives a failure bit from an observation.
pub(crate) fn observation_result_is_error(result: &AtifObservationResult) -> bool {
    result
        .extra
        .as_ref()
        .and_then(|e| e.get("is_error"))
        .and_then(|v| v.as_bool())
        .unwrap_or_else(|| {
            result
                .content
                .as_deref()
                .map(observation_looks_like_error)
                .unwrap_or(false)
        })
}

/// UTF-8 safe truncation with an ellipsis suffix.
pub(crate) fn truncate_chars(raw: &str, max_chars: usize) -> String {
    if raw.chars().count() > max_chars {
        let truncated: String = raw.chars().take(max_chars).collect();
        format!("{truncated}…")
    } else {
        raw.to_string()
    }
}

// ─── LLM-facing rendering ────────────────────────────────────────────────────

/// Render the trajectory as compact readable text for LLM prompts, trimming
/// tool observations to a short prefix and narration (thinking / text / user
/// messages) to a head cap. Preserves step order, sources, and tool
/// names/arguments summaries.
pub fn render_trimmed(traj: &AtifTrajectory) -> String {
    let mut out = String::new();
    for step in &traj.steps {
        let ts = step.timestamp.as_deref().unwrap_or("-");
        match step.source.as_str() {
            "system" => {
                let msg = step.message.as_deref().unwrap_or("");
                out.push_str(&format!(
                    "[{ts}] system: {}\n",
                    truncate_chars(msg, NARRATION_TRIM_CHARS)
                ));
            }
            "user" => {
                let msg = step.message.as_deref().unwrap_or("");
                out.push_str(&format!(
                    "[{ts}] user: {}\n",
                    truncate_chars(msg, NARRATION_TRIM_CHARS)
                ));
            }
            _ => {
                out.push_str(&format!("[{ts}] agent (step {}):\n", step.step_id));
                if let Some(r) = step.reasoning_content.as_deref() {
                    if !r.is_empty() {
                        out.push_str(&format!(
                            "  thinking: {}\n",
                            truncate_chars(r, NARRATION_TRIM_CHARS)
                        ));
                    }
                }
                if let Some(m) = step.message.as_deref() {
                    if !m.is_empty() {
                        out.push_str(&format!(
                            "  text: {}\n",
                            truncate_chars(m, NARRATION_TRIM_CHARS)
                        ));
                    }
                }
                for call in step.calls() {
                    out.push_str(&format!(
                        "  tool_use {}: {}\n",
                        call.display_name(),
                        call.command_summary(200)
                    ));
                }
                for result in step.results() {
                    let content = result.content.as_deref().unwrap_or("");
                    let total = content.chars().count();
                    if total > OBSERVATION_TRIM_CHARS {
                        out.push_str(&format!(
                            "  tool_result: {}…[trimmed, {} chars total]\n",
                            content
                                .chars()
                                .take(OBSERVATION_TRIM_CHARS)
                                .collect::<String>(),
                            total
                        ));
                    } else {
                        out.push_str(&format!("  tool_result: {content}\n"));
                    }
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_minimal_document() {
        let json = r#"{
            "schema_version": "ATIF-v1.6",
            "session_id": "s1",
            "agent": {"name": "TestAgent", "version": "1.0.0", "model_name": "m1"},
            "steps": [
                {"step_id": 1, "source": "user", "message": "hi",
                 "timestamp": "2026-01-01T00:00:00Z"},
                {"step_id": 2, "source": "agent", "message": "hello",
                 "timestamp": "2026-01-01T00:00:05Z",
                 "extra": {"start_timestamp": "2026-01-01T00:00:01Z"},
                 "metrics": {"prompt_tokens": 100, "completion_tokens": 10}}
            ]
        }"#;
        let traj = AtifTrajectory::from_json(json).unwrap();
        assert_eq!(traj.steps.len(), 2);
        assert_eq!(traj.model_name(), "m1");
        let agent = &traj.steps[1];
        assert!(agent.is_agent());
        let dur = (agent.end_ts().unwrap() - agent.start_ts().unwrap()).as_seconds_f64();
        assert!((dur - 4.0).abs() < 0.001);
    }

    #[test]
    fn parses_schema_valid_metrics_extra() {
        // The shared agentsight-atif schema types Metrics.extra as an
        // extension map; the lenient reader must accept (and ignore) it
        // instead of failing the whole document.
        let json = r#"{
            "schema_version": "ATIF-v1.7",
            "session_id": "s1",
            "agent": {"name": "a", "version": "1"},
            "steps": [{
                "step_id": 1, "source": "agent", "timestamp": "2026-01-01T00:00:00Z",
                "message": "hi",
                "metrics": {"prompt_tokens": 10, "completion_tokens": 5,
                            "extra": {"provider_call_id": "call_abc"}}
            }]
        }"#;
        let traj = AtifTrajectory::from_json(json).unwrap();
        let metrics = traj.steps[0].metrics.as_ref().unwrap();
        assert_eq!(metrics.prompt_tokens, Some(10));
        assert_eq!(
            metrics
                .extra
                .as_ref()
                .and_then(|e| e.get("provider_call_id")),
            Some(&serde_json::json!("call_abc"))
        );
    }

    #[test]
    fn parses_schema_valid_content_part_array_message() {
        // ATIF v1.6+ types StepObject.message as `String | Array<ContentPart>`
        // ("Extended `message` field in `StepObject` to accept either a string
        // or array of `ContentPart` objects"), and the in-repo producers always
        // write the string form. A document from another producer used to fail
        // the whole parse with "invalid type: sequence, expected a string",
        // losing every step rather than flattening one field.
        let json = r#"{
            "schema_version": "ATIF-v1.7",
            "session_id": "s1",
            "agent": {"name": "a", "version": "1"},
            "steps": [{
                "step_id": 1, "source": "user", "timestamp": "2026-01-01T00:00:00Z",
                "message": [{"type": "text", "text": "What is in this image?"},
                            {"type": "image", "source": {"media_type": "image/png",
                                                         "path": "images/step_1_input.png"}}]
            }]
        }"#;
        let traj = AtifTrajectory::from_json(json).expect("spec-valid message array must parse");
        let message = traj.steps[0]
            .message
            .as_deref()
            .expect("message must survive");
        assert!(
            message.contains("What is in this image?"),
            "content-part message must be flattened to text: {message}"
        );
    }

    #[test]
    fn keeps_string_step_message_verbatim() {
        // Guard: flattening is a no-op for the string shape the in-repo
        // producers write.
        let json = r#"{
            "schema_version": "ATIF-v1.7",
            "session_id": "s1",
            "agent": {"name": "a", "version": "1"},
            "steps": [{
                "step_id": 1, "source": "agent", "timestamp": "2026-01-01T00:00:01Z",
                "message": "hi\n"
            }]
        }"#;
        let traj = AtifTrajectory::from_json(json).unwrap();
        assert_eq!(traj.steps[0].message.as_deref(), Some("hi\n"));
    }

    #[test]
    fn parses_a_v1_7_document_without_session_id() {
        // ATIF v1.7 relaxed the top-level `session_id` to optional — the shared
        // schema types it as `Option<String>` and documents it as "informational
        // only (run-scoped)", and nothing in this crate reads it. This reader
        // still required it, so a v1.7-legal document failed the whole parse.
        let json = r#"{
            "schema_version": "ATIF-v1.7",
            "agent": {"name": "a", "version": "1"},
            "steps": [{
                "step_id": 1, "source": "user", "timestamp": "2026-01-01T00:00:00Z",
                "message": "hi"
            }]
        }"#;
        let traj = AtifTrajectory::from_json(json).expect("v1.7 documents may omit session_id");
        assert_eq!(traj.session_id, "");
        assert_eq!(traj.steps.len(), 1);
    }

    #[test]
    fn parses_schema_valid_structured_observation_content() {
        // The shared agentsight-atif schema types ObservationResult.content as
        // any JSON ("ATIF allows any JSON for observation content" — see
        // src/atif/converter.rs, which flattens for this very reason). The
        // lenient reader must accept a structured result instead of rejecting
        // the whole document with "invalid type: map, expected a string".
        let json = r#"{
            "schema_version": "ATIF-v1.7",
            "session_id": "s1",
            "agent": {"name": "a", "version": "1"},
            "steps": [{
                "step_id": 1, "source": "agent", "timestamp": "2026-01-01T00:00:01Z",
                "tool_calls": [{"tool_call_id": "c1", "function_name": "Read",
                                "arguments": {"file_path": "a.rs"}}],
                "observation": {"results": [{"source_call_id": "c1",
                    "content": {"exit_code": 1, "stdout": "boom"}}]}
            }]
        }"#;
        let traj = AtifTrajectory::from_json(json).unwrap();
        let result = &traj.steps[0].results()[0];
        let text = result.content.as_deref().expect("content must survive");
        assert!(
            text.contains("exit_code"),
            "structured content must be flattened to text: {text}"
        );
    }

    #[test]
    fn keeps_string_observation_content_verbatim() {
        // Guard: flattening is a no-op for the string shape the in-repo
        // producers write.
        let json = r#"{
            "schema_version": "ATIF-v1.7",
            "session_id": "s1",
            "agent": {"name": "a", "version": "1"},
            "steps": [{
                "step_id": 1, "source": "agent", "timestamp": "2026-01-01T00:00:01Z",
                "tool_calls": [{"tool_call_id": "c1", "function_name": "Read",
                                "arguments": {"file_path": "a.rs"}}],
                "observation": {"results": [{"source_call_id": "c1",
                    "content": "boom\n"}]}
            }]
        }"#;
        let traj = AtifTrajectory::from_json(json).unwrap();
        assert_eq!(
            traj.steps[0].results()[0].content.as_deref(),
            Some("boom\n")
        );
    }

    #[test]
    fn tool_call_summaries() {
        let call = AtifToolCall {
            tool_call_id: "c1".into(),
            function_name: "Agent".into(),
            arguments: serde_json::json!({"subagent_type": "Explore", "description": "scan"}),
        };
        assert_eq!(call.display_name(), "Agent(Explore)");
        assert_eq!(
            call.command_summary(200),
            r#"{"subagent_type":"Explore","description":"scan"}"#
        );
    }

    #[test]
    fn tool_call_summary_is_json() {
        let call = AtifToolCall {
            tool_call_id: "c2".into(),
            function_name: "Grep".into(),
            arguments: serde_json::json!({"regex": "fn main\\(\\)", "path": "/src"}),
        };
        assert_eq!(
            call.command_summary(200),
            r#"{"regex":"fn main\\(\\)","path":"/src"}"#
        );

        // Empty object → empty string.
        let call2 = AtifToolCall {
            tool_call_id: "c3".into(),
            function_name: "Noop".into(),
            arguments: serde_json::json!({}),
        };
        assert_eq!(call2.command_summary(50), "");
    }

    #[test]
    fn render_trimmed_truncates_observations() {
        let json = format!(
            r#"{{
            "schema_version": "ATIF-v1.6", "session_id": "s1",
            "agent": {{"name": "a", "version": "1"}},
            "steps": [
                {{"step_id": 1, "source": "agent", "timestamp": "2026-01-01T00:00:05Z",
                 "tool_calls": [{{"tool_call_id": "c1", "function_name": "Bash",
                                 "arguments": {{"command": "ls"}}}}],
                 "observation": {{"results": [{{"source_call_id": "c1", "content": "{}"}}]}}}}
            ]
        }}"#,
            "x".repeat(500)
        );
        let traj = AtifTrajectory::from_json(&json).unwrap();
        let text = render_trimmed(&traj);
        assert!(text.contains("tool_use Bash: {\"command\":\"ls\"}"));
        assert!(text.contains("[trimmed, 500 chars total]"));
        assert!(!text.contains(&"x".repeat(200)));
    }

    /// Per-step narration (thinking / text) must be head-capped like tool
    /// observations: `render_trimmed` feeds the perf experience-library
    /// prompt, and a reasoning-heavy trace would otherwise ship megabytes of
    /// hidden chain-of-thought into one LLM call (observed: 240k chars from
    /// a single step) — the exact context overflow `summary` caps its
    /// payload to avoid.
    #[test]
    fn render_trimmed_caps_thinking_and_text() {
        let json = String::from(
            r#"{"schema_version":"ATIF-v1.6","session_id":"s1",
                "agent":{"name":"a","version":"1"},
                "steps":[{"step_id":1,"source":"agent","timestamp":"2026-01-01T00:00:05Z",
                 "reasoning_content":"BIGTHINK","message":"BIGTEXT"}]}"#,
        )
        .replace("BIGTHINK", &"think ".repeat(20_000))
        .replace("BIGTEXT", &"text ".repeat(20_000));
        let traj = AtifTrajectory::from_json(&json).unwrap();
        let text = render_trimmed(&traj);
        assert!(
            text.chars().count() < 4_000,
            "narration must be capped, got {} chars for one step",
            text.chars().count()
        );
        assert!(text.contains("thinking: think"));
        assert!(text.contains("text: text"));
    }

    /// A user step carrying a pasted payload (a log, a config dump, a whole
    /// file) must be head-capped like every other narration: `render_trimmed`
    /// feeds the perf experience-library prompt, and the user turn is the one
    /// place oversized input routinely enters a trajectory — a single pasted
    /// log shipped verbatim is the exact context overflow the narration cap
    /// exists to avoid.
    #[test]
    fn render_trimmed_caps_user_messages() {
        let json = String::from(
            r#"{"schema_version":"ATIF-v1.6","session_id":"s1",
                "agent":{"name":"a","version":"1"},
                "steps":[
                    {"step_id":1,"source":"user","timestamp":"2026-01-01T00:00:01Z",
                     "message":"BIGLOG"},
                    {"step_id":2,"source":"user","timestamp":"2026-01-01T00:00:02Z",
                     "message":"keep this short turn verbatim"}]}"#,
        )
        .replace("BIGLOG", &"log ".repeat(20_000));
        let traj = AtifTrajectory::from_json(&json).unwrap();
        let text = render_trimmed(&traj);
        assert!(
            text.chars().count() < 4_000,
            "a pasted user payload must be capped, got {} chars",
            text.chars().count()
        );
        assert!(text.contains("user: log"));
        assert!(!text.contains(&"log ".repeat(1_000)));
        // A short user turn is not a pasted payload and stays verbatim.
        assert!(text.contains("user: keep this short turn verbatim\n"));
    }

    /// A system prompt is narration — it belongs to the same head cap as
    /// thinking/text/user, not the 80-char observation cap. A typical role
    /// instruction ("You are a senior code reviewer...") is 200-800 chars;
    /// the observation cap truncated it to a sliver the perf prompt could
    /// not read.
    #[test]
    fn render_trimmed_caps_system_prompts_as_narration() {
        // Put the distinguishing content past the 80-char observation cap:
        // under OBSERVATION it would be cut; under NARRATION it survives.
        let filler = "R".repeat(100);
        let system_prompt = format!("{filler} distributed-systems reviewer role");
        let json = serde_json::json!({
            "schema_version": "ATIF-v1.6",
            "session_id": "s1",
            "agent": {"name": "a", "version": "1"},
            "steps": [
                {"step_id": 1, "source": "system", "timestamp": "2026-01-01T00:00:01Z",
                 "message": system_prompt}
            ]
        });
        let traj = AtifTrajectory::from_json(&json.to_string()).unwrap();
        let text = render_trimmed(&traj);
        // "distributed" sits at char ~101: under the 80-char observation cap
        // it is truncated away; under the 800-char narration cap it survives.
        assert!(
            text.contains("distributed-systems"),
            "system prompt content past 80 chars must survive the narration cap"
        );
    }
}
