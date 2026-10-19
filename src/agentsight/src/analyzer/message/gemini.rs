//! Google Gemini `generateContent` API parser
//!
//! This module parses request and response bodies for Google's
//! generative-language API. The model is carried by the URL, not the body:
//! `POST /v1beta/models/{model}:generateContent` (or
//! `:streamGenerateContent`, with `?alt=sse` when streaming), so callers pass
//! the name [`MessageParser::gemini_model_from_path`] read back into
//! [`GeminiParser::parse_request`].
//!
//! # Request format
//! ```json
//! {
//!   "systemInstruction": {"parts": [{"text": "Be concise"}]},
//!   "contents": [
//!     {"role": "user", "parts": [{"text": "Hi"}]},
//!     {"role": "model", "parts": [{"functionCall": {"name": "lookup", "args": {}}}]},
//!     {"role": "user", "parts": [{"functionResponse": {"name": "lookup", "response": {}}}]}
//!   ],
//!   "generationConfig": {"temperature": 0.2, "maxOutputTokens": 64}
//! }
//! ```
//!
//! # Response format
//! `candidates[].content.parts[]` carries text and whole `functionCall`
//! objects (calls are not streamed as deltas), `candidates[].finishReason`
//! closes a candidate, and `usageMetadata` carries the counters. A streaming
//! response arrives as the same payload repeated per chunk, so
//! [`GeminiParser::parse_response`] also accepts a JSON array of them.
//!
//! [`MessageParser::gemini_model_from_path`]: super::MessageParser::gemini_model_from_path

use serde::{Deserialize, Serialize};

// ============================================================================
// Gemini-specific request / response types
// ============================================================================

/// A Gemini `generateContent` request body.
///
/// `model` and `stream` are not on the wire: both are derived from the request
/// path (`/models/{model}:streamGenerateContent`), which is the only place
/// Gemini names them.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeminiRequest {
    /// Model name read from the request path (e.g. "gemini-2.5-pro").
    /// Not on the wire: filled in by [`GeminiParser::parse_request`].
    #[serde(default)]
    pub model: String,

    /// Conversation turns; `role` is `user` or `model`
    #[serde(default)]
    pub contents: Vec<GeminiContent>,

    /// System prompt, carried outside `contents`
    #[serde(
        default,
        rename = "systemInstruction",
        skip_serializing_if = "Option::is_none"
    )]
    pub system_instruction: Option<GeminiContent>,

    /// Sampling and output controls
    #[serde(
        default,
        rename = "generationConfig",
        skip_serializing_if = "Option::is_none"
    )]
    pub generation_config: Option<GeminiGenerationConfig>,

    /// Whether the path action is `:streamGenerateContent`.
    /// Not on the wire: filled in by [`GeminiParser::parse_request`].
    #[serde(default)]
    pub stream: bool,
}

/// One Gemini conversation turn (`contents[]` entry)
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GeminiContent {
    /// `user` or `model`; absent for `systemInstruction`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,

    /// Ordered content parts
    #[serde(default)]
    pub parts: Vec<GeminiPart>,
}

/// One content part.
///
/// Gemini marks the payload with a key rather than a `type` discriminator, so
/// every shape is optional and unknown keys are ignored: an unmodeled part
/// (e.g. `executableCode`) must not fail the whole exchange.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GeminiPart {
    /// Plain text
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,

    /// Marks a text part as model reasoning (`thought: true`)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thought: Option<bool>,

    /// A tool call the model issued
    #[serde(
        default,
        rename = "functionCall",
        skip_serializing_if = "Option::is_none"
    )]
    pub function_call: Option<GeminiFunctionCall>,

    /// A tool result the application sent back
    #[serde(
        default,
        rename = "functionResponse",
        skip_serializing_if = "Option::is_none"
    )]
    pub function_response: Option<GeminiFunctionResponse>,

    /// Inline binary payload (image/audio); kept opaque
    #[serde(
        default,
        rename = "inlineData",
        skip_serializing_if = "Option::is_none"
    )]
    pub inline_data: Option<serde_json::Value>,

    /// File reference (`fileData`); kept opaque
    #[serde(default, rename = "fileData", skip_serializing_if = "Option::is_none")]
    pub file_data: Option<serde_json::Value>,
}

/// A Gemini tool call: the tool name plus its argument object
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GeminiFunctionCall {
    /// Tool name
    #[serde(default)]
    pub name: String,

    /// Argument object as the model produced it
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<serde_json::Value>,
}

/// A Gemini tool result.
///
/// The API allows either a `response` object or a `parts` array; both are kept
/// raw because the response is caller-defined.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GeminiFunctionResponse {
    /// Tool name this result belongs to
    #[serde(default)]
    pub name: String,

    /// Result object
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response: Option<serde_json::Value>,
}

/// Gemini `generationConfig` sampling and output controls
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GeminiGenerationConfig {
    /// Sampling temperature
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,

    /// Output token cap
    #[serde(
        default,
        rename = "maxOutputTokens",
        skip_serializing_if = "Option::is_none"
    )]
    pub max_output_tokens: Option<u32>,

    /// Nucleus sampling
    #[serde(default, rename = "topP", skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f64>,

    /// Top-k sampling
    #[serde(default, rename = "topK", skip_serializing_if = "Option::is_none")]
    pub top_k: Option<u32>,

    /// Number of candidates to return
    #[serde(
        default,
        rename = "candidateCount",
        skip_serializing_if = "Option::is_none"
    )]
    pub candidate_count: Option<u32>,

    /// Stop sequences
    #[serde(
        default,
        rename = "stopSequences",
        skip_serializing_if = "Option::is_none"
    )]
    pub stop_sequences: Option<Vec<String>>,
}

/// A Gemini `generateContent` response body
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GeminiResponse {
    /// Provider response id, when the gateway sends one
    #[serde(
        default,
        rename = "responseId",
        skip_serializing_if = "Option::is_none"
    )]
    pub response_id: Option<String>,

    /// Model version the provider actually served
    #[serde(
        default,
        rename = "modelVersion",
        skip_serializing_if = "Option::is_none"
    )]
    pub model_version: Option<String>,

    /// One entry per candidate
    #[serde(default)]
    pub candidates: Vec<GeminiCandidate>,

    /// Token counters
    #[serde(
        default,
        rename = "usageMetadata",
        skip_serializing_if = "Option::is_none"
    )]
    pub usage_metadata: Option<GeminiUsageMetadata>,
}

/// One Gemini response candidate
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GeminiCandidate {
    /// Completion content
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<GeminiContent>,

    /// Why the candidate stopped (`STOP`, `MAX_TOKENS`, `SAFETY`, ...)
    #[serde(
        default,
        rename = "finishReason",
        skip_serializing_if = "Option::is_none"
    )]
    pub finish_reason: Option<String>,

    /// Candidate position in the response
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index: Option<u32>,
}

impl GeminiResponse {
    /// Finish reason of the last candidate, when it reports one.
    ///
    /// Gemini answers with one candidate unless `candidateCount > 1`; the
    /// pipeline treats the last choice as decisive, matching the SSE
    /// aggregation that takes the final chunk's reason.
    pub fn finish_reason(&self) -> Option<&str> {
        self.candidates
            .iter()
            .rev()
            .find_map(|candidate| candidate.finish_reason.as_deref())
            .filter(|reason| !reason.is_empty())
    }
}

/// Gemini `usageMetadata` counters
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GeminiUsageMetadata {
    /// Prompt tokens (`promptTokenCount`)
    #[serde(default, rename = "promptTokenCount")]
    pub prompt_token_count: u64,

    /// Generated tokens (`candidatesTokenCount`)
    #[serde(default, rename = "candidatesTokenCount")]
    pub candidates_token_count: u64,

    /// Total billed tokens (`totalTokenCount`)
    #[serde(default, rename = "totalTokenCount")]
    pub total_token_count: u64,

    /// Prompt tokens served from a cache (`cachedContentTokenCount`)
    #[serde(
        default,
        rename = "cachedContentTokenCount",
        skip_serializing_if = "Option::is_none"
    )]
    pub cached_content_token_count: Option<u64>,
}

// ============================================================================
// GeminiParser
// ============================================================================

/// Parser for Google's Gemini `generateContent` API
pub struct GeminiParser;

impl GeminiParser {
    /// Returns `true` if the path is a Gemini inference action.
    ///
    /// Delegates to [`MessageParser::gemini_model_from_path`] so the parser
    /// gate and `detect_provider`/`is_llm_api_path` cannot drift apart:
    /// `:countTokens` shares the URL shape but is not an inference call.
    ///
    /// [`MessageParser::gemini_model_from_path`]: super::MessageParser::gemini_model_from_path
    pub fn matches_path(path: &str) -> bool {
        super::MessageParser::gemini_model_from_path(path).is_some()
    }

    /// Parse a Gemini request body.
    ///
    /// `model` and `stream` cannot be read from the body; the caller supplies
    /// them from the request path. Returns `None` unless the body carries the
    /// `contents` array every generation request needs.
    pub fn parse_request(
        body: &serde_json::Value,
        model: &str,
        stream: bool,
    ) -> Option<GeminiRequest> {
        if !body
            .get("contents")
            .is_some_and(serde_json::Value::is_array)
        {
            log::trace!("Gemini request has no contents array");
            return None;
        }
        let mut request: GeminiRequest = serde_json::from_value(body.clone())
            .map_err(|e| log::trace!("Failed to parse Gemini request: {e}"))
            .ok()?;
        request.model = model.to_string();
        request.stream = stream;
        log::debug!(
            "Parsed Gemini request: model={}, contents={}, stream={}",
            request.model,
            request.contents.len(),
            request.stream,
        );
        Some(request)
    }

    /// Parse a Gemini response body.
    ///
    /// Accepts the single-object answer of `:generateContent` and the JSON
    /// array of chunk payloads that `parse_by_path_with_sse` reduces a
    /// `:streamGenerateContent` stream to.
    pub fn parse_response(body: &serde_json::Value) -> Option<GeminiResponse> {
        if let Some(chunks) = body.as_array() {
            return Self::aggregate_sse_chunks(chunks);
        }
        if body.get("candidates").is_none() && body.get("usageMetadata").is_none() {
            return None;
        }
        let response: GeminiResponse = serde_json::from_value(body.clone())
            .map_err(|e| log::trace!("Failed to parse Gemini response: {e}"))
            .ok()?;
        log::debug!(
            "Parsed Gemini response: candidates={}, finish_reason={:?}",
            response.candidates.len(),
            response.finish_reason(),
        );
        Some(response)
    }

    /// Merge the chunk payloads of a `streamGenerateContent` stream.
    ///
    /// Each chunk repeats the response envelope with one increment: text parts
    /// carry the delta (`parts[].text`), function calls arrive as whole
    /// objects, only the terminal chunk carries a `finishReason`, and
    /// `usageMetadata` is a cumulative snapshot. Candidates are merged by
    /// `index` (position when absent) so a multi-candidate stream keeps its
    /// choices apart. Returns `None` when no chunk is Gemini-shaped, leaving
    /// the other providers' parsers to try.
    fn aggregate_sse_chunks(chunks: &[serde_json::Value]) -> Option<GeminiResponse> {
        let mut merged = GeminiResponse::default();
        let mut saw_gemini = false;

        for chunk in chunks {
            if merged.response_id.is_none() {
                merged.response_id = chunk
                    .get("responseId")
                    .and_then(serde_json::Value::as_str)
                    .filter(|id| !id.is_empty())
                    .map(str::to_string);
            }
            if let Some(version) = chunk
                .get("modelVersion")
                .and_then(serde_json::Value::as_str)
                .filter(|version| !version.is_empty())
            {
                merged.model_version = Some(version.to_string());
            }
            if let Some(usage) = chunk.get("usageMetadata") {
                saw_gemini = true;
                merged.usage_metadata = serde_json::from_value(usage.clone())
                    .ok()
                    .or(merged.usage_metadata);
            }
            let Some(candidates) = chunk.get("candidates").and_then(|c| c.as_array()) else {
                continue;
            };
            saw_gemini = true;
            for (position, candidate) in candidates.iter().enumerate() {
                let index = candidate
                    .get("index")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(position as u64) as usize;
                while merged.candidates.len() <= index {
                    merged.candidates.push(GeminiCandidate::default());
                }
                let slot = &mut merged.candidates[index];
                if slot.index.is_none() {
                    slot.index = Some(index as u32);
                }
                if let Some(reason) = candidate
                    .get("finishReason")
                    .and_then(serde_json::Value::as_str)
                    .filter(|reason| !reason.is_empty())
                {
                    slot.finish_reason = Some(reason.to_string());
                }
                if let Some(role) = candidate
                    .get("content")
                    .and_then(|content| content.get("role"))
                    .and_then(serde_json::Value::as_str)
                {
                    let entry = slot.content.get_or_insert_with(GeminiContent::default);
                    if entry.role.is_none() {
                        entry.role = Some(role.to_string());
                    }
                }
                let Some(parts) = candidate
                    .get("content")
                    .and_then(|content| content.get("parts"))
                    .and_then(serde_json::Value::as_array)
                else {
                    continue;
                };
                let entry = slot.content.get_or_insert_with(GeminiContent::default);
                for part in parts {
                    if let Ok(part) = serde_json::from_value::<GeminiPart>(part.clone()) {
                        entry.parts.push(part);
                    }
                }
            }
        }

        if !saw_gemini {
            return None;
        }
        // A slot only allocated to hold a position with no content and no
        // finish reason of its own is not an answer.
        merged
            .candidates
            .retain(|candidate| candidate.content.is_some() || candidate.finish_reason.is_some());
        Some(merged)
    }
}

impl Default for GeminiParser {
    fn default() -> Self {
        Self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_the_generation_actions_only() {
        assert!(GeminiParser::matches_path(
            "/v1beta/models/gemini-2.5-pro:generateContent"
        ));
        assert!(GeminiParser::matches_path(
            "/v1beta/models/gemini-2.5-pro:streamGenerateContent?alt=sse"
        ));
        assert!(GeminiParser::matches_path(
            "/v1/projects/p/locations/us-central1/publishers/google/models/gemini-2.5-pro:generateContent"
        ));
        // Token counting shares the URL shape but is not an inference call.
        assert!(!GeminiParser::matches_path(
            "/v1beta/models/gemini-2.5-pro:countTokens"
        ));
        assert!(!GeminiParser::matches_path("/v1/chat/completions"));
    }

    #[test]
    fn parses_a_request_with_contents_system_and_tools() {
        let body = serde_json::json!({
            "systemInstruction": {"parts": [{"text": "Be concise"}]},
            "contents": [
                {"role": "user", "parts": [{"text": "Hi"}]},
                {"role": "model", "parts": [{"functionCall": {"name": "lookup", "args": {"q": "x"}}}]},
                {"role": "user", "parts": [{"functionResponse": {"name": "lookup", "response": {"ok": true}}}]}
            ],
            "generationConfig": {
                "temperature": 0.2,
                "maxOutputTokens": 64,
                "topP": 0.9,
                "topK": 5,
                "candidateCount": 1,
                "stopSequences": ["END"]
            }
        });

        let request = GeminiParser::parse_request(&body, "gemini-2.5-pro", false)
            .expect("Gemini request parses");

        assert_eq!(request.model, "gemini-2.5-pro");
        assert!(!request.stream);
        assert_eq!(request.contents.len(), 3);
        assert_eq!(request.contents[0].role.as_deref(), Some("user"));
        assert_eq!(request.contents[0].parts[0].text.as_deref(), Some("Hi"));
        let call = request.contents[1].parts[0]
            .function_call
            .as_ref()
            .expect("functionCall is kept");
        assert_eq!(call.name, "lookup");
        assert_eq!(call.args.as_ref(), Some(&serde_json::json!({"q": "x"})));
        let result = request.contents[2].parts[0]
            .function_response
            .as_ref()
            .expect("functionResponse is kept");
        assert_eq!(result.name, "lookup");
        assert_eq!(
            result.response.as_ref(),
            Some(&serde_json::json!({"ok": true}))
        );
        let system = request
            .system_instruction
            .as_ref()
            .expect("systemInstruction is kept");
        assert_eq!(system.parts[0].text.as_deref(), Some("Be concise"));
        let config = request
            .generation_config
            .as_ref()
            .expect("generationConfig is kept");
        assert_eq!(config.temperature, Some(0.2));
        assert_eq!(config.max_output_tokens, Some(64));
        assert_eq!(config.top_p, Some(0.9));
        assert_eq!(config.top_k, Some(5));
        assert_eq!(config.candidate_count, Some(1));
        assert_eq!(
            config.stop_sequences.as_deref(),
            Some(&["END".to_string()][..])
        );
    }

    #[test]
    fn a_body_without_contents_is_not_a_gemini_request() {
        let body = serde_json::json!({"model": "gpt-4", "messages": []});
        assert!(GeminiParser::parse_request(&body, "gemini-2.5-pro", false).is_none());
        // A `contents` field of the wrong shape must not be claimed either.
        let body = serde_json::json!({"contents": "hello"});
        assert!(GeminiParser::parse_request(&body, "gemini-2.5-pro", false).is_none());
    }

    #[test]
    fn parses_a_non_streaming_response() {
        let body = serde_json::json!({
            "candidates": [{
                "content": {"role": "model", "parts": [
                    {"text": "4"},
                    {"functionCall": {"name": "lookup", "args": {"q": "x"}}}
                ]},
                "finishReason": "STOP",
                "index": 0
            }],
            "usageMetadata": {
                "promptTokenCount": 10,
                "candidatesTokenCount": 2,
                "totalTokenCount": 12
            },
            "responseId": "resp-gemini",
            "modelVersion": "gemini-2.5-flash"
        });

        let response = GeminiParser::parse_response(&body).expect("Gemini response parses");

        assert_eq!(response.response_id.as_deref(), Some("resp-gemini"));
        assert_eq!(response.model_version.as_deref(), Some("gemini-2.5-flash"));
        assert_eq!(response.finish_reason(), Some("STOP"));
        let content = response.candidates[0]
            .content
            .as_ref()
            .expect("candidate content");
        assert_eq!(content.parts[0].text.as_deref(), Some("4"));
        assert_eq!(
            content.parts[1]
                .function_call
                .as_ref()
                .expect("function call")
                .name,
            "lookup"
        );
        let usage = response.usage_metadata.expect("usage");
        assert_eq!(usage.prompt_token_count, 10);
        assert_eq!(usage.candidates_token_count, 2);
        assert_eq!(usage.total_token_count, 12);
    }

    #[test]
    fn the_last_candidate_finish_reason_is_decisive() {
        let body = serde_json::json!({"candidates": [
            {"content": {"parts": [{"text": "a"}]}, "finishReason": "SAFETY", "index": 0},
            {"content": {"parts": [{"text": "b"}]}, "finishReason": "STOP", "index": 1}
        ]});
        let response = GeminiParser::parse_response(&body).expect("response");
        assert_eq!(response.finish_reason(), Some("STOP"));
    }

    #[test]
    fn aggregates_streaming_chunks_into_one_response() {
        let chunks = serde_json::json!([
            {"candidates": [{"content": {"role": "model", "parts": [{"text": "hel"}]}, "index": 0}]},
            {"candidates": [{"content": {"role": "model", "parts": [{"text": "lo"}]}, "index": 0}]},
            {"candidates": [{"content": {"role": "model", "parts": [
                {"functionCall": {"name": "lookup", "args": {"q": "x"}}}
            ]}, "finishReason": "STOP", "index": 0}],
             "usageMetadata": {"promptTokenCount": 9, "candidatesTokenCount": 2, "totalTokenCount": 11},
             "responseId": "resp-stream"}
        ]);

        let response = GeminiParser::parse_response(&chunks).expect("stream aggregates");

        assert_eq!(response.candidates.len(), 1);
        assert_eq!(response.finish_reason(), Some("STOP"));
        let parts = &response.candidates[0]
            .content
            .as_ref()
            .expect("merged content")
            .parts;
        let text: String = parts
            .iter()
            .filter_map(|part| part.text.as_deref())
            .collect();
        assert_eq!(text, "hello");
        assert_eq!(
            parts[2]
                .function_call
                .as_ref()
                .map(|call| call.name.as_str()),
            Some("lookup")
        );
        assert_eq!(
            response.usage_metadata.expect("usage").prompt_token_count,
            9
        );
        assert_eq!(response.response_id.as_deref(), Some("resp-stream"));
    }

    #[test]
    fn a_stream_keeps_its_candidates_apart_and_takes_the_last_reason() {
        let chunks = serde_json::json!([
            {"candidates": [
                {"content": {"parts": [{"text": "a"}]}, "index": 0},
                {"content": {"parts": [{"text": "b"}]}, "index": 1}
            ]},
            {"candidates": [
                {"content": {"parts": [{"text": "!"}]}, "finishReason": "SAFETY", "index": 0},
                {"content": {"parts": [{"text": "?"}]}, "finishReason": "STOP", "index": 1}
            ]}
        ]);

        let response = GeminiParser::parse_response(&chunks).expect("stream aggregates");
        assert_eq!(response.candidates.len(), 2);
        assert_eq!(
            response.finish_reason(),
            Some("STOP"),
            "the last candidate's reason is decisive"
        );
        let texts: Vec<String> = response
            .candidates
            .iter()
            .map(|candidate| {
                candidate
                    .content
                    .as_ref()
                    .map(|content| {
                        content
                            .parts
                            .iter()
                            .filter_map(|part| part.text.as_deref())
                            .collect()
                    })
                    .unwrap_or_default()
            })
            .collect();
        assert_eq!(texts, vec!["a!".to_string(), "b?".to_string()]);
    }

    #[test]
    fn a_foreign_body_is_not_a_gemini_response() {
        assert!(GeminiParser::parse_response(&serde_json::json!({"id": "chatcmpl-1"})).is_none());
        assert!(GeminiParser::parse_response(&serde_json::json!([{"id": "1"}])).is_none());
    }
}
