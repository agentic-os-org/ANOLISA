//! Per-turn metrics collected during a single `handle_user_message` invocation.
//!
//! These counters are accumulated within one agent turn (which may span
//! multiple LLM API calls and tool executions) and then serialised into
//! the SLS JSONL record before the cosh-core process exits.

/// Aggregated statistics for a single agent turn.
#[derive(Debug, Default)]
pub struct TurnMetrics {
    // Token usage
    pub tokens_input: u64,
    pub tokens_output: u64,
    pub tokens_total: u64,
    pub tokens_cached: u64,

    // API statistics
    pub api_requests: u32,
    pub api_errors: u32,
    pub api_latency_ms: u64,

    // Tool call statistics
    pub tool_calls_total: u32,
    pub tool_calls_success: u32,
    pub tool_calls_fail: u32,
    pub tool_calls_duration_ms: u64,

    // Approval statistics
    pub approval_allow: u32,
    pub approval_deny: u32,
    pub approval_wait_ms: u64,
    /// Number of approval interactions (for computing avg wait).
    pub approval_count: u32,

    // Sandbox statistics
    // Phase 2 placeholder: sandbox_runs requires hook-level attribution
    // (PreToolUseResult does not expose which hook produced the tool_input_patch),
    // so it cannot be reliably counted yet. Always outputs 0.
    pub sandbox_runs: u32,
    pub sandbox_blocked: u32,
}
