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
//!   absent, the previous step's timestamp is used as an approximation.
//! - Tool execution time of a step = next agent step's `start` − this step's
//!   `end`, valid only when no user step intervenes (a user step means the
//!   turn ended and the gap is user idle, not tool time).

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Max characters kept per tool observation in [`render_trimmed`].
const OBSERVATION_TRIM_CHARS: usize = 80;

// ─── Document types ──────────────────────────────────────────────────────────

/// Root ATIF trajectory document (analysis-side mirror of the shared schema).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AtifTrajectory {
    pub schema_version: String,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
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

/// One tool result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AtifObservationResult {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
}

/// Per-step LLM billing metrics.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct AtifStepMetrics {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra: Option<()>,
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

/// Heuristic error detection for tool observations — ATIF carries no explicit
/// `is_error` flag, so we scan the head of the content for common failure
/// markers. Conservative: prefer false negatives over false positives.
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
/// tool observations to a short prefix. Preserves step order, sources, tool
/// names/arguments summaries, and message/reasoning text.
pub fn render_trimmed(traj: &AtifTrajectory) -> String {
    let mut out = String::new();
    for step in &traj.steps {
        let ts = step.timestamp.as_deref().unwrap_or("-");
        match step.source.as_str() {
            "system" => {
                let msg = step.message.as_deref().unwrap_or("");
                out.push_str(&format!(
                    "[{ts}] system: {}\n",
                    truncate_chars(msg, OBSERVATION_TRIM_CHARS)
                ));
            }
            "user" => {
                let msg = step.message.as_deref().unwrap_or("");
                out.push_str(&format!("[{ts}] user: {msg}\n"));
            }
            _ => {
                out.push_str(&format!("[{ts}] agent (step {}):\n", step.step_id));
                if let Some(r) = step.reasoning_content.as_deref() {
                    if !r.is_empty() {
                        out.push_str(&format!("  thinking: {r}\n"));
                    }
                }
                if let Some(m) = step.message.as_deref() {
                    if !m.is_empty() {
                        out.push_str(&format!("  text: {m}\n"));
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
}
