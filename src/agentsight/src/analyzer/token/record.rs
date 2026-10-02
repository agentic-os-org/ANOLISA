//! Token usage record for database storage

use serde::{Deserialize, Serialize};

/// Token usage record stored in the database
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenRecord {
    /// Unique record ID
    pub id: i64,
    /// Timestamp in nanoseconds since Unix epoch
    pub timestamp_ns: u64,
    /// Process ID that made the request
    pub pid: u32,
    /// Process command name
    pub comm: String,
    /// Agent name (if identifiable)
    pub agent: Option<String>,
    /// Model used
    pub model: Option<String>,
    /// LLM provider (openai, anthropic, etc.)
    pub provider: String,
    /// Input tokens count
    pub input_tokens: u64,
    /// Output tokens count
    pub output_tokens: u64,
    /// Cache creation input tokens (if applicable)
    pub cache_creation_tokens: Option<u64>,
    /// Cache read input tokens (if applicable)
    pub cache_read_tokens: Option<u64>,
    /// Request ID (for correlation)
    pub request_id: Option<String>,
    /// API endpoint
    pub endpoint: Option<String>,
    /// Tool calls extracted from SSE response (JSON strings)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<String>,
    /// Reasoning content extracted from SSE response
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
}

impl TokenRecord {
    /// Whether the provider bills cache tokens on top of `input_tokens`.
    ///
    /// Anthropic reports `cache_creation_input_tokens` and
    /// `cache_read_input_tokens` separately from `input_tokens`, so they add to
    /// the call total. OpenAI-compatible APIs — including DashScope and the
    /// Qoder CLI gateway — already count cached tokens inside `prompt_tokens`,
    /// and Gemini counts cached content inside `prompt_token_count`; adding the
    /// cache counters again would inflate every cached call.
    fn cache_billed_on_top(&self) -> bool {
        self.provider.eq_ignore_ascii_case("anthropic")
    }

    /// Input tokens as billed: the reported input plus only those cache
    /// counters the provider keeps outside of it.
    pub fn billed_input_tokens(&self) -> u64 {
        if self.cache_billed_on_top() {
            self.input_tokens
                + self.cache_creation_tokens.unwrap_or(0)
                + self.cache_read_tokens.unwrap_or(0)
        } else {
            self.input_tokens
        }
    }

    /// Total tokens for the call (billed input + output)
    pub fn total_tokens(&self) -> u64 {
        self.billed_input_tokens() + self.output_tokens
    }

    /// Create a new record with current timestamp
    pub fn new(
        pid: u32,
        comm: String,
        provider: String,
        input_tokens: u64,
        output_tokens: u64,
    ) -> Self {
        use std::time::{SystemTime, UNIX_EPOCH};

        let timestamp_ns = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);

        TokenRecord {
            id: 0, // Will be assigned by database
            timestamp_ns,
            pid,
            comm,
            agent: None,
            model: None,
            provider,
            input_tokens,
            output_tokens,
            cache_creation_tokens: None,
            cache_read_tokens: None,
            request_id: None,
            endpoint: None,
            tool_calls: Vec::new(),
            reasoning_content: None,
        }
    }

    /// Set agent name
    pub fn with_agent(mut self, agent: impl Into<String>) -> Self {
        self.agent = Some(agent.into());
        self
    }

    /// Set model name
    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }

    /// Set cache tokens
    pub fn with_cache_tokens(mut self, creation: u64, read: u64) -> Self {
        self.cache_creation_tokens = Some(creation);
        self.cache_read_tokens = Some(read);
        self
    }

    /// Set request ID
    pub fn with_request_id(mut self, id: impl Into<String>) -> Self {
        self.request_id = Some(id.into());
        self
    }

    /// Set endpoint
    pub fn with_endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.endpoint = Some(endpoint.into());
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_token_record_total() {
        let record = TokenRecord::new(1234, "python".to_string(), "openai".to_string(), 100, 50);
        assert_eq!(record.total_tokens(), 150);
    }

    /// OpenAI-compatible gateways count cached tokens inside `prompt_tokens`,
    /// so the cache counters must not be added again. Numbers taken from a
    /// captured Qoder CLI call (prompt 3021, completion 170, cached 491).
    #[test]
    fn test_openai_style_cache_is_already_inside_input() {
        let record = TokenRecord::new(1, "qodercli".to_string(), "openai".to_string(), 3021, 170)
            .with_cache_tokens(0, 491);
        assert_eq!(record.billed_input_tokens(), 3021);
        assert_eq!(record.total_tokens(), 3191);
    }

    /// Anthropic reports cache tokens outside `input_tokens`, so they add up.
    #[test]
    fn test_anthropic_style_cache_adds_to_input() {
        let record = TokenRecord::new(1, "claude".to_string(), "anthropic".to_string(), 100, 50)
            .with_cache_tokens(10, 20);
        assert_eq!(record.billed_input_tokens(), 130);
        assert_eq!(record.total_tokens(), 180);
    }

    /// Provider casing comes from whatever the wire reported, so matching must
    /// not be case-sensitive.
    #[test]
    fn test_anthropic_match_is_case_insensitive() {
        let record = TokenRecord::new(1, "c".to_string(), "Anthropic".to_string(), 100, 50)
            .with_cache_tokens(10, 20);
        assert_eq!(record.total_tokens(), 180);
    }

    /// An unrecognised provider is treated as OpenAI-style, which is what every
    /// gateway seen so far does.
    #[test]
    fn test_unknown_provider_keeps_cache_inside_input() {
        let record = TokenRecord::new(1, "p".to_string(), "unknown".to_string(), 100, 50)
            .with_cache_tokens(10, 20);
        assert_eq!(record.total_tokens(), 150);
    }

    #[test]
    fn test_token_record_builder() {
        let record = TokenRecord::new(1234, "python".to_string(), "anthropic".to_string(), 100, 50)
            .with_agent("OpenClaw")
            .with_model("claude-3-opus")
            .with_cache_tokens(10, 20);

        assert_eq!(record.agent, Some("OpenClaw".to_string()));
        assert_eq!(record.model, Some("claude-3-opus".to_string()));
        assert_eq!(record.cache_creation_tokens, Some(10));
        assert_eq!(record.cache_read_tokens, Some(20));
    }

    #[test]
    fn test_token_record_new_defaults() {
        let record = TokenRecord::new(999, "node".to_string(), "openai".to_string(), 200, 100);
        assert_eq!(record.id, 0);
        assert!(record.timestamp_ns > 0);
        assert_eq!(record.pid, 999);
        assert_eq!(record.comm, "node");
        assert!(record.agent.is_none());
        assert!(record.model.is_none());
        assert_eq!(record.provider, "openai");
        assert_eq!(record.input_tokens, 200);
        assert_eq!(record.output_tokens, 100);
        assert!(record.cache_creation_tokens.is_none());
        assert!(record.cache_read_tokens.is_none());
        assert!(record.request_id.is_none());
        assert!(record.endpoint.is_none());
        assert!(record.tool_calls.is_empty());
        assert!(record.reasoning_content.is_none());
    }

    #[test]
    fn test_with_request_id() {
        let record =
            TokenRecord::new(1, "p".to_string(), "o".to_string(), 0, 0).with_request_id("req-123");
        assert_eq!(record.request_id, Some("req-123".to_string()));
    }

    #[test]
    fn test_with_endpoint() {
        let record = TokenRecord::new(1, "p".to_string(), "o".to_string(), 0, 0)
            .with_endpoint("/v1/chat/completions");
        assert_eq!(record.endpoint, Some("/v1/chat/completions".to_string()));
    }

    #[test]
    fn test_serde_roundtrip() {
        let record = TokenRecord::new(42, "agent".to_string(), "anthropic".to_string(), 500, 200)
            .with_agent("Claude Code")
            .with_model("claude-3")
            .with_cache_tokens(50, 100)
            .with_request_id("req-abc")
            .with_endpoint("/v1/messages");
        let json = serde_json::to_string(&record).unwrap();
        let back: TokenRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(back.pid, 42);
        assert_eq!(back.comm, "agent");
        assert_eq!(back.provider, "anthropic");
        assert_eq!(back.input_tokens, 500);
        assert_eq!(back.output_tokens, 200);
        assert_eq!(back.total_tokens(), 850);
        assert_eq!(back.agent, Some("Claude Code".to_string()));
        assert_eq!(back.model, Some("claude-3".to_string()));
    }

    #[test]
    fn test_serde_skip_empty_tool_calls() {
        let record = TokenRecord::new(1, "p".to_string(), "o".to_string(), 0, 0);
        let json = serde_json::to_string(&record).unwrap();
        assert!(!json.contains("tool_calls"));
        assert!(!json.contains("reasoning_content"));
    }
}
