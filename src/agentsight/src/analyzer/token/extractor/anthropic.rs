//! Anthropic token data extraction

use super::super::data::{MessageTokenData, ResponseTokenData, TokenData};
use super::utils::extract_model_from_json;
use serde_json::Value;

/// Extract token data from Anthropic format JSON
pub fn extract_token_data(
    request_json: Option<&Value>,
    response_json: Option<&Value>,
) -> Option<TokenData> {
    let model = extract_model_from_json(request_json, response_json)
        .unwrap_or_else(|| "unknown".to_string());

    let mut token_data = TokenData::new("anthropic", model);
    let mut has_content = false;

    // Extract from request
    if let Some(req) = request_json {
        // Extract system prompt
        if let Some(system) = req.get("system") {
            let system_text = match system {
                Value::String(s) => Some(s.clone()),
                Value::Array(blocks) => {
                    let text: String = blocks
                        .iter()
                        .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
                        .collect::<Vec<_>>()
                        .join("\n");
                    if text.is_empty() { None } else { Some(text) }
                }
                _ => None,
            };
            if let Some(text) = system_text {
                token_data.system_prompt = Some(text);
                has_content = true;
            }
        }

        // Extract messages
        if let Some(messages) = req.get("messages").and_then(|m| m.as_array()) {
            for msg in messages {
                if let Some((role, content)) = extract_message(msg) {
                    token_data
                        .request_messages
                        .push(MessageTokenData { role, content });
                    has_content = true;
                }
            }
        }

        // Extract tools
        if let Some(tools) = req.get("tools").and_then(|t| t.as_array()) {
            for tool in tools {
                if let Ok(tool_str) = serde_json::to_string(tool) {
                    token_data.tools.push(tool_str);
                    has_content = true;
                }
            }
        }
    }

    // Extract from response
    if let Some(resp) = response_json {
        // Extract content blocks
        if let Some(content) = resp.get("content").and_then(|c| c.as_array()) {
            let mut reasoning = String::new();
            for block in content {
                let block_type = block.get("type").and_then(|t| t.as_str());

                match block_type {
                    Some("text") => {
                        if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                            if !text.is_empty() {
                                token_data.response_content.push(ResponseTokenData {
                                    content: text.to_string(),
                                });
                                has_content = true;
                            }
                        }
                    }
                    // Extended thinking arrives as a `thinking` block whose
                    // text sits in the `thinking` field (a `redacted_thinking`
                    // block carries no plaintext and stays unextracted). The
                    // shared SSE extractor already maps thinking deltas to
                    // reasoning_content; without this arm a non-streaming
                    // thinking response reported no reasoning at all while its
                    // streamed twin did.
                    Some("thinking") => {
                        if let Some(text) = block.get("thinking").and_then(|t| t.as_str()) {
                            if !text.is_empty() {
                                if !reasoning.is_empty() {
                                    reasoning.push('\n');
                                }
                                reasoning.push_str(text);
                            }
                        }
                    }
                    // `server_tool_use` and `mcp_tool_use` are the same call
                    // made by a tool the provider runs on its side; the
                    // message parser reads both spellings as `tool_use`, so
                    // this reader must not drop them and lose the record.
                    Some("tool_use" | "server_tool_use" | "mcp_tool_use") => {
                        if let Some(call) = tool_use_text(block) {
                            token_data.tool_calls.push(call);
                            has_content = true;
                        }
                    }
                    _ => {}
                }
            }
            if !reasoning.is_empty() {
                token_data.reasoning_content = Some(reasoning);
                has_content = true;
            }
        }
    }

    if has_content { Some(token_data) } else { None }
}

/// Extract role and content from Anthropic message JSON
fn extract_message(msg: &Value) -> Option<(String, String)> {
    let role = msg.get("role").and_then(|r| r.as_str())?;
    let content = extract_content(msg.get("content"))?;

    if content.is_empty() {
        None
    } else {
        Some((role.to_string(), content))
    }
}

/// Extract text content from Anthropic content field (string or array)
fn extract_content(content: Option<&Value>) -> Option<String> {
    match content? {
        Value::String(s) => {
            if s.is_empty() {
                None
            } else {
                Some(s.clone())
            }
        }
        Value::Array(blocks) => {
            let text: String = blocks
                .iter()
                .filter_map(|b| match b.get("type").and_then(|t| t.as_str()) {
                    Some("text") => b.get("text").and_then(|t| t.as_str()).map(str::to_string),
                    // A tool-result turn carries the tool's output in a
                    // `tool_result` block, so without this arm the payload
                    // the API actually counts vanishes from the message.
                    Some(
                        "tool_result"
                        | "web_search_tool_result"
                        | "web_fetch_tool_result"
                        | "code_execution_tool_result"
                        | "bash_code_execution_tool_result"
                        | "text_editor_code_execution_tool_result"
                        | "tool_search_tool_result"
                        | "mcp_tool_result",
                    ) => tool_result_text(b.get("content")),
                    // A replayed conversation keeps the assistant's own
                    // `tool_use` blocks; without this arm the turn that asked
                    // for the tool extracted to nothing, dropping the tool
                    // name and its JSON arguments from the request.
                    Some("tool_use" | "server_tool_use" | "mcp_tool_use") => tool_use_text(b),
                    _ => None,
                })
                .collect::<Vec<String>>()
                .join("");

            if text.is_empty() { None } else { Some(text) }
        }
        _ => None,
    }
}

/// Text of an Anthropic `tool_result` block's `content` field: the plain
/// string or the array of text blocks the Messages API accepts for a tool
/// result. `None` for an empty result — the field is optional.
fn tool_result_text(content: Option<&Value>) -> Option<String> {
    match content? {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Array(blocks) => {
            let text: String = blocks
                .iter()
                .filter_map(|b| {
                    if b.get("type").and_then(|t| t.as_str()) == Some("text") {
                        b.get("text").and_then(|t| t.as_str())
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>()
                .join("");

            if text.is_empty() { None } else { Some(text) }
        }
        _ => None,
    }
}

/// Text of an Anthropic `tool_use` block: the tool name and its input as
/// JSON, the same `name: input` rendering the response reader emits. `None`
/// for a block with no serializable input.
fn tool_use_text(block: &Value) -> Option<String> {
    let name = block
        .get("name")
        .and_then(|n| n.as_str())
        .unwrap_or("unknown");
    let input = block.get("input")?;
    let input_str = serde_json::to_string(input).ok()?;
    Some(format!("{name}: {input_str}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_anthropic_request() {
        let request = serde_json::json!({
            "model": "claude-3-opus",
            "system": "You are Claude",
            "messages": [
                {"role": "user", "content": "Hello"}
            ]
        });

        let token_data = extract_token_data(Some(&request), None);
        assert!(token_data.is_some());

        let data = token_data.unwrap();
        assert_eq!(data.provider, "anthropic");
        assert_eq!(data.model, "claude-3-opus");
        assert_eq!(data.system_prompt, Some("You are Claude".to_string()));
        assert_eq!(data.request_messages.len(), 1);
    }

    #[test]
    fn test_extract_anthropic_response() {
        let response = serde_json::json!({
            "model": "claude-3-opus",
            "content": [
                {"type": "text", "text": "Hello!"}
            ]
        });

        let token_data = extract_token_data(None, Some(&response));
        assert!(token_data.is_some());

        let data = token_data.unwrap();
        assert_eq!(data.response_content.len(), 1);
        assert_eq!(data.response_content[0].content, "Hello!");
    }

    #[test]
    fn test_extract_system_array() {
        let request = serde_json::json!({
            "model": "claude-3-opus",
            "system": [
                {"type": "text", "text": "You are Claude"},
                {"type": "text", "text": "Be helpful"}
            ],
            "messages": []
        });

        let token_data = extract_token_data(Some(&request), None);
        assert!(token_data.is_some());

        let data = token_data.unwrap();
        assert_eq!(
            data.system_prompt,
            Some("You are Claude\nBe helpful".to_string())
        );
    }

    #[test]
    fn test_extract_content_array() {
        let request = serde_json::json!({
            "model": "claude-3-opus",
            "messages": [{
                "role": "user",
                "content": [
                    {"type": "text", "text": "Hello "},
                    {"type": "text", "text": "World"}
                ]
            }]
        });

        let token_data = extract_token_data(Some(&request), None);
        assert!(token_data.is_some());

        let data = token_data.unwrap();
        assert_eq!(data.request_messages.len(), 1);
        assert_eq!(data.request_messages[0].content, "Hello World");
    }

    #[test]
    fn test_extract_tool_use() {
        let response = serde_json::json!({
            "model": "claude-3-opus",
            "content": [
                {"type": "tool_use", "name": "get_weather", "input": {"city": "Beijing"}}
            ]
        });

        let token_data = extract_token_data(None, Some(&response));
        assert!(token_data.is_some());

        let data = token_data.unwrap();
        assert_eq!(data.tool_calls.len(), 1);
        assert!(data.tool_calls[0].contains("get_weather"));
    }

    /// An agentic request's bulk is the tool output, which the Messages API
    /// returns as a `tool_result` block inside a user message. The extractor
    /// read only `text` blocks, so that message was dropped from
    /// `request_messages` and the tool output vanished from the character and
    /// token estimates, while the OpenAI arm counts the same data (a
    /// `role: "tool"` message is a plain string there).
    #[test]
    fn test_extract_tool_result_blocks() {
        let request = serde_json::json!({
            "model": "claude-3-7-sonnet",
            "messages": [
                {"role": "user", "content": "run the tool"},
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "toolu_01", "content": "file not found"}
                ]},
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "toolu_02", "content": [
                        {"type": "text", "text": "first "},
                        {"type": "text", "text": "second"}
                    ]},
                    {"type": "text", "text": " trailing"}
                ]}
            ]
        });

        let token_data = extract_token_data(Some(&request), None).expect("request yields data");
        assert_eq!(
            token_data.request_messages.len(),
            3,
            "the two tool-result turns must be counted"
        );
        assert_eq!(token_data.request_messages[1].content, "file not found");
        assert_eq!(
            token_data.request_messages[2].content,
            "first second trailing"
        );
    }

    /// A replayed conversation contains the assistant's own `tool_use`
    /// blocks: the turn that asked for the tool, followed by the user turn
    /// carrying the matching `tool_result`. The request reader already counts
    /// the result half, but not the call that asked for it, so the assistant
    /// turn extracted to nothing and was dropped from `request_messages` —
    /// losing the tool name and the JSON arguments, which are a sizeable part
    /// of an agentic prompt. The response reader in this same file reads the
    /// block; the request arm was the copy missing it.
    #[test]
    fn test_extract_request_keeps_a_replayed_tool_use() {
        let request = serde_json::json!({
            "model": "claude-3-7-sonnet",
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

        let token_data = extract_token_data(Some(&request), None).expect("request yields data");
        assert_eq!(
            token_data.request_messages.len(),
            3,
            "the replayed assistant tool call must be counted"
        );
        let call = &token_data.request_messages[1];
        assert_eq!(call.role, "assistant");
        assert!(call.content.contains("Bash"), "got {:?}", call.content);
        assert!(call.content.contains("ls /tmp"), "got {:?}", call.content);
    }

    /// Extended thinking arrives as a `thinking` content block. The shared
    /// SSE extractor already maps thinking deltas to reasoning_content, so
    /// the non-streaming arm must too — a thinking-only response otherwise
    /// extracted to nothing.
    #[test]
    fn test_extract_thinking_blocks() {
        let response = serde_json::json!({
            "model": "claude-3-7-sonnet",
            "content": [
                {"type": "thinking", "thinking": "consider the options"},
                {"type": "text", "text": "The answer"}
            ]
        });
        let token_data = extract_token_data(None, Some(&response));
        assert!(token_data.is_some());
        let data = token_data.unwrap();
        assert_eq!(
            data.reasoning_content.as_deref(),
            Some("consider the options")
        );
        assert_eq!(data.response_content.len(), 1);
        assert_eq!(data.response_content[0].content, "The answer");
    }

    #[test]
    fn test_extract_thinking_only_response() {
        let response = serde_json::json!({
            "model": "claude-3-7-sonnet",
            "content": [{"type": "thinking", "thinking": "still thinking"}]
        });
        let token_data = extract_token_data(None, Some(&response));
        assert!(
            token_data.is_some(),
            "a thinking-only response is assistant output"
        );
        assert_eq!(
            token_data.unwrap().reasoning_content.as_deref(),
            Some("still thinking")
        );
    }

    #[test]
    fn test_redacted_thinking_block_is_not_extracted() {
        let response = serde_json::json!({
            "model": "claude-3-7-sonnet",
            "content": [
                {"type": "redacted_thinking", "data": "encrypted"},
                {"type": "text", "text": "Answer"}
            ]
        });
        let token_data = extract_token_data(None, Some(&response));
        assert!(token_data.is_some());
        let data = token_data.unwrap();
        assert!(data.reasoning_content.is_none());
        assert_eq!(data.response_content.len(), 1);
    }

    /// Server-tool answers are built from the provider-side block spellings
    /// the message parser already models: `server_tool_use` / `mcp_tool_use`
    /// carry the same `name` / `input` payload as `tool_use`. This extractor
    /// matched only the plain spelling, so an answer that only searched
    /// extracted to no content at all and its whole token record was dropped
    /// — while the same JSON parsed into messages without error.
    #[test]
    fn test_extract_server_tool_use_blocks() {
        let response = serde_json::json!({
            "model": "claude-sonnet-4-5",
            "content": [
                {"type": "server_tool_use", "id": "srvtoolu_1", "name": "web_search",
                 "input": {"query": "rust 2026"}}
            ]
        });
        let token_data = extract_token_data(None, Some(&response))
            .expect("a search-only answer must not lose its token record");
        assert_eq!(token_data.tool_calls.len(), 1);
        assert_eq!(
            token_data.tool_calls[0],
            "web_search: {\"query\":\"rust 2026\"}"
        );

        let response = serde_json::json!({
            "model": "claude-sonnet-4-5",
            "content": [
                {"type": "mcp_tool_use", "id": "mcptoolu_1", "name": "github_search",
                 "input": {"q": "repo:anolisa"}}
            ]
        });
        let token_data = extract_token_data(None, Some(&response))
            .expect("an MCP connector call must not lose its token record");
        assert_eq!(
            token_data.tool_calls[0],
            "github_search: {\"q\":\"repo:anolisa\"}"
        );
    }

    /// The request arm has the same blind spot: a replayed assistant turn
    /// that asked for a server-side tool was dropped from
    /// `request_messages`, and an `mcp_tool_result` carrying the connector's
    /// text output was dropped too.
    #[test]
    fn test_extract_request_server_tool_blocks() {
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
                    {"type": "mcp_tool_result", "tool_use_id": "srvtoolu_1", "content": [
                        {"type": "text", "text": "Rust 2026 edition"}
                    ]}
                ]}
            ]
        });
        let token_data = extract_token_data(Some(&request), None).expect("the request yields data");
        assert_eq!(
            token_data.request_messages.len(),
            3,
            "the replayed server call and its result must be counted"
        );
        assert!(
            token_data.request_messages[1]
                .content
                .contains("web_search"),
            "got {:?}",
            token_data.request_messages[1].content
        );
        assert_eq!(token_data.request_messages[2].content, "Rust 2026 edition");
    }

    #[test]
    fn test_empty_content_returns_none() {
        let request = serde_json::json!({"model": "claude-3-opus"});
        let result = extract_token_data(Some(&request), None);
        assert!(result.is_none());
    }
}
