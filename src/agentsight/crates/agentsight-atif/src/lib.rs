//! ATIF v1.7 data models — shared trajectory schema crate.
//!
//! Ported from AgentOpt's `atif/models.rs` (the authoritative ATIF v1.7
//! implementation). Schema follows:
//! <https://github.com/harbor-framework/harbor/blob/main/rfcs/0001-trajectory-format.md>
//!
//! This is a leaf crate (serde/serde_json only) and the single ATIF data model
//! for the workspace: the log collector (`agentsight-trajectory-collector`) and
//! the eBPF export path (`agentsight::atif`) both produce these types.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

pub const ATIF_SCHEMA_VERSION: &str = "ATIF-v1.7";

// ---------------------------------------------------------------------------
// ToolCall
// ---------------------------------------------------------------------------

/// A tool/function invocation made by the agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub tool_call_id: String,
    pub function_name: String,
    #[serde(default)]
    pub arguments: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extra: Option<HashMap<String, serde_json::Value>>,
}

// ---------------------------------------------------------------------------
// SubagentTrajectoryRef
// ---------------------------------------------------------------------------

/// Reference to a delegated subagent trajectory (ATIF v1.7).
///
/// Resolution: `trajectory_id` matches an embedded entry in the parent's
/// `subagent_trajectories` array; `trajectory_path` points at an external file.
/// At least one MUST be set. `session_id` is informational only (run-scoped).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubagentTrajectoryRef {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trajectory_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trajectory_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extra: Option<HashMap<String, serde_json::Value>>,
}

// ---------------------------------------------------------------------------
// ObservationResult & Observation
// ---------------------------------------------------------------------------

/// Key under which a provider's out-of-band tool-failure flag is preserved in
/// [`ObservationResult::extra`]. ATIF has no typed status field, and folding the
/// flag into `content` would force consumers to re-derive failure by matching
/// error words in free text — which misfires on successful calls whose output
/// merely mentions "error".
pub const EXTRA_IS_ERROR: &str = "is_error";

/// Whether two identifiers name the same tool call.
///
/// Comparison first accepts the original identifiers case-insensitively. It
/// collapses `_` and `-` only when exactly one side contains a separator, which
/// covers the measured `call_cb113b8e…` → `callcb113b8e…` echo without merging
/// distinct IDs that both use punctuation (`call_a-1` vs `call_a_1`).
pub fn same_call_id(a: &str, b: &str) -> bool {
    if a.eq_ignore_ascii_case(b) {
        return true;
    }

    let has_separator = |s: &str| s.contains(['_', '-']);
    if has_separator(a) == has_separator(b) {
        return false;
    }

    let normalized = |s: &str| -> String {
        s.chars()
            .filter(|c| *c != '_' && *c != '-')
            .flat_map(char::to_lowercase)
            .collect()
    };
    normalized(a) == normalized(b)
}

/// A single observation result from a tool call or action.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObservationResult {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subagent_trajectory_ref: Option<Vec<SubagentTrajectoryRef>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extra: Option<HashMap<String, serde_json::Value>>,
}

impl ObservationResult {
    /// Whether the tool call behind this result failed, as reported by the
    /// provider.
    ///
    /// `None` means the provider reported nothing, which is **not** the same as
    /// success — callers must treat it as unknown rather than passing.
    pub fn is_error(&self) -> Option<bool> {
        self.extra.as_ref()?.get(EXTRA_IS_ERROR)?.as_bool()
    }
}

/// Environment feedback/results after actions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Observation {
    #[serde(default)]
    pub results: Vec<ObservationResult>,
}

// ---------------------------------------------------------------------------
// Metrics & FinalMetrics
// ---------------------------------------------------------------------------

/// LLM operational metrics for a single step.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Metrics {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completion_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cached_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub logprobs: Option<Vec<f64>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completion_token_ids: Option<Vec<u64>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_token_ids: Option<Vec<u64>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extra: Option<HashMap<String, serde_json::Value>>,
}

/// Aggregate metrics for the entire trajectory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FinalMetrics {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_prompt_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_completion_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_cached_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_cost_usd: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_steps: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extra: Option<HashMap<String, serde_json::Value>>,
}

// ---------------------------------------------------------------------------
// Agent
// ---------------------------------------------------------------------------

/// Agent configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Agent {
    pub name: String,
    #[serde(default = "default_version")]
    pub version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_definitions: Option<Vec<serde_json::Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extra: Option<HashMap<String, serde_json::Value>>,
}

fn default_version() -> String {
    "unknown".into()
}

// ---------------------------------------------------------------------------
// Step
// ---------------------------------------------------------------------------

/// Allowed `source` values for a Step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StepSource {
    System,
    User,
    Agent,
}

impl StepSource {
    /// Wire representation, matching the `lowercase` serde renaming.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::System => "system",
            Self::User => "user",
            Self::Agent => "agent",
        }
    }
}

/// Derived, multi-label classification of a [`Step`] for querying.
///
/// Not part of the ATIF wire format: `source` alone cannot express tool or
/// reasoning activity (both live *inside* an agent step), so these labels are
/// computed from the typed step fields by [`Step::categories`]. A single step
/// may carry several labels — an agent step can reason and call tools at once.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepCategory {
    UserInput,
    System,
    AgentMessage,
    Thinking,
    ToolCall,
    ToolResult,
}

impl StepCategory {
    /// Every category, in the order surfaced to API clients.
    pub const ALL: [StepCategory; 6] = [
        Self::UserInput,
        Self::System,
        Self::AgentMessage,
        Self::Thinking,
        Self::ToolCall,
        Self::ToolResult,
    ];

    /// Wire representation, matching the `snake_case` serde renaming.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::UserInput => "user_input",
            Self::System => "system",
            Self::AgentMessage => "agent_message",
            Self::Thinking => "thinking",
            Self::ToolCall => "tool_call",
            Self::ToolResult => "tool_result",
        }
    }

    /// Parses a query-parameter token; `None` for an unknown category so
    /// callers can reject it rather than silently returning wrong results.
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim() {
            "user_input" => Some(Self::UserInput),
            "system" => Some(Self::System),
            "agent_message" => Some(Self::AgentMessage),
            "thinking" => Some(Self::Thinking),
            "tool_call" => Some(Self::ToolCall),
            "tool_result" => Some(Self::ToolResult),
            _ => None,
        }
    }
}

/// A single step in an ATIF trajectory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Step {
    pub step_id: usize,
    pub source: StepSource,
    #[serde(default)]
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub observation: Option<Observation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metrics: Option<Metrics>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extra: Option<HashMap<String, serde_json::Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub llm_call_count: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_copied_context: Option<bool>,
}

impl Step {
    /// Derived query labels for this step; see [`StepCategory`].
    ///
    /// Multi-label by design: an agent step that reasons and then calls a tool
    /// yields `agent_message`/`thinking`/`tool_call` together, so filtering on
    /// any one of them finds it. Labels are emitted in [`StepCategory::ALL`]
    /// order for stable output.
    pub fn categories(&self) -> Vec<StepCategory> {
        let mut out = Vec::new();
        match self.source {
            StepSource::User => out.push(StepCategory::UserInput),
            StepSource::System => out.push(StepCategory::System),
            StepSource::Agent => {
                // Blank agent turns (pure tool dispatch, empty completion)
                // carry no message worth surfacing as `agent_message`.
                if !self.message.trim().is_empty() {
                    out.push(StepCategory::AgentMessage);
                }
            }
        }
        if self.has_reasoning() {
            out.push(StepCategory::Thinking);
        }
        if self.has_tool_calls() {
            out.push(StepCategory::ToolCall);
        }
        if self.has_observation() {
            out.push(StepCategory::ToolResult);
        }
        out
    }

    /// Whether this step carries non-blank reasoning content.
    pub fn has_reasoning(&self) -> bool {
        self.reasoning_content
            .as_ref()
            .is_some_and(|r| !r.trim().is_empty())
    }

    /// Whether this step dispatched at least one tool call.
    pub fn has_tool_calls(&self) -> bool {
        self.tool_calls.as_ref().is_some_and(|tc| !tc.is_empty())
    }

    /// Whether this step carries at least one observation result.
    ///
    /// An `Observation` with an empty `results` vector conveys nothing, so it
    /// does not count as a tool result.
    pub fn has_observation(&self) -> bool {
        self.observation
            .as_ref()
            .is_some_and(|o| !o.results.is_empty())
    }

    /// Names of the functions invoked by this step, in call order.
    pub fn tool_names(&self) -> Vec<&str> {
        self.tool_calls
            .as_deref()
            .unwrap_or_default()
            .iter()
            .map(|tc| tc.function_name.as_str())
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Trajectory (root)
// ---------------------------------------------------------------------------

/// Root-level ATIF trajectory object.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AtifTrajectory {
    #[serde(default = "default_schema_version")]
    pub schema_version: String,
    pub agent: Agent,
    #[serde(default)]
    pub steps: Vec<Step>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trajectory_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub final_metrics: Option<FinalMetrics>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub continued_trajectory_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subagent_trajectories: Option<Vec<AtifTrajectory>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extra: Option<HashMap<String, serde_json::Value>>,
}

fn default_schema_version() -> String {
    ATIF_SCHEMA_VERSION.into()
}

impl AtifTrajectory {
    /// Serialize to a JSON Value with `None` fields stripped out.
    ///
    /// # Errors
    /// Returns a `serde_json` error if serialization fails.
    pub fn to_json_value(&self) -> serde_json::Result<serde_json::Value> {
        let v = serde_json::to_value(self)?;
        Ok(strip_nulls(v))
    }

    /// Validate step_id sequence (1-based, contiguous).
    ///
    /// # Errors
    /// Returns a message describing the first out-of-sequence step.
    pub fn validate_step_ids(&self) -> Result<(), String> {
        for (i, step) in self.steps.iter().enumerate() {
            if step.step_id != i + 1 {
                return Err(format!(
                    "Step at index {} has step_id={}, expected {}",
                    i,
                    step.step_id,
                    i + 1
                ));
            }
        }
        Ok(())
    }
}

/// Recursively strip null values from a JSON object.
fn strip_nulls(v: serde_json::Value) -> serde_json::Value {
    match v {
        serde_json::Value::Object(map) => {
            let filtered: serde_json::Map<String, serde_json::Value> = map
                .into_iter()
                .filter(|(_, val)| !val.is_null())
                .map(|(k, val)| (k, strip_nulls(val)))
                .collect();
            serde_json::Value::Object(filtered)
        }
        serde_json::Value::Array(arr) => {
            serde_json::Value::Array(arr.into_iter().map(strip_nulls).collect())
        }
        other => other,
    }
}

// ---------------------------------------------------------------------------
// Validator
// ---------------------------------------------------------------------------

/// Validate an ATIF trajectory from a JSON string.
///
/// # Errors
/// Returns a message on parse failure, schema mismatch, or bad step ids.
pub fn validate_trajectory_str(json: &str) -> Result<AtifTrajectory, String> {
    let data: serde_json::Value = serde_json::from_str(json).map_err(|e| e.to_string())?;
    let trajectory: AtifTrajectory =
        serde_json::from_value(data).map_err(|e| format!("Schema validation failed: {e}"))?;
    trajectory.validate_step_ids()?;
    Ok(trajectory)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn minimal_trajectory() -> AtifTrajectory {
        AtifTrajectory {
            schema_version: ATIF_SCHEMA_VERSION.into(),
            agent: Agent {
                name: "qoder".into(),
                version: "unknown".into(),
                model_name: Some("qwen-max".into()),
                tool_definitions: None,
                extra: None,
            },
            steps: vec![Step {
                step_id: 1,
                source: StepSource::User,
                message: "hello".into(),
                timestamp: Some("2026-07-25T00:00:00Z".into()),
                model_name: None,
                reasoning_effort: None,
                reasoning_content: None,
                tool_calls: None,
                observation: None,
                metrics: None,
                extra: None,
                llm_call_count: None,
                is_copied_context: None,
            }],
            session_id: Some("s-1".into()),
            trajectory_id: None,
            notes: None,
            final_metrics: None,
            continued_trajectory_ref: None,
            subagent_trajectories: None,
            extra: None,
        }
    }

    #[test]
    fn call_id_matching_only_tolerates_one_sided_separator_loss() {
        assert!(same_call_id("CALL_abc", "call_ABC"));
        assert!(same_call_id("call_47ad96", "call47ad96"));
        assert!(same_call_id("call47ad96", "call-47ad96"));

        assert!(!same_call_id("call_a-1", "call_a_1"));
        assert!(!same_call_id("call-a-b", "call_a_b"));
        assert!(!same_call_id("call_abc", "call_def"));
    }

    #[test]
    fn schema_version_constant() {
        assert_eq!(ATIF_SCHEMA_VERSION, "ATIF-v1.7");
    }

    #[test]
    fn serialize_skips_none_fields() {
        let traj = minimal_trajectory();
        let json = serde_json::to_string(&traj).unwrap();
        assert!(!json.contains("trajectory_id"));
        assert!(!json.contains("reasoning_content"));
        assert!(json.contains("\"source\":\"user\""));
    }

    #[test]
    fn roundtrip_and_validate() {
        let traj = minimal_trajectory();
        let json = serde_json::to_string(&traj).unwrap();
        let parsed = validate_trajectory_str(&json).unwrap();
        assert_eq!(parsed.schema_version, ATIF_SCHEMA_VERSION);
        assert_eq!(parsed.session_id.as_deref(), Some("s-1"));
        assert_eq!(parsed.steps.len(), 1);
    }

    #[test]
    fn validate_rejects_bad_step_ids() {
        let mut traj = minimal_trajectory();
        traj.steps[0].step_id = 5;
        assert!(traj.validate_step_ids().is_err());
    }

    #[test]
    fn strip_nulls_removes_nested_nulls() {
        let v = serde_json::json!({"a": null, "b": {"c": null, "d": 1}, "e": [null, 2]});
        let stripped = strip_nulls(v);
        assert_eq!(stripped, serde_json::json!({"b": {"d": 1}, "e": [null, 2]}));
    }

    // ─── Derived step categories ─────────────────────────────────────────

    /// A bare step with everything optional left unset.
    fn bare_step(source: StepSource) -> Step {
        Step {
            step_id: 1,
            source,
            message: String::new(),
            timestamp: None,
            model_name: None,
            reasoning_effort: None,
            reasoning_content: None,
            tool_calls: None,
            observation: None,
            metrics: None,
            extra: None,
            llm_call_count: None,
            is_copied_context: None,
        }
    }

    fn tool_call(name: &str) -> ToolCall {
        ToolCall {
            tool_call_id: "tc-1".into(),
            function_name: name.into(),
            arguments: serde_json::json!({}),
            extra: None,
        }
    }

    #[test]
    fn category_user_input() {
        let mut step = bare_step(StepSource::User);
        step.message = "list files".into();
        assert_eq!(step.categories(), vec![StepCategory::UserInput]);
    }

    #[test]
    fn category_system() {
        let mut step = bare_step(StepSource::System);
        step.message = "you are a helpful assistant".into();
        assert_eq!(step.categories(), vec![StepCategory::System]);
    }

    #[test]
    fn category_agent_message() {
        let mut step = bare_step(StepSource::Agent);
        step.message = "Only a.txt exists.".into();
        assert_eq!(step.categories(), vec![StepCategory::AgentMessage]);
    }

    #[test]
    fn category_thinking() {
        let mut step = bare_step(StepSource::Agent);
        step.reasoning_content = Some("need to run ls".into());
        assert_eq!(step.categories(), vec![StepCategory::Thinking]);
    }

    #[test]
    fn category_tool_call() {
        let mut step = bare_step(StepSource::Agent);
        step.tool_calls = Some(vec![tool_call("bash")]);
        assert_eq!(step.categories(), vec![StepCategory::ToolCall]);
        assert_eq!(step.tool_names(), vec!["bash"]);
    }

    #[test]
    fn category_tool_result() {
        let mut step = bare_step(StepSource::User);
        step.observation = Some(Observation {
            results: vec![ObservationResult {
                source_call_id: Some("tc-1".into()),
                content: Some(serde_json::json!("a.txt")),
                subagent_trajectory_ref: None,
                extra: None,
            }],
        });
        // A tool result arrives on a user-role turn, so both labels apply.
        assert_eq!(
            step.categories(),
            vec![StepCategory::UserInput, StepCategory::ToolResult]
        );
    }

    #[test]
    fn categories_are_multi_label() {
        let mut step = bare_step(StepSource::Agent);
        step.message = "Let me check.".into();
        step.reasoning_content = Some("need to run ls".into());
        step.tool_calls = Some(vec![tool_call("bash"), tool_call("read")]);
        assert_eq!(
            step.categories(),
            vec![
                StepCategory::AgentMessage,
                StepCategory::Thinking,
                StepCategory::ToolCall,
            ]
        );
        assert_eq!(step.tool_names(), vec!["bash", "read"]);
    }

    #[test]
    fn blank_agent_message_is_not_a_category() {
        let mut step = bare_step(StepSource::Agent);
        step.message = "   \n\t ".into();
        step.tool_calls = Some(vec![tool_call("bash")]);
        // Pure tool dispatch: no readable message, so no `agent_message`.
        assert_eq!(step.categories(), vec![StepCategory::ToolCall]);
    }

    #[test]
    fn blank_reasoning_is_not_thinking() {
        let mut step = bare_step(StepSource::Agent);
        step.message = "done".into();
        step.reasoning_content = Some("  ".into());
        assert_eq!(step.categories(), vec![StepCategory::AgentMessage]);
        assert!(!step.has_reasoning());
    }

    #[test]
    fn empty_collections_are_not_categories() {
        let mut step = bare_step(StepSource::Agent);
        step.message = "done".into();
        step.tool_calls = Some(Vec::new());
        step.observation = Some(Observation {
            results: Vec::new(),
        });
        // Present-but-empty conveys nothing.
        assert_eq!(step.categories(), vec![StepCategory::AgentMessage]);
        assert!(!step.has_tool_calls());
        assert!(!step.has_observation());
        assert!(step.tool_names().is_empty());
    }

    #[test]
    fn category_parse_roundtrip() {
        for category in StepCategory::ALL {
            assert_eq!(StepCategory::parse(category.as_str()), Some(category));
        }
        assert_eq!(
            StepCategory::parse(" tool_call "),
            Some(StepCategory::ToolCall)
        );
        assert_eq!(StepCategory::parse("assistant"), None);
        assert_eq!(StepCategory::parse(""), None);
    }

    #[test]
    fn category_wire_names_match_serde() {
        // `as_str` must agree with the snake_case serde renaming, since clients
        // filter using the values they see in responses.
        for category in StepCategory::ALL {
            let json = serde_json::to_string(&category).unwrap();
            assert_eq!(json, format!("\"{}\"", category.as_str()));
        }
    }

    #[test]
    fn step_source_wire_names_match_serde() {
        for source in [StepSource::System, StepSource::User, StepSource::Agent] {
            let json = serde_json::to_string(&source).unwrap();
            assert_eq!(json, format!("\"{}\"", source.as_str()));
        }
    }
}
