//! The observability hook taxonomy and its per-hook metric allowlists.
//!
//! Every list below mirrors the *declaration order* of the corresponding
//! `ObservabilityMetrics` subclass in v1 `observability/schema.py`. The order is
//! part of the wire contract: pydantic's `model_dump(exclude_unset=True)` emits
//! model fields in declaration order, so `metrics_json` byte layout depends on
//! it.

use std::fmt;

/// One supported observability hook.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ObservabilityHook {
    /// Fired before an agent run begins.
    BeforeAgentRun,
    /// Fired before a model API call.
    BeforeLlmCall,
    /// Fired after a model API call.
    AfterLlmCall,
    /// Fired before a tool invocation.
    BeforeToolCall,
    /// Fired after a tool invocation.
    AfterToolCall,
    /// Fired after an agent run completes.
    AfterAgentRun,
}

/// Shape of the correlation metadata a hook accepts.
///
/// Mirrors the three v1 metadata classes; unmodeled correlation keys are
/// dropped on ingest because v1 sets `extra="ignore"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetadataShape {
    /// `ObservabilityMetadata`: session and run only.
    Common,
    /// `ModelCallMetadata`: adds an optional `callId`.
    ModelCall,
    /// `ToolCallMetadata`: adds a required `toolCallId` and optional `callId`.
    ToolCall,
}

/// All hooks, in v1 `OBSERVABILITY_RECORD_TYPES` order.
pub const OBSERVABILITY_HOOKS: &[ObservabilityHook] = &[
    ObservabilityHook::BeforeAgentRun,
    ObservabilityHook::BeforeLlmCall,
    ObservabilityHook::AfterLlmCall,
    ObservabilityHook::BeforeToolCall,
    ObservabilityHook::AfterToolCall,
    ObservabilityHook::AfterAgentRun,
];

const BEFORE_AGENT_RUN_METRICS: &[&str] = &[
    "prompt",
    "system_prompt",
    "user_input",
    "pii_scan_input_sha256",
    "history_messages_count",
    "images_count",
    "context_window_utilization",
    "model_id",
    "model_provider",
];

const BEFORE_LLM_CALL_METRICS: &[&str] = &[
    "prompt",
    "system_prompt",
    "user_input",
    "history_messages_count",
    "images_count",
    "context_window_utilization",
    "model_id",
    "model_provider",
    "api",
    "transport",
];

const AFTER_LLM_CALL_METRICS: &[&str] = &[
    "latency_ms",
    "outcome",
    "error_category",
    "failure_kind",
    "response",
    "output_kind",
    "stop_reason",
    "assistant_texts_count",
    "tool_calls_count",
    "tool_calls",
    "request_payload_bytes",
    "response_stream_bytes",
    "time_to_first_byte_ms",
    "upstream_request_id_hash",
];

const BEFORE_TOOL_CALL_METRICS: &[&str] = &["tool_name", "parameters", "pii_scan_input_sha256"];

const AFTER_TOOL_CALL_METRICS: &[&str] = &[
    "result",
    "error",
    "pii_scan_input_sha256",
    "duration_ms",
    "status",
    "exit_code",
    "result_size_bytes",
];

const AFTER_AGENT_RUN_METRICS: &[&str] = &[
    "response",
    "output_kind",
    "stop_reason",
    "assistant_texts_count",
    "tool_calls_count",
    "tool_calls",
    "success",
    "error",
    "duration_ms",
    "total_api_calls",
    "total_tool_calls",
    "final_model_id",
    "final_model_provider",
];

impl ObservabilityHook {
    /// Returns the wire name used in the `hook` field.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::BeforeAgentRun => "before_agent_run",
            Self::BeforeLlmCall => "before_llm_call",
            Self::AfterLlmCall => "after_llm_call",
            Self::BeforeToolCall => "before_tool_call",
            Self::AfterToolCall => "after_tool_call",
            Self::AfterAgentRun => "after_agent_run",
        }
    }

    /// Parses a wire name, returning `None` for unsupported hooks.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        OBSERVABILITY_HOOKS
            .iter()
            .copied()
            .find(|hook| hook.as_str() == value)
    }

    /// Returns the allowed metric names in v1 declaration order.
    #[must_use]
    pub const fn metric_names(self) -> &'static [&'static str] {
        match self {
            Self::BeforeAgentRun => BEFORE_AGENT_RUN_METRICS,
            Self::BeforeLlmCall => BEFORE_LLM_CALL_METRICS,
            Self::AfterLlmCall => AFTER_LLM_CALL_METRICS,
            Self::BeforeToolCall => BEFORE_TOOL_CALL_METRICS,
            Self::AfterToolCall => AFTER_TOOL_CALL_METRICS,
            Self::AfterAgentRun => AFTER_AGENT_RUN_METRICS,
        }
    }

    /// Returns the metadata shape this hook is validated against.
    #[must_use]
    pub const fn metadata_shape(self) -> MetadataShape {
        match self {
            Self::BeforeAgentRun | Self::AfterAgentRun => MetadataShape::Common,
            Self::BeforeLlmCall | Self::AfterLlmCall => MetadataShape::ModelCall,
            Self::BeforeToolCall | Self::AfterToolCall => MetadataShape::ToolCall,
        }
    }
}

impl fmt::Display for ObservabilityHook {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Returns the hook names sorted the way v1 renders them in error messages.
#[must_use]
pub fn sorted_hook_names() -> Vec<&'static str> {
    let mut names: Vec<&'static str> = OBSERVABILITY_HOOKS.iter().map(|h| h.as_str()).collect();
    names.sort_unstable();
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hook_names_round_trip() {
        for hook in OBSERVABILITY_HOOKS {
            assert_eq!(ObservabilityHook::parse(hook.as_str()), Some(*hook));
        }
        assert_eq!(ObservabilityHook::parse("nope"), None);
    }

    /// Counts asserted against a live v1 `observability_hook_metric_allowlist()`.
    #[test]
    fn metric_counts_match_v1() {
        assert_eq!(ObservabilityHook::BeforeAgentRun.metric_names().len(), 9);
        assert_eq!(ObservabilityHook::BeforeLlmCall.metric_names().len(), 10);
        assert_eq!(ObservabilityHook::AfterLlmCall.metric_names().len(), 14);
        assert_eq!(ObservabilityHook::BeforeToolCall.metric_names().len(), 3);
        assert_eq!(ObservabilityHook::AfterToolCall.metric_names().len(), 7);
        assert_eq!(ObservabilityHook::AfterAgentRun.metric_names().len(), 13);
    }

    #[test]
    fn metric_names_are_unique_per_hook() {
        for hook in OBSERVABILITY_HOOKS {
            let mut seen = hook.metric_names().to_vec();
            seen.sort_unstable();
            let total = seen.len();
            seen.dedup();
            assert_eq!(seen.len(), total, "duplicate metric name in {hook}");
        }
    }

    #[test]
    fn sorted_names_match_v1_error_ordering() {
        assert_eq!(
            sorted_hook_names(),
            vec![
                "after_agent_run",
                "after_llm_call",
                "after_tool_call",
                "before_agent_run",
                "before_llm_call",
                "before_tool_call",
            ]
        );
    }

    #[test]
    fn metadata_shapes_match_v1_record_classes() {
        assert_eq!(
            ObservabilityHook::BeforeAgentRun.metadata_shape(),
            MetadataShape::Common
        );
        assert_eq!(
            ObservabilityHook::AfterAgentRun.metadata_shape(),
            MetadataShape::Common
        );
        assert_eq!(
            ObservabilityHook::BeforeLlmCall.metadata_shape(),
            MetadataShape::ModelCall
        );
        assert_eq!(
            ObservabilityHook::AfterLlmCall.metadata_shape(),
            MetadataShape::ModelCall
        );
        assert_eq!(
            ObservabilityHook::BeforeToolCall.metadata_shape(),
            MetadataShape::ToolCall
        );
        assert_eq!(
            ObservabilityHook::AfterToolCall.metadata_shape(),
            MetadataShape::ToolCall
        );
    }
}
