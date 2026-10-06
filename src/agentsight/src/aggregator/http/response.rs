//! Aggregated HTTP Response types
//
//! This module defines the aggregated response structure that combines
//! parsed response data with aggregation metadata.

use crate::chrome_trace::{ChromeTraceEvent, ToChromeTraceEvent, TraceArgs, ns_to_us};
use crate::parser::http::ParsedResponse;
use crate::parser::sse::ParsedSseEvent;
use serde_json::json;

/// Aggregated HTTP Response with metadata
#[derive(Debug, Clone)]
pub struct AggregatedResponse {
    /// Parsed response data
    pub parsed: ParsedResponse,
    /// Last captured byte timestamp for a completed non-SSE response.
    pub completion_timestamp_ns: Option<u64>,
    /// SSE events collected during streaming (if is_sse is true)
    pub sse_events: Vec<ParsedSseEvent>,
    /// Raw bytes that arrived as RawData while in SseActive state. These are
    /// continuation chunks of an oversized SSE event (e.g. OpenAI Responses
    /// API's `response.completed` echoing the full system prompt + tools)
    /// whose first chunk parsed as a (truncated) SseEvent. Used by downstream
    /// extractors to recover token usage embedded past the truncation point.
    pub sse_continuation_bytes: Option<Vec<u8>>,
}

impl AggregatedResponse {
    /// Create from ParsedResponse
    pub fn from_parsed(parsed: ParsedResponse) -> Self {
        AggregatedResponse {
            parsed,
            completion_timestamp_ns: None,
            sse_events: Vec::new(),
            sse_continuation_bytes: None,
        }
    }

    pub fn body(&self) -> &[u8] {
        self.parsed.body()
    }

    pub fn body_string(&self) -> String {
        let first = std::str::from_utf8(self.body()).unwrap_or("");
        let sse_body: String = self
            .sse_events
            .iter()
            .map(|event| event.body_str())
            .collect::<Vec<_>>()
            .join("");
        if first.is_empty() {
            sse_body
        } else if sse_body.is_empty() {
            first.to_string()
        } else {
            format!("{first}{sse_body}")
        }
    }

    /// Get JSON bodies from SSE events, aggregated into a Vec
    ///
    /// Parses each SSE event's payload as JSON and collects into a Vec,
    /// skipping events that are not valid JSON (e.g., `[DONE]` markers).
    pub fn json_body(&self) -> Vec<serde_json::Value> {
        self.sse_events
            .iter()
            .filter_map(|event| event.json_body())
            .collect()
    }

    /// Get start timestamp (first packet) in nanoseconds
    /// Derived from parsed.source_event.timestamp_ns
    pub fn start_timestamp_ns(&self) -> u64 {
        self.parsed.source_event.timestamp_ns
    }

    /// Get end timestamp (last packet) in nanoseconds
    /// Uses the final body fragment for non-SSE responses.
    pub fn end_timestamp_ns(&self) -> u64 {
        self.sse_events
            .last()
            .map(|e| e.source_event().timestamp_ns)
            .or(self.completion_timestamp_ns)
            .unwrap_or_else(|| self.start_timestamp_ns())
    }

    /// Returns the timestamp of the first SSE event carrying model output.
    ///
    /// Lifecycle and usage events are skipped because they do not represent a
    /// token becoming available to the caller. Text, reasoning, and tool-call
    /// deltas all count as output because providers include them in output-token
    /// accounting.
    pub fn first_output_timestamp_ns(&self) -> Option<u64> {
        self.sse_events
            .iter()
            .find(|event| event_has_meaningful_output(event.json_body().as_ref()))
            .map(|event| event.source_event().timestamp_ns)
    }

    /// Get duration in nanoseconds
    pub fn duration_ns(&self) -> u64 {
        self.end_timestamp_ns()
            .saturating_sub(self.start_timestamp_ns())
    }

    /// Get status code
    pub fn status_code(&self) -> u16 {
        self.parsed.status_code
    }

    /// Get reason phrase
    pub fn reason(&self) -> &str {
        &self.parsed.reason
    }

    /// Check if SSE response
    pub fn is_sse(&self) -> bool {
        self.parsed.is_sse()
    }

    /// Get process ID
    pub fn pid(&self) -> u32 {
        self.parsed.source_event.pid
    }

    /// Get thread ID
    pub fn tid(&self) -> u32 {
        self.parsed.source_event.tid
    }

    /// Get SSE event count
    pub fn sse_event_count(&self) -> usize {
        self.sse_events.len()
    }

    /// Add SSE event
    pub fn add_sse_event(&mut self, event: ParsedSseEvent) {
        self.sse_events.push(event);
    }

    /// Set SSE events (replace existing)
    pub fn set_sse_events(&mut self, events: Vec<ParsedSseEvent>) {
        self.sse_events = events;
    }

    /// Get combined SSE data
    pub fn combined_sse_data(&self) -> String {
        self.sse_events
            .iter()
            .map(|e| String::from_utf8_lossy(e.data()).to_string())
            .collect::<Vec<_>>()
            .join("")
    }
}

fn non_empty_string(value: Option<&serde_json::Value>) -> bool {
    value
        .and_then(serde_json::Value::as_str)
        .is_some_and(|value| !value.is_empty())
}

pub(crate) fn event_has_meaningful_output(value: Option<&serde_json::Value>) -> bool {
    let Some(value) = value else {
        return false;
    };

    if let Some(event_type) = value.get("type").and_then(serde_json::Value::as_str) {
        match event_type {
            "response.output_text.delta"
            | "response.reasoning_text.delta"
            | "response.reasoning_summary_text.delta"
            | "response.function_call_arguments.delta" => {
                return non_empty_string(value.get("delta"));
            }
            "content_block_delta" => {
                let delta = value.get("delta");
                return non_empty_string(delta.and_then(|item| item.get("text")))
                    || non_empty_string(delta.and_then(|item| item.get("thinking")))
                    || non_empty_string(delta.and_then(|item| item.get("partial_json")));
            }
            "content_block_start" => {
                let block = value.get("content_block");
                return non_empty_string(block.and_then(|item| item.get("text")))
                    || non_empty_string(block.and_then(|item| item.get("thinking")))
                    || (block.and_then(|item| item.get("type"))
                        == Some(&serde_json::Value::String("tool_use".to_string()))
                        && non_empty_string(block.and_then(|item| item.get("name"))));
            }
            "response.output_item.added" => {
                let item = value.get("item");
                return item.and_then(|item| item.get("type"))
                    == Some(&serde_json::Value::String("function_call".to_string()))
                    && non_empty_string(item.and_then(|item| item.get("name")));
            }
            _ => {}
        }
    }

    // DashScope/Bailian native envelope: the payload is nested under `output`
    // (`output.text`, or `output.choices[].message` carrying content, reasoning
    // or whole tool calls), so the top-level `choices` walk below never sees
    // it. Native streams are parsed and stored like any other protocol, so
    // without this branch their first-output timestamp stayed empty.
    if let Some(output) = value.get("output").filter(|o| o.is_object()) {
        if non_empty_string(output.get("text")) {
            return true;
        }
        if let Some(choices) = output.get("choices").and_then(serde_json::Value::as_array) {
            return choices.iter().any(|choice| {
                let Some(message) = choice.get("message") else {
                    return false;
                };
                let has_content = message.get("content").is_some_and(|content| {
                    non_empty_string(Some(content))
                        || content.as_array().is_some_and(|blocks| {
                            blocks.iter().any(|b| non_empty_string(b.get("text")))
                        })
                });
                has_content
                    || non_empty_string(message.get("reasoning_content"))
                    || message
                        .get("tool_calls")
                        .and_then(serde_json::Value::as_array)
                        .is_some_and(|calls| {
                            calls.iter().any(|call| {
                                let function = call.get("function");
                                non_empty_string(function.and_then(|f| f.get("name")))
                                    || non_empty_string(function.and_then(|f| f.get("arguments")))
                            })
                        })
            });
        }
    }

    // Gemini streamGenerateContent: the payload rides under
    // `candidates[].content.parts[]` — text as `{"text": ...}` parts, tool
    // calls as `{"functionCall": {"name": ...}}` parts — and usage under
    // `usageMetadata`. None of the shapes above match it, so a Gemini
    // stream's first answer token never dated the stream and its TTFT
    // stayed empty.
    if let Some(candidates) = value
        .get("candidates")
        .and_then(serde_json::Value::as_array)
    {
        return candidates.iter().any(|candidate| {
            let Some(parts) = candidate
                .get("content")
                .and_then(|content| content.get("parts"))
                .and_then(serde_json::Value::as_array)
            else {
                return false;
            };
            parts.iter().any(|part| {
                non_empty_string(part.get("text"))
                    || part
                        .get("functionCall")
                        .is_some_and(|call| non_empty_string(call.get("name")))
                    // Gemini image generation streams the answer as inline
                    // image parts (`inlineData.data`) and code execution
                    // streams the model-authored code as `executableCode.code`
                    // — both are the first content-bearing output of the
                    // answer. `codeExecutionResult` is sandbox tool output,
                    // not model output, and stays non-dating.
                    || part
                        .get("inlineData")
                        .is_some_and(|data| non_empty_string(data.get("data")))
                    || part
                        .get("executableCode")
                        .is_some_and(|code| non_empty_string(code.get("code")))
            })
        });
    }

    value
        .get("choices")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|choices| {
            choices.iter().any(|choice| {
                let delta = choice.get("delta");
                // SysOM/Bailian Copilot streams accumulated `choices[].message`
                // chunks instead of `choices[].delta`, so its first answer
                // token never dated the stream and the call had no TTFT.
                let message = choice.get("message");
                non_empty_string(choice.get("text"))
                    || non_empty_string(message.and_then(|item| item.get("content")))
                    || non_empty_string(delta.and_then(|item| item.get("content")))
                    || non_empty_string(delta.and_then(|item| item.get("reasoning_content")))
                    || delta
                        .and_then(|item| item.get("tool_calls"))
                        .and_then(serde_json::Value::as_array)
                        .is_some_and(|calls| {
                            calls.iter().any(|call| {
                                let function = call.get("function");
                                non_empty_string(function.and_then(|item| item.get("name")))
                                    || non_empty_string(
                                        function.and_then(|item| item.get("arguments")),
                                    )
                            })
                        })
                    || message
                        .and_then(|item| item.get("tool_use"))
                        .and_then(serde_json::Value::as_array)
                        .is_some_and(|calls| {
                            calls.iter().any(|call| {
                                let function = call.get("function");
                                non_empty_string(function.and_then(|item| item.get("name")))
                                    || non_empty_string(
                                        function.and_then(|item| item.get("arguments")),
                                    )
                            })
                        })
            })
        })
}

impl TraceArgs for AggregatedResponse {
    fn to_trace_args(&self) -> serde_json::Value {
        let mut args = serde_json::Map::new();

        args.insert("status_code".to_string(), json!(self.parsed.status_code));
        args.insert("reason".to_string(), json!(&self.parsed.reason));
        args.insert(
            "version".to_string(),
            json!(format!("HTTP/1.{}", self.parsed.version)),
        );

        // Add headers if present
        if !self.parsed.headers.is_empty() {
            args.insert("headers".to_string(), json!(&self.parsed.headers));
        }

        if self.parsed.is_sse() {
            args.insert("is_sse".to_string(), json!(true));
            if !self.sse_events.is_empty() {
                args.insert("sse_event_count".to_string(), json!(self.sse_events.len()));

                // Add SSE events data
                let events_data: Vec<_> = self
                    .sse_events
                    .iter()
                    .map(|e| {
                        let data_str = String::from_utf8_lossy(e.data()).to_string();
                        serde_json::json!({
                            "id": &e.id,
                            "event": &e.event,
                            "data": data_str,
                        })
                    })
                    .collect();
                args.insert("sse_events".to_string(), json!(events_data));
            }
        }

        if self.parsed.body_len > 0 && !self.parsed.is_sse() {
            args.insert("body_length".to_string(), json!(self.parsed.body_len));

            // Try to parse as JSON first (with gzip decompression), fallback to decompressed string
            if let Some(json_body) = self.parsed.json_body() {
                args.insert("body".to_string(), json_body);
            } else {
                let body_str = self.parsed.body_str_decompressed();
                if !body_str.is_empty() {
                    args.insert("body".to_string(), json!(body_str));
                }
            }
        }

        serde_json::Value::Object(args)
    }
}

impl ToChromeTraceEvent for AggregatedResponse {
    fn to_chrome_trace_events(&self) -> Vec<ChromeTraceEvent> {
        let ts_us = ns_to_us(self.start_timestamp_ns());
        let dur_us = ns_to_us(self.duration_ns());

        // Minimum duration: 10ms = 10,000 microseconds
        const MIN_DUR_US: u64 = 10_000;
        let dur_us = dur_us.max(MIN_DUR_US);

        let event = ChromeTraceEvent::complete(
            format!("{} {}", self.parsed.status_code, self.parsed.reason),
            "http.response",
            self.parsed.source_event.pid,
            self.parsed.source_event.tid as u64,
            ts_us,
            dur_us,
        )
        .with_trace_args(self);

        vec![event]
    }
}

#[cfg(test)]
mod latency_tests {
    use super::{AggregatedResponse, event_has_meaningful_output};
    use crate::parser::http::ParsedResponse;
    use crate::parser::sse::ParsedSseEvent;
    use crate::probes::sslsniff::SslEvent;
    use std::collections::HashMap;
    use std::rc::Rc;

    fn ssl_event(data: &str, timestamp_ns: u64) -> Rc<SslEvent> {
        Rc::new(SslEvent {
            source: 0,
            timestamp_ns,
            delta_ns: 0,
            pid: 1,
            tid: 1,
            uid: 0,
            len: data.len() as u32,
            rw: 0,
            comm: String::new(),
            buf: data.as_bytes().to_vec(),
            is_handshake: false,
            ssl_ptr: 0,
        })
    }

    fn sse_event(data: &str, timestamp_ns: u64) -> ParsedSseEvent {
        ParsedSseEvent::new(
            None,
            None,
            None,
            0,
            data.len(),
            ssl_event(data, timestamp_ns),
        )
    }

    fn response_with_sse_events(sse_events: Vec<ParsedSseEvent>) -> AggregatedResponse {
        AggregatedResponse {
            completion_timestamp_ns: None,
            parsed: ParsedResponse {
                version: 1,
                status_code: 200,
                reason: "OK".to_string(),
                headers: HashMap::new(),
                body_offset: 0,
                body_len: 0,
                source_event: ssl_event("", 10),
            },
            sse_events,
            sse_continuation_bytes: None,
        }
    }

    #[test]
    fn skips_openai_metadata_before_output_delta() {
        let created = serde_json::json!({"type": "response.created"});
        let empty = serde_json::json!({"type": "response.output_text.delta", "delta": ""});
        let output = serde_json::json!({
            "type": "response.output_text.delta",
            "delta": "hello"
        });
        assert!(!event_has_meaningful_output(Some(&created)));
        assert!(!event_has_meaningful_output(Some(&empty)));
        assert!(event_has_meaningful_output(Some(&output)));
    }

    #[test]
    fn recognizes_anthropic_and_chat_completion_output() {
        let anthropic = serde_json::json!({
            "type": "content_block_delta",
            "delta": {"type": "text_delta", "text": "hello"}
        });
        let chat = serde_json::json!({
            "choices": [{"delta": {"content": "hello"}}]
        });
        assert!(event_has_meaningful_output(Some(&anthropic)));
        assert!(event_has_meaningful_output(Some(&chat)));
    }

    /// The DashScope/Bailian native envelope nests its payload under `output`
    /// (`output.text` or `output.choices[].message`), so the top-level
    /// `choices` walk never saw it: a native stream produced no first-output
    /// timestamp, and its TTFT was missing from the latency stats.
    #[test]
    fn recognizes_dashscope_native_output() {
        let content = serde_json::json!({
            "output": {"choices": [{"finish_reason": "null",
                                    "message": {"role": "assistant", "content": "你"}}]},
            "usage": {"input_tokens": 10, "output_tokens": 1, "total_tokens": 11}
        });
        let text_format = serde_json::json!({
            "output": {"finish_reason": "null", "text": "1, 2"}
        });
        let tool_call = serde_json::json!({
            "output": {"choices": [{"finish_reason": "null",
                                    "message": {"content": "",
                                                "tool_calls": [{"index": 0, "id": "call_1",
                                                                "type": "function",
                                                                "function": {"name": "get_weather",
                                                                             "arguments": ""}}]}}]}
        });
        let usage_only = serde_json::json!({"output": {"finish_reason": "null", "text": ""}});

        assert!(event_has_meaningful_output(Some(&content)));
        assert!(event_has_meaningful_output(Some(&text_format)));
        assert!(event_has_meaningful_output(Some(&tool_call)));
        assert!(!event_has_meaningful_output(Some(&usage_only)));
    }

    #[test]
    fn recognizes_sysom_cumulative_message_output() {
        // The SysOM/Copilot endpoint streams `choices[].message` chunks whose
        // content grows as the answer is produced; it never sends `delta`.
        let chunk = serde_json::json!({
            "choices": [{"message": {"content": "Hel", "tool_use": null}}]
        });
        assert!(event_has_meaningful_output(Some(&chunk)));

        // A chunk that only opens the turn carries no output yet.
        let opening = serde_json::json!({
            "choices": [{"message": {"content": "", "tool_use": null}}]
        });
        assert!(!event_has_meaningful_output(Some(&opening)));

        // A tool-only turn counts once the call arrives.
        let tool_call = serde_json::json!({
            "choices": [{"message": {"content": "", "tool_use": [
                {"index": 0, "id": "c1", "type": "function",
                 "function": {"name": "read_file", "arguments": "{\"path\":\"/tmp/a\"}"}}]}}]
        });
        assert!(event_has_meaningful_output(Some(&tool_call)));
    }

    #[test]
    fn sysom_stream_dates_its_first_message_chunk() {
        let response = response_with_sse_events(vec![
            sse_event(
                r#"{"choices":[{"message":{"content":"","tool_use":null}}]}"#,
                100,
            ),
            sse_event(
                r#"{"choices":[{"message":{"content":"Hello","tool_use":null}}]}"#,
                200,
            ),
            sse_event(
                r#"{"choices":[{"message":{"content":"Hello there!","tool_use":null}}]}"#,
                300,
            ),
        ]);

        assert_eq!(response.first_output_timestamp_ns(), Some(200));
    }

    #[test]
    fn first_output_timestamp_uses_first_nonempty_output_delta() {
        let response = response_with_sse_events(vec![
            sse_event(r#"{"type":"response.created"}"#, 100),
            sse_event(r#"{"type":"response.output_text.delta","delta":""}"#, 200),
            sse_event(
                r#"{"type":"response.output_text.delta","delta":"hello"}"#,
                300,
            ),
            sse_event(
                r#"{"type":"response.output_text.done","text":"hello world"}"#,
                400,
            ),
            sse_event(
                r#"{"type":"response.output_item.done","item":{"type":"message","content":[{"type":"output_text","text":"hello world"}]}}"#,
                500,
            ),
            sse_event(r#"{"type":"response.completed"}"#, 600),
        ]);

        assert_eq!(response.first_output_timestamp_ns(), Some(300));
    }

    #[test]
    fn first_output_timestamp_is_none_without_observable_delta() {
        let response = response_with_sse_events(vec![
            sse_event(
                r#"{"type":"response.output_text.done","text":"final text"}"#,
                500,
            ),
            sse_event(
                r#"{"type":"response.output_item.done","item":{"type":"message","content":[{"type":"output_text","text":"final text"}]}}"#,
                600,
            ),
            sse_event(r#"{"type":"response.completed"}"#, 700),
        ]);

        assert_eq!(response.first_output_timestamp_ns(), None);
    }

    /// Gemini streamGenerateContent streams its answer as
    /// `candidates[].content.parts[]` — text as `{"text": ...}` parts, tool
    /// calls as `{"functionCall": ...}` parts — so none of the shapes above
    /// matched it: the stream never dated its first output and its TTFT
    /// stayed empty.
    #[test]
    fn recognizes_gemini_candidates_output() {
        let text = serde_json::json!({
            "candidates": [{"content": {"parts": [{"text": "Hel"}], "role": "model"},
                            "index": 0}],
            "usageMetadata": {"promptTokenCount": 10}
        });
        let tool_call = serde_json::json!({
            "candidates": [{"content": {"parts": [
                {"functionCall": {"name": "get_weather", "args": {"city": "X"}}}],
                "role": "model"}}]
        });
        let usage_only = serde_json::json!({
            "candidates": [{"content": {"parts": [{"text": ""}], "role": "model"},
                            "finishReason": "STOP", "index": 0}],
            "usageMetadata": {"promptTokenCount": 10, "candidatesTokenCount": 0}
        });
        let image_part = serde_json::json!({
            "candidates": [{"content": {"parts": [
                {"inlineData": {"mimeType": "image/png", "data": "iVBORw0KGgo="}}],
                "role": "model"}, "index": 0}],
            "usageMetadata": {"promptTokenCount": 10}
        });
        let code_part = serde_json::json!({
            "candidates": [{"content": {"parts": [
                {"executableCode": {"language": "PYTHON", "code": "print(1)"}}],
                "role": "model"}, "index": 0}]
        });
        let empty_inline_data = serde_json::json!({
            "candidates": [{"content": {"parts": [
                {"inlineData": {"mimeType": "image/png", "data": ""}}],
                "role": "model"}, "index": 0}]
        });
        let empty_executable_code = serde_json::json!({
            "candidates": [{"content": {"parts": [
                {"executableCode": {"language": "PYTHON", "code": ""}}],
                "role": "model"}, "index": 0}]
        });
        let code_execution_result = serde_json::json!({
            "candidates": [{"content": {"parts": [
                {"codeExecutionResult": {"outcome": "OUTCOME_OK", "output": "1"}}],
                "role": "model"}, "index": 0}]
        });

        assert!(event_has_meaningful_output(Some(&text)));
        assert!(event_has_meaningful_output(Some(&tool_call)));
        assert!(!event_has_meaningful_output(Some(&usage_only)));
        // Image and model-authored code parts are incremental answer output.
        assert!(event_has_meaningful_output(Some(&image_part)));
        assert!(event_has_meaningful_output(Some(&code_part)));
        // Empty payloads and sandbox tool results stay non-meaningful.
        assert!(!event_has_meaningful_output(Some(&empty_inline_data)));
        assert!(!event_has_meaningful_output(Some(&empty_executable_code)));
        assert!(!event_has_meaningful_output(Some(&code_execution_result)));
    }

    #[test]
    fn gemini_stream_dates_its_first_text_part() {
        let response = response_with_sse_events(vec![
            sse_event(
                r#"{"candidates":[{"content":{"parts":[{"text":""}],"role":"model"},"index":0}]}"#,
                100,
            ),
            sse_event(
                r#"{"candidates":[{"content":{"parts":[{"text":"Hel"}],"role":"model"},"index":0}]}"#,
                200,
            ),
            sse_event(
                r#"{"candidates":[{"content":{"parts":[{"text":"Hello"}],"role":"model"},"index":0}],
                    "usageMetadata":{"promptTokenCount":10,"candidatesTokenCount":5}}"#,
                300,
            ),
        ]);

        assert_eq!(response.first_output_timestamp_ns(), Some(200));
    }
}
