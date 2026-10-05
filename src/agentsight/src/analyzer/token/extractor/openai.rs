//! OpenAI token data extraction

use super::super::data::{MessageTokenData, ResponseTokenData, TokenData};
use super::utils::extract_model_from_json;
use serde_json::Value;

/// Extract token data from OpenAI format JSON
pub fn extract_token_data(
    request_json: Option<&Value>,
    response_json: Option<&Value>,
) -> Option<TokenData> {
    let model = extract_model_from_json(request_json, response_json)
        .unwrap_or_else(|| "unknown".to_string());

    let mut token_data = TokenData::new("openai", model);
    let mut has_content = false;

    // Extract from request
    if let Some(req) = request_json {
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

    // Extract from response using shared logic
    if let Some((content, reasoning, tool_calls)) = extract_response_content(response_json) {
        if !content.is_empty() {
            token_data
                .response_content
                .push(ResponseTokenData { content });
            has_content = true;
        }
        if let Some(r) = reasoning {
            token_data.reasoning_content = Some(r);
            has_content = true;
        }
        for tool_call in tool_calls {
            token_data.tool_calls.push(tool_call);
            has_content = true;
        }
    }

    if has_content { Some(token_data) } else { None }
}

/// Extract response content from OpenAI format response JSON
///
/// Returns a tuple of (content, reasoning_content, tool_calls)
/// - content: The main response text
/// - reasoning_content: Optional reasoning/thinking content
/// - tool_calls: Vec of formatted tool call strings "name: arguments"
pub fn extract_response_content(
    response_json: Option<&Value>,
) -> Option<(String, Option<String>, Vec<String>)> {
    let resp = response_json?;

    let mut content = String::new();
    let mut reasoning = None;
    let mut tool_calls = Vec::new();
    let mut has_data = false;

    if let Some(choices) = resp.get("choices").and_then(|c| c.as_array()) {
        for choice in choices {
            // Support both "message" (standard response) and "delta" (SSE streaming) formats
            let msg_or_delta = choice.get("message").or_else(|| choice.get("delta"));

            if let Some(msg) = msg_or_delta {
                // Extract content
                if let Some(c) = msg.get("content").and_then(|c| c.as_str()) {
                    if !c.is_empty() {
                        content.push_str(c);
                        has_data = true;
                    }
                }

                // Extract reasoning_content
                if let Some(r) = msg.get("reasoning_content").and_then(|r| r.as_str()) {
                    if !r.is_empty() {
                        // For SSE chunks, accumulate reasoning content
                        reasoning = match reasoning {
                            Some(existing) => Some(existing + r),
                            None => Some(r.to_string()),
                        };
                        has_data = true;
                    }
                }

                // Extract tool_calls - only extract function name and arguments
                if let Some(calls) = msg.get("tool_calls").and_then(|t| t.as_array()) {
                    for tool_call in calls {
                        if let Some(func) = tool_call.get("function") {
                            let name = func.get("name").and_then(|n| n.as_str()).unwrap_or("");
                            let arguments =
                                func.get("arguments").and_then(|a| a.as_str()).unwrap_or("");
                            let tool_content = format!("{name}: {arguments}");
                            if !tool_content.is_empty() {
                                tool_calls.push(tool_content);
                                has_data = true;
                            }
                        }
                    }
                }
            }
        }
    }

    if has_data {
        return Some((content, reasoning, tool_calls));
    }

    // OpenAI Responses API SSE chunks have a different shape — top-level
    // "type" tags such as `response.output_text.delta` carry text in
    // `delta` / `text`, while `response.output_item.done` embeds the
    // assistant message under `item.content[].text`. Extract text from
    // the kinds that contribute to assistant output tokens.
    if let Some(ty) = resp.get("type").and_then(|t| t.as_str()) {
        match ty {
            "response.output_text.delta" => {
                if let Some(d) = resp.get("delta").and_then(|d| d.as_str()) {
                    if !d.is_empty() {
                        return Some((d.to_string(), None, Vec::new()));
                    }
                }
            }
            "response.output_text.done" => {
                if let Some(t) = resp.get("text").and_then(|t| t.as_str()) {
                    if !t.is_empty() {
                        return Some((t.to_string(), None, Vec::new()));
                    }
                }
            }
            "response.reasoning_text.delta" | "response.reasoning_summary_text.delta" => {
                if let Some(d) = resp.get("delta").and_then(|d| d.as_str()) {
                    if !d.is_empty() {
                        return Some((String::new(), Some(d.to_string()), Vec::new()));
                    }
                }
            }
            "response.output_item.done" => {
                if let Some(item_content) = resp
                    .get("item")
                    .and_then(|i| i.get("content"))
                    .and_then(|c| c.as_array())
                {
                    let mut text = String::new();
                    for part in item_content {
                        if let Some(t) = part.get("text").and_then(|t| t.as_str()) {
                            text.push_str(t);
                        }
                    }
                    if !text.is_empty() {
                        return Some((text, None, Vec::new()));
                    }
                }
            }
            // Anthropic Messages streams text, thinking and tool input as
            // `content_block_delta` events, each carrying its own
            // `delta.type`. The drained-stream and analyze-chatml paths
            // aggregate the same shape, so leaving it unextracted makes the
            // callers count a response with content as zero output tokens.
            "content_block_delta" => {
                if let Some(delta) = resp.get("delta") {
                    match delta.get("type").and_then(|t| t.as_str()) {
                        Some("text_delta") => {
                            if let Some(text) = delta.get("text").and_then(|t| t.as_str()) {
                                if !text.is_empty() {
                                    return Some((text.to_string(), None, Vec::new()));
                                }
                            }
                        }
                        Some("thinking_delta") => {
                            if let Some(thinking) = delta.get("thinking").and_then(|t| t.as_str()) {
                                if !thinking.is_empty() {
                                    return Some((
                                        String::new(),
                                        Some(thinking.to_string()),
                                        Vec::new(),
                                    ));
                                }
                            }
                        }
                        Some("input_json_delta") => {
                            if let Some(partial) =
                                delta.get("partial_json").and_then(|t| t.as_str())
                            {
                                if !partial.is_empty() {
                                    return Some((String::new(), None, vec![partial.to_string()]));
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    None
}

/// Extract role and content from OpenAI message JSON
fn extract_message(msg: &Value) -> Option<(String, String)> {
    let role = msg.get("role").and_then(|r| r.as_str())?;
    let content = extract_content(msg.get("content"))?;

    if content.is_empty() {
        None
    } else {
        Some((role.to_string(), content))
    }
}

/// Extract text content from OpenAI content field (string or array)
fn extract_content(content: Option<&Value>) -> Option<String> {
    match content? {
        Value::String(s) => {
            if s.is_empty() {
                None
            } else {
                Some(s.clone())
            }
        }
        Value::Array(parts) => {
            let text: String = parts
                .iter()
                .filter_map(|p| {
                    if p.get("type").and_then(|t| t.as_str()) == Some("text") {
                        p.get("text").and_then(|t| t.as_str())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_openai_request() {
        let request = serde_json::json!({
            "model": "gpt-4",
            "messages": [
                {"role": "system", "content": "You are helpful"},
                {"role": "user", "content": "Hello"}
            ]
        });

        let token_data = extract_token_data(Some(&request), None);
        assert!(token_data.is_some());

        let data = token_data.unwrap();
        assert_eq!(data.provider, "openai");
        assert_eq!(data.model, "gpt-4");
        assert_eq!(data.request_messages.len(), 2);
        assert_eq!(data.request_messages[0].role, "system");
        assert_eq!(data.request_messages[0].content, "You are helpful");
    }

    #[test]
    fn test_extract_openai_response() {
        let response = serde_json::json!({
            "model": "gpt-4",
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "Hi there!"
                }
            }]
        });

        let token_data = extract_token_data(None, Some(&response));
        assert!(token_data.is_some());

        let data = token_data.unwrap();
        assert_eq!(data.response_content.len(), 1);
        assert_eq!(data.response_content[0].content, "Hi there!");
    }

    #[test]
    fn test_extract_with_tools() {
        let request = serde_json::json!({
            "model": "gpt-4",
            "messages": [{"role": "user", "content": "What's the weather?"}],
            "tools": [{
                "type": "function",
                "function": {
                    "name": "get_weather",
                    "description": "Get weather info"
                }
            }]
        });

        let token_data = extract_token_data(Some(&request), None);
        assert!(token_data.is_some());

        let data = token_data.unwrap();
        assert_eq!(data.tools.len(), 1);
    }

    #[test]
    fn test_extract_reasoning_content() {
        let response = serde_json::json!({
            "model": "qwen",
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "The answer is 42",
                    "reasoning_content": "Let me think about this..."
                }
            }]
        });

        let token_data = extract_token_data(None, Some(&response));
        assert!(token_data.is_some());

        let data = token_data.unwrap();
        assert_eq!(
            data.reasoning_content,
            Some("Let me think about this...".to_string())
        );
    }

    #[test]
    fn test_extract_content_array() {
        let request = serde_json::json!({
            "model": "gpt-4-vision",
            "messages": [{
                "role": "user",
                "content": [
                    {"type": "text", "text": "What's in this image?"},
                    {"type": "image_url", "image_url": {"url": "http://example.com/image.jpg"}}
                ]
            }]
        });

        let token_data = extract_token_data(Some(&request), None);
        assert!(token_data.is_some());

        let data = token_data.unwrap();
        assert_eq!(data.request_messages.len(), 1);
        assert_eq!(data.request_messages[0].content, "What's in this image?");
    }

    #[test]
    fn test_empty_content_returns_none() {
        let request = serde_json::json!({"model": "gpt-4"});
        let result = extract_token_data(Some(&request), None);
        assert!(result.is_none());
    }

    #[test]
    fn test_responses_api_output_text_delta() {
        let chunk = serde_json::json!({
            "type": "response.output_text.delta",
            "delta": "hello world",
        });
        let (content, reasoning, tools) =
            extract_response_content(Some(&chunk)).expect("should extract delta text");
        assert_eq!(content, "hello world");
        assert!(reasoning.is_none());
        assert!(tools.is_empty());
    }

    #[test]
    fn test_responses_api_output_text_done() {
        let chunk = serde_json::json!({
            "type": "response.output_text.done",
            "text": "complete output text",
        });
        let (content, _, _) =
            extract_response_content(Some(&chunk)).expect("should extract final text");
        assert_eq!(content, "complete output text");
    }

    #[test]
    fn test_responses_api_output_item_done() {
        let chunk = serde_json::json!({
            "type": "response.output_item.done",
            "item": {
                "type": "message",
                "role": "assistant",
                "content": [
                    {"type": "output_text", "text": "first"},
                    {"type": "output_text", "text": " second"},
                ],
            },
        });
        let (content, _, _) =
            extract_response_content(Some(&chunk)).expect("should extract item content");
        assert_eq!(content, "first second");
    }

    #[test]
    fn test_responses_api_unknown_type_returns_none() {
        let chunk = serde_json::json!({
            "type": "response.created",
            "response": {"id": "abc"},
        });
        assert!(extract_response_content(Some(&chunk)).is_none());
    }

    /// Anthropic streams content as `content_block_delta` events; the drained
    /// and analyze-chatml paths aggregate them, so the shared extractor must
    /// not count the whole response as zero output tokens.
    #[test]
    fn test_anthropic_text_delta() {
        let chunk = serde_json::json!({
            "type": "content_block_delta",
            "index": 0,
            "delta": {"type": "text_delta", "text": "Hel"},
        });
        let (content, reasoning, tools) =
            extract_response_content(Some(&chunk)).expect("should extract the text delta");
        assert_eq!(content, "Hel");
        assert!(reasoning.is_none());
        assert!(tools.is_empty());
    }

    #[test]
    fn test_anthropic_thinking_delta() {
        let chunk = serde_json::json!({
            "type": "content_block_delta",
            "index": 0,
            "delta": {"type": "thinking_delta", "thinking": "step by step"},
        });
        let (content, reasoning, _) =
            extract_response_content(Some(&chunk)).expect("should extract the thinking delta");
        assert!(content.is_empty());
        assert_eq!(reasoning.as_deref(), Some("step by step"));
    }

    #[test]
    fn test_anthropic_input_json_delta() {
        let chunk = serde_json::json!({
            "type": "content_block_delta",
            "index": 1,
            "delta": {"type": "input_json_delta", "partial_json": "{\"city\":"},
        });
        let (content, _, tools) =
            extract_response_content(Some(&chunk)).expect("should extract the tool input fragment");
        assert!(content.is_empty());
        assert_eq!(tools, vec!["{\"city\":".to_string()]);
    }

    #[test]
    fn test_anthropic_non_content_events_are_not_extracted() {
        // Guard: a message_start/message_delta/ping carries no assistant text
        // and must keep returning None instead of a fabricated empty payload.
        for chunk in [
            serde_json::json!({"type": "message_start", "message": {"id": "msg_1"}}),
            serde_json::json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"}}),
            serde_json::json!({"type": "ping"}),
        ] {
            assert!(
                extract_response_content(Some(&chunk)).is_none(),
                "{chunk} must stay unextracted"
            );
        }
    }

    #[test]
    fn test_responses_api_reasoning_text_delta() {
        let chunk = serde_json::json!({
            "type": "response.reasoning_text.delta",
            "delta": "thinking...",
        });
        let (content, reasoning, tools) =
            extract_response_content(Some(&chunk)).expect("should extract reasoning");
        assert!(content.is_empty());
        assert_eq!(reasoning, Some("thinking...".to_string()));
        assert!(tools.is_empty());
    }

    #[test]
    fn test_responses_api_reasoning_summary_text_delta() {
        let chunk = serde_json::json!({
            "type": "response.reasoning_summary_text.delta",
            "delta": "summary...",
        });
        let (content, reasoning, tools) =
            extract_response_content(Some(&chunk)).expect("should extract reasoning summary");
        assert!(content.is_empty());
        assert_eq!(reasoning, Some("summary...".to_string()));
        assert!(tools.is_empty());
    }

    #[test]
    fn test_responses_api_output_text_delta_empty() {
        let chunk = serde_json::json!({
            "type": "response.output_text.delta",
            "delta": "",
        });
        assert!(extract_response_content(Some(&chunk)).is_none());
    }

    #[test]
    fn test_responses_api_output_text_done_empty() {
        let chunk = serde_json::json!({
            "type": "response.output_text.done",
            "text": "",
        });
        assert!(extract_response_content(Some(&chunk)).is_none());
    }

    #[test]
    fn test_responses_api_output_item_done_empty() {
        let chunk = serde_json::json!({
            "type": "response.output_item.done",
            "item": {
                "type": "message",
                "role": "assistant",
                "content": [],
            },
        });
        assert!(extract_response_content(Some(&chunk)).is_none());
    }

    #[test]
    fn test_extract_response_with_tool_calls() {
        let response = serde_json::json!({
            "model": "gpt-4",
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "",
                    "tool_calls": [{
                        "id": "tc_1",
                        "function": {"name": "search", "arguments": "{\"q\":\"rust\"}"}
                    }]
                }
            }]
        });
        let token_data = extract_token_data(None, Some(&response)).unwrap();
        assert_eq!(token_data.tool_calls.len(), 1);
        assert!(token_data.tool_calls[0].contains("search"));
    }

    #[test]
    fn test_extract_sse_reasoning_content_accumulation() {
        let chunk = serde_json::json!({
            "model": "qwen",
            "choices": [
                {"delta": {"content": "a", "reasoning_content": "think1"}},
                {"delta": {"content": "b", "reasoning_content": "think2"}}
            ]
        });
        let (content, reasoning, _) =
            extract_response_content(Some(&chunk)).expect("should extract");
        assert_eq!(content, "ab");
        assert_eq!(reasoning, Some("think1think2".to_string()));
    }

    #[test]
    fn test_extract_tool_calls_skips_missing_function() {
        let response = serde_json::json!({
            "model": "gpt-4",
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": "answer",
                    "tool_calls": [{"id": "tc_1"}]
                }
            }]
        });
        let (content, _, tool_calls) =
            extract_response_content(Some(&response)).expect("should extract content");
        assert_eq!(content, "answer");
        assert!(
            tool_calls.is_empty(),
            "tool_call without function should be skipped"
        );
    }
}
