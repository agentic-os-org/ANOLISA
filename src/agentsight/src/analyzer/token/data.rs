//! Extracted token data types for local tokenization
//!
//! These types represent the actual text content extracted from LLM API
//! request/response bodies that would be counted as tokens.

use serde::{Deserialize, Serialize};

/// Extracted token data from request/response for local tokenization
///
/// This struct contains the actual text content that would be counted as tokens,
/// allowing for local token counting and analysis.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenData {
    /// Provider type (openai, anthropic, etc.)
    pub provider: String,
    /// Model name
    pub model: String,
    /// Request messages that contribute to input tokens
    pub request_messages: Vec<MessageTokenData>,
    /// System prompt (if present)
    pub system_prompt: Option<String>,
    /// Tools definitions (if present)
    pub tools: Vec<String>,
    /// Response content that contributes to output tokens
    pub response_content: Vec<ResponseTokenData>,
    /// Response reasoning content (if present, e.g., Qwen reasoning models)
    pub reasoning_content: Option<String>,
    /// Tool calls in response (if present)
    pub tool_calls: Vec<String>,
}

impl TokenData {
    /// Create a new TokenData instance
    pub fn new(provider: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            provider: provider.into(),
            model: model.into(),
            request_messages: Vec::new(),
            system_prompt: None,
            tools: Vec::new(),
            response_content: Vec::new(),
            reasoning_content: None,
            tool_calls: Vec::new(),
        }
    }

    /// Add a request message
    pub fn add_request_message(
        mut self,
        role: impl Into<String>,
        content: impl Into<String>,
    ) -> Self {
        self.request_messages.push(MessageTokenData {
            role: role.into(),
            content: content.into(),
        });
        self
    }

    /// Set system prompt
    pub fn with_system_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.system_prompt = Some(prompt.into());
        self
    }

    /// Add a tool definition
    pub fn add_tool(mut self, tool_json: impl Into<String>) -> Self {
        self.tools.push(tool_json.into());
        self
    }

    /// Add response content
    pub fn add_response_content(mut self, content: impl Into<String>) -> Self {
        self.response_content.push(ResponseTokenData {
            content: content.into(),
        });
        self
    }

    /// Set reasoning content
    pub fn with_reasoning_content(mut self, content: impl Into<String>) -> Self {
        self.reasoning_content = Some(content.into());
        self
    }

    /// Add a tool call
    pub fn add_tool_call(mut self, tool_call_json: impl Into<String>) -> Self {
        self.tool_calls.push(tool_call_json.into());
        self
    }

    /// Get all request text content combined
    pub fn request_text(&self) -> String {
        let mut parts = Vec::new();

        if let Some(ref system) = self.system_prompt {
            parts.push(format!("system: {system}"));
        }

        for msg in &self.request_messages {
            parts.push(format!("{}: {}", msg.role, msg.content));
        }

        for tool in &self.tools {
            parts.push(format!("tool: {tool}"));
        }

        parts.join("\n")
    }

    /// Get all response text content combined
    pub fn response_text(&self) -> String {
        let mut parts = Vec::new();

        if let Some(ref reasoning) = self.reasoning_content {
            parts.push(format!("reasoning: {reasoning}"));
        }

        for content in &self.response_content {
            parts.push(content.content.clone());
        }

        for tool_call in &self.tool_calls {
            parts.push(format!("tool_call: {tool_call}"));
        }

        parts.join("\n")
    }

    /// Get all text content (request + response)
    pub fn all_text(&self) -> String {
        format!("{}\n{}", self.request_text(), self.response_text())
    }

    /// Get messages grouped by role
    pub fn messages_by_role(&self) -> std::collections::HashMap<String, Vec<&MessageTokenData>> {
        let mut map: std::collections::HashMap<String, Vec<&MessageTokenData>> =
            std::collections::HashMap::new();

        for msg in &self.request_messages {
            map.entry(msg.role.clone()).or_default().push(msg);
        }

        map
    }

    /// Count messages by role
    pub fn count_by_role(&self) -> std::collections::HashMap<String, usize> {
        let mut counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();

        for msg in &self.request_messages {
            *counts.entry(msg.role.clone()).or_insert(0) += 1;
        }

        counts
    }

    /// Check if there are any messages
    pub fn has_messages(&self) -> bool {
        !self.request_messages.is_empty()
    }

    /// Get total character count (rough estimate for token calculation)
    pub fn total_chars(&self) -> usize {
        let mut total = 0;

        if let Some(ref system) = self.system_prompt {
            total += system.len();
        }

        for msg in &self.request_messages {
            total += msg.content.len();
        }

        for tool in &self.tools {
            total += tool.len();
        }

        for content in &self.response_content {
            total += content.content.len();
        }

        if let Some(ref reasoning) = self.reasoning_content {
            total += reasoning.len();
        }

        for tool_call in &self.tool_calls {
            total += tool_call.len();
        }

        total
    }
}

/// Message token data for request messages
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageTokenData {
    /// Message role (system, user, assistant, tool)
    pub role: String,
    /// Message content
    pub content: String,
}

/// Response token data
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseTokenData {
    /// Response content text
    pub content: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_token_data_builder() {
        let data = TokenData::new("openai", "gpt-4")
            .with_system_prompt("You are a helpful assistant")
            .add_request_message("user", "Hello")
            .add_response_content("Hi there!");

        assert_eq!(data.provider, "openai");
        assert_eq!(data.model, "gpt-4");
        assert_eq!(
            data.system_prompt,
            Some("You are a helpful assistant".to_string())
        );
        assert_eq!(data.request_messages.len(), 1);
        assert_eq!(data.response_content.len(), 1);
    }

    #[test]
    fn test_token_data_text_extraction() {
        let data = TokenData::new("openai", "gpt-4")
            .with_system_prompt("System prompt")
            .add_request_message("user", "Hello")
            .add_response_content("Hi!");

        let request_text = data.request_text();
        assert!(request_text.contains("System prompt"));
        assert!(request_text.contains("Hello"));

        let response_text = data.response_text();
        assert!(response_text.contains("Hi!"));
    }

    #[test]
    fn test_add_tool() {
        let data = TokenData::new("openai", "gpt-4").add_tool(r#"{"name":"search"}"#);
        assert_eq!(data.tools.len(), 1);
        assert!(data.request_text().contains("tool: {\"name\":\"search\"}"));
    }

    #[test]
    fn test_with_reasoning_content() {
        let data = TokenData::new("openai", "qwen").with_reasoning_content("Let me think...");
        assert_eq!(data.reasoning_content, Some("Let me think...".to_string()));
        assert!(data.response_text().contains("reasoning: Let me think..."));
    }

    #[test]
    fn test_add_tool_call() {
        let data =
            TokenData::new("openai", "gpt-4").add_tool_call(r#"get_weather({"city":"Beijing"})"#);
        assert_eq!(data.tool_calls.len(), 1);
        assert!(data.response_text().contains("tool_call: get_weather"));
    }

    #[test]
    fn test_all_text() {
        let data = TokenData::new("openai", "gpt-4")
            .with_system_prompt("sys")
            .add_request_message("user", "hi")
            .add_response_content("hello");
        let all = data.all_text();
        assert!(all.contains("sys"));
        assert!(all.contains("hi"));
        assert!(all.contains("hello"));
    }

    #[test]
    fn test_messages_by_role() {
        let data = TokenData::new("openai", "gpt-4")
            .add_request_message("user", "q1")
            .add_request_message("assistant", "a1")
            .add_request_message("user", "q2");
        let by_role = data.messages_by_role();
        assert_eq!(by_role.get("user").unwrap().len(), 2);
        assert_eq!(by_role.get("assistant").unwrap().len(), 1);
    }

    #[test]
    fn test_count_by_role() {
        let data = TokenData::new("openai", "gpt-4")
            .add_request_message("user", "q1")
            .add_request_message("assistant", "a1")
            .add_request_message("user", "q2");
        let counts = data.count_by_role();
        assert_eq!(counts["user"], 2);
        assert_eq!(counts["assistant"], 1);
    }

    #[test]
    fn test_has_messages() {
        let empty = TokenData::new("openai", "gpt-4");
        assert!(!empty.has_messages());
        let with_msg = empty.add_request_message("user", "hi");
        assert!(with_msg.has_messages());
    }

    #[test]
    fn test_total_chars() {
        let data = TokenData::new("openai", "gpt-4")
            .with_system_prompt("abc") // 3
            .add_request_message("user", "de") // 2
            .add_tool("fg") // 2
            .add_response_content("hij") // 3
            .with_reasoning_content("kl") // 2
            .add_tool_call("mno"); // 3
        assert_eq!(data.total_chars(), 15);
    }

    #[test]
    fn test_serde_roundtrip() {
        let data = TokenData::new("anthropic", "claude-3")
            .with_system_prompt("Be helpful")
            .add_request_message("user", "Hello")
            .add_response_content("Hi!")
            .with_reasoning_content("thinking")
            .add_tool_call("search({})");
        let json = serde_json::to_string(&data).unwrap();
        let back: TokenData = serde_json::from_str(&json).unwrap();
        assert_eq!(back.provider, "anthropic");
        assert_eq!(back.model, "claude-3");
        assert_eq!(back.system_prompt, Some("Be helpful".to_string()));
        assert_eq!(back.request_messages.len(), 1);
        assert_eq!(back.response_content.len(), 1);
        assert_eq!(back.reasoning_content, Some("thinking".to_string()));
        assert_eq!(back.tool_calls.len(), 1);
    }

    #[test]
    fn test_request_text_no_system() {
        let data = TokenData::new("openai", "gpt-4").add_request_message("user", "Hello");
        let text = data.request_text();
        assert!(!text.contains("system:"));
        assert!(text.contains("user: Hello"));
    }

    #[test]
    fn test_response_text_empty() {
        let data = TokenData::new("openai", "gpt-4");
        let text = data.response_text();
        assert!(text.is_empty());
    }
}
