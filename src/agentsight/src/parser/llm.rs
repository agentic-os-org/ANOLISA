//! Shared LLM protocol classification and request-body views.
//!
//! Both the analyzer layer (L4: the audit gate and unified token counting)
//! and the genai layer (L5: row creation and deep parsing) need to answer
//! the same two questions about a captured call: "is this path an LLM
//! inference endpoint?" and "which messages does this request body carry?".
//! The answers live here in the parser layer (L2) — the highest layer both
//! sides may import — so the two views are identical by construction
//! instead of synchronized by discipline: a private analyzer copy of the
//! path set drifted before, and `/v1/responses` (plus the DashScope native
//! endpoints) were parsed into trajectories yet never audited.

use serde_json::Value;

/// Path suffixes of the DashScope/Bailian **native** protocol.
///
/// Full form: `POST https://{WorkspaceId}.{region}.maas.aliyuncs.com
/// /api/v1/services/aigc/{text,multimodal}-generation/generation`.
/// Distinct from the OpenAI-compatible mode
/// (`/compatible-mode/v1/chat/completions`), which already matches the
/// `/v1/chat/completions` pattern.
const DASHSCOPE_NATIVE_PATHS: [&str; 2] = [
    "/aigc/text-generation/generation",
    "/aigc/multimodal-generation/generation",
];

/// Whether the path belongs to the DashScope/Bailian native protocol.
pub fn is_dashscope_native_path(path: &str) -> bool {
    DASHSCOPE_NATIVE_PATHS.iter().any(|p| path.contains(p))
}

/// Check if the path indicates an LLM API call.
///
/// Shared by the genai row-creation gate and the analyzer audit gate so the
/// set of paths that create a row and the set that is audited cannot drift
/// apart again.
pub fn is_llm_api_path(path: &str) -> bool {
    path.contains("/v1/chat/completions")
        || path.contains("/v1/completions")
        // Anthropic's /v1/messages/count_tokens (token counting) and
        // /v1/messages/batches* (Batch API) share the inference prefix
        // but are not inference calls. Must stay in lockstep with
        // AnthropicParser::matches_path so a count-tokens call neither
        // creates a row (this gate) nor gets deep-parsed (that gate).
        || (path.contains("/v1/messages")
            && !path.contains("/v1/messages/count_tokens")
            && !path.contains("/v1/messages/batches"))
        // The Responses API's per-id sub-endpoints (GET retrieve,
        // POST cancel, DELETE) share the /v1/responses prefix but are
        // not inference calls: the retrieval response IS the stored
        // response object, so admitting a poll re-records the create
        // call's output and re-counts its usage tokens. Must stay in
        // lockstep with OpenAIParser::matches_path so a retrieval
        // neither creates a row (this gate) nor gets deep-parsed
        // (that gate).
        || (path.contains("/v1/responses") && !path.contains("/v1/responses/"))
        || path.contains("/chat/completions")
        || path.contains("/completions")
        || path.contains("/api/v1/copilot/generate_copilot")
        || is_gemini_generation_path(path)
        || is_dashscope_native_path(path)
}

/// Gemini's inference endpoints: `:generateContent` and
/// `:streamGenerateContent`.
///
/// The model rides in the path (`/v1beta/models/{model}:streamGenerateContent`,
/// and Vertex AI's `/publishers/google/models/{model}:generateContent` shares
/// the tail) while the request body carries `contents`/`generationConfig`
/// instead of a top-level `model`. `:countTokens` shares the prefix but counts
/// tokens, it does not infer — the same rule the Anthropic `count_tokens`
/// branch above applies.
fn is_gemini_generation_path(path: &str) -> bool {
    path.contains(":generateContent") || path.contains(":streamGenerateContent")
}

/// Normalize the messages array from a parsed request body.
///
/// Supports:
/// - OpenAI chat completions: top-level `"messages"` array.
/// - OpenAI Responses API (codex 0.137+ via dashscope `/v1/responses`):
///   top-level `"input"` array with sibling `"instructions"` string.
/// - OpenAI Responses API string shorthand: top-level `"input"` as a
///   plain non-empty string, equivalent to a single-user-message
///   request.
/// - DashScope/Bailian native protocol: top-level `"input"` **object**
///   wrapping a `"messages"` array.
///
/// Returns `(messages_vec, instructions_text)` where `instructions_text`
/// is the system-prompt fallback used when the messages array has no
/// `role == "system"` entry. It is set for:
/// - OpenAI Responses API: the top-level `"instructions"` string.
/// - Anthropic Messages API: the top-level `"system"` field (string or
///   array of `{"type":"text","text":"..."}` blocks), since Anthropic
///   carries the system prompt outside the messages array.
///
/// The native protocol needs no fallback: its system prompt lives inside
/// `input.messages`.
pub fn extract_messages_view(body: &Value) -> Option<(Vec<Value>, Option<String>)> {
    if let Some(arr) = body.get("messages").and_then(|m| m.as_array()) {
        let system_text = body.get("system").and_then(extract_system_text);
        return Some((arr.clone(), system_text));
    }
    // Gemini v1beta requests carry their messages under `contents` (each with
    // a `role` and a `parts` array) and their system prompt as
    // `systemInstruction.parts[].text`. Without this arm a drained Gemini
    // call's `input_tokens` stays NULL because no other shape matches.
    //
    // The contents are mapped onto the canonical `{role, content}` shape (see
    // [`gemini_content_to_message`]) instead of being returned verbatim: every
    // consumer of the view — the chat-template renderers behind
    // `count_request_tokens`, `drain_request_input_tokens` and the
    // analyze-chatml CLI, and the semantic layer's `content`/`tool_calls`
    // readers — reads the canonical shape, and a raw Gemini content carries
    // its text in `parts` with the assistant role spelled `model`, so a
    // verbatim return made the whole request render or parse as empty.
    if let Some(arr) = body.get("contents").and_then(|c| c.as_array()) {
        let system_text = body
            .get("systemInstruction")
            .and_then(|si| si.get("parts"))
            .and_then(|p| p.as_array())
            .map(|parts| {
                parts
                    .iter()
                    .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .filter(|s| !s.is_empty());
        return Some((
            arr.iter().filter_map(gemini_content_to_message).collect(),
            system_text,
        ));
    }
    if let Some(input) = body.get("input") {
        if let Some(arr) = input.as_array() {
            let instructions = body
                .get("instructions")
                .and_then(|s| s.as_str())
                .map(|s| s.to_string());
            return Some((arr.clone(), instructions));
        }
        // OpenAI Responses API string shorthand: `"input": "<text>"` is
        // defined as a request with exactly one user message carrying
        // that text. Map it onto that message so the request event is
        // not silently skipped from the breakdown. An empty string
        // carries no message: fall through to `None` as before.
        if let Some(s) = input.as_str().filter(|s| !s.is_empty()) {
            let instructions = body
                .get("instructions")
                .and_then(|i| i.as_str())
                .map(|i| i.to_string());
            return Some((
                vec![serde_json::json!({"role": "user", "content": s})],
                instructions,
            ));
        }
        if let Some(arr) = input.get("messages").and_then(|m| m.as_array()) {
            return Some((arr.clone(), None));
        }
    }
    None
}

/// Map one Gemini `contents` entry onto the canonical `{role, content}`
/// message shape every consumer of the view reads.
///
/// Gemini carries the text in a `parts` array, spells the assistant role
/// `model`, and rides tool interactions as `functionCall` / `functionResponse`
/// parts instead of `tool_calls` / tool-role messages:
///
/// - text parts are joined with `"\n"` into `content` (the same join the
///   Responses string-shorthand arm performs), so chat-template renderers see
///   a string and the semantic layer's text readers see the conversation;
/// - `role: "model"` becomes `assistant` (any other role is passed through,
///   with the legacy `function` role covered by the mapping below);
/// - a `functionCall` part becomes a `tool_calls` entry whose `arguments`
///   carry the call's `args` object — the arguments-in-place decoding the
///   template counters run only touches string arguments, so an object passes
///   through untouched;
/// - a `functionResponse` part turns the content into a `role: "tool"`
///   message whose content is the serialized response — the shape the semantic
///   layer and the chat templates read OpenAI tool results in;
/// - binary parts (`inlineData` / `fileData`) carry no text any tokenizer in
///   this pipeline can count and are dropped.
///
/// A content with no part this mapping recognizes contributes no message, so
/// an all-binary `contents` array yields an empty view rather than a list of
/// empty messages.
fn gemini_content_to_message(content: &Value) -> Option<Value> {
    let parts = content.get("parts").and_then(|p| p.as_array())?;
    let mut text_parts: Vec<&str> = Vec::new();
    let mut tool_calls: Vec<Value> = Vec::new();
    let mut function_response: Option<(&str, &Value)> = None;
    for part in parts {
        if let Some(text) = part.get("text").and_then(|t| t.as_str()) {
            if !text.is_empty() {
                text_parts.push(text);
            }
        }
        if let Some(call) = part.get("functionCall").filter(|c| c.is_object()) {
            tool_calls.push(serde_json::json!({
                "type": "function",
                "function": {
                    "name": call.get("name").and_then(|n| n.as_str()).unwrap_or_default(),
                    "arguments": call.get("args").cloned().unwrap_or_else(|| serde_json::json!({})),
                },
            }));
        }
        if let Some(response) = part.get("functionResponse").filter(|r| r.is_object()) {
            function_response = Some((
                response
                    .get("name")
                    .and_then(|n| n.as_str())
                    .unwrap_or_default(),
                response.get("response").unwrap_or(&Value::Null),
            ));
        }
    }

    if let Some((name, response)) = function_response {
        // A tool result is its own turn in the canonical shape. Text riding in
        // the same content is rare (a function response carries the tool
        // output); it is prepended so it is not silently lost.
        let mut content_text = text_parts.join("\n");
        match serde_json::to_string(response) {
            Ok(serialized) => {
                if !content_text.is_empty() {
                    content_text.push('\n');
                }
                content_text.push_str(&serialized);
            }
            Err(_) => {
                // `Value` always serializes; the arm only exists so the
                // mapping cannot fail on a malformed part.
            }
        }
        return Some(serde_json::json!({
            "role": "tool",
            "name": name,
            "content": content_text,
        }));
    }

    if text_parts.is_empty() && tool_calls.is_empty() {
        return None;
    }

    let role = match content.get("role").and_then(|r| r.as_str()) {
        Some("model") => "assistant",
        Some(role) => role,
        None => "user",
    };
    let mut message = serde_json::json!({
        "role": role,
        // An empty string, not null: the strict chat templates concatenate
        // `message['content']` and a null breaks the render exactly like a
        // missing key does.
        "content": text_parts.join("\n"),
    });
    if !tool_calls.is_empty() {
        message["tool_calls"] = Value::Array(tool_calls);
    }
    Some(message)
}

/// Normalize the tool definitions from a parsed request body.
///
/// Supports:
/// - OpenAI chat completions and the Anthropic Messages API: top-level
///   `"tools"` array.
/// - DashScope/Bailian native protocol: `"tools"` nested in the top-level
///   `"parameters"` object, which is where that protocol carries every
///   sampling parameter. The top-level spelling wins when both are
///   present, matching `GenAIBuilder::parse_request_body`.
///
/// Returns `None` when the request declares no tools. Callers that count
/// prompt tokens must use this rather than reading `"tools"` directly: a
/// native request nests its tool definitions, so a top-level-only read
/// silently drops them from the count.
pub fn extract_tools_view(body: &Value) -> Option<Vec<Value>> {
    body.get("tools")
        .or_else(|| body.get("parameters").and_then(|p| p.get("tools")))
        .and_then(|t| t.as_array())
        .cloned()
}

/// Extract text from Anthropic's top-level `system` field.
///
/// The field is either a plain string or an array of content blocks
/// (`{"type":"text","text":"..."}`). Returns `None` when empty so the
/// caller's "no system role in messages" fallback stays inactive.
fn extract_system_text(system: &Value) -> Option<String> {
    match system {
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
                .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
                .collect::<Vec<_>>()
                .join("\n");
            if text.is_empty() { None } else { Some(text) }
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_llm_api_path() {
        assert!(is_llm_api_path("/v1/chat/completions"));
        assert!(is_llm_api_path("/v1/completions"));
        assert!(is_llm_api_path("/v1/messages"));
        assert!(is_llm_api_path("/api/v1/copilot/generate_copilot"));
        assert!(is_llm_api_path("/proxy/v1/chat/completions"));
        assert!(!is_llm_api_path("/api/health"));
        assert!(!is_llm_api_path("/v1/models"));
    }

    /// Anthropic's count-tokens and Batch sub-endpoints share the inference
    /// prefix but are not inference calls — admitting them at this gate
    /// creates a phantom llm_call row per count (same conversation_id, zero
    /// tokens, no output) that inflates call counts and consumes
    /// preference-window slots.
    #[test]
    fn test_is_llm_api_path_rejects_anthropic_sub_endpoints() {
        assert!(!is_llm_api_path("/v1/messages/count_tokens"));
        assert!(!is_llm_api_path(
            "https://api.anthropic.com/v1/messages/count_tokens"
        ));
        assert!(!is_llm_api_path("/v1/messages/batches"));
        assert!(!is_llm_api_path("/v1/messages/batches/msgbatch_01ABC"));
        // The real endpoint still passes the gate.
        assert!(is_llm_api_path("/v1/messages"));
        assert!(is_llm_api_path("https://api.anthropic.com/v1/messages"));
    }

    /// The Responses API's per-id sub-endpoints (GET retrieve, POST cancel,
    /// DELETE) share the /v1/responses prefix but are not inference calls —
    /// the OpenAI twin of the count-tokens gate above. The retrieval
    /// response is the stored response object, so admitting a poll here
    /// re-records the create call's output and re-counts its usage tokens.
    #[test]
    fn test_is_llm_api_path_rejects_responses_sub_endpoints() {
        assert!(!is_llm_api_path("/v1/responses/resp_abc123"));
        assert!(!is_llm_api_path(
            "https://api.openai.com/v1/responses/resp_abc123"
        ));
        assert!(!is_llm_api_path("/v1/responses/resp_abc123/cancel"));
        assert!(!is_llm_api_path("/v1/responses/resp_abc123/input_items"));
        // The create endpoint still passes the gate, in bare-path,
        // full-URL and compatible-mode shapes.
        assert!(is_llm_api_path("/v1/responses"));
        assert!(is_llm_api_path("https://api.openai.com/v1/responses"));
        assert!(is_llm_api_path(
            "https://dashscope.aliyuncs.com/compatible-mode/v1/responses"
        ));
    }

    /// DashScope/Bailian native protocol endpoints end in `/generation`, which
    /// matched none of the compatible-mode patterns. Without them the whole
    /// non-streaming call was dropped at the `build_llm_call` gate.
    #[test]
    fn test_is_llm_api_path_dashscope_native() {
        assert!(is_llm_api_path(
            "/api/v1/services/aigc/text-generation/generation"
        ));
        assert!(is_llm_api_path(
            "/api/v1/services/aigc/multimodal-generation/generation"
        ));
        // Other aigc services (image synthesis, embeddings) stay out.
        assert!(!is_llm_api_path(
            "/api/v1/services/aigc/text2image/image-synthesis"
        ));
    }

    /// Gemini `generateContent` / `streamGenerateContent` share one tail with
    /// Vertex AI's publisher spelling and put the model in the path, so the
    /// endpoint has to be recognised here: without it a Gemini stream is
    /// audited only when its usage was parsed (the audit gate's path set is
    /// this one), a non-streaming call is dropped at the `build_llm_call` gate,
    /// and — worse — no pending row is created for a streaming call, so an
    /// interrupted Gemini call never reaches the drain path. `:countTokens`
    /// shares the prefix but is not an inference call, the same rule the
    /// Anthropic count-tokens branch applies.
    #[test]
    fn test_is_llm_api_path_gemini_generation() {
        assert!(is_llm_api_path(
            "/v1beta/models/gemini-2.5-pro:streamGenerateContent?alt=sse"
        ));
        assert!(is_llm_api_path(
            "/v1beta/models/gemini-2.0-flash:generateContent"
        ));
        assert!(is_llm_api_path(
            "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.5-flash:streamGenerateContent?alt=sse"
        ));
        // Vertex AI's publisher spelling shares the same tail.
        assert!(is_llm_api_path(
            "/v1/projects/p/locations/us-central1/publishers/google/models/gemini-2.5-pro:generateContent"
        ));
        // Token counting is not inference.
        assert!(!is_llm_api_path(
            "/v1beta/models/gemini-2.5-pro:countTokens"
        ));
        assert!(!is_llm_api_path("/v1beta/models"));
    }

    #[test]
    fn test_extract_messages_view_chat_completions() {
        let body = serde_json::json!({
            "model": "gpt-4",
            "messages": [
                {"role": "system", "content": "sys"},
                {"role": "user", "content": "hi"}
            ]
        });
        let (msgs, instructions) = extract_messages_view(&body).unwrap();
        assert_eq!(msgs.len(), 2);
        assert!(instructions.is_none());
    }

    #[test]
    fn test_extract_messages_view_responses_api() {
        let body = serde_json::json!({
            "model": "gpt-4",
            "input": [{"role": "user", "content": "hi"}],
            "instructions": "sys prompt"
        });
        let (msgs, instructions) = extract_messages_view(&body).unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(instructions.as_deref(), Some("sys prompt"));
    }

    #[test]
    fn test_extract_messages_view_none() {
        let body = serde_json::json!({"model": "gpt-4"});
        assert!(extract_messages_view(&body).is_none());
    }

    #[test]
    fn test_extract_tools_view_chat_completions() {
        let body = serde_json::json!({
            "model": "gpt-4",
            "messages": [{"role": "user", "content": "hi"}],
            "tools": [{"type": "function", "function": {"name": "read_file"}}]
        });
        let tools = extract_tools_view(&body).expect("tools exist");
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["function"]["name"], "read_file");
    }

    /// DashScope/Bailian native requests nest their tool definitions under
    /// the top-level `parameters` object, so a read of `"tools"` alone
    /// returns nothing for that protocol.
    #[test]
    fn test_extract_tools_view_dashscope_native_parameters() {
        let body = serde_json::json!({
            "model": "qwen3-max",
            "input": {"messages": [{"role": "user", "content": "hi"}]},
            "parameters": {
                "temperature": 0.5,
                "tools": [{"type": "function", "function": {"name": "read_file"}}]
            }
        });
        let tools = extract_tools_view(&body).expect("native tools exist");
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["function"]["name"], "read_file");
    }

    /// When both spellings are present the top-level one wins, so the view
    /// agrees with `GenAIBuilder::parse_request_body`.
    #[test]
    fn test_extract_tools_view_prefers_top_level() {
        let body = serde_json::json!({
            "model": "qwen3-max",
            "messages": [{"role": "user", "content": "hi"}],
            "tools": [{"type": "function", "function": {"name": "top"}}],
            "parameters": {"tools": [{"type": "function", "function": {"name": "nested"}}]}
        });
        let tools = extract_tools_view(&body).expect("tools exist");
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["function"]["name"], "top");
    }

    #[test]
    fn test_extract_tools_view_none() {
        let body = serde_json::json!({
            "model": "gpt-4",
            "messages": [{"role": "user", "content": "hi"}],
            "parameters": {"temperature": 0.5}
        });
        assert!(extract_tools_view(&body).is_none());
    }

    #[test]
    fn test_extract_messages_view_responses_api_without_instructions() {
        let body = serde_json::json!({
            "model": "gpt-4",
            "input": [{"role": "user", "content": "hi"}]
        });
        let (msgs, instructions) = extract_messages_view(&body).unwrap();
        assert_eq!(msgs.len(), 1);
        assert!(instructions.is_none());
    }

    /// OpenAI Responses API string shorthand: `"input": "<string>"`
    /// (e.g. codex CLI one-shot requests) is equivalent to a single
    /// user message and must yield a one-message view, not `None`.
    #[test]
    fn test_extract_messages_view_responses_api_string_input() {
        let body = serde_json::json!({
            "model": "gpt-4",
            "input": "write a haiku",
            "instructions": "sys prompt"
        });
        let (msgs, instructions) = extract_messages_view(&body).unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].get("role").and_then(|r| r.as_str()), Some("user"));
        assert_eq!(
            msgs[0].get("content").and_then(|c| c.as_str()),
            Some("write a haiku")
        );
        // Instructions prepending is unchanged for the string shape.
        assert_eq!(instructions.as_deref(), Some("sys prompt"));
    }

    #[test]
    fn test_extract_messages_view_responses_api_string_input_without_instructions() {
        let body = serde_json::json!({
            "model": "gpt-4",
            "input": "write a haiku"
        });
        let (msgs, instructions) = extract_messages_view(&body).unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].get("role").and_then(|r| r.as_str()), Some("user"));
        assert!(instructions.is_none());
    }

    /// An empty string is the only falsy shape of the string shorthand:
    /// it carries no message, so the view stays `None` as before.
    #[test]
    fn test_extract_messages_view_responses_api_empty_string_input() {
        let body = serde_json::json!({
            "model": "gpt-4",
            "input": ""
        });
        assert!(extract_messages_view(&body).is_none());
    }

    #[test]
    fn test_extract_system_text_string() {
        let system = serde_json::json!("You are helpful");
        assert_eq!(
            extract_system_text(&system),
            Some("You are helpful".to_string())
        );
    }

    #[test]
    fn test_extract_system_text_empty_string() {
        let system = serde_json::json!("");
        assert_eq!(extract_system_text(&system), None);
    }

    #[test]
    fn test_extract_system_text_array() {
        let system = serde_json::json!([
            {"type": "text", "text": "Part 1"},
            {"type": "text", "text": "Part 2"}
        ]);
        assert_eq!(
            extract_system_text(&system),
            Some("Part 1\nPart 2".to_string())
        );
    }

    #[test]
    fn test_extract_system_text_empty_array() {
        let system = serde_json::json!([]);
        assert_eq!(extract_system_text(&system), None);
    }

    #[test]
    fn test_extract_system_text_non_text() {
        assert_eq!(extract_system_text(&serde_json::json!(123)), None);
        assert_eq!(extract_system_text(&serde_json::Value::Null), None);
    }

    /// DashScope/Bailian native protocol wraps the messages array inside an
    /// `input` **object**, unlike the Responses API where `input` is an array.
    #[test]
    fn test_extract_messages_view_dashscope_native_input_object() {
        let body = serde_json::json!({
            "model": "qwen-plus",
            "input": {
                "messages": [
                    {"role": "system", "content": "sys"},
                    {"role": "user", "content": "hi"}
                ]
            },
            "parameters": {"result_format": "message"}
        });
        let (msgs, instructions) = extract_messages_view(&body).unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].get("role").and_then(|r| r.as_str()), Some("system"));
        // Native protocol carries the system prompt inside the messages array,
        // so no top-level instructions fallback is needed.
        assert!(instructions.is_none());
    }

    /// An `input` object without a `messages` array carries no conversation.
    #[test]
    fn test_extract_messages_view_dashscope_native_input_object_without_messages() {
        let body = serde_json::json!({
            "model": "qwen-plus",
            "input": {"prompt": "hi"}
        });
        assert!(extract_messages_view(&body).is_none());
    }

    #[test]
    fn test_extract_messages_view_anthropic_system() {
        let body = serde_json::json!({
            "model": "claude-3",
            "system": "You are helpful",
            "messages": [{"role": "user", "content": "Hi"}]
        });
        let (msgs, instructions) = extract_messages_view(&body).unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(instructions.as_deref(), Some("You are helpful"));
    }

    #[test]
    fn test_extract_messages_view_anthropic_system_array() {
        let body = serde_json::json!({
            "model": "claude-3",
            "system": [{"type": "text", "text": "sys prompt"}],
            "messages": [{"role": "user", "content": "Hi"}]
        });
        let (msgs, instructions) = extract_messages_view(&body).unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(instructions.as_deref(), Some("sys prompt"));
    }

    #[test]
    fn test_extract_messages_view_gemini_contents() {
        let body = serde_json::json!({
            "contents": [
                {"role": "user", "parts": [{"text": "Hello"}]},
                {"role": "model", "parts": [{"text": "Hi there"}]}
            ],
            "systemInstruction": {"parts": [{"text": "Be concise"}]}
        });
        let (msgs, instructions) = extract_messages_view(&body).unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(instructions.as_deref(), Some("Be concise"));
    }

    #[test]
    fn test_extract_messages_view_gemini_no_system() {
        let body = serde_json::json!({
            "contents": [{"role": "user", "parts": [{"text": "Hi"}]}]
        });
        let (msgs, instructions) = extract_messages_view(&body).unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(instructions, None);
    }

    /// The contents arm must return the canonical shape, not the raw Gemini
    /// content: text parts joined into `content` (the chat templates
    /// concatenate `message['content']`, which is undefined on a raw content),
    /// and the assistant role spelled `assistant`, not `model`.
    #[test]
    fn test_extract_messages_view_gemini_contents_map_onto_the_canonical_shape() {
        let body = serde_json::json!({
            "contents": [
                {"role": "user", "parts": [{"text": "Hello"}, {"text": "again"}]},
                {"role": "model", "parts": [{"text": "Hi there"}]}
            ]
        });
        let (msgs, _) = extract_messages_view(&body).unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0]["role"], "user");
        assert_eq!(msgs[0]["content"], "Hello\nagain");
        assert_eq!(msgs[1]["role"], "assistant");
        assert_eq!(msgs[1]["content"], "Hi there");
    }

    /// An agentic Gemini trace rides its tool interaction as `functionCall` /
    /// `functionResponse` parts. Returned verbatim, the assistant turn has no
    /// `content` and no `tool_calls`, and the tool result has neither — every
    /// consumer sees an empty conversation. The canonical mapping turns the
    /// call into `tool_calls` on the assistant message and the result into a
    /// `role: "tool"` message whose content is the serialized response.
    #[test]
    fn test_extract_messages_view_gemini_tool_parts_map_onto_tool_calls_and_tool_messages() {
        let body = serde_json::json!({
            "contents": [
                {"role": "user", "parts": [{"text": "list the files"}]},
                {
                    "role": "model",
                    "parts": [
                        {"text": "let me check"},
                        {"functionCall": {"name": "list_dir", "args": {"path": "/tmp"}}}
                    ]
                },
                {
                    "role": "user",
                    "parts": [{
                        "functionResponse": {
                            "name": "list_dir",
                            "response": {"entries": ["a", "b"]}
                        }
                    }]
                }
            ]
        });
        let (msgs, _) = extract_messages_view(&body).unwrap();
        assert_eq!(msgs.len(), 3);

        // The assistant turn keeps its text and gains the tool call.
        assert_eq!(msgs[1]["role"], "assistant");
        assert_eq!(msgs[1]["content"], "let me check");
        let calls = msgs[1]["tool_calls"].as_array().expect("tool_calls exist");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0]["function"]["name"], "list_dir");
        assert_eq!(calls[0]["function"]["arguments"]["path"], "/tmp");

        // The tool result is its own tool-role message.
        assert_eq!(msgs[2]["role"], "tool");
        assert_eq!(msgs[2]["name"], "list_dir");
        let response: serde_json::Value =
            serde_json::from_str(msgs[2]["content"].as_str().expect("content is a string"))
                .expect("content is the serialized response");
        assert_eq!(response["entries"][0], "a");
    }

    /// A tool-call-only assistant turn carries no text: its `content` must be
    /// an empty string (a null breaks the strict templates' concatenation the
    /// same way a missing key does), and binary parts contribute nothing.
    #[test]
    fn test_extract_messages_view_gemini_binary_only_content_is_dropped() {
        let body = serde_json::json!({
            "contents": [
                {"role": "user", "parts": [{"text": "look"}]},
                {
                    "role": "model",
                    "parts": [{"functionCall": {"name": "snap", "args": {}}}]
                },
                {
                    "role": "user",
                    "parts": [{"inlineData": {"mimeType": "image/png", "data": "aGk="}}]
                }
            ]
        });
        let (msgs, _) = extract_messages_view(&body).unwrap();
        assert_eq!(
            msgs.len(),
            2,
            "the binary-only content contributes no message"
        );
        assert_eq!(msgs[1]["role"], "assistant");
        assert_eq!(msgs[1]["content"], "");
        assert!(msgs[1]["tool_calls"].is_array());
    }
}
