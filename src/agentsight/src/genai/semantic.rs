//! GenAI Semantic Data Structures
//!
//! This module defines GenAI-specific semantic structures that represent
//! LLM interactions at a higher abstraction level than raw HTTP requests/responses.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// GenAI semantic event types
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum GenAISemanticEvent {
    /// LLM API call with request/response
    LLMCall(LLMCall),
    /// Tool/function invocation
    ToolUse(ToolUse),
    /// Agent interaction/decision
    AgentInteraction(AgentInteraction),
    /// Streaming response chunk
    StreamChunk(StreamChunk),
}

/// LLM API call representation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LLMCall {
    /// Unique identifier for this call
    pub call_id: String,
    /// Timestamp when the call started (nanoseconds)
    pub start_timestamp_ns: u64,
    /// Timestamp when the call completed (nanoseconds)
    pub end_timestamp_ns: u64,
    /// Duration in nanoseconds
    pub duration_ns: u64,
    /// LLM provider (openai, anthropic, etc.)
    pub provider: String,
    /// Model name
    pub model: String,
    /// Request details
    pub request: LLMRequest,
    /// Response details
    pub response: LLMResponse,
    /// Token usage information
    pub token_usage: Option<TokenUsage>,
    /// Error information if any
    pub error: Option<String>,
    /// Process ID that made the call
    pub pid: i32,
    /// Process name
    pub process_name: String,
    /// Resolved agent name from discovery registry (e.g. "OpenClaw", "Cosh")
    pub agent_name: Option<String>,
    /// Additional metadata
    pub metadata: HashMap<String, String>,
}

/// LLM request details
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LLMRequest {
    /// Request messages in OTel parts-based format
    pub messages: Vec<InputMessage>,
    /// Temperature setting
    pub temperature: Option<f64>,
    /// Max tokens
    pub max_tokens: Option<u32>,
    /// Frequency penalty
    pub frequency_penalty: Option<f64>,
    /// Presence penalty
    pub presence_penalty: Option<f64>,
    /// Top-p sampling
    pub top_p: Option<f64>,
    /// Top-k sampling
    pub top_k: Option<f64>,
    /// Seed for reproducibility
    pub seed: Option<i64>,
    /// Stop sequences
    pub stop_sequences: Option<Vec<String>>,
    /// Stream mode enabled
    pub stream: bool,
    /// Tools/functions available (raw JSON from request)
    pub tools: Option<Vec<serde_json::Value>>,
    /// Raw request body (optional, for debugging)
    pub raw_body: Option<String>,
}

/// LLM response details
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LLMResponse {
    /// Response messages in OTel parts-based format
    pub messages: Vec<OutputMessage>,
    /// Whether response was streamed
    pub streamed: bool,
    /// Raw response body (optional, for debugging)
    pub raw_body: Option<String>,
}

/// Message part types (OTel GenAI parts-based format)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum MessagePart {
    /// Text content
    #[serde(rename = "text")]
    Text { content: String },
    /// Reasoning/thinking content
    #[serde(rename = "reasoning")]
    Reasoning { content: String },
    /// Tool call request from model
    #[serde(rename = "tool_call")]
    ToolCall {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        arguments: Option<serde_json::Value>,
    },
    /// Tool call response
    #[serde(rename = "tool_call_response")]
    ToolCallResponse {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        response: serde_json::Value,
    },
}

/// Input message (OTel ChatMessage)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InputMessage {
    /// Role (system, user, assistant, tool)
    pub role: String,
    /// Message parts
    pub parts: Vec<MessagePart>,
    /// Participant name
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// Compute the incremental ("latest round") input messages from a full request
/// history: drop all `system` messages, then keep everything from the last
/// *real* user turn onward (inclusive). Falls back to all non-system messages
/// when there is no real user message.
///
/// A "real user turn" is a `user` message that carries actual text. This is
/// important for Anthropic-style traffic, which encodes tool results as
/// `role = "user"` messages whose only content is a `tool_result` (no text).
/// Anchoring on the last *any* user message would land on such a tool-result
/// message and drop the user's actual question (the first segment of the
/// round). Mirrors the skipping logic in `GenAIBuilder::extract_last_user_raw`.
///
/// This is the single source of truth for the per-round increment that is
/// stored in SQLite (`genai_events.input_messages`) and exposed over FFI
/// (`AgentsightLLMData.input_message_delta`).
pub fn latest_round_input_messages(messages: &[InputMessage]) -> Vec<&InputMessage> {
    // A user message that carries actual text (not just a tool_result).
    fn is_text_user(m: &InputMessage) -> bool {
        m.role == "user"
            && m.parts
                .iter()
                .any(|p| matches!(p, MessagePart::Text { content } if !content.is_empty()))
    }

    let non_system: Vec<&InputMessage> = messages.iter().filter(|m| m.role != "system").collect();

    let last = non_system.iter().rposition(|m| is_text_user(m));
    let Some(mut idx) = last else {
        return non_system;
    };
    // Walk back across a contiguous run of text-bearing user messages so we
    // anchor on the FIRST message of the user's turn. Agents such as OpenClaw
    // emit the real question followed by a separate "runtime context" user
    // message; both are text-bearing, so anchoring on the last one would drop
    // the actual question.
    while idx > 0 && is_text_user(non_system[idx - 1]) {
        idx -= 1;
    }
    non_system[idx..].to_vec()
}

/// Output message (OTel OutputMessage)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutputMessage {
    /// Role (usually assistant)
    pub role: String,
    /// Message parts
    pub parts: Vec<MessagePart>,
    /// Participant name
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Finish reason (stop, length, tool_call, etc.)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finish_reason: Option<String>,
}

/// Tool definition
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    /// Tool name
    pub name: String,
    /// Tool description
    pub description: String,
    /// Parameters schema (JSON)
    pub parameters: serde_json::Value,
}

/// Token usage information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenUsage {
    /// Input/prompt tokens
    pub input_tokens: u32,
    /// Output/completion tokens
    pub output_tokens: u32,
    /// Total tokens
    pub total_tokens: u32,
    /// Cache creation tokens
    pub cache_creation_input_tokens: Option<u32>,
    /// Cache read tokens
    pub cache_read_input_tokens: Option<u32>,
}

/// Tool/function use event
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolUse {
    /// Unique identifier
    pub tool_use_id: String,
    /// Timestamp (nanoseconds)
    pub timestamp_ns: u64,
    /// Tool name
    pub tool_name: String,
    /// Tool arguments
    pub arguments: serde_json::Value,
    /// Tool result/output
    pub result: Option<String>,
    /// Duration in nanoseconds (if completed)
    pub duration_ns: Option<u64>,
    /// Success/failure status
    pub success: bool,
    /// Error message if failed
    pub error: Option<String>,
    /// Associated LLM call ID
    pub parent_llm_call_id: Option<String>,
    /// Process ID
    pub pid: i32,
}

/// Agent interaction/decision event
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentInteraction {
    /// Unique identifier
    pub interaction_id: String,
    /// Timestamp (nanoseconds)
    pub timestamp_ns: u64,
    /// Agent name/type
    pub agent_name: String,
    /// Interaction type (think, plan, decide, etc.)
    pub interaction_type: String,
    /// Content/description of the interaction
    pub content: String,
    /// Associated LLM call ID
    pub parent_llm_call_id: Option<String>,
    /// Process ID
    pub pid: i32,
}

/// Streaming response chunk
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamChunk {
    /// Unique identifier for the stream
    pub stream_id: String,
    /// Chunk sequence number
    pub chunk_index: u32,
    /// Timestamp (nanoseconds)
    pub timestamp_ns: u64,
    /// Chunk content
    pub content: String,
    /// Associated LLM call ID
    pub parent_llm_call_id: String,
    /// Process ID
    pub pid: i32,
}

impl LLMCall {
    /// Create a new LLMCall instance
    pub fn new(
        call_id: String,
        start_timestamp_ns: u64,
        provider: String,
        model: String,
        request: LLMRequest,
        pid: i32,
        process_name: String,
    ) -> Self {
        LLMCall {
            call_id,
            start_timestamp_ns,
            end_timestamp_ns: 0, // Will be set when response arrives
            duration_ns: 0,
            provider,
            model,
            request,
            response: LLMResponse {
                messages: vec![],
                streamed: false,
                raw_body: None,
            },
            token_usage: None,
            error: None,
            pid,
            process_name,
            agent_name: None,
            metadata: HashMap::new(),
        }
    }

    /// Whether this call carries no parsed semantic content.
    ///
    /// True when neither the request nor the response produced any structured
    /// messages — i.e. the underlying body format could not be parsed into
    /// `AgentsightLLMData`. Used by the FFI dispatch path to decide whether to
    /// fall back to raw-HTTP (`AgentsightHttpsData`) reporting.
    pub fn is_semantically_empty(&self) -> bool {
        self.request.messages.is_empty() && self.response.messages.is_empty()
    }

    /// Set response and calculate duration
    pub fn set_response(&mut self, response: LLMResponse, end_timestamp_ns: u64) {
        self.end_timestamp_ns = end_timestamp_ns;
        self.duration_ns = end_timestamp_ns.saturating_sub(self.start_timestamp_ns);
        self.response = response;
    }

    /// Set token usage
    pub fn set_token_usage(&mut self, usage: TokenUsage) {
        self.token_usage = Some(usage);
    }

    /// Set error
    pub fn set_error(&mut self, error: String) {
        self.error = Some(error);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn make_request() -> LLMRequest {
        LLMRequest {
            messages: vec![InputMessage {
                role: "user".to_string(),
                parts: vec![MessagePart::Text {
                    content: "Hello".to_string(),
                }],
                name: None,
            }],
            temperature: Some(0.7),
            max_tokens: Some(1024),
            frequency_penalty: None,
            presence_penalty: None,
            top_p: None,
            top_k: None,
            seed: None,
            stop_sequences: None,
            stream: false,
            tools: None,
            raw_body: None,
        }
    }

    #[test]
    fn test_llm_call_new() {
        let req = make_request();
        let call = LLMCall::new(
            "call-1".to_string(),
            1000,
            "openai".to_string(),
            "gpt-4".to_string(),
            req,
            100,
            "test".to_string(),
        );
        assert_eq!(call.call_id, "call-1");
        assert_eq!(call.end_timestamp_ns, 0);
        assert_eq!(call.duration_ns, 0);
        assert!(call.response.messages.is_empty());
        assert!(call.token_usage.is_none());
        assert!(call.error.is_none());
        assert!(call.agent_name.is_none());
    }

    #[test]
    fn test_llm_call_set_response() {
        let req = make_request();
        let mut call = LLMCall::new(
            "call-2".to_string(),
            1000,
            "anthropic".to_string(),
            "claude-3".to_string(),
            req,
            200,
            "agent".to_string(),
        );
        let resp = LLMResponse {
            messages: vec![OutputMessage {
                role: "assistant".to_string(),
                parts: vec![MessagePart::Text {
                    content: "Hi".to_string(),
                }],
                name: None,
                finish_reason: Some("stop".to_string()),
            }],
            streamed: false,
            raw_body: None,
        };
        call.set_response(resp, 5000);
        assert_eq!(call.end_timestamp_ns, 5000);
        assert_eq!(call.duration_ns, 4000);
        assert_eq!(call.response.messages.len(), 1);
    }

    #[test]
    fn test_is_semantically_empty() {
        // Request has a user message → not empty.
        let call = LLMCall::new(
            "call-3".to_string(),
            1000,
            "openai".to_string(),
            "gpt-4".to_string(),
            make_request(),
            100,
            "test".to_string(),
        );
        assert!(!call.is_semantically_empty());

        // Empty request and empty response → semantically empty.
        let empty = LLMCall::new(
            "call-4".to_string(),
            1000,
            "unknown".to_string(),
            "unknown".to_string(),
            LLMRequest {
                messages: vec![],
                temperature: None,
                max_tokens: None,
                frequency_penalty: None,
                presence_penalty: None,
                top_p: None,
                top_k: None,
                seed: None,
                stop_sequences: None,
                stream: false,
                tools: None,
                raw_body: None,
            },
            100,
            "test".to_string(),
        );
        assert!(empty.is_semantically_empty());
    }

    #[test]
    fn test_llm_call_set_token_usage() {
        let req = make_request();
        let mut call = LLMCall::new(
            "call-3".to_string(),
            0,
            "openai".to_string(),
            "gpt-4".to_string(),
            req,
            1,
            "p".to_string(),
        );
        let usage = TokenUsage {
            input_tokens: 100,
            output_tokens: 50,
            total_tokens: 150,
            cache_creation_input_tokens: None,
            cache_read_input_tokens: Some(10),
        };
        call.set_token_usage(usage);
        assert_eq!(call.token_usage.as_ref().unwrap().total_tokens, 150);
        assert_eq!(
            call.token_usage.as_ref().unwrap().cache_read_input_tokens,
            Some(10)
        );
    }

    #[test]
    fn test_llm_call_set_error() {
        let req = make_request();
        let mut call = LLMCall::new(
            "call-4".to_string(),
            0,
            "openai".to_string(),
            "gpt-4".to_string(),
            req,
            1,
            "p".to_string(),
        );
        call.set_error("timeout".to_string());
        assert_eq!(call.error.as_ref().unwrap(), "timeout");
    }

    #[test]
    fn test_message_part_serde_text() {
        let part = MessagePart::Text {
            content: "hello world".to_string(),
        };
        let json = serde_json::to_string(&part).unwrap();
        assert!(json.contains("\"type\":\"text\""));
        let parsed: MessagePart = serde_json::from_str(&json).unwrap();
        match parsed {
            MessagePart::Text { content } => assert_eq!(content, "hello world"),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn test_message_part_serde_reasoning() {
        let part = MessagePart::Reasoning {
            content: "thinking...".to_string(),
        };
        let json = serde_json::to_string(&part).unwrap();
        assert!(json.contains("\"type\":\"reasoning\""));
        let parsed: MessagePart = serde_json::from_str(&json).unwrap();
        match parsed {
            MessagePart::Reasoning { content } => assert_eq!(content, "thinking..."),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn test_message_part_serde_tool_call() {
        let part = MessagePart::ToolCall {
            id: Some("tc-1".to_string()),
            name: "search".to_string(),
            arguments: Some(json!({"query": "rust"})),
        };
        let json = serde_json::to_string(&part).unwrap();
        let parsed: MessagePart = serde_json::from_str(&json).unwrap();
        match parsed {
            MessagePart::ToolCall {
                id,
                name,
                arguments,
            } => {
                assert_eq!(id.unwrap(), "tc-1");
                assert_eq!(name, "search");
                assert_eq!(arguments.unwrap()["query"], "rust");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn test_message_part_serde_tool_call_response() {
        let part = MessagePart::ToolCallResponse {
            id: Some("tc-1".to_string()),
            response: json!({"result": "found 10 items"}),
        };
        let json = serde_json::to_string(&part).unwrap();
        let parsed: MessagePart = serde_json::from_str(&json).unwrap();
        match parsed {
            MessagePart::ToolCallResponse { id, response } => {
                assert_eq!(id.unwrap(), "tc-1");
                assert_eq!(response["result"], "found 10 items");
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn test_token_usage_serde_roundtrip() {
        let usage = TokenUsage {
            input_tokens: 500,
            output_tokens: 200,
            total_tokens: 700,
            cache_creation_input_tokens: Some(100),
            cache_read_input_tokens: Some(50),
        };
        let json = serde_json::to_string(&usage).unwrap();
        let parsed: TokenUsage = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.input_tokens, 500);
        assert_eq!(parsed.output_tokens, 200);
        assert_eq!(parsed.total_tokens, 700);
        assert_eq!(parsed.cache_creation_input_tokens, Some(100));
        assert_eq!(parsed.cache_read_input_tokens, Some(50));
    }

    #[test]
    fn test_tool_use_serde_roundtrip() {
        let tool = ToolUse {
            tool_use_id: "tu-1".to_string(),
            timestamp_ns: 999,
            tool_name: "calculator".to_string(),
            arguments: json!({"expr": "1+1"}),
            result: Some("2".to_string()),
            duration_ns: Some(500),
            success: true,
            error: None,
            parent_llm_call_id: Some("call-1".to_string()),
            pid: 42,
        };
        let json = serde_json::to_string(&tool).unwrap();
        let parsed: ToolUse = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.tool_name, "calculator");
        assert!(parsed.success);
        assert_eq!(parsed.result.unwrap(), "2");
    }

    #[test]
    fn test_agent_interaction_serde() {
        let interaction = AgentInteraction {
            interaction_id: "ai-1".to_string(),
            timestamp_ns: 1000,
            agent_name: "coder".to_string(),
            interaction_type: "think".to_string(),
            content: "analyzing code".to_string(),
            parent_llm_call_id: None,
            pid: 10,
        };
        let json = serde_json::to_string(&interaction).unwrap();
        let parsed: AgentInteraction = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.agent_name, "coder");
        assert_eq!(parsed.interaction_type, "think");
    }

    #[test]
    fn test_stream_chunk_serde() {
        let chunk = StreamChunk {
            stream_id: "s-1".to_string(),
            chunk_index: 3,
            timestamp_ns: 5000,
            content: "partial response".to_string(),
            parent_llm_call_id: "call-1".to_string(),
            pid: 7,
        };
        let json = serde_json::to_string(&chunk).unwrap();
        let parsed: StreamChunk = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.chunk_index, 3);
        assert_eq!(parsed.content, "partial response");
    }

    #[test]
    fn test_genai_semantic_event_enum_serde() {
        let event = GenAISemanticEvent::ToolUse(ToolUse {
            tool_use_id: "tu-2".to_string(),
            timestamp_ns: 100,
            tool_name: "grep".to_string(),
            arguments: json!({}),
            result: None,
            duration_ns: None,
            success: false,
            error: Some("not found".to_string()),
            parent_llm_call_id: None,
            pid: 1,
        });
        let json = serde_json::to_string(&event).unwrap();
        let parsed: GenAISemanticEvent = serde_json::from_str(&json).unwrap();
        match parsed {
            GenAISemanticEvent::ToolUse(t) => {
                assert_eq!(t.tool_name, "grep");
                assert!(!t.success);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn test_tool_definition_serde() {
        let tool = ToolDefinition {
            name: "search".to_string(),
            description: "Search the web".to_string(),
            parameters: json!({"type": "object", "properties": {"q": {"type": "string"}}}),
        };
        let json = serde_json::to_string(&tool).unwrap();
        let parsed: ToolDefinition = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.name, "search");
        assert_eq!(parsed.parameters["type"], "object");
    }

    #[test]
    fn test_input_output_message_serde() {
        let input = InputMessage {
            role: "user".to_string(),
            parts: vec![
                MessagePart::Text {
                    content: "Hello".to_string(),
                },
                MessagePart::ToolCallResponse {
                    id: Some("tc".to_string()),
                    response: json!("ok"),
                },
            ],
            name: Some("alice".to_string()),
        };
        let json = serde_json::to_string(&input).unwrap();
        let parsed: InputMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.role, "user");
        assert_eq!(parsed.parts.len(), 2);
        assert_eq!(parsed.name.unwrap(), "alice");

        let output = OutputMessage {
            role: "assistant".to_string(),
            parts: vec![MessagePart::Text {
                content: "Hi".to_string(),
            }],
            name: None,
            finish_reason: Some("stop".to_string()),
        };
        let json = serde_json::to_string(&output).unwrap();
        let parsed: OutputMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.finish_reason.unwrap(), "stop");
    }

    #[test]
    fn test_llm_call_full_serde_roundtrip() {
        let req = make_request();
        let mut call = LLMCall::new(
            "call-rt".to_string(),
            1000,
            "openai".to_string(),
            "gpt-4o".to_string(),
            req,
            42,
            "agent".to_string(),
        );
        call.set_response(
            LLMResponse {
                messages: vec![OutputMessage {
                    role: "assistant".to_string(),
                    parts: vec![MessagePart::Text {
                        content: "world".to_string(),
                    }],
                    name: None,
                    finish_reason: Some("stop".to_string()),
                }],
                streamed: true,
                raw_body: None,
            },
            5000,
        );
        call.set_token_usage(TokenUsage {
            input_tokens: 10,
            output_tokens: 5,
            total_tokens: 15,
            cache_creation_input_tokens: None,
            cache_read_input_tokens: None,
        });
        call.metadata.insert("key".to_string(), "value".to_string());

        let json = serde_json::to_string(&call).unwrap();
        let parsed: LLMCall = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.call_id, "call-rt");
        assert_eq!(parsed.duration_ns, 4000);
        assert_eq!(parsed.model, "gpt-4o");
        assert_eq!(parsed.response.messages.len(), 1);
        assert!(parsed.response.streamed);
        assert_eq!(parsed.token_usage.unwrap().total_tokens, 15);
        assert_eq!(parsed.metadata["key"], "value");
    }
}
