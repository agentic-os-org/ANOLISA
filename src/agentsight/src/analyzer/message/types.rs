//! Message types for LLM API request/response parsing
//!
//! This module defines structured types for parsing OpenAI Chat Completions
//! and Anthropic Messages API formats.
//!
//! # Supported APIs
//! - OpenAI Chat Completions (`/v1/chat/completions`)
//! - Anthropic Messages (`/v1/messages`)

use serde::{Deserialize, Serialize};

// ============================================================================
// Common Types
// ============================================================================

/// Unified message role across different LLM providers
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[derive(Default)]
pub enum MessageRole {
    /// System message (instructions/context)
    /// Note: Some newer OpenAI models use "developer" instead of "system"
    System,
    /// Developer message (OpenAI o1/o3 models)
    /// Used for developer instructions that should take precedence over user messages
    Developer,
    /// User message (human input)
    #[default]
    User,
    /// Assistant message (LLM response)
    Assistant,
    /// Tool/Function message
    Tool,
}

// ============================================================================
// OpenAI Types
// ============================================================================

/// OpenAI Chat Completions API request body
///
/// Reference: https://platform.openai.com/docs/api-reference/chat/create
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAIRequest {
    /// Model ID to use (e.g., "gpt-4", "gpt-3.5-turbo")
    pub model: String,

    /// List of messages in the conversation
    pub messages: Vec<OpenAIChatMessage>,

    /// Sampling temperature (0.0 - 2.0)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,

    /// Maximum number of tokens to generate
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,

    /// Whether to stream the response
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream: Option<bool>,

    /// Top-p sampling parameter
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f64>,

    /// Number of completions to generate
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub n: Option<u32>,

    /// Stop sequences
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop: Option<Vec<String>>,

    /// Presence penalty (-2.0 to 2.0)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presence_penalty: Option<f64>,

    /// Frequency penalty (-2.0 to 2.0)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frequency_penalty: Option<f64>,

    /// User identifier for abuse monitoring
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,

    /// Tools available to the model
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<serde_json::Value>>,

    /// Tool choice preference (auto, none, or specific tool)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<serde_json::Value>,

    /// Response format specification (e.g., JSON mode)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_format: Option<serde_json::Value>,

    /// Seed for deterministic sampling
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<i64>,

    /// Whether to return log probabilities
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logprobs: Option<bool>,

    /// Number of top logprobs to return (0-20)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_logprobs: Option<u32>,

    /// Whether to allow parallel tool calls
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parallel_tool_calls: Option<bool>,
}

/// OpenAI Chat Completions API response body
///
/// Reference: https://platform.openai.com/docs/api-reference/chat/object
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAIResponse {
    /// Unique completion ID
    pub id: String,

    /// Object type (always "chat.completion")
    pub object: String,

    /// Unix timestamp of creation
    pub created: u64,

    /// Model used for the completion
    pub model: String,

    /// List of completion choices
    pub choices: Vec<OpenAIChoice>,

    /// Token usage statistics
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<OpenAIUsage>,

    /// System fingerprint
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_fingerprint: Option<String>,
}

/// OpenAI chat message structure
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAIChatMessage {
    /// Role of the message sender
    pub role: MessageRole,

    /// Message content (can be string or array for vision)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<OpenAIContent>,

    /// Reasoning content (for models like Qwen with reasoning)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,

    /// Refusal message (when model refuses to answer)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refusal: Option<String>,

    /// Function call (deprecated, use tool_calls)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub function_call: Option<serde_json::Value>,

    /// Tool calls made by the assistant
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<serde_json::Value>>,

    /// Tool call ID (for tool response messages)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,

    /// Name of the function/tool
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,

    /// Annotations for the message (e.g., citations, file references)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub annotations: Option<Vec<serde_json::Value>>,

    /// Audio output data (when audio output modality is requested)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio: Option<serde_json::Value>,
}

/// OpenAI message content (can be string or array of content parts)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum OpenAIContent {
    /// Simple text content
    Text(String),
    /// Array of content parts (for multi-modal)
    Parts(Vec<OpenAIContentPart>),
}

impl OpenAIContent {
    /// Extract text content as a single string
    pub fn as_text(&self) -> String {
        match self {
            OpenAIContent::Text(s) => s.clone(),
            OpenAIContent::Parts(parts) => parts
                .iter()
                .filter_map(|p| match p {
                    OpenAIContentPart::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join(""),
        }
    }
}

/// OpenAI content part for multi-modal messages
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum OpenAIContentPart {
    /// Text content part
    #[serde(rename = "text")]
    Text {
        /// The text content
        text: String,
    },
    /// Image URL content part
    #[serde(rename = "image_url")]
    ImageUrl {
        /// Image URL object
        image_url: OpenAIImageUrl,
    },
}

/// OpenAI image URL structure
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAIImageUrl {
    /// URL of the image
    pub url: String,
    /// Detail level (auto, low, high)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// OpenAI completion choice
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAIChoice {
    /// Index of the choice
    pub index: u32,

    /// The generated message
    pub message: OpenAIChatMessage,

    /// Reason for finishing (stop, length, tool_calls, etc.)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finish_reason: Option<String>,

    /// Log probabilities (if requested)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logprobs: Option<serde_json::Value>,
}

/// OpenAI token usage statistics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAIUsage {
    /// Number of tokens in the prompt
    pub prompt_tokens: u64,

    /// Number of tokens in the completion
    pub completion_tokens: u64,

    /// Total tokens used
    pub total_tokens: u64,

    /// Detailed prompt token breakdown
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_tokens_details: Option<serde_json::Value>,

    /// Detailed completion token breakdown
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion_tokens_details: Option<serde_json::Value>,
}

// ============================================================================
// Anthropic Types
// ============================================================================

/// Anthropic Messages API request body
///
/// Reference: https://docs.anthropic.com/en/api/messages
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnthropicRequest {
    /// Model ID to use (e.g., "claude-3-opus-20240229")
    pub model: String,

    /// List of messages in the conversation
    pub messages: Vec<AnthropicMessage>,

    /// Maximum tokens to generate (required)
    pub max_tokens: u32,

    /// System prompt
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system: Option<AnthropicSystemPrompt>,

    /// Whether to stream the response
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream: Option<bool>,

    /// Sampling temperature (0.0 - 1.0)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,

    /// Top-p sampling parameter
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f64>,

    /// Top-k sampling parameter
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_k: Option<u32>,

    /// Stop sequences
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_sequences: Option<Vec<String>>,

    /// Metadata for the request
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,

    /// Tools available to the model
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<serde_json::Value>>,

    /// Tool choice preference
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<serde_json::Value>,
}

/// Anthropic system prompt (can be string or array with cache control)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AnthropicSystemPrompt {
    /// Simple text system prompt
    Text(String),
    /// Array of system prompt blocks (with cache control support)
    Blocks(Vec<AnthropicSystemBlock>),
}

impl AnthropicSystemPrompt {
    /// Extract system prompt as a single string
    pub fn as_text(&self) -> String {
        match self {
            AnthropicSystemPrompt::Text(s) => s.clone(),
            AnthropicSystemPrompt::Blocks(blocks) => blocks
                .iter()
                .filter_map(|b| b.text.as_ref())
                .cloned()
                .collect::<Vec<_>>()
                .join("\n"),
        }
    }
}

/// Anthropic system prompt block
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnthropicSystemBlock {
    /// Block type (usually "text")
    #[serde(rename = "type")]
    pub type_: String,

    /// Text content
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,

    /// Cache control settings
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_control: Option<serde_json::Value>,
}

/// Anthropic Messages API response body
///
/// Reference: https://docs.anthropic.com/en/api/messages
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnthropicResponse {
    /// Unique message ID
    pub id: String,

    /// Object type (always "message")
    #[serde(rename = "type")]
    pub type_: String,

    /// Role of the responder (always "assistant")
    pub role: MessageRole,

    /// Content blocks in the response
    pub content: Vec<AnthropicContentBlock>,

    /// Model used for the response
    pub model: String,

    /// Reason for stopping (end_turn, max_tokens, stop_sequence, tool_use)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<String>,

    /// Stop sequence that triggered the stop (if applicable)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_sequence: Option<String>,

    /// Token usage statistics
    pub usage: AnthropicUsage,
}

/// Anthropic message structure
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnthropicMessage {
    /// Role of the message sender
    pub role: MessageRole,

    /// Message content (can be string or array of content blocks)
    pub content: AnthropicMessageContent,
}

/// Anthropic message content (can be string or array of content blocks)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AnthropicMessageContent {
    /// Simple text content
    Text(String),
    /// Array of content blocks
    Blocks(Vec<AnthropicContentBlock>),
}

impl AnthropicMessageContent {
    /// Extract text content as a single string
    pub fn as_text(&self) -> String {
        match self {
            AnthropicMessageContent::Text(s) => s.clone(),
            AnthropicMessageContent::Blocks(blocks) => blocks
                .iter()
                .filter_map(|b| match b {
                    AnthropicContentBlock::Text { text, .. } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join(""),
        }
    }
}

/// Anthropic content block in response
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum AnthropicContentBlock {
    /// Text content block
    #[serde(rename = "text")]
    Text {
        /// The text content
        text: String,
        /// Cache control (if present)
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cache_control: Option<serde_json::Value>,
    },
    /// Thinking content block (extended thinking / chain-of-thought)
    #[serde(rename = "thinking")]
    Thinking {
        /// The thinking/reasoning content
        #[serde(default)]
        thinking: String,
        /// Cryptographic signature for thinking verification
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signature: Option<String>,
    },
    /// Image content block
    #[serde(rename = "image")]
    Image {
        /// Image source
        source: AnthropicImageSource,
    },
    /// Tool use content block
    #[serde(rename = "tool_use")]
    ToolUse {
        /// Tool use ID
        id: String,
        /// Tool name
        name: String,
        /// Tool input
        input: serde_json::Value,
    },
    /// Tool result content block
    #[serde(rename = "tool_result")]
    ToolResult {
        /// Tool use ID this result is for
        tool_use_id: String,
        /// Tool result content
        #[serde(default, skip_serializing_if = "Option::is_none")]
        content: Option<serde_json::Value>,
        /// Whether the tool execution errored
        #[serde(default, skip_serializing_if = "Option::is_none")]
        is_error: Option<bool>,
    },
}

/// Anthropic image source
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnthropicImageSource {
    /// Source type (base64 or url)
    #[serde(rename = "type")]
    pub type_: String,

    /// Media type (e.g., "image/jpeg")
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_type: Option<String>,

    /// Base64 encoded data (for base64 type)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<String>,

    /// URL (for url type)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

/// Anthropic token usage statistics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnthropicUsage {
    /// Number of input tokens
    pub input_tokens: u64,

    /// Number of output tokens
    pub output_tokens: u64,

    /// Input tokens used to create cache entries
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_creation_input_tokens: Option<u64>,

    /// Input tokens read from cache
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read_input_tokens: Option<u64>,
}

impl AnthropicUsage {
    /// Total tokens (input + output)
    pub fn total_tokens(&self) -> u64 {
        self.input_tokens + self.output_tokens
    }
}

// ============================================================================
// OpenAI SSE Streaming Types
// ============================================================================

/// OpenAI SSE streaming chunk (delta format)
///
/// Each SSE event in an OpenAI streaming response contains a chunk
/// with delta updates instead of complete messages.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAiSseChunk {
    /// Unique completion ID
    pub id: String,
    /// Object type (usually "chat.completion.chunk")
    pub object: String,
    /// Unix timestamp of creation
    pub created: u64,
    /// Model used for the completion
    pub model: String,
    /// List of chunk choices with deltas
    pub choices: Vec<OpenAiSseChoice>,
    /// System fingerprint
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_fingerprint: Option<String>,
}

/// OpenAI SSE streaming choice (with delta)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAiSseChoice {
    /// Index of the choice
    pub index: u32,
    /// Delta content (incremental update)
    pub delta: OpenAiSseDelta,
    /// Reason for finishing (stop, length, tool_calls, etc.)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finish_reason: Option<String>,
    /// Log probabilities (if requested)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logprobs: Option<serde_json::Value>,
}

/// OpenAI SSE delta content
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenAiSseDelta {
    /// Role (usually only in first chunk)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<MessageRole>,
    /// Content delta (incremental text)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    /// Reasoning content delta (for models like Qwen with reasoning)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
    /// Refusal delta (when model refuses to answer)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refusal: Option<String>,
    /// Function call (deprecated)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub function_call: Option<serde_json::Value>,
    /// Tool calls
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<serde_json::Value>>,
}

// ============================================================================
// Anthropic SSE Streaming Types
// ============================================================================

/// Anthropic SSE event types
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
#[serde(rename_all = "snake_case")]
pub enum AnthropicSseEvent {
    /// Message start event (contains initial message metadata)
    MessageStart { message: AnthropicSseMessageStart },
    /// Content block start event
    ContentBlockStart {
        index: u32,
        content_block: AnthropicContentBlock,
    },
    /// Content block delta event (incremental content)
    ContentBlockDelta {
        index: u32,
        delta: AnthropicSseDelta,
    },
    /// Content block stop event
    ContentBlockStop { index: u32 },
    /// Message delta event (stop_reason, usage update)
    MessageDelta {
        delta: AnthropicSseMessageDelta,
        usage: Option<AnthropicSseUsageDelta>,
    },
    /// Message stop event
    MessageStop,
    /// Ping event
    Ping,
    /// Error event
    Error { error: serde_json::Value },
}

/// Anthropic SSE message start data
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnthropicSseMessageStart {
    /// Message ID
    pub id: String,
    /// Message type (always "message")
    #[serde(rename = "type")]
    pub type_: String,
    /// Role (always "assistant")
    pub role: MessageRole,
    /// Model used
    pub model: String,
    /// Initial usage statistics
    pub usage: AnthropicUsage,
    /// Content blocks (empty at start)
    #[serde(default)]
    pub content: Vec<AnthropicContentBlock>,
    /// Stop reason
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<String>,
    /// Stop sequence
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_sequence: Option<String>,
}

/// Anthropic SSE content delta
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
#[serde(rename_all = "snake_case")]
pub enum AnthropicSseDelta {
    /// Text delta
    TextDelta { text: String },
    /// Thinking delta (extended thinking / chain-of-thought)
    ThinkingDelta { thinking: String },
    /// Signature delta (thinking block signature)
    SignatureDelta {
        #[serde(default)]
        signature: String,
    },
    /// Input JSON delta (for tool use)
    InputJsonDelta { partial_json: String },
}

/// Anthropic SSE message delta
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnthropicSseMessageDelta {
    /// Stop reason
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<String>,
    /// Stop sequence
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_sequence: Option<String>,
}

/// Anthropic SSE usage delta
///
/// Official Anthropic sends only `output_tokens` here and reports
/// `input_tokens` plus the cache counters in `message_start`. Some
/// Anthropic-compatible proxies invert this and put the full terminal usage in
/// the delta instead, so every counter is optional and callers must merge the
/// delta with `message_start` rather than trusting either alone.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnthropicSseUsageDelta {
    /// Output tokens
    pub output_tokens: u64,
    /// Input tokens, when the provider reports them in the delta
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u64>,
    /// Input tokens used to create cache entries
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_creation_input_tokens: Option<u64>,
    /// Input tokens read from cache
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read_input_tokens: Option<u64>,
}

// ============================================================================
// Unified Parsed Message
// ============================================================================

/// Unified parsed API message for both providers
///
/// This enum wraps parsed request/response pairs from different LLM providers
/// into a single type for unified handling.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ParsedApiMessage {
    /// OpenAI Chat Completions API message
    OpenAICompletion {
        /// Parsed request body (if available)
        request: Option<OpenAIRequest>,
        /// Parsed response body (if available)
        response: Option<OpenAIResponse>,
    },
    /// Anthropic Messages API message
    AnthropicMessage {
        /// Parsed request body (if available)
        request: Option<AnthropicRequest>,
        /// Parsed response body (if available)
        response: Option<AnthropicResponse>,
    },
    /// Aliyun SysOM Copilot API message (AK/SK auth mode)
    SysomMessage {
        /// Parsed request body (decoded from llmParamString)
        request: Option<super::sysom::SysomRequest>,
        /// Parsed response body
        response: Option<super::sysom::SysomResponse>,
    },
}

impl ParsedApiMessage {
    /// Get the model name from the parsed message
    pub fn model(&self) -> Option<&str> {
        match self {
            ParsedApiMessage::OpenAICompletion { request, response } => response
                .as_ref()
                .map(|r| r.model.as_str())
                .or_else(|| request.as_ref().map(|r| r.model.as_str())),
            ParsedApiMessage::AnthropicMessage { request, response } => response
                .as_ref()
                .map(|r| r.model.as_str())
                .or_else(|| request.as_ref().map(|r| r.model.as_str())),
            ParsedApiMessage::SysomMessage { request, .. } => {
                request.as_ref().map(|r| r.params.model.as_str())
            }
        }
    }

    /// Get the provider name
    pub fn provider(&self) -> &'static str {
        match self {
            ParsedApiMessage::OpenAICompletion { .. } => "openai",
            ParsedApiMessage::AnthropicMessage { .. } => "anthropic",
            ParsedApiMessage::SysomMessage { .. } => "sysom",
        }
    }

    /// Get the LLM response ID (e.g., "chatcmpl-xxx" for OpenAI, "msg_xxx" for Anthropic)
    pub fn response_id(&self) -> Option<&str> {
        match self {
            ParsedApiMessage::OpenAICompletion { response, .. } => {
                response.as_ref().map(|r| r.id.as_str())
            }
            ParsedApiMessage::AnthropicMessage { response, .. } => {
                response.as_ref().map(|r| r.id.as_str())
            }
            ParsedApiMessage::SysomMessage { response, .. } => {
                response.as_ref().and_then(|r| r.id.as_deref())
            }
        }
    }

    /// Extract session_id from request metadata (Anthropic only).
    ///
    /// Priority:
    /// 1. `metadata.session_id` or `metadata.sessionId` (direct string)
    /// 2. `metadata.user_id` — JSON-encoded object with `session_id`, or `_session_<UUID>` pattern
    pub fn request_metadata_session_id(&self) -> Option<String> {
        match self {
            ParsedApiMessage::AnthropicMessage { request, .. } => {
                let meta = request.as_ref()?.metadata.as_ref()?;
                session_id_from_metadata(meta)
            }
            _ => None,
        }
    }

    /// Check if streaming was requested
    pub fn is_streaming(&self) -> Option<bool> {
        match self {
            ParsedApiMessage::OpenAICompletion { request, .. } => {
                request.as_ref().and_then(|r| r.stream)
            }
            ParsedApiMessage::AnthropicMessage { request, .. } => {
                request.as_ref().and_then(|r| r.stream)
            }
            ParsedApiMessage::SysomMessage { request, .. } => {
                request.as_ref().map(|r| r.params.stream)
            }
        }
    }
}

/// Extract session_id from a metadata JSON value (Anthropic format).
///
/// Priority:
/// 1. `metadata.session_id` or `metadata.sessionId` (direct string)
/// 2. `metadata.user_id` — JSON-encoded object with `session_id`, or `_session_<UUID>` pattern
pub(crate) fn session_id_from_metadata(meta: &serde_json::Value) -> Option<String> {
    // 1. Direct session_id / sessionId
    if let Some(sid) = meta.get("session_id").and_then(|v| v.as_str()) {
        if !sid.is_empty() {
            return Some(sid.to_string());
        }
    }
    if let Some(sid) = meta.get("sessionId").and_then(|v| v.as_str()) {
        if !sid.is_empty() {
            return Some(sid.to_string());
        }
    }
    // 2. user_id field — try JSON decode, then substring pattern
    if let Some(user_id) = meta.get("user_id").and_then(|v| v.as_str()) {
        // 2a. JSON-encoded object: {"session_id": "..."}
        if let Ok(obj) = serde_json::from_str::<serde_json::Value>(user_id) {
            if let Some(sid) = obj.get("session_id").and_then(|v| v.as_str()) {
                if !sid.is_empty() {
                    return Some(sid.to_string());
                }
            }
        }
        // 2b. Pattern: ..._session_<UUID>
        if let Some(pos) = user_id.find("_session_") {
            let after = &user_id[pos + "_session_".len()..];
            if !after.is_empty() {
                return Some(after.to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_message_role_serde() {
        let role = MessageRole::User;
        let json = serde_json::to_string(&role).unwrap();
        assert_eq!(json, "\"user\"");

        let parsed: MessageRole = serde_json::from_str("\"assistant\"").unwrap();
        assert_eq!(parsed, MessageRole::Assistant);
    }

    #[test]
    fn test_openai_content_as_text() {
        let text_content = OpenAIContent::Text("Hello".to_string());
        assert_eq!(text_content.as_text(), "Hello");

        let parts_content = OpenAIContent::Parts(vec![
            OpenAIContentPart::Text {
                text: "Hello ".to_string(),
            },
            OpenAIContentPart::Text {
                text: "World".to_string(),
            },
        ]);
        assert_eq!(parts_content.as_text(), "Hello World");
    }

    #[test]
    fn test_anthropic_usage_total() {
        let usage = AnthropicUsage {
            input_tokens: 100,
            output_tokens: 50,
            cache_creation_input_tokens: None,
            cache_read_input_tokens: None,
        };
        assert_eq!(usage.total_tokens(), 150);
    }

    #[test]
    fn test_parsed_api_message_provider() {
        let openai_msg = ParsedApiMessage::OpenAICompletion {
            request: None,
            response: None,
        };
        assert_eq!(openai_msg.provider(), "openai");

        let anthropic_msg = ParsedApiMessage::AnthropicMessage {
            request: None,
            response: None,
        };
        assert_eq!(anthropic_msg.provider(), "anthropic");
    }

    #[test]
    fn test_openai_request_serde_roundtrip() {
        let json_str = r#"{
            "model": "gpt-4",
            "messages": [{"role": "user", "content": "Hi"}],
            "temperature": 0.7,
            "max_tokens": 1024,
            "stream": true
        }"#;
        let req: OpenAIRequest = serde_json::from_str(json_str).unwrap();
        assert_eq!(req.model, "gpt-4");
        assert_eq!(req.messages.len(), 1);
        assert_eq!(req.temperature, Some(0.7));
        assert_eq!(req.max_tokens, Some(1024));
        assert_eq!(req.stream, Some(true));
        // roundtrip
        let serialized = serde_json::to_string(&req).unwrap();
        let req2: OpenAIRequest = serde_json::from_str(&serialized).unwrap();
        assert_eq!(req2.model, "gpt-4");
    }

    #[test]
    fn test_openai_response_serde() {
        let json_str = r#"{
            "id": "chatcmpl-abc",
            "object": "chat.completion",
            "created": 1700000000,
            "model": "gpt-4",
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": "Hello!"},
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 5,
                "total_tokens": 15
            }
        }"#;
        let resp: OpenAIResponse = serde_json::from_str(json_str).unwrap();
        assert_eq!(resp.id, "chatcmpl-abc");
        assert_eq!(resp.choices.len(), 1);
        assert_eq!(resp.choices[0].finish_reason.as_deref(), Some("stop"));
        assert_eq!(resp.usage.as_ref().unwrap().total_tokens, 15);
    }

    #[test]
    fn test_openai_content_parts_with_image() {
        let parts = OpenAIContent::Parts(vec![
            OpenAIContentPart::Text {
                text: "Look at this:".to_string(),
            },
            OpenAIContentPart::ImageUrl {
                image_url: OpenAIImageUrl {
                    url: "https://example.com/img.png".to_string(),
                    detail: Some("high".to_string()),
                },
            },
        ]);
        // as_text should only extract text parts
        assert_eq!(parts.as_text(), "Look at this:");
    }

    #[test]
    fn test_anthropic_request_serde() {
        let json_str = r#"{
            "model": "claude-3-opus-20240229",
            "messages": [{"role": "user", "content": "Hello"}],
            "max_tokens": 4096,
            "system": "You are helpful.",
            "temperature": 0.5
        }"#;
        let req: AnthropicRequest = serde_json::from_str(json_str).unwrap();
        assert_eq!(req.model, "claude-3-opus-20240229");
        assert_eq!(req.max_tokens, 4096);
        assert_eq!(req.temperature, Some(0.5));
        match req.system.unwrap() {
            AnthropicSystemPrompt::Text(s) => assert_eq!(s, "You are helpful."),
            _ => panic!("expected text system prompt"),
        }
    }

    #[test]
    fn test_anthropic_system_prompt_blocks() {
        let json_str =
            r#"[{"type": "text", "text": "Part 1"}, {"type": "text", "text": "Part 2"}]"#;
        let blocks: Vec<AnthropicSystemBlock> = serde_json::from_str(json_str).unwrap();
        let prompt = AnthropicSystemPrompt::Blocks(blocks);
        assert_eq!(prompt.as_text(), "Part 1\nPart 2");
    }

    #[test]
    fn test_anthropic_response_serde() {
        let json_str = r#"{
            "id": "msg_123",
            "type": "message",
            "role": "assistant",
            "content": [{"type": "text", "text": "Hello!"}],
            "model": "claude-3-opus-20240229",
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 20, "output_tokens": 10}
        }"#;
        let resp: AnthropicResponse = serde_json::from_str(json_str).unwrap();
        assert_eq!(resp.id, "msg_123");
        assert_eq!(resp.stop_reason.as_deref(), Some("end_turn"));
        assert_eq!(resp.usage.input_tokens, 20);
        assert_eq!(resp.usage.total_tokens(), 30);
    }

    #[test]
    fn test_anthropic_content_blocks() {
        let json_str = r#"[
            {"type": "text", "text": "Let me search."},
            {"type": "tool_use", "id": "tu_1", "name": "search", "input": {"q": "rust"}}
        ]"#;
        let blocks: Vec<AnthropicContentBlock> = serde_json::from_str(json_str).unwrap();
        assert_eq!(blocks.len(), 2);
    }

    #[test]
    fn test_anthropic_message_content_as_text() {
        let text = AnthropicMessageContent::Text("simple".to_string());
        assert_eq!(text.as_text(), "simple");

        let blocks = AnthropicMessageContent::Blocks(vec![
            AnthropicContentBlock::Text {
                text: "part1".to_string(),
                cache_control: None,
            },
            AnthropicContentBlock::Text {
                text: "part2".to_string(),
                cache_control: None,
            },
        ]);
        assert_eq!(blocks.as_text(), "part1part2");
    }

    #[test]
    fn test_openai_sse_chunk_serde() {
        let json_str = r#"{
            "id": "chatcmpl-chunk",
            "object": "chat.completion.chunk",
            "created": 1700000000,
            "model": "gpt-4",
            "choices": [{
                "index": 0,
                "delta": {"content": "Hello"},
                "finish_reason": null
            }]
        }"#;
        let chunk: OpenAiSseChunk = serde_json::from_str(json_str).unwrap();
        assert_eq!(chunk.id, "chatcmpl-chunk");
        assert_eq!(chunk.choices[0].delta.content.as_deref(), Some("Hello"));
        assert!(chunk.choices[0].finish_reason.is_none());
    }

    #[test]
    fn test_anthropic_sse_event_message_start() {
        let json_str = r#"{
            "type": "message_start",
            "message": {
                "id": "msg_1",
                "type": "message",
                "role": "assistant",
                "model": "claude-3-opus",
                "usage": {"input_tokens": 25, "output_tokens": 1},
                "content": []
            }
        }"#;
        let event: AnthropicSseEvent = serde_json::from_str(json_str).unwrap();
        match event {
            AnthropicSseEvent::MessageStart { message } => {
                assert_eq!(message.id, "msg_1");
                assert_eq!(message.usage.input_tokens, 25);
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn test_anthropic_sse_delta_text() {
        let json_str = r#"{"type": "text_delta", "text": "world"}"#;
        let delta: AnthropicSseDelta = serde_json::from_str(json_str).unwrap();
        match delta {
            AnthropicSseDelta::TextDelta { text } => assert_eq!(text, "world"),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn test_parsed_api_message_model() {
        let msg = ParsedApiMessage::OpenAICompletion {
            request: Some(OpenAIRequest {
                model: "gpt-4".to_string(),
                messages: vec![],
                temperature: None,
                max_tokens: None,
                stream: None,
                top_p: None,
                n: None,
                stop: None,
                presence_penalty: None,
                frequency_penalty: None,
                user: None,
                tools: None,
                tool_choice: None,
                response_format: None,
                seed: None,
                logprobs: None,
                top_logprobs: None,
                parallel_tool_calls: None,
            }),
            response: None,
        };
        assert_eq!(msg.model(), Some("gpt-4"));
        assert_eq!(msg.is_streaming(), None);
    }

    #[test]
    fn test_parsed_api_message_response_id() {
        let msg = ParsedApiMessage::OpenAICompletion {
            request: None,
            response: Some(OpenAIResponse {
                id: "chatcmpl-xyz".to_string(),
                object: "chat.completion".to_string(),
                created: 0,
                model: "gpt-4".to_string(),
                choices: vec![],
                usage: None,
                system_fingerprint: None,
            }),
        };
        assert_eq!(msg.response_id(), Some("chatcmpl-xyz"));
        assert_eq!(msg.model(), Some("gpt-4"));
    }

    #[test]
    fn test_message_role_default() {
        let role = MessageRole::default();
        assert_eq!(role, MessageRole::User);
    }

    #[test]
    fn test_openai_chat_message_with_tool_calls() {
        let json_str = r#"{
            "role": "assistant",
            "content": null,
            "tool_calls": [{"id": "tc_1", "type": "function", "function": {"name": "get_weather", "arguments": "{}"}}]
        }"#;
        let msg: OpenAIChatMessage = serde_json::from_str(json_str).unwrap();
        assert!(msg.content.is_none());
        assert_eq!(msg.tool_calls.as_ref().unwrap().len(), 1);
    }

    #[test]
    fn test_request_metadata_session_id_direct() {
        // Anthropic metadata.session_id 直接取
        let msg = ParsedApiMessage::AnthropicMessage {
            request: Some(AnthropicRequest {
                model: "claude-3".to_string(),
                messages: vec![],
                max_tokens: 4096,
                system: None,
                stream: None,
                temperature: None,
                top_p: None,
                top_k: None,
                stop_sequences: None,
                tools: None,
                tool_choice: None,
                metadata: Some(serde_json::json!({"session_id": "abc-123"})),
            }),
            response: None,
        };
        assert_eq!(
            msg.request_metadata_session_id(),
            Some("abc-123".to_string())
        );
    }

    #[test]
    fn test_request_metadata_session_id_user_id_json() {
        // metadata.user_id 是 JSON 编码的对象
        let msg = ParsedApiMessage::AnthropicMessage {
            request: Some(AnthropicRequest {
                model: "claude-3".to_string(),
                messages: vec![],
                max_tokens: 4096,
                system: None,
                stream: None,
                temperature: None,
                top_p: None,
                top_k: None,
                stop_sequences: None,
                tools: None,
                tool_choice: None,
                metadata: Some(serde_json::json!({"user_id": "{\"session_id\":\"sess-456\"}"})),
            }),
            response: None,
        };
        assert_eq!(
            msg.request_metadata_session_id(),
            Some("sess-456".to_string())
        );
    }

    #[test]
    fn test_request_metadata_session_id_user_id_pattern() {
        // metadata.user_id 是 ..._session_<UUID> 格式
        let msg = ParsedApiMessage::AnthropicMessage {
            request: Some(AnthropicRequest {
                model: "claude-3".to_string(),
                messages: vec![],
                max_tokens: 4096,
                system: None,
                stream: None,
                temperature: None,
                top_p: None,
                top_k: None,
                stop_sequences: None,
                tools: None,
                tool_choice: None,
                metadata: Some(serde_json::json!({"user_id": "proj_abc_session_uuid-789-xyz"})),
            }),
            response: None,
        };
        assert_eq!(
            msg.request_metadata_session_id(),
            Some("uuid-789-xyz".to_string())
        );
    }

    #[test]
    fn test_request_metadata_session_id_none_fallback() {
        // 无 metadata 时返回 None（反向：不误报）
        let msg = ParsedApiMessage::AnthropicMessage {
            request: Some(AnthropicRequest {
                model: "claude-3".to_string(),
                messages: vec![],
                max_tokens: 4096,
                system: None,
                stream: None,
                temperature: None,
                top_p: None,
                top_k: None,
                stop_sequences: None,
                tools: None,
                tool_choice: None,
                metadata: None,
            }),
            response: None,
        };
        assert_eq!(msg.request_metadata_session_id(), None);
    }

    #[test]
    fn test_request_metadata_session_id_openai_returns_none() {
        // OpenAI 不走 metadata 抽取，始终返回 None
        let msg = ParsedApiMessage::OpenAICompletion {
            request: Some(OpenAIRequest {
                model: "gpt-4".to_string(),
                messages: vec![],
                max_tokens: None,
                temperature: None,
                top_p: None,
                frequency_penalty: None,
                presence_penalty: None,
                n: None,
                stream: None,
                stop: None,
                user: Some("user-session-abc".to_string()),
                tools: None,
                tool_choice: None,
                response_format: None,
                seed: None,
                logprobs: None,
                top_logprobs: None,
                parallel_tool_calls: None,
            }),
            response: None,
        };
        assert_eq!(msg.request_metadata_session_id(), None);
    }
}
