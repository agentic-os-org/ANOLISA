//! Interruption event types, severity levels, and core data structures.

use serde::{Deserialize, Serialize};

/// The type of conversation interruption detected.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InterruptionType {
    /// Agent process disappeared mid-session (detected by HealthChecker)
    AgentCrash,
    /// HTTP 429 or error containing "rate_limit"
    RateLimit,
    /// HTTP 401/403 or error containing "invalid_api_key" / "unauthorized"
    AuthError,
    /// HTTP 408/504 or error containing "timeout" (gateway-level only)
    NetworkTimeout,
    /// HTTP 502/503 or error containing "overloaded" / "service_unavailable"
    ServiceUnavailable,
    /// finish_reason == "content_filter" from LLM safety policy
    SafetyFilter,
    /// SSE stream ended without a normal finish_reason
    /// (stop/tool_calls/end_turn/tool_use/stop_sequence/pause_turn)
    /// and not due to a token-limit finish (length/max_tokens)
    SseTruncated,
    /// context_length_exceeded or similar context-bound errors
    ContextOverflow,
    /// finish_reason == "length" and output tokens exceed threshold
    TokenLimit,
    /// HTTP status_code >= 400 的通用兜底（优先级最低，在所有特定类型之后）
    LlmError,
    /// Same error type repeated > threshold times in one conversation (agent stuck retrying)
    RetryStorm,
    /// Agent stuck in a logical loop: repeated tool sequences or similar LLM outputs
    /// without meaningful progress (no errors, just looping behavior)
    DeadLoop,
    /// Tool/function execution failed (ToolUse.success == false)
    ToolFailure,
    /// LLM returned HTTP 200 but produced no output messages and no error
    EmptyResponse,
    /// API quota or billing limit exhausted (distinct from per-minute rate limiting)
    ResourceExhaustion,
    /// LLM call succeeded but response time exceeded threshold
    SlowResponse,
    /// Agent protocol or state-machine error (malformed response, invalid transition)
    StateMachineError,
    /// Tool attempted an operation denied by permission system or sandbox
    UnauthorizedAction,
}

impl InterruptionType {
    /// Every variant, in declaration order.
    ///
    /// Single source of truth for CLI filters and API validation so a new
    /// variant cannot silently stay unselectable.
    pub const ALL: [InterruptionType; 18] = [
        Self::AgentCrash,
        Self::RateLimit,
        Self::AuthError,
        Self::NetworkTimeout,
        Self::ServiceUnavailable,
        Self::SafetyFilter,
        Self::SseTruncated,
        Self::ContextOverflow,
        Self::TokenLimit,
        Self::LlmError,
        Self::RetryStorm,
        Self::DeadLoop,
        Self::ToolFailure,
        Self::EmptyResponse,
        Self::ResourceExhaustion,
        Self::SlowResponse,
        Self::StateMachineError,
        Self::UnauthorizedAction,
    ];

    /// String identifier stored in the database
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::AgentCrash => "agent_crash",
            Self::RateLimit => "rate_limit",
            Self::AuthError => "auth_error",
            Self::NetworkTimeout => "network_timeout",
            Self::ServiceUnavailable => "service_unavailable",
            Self::SafetyFilter => "safety_filter",
            Self::SseTruncated => "sse_truncated",
            Self::ContextOverflow => "context_overflow",
            Self::TokenLimit => "token_limit",
            Self::LlmError => "llm_error",
            Self::RetryStorm => "retry_storm",
            Self::DeadLoop => "dead_loop",
            Self::ToolFailure => "tool_failure",
            Self::EmptyResponse => "empty_response",
            Self::ResourceExhaustion => "resource_exhaustion",
            Self::SlowResponse => "slow_response",
            Self::StateMachineError => "state_machine_error",
            Self::UnauthorizedAction => "unauthorized_action",
        }
    }

    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "agent_crash" => Some(Self::AgentCrash),
            "rate_limit" => Some(Self::RateLimit),
            "auth_error" => Some(Self::AuthError),
            "network_timeout" => Some(Self::NetworkTimeout),
            "service_unavailable" => Some(Self::ServiceUnavailable),
            "safety_filter" => Some(Self::SafetyFilter),
            "sse_truncated" => Some(Self::SseTruncated),
            "context_overflow" => Some(Self::ContextOverflow),
            "token_limit" => Some(Self::TokenLimit),
            "llm_error" => Some(Self::LlmError),
            "retry_storm" => Some(Self::RetryStorm),
            "dead_loop" => Some(Self::DeadLoop),
            "tool_failure" => Some(Self::ToolFailure),
            "empty_response" => Some(Self::EmptyResponse),
            "resource_exhaustion" => Some(Self::ResourceExhaustion),
            "slow_response" => Some(Self::SlowResponse),
            "state_machine_error" => Some(Self::StateMachineError),
            "unauthorized_action" => Some(Self::UnauthorizedAction),
            _ => None,
        }
    }

    /// Default severity for this interruption type
    pub fn default_severity(&self) -> Severity {
        match self {
            Self::AgentCrash => Severity::Critical,
            Self::RateLimit => Severity::Medium,
            Self::AuthError => Severity::High,
            Self::NetworkTimeout => Severity::High,
            Self::ServiceUnavailable => Severity::High,
            Self::SafetyFilter => Severity::Medium,
            Self::SseTruncated => Severity::High,
            Self::ContextOverflow => Severity::High,
            Self::TokenLimit => Severity::Medium,
            Self::LlmError => Severity::High,
            Self::RetryStorm => Severity::Critical,
            Self::DeadLoop => Severity::Critical,
            Self::ToolFailure => Severity::Medium,
            Self::EmptyResponse => Severity::High,
            Self::ResourceExhaustion => Severity::High,
            Self::SlowResponse => Severity::Medium,
            Self::StateMachineError => Severity::High,
            Self::UnauthorizedAction => Severity::Medium,
        }
    }
}

/// Severity of the interruption
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Critical,
    High,
    Medium,
    Low,
}

impl Severity {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Critical => "critical",
            Self::High => "high",
            Self::Medium => "medium",
            Self::Low => "low",
        }
    }

    /// Numeric weight for comparison (higher = worse)
    pub fn weight(&self) -> u8 {
        match self {
            Self::Critical => 4,
            Self::High => 3,
            Self::Medium => 2,
            Self::Low => 1,
        }
    }
}

/// A single detected interruption event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InterruptionEvent {
    /// Unique identifier (UUID v4 hex, 32 chars)
    pub interruption_id: String,
    pub session_id: Option<String>,
    pub trace_id: Option<String>,
    pub conversation_id: Option<String>,
    pub call_id: Option<String>,
    pub pid: Option<i32>,
    pub agent_name: Option<String>,
    pub interruption_type: InterruptionType,
    pub severity: Severity,
    /// Occurrence timestamp (nanoseconds since Unix epoch)
    pub occurred_at_ns: i64,
    /// JSON-encoded detail (model, error message, finish_reason, etc.)
    pub detail: Option<String>,
    /// Whether the event has been acknowledged / resolved
    pub resolved: bool,
}

impl InterruptionEvent {
    /// Create a new unresolved interruption event with auto-generated ID
    pub fn new(
        itype: InterruptionType,
        session_id: Option<String>,
        trace_id: Option<String>,
        conversation_id: Option<String>,
        call_id: Option<String>,
        pid: Option<i32>,
        agent_name: Option<String>,
        occurred_at_ns: i64,
        detail: Option<serde_json::Value>,
    ) -> Self {
        let severity = itype.default_severity();
        InterruptionEvent {
            interruption_id: new_id(),
            session_id,
            trace_id,
            conversation_id,
            call_id,
            pid,
            agent_name,
            interruption_type: itype,
            severity,
            occurred_at_ns,
            detail: detail.map(|v| v.to_string()),
            resolved: false,
        }
    }
}

fn new_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

/// Decoded `task_struct->exit_code` (wait(2) status encoding).
///
/// Assumes the Linux wait(2) encoding of the raw value; this matches the
/// procmon BPF probe, which is Linux-only, so no other platform layout is
/// supported.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcessExitStatus {
    /// Terminating signal number; 0 when the process exited voluntarily.
    pub signal: u32,
    /// Whether the kernel produced a core dump.
    pub core_dump: bool,
    /// Exit code passed to exit(); meaningful only when `signal == 0`.
    pub code: u32,
}

impl ProcessExitStatus {
    /// Decode the raw kernel exit_code: low 7 bits = terminating signal,
    /// bit 7 = core-dump flag, bits 8..=15 = exit code.
    pub fn decode(raw: u32) -> Self {
        Self {
            signal: raw & 0x7f,
            core_dump: (raw & 0x80) != 0,
            code: (raw >> 8) & 0xff,
        }
    }

    /// True when the process terminated voluntarily with exit code 0.
    pub fn is_clean(self) -> bool {
        self.signal == 0 && self.code == 0
    }

    /// True when the process was reaped with SIGTERM and produced no core
    /// dump. This is the canonical parent-initiated shutdown for per-request
    /// worker agents, but it is not proof of graceful shutdown by itself:
    /// callers must also verify the agent belongs to a known worker lifecycle
    /// via [`is_reap_worker_agent`] before suppressing a crash.
    pub fn is_graceful_reap(self) -> bool {
        self.signal == libc::SIGTERM as u32 && !self.core_dump
    }
}

/// Agent families known to fork a short-lived worker per request and reap it
/// with SIGTERM after completion. The graceful-reap exemption is limited to
/// these families; every other agent keeps the historical crash behavior for
/// signal termination.
pub fn is_reap_worker_agent(agent_name: &str) -> bool {
    matches!(agent_name, "Cosh" | "CoshNG")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_interruption_type_as_str() {
        assert_eq!(InterruptionType::AgentCrash.as_str(), "agent_crash");
        assert_eq!(InterruptionType::RateLimit.as_str(), "rate_limit");
        assert_eq!(InterruptionType::AuthError.as_str(), "auth_error");
        assert_eq!(InterruptionType::NetworkTimeout.as_str(), "network_timeout");
        assert_eq!(
            InterruptionType::ServiceUnavailable.as_str(),
            "service_unavailable"
        );
        assert_eq!(InterruptionType::SafetyFilter.as_str(), "safety_filter");
        assert_eq!(InterruptionType::SseTruncated.as_str(), "sse_truncated");
        assert_eq!(
            InterruptionType::ContextOverflow.as_str(),
            "context_overflow"
        );
        assert_eq!(InterruptionType::TokenLimit.as_str(), "token_limit");
        assert_eq!(InterruptionType::LlmError.as_str(), "llm_error");
        assert_eq!(InterruptionType::RetryStorm.as_str(), "retry_storm");
        assert_eq!(InterruptionType::DeadLoop.as_str(), "dead_loop");
        assert_eq!(InterruptionType::ToolFailure.as_str(), "tool_failure");
        assert_eq!(InterruptionType::EmptyResponse.as_str(), "empty_response");
        assert_eq!(
            InterruptionType::ResourceExhaustion.as_str(),
            "resource_exhaustion"
        );
        assert_eq!(InterruptionType::SlowResponse.as_str(), "slow_response");
        assert_eq!(
            InterruptionType::StateMachineError.as_str(),
            "state_machine_error"
        );
        assert_eq!(
            InterruptionType::UnauthorizedAction.as_str(),
            "unauthorized_action"
        );
    }

    #[test]
    fn test_interruption_type_from_str() {
        assert_eq!(
            InterruptionType::from_str("agent_crash"),
            Some(InterruptionType::AgentCrash)
        );
        assert_eq!(
            InterruptionType::from_str("rate_limit"),
            Some(InterruptionType::RateLimit)
        );
        assert_eq!(
            InterruptionType::from_str("auth_error"),
            Some(InterruptionType::AuthError)
        );
        assert_eq!(
            InterruptionType::from_str("network_timeout"),
            Some(InterruptionType::NetworkTimeout)
        );
        assert_eq!(
            InterruptionType::from_str("service_unavailable"),
            Some(InterruptionType::ServiceUnavailable)
        );
        assert_eq!(
            InterruptionType::from_str("safety_filter"),
            Some(InterruptionType::SafetyFilter)
        );
        assert_eq!(
            InterruptionType::from_str("sse_truncated"),
            Some(InterruptionType::SseTruncated)
        );
        assert_eq!(
            InterruptionType::from_str("context_overflow"),
            Some(InterruptionType::ContextOverflow)
        );
        assert_eq!(
            InterruptionType::from_str("token_limit"),
            Some(InterruptionType::TokenLimit)
        );
        assert_eq!(
            InterruptionType::from_str("llm_error"),
            Some(InterruptionType::LlmError)
        );
        assert_eq!(
            InterruptionType::from_str("retry_storm"),
            Some(InterruptionType::RetryStorm)
        );
        assert_eq!(
            InterruptionType::from_str("dead_loop"),
            Some(InterruptionType::DeadLoop)
        );
        assert_eq!(
            InterruptionType::from_str("tool_failure"),
            Some(InterruptionType::ToolFailure)
        );
        assert_eq!(
            InterruptionType::from_str("empty_response"),
            Some(InterruptionType::EmptyResponse)
        );
        assert_eq!(
            InterruptionType::from_str("resource_exhaustion"),
            Some(InterruptionType::ResourceExhaustion)
        );
        assert_eq!(
            InterruptionType::from_str("slow_response"),
            Some(InterruptionType::SlowResponse)
        );
        assert_eq!(
            InterruptionType::from_str("state_machine_error"),
            Some(InterruptionType::StateMachineError)
        );
        assert_eq!(
            InterruptionType::from_str("unauthorized_action"),
            Some(InterruptionType::UnauthorizedAction)
        );
        assert_eq!(InterruptionType::from_str("unknown"), None);
        assert_eq!(InterruptionType::from_str(""), None);
    }

    #[test]
    fn test_interruption_type_default_severity() {
        assert_eq!(
            InterruptionType::AgentCrash.default_severity(),
            Severity::Critical
        );
        assert_eq!(
            InterruptionType::RateLimit.default_severity(),
            Severity::Medium
        );
        assert_eq!(
            InterruptionType::AuthError.default_severity(),
            Severity::High
        );
        assert_eq!(
            InterruptionType::NetworkTimeout.default_severity(),
            Severity::High
        );
        assert_eq!(
            InterruptionType::ServiceUnavailable.default_severity(),
            Severity::High
        );
        assert_eq!(
            InterruptionType::SafetyFilter.default_severity(),
            Severity::Medium
        );
        assert_eq!(
            InterruptionType::SseTruncated.default_severity(),
            Severity::High
        );
        assert_eq!(
            InterruptionType::ContextOverflow.default_severity(),
            Severity::High
        );
        assert_eq!(
            InterruptionType::TokenLimit.default_severity(),
            Severity::Medium
        );
        assert_eq!(
            InterruptionType::LlmError.default_severity(),
            Severity::High
        );
        assert_eq!(
            InterruptionType::RetryStorm.default_severity(),
            Severity::Critical
        );
        assert_eq!(
            InterruptionType::DeadLoop.default_severity(),
            Severity::Critical
        );
        assert_eq!(
            InterruptionType::ToolFailure.default_severity(),
            Severity::Medium
        );
        assert_eq!(
            InterruptionType::EmptyResponse.default_severity(),
            Severity::High
        );
        assert_eq!(
            InterruptionType::ResourceExhaustion.default_severity(),
            Severity::High
        );
        assert_eq!(
            InterruptionType::SlowResponse.default_severity(),
            Severity::Medium
        );
        assert_eq!(
            InterruptionType::StateMachineError.default_severity(),
            Severity::High
        );
        assert_eq!(
            InterruptionType::UnauthorizedAction.default_severity(),
            Severity::Medium
        );
    }

    #[test]
    fn test_severity_as_str() {
        assert_eq!(Severity::Critical.as_str(), "critical");
        assert_eq!(Severity::High.as_str(), "high");
        assert_eq!(Severity::Medium.as_str(), "medium");
        assert_eq!(Severity::Low.as_str(), "low");
    }

    #[test]
    fn test_severity_weight_ordering() {
        assert!(Severity::Critical.weight() > Severity::High.weight());
        assert!(Severity::High.weight() > Severity::Medium.weight());
        assert!(Severity::Medium.weight() > Severity::Low.weight());
    }

    #[test]
    fn test_severity_weight_values() {
        assert_eq!(Severity::Critical.weight(), 4);
        assert_eq!(Severity::High.weight(), 3);
        assert_eq!(Severity::Medium.weight(), 2);
        assert_eq!(Severity::Low.weight(), 1);
    }

    #[test]
    fn test_interruption_event_new() {
        let event = InterruptionEvent::new(
            InterruptionType::LlmError,
            Some("session-1".to_string()),
            Some("trace-1".to_string()),
            Some("conv-1".to_string()),
            Some("call-1".to_string()),
            Some(1234),
            Some("my-agent".to_string()),
            1_000_000_000,
            Some(serde_json::json!({"status_code": 500})),
        );
        assert_eq!(event.interruption_type, InterruptionType::LlmError);
        assert_eq!(event.severity, Severity::High);
        assert_eq!(event.session_id, Some("session-1".to_string()));
        assert_eq!(event.trace_id, Some("trace-1".to_string()));
        assert_eq!(event.conversation_id, Some("conv-1".to_string()));
        assert_eq!(event.call_id, Some("call-1".to_string()));
        assert_eq!(event.pid, Some(1234));
        assert_eq!(event.agent_name, Some("my-agent".to_string()));
        assert_eq!(event.occurred_at_ns, 1_000_000_000);
        assert!(!event.resolved);
        assert!(event.detail.is_some());
        assert_eq!(event.interruption_id.len(), 32);
    }

    #[test]
    fn test_interruption_event_new_no_detail() {
        let event = InterruptionEvent::new(
            InterruptionType::AgentCrash,
            None,
            None,
            None,
            None,
            None,
            None,
            500_000,
            None,
        );
        assert_eq!(event.interruption_type, InterruptionType::AgentCrash);
        assert_eq!(event.severity, Severity::Critical);
        assert!(event.session_id.is_none());
        assert!(event.detail.is_none());
        assert!(!event.resolved);
    }

    #[test]
    fn test_new_id_uniqueness() {
        let id1 = new_id();
        let id2 = new_id();
        assert_eq!(id1.len(), 32);
        assert_eq!(id2.len(), 32);
        assert!(id1.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(id1, id2);
    }

    #[test]
    fn test_interruption_type_serde_roundtrip() {
        let types = vec![
            InterruptionType::AgentCrash,
            InterruptionType::RateLimit,
            InterruptionType::AuthError,
            InterruptionType::NetworkTimeout,
            InterruptionType::ServiceUnavailable,
            InterruptionType::SafetyFilter,
            InterruptionType::SseTruncated,
            InterruptionType::ContextOverflow,
            InterruptionType::TokenLimit,
            InterruptionType::LlmError,
            InterruptionType::RetryStorm,
            InterruptionType::DeadLoop,
            InterruptionType::ToolFailure,
            InterruptionType::EmptyResponse,
            InterruptionType::ResourceExhaustion,
            InterruptionType::SlowResponse,
            InterruptionType::StateMachineError,
            InterruptionType::UnauthorizedAction,
        ];
        for t in types {
            let json = serde_json::to_string(&t).unwrap();
            let back: InterruptionType = serde_json::from_str(&json).unwrap();
            assert_eq!(t, back);
        }
    }

    #[test]
    fn test_severity_serde_roundtrip() {
        let severities = vec![
            Severity::Critical,
            Severity::High,
            Severity::Medium,
            Severity::Low,
        ];
        for s in severities {
            let json = serde_json::to_string(&s).unwrap();
            let back: Severity = serde_json::from_str(&json).unwrap();
            assert_eq!(s, back);
        }
    }

    // Raw values below were captured on a real kernel (issue #1989):
    // exit 0 → 0x0, exit 3 → 0x300, SIGKILL → 0x9, SIGSEGV+core → 0x8b.

    #[test]
    fn test_decode_clean_exit() {
        let s = ProcessExitStatus::decode(0x0);
        assert_eq!(s.signal, 0);
        assert!(!s.core_dump);
        assert_eq!(s.code, 0);
        assert!(s.is_clean());
    }

    #[test]
    fn test_decode_nonzero_exit_code() {
        let s = ProcessExitStatus::decode(0x300);
        assert_eq!(s.signal, 0);
        assert!(!s.core_dump);
        assert_eq!(s.code, 3);
        assert!(!s.is_clean());
    }

    #[test]
    fn test_decode_sigkill() {
        let s = ProcessExitStatus::decode(0x9);
        assert_eq!(s.signal, 9);
        assert!(!s.core_dump);
        assert_eq!(s.code, 0);
        assert!(!s.is_clean());
    }

    #[test]
    fn test_decode_sigterm_reap_is_clean() {
        // SIGTERM → raw 0x0f (measured on a real kernel). Signal termination is
        // not a clean exit by itself, but it is a candidate graceful reap that
        // callers may accept only for known worker-lifecycle agents.
        let s = ProcessExitStatus::decode(0x0f);
        assert_eq!(s.signal, 15);
        assert!(!s.core_dump);
        assert_eq!(s.code, 0);
        assert!(!s.is_clean(), "SIGTERM alone must not be a clean exit");
        assert!(
            s.is_graceful_reap(),
            "SIGTERM without core dump is a reap candidate"
        );
    }

    #[test]
    fn test_decode_sigint_and_sighup_are_not_graceful_reaps() {
        // Ctrl-C (SIGINT) and terminal hangup (SIGHUP) interrupt a session; they
        // must never be classified as the parent-initiated reap of a worker.
        assert!(!ProcessExitStatus::decode(0x02).is_clean());
        assert!(!ProcessExitStatus::decode(0x02).is_graceful_reap());
        assert!(!ProcessExitStatus::decode(0x01).is_clean());
        assert!(!ProcessExitStatus::decode(0x01).is_graceful_reap());
    }

    #[test]
    fn test_decode_sigterm_with_core_dump_is_not_clean() {
        // SIGTERM with the core-dump bit set (0x80 | 0x0f) must remain a
        // crash: a core dump is hard evidence of a fault.
        let s = ProcessExitStatus::decode(0x8f);
        assert_eq!(s.signal, 15);
        assert!(s.core_dump);
        assert!(!s.is_clean());
        assert!(!s.is_graceful_reap());
    }

    #[test]
    fn test_is_reap_worker_agent() {
        assert!(is_reap_worker_agent("Cosh"));
        assert!(is_reap_worker_agent("CoshNG"));
        assert!(!is_reap_worker_agent("Codex"));
        assert!(!is_reap_worker_agent("Claude"));
    }

    #[test]
    fn test_decode_sigsegv_with_core_dump() {
        let s = ProcessExitStatus::decode(0x8b);
        assert_eq!(s.signal, 11);
        assert!(s.core_dump);
        assert_eq!(s.code, 0);
        assert!(!s.is_clean());
    }

    #[test]
    fn test_decode_exit_code_255_boundary() {
        let s = ProcessExitStatus::decode(0xff00);
        assert_eq!(s.signal, 0);
        assert!(!s.core_dump);
        assert_eq!(s.code, 255);
        assert!(!s.is_clean());
    }

    #[test]
    fn all_lists_every_variant_exactly_once() {
        let identifiers: Vec<&str> = InterruptionType::ALL.iter().map(|t| t.as_str()).collect();
        let unique: std::collections::HashSet<&str> = identifiers.iter().copied().collect();
        assert_eq!(
            unique.len(),
            identifiers.len(),
            "InterruptionType::ALL repeats a variant"
        );

        // Round-tripping through from_str proves each identifier is the one the
        // database and the CLI filters use.
        for interruption_type in InterruptionType::ALL {
            let identifier = interruption_type.as_str();
            assert_eq!(
                InterruptionType::from_str(identifier),
                Some(interruption_type),
                "identifier {identifier} does not round-trip"
            );
        }
    }
}
