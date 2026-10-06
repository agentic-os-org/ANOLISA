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
        || is_dashscope_native_path(path)
        // Gemini embeds the model in the URL and the inference action in the
        // path tail: `/v1beta/models/{model}:streamGenerateContent?alt=sse`
        // and `:generateContent`, plus the Vertex AI publisher spelling that
        // carries the same tail. Must stay in lockstep with
        // `MessageParser::gemini_model_from_path` (which defines the action
        // set and strips the query first) so a Gemini call neither vanishes
        // from the genai pipeline (a non-streaming `:generateContent` request
        // has no `is_sse` fallback in `build_pending_from_request`) nor gets
        // rejected by the audit gate that re-uses this set — the gate that
        // e31dd68f8 tightened made the Gemini labeling merged in cd55e0b5b
        // unreachable and turned its own test red on main. Token counting
        // (`:countTokens`) is not an inference call and stays out, the same
        // way Anthropic's count_tokens does.
        || path.contains(":streamGenerateContent")
        || path.contains(":generateContent")
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

        // Gemini inference actions, in lockstep with
        // `MessageParser::gemini_model_from_path`: the generative-language
        // spelling (streaming with its `?alt=sse` query, and plain), the
        // Vertex AI publisher spelling, and the non-inference exclusions.
        assert!(is_llm_api_path(
            "/v1beta/models/gemini-2.5-pro:streamGenerateContent?alt=sse"
        ));
        assert!(is_llm_api_path(
            "/v1/models/gemini-2.0-flash:generateContent"
        ));
        assert!(is_llm_api_path(
            "/v1/projects/p/locations/us-central1/publishers/google/models/gemini-2.5-pro:streamGenerateContent"
        ));
        assert!(!is_llm_api_path(
            "/v1beta/models/gemini-2.5-pro:countTokens"
        ));
        assert!(!is_llm_api_path("/v1beta/models"));
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
}
