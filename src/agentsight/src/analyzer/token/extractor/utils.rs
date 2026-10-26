//! Utility functions for token data extraction

use super::Provider;
use serde_json::Value;

/// Detect provider from API endpoint path and/or JSON content
///
/// For standard paths like `/v1/chat/completions` or `/v1/messages`,
/// the provider is determined by the path itself.
///
/// For compatible mode paths like `/compatible-mode/v1/chat/completions`,
/// the provider is detected from the JSON content structure.
///
/// # Arguments
/// * `path` - The API endpoint path
/// * `request_json` - Optional request body for content-based detection
pub fn detect_provider(path: &str, request_json: Option<&Value>) -> Option<Provider> {
    // First, try to detect from path
    if let Some(provider) = detect_provider_from_path(path) {
        return Some(provider);
    }

    // For compatible mode or unknown paths, try to detect from JSON content
    if let Some(json) = request_json {
        return detect_provider_from_json(json);
    }

    None
}

/// Detect provider from API endpoint path only
///
/// This function only checks the URL path and does not inspect JSON content.
/// Returns `None` for compatible mode paths that require JSON content detection.
pub fn detect_provider_from_path(path: &str) -> Option<Provider> {
    // Check for compatible mode paths first - these need JSON content detection
    if path.contains("/compatible-mode") || path.contains("/compat-mode") {
        return None;
    }

    if path.contains("/v1/messages") && !path.contains("chat") {
        Some(Provider::Anthropic)
    } else if path.contains("/v1/chat/completions") || path.contains("/v1/completions") {
        Some(Provider::OpenAI)
    } else {
        None
    }
}

/// Whether a message carries a content block only the Anthropic schema has.
///
/// `tool_result`, `tool_use`, `thinking` and `redacted_thinking` are Anthropic
/// Messages block types, and so are the provider-side spellings the message
/// parser aliases onto them (`server_tool_use`, `mcp_tool_use`,
/// `web_search_tool_result`, `web_fetch_tool_result`, the code-execution
/// results, `tool_search_tool_result`, `mcp_tool_result`); the
/// OpenAI-compatible schema spells the same information with `tool_calls`,
/// `tool_call_id` and plain `text` / `image_url` parts, so one of these blocks
/// is proof of the protocol no matter which path carried it.
fn has_anthropic_block(message: &Value) -> bool {
    message
        .get("content")
        .and_then(|c| c.as_array())
        .is_some_and(|blocks| {
            blocks.iter().any(|block| {
                matches!(
                    block.get("type").and_then(|t| t.as_str()),
                    Some(
                        "tool_result"
                            | "tool_use"
                            | "thinking"
                            | "redacted_thinking"
                            | "server_tool_use"
                            | "mcp_tool_use"
                            | "web_search_tool_result"
                            | "web_fetch_tool_result"
                            | "code_execution_tool_result"
                            | "bash_code_execution_tool_result"
                            | "text_editor_code_execution_tool_result"
                            | "tool_search_tool_result"
                            | "mcp_tool_result"
                    )
                )
            })
        })
}

/// Detect provider from JSON content structure
///
/// This function analyzes the JSON content to determine the protocol format:
/// - Anthropic: Has "system" as a top-level field, or a message carrying a
///   content block only the Anthropic schema has (`tool_result`, `tool_use`,
///   `thinking`, `redacted_thinking`)
/// - OpenAI: Has "response_format", "seed" or "parallel_tool_calls"
/// - Otherwise a message's "name" or "tool_calls" identifies OpenAI, and an
///   undecided body defaults to OpenAI because most compatible endpoints
///   follow that format
///
/// # Arguments
/// * `json` - The request JSON body
pub fn detect_provider_from_json(json: &Value) -> Option<Provider> {
    // Check for Anthropic-specific fields
    // Anthropic has "system" as a top-level field (not inside messages)
    // Note: "max_tokens" alone is not unique to Anthropic (OpenAI also has it)
    if json.get("system").is_some() {
        return Some(Provider::Anthropic);
    }

    // Check for OpenAI-specific fields
    // OpenAI has "response_format", "seed", "parallel_tool_calls"
    if json.get("response_format").is_some()
        || json.get("seed").is_some()
        || json.get("parallel_tool_calls").is_some()
    {
        return Some(Provider::OpenAI);
    }

    // An Anthropic request does not have to carry a top-level `system`: the
    // field is optional, and a replayed agentic conversation usually omits it.
    // On a compatible-mode path the path check bows out, so without this arm
    // such a request fell through to the OpenAI default and the Anthropic
    // extractor — the one that reads `tool_result`, `tool_use` and `thinking`
    // blocks — never ran. The OpenAI arm knows only `text` parts, so the whole
    // tool round vanished from the extracted token data.
    //
    // The block type names are the protocol's, not the provider's: OpenAI
    // spells its calls `tool_calls` with `text` / `input_text` / `image_url`
    // parts, so these three can only come from the Anthropic schema.
    if json
        .get("messages")
        .and_then(|m| m.as_array())
        .is_some_and(|messages| messages.iter().any(has_anthropic_block))
    {
        return Some(Provider::Anthropic);
    }

    // Check messages structure
    if let Some(messages) = json.get("messages").and_then(|m| m.as_array()) {
        // Look at the first message to determine format
        if let Some(first_msg) = messages.first() {
            // OpenAI messages often have "name" in function/tool contexts
            if first_msg.get("name").is_some() {
                return Some(Provider::OpenAI);
            }

            // Check for tool_calls which is OpenAI-specific
            if first_msg.get("tool_calls").is_some() {
                return Some(Provider::OpenAI);
            }
        }
    }

    // Default to OpenAI for compatible mode if we can't determine
    // This is because most compatible endpoints follow OpenAI format
    Some(Provider::OpenAI)
}

/// Extract model name from JSON (check request first, then response)
pub fn extract_model_from_json(
    request: Option<&Value>,
    response: Option<&Value>,
) -> Option<String> {
    request
        .and_then(|r| r.get("model").and_then(|m| m.as_str()))
        .or_else(|| response.and_then(|r| r.get("model").and_then(|m| m.as_str())))
        .map(|s| s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_openai_path() {
        assert_eq!(
            detect_provider_from_path("/v1/chat/completions"),
            Some(Provider::OpenAI)
        );
        assert_eq!(
            detect_provider_from_path("/v1/completions"),
            Some(Provider::OpenAI)
        );
    }

    #[test]
    fn test_detect_anthropic_path() {
        assert_eq!(
            detect_provider_from_path("/v1/messages"),
            Some(Provider::Anthropic)
        );
    }

    #[test]
    fn test_detect_unknown_path() {
        assert_eq!(detect_provider_from_path("/v1/embeddings"), None);
        assert_eq!(detect_provider_from_path("/v1/models"), None);
    }

    #[test]
    fn test_extract_model_from_request() {
        let request = serde_json::json!({"model": "gpt-4"});
        let model = extract_model_from_json(Some(&request), None);
        assert_eq!(model, Some("gpt-4".to_string()));
    }

    #[test]
    fn test_extract_model_from_response() {
        let response = serde_json::json!({"model": "claude-3-opus"});
        let model = extract_model_from_json(None, Some(&response));
        assert_eq!(model, Some("claude-3-opus".to_string()));
    }

    #[test]
    fn test_extract_model_request_priority() {
        let request = serde_json::json!({"model": "gpt-4"});
        let response = serde_json::json!({"model": "gpt-3.5"});
        let model = extract_model_from_json(Some(&request), Some(&response));
        assert_eq!(model, Some("gpt-4".to_string()));
    }

    #[test]
    fn test_detect_provider_with_compatible_mode() {
        // Compatible mode path with OpenAI format JSON
        let openai_request = serde_json::json!({
            "model": "gpt-4",
            "messages": [{"role": "user", "content": "Hello"}]
        });
        let provider = detect_provider(
            "/compatible-mode/v1/chat/completions",
            Some(&openai_request),
        );
        assert_eq!(provider, Some(Provider::OpenAI));

        // Compatible mode path with Anthropic format JSON
        let anthropic_request = serde_json::json!({
            "model": "claude-3-opus",
            "max_tokens": 1024,
            "system": "You are Claude",
            "messages": [{"role": "user", "content": "Hello"}]
        });
        let provider = detect_provider(
            "/compatible-mode/v1/chat/completions",
            Some(&anthropic_request),
        );
        assert_eq!(provider, Some(Provider::Anthropic));
    }

    #[test]
    fn test_detect_provider_from_json_openai() {
        let request = serde_json::json!({
            "model": "gpt-4",
            "response_format": {"type": "json_object"},
            "messages": [{"role": "user", "content": "Hello"}]
        });
        let provider = detect_provider_from_json(&request);
        assert_eq!(provider, Some(Provider::OpenAI));
    }

    #[test]
    fn test_detect_provider_from_json_anthropic() {
        let request = serde_json::json!({
            "model": "claude-3-opus",
            "max_tokens": 1024,
            "system": "You are Claude",
            "messages": [{"role": "user", "content": "Hello"}]
        });
        let provider = detect_provider_from_json(&request);
        assert_eq!(provider, Some(Provider::Anthropic));
    }

    /// An Anthropic request does not have to carry a top-level `system`
    /// prompt: the field is optional and most single-turn and agentic calls
    /// omit it. On a compatible-mode path the path check bows out and the JSON
    /// check decides, where Anthropic is currently recognised only by
    /// `system` — so such a request fell through to the OpenAI default and the
    /// Anthropic extractor (the one that reads `tool_result`, `tool_use` and
    /// `thinking` blocks) never ran. Its content blocks were then read by the
    /// OpenAI arm, which knows only `text`, and the whole turn vanished.
    ///
    /// The block type names are the protocol's, not the provider's: OpenAI
    /// spells its calls `tool_calls` with `text`/`image_url` parts, so a
    /// `tool_result` block can only come from the Anthropic schema.
    #[test]
    fn test_detect_provider_from_json_anthropic_without_system() {
        let request = serde_json::json!({
            "model": "claude-3-7-sonnet",
            "max_tokens": 1024,
            "messages": [
                {"role": "user", "content": "list /tmp"},
                {"role": "assistant", "content": [
                    {"type": "tool_use", "id": "toolu_01", "name": "Bash",
                     "input": {"command": "ls /tmp"}}
                ]},
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "toolu_01", "content": "a.txt"}
                ]}
            ]
        });
        assert_eq!(
            detect_provider_from_json(&request),
            Some(Provider::Anthropic),
            "an Anthropic block in a system-less request still names the protocol"
        );

        // The same request through the whole detector: a compatible-mode path
        // bows out of the path check, so the JSON verdict is the one used.
        assert_eq!(
            detect_provider("/compatible-mode/v1/messages", Some(&request)),
            Some(Provider::Anthropic)
        );
    }

    /// The server-tool spellings are Anthropic block types too, and they are
    /// the ones a search-only turn is made of: the assistant turn holds
    /// `server_tool_use` and the user turn holds the matching
    /// `web_search_tool_result`. The block list here omitted them, so on a
    /// compatible-mode path such a request fell through to the OpenAI
    /// default; the OpenAI reader knows only `text` parts, so the whole
    /// search round — the call and the result the answer was built from —
    /// vanished from the extracted token data.
    #[test]
    fn test_detect_provider_from_json_server_tool_without_system() {
        let request = serde_json::json!({
            "model": "claude-sonnet-4-5",
            "max_tokens": 1024,
            "messages": [
                {"role": "user", "content": "search for rust 2026"},
                {"role": "assistant", "content": [
                    {"type": "server_tool_use", "id": "srvtoolu_1", "name": "web_search",
                     "input": {"query": "rust 2026"}}
                ]},
                {"role": "user", "content": [
                    {"type": "web_search_tool_result", "tool_use_id": "srvtoolu_1",
                     "content": [{"type": "web_search_result", "url": "https://example.com"}]}
                ]}
            ]
        });
        assert_eq!(
            detect_provider_from_json(&request),
            Some(Provider::Anthropic),
            "a server-tool block in a system-less request still names the protocol"
        );
        assert_eq!(
            detect_provider("/compatible-mode/v1/messages", Some(&request)),
            Some(Provider::Anthropic)
        );
    }

    /// Each server-tool spelling names the protocol on its own: a request
    /// whose only protocol-specific block is that one still detects as
    /// Anthropic through the whole detector.
    #[test]
    fn test_detect_provider_from_json_each_server_tool_block() {
        for block_type in [
            "server_tool_use",
            "mcp_tool_use",
            "web_search_tool_result",
            "web_fetch_tool_result",
            "code_execution_tool_result",
            "bash_code_execution_tool_result",
            "text_editor_code_execution_tool_result",
            "tool_search_tool_result",
            "mcp_tool_result",
        ] {
            let request = serde_json::json!({
                "model": "claude-sonnet-4-5",
                "max_tokens": 1024,
                "messages": [
                    {"role": "user", "content": "go on"},
                    {"role": "assistant", "content": [
                        {"type": block_type, "id": "srvtoolu_1"}
                    ]}
                ]
            });
            assert_eq!(
                detect_provider("/compatible-mode/v1/messages", Some(&request)),
                Some(Provider::Anthropic),
                "a lone {block_type} block names the protocol"
            );
        }
    }

    #[test]
    fn test_detect_provider_from_json_redacted_thinking_without_system() {
        // `redacted_thinking` is an Anthropic block type too. A request whose
        // only protocol-specific block is that one, and which carries no
        // top-level `system`, still names the Anthropic schema; without the
        // block in the arm such a request fell through to the OpenAI default.
        let request = serde_json::json!({
            "model": "claude-3-7-sonnet",
            "max_tokens": 1024,
            "messages": [
                {"role": "user", "content": "what did you conclude?"},
                {"role": "assistant", "content": [
                    {"type": "redacted_thinking", "data": "EmwKAhgBEgy3vdaL"},
                    {"type": "text", "text": "The answer is 42."}
                ]}
            ]
        });
        assert_eq!(
            detect_provider_from_json(&request),
            Some(Provider::Anthropic),
            "a redacted_thinking block in a system-less request names the protocol"
        );

        // Through the whole detector, on a path the path check bows out of.
        assert_eq!(
            detect_provider("/compatible-mode/v1/messages", Some(&request)),
            Some(Provider::Anthropic)
        );
    }

    #[test]
    fn test_detect_provider_from_path_takes_priority() {
        // Even with Anthropic-like JSON, standard OpenAI path should return OpenAI
        let request = serde_json::json!({
            "model": "claude-3-opus",
            "max_tokens": 1024,
            "system": "You are Claude",
            "messages": [{"role": "user", "content": "Hello"}]
        });
        let provider = detect_provider("/v1/chat/completions", Some(&request));
        assert_eq!(provider, Some(Provider::OpenAI));

        // Even with OpenAI-like JSON, standard Anthropic path should return Anthropic
        let request = serde_json::json!({
            "model": "gpt-4",
            "response_format": {"type": "json_object"},
            "messages": [{"role": "user", "content": "Hello"}]
        });
        let provider = detect_provider("/v1/messages", Some(&request));
        assert_eq!(provider, Some(Provider::Anthropic));
    }
}
