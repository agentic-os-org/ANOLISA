use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use futures::StreamExt;
use serde_json::Value;

use super::profile::{self, ProviderProfile};
use super::{
    ContentGenerator, GenerateConfig, GenerateEvent, GenerateStream, Message, ToolDeclaration,
};

pub struct OpenAICompatProvider {
    pub base_url: String,
    pub api_key: String,
    cancelled: Arc<AtomicBool>,
    profile: Box<dyn ProviderProfile>,
    explicit_cache: bool,
}

impl OpenAICompatProvider {
    pub fn new(
        base_url: &str,
        api_key: &str,
        profile: Box<dyn ProviderProfile>,
        explicit_cache: bool,
    ) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key: api_key.to_string(),
            cancelled: Arc::new(AtomicBool::new(false)),
            profile,
            explicit_cache,
        }
    }

    pub fn new_generic(base_url: &str, api_key: &str) -> Self {
        // explicit_cache is false here because GenericProfile already returns
        // false from supports_cache_control(); the config flag is meaningless
        // for generic endpoints but kept for API symmetry.
        Self::new(base_url, api_key, Box::new(profile::GenericProfile), false)
    }

    fn cache_control_enabled(&self) -> bool {
        self.profile.supports_cache_control() && self.explicit_cache
    }

    /// Inject DashScope prompt-cache markers into the request body.
    ///
    /// Mirrors copilot-shell's `addDashScopeCacheControl` with the `all`
    /// strategy when streaming (system + last message) and `system_only`
    /// when not streaming. Although cosh-core currently hardcodes `stream: true`,
    /// the strategy is selected dynamically from the body so future non-stream
    /// support needs no cache-logic change.
    fn add_cache_control(&self, body: &mut Value) {
        if !self.cache_control_enabled() {
            return;
        }

        let is_stream = body.get("stream").and_then(|v| v.as_bool()).unwrap_or(true);
        let cache_all = is_stream;

        if let Some(messages) = body.get_mut("messages").and_then(|m| m.as_array_mut()) {
            // Track the system message index so we can skip re-marking it
            // when it is also the last message (single-message edge case).
            let mut system_index: Option<usize> = None;

            // System message: always cache.
            for (i, msg) in messages.iter_mut().enumerate() {
                if msg.get("role").and_then(|r| r.as_str()) == Some("system") {
                    add_cache_control_to_message_content(msg);
                    system_index = Some(i);
                    break;
                }
            }

            // Last message: cache only in `all` strategy.
            // Skip if the last message is the same as the system message
            // (already marked above).
            if cache_all {
                if let Some(last_idx) = messages.len().checked_sub(1) {
                    if system_index != Some(last_idx) {
                        if let Some(last_msg) = messages.get_mut(last_idx) {
                            add_cache_control_to_message_content(last_msg);
                        }
                    }
                }
            }
        }

        // Note: DashScope docs explicitly state that cache_control markers
        // can only be placed in messages[].content. Tool definitions
        // already participate in the system prefix cache calculation and
        // do not support independent cache markers.
        // Ref: https://help.aliyun.com/zh/model-studio/context-cache
    }

    fn build_request_body(
        &self,
        messages: &[Message],
        tools: &[ToolDeclaration],
        config: &GenerateConfig,
    ) -> Value {
        let max_tokens_field = self.profile.max_tokens_field();
        let max_tokens = config.max_tokens;
        let mut body = serde_json::json!({
            "model": config.model,
            "messages": messages,
            max_tokens_field: max_tokens,
            "stream": true,
        });

        if let Some(temp) = config.temperature {
            body["temperature"] = serde_json::json!(temp);
        }

        // Only request usage telemetry from backends that accept the field.
        // A generic OpenAI-compatible endpoint may reject unknown
        // `stream_options` and fail the entire turn, so usage stays an
        // enhancement layered on top of the always-present local estimate.
        if config.include_usage && self.profile.supports_stream_usage() {
            body["stream_options"] = serde_json::json!({"include_usage": true});
        }

        if !tools.is_empty() {
            let tool_defs: Vec<Value> = tools
                .iter()
                .map(|t| {
                    serde_json::json!({
                        "type": "function",
                        "function": {
                            "name": t.name,
                            "description": t.description,
                            "parameters": t.parameters,
                        }
                    })
                })
                .collect();
            body["tools"] = serde_json::json!(tool_defs);
        }

        // extra_params is merged before cache markers are injected so that
        // user-supplied fields (e.g. custom `messages` or `tools`) are in
        // place when markers are applied. If extra_params contains a
        // `messages` key it would replace the constructed messages entirely
        // — that is existing behavior and not changed by cache support.
        if let Some(extra) = &config.extra_params {
            if let (Some(body_obj), Some(extra_obj)) = (body.as_object_mut(), extra.as_object()) {
                for (k, v) in extra_obj {
                    body_obj.insert(k.clone(), v.clone());
                }
            }
        }

        self.profile.adjust_request(&mut body);

        self.add_cache_control(&mut body);

        // Last word on the output cap: `extra_params` above may have replaced
        // the resolved `max_tokens` (or introduced the `max_completion_tokens`
        // alias), which would let the wire request outspend the output reserve
        // the compaction budget priced this request against (#2240).
        super::clamp_output_cap_fields(&mut body, config.max_tokens);

        body
    }
}

#[async_trait]
impl ContentGenerator for OpenAICompatProvider {
    async fn generate(
        &self,
        messages: &[Message],
        tools: &[ToolDeclaration],
        config: &GenerateConfig,
    ) -> Result<GenerateStream, String> {
        self.cancelled.store(false, Ordering::SeqCst);
        let body = self.build_request_body(messages, tools, config);
        let url = format!("{}/chat/completions", self.base_url);

        let client = reqwest::Client::new();
        let auth_value = self.profile.auth_header_value(&self.api_key);
        let mut request = client
            .post(&url)
            .header("Authorization", auth_value)
            .header("Content-Type", "application/json");

        if self.cache_control_enabled() {
            request = request.header("X-DashScope-CacheControl", "enable");
        }

        // Report the request before it is encrypted: rustls leaves nothing on the
        // wire an out-of-process observer can read (see `provider::observe`).
        // Serialising twice is acceptable here — this runs once per LLM call, not
        // per chunk — and reusing reqwest's own body would mean building the
        // request in two steps just to peek at it.
        if let Ok(serialised) = serde_json::to_vec(&body) {
            super::observe::tap_request("POST", &url, &serialised);
        }

        let response = request
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("HTTP request failed: {e}"))?;

        // Announce the response before the status check: an error response
        // (401/429/5xx) must still be reported, otherwise the observer is left
        // with a pending request that never completes. The body below is
        // reported through the same chunk tap as a success stream.
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("text/event-stream")
            .to_string();
        super::observe::tap_response_head(response.status().as_u16(), &content_type);

        if !response.status().is_success() {
            let status = response.status();
            let text = response
                .text()
                .await
                .unwrap_or_else(|_| "unknown".to_string());
            super::observe::tap_response_chunk(text.as_bytes());
            return Err(format!("API error {status}: {text}"));
        }

        let cancelled = Arc::clone(&self.cancelled);
        let thinking_field: Option<String> = self.profile.thinking_field().map(|s| s.to_string());
        // With usage reporting enabled, OpenAI-compatible backends deliver the
        // usage payload in a chunk after finish_reason. Defer MessageEnd to
        // [DONE]/stream end so consumers never break before seeing Usage. Only
        // defer when usage was actually requested (a supporting profile);
        // otherwise no usage chunk is coming and MessageEnd must not wait.
        let defer_message_end = config.include_usage && self.profile.supports_stream_usage();
        let byte_stream = response.bytes_stream();
        let buffer = SseByteBuffer::default();
        let sse_event = SseEventBuffer::default();
        let event_queue: Vec<GenerateEvent> = Vec::new();
        let stream_state = OpenAICompatStreamState::default();

        let event_stream = futures::stream::unfold(
            (
                byte_stream,
                buffer,
                sse_event,
                cancelled,
                event_queue,
                thinking_field,
                stream_state,
                false, // exhausted: terminal emitted or byte stream drained
            ),
            move |(
                mut stream,
                mut buf,
                mut sse_event,
                cancelled,
                mut pending,
                thinking_field,
                mut stream_state,
                exhausted,
            )| async move {
                let tf = thinking_field.as_deref();
                loop {
                    if let Some(event) = pending.pop() {
                        // A terminal event popped from the queue seals the
                        // stream too; otherwise a [DONE] still in flight
                        // after e.g. a finish_reason-driven MessageEnd would
                        // produce a second terminal event.
                        let exhausted = exhausted
                            || matches!(
                                event,
                                GenerateEvent::MessageEnd
                                    | GenerateEvent::Error(_)
                                    | GenerateEvent::Cancelled
                            );
                        return Some((
                            event,
                            (
                                stream,
                                buf,
                                sse_event,
                                cancelled,
                                pending,
                                thinking_field,
                                stream_state,
                                exhausted,
                            ),
                        ));
                    }

                    // Never poll the byte stream again once a terminal event
                    // went out or the stream already returned end-of-stream.
                    if exhausted {
                        return None;
                    }

                    if cancelled.load(Ordering::SeqCst) {
                        return Some((
                            GenerateEvent::Cancelled,
                            (
                                stream,
                                buf,
                                sse_event,
                                cancelled,
                                pending,
                                thinking_field,
                                stream_state,
                                true,
                            ),
                        ));
                    }

                    // Extract one complete line (LF, CRLF, or bare CR). Bytes
                    // are buffered raw and decoded per line so a multi-byte
                    // character split across network chunks reassembles
                    // instead of turning into replacement characters that
                    // corrupt the payload.
                    if let Some(line_bytes) = buf.take_line() {
                        let line = match std::str::from_utf8(&line_bytes) {
                            Ok(line) => line,
                            Err(error) => {
                                return Some((
                                    GenerateEvent::Error(format!(
                                        "SSE stream carried invalid UTF-8: {error}"
                                    )),
                                    (
                                        stream,
                                        buf,
                                        sse_event,
                                        cancelled,
                                        pending,
                                        thinking_field,
                                        stream_state,
                                        true,
                                    ),
                                ));
                            }
                        };
                        let data = match sse_event.push_line(line) {
                            Ok(Some(data)) => data,
                            Ok(None) => continue,
                            Err(message) => {
                                return Some((
                                    GenerateEvent::Error(message),
                                    (
                                        stream,
                                        buf,
                                        sse_event,
                                        cancelled,
                                        pending,
                                        thinking_field,
                                        stream_state,
                                        true,
                                    ),
                                ));
                            }
                        };
                        match dispatch_sse_data(&data, tf, defer_message_end, &mut stream_state) {
                            SseDispatch::Done => {
                                return Some((
                                    GenerateEvent::MessageEnd,
                                    (
                                        stream,
                                        buf,
                                        sse_event,
                                        cancelled,
                                        pending,
                                        thinking_field,
                                        stream_state,
                                        true,
                                    ),
                                ));
                            }
                            SseDispatch::Malformed(message) => {
                                return Some((
                                    GenerateEvent::Error(message),
                                    (
                                        stream,
                                        buf,
                                        sse_event,
                                        cancelled,
                                        pending,
                                        thinking_field,
                                        stream_state,
                                        true,
                                    ),
                                ));
                            }
                            SseDispatch::Events(mut events) => {
                                if events.is_empty() {
                                    continue;
                                }
                                let first = events.remove(0);
                                events.reverse();
                                pending = events;
                                // A chunk-parsed terminal (finish_reason in
                                // non-deferred mode) seals the stream here as
                                // well, so a trailing [DONE] cannot emit a
                                // second MessageEnd.
                                let exhausted = matches!(
                                    first,
                                    GenerateEvent::MessageEnd
                                        | GenerateEvent::Error(_)
                                        | GenerateEvent::Cancelled
                                );
                                return Some((
                                    first,
                                    (
                                        stream,
                                        buf,
                                        sse_event,
                                        cancelled,
                                        pending,
                                        thinking_field,
                                        stream_state,
                                        exhausted,
                                    ),
                                ));
                            }
                        }
                    }

                    match stream.next().await {
                        Some(Ok(bytes)) => {
                            // Report the decrypted chunk before it is consumed.
                            super::observe::tap_response_chunk(&bytes);
                            buf.extend(&bytes);
                            // A line is only extracted once its terminator
                            // arrives, so an endless line must be bounded
                            // here before it exhausts memory. Complete lines
                            // still in the buffer drain on the next
                            // iterations, so only a terminator-free overflow
                            // is an error.
                            if buf.overflowed_without_line_ending() {
                                return Some((
                                    GenerateEvent::Error(format!(
                                        "SSE line exceeds the maximum size of \
                                         {MAX_SSE_EVENT_BYTES} bytes"
                                    )),
                                    (
                                        stream,
                                        buf,
                                        sse_event,
                                        cancelled,
                                        pending,
                                        thinking_field,
                                        stream_state,
                                        true,
                                    ),
                                ));
                            }
                        }
                        Some(Err(e)) => {
                            return Some((
                                GenerateEvent::Error(format!("stream error: {e}")),
                                (
                                    stream,
                                    buf,
                                    sse_event,
                                    cancelled,
                                    pending,
                                    thinking_field,
                                    stream_state,
                                    true,
                                ),
                            ));
                        }
                        None => {
                            // Flush a final line missing its trailing newline,
                            // then the event buffer (streams may end without
                            // the blank-line separator).
                            let mut terminal: Option<GenerateEvent> = None;
                            let mut flushed: Option<String> = None;
                            if !buf.is_empty() {
                                let tail = buf.take_tail();
                                match std::str::from_utf8(&tail) {
                                    Ok(line) => match sse_event.push_line(line) {
                                        Ok(data) => flushed = data,
                                        Err(message) => {
                                            terminal = Some(GenerateEvent::Error(message));
                                        }
                                    },
                                    Err(error) => {
                                        terminal = Some(GenerateEvent::Error(format!(
                                            "SSE stream carried invalid UTF-8: {error}"
                                        )));
                                    }
                                }
                            }
                            let mut queued: Vec<GenerateEvent> = Vec::new();
                            if terminal.is_none() {
                                if let Some(data) = flushed.or_else(|| sse_event.take_data()) {
                                    match dispatch_sse_data(
                                        &data,
                                        tf,
                                        defer_message_end,
                                        &mut stream_state,
                                    ) {
                                        SseDispatch::Done => {
                                            terminal = Some(GenerateEvent::MessageEnd);
                                        }
                                        SseDispatch::Malformed(message) => {
                                            terminal = Some(GenerateEvent::Error(message));
                                        }
                                        SseDispatch::Events(events) => queued = events,
                                    }
                                }
                            }
                            // End-of-stream without [DONE] is complete only
                            // when the model already signaled finish_reason;
                            // otherwise the tail was lost in transit and the
                            // partial output must not pass as success.
                            let terminal = terminal.unwrap_or_else(|| {
                                if stream_state.saw_finish_reason {
                                    GenerateEvent::MessageEnd
                                } else {
                                    GenerateEvent::Error(
                                        "SSE stream ended before completion: \
                                         no [DONE] marker or finish_reason received"
                                            .to_string(),
                                    )
                                }
                            });
                            let already_terminal = queued.iter().any(|event| {
                                matches!(event, GenerateEvent::MessageEnd | GenerateEvent::Error(_))
                            });
                            if !already_terminal {
                                queued.push(terminal);
                            }
                            let first = queued.remove(0);
                            queued.reverse();
                            pending = queued;
                            return Some((
                                first,
                                (
                                    stream,
                                    buf,
                                    sse_event,
                                    cancelled,
                                    pending,
                                    thinking_field,
                                    stream_state,
                                    true,
                                ),
                            ));
                        }
                    }
                }
            },
        );

        Ok(Box::pin(event_stream))
    }

    fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }
}

/// Convert a message's string content to an array format and append a
/// `cache_control: { type: "ephemeral" }` marker to the last content part.
///
/// DashScope requires `cache_control` on individual content parts within a
/// message, not on the message itself. When the content is a plain string it
/// must first be wrapped in `[{ "type": "text", "text": "..." }]` so the
/// marker has somewhere to live.
fn add_cache_control_to_message_content(msg: &mut Value) {
    let Some(content) = msg.get_mut("content") else {
        return;
    };

    // Null content (e.g. assistant tool-call messages) has nothing to cache.
    if content.is_null() {
        return;
    }

    // If content is a string, convert to array format.
    if content.is_string() {
        let text = content.as_str().unwrap_or("").to_string();
        *content = serde_json::json!([{
            "type": "text",
            "text": text
        }]);
    }

    // Append cache_control to the last content part.
    if let Some(parts) = content.as_array_mut() {
        if let Some(last_part) = parts.last_mut() {
            // Wrap raw strings in a text object so the marker has a home.
            // Numbers, booleans and null are not cacheable content — skip.
            if last_part.is_string() {
                let text = last_part.as_str().unwrap_or("").to_string();
                *last_part = serde_json::json!({
                    "type": "text",
                    "text": text
                });
            }
            if let Some(obj) = last_part.as_object_mut() {
                obj.insert(
                    "cache_control".to_string(),
                    serde_json::json!({"type": "ephemeral"}),
                );
            }
        }
    }
}

#[derive(Default)]
struct OpenAICompatStreamState {
    argument_deltas_seen: HashSet<u32>,
    started_tool_calls: HashSet<u32>,
    emitted_text: HashMap<u32, String>,
    /// Whether any choice carried a non-null `finish_reason`. A stream that
    /// ends without `[DONE]` is only complete when the model already signaled
    /// completion; otherwise the transport dropped the tail and the turn must
    /// fail loud instead of executing a partially assembled tool call.
    saw_finish_reason: bool,
}

/// Upper bound for one assembled SSE event (and thus for one buffered line).
///
/// Legitimate chat-completion chunks are a few kilobytes; even a full-message
/// snapshot stays far below this. Without a bound, an endpoint that keeps
/// sending `data:` lines while withholding the blank-line separator (or one
/// endless line) would grow the buffers without limit while the socket
/// buffer stays small. Overflow is a stream error, mirroring the other
/// fail-loud corruption paths.
const MAX_SSE_EVENT_BYTES: usize = 8 * 1024 * 1024;

/// Flat per-line charge on top of the payload bytes when accounting an
/// event against [`MAX_SSE_EVENT_BYTES`]. Each buffered line also costs a
/// `String` header, allocator slack, and a join separator, so a stream of
/// empty or tiny `data:` lines must consume the budget too — otherwise the
/// event bound could be bypassed with payload-free lines.
const SSE_LINE_OVERHEAD_BYTES: usize = 64;

/// Buffers raw stream bytes and yields complete lines per the SSE line
/// grammar: CRLF, LF, or a bare CR all terminate a line.
///
/// A bare CR ends its line immediately — postponing it until the next byte
/// would stall a stream that flushes a complete CR-framed event and then
/// idles. When the CR is the last buffered byte, the following chunk may
/// still open with the LF half of a CRLF; `skip_lf` swallows exactly that
/// byte so a chunk-split CRLF does not fabricate an extra empty line (an
/// event boundary).
#[derive(Default)]
struct SseByteBuffer {
    bytes: Vec<u8>,
    skip_lf: bool,
}

impl SseByteBuffer {
    fn extend(&mut self, chunk: &[u8]) {
        self.bytes.extend_from_slice(chunk);
        if self.skip_lf {
            if let Some(&first) = self.bytes.first() {
                if first == b'\n' {
                    self.bytes.remove(0);
                }
                self.skip_lf = false;
            }
        }
    }

    fn take_line(&mut self) -> Option<Vec<u8>> {
        let mut i = 0;
        while i < self.bytes.len() {
            match self.bytes[i] {
                b'\n' => {
                    let mut line: Vec<u8> = self.bytes.drain(..=i).collect();
                    line.truncate(i);
                    return Some(line);
                }
                b'\r' => {
                    let last = if i + 1 < self.bytes.len() {
                        if self.bytes[i + 1] == b'\n' {
                            i + 1
                        } else {
                            i
                        }
                    } else {
                        self.skip_lf = true;
                        i
                    };
                    let mut line: Vec<u8> = self.bytes.drain(..=last).collect();
                    line.truncate(i);
                    return Some(line);
                }
                _ => i += 1,
            }
        }
        None
    }

    /// Drains the remaining bytes at end of stream (a final line missing
    /// its terminator). A pending LF skip was already applied on arrival.
    fn take_tail(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.bytes)
    }

    fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    /// True when the buffer grew past the bound without a single line
    /// ending — an endless line that must fail before exhausting memory.
    /// Complete lines still in the buffer drain on later iterations.
    fn overflowed_without_line_ending(&self) -> bool {
        self.bytes.len() > MAX_SSE_EVENT_BYTES
            && !self.bytes.iter().any(|&b| b == b'\n' || b == b'\r')
    }
}

/// Assembles Server-Sent Events from individual lines.
///
/// The wire format allows `data:` with or without a following space, spreads
/// one event across multiple `data` lines, and terminates each event with a
/// blank line. Matching only `"data: "` (as the previous decoder did) silently
/// drops spec-compliant frames, and a dropped mid-stream `arguments` delta can
/// still concatenate into syntactically valid JSON — a truncated shell command
/// that then executes. Every `data` line therefore has to be captured here.
#[derive(Default)]
struct SseEventBuffer {
    data_lines: Vec<String>,
    buffered_bytes: usize,
    first_line_seen: bool,
}

impl SseEventBuffer {
    /// Feeds one line; returns the joined event data when the blank-line
    /// event boundary is reached, or an error when the accumulated event
    /// exceeds [`MAX_SSE_EVENT_BYTES`].
    fn push_line(&mut self, line: &str) -> Result<Option<String>, String> {
        let line = line.strip_suffix('\r').unwrap_or(line);
        // The stream may open with exactly one BOM, which must be ignored;
        // left in place it would hide the first line's field name.
        let line = if self.first_line_seen {
            line
        } else {
            self.first_line_seen = true;
            line.strip_prefix('\u{feff}').unwrap_or(line)
        };
        if line.is_empty() {
            return Ok(self.take_data());
        }
        if line.starts_with(':') {
            return Ok(None); // comment line
        }
        let value = if let Some(value) = line.strip_prefix("data:") {
            // The space after the colon is optional in the SSE format.
            value.strip_prefix(' ').unwrap_or(value)
        } else if line == "data" {
            // A bare field name carries an empty value.
            ""
        } else {
            // Other fields (event:, id:, retry:) do not affect data assembly.
            return Ok(None);
        };
        // Every buffered line is charged a flat overhead on top of its
        // payload, so empty or tiny data lines cannot bypass the bound.
        self.buffered_bytes += SSE_LINE_OVERHEAD_BYTES + value.len();
        if self.buffered_bytes > MAX_SSE_EVENT_BYTES {
            self.data_lines.clear();
            self.buffered_bytes = 0;
            return Err(format!(
                "SSE event exceeds the maximum size of {MAX_SSE_EVENT_BYTES} bytes"
            ));
        }
        self.data_lines.push(value.to_string());
        Ok(None)
    }

    /// Flushes buffered data lines, for the event boundary and stream end.
    fn take_data(&mut self) -> Option<String> {
        self.buffered_bytes = 0;
        if self.data_lines.is_empty() {
            return None;
        }
        let data = std::mem::take(&mut self.data_lines).join("\n");
        (!data.trim().is_empty()).then_some(data)
    }
}

/// Outcome of one assembled SSE data payload.
enum SseDispatch {
    /// `[DONE]` sentinel: the stream completed normally.
    Done,
    /// Parsed chunk events (possibly empty).
    Events(Vec<GenerateEvent>),
    /// Non-empty data that is not valid JSON. Skipping it would drop part of
    /// the model output while the rest still parses, so it is a stream error.
    Malformed(String),
}

fn dispatch_sse_data(
    data: &str,
    thinking_field: Option<&str>,
    defer_message_end: bool,
    stream_state: &mut OpenAICompatStreamState,
) -> SseDispatch {
    if data.trim() == "[DONE]" {
        return SseDispatch::Done;
    }
    match serde_json::from_str::<Value>(data) {
        Ok(chunk) => SseDispatch::Events(
            parse_sse_chunk_with_state(&chunk, thinking_field, defer_message_end, stream_state)
                .unwrap_or_default(),
        ),
        Err(error) => SseDispatch::Malformed(format!("malformed SSE data: {error}")),
    }
}

#[cfg(test)]
fn parse_sse_chunk(
    chunk: &Value,
    thinking_field: Option<&str>,
    defer_message_end: bool,
) -> Option<Vec<GenerateEvent>> {
    parse_sse_chunk_with_state(
        chunk,
        thinking_field,
        defer_message_end,
        &mut OpenAICompatStreamState::default(),
    )
}

fn parse_sse_chunk_with_state(
    chunk: &Value,
    thinking_field: Option<&str>,
    defer_message_end: bool,
    stream_state: &mut OpenAICompatStreamState,
) -> Option<Vec<GenerateEvent>> {
    let mut events = Vec::new();

    if let Some(choices) = chunk.get("choices").and_then(|c| c.as_array()) {
        for choice in choices {
            let choice_index = choice.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as u32;
            let empty_delta = Value::Null;
            let delta = choice.get("delta").unwrap_or(&empty_delta);
            if let Some(field) = thinking_field {
                if let Some(text) = delta.get(field).and_then(|v| v.as_str()) {
                    if !text.is_empty() {
                        events.push(GenerateEvent::ThinkingDelta(text.to_string()));
                    }
                }
            }

            if let Some(content) = delta
                .get("content")
                .and_then(|c| c.as_str())
                .filter(|content| !content.is_empty())
            {
                stream_state
                    .emitted_text
                    .entry(choice_index)
                    .or_default()
                    .push_str(content);
                events.push(GenerateEvent::TextDelta(content.to_string()));
            } else if let Some(snapshot) = choice
                .get("message")
                .and_then(|message| message.get("content"))
                .and_then(|content| content.as_str())
                .filter(|content| !content.is_empty())
            {
                let emitted = stream_state.emitted_text.entry(choice_index).or_default();
                let suffix = snapshot
                    .strip_prefix(emitted.as_str())
                    .or_else(|| emitted.is_empty().then_some(snapshot));
                if let Some(suffix) = suffix.filter(|suffix| !suffix.is_empty()) {
                    emitted.push_str(suffix);
                    events.push(GenerateEvent::TextDelta(suffix.to_string()));
                }
            }

            if let Some(tool_calls) = delta.get("tool_calls").and_then(|t| t.as_array()) {
                let final_tool_calls = choice
                    .get("message")
                    .and_then(|message| message.get("tool_calls"))
                    .and_then(|calls| calls.as_array());
                for tc in tool_calls {
                    let index = tc.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as u32;
                    let final_call = final_tool_calls.and_then(|calls| {
                        calls.iter().find(|call| {
                            call.get("index").and_then(|i| i.as_u64()).unwrap_or(0)
                                == u64::from(index)
                        })
                    });

                    let delta_function = tc.get("function");
                    let final_function = final_call.and_then(|call| call.get("function"));
                    if let Some(name) = delta_function
                        .and_then(|function| function.get("name"))
                        .and_then(|name| name.as_str())
                        .filter(|name| !name.is_empty())
                        .or_else(|| {
                            final_function
                                .and_then(|function| function.get("name"))
                                .and_then(|name| name.as_str())
                                .filter(|name| !name.is_empty())
                        })
                    {
                        let id = tc
                            .get("id")
                            .and_then(|i| i.as_str())
                            .or_else(|| {
                                final_call
                                    .and_then(|call| call.get("id"))
                                    .and_then(|id| id.as_str())
                            })
                            .unwrap_or("")
                            .to_string();
                        if stream_state.started_tool_calls.insert(index) {
                            events.push(GenerateEvent::ToolCallStart {
                                index,
                                id,
                                name: name.to_string(),
                            });
                        }
                    }

                    let delta_arguments = delta_function
                        .and_then(|function| function.get("arguments"))
                        .and_then(|arguments| arguments.as_str());
                    if let Some(args) = delta_arguments.filter(|args| !args.is_empty()) {
                        stream_state.argument_deltas_seen.insert(index);
                        events.push(GenerateEvent::ToolCallDelta {
                            index,
                            arguments_delta: args.to_string(),
                        });
                    } else if !stream_state.argument_deltas_seen.contains(&index) {
                        if let Some(args) = final_function
                            .and_then(|function| function.get("arguments"))
                            .and_then(|arguments| arguments.as_str())
                            .filter(|args| !args.is_empty())
                        {
                            stream_state.argument_deltas_seen.insert(index);
                            events.push(GenerateEvent::ToolCallDelta {
                                index,
                                arguments_delta: args.to_string(),
                            });
                        }
                    }
                }
            } else if let Some(tool_calls) = choice
                .get("message")
                .and_then(|message| message.get("tool_calls"))
                .and_then(|calls| calls.as_array())
            {
                for tc in tool_calls {
                    let index = tc.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as u32;
                    let Some(function) = tc.get("function") else {
                        continue;
                    };
                    let name = function
                        .get("name")
                        .and_then(|name| name.as_str())
                        .filter(|name| !name.is_empty());
                    if let Some(name) = name {
                        let id = tc
                            .get("id")
                            .and_then(|id| id.as_str())
                            .unwrap_or("")
                            .to_string();
                        if stream_state.started_tool_calls.insert(index) {
                            events.push(GenerateEvent::ToolCallStart {
                                index,
                                id,
                                name: name.to_string(),
                            });
                        }
                    }
                    if !stream_state.argument_deltas_seen.contains(&index) {
                        if let Some(arguments) = function
                            .get("arguments")
                            .and_then(|args| args.as_str())
                            .filter(|arguments| !arguments.is_empty())
                        {
                            stream_state.argument_deltas_seen.insert(index);
                            events.push(GenerateEvent::ToolCallDelta {
                                index,
                                arguments_delta: arguments.to_string(),
                            });
                        }
                    }
                }
            }

            if let Some(finish) = choice.get("finish_reason").and_then(|f| f.as_str()) {
                stream_state.saw_finish_reason = true;
                if (finish == "stop" || finish == "tool_calls") && !defer_message_end {
                    events.push(GenerateEvent::MessageEnd);
                }
            }
        }
    }

    if let Some(usage) = chunk.get("usage").and_then(|u| u.as_object()) {
        let prompt = usage
            .get("prompt_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32;
        let completion = usage
            .get("completion_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32;
        let total = usage
            .get("total_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32;
        let cached = super::extract_cached_tokens(usage);
        let usage_event = GenerateEvent::Usage {
            prompt_tokens: prompt,
            completion_tokens: completion,
            total_tokens: total,
            cached_tokens: cached,
        };
        // Usage must precede MessageEnd; consumers stop reading at the end
        // marker and would otherwise drop the usage payload.
        match events
            .iter()
            .position(|event| matches!(event, GenerateEvent::MessageEnd))
        {
            Some(index) => events.insert(index, usage_event),
            None => events.push(usage_event),
        }
    }

    if events.is_empty() {
        None
    } else {
        Some(events)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_text_delta_chunk() {
        let chunk = serde_json::json!({
            "choices": [{
                "index": 0,
                "delta": {"content": "Hello"},
                "finish_reason": null
            }]
        });
        let events = parse_sse_chunk(&chunk, None, false).unwrap();
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], GenerateEvent::TextDelta(t) if t == "Hello"));
    }

    #[test]
    fn parse_final_message_text_without_delta() {
        let chunk = serde_json::json!({
            "choices": [{
                "index": 0,
                "delta": {},
                "message": {"content": "Repository analysis complete."},
                "finish_reason": "stop"
            }]
        });

        let events = parse_sse_chunk(&chunk, None, false).unwrap();
        assert!(matches!(
            &events[..],
            [GenerateEvent::TextDelta(text), GenerateEvent::MessageEnd]
                if text == "Repository analysis complete."
        ));
    }

    #[test]
    fn parse_tool_call_chunk() {
        let chunk = serde_json::json!({
            "choices": [{
                "index": 0,
                "delta": {
                    "tool_calls": [{
                        "index": 0,
                        "id": "call_1",
                        "function": {
                            "name": "shell",
                            "arguments": ""
                        }
                    }]
                },
                "finish_reason": null
            }]
        });
        let events = parse_sse_chunk(&chunk, None, false).unwrap();
        assert!(!events.is_empty());
        assert!(
            matches!(&events[0], GenerateEvent::ToolCallStart { name, id, .. } if name == "shell" && id == "call_1")
        );
    }

    #[test]
    fn parse_tool_call_arguments_delta() {
        let chunk = serde_json::json!({
            "choices": [{
                "index": 0,
                "delta": {
                    "tool_calls": [{
                        "index": 0,
                        "function": {
                            "arguments": "{\"command\":"
                        }
                    }]
                },
                "finish_reason": null
            }]
        });
        let events = parse_sse_chunk(&chunk, None, false).unwrap();
        assert_eq!(events.len(), 1);
        assert!(
            matches!(&events[0], GenerateEvent::ToolCallDelta { arguments_delta, .. } if arguments_delta == "{\"command\":")
        );
    }

    #[test]
    fn parse_final_message_tool_call_when_delta_has_only_arguments() {
        let chunk = serde_json::json!({
            "choices": [{
                "index": 0,
                "delta": {
                    "tool_calls": [{
                        "index": 0,
                        "function": {"arguments": "{\"command\":\"pwd\"}"}
                    }]
                },
                "message": {
                    "tool_calls": [{
                        "index": 0,
                        "id": "call_1",
                        "function": {
                            "name": "shell",
                            "arguments": "{\"command\":\"pwd\"}"
                        }
                    }]
                },
                "finish_reason": "tool_calls"
            }]
        });

        let events = parse_sse_chunk(&chunk, None, false).expect("tool call events");
        assert!(matches!(
            &events[0],
            GenerateEvent::ToolCallStart { id, name, .. } if id == "call_1" && name == "shell"
        ));
        assert!(matches!(
            &events[1],
            GenerateEvent::ToolCallDelta { arguments_delta, .. } if arguments_delta == "{\"command\":\"pwd\"}"
        ));
        assert!(matches!(&events[2], GenerateEvent::MessageEnd));
    }

    #[test]
    fn final_tool_call_snapshot_does_not_repeat_streamed_arguments() {
        let first_chunk = serde_json::json!({
            "choices": [{
                "index": 0,
                "delta": {
                    "tool_calls": [{
                        "index": 0,
                        "id": "call_1",
                        "function": {
                            "name": "shell",
                            "arguments": "{\"command\":\"pwd\"}"
                        }
                    }]
                },
                "finish_reason": null
            }]
        });
        let final_chunk = serde_json::json!({
            "choices": [{
                "index": 0,
                "delta": {"tool_calls": [{"index": 0, "function": {}}]},
                "message": {
                    "tool_calls": [{
                        "index": 0,
                        "id": "call_1",
                        "function": {
                            "name": "shell",
                            "arguments": "{\"command\":\"pwd\"}"
                        }
                    }]
                },
                "finish_reason": "tool_calls"
            }]
        });
        let mut state = OpenAICompatStreamState::default();

        let first = parse_sse_chunk_with_state(&first_chunk, None, false, &mut state)
            .expect("first streamed tool-call chunk");
        assert!(matches!(
            &first[..],
            [GenerateEvent::ToolCallStart { .. }, GenerateEvent::ToolCallDelta { arguments_delta, .. }]
                if arguments_delta == "{\"command\":\"pwd\"}"
        ));

        let final_events = parse_sse_chunk_with_state(&final_chunk, None, false, &mut state)
            .expect("final tool-call snapshot");
        assert!(
            !final_events
                .iter()
                .any(|event| matches!(event, GenerateEvent::ToolCallDelta { .. })),
            "final tool-call snapshots must not repeat streamed arguments"
        );
        assert!(
            !final_events
                .iter()
                .any(|event| matches!(event, GenerateEvent::ToolCallStart { .. })),
            "final tool-call snapshots must not reopen an existing tool block"
        );
        assert!(matches!(
            final_events.last(),
            Some(GenerateEvent::MessageEnd)
        ));
    }

    #[test]
    fn final_message_only_tool_call_does_not_repeat_streamed_arguments() {
        let first_chunk = serde_json::json!({
            "choices": [{
                "index": 0,
                "delta": {
                    "tool_calls": [{
                        "index": 0,
                        "id": "call_1",
                        "function": {
                            "name": "shell",
                            "arguments": "{\"command\":\"pwd\"}"
                        }
                    }]
                },
                "finish_reason": null
            }]
        });
        let final_chunk = serde_json::json!({
            "choices": [{
                "index": 0,
                "message": {
                    "tool_calls": [{
                        "index": 0,
                        "id": "call_1",
                        "function": {
                            "name": "shell",
                            "arguments": "{\"command\":\"pwd\"}"
                        }
                    }]
                },
                "finish_reason": "tool_calls"
            }]
        });
        let mut state = OpenAICompatStreamState::default();

        let _ = parse_sse_chunk_with_state(&first_chunk, None, false, &mut state)
            .expect("first streamed tool-call chunk");
        let final_events = parse_sse_chunk_with_state(&final_chunk, None, false, &mut state)
            .expect("final message-only tool-call snapshot");

        assert!(
            !final_events
                .iter()
                .any(|event| matches!(event, GenerateEvent::ToolCallDelta { .. })),
            "message-only snapshots must not repeat streamed arguments"
        );
        assert!(
            !final_events
                .iter()
                .any(|event| matches!(event, GenerateEvent::ToolCallStart { .. })),
            "message-only snapshots must not reopen an existing tool block"
        );
        assert!(matches!(
            final_events.last(),
            Some(GenerateEvent::MessageEnd)
        ));
    }

    #[test]
    fn parse_tool_call_from_final_message_without_delta() {
        let chunk = serde_json::json!({
            "choices": [{
                "index": 0,
                "message": {
                    "tool_calls": [{
                        "index": 0,
                        "id": "call_1",
                        "function": {
                            "name": "shell",
                            "arguments": "{\"command\":\"pwd\"}"
                        }
                    }]
                },
                "finish_reason": "tool_calls"
            }]
        });

        let events = parse_sse_chunk(&chunk, None, false).expect("tool call events");
        assert!(matches!(
            &events[0],
            GenerateEvent::ToolCallStart { id, name, .. } if id == "call_1" && name == "shell"
        ));
        assert!(matches!(
            &events[1],
            GenerateEvent::ToolCallDelta { arguments_delta, .. } if arguments_delta == "{\"command\":\"pwd\"}"
        ));
        assert!(matches!(&events[2], GenerateEvent::MessageEnd));
    }

    #[test]
    fn final_message_text_snapshot_only_emits_the_unseen_suffix() {
        let first_chunk = serde_json::json!({
            "choices": [{
                "index": 0,
                "delta": {"content": "Do"},
                "finish_reason": null
            }]
        });
        let final_chunk = serde_json::json!({
            "choices": [{
                "index": 0,
                "message": {"content": "Done"},
                "finish_reason": "stop"
            }]
        });
        let mut state = OpenAICompatStreamState::default();

        let first = parse_sse_chunk_with_state(&first_chunk, None, false, &mut state)
            .expect("first text chunk");
        assert!(matches!(
            &first[..],
            [GenerateEvent::TextDelta(text)] if text == "Do"
        ));

        let final_events = parse_sse_chunk_with_state(&final_chunk, None, false, &mut state)
            .expect("final text snapshot");
        assert!(matches!(
            &final_events[..],
            [GenerateEvent::TextDelta(text), GenerateEvent::MessageEnd] if text == "ne"
        ));
    }

    #[test]
    fn message_only_unnamed_tool_call_forwards_arguments_for_rejection() {
        let chunk = serde_json::json!({
            "choices": [{
                "index": 0,
                "message": {
                    "tool_calls": [{
                        "index": 0,
                        "id": "call_1",
                        "function": {"arguments": "{\"command\":\"pwd\"}"}
                    }]
                },
                "finish_reason": "tool_calls"
            }]
        });

        let events = parse_sse_chunk(&chunk, None, false).expect("tool call events");
        assert!(matches!(
            &events[..],
            [GenerateEvent::ToolCallDelta { arguments_delta, .. }, GenerateEvent::MessageEnd]
                if arguments_delta == "{\"command\":\"pwd\"}"
        ));
    }

    #[test]
    fn parse_finish_reason_stop() {
        let chunk = serde_json::json!({
            "choices": [{
                "index": 0,
                "delta": {},
                "finish_reason": "stop"
            }]
        });
        let events = parse_sse_chunk(&chunk, None, false).unwrap();
        assert!(matches!(&events[0], GenerateEvent::MessageEnd));
    }

    #[test]
    fn parse_reasoning_content_chunk() {
        let chunk = serde_json::json!({
            "choices": [{
                "index": 0,
                "delta": {
                    "reasoning_content": "Let me think step by step...",
                    "content": ""
                },
                "finish_reason": null
            }]
        });
        let events = parse_sse_chunk(&chunk, Some("reasoning_content"), false).unwrap();
        assert_eq!(events.len(), 1);
        assert!(
            matches!(&events[0], GenerateEvent::ThinkingDelta(t) if t == "Let me think step by step...")
        );
    }

    #[test]
    fn parse_reasoning_content_without_field_configured() {
        let chunk = serde_json::json!({
            "choices": [{
                "index": 0,
                "delta": {
                    "reasoning_content": "thinking...",
                    "content": "visible"
                },
                "finish_reason": null
            }]
        });
        let events = parse_sse_chunk(&chunk, None, false).unwrap();
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], GenerateEvent::TextDelta(t) if t == "visible"));
    }

    #[test]
    fn parse_usage_chunk() {
        let chunk = serde_json::json!({
            "choices": [],
            "usage": {
                "prompt_tokens": 100,
                "completion_tokens": 50,
                "total_tokens": 150
            }
        });
        let events = parse_sse_chunk(&chunk, None, false).unwrap();
        assert_eq!(events.len(), 1);
        assert!(matches!(
            &events[0],
            GenerateEvent::Usage {
                prompt_tokens: 100,
                completion_tokens: 50,
                total_tokens: 150,
                cached_tokens: 0
            }
        ));
    }

    #[test]
    fn usage_precedes_message_end_in_combined_chunk() {
        let chunk = serde_json::json!({
            "choices": [{
                "index": 0,
                "delta": {},
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 5,
                "total_tokens": 15
            }
        });
        let events = parse_sse_chunk(&chunk, None, false).unwrap();
        assert!(matches!(&events[0], GenerateEvent::Usage { .. }));
        assert!(matches!(&events[1], GenerateEvent::MessageEnd));
    }

    #[test]
    fn finish_reason_defers_message_end_when_usage_enabled() {
        let chunk = serde_json::json!({
            "choices": [{
                "index": 0,
                "delta": {},
                "finish_reason": "stop"
            }]
        });
        // With include_usage the usage chunk arrives after finish_reason;
        // MessageEnd must wait for [DONE] so usage is never dropped.
        assert!(parse_sse_chunk(&chunk, None, true).is_none());
        let usage_chunk = serde_json::json!({
            "choices": [],
            "usage": {"prompt_tokens": 7, "completion_tokens": 3, "total_tokens": 10}
        });
        let events = parse_sse_chunk(&usage_chunk, None, true).unwrap();
        assert!(matches!(&events[0], GenerateEvent::Usage { .. }));
    }

    #[test]
    fn build_request_with_max_completion_tokens() {
        let provider = OpenAICompatProvider::new(
            "https://api.openai.com/v1",
            "sk-test",
            Box::new(super::super::profile::OpenAIProfile),
            false,
        );
        let config = GenerateConfig {
            model: "o3".to_string(),
            max_tokens: 8192,
            ..Default::default()
        };
        let body = provider.build_request_body(&[], &[], &config);
        assert!(body.get("max_completion_tokens").is_some());
        assert!(body.get("max_tokens").is_none());
    }

    #[test]
    fn build_request_respects_caller_max_tokens_for_recognized_model() {
        // Regression: provider must not override the caller's max_tokens
        // even when the model is recognized.  The compaction summarizer sets
        // 2048 for qwen3.7-max; the provider must send 2048, not 65536.
        let provider = OpenAICompatProvider::new_generic("https://example.com/v1", "sk-test");
        let config = GenerateConfig {
            model: "qwen3.7-max".to_string(),
            max_tokens: 2_048,
            ..Default::default()
        };
        let body = provider.build_request_body(&[], &[], &config);
        assert_eq!(body["max_tokens"], 2_048);
    }

    #[test]
    fn build_request_uses_config_max_tokens_for_unknown_model() {
        let provider = OpenAICompatProvider::new_generic("https://example.com/v1", "sk-test");
        let config = GenerateConfig {
            model: "unknown-model".to_string(),
            max_tokens: 4096,
            ..Default::default()
        };
        let body = provider.build_request_body(&[], &[], &config);
        assert_eq!(body["max_tokens"], 4096);
    }

    #[test]
    fn build_request_with_extra_params() {
        let provider = OpenAICompatProvider::new_generic("https://example.com/v1", "sk-test");
        let config = GenerateConfig {
            model: "test".to_string(),
            max_tokens: 4096,
            extra_params: Some(serde_json::json!({
                "enable_thinking": true,
                "thinking_budget": 4096
            })),
            ..Default::default()
        };
        let body = provider.build_request_body(&[], &[], &config);
        assert_eq!(body["enable_thinking"], true);
        assert_eq!(body["thinking_budget"], 4096);
    }

    #[test]
    fn extra_params_cannot_raise_the_wire_output_cap() {
        // Regression (#2240): the compaction budget reserves `O` from the same
        // resolver that produced `max_tokens`, so the serialized request must
        // never be able to spend more than that reserve. Both cap aliases are
        // clamped because a backend may honor either one.
        let provider = OpenAICompatProvider::new_generic("https://example.com/v1", "sk-test");
        let config = GenerateConfig {
            model: "test".to_string(),
            max_tokens: 16_384,
            extra_params: Some(serde_json::json!({
                "max_tokens": 65_536u32,
                "max_completion_tokens": 65_536u32,
            })),
            ..Default::default()
        };

        let body = provider.build_request_body(&[], &[], &config);

        assert_eq!(body["max_tokens"], 16_384);
        assert_eq!(body["max_completion_tokens"], 16_384);
    }

    #[test]
    fn extra_params_cannot_raise_the_wire_cap_on_the_alias_field_profile() {
        // The OpenAI profile serializes `max_completion_tokens`; extra_params
        // must not raise it, nor sneak an unbounded `max_tokens` past the cap.
        let provider = OpenAICompatProvider::new(
            "https://api.openai.com/v1",
            "sk-test",
            Box::new(super::super::profile::OpenAIProfile),
            false,
        );
        let config = GenerateConfig {
            model: "o3".to_string(),
            max_tokens: 8_192,
            extra_params: Some(serde_json::json!({
                "max_completion_tokens": 65_536u32,
                "max_tokens": 65_536u32,
            })),
            ..Default::default()
        };

        let body = provider.build_request_body(&[], &[], &config);

        assert_eq!(body["max_completion_tokens"], 8_192);
        assert_eq!(body["max_tokens"], 8_192);
    }

    #[test]
    fn extra_params_may_still_lower_the_wire_output_cap() {
        // Asking for less than the reserve is always safe and is preserved.
        let provider = OpenAICompatProvider::new_generic("https://example.com/v1", "sk-test");
        let config = GenerateConfig {
            model: "test".to_string(),
            max_tokens: 16_384,
            extra_params: Some(serde_json::json!({"max_tokens": 512u32})),
            ..Default::default()
        };

        let body = provider.build_request_body(&[], &[], &config);

        assert_eq!(body["max_tokens"], 512);
        // The alias is not invented for a backend that never received it.
        assert!(body.get("max_completion_tokens").is_none(), "{body}");
    }

    #[test]
    fn non_numeric_extra_params_cap_is_replaced_by_the_reserve() {
        // A string or null cap could be coerced by a backend; fail closed to
        // the resolved reserve instead of forwarding it.
        let provider = OpenAICompatProvider::new_generic("https://example.com/v1", "sk-test");
        let config = GenerateConfig {
            model: "test".to_string(),
            max_tokens: 4_096,
            extra_params: Some(serde_json::json!({"max_tokens": "65536"})),
            ..Default::default()
        };

        let body = provider.build_request_body(&[], &[], &config);

        assert_eq!(body["max_tokens"], 4_096);
    }

    #[test]
    fn build_request_with_include_usage() {
        // A profile that supports usage telemetry receives stream_options.
        let provider = OpenAICompatProvider::new(
            "https://api.openai.com/v1",
            "sk-test",
            Box::new(super::super::profile::OpenAIProfile),
            false,
        );
        let config = GenerateConfig {
            model: "test".to_string(),
            max_tokens: 4096,
            include_usage: true,
            ..Default::default()
        };
        let body = provider.build_request_body(&[], &[], &config);
        assert_eq!(body["stream_options"]["include_usage"], true);
    }

    #[test]
    fn generic_profile_omits_stream_options_even_with_include_usage() {
        // Regression: the compaction feature enables include_usage on every
        // agent turn. A generic OpenAI-compatible endpoint must not be forced
        // to accept stream_options it may reject; usage degrades to the local
        // estimate instead of breaking the model conversation.
        let provider = OpenAICompatProvider::new_generic("https://example.com/v1", "sk-test");
        let config = GenerateConfig {
            model: "test".to_string(),
            max_tokens: 4096,
            include_usage: true,
            ..Default::default()
        };
        let body = provider.build_request_body(&[], &[], &config);
        assert!(
            body.get("stream_options").is_none(),
            "generic endpoints must not receive stream_options: {body}"
        );
    }

    #[test]
    fn build_request_preserves_user_provided_secrets() {
        let provider = OpenAICompatProvider::new_generic("https://example.com/v1", "sk-test");
        let secret = "short-provider-secret";
        let messages = vec![Message::user(&format!("api_key={secret}"))];

        let body = provider.build_request_body(&messages, &[], &GenerateConfig::default());
        let payload = body.to_string();

        assert!(payload.contains(secret), "{payload}");
        assert!(!payload.contains("<redacted>"), "{payload}");
    }

    #[test]
    fn parse_sse_extracts_cached_tokens_from_prompt_tokens_details() {
        let chunk = serde_json::json!({
            "choices": [],
            "usage": {
                "prompt_tokens": 1000,
                "completion_tokens": 50,
                "total_tokens": 1050,
                "prompt_tokens_details": {
                    "cached_tokens": 800
                }
            }
        });
        let events = parse_sse_chunk(&chunk, None, false).unwrap();
        assert!(matches!(
            &events[0],
            GenerateEvent::Usage {
                prompt_tokens: 1000,
                completion_tokens: 50,
                total_tokens: 1050,
                cached_tokens: 800
            }
        ));
    }

    #[test]
    fn parse_sse_extracts_cached_tokens_from_top_level() {
        let chunk = serde_json::json!({
            "choices": [],
            "usage": {
                "prompt_tokens": 1000,
                "completion_tokens": 50,
                "total_tokens": 1050,
                "cached_tokens": 600
            }
        });
        let events = parse_sse_chunk(&chunk, None, false).unwrap();
        assert!(matches!(
            &events[0],
            GenerateEvent::Usage {
                prompt_tokens: 1000,
                completion_tokens: 50,
                total_tokens: 1050,
                cached_tokens: 600
            }
        ));
    }

    #[test]
    fn parse_sse_prefers_prompt_tokens_details_over_top_level_cached_tokens() {
        let chunk = serde_json::json!({
            "choices": [],
            "usage": {
                "prompt_tokens": 1000,
                "completion_tokens": 50,
                "total_tokens": 1050,
                "prompt_tokens_details": {
                    "cached_tokens": 800
                },
                "cached_tokens": 600
            }
        });
        let events = parse_sse_chunk(&chunk, None, false).unwrap();
        assert!(matches!(
            &events[0],
            GenerateEvent::Usage {
                prompt_tokens: 1000,
                completion_tokens: 50,
                total_tokens: 1050,
                cached_tokens: 800
            }
        ));
    }

    #[test]
    fn parse_sse_defaults_cached_tokens_to_zero_when_absent() {
        let chunk = serde_json::json!({
            "choices": [],
            "usage": {
                "prompt_tokens": 100,
                "completion_tokens": 50,
                "total_tokens": 150
            }
        });
        let events = parse_sse_chunk(&chunk, None, false).unwrap();
        assert!(matches!(
            &events[0],
            GenerateEvent::Usage {
                prompt_tokens: 100,
                completion_tokens: 50,
                total_tokens: 150,
                cached_tokens: 0
            }
        ));
    }

    // ── Cache control tests ──

    fn dashscope_provider(explicit: bool) -> OpenAICompatProvider {
        OpenAICompatProvider::new(
            "https://dashscope.aliyuncs.com/compatible-mode/v1",
            "sk-test",
            Box::new(super::super::profile::DashScopeProfile),
            explicit,
        )
    }

    #[test]
    fn cache_control_adds_markers_to_system_and_last_message() {
        let provider = dashscope_provider(true);
        let messages = vec![
            Message::system("You are a helpful assistant."),
            Message::user("Hello"),
            Message::assistant("Hi there"),
            Message::user("How are you?"),
        ];
        let body = provider.build_request_body(&messages, &[], &GenerateConfig::default());

        let msgs = body["messages"].as_array().unwrap();
        // System message content should be array with cache_control on last part.
        let system_content = &msgs[0]["content"];
        assert!(system_content.is_array(), "system content should be array");
        let system_parts = system_content.as_array().unwrap();
        assert!(system_parts.last().unwrap().get("cache_control").is_some());
        // Last message content should also have cache_control.
        let last_content = msgs.last().unwrap()["content"].as_array().unwrap();
        assert!(last_content.last().unwrap().get("cache_control").is_some());
    }

    #[test]
    fn cache_control_disabled_skips_markers() {
        let provider = dashscope_provider(false);
        let messages = vec![
            Message::system("You are a helpful assistant."),
            Message::user("Hello"),
        ];
        let body = provider.build_request_body(&messages, &[], &GenerateConfig::default());
        assert!(
            !body.to_string().contains("cache_control"),
            "body should not contain cache_control when disabled"
        );
    }

    #[test]
    fn cache_control_string_content_converted_to_array() {
        let provider = dashscope_provider(true);
        let messages = vec![Message::system("System prompt text")];
        let body = provider.build_request_body(&messages, &[], &GenerateConfig::default());
        let system_content = &body["messages"][0]["content"];
        assert!(
            system_content.is_array(),
            "string content should be converted to array"
        );
        let parts = system_content.as_array().unwrap();
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0]["type"], "text");
        assert_eq!(parts[0]["text"], "System prompt text");
        assert_eq!(parts[0]["cache_control"]["type"], "ephemeral");
    }

    #[test]
    fn non_dashscope_profile_no_cache_control() {
        let provider = OpenAICompatProvider::new(
            "https://api.openai.com/v1",
            "sk-test",
            Box::new(super::super::profile::GenericProfile),
            false,
        );
        let messages = vec![Message::system("System"), Message::user("Hello")];
        let body = provider.build_request_body(&messages, &[], &GenerateConfig::default());
        assert!(
            !body.to_string().contains("cache_control"),
            "generic profile should not inject cache_control"
        );
    }

    #[test]
    fn non_dashscope_profile_no_cache_control_even_when_opted_in() {
        // User sets explicit_cache = true, but the provider profile is not
        // DashScope — the profile gate must still block marker injection.
        let provider = OpenAICompatProvider::new(
            "https://api.openai.com/v1",
            "sk-test",
            Box::new(super::super::profile::GenericProfile),
            true,
        );
        let messages = vec![Message::system("System"), Message::user("Hello")];
        let body = provider.build_request_body(&messages, &[], &GenerateConfig::default());
        assert!(
            !body.to_string().contains("cache_control"),
            "generic profile should not inject cache_control even with explicit_cache = true"
        );
    }

    #[test]
    fn cache_control_no_system_message_caches_last_only() {
        let provider = dashscope_provider(true);
        let messages = vec![
            Message::user("Hello"),
            Message::assistant("Hi there"),
            Message::user("How are you?"),
        ];
        let body = provider.build_request_body(&messages, &[], &GenerateConfig::default());

        let msgs = body["messages"].as_array().unwrap();
        // No system message, so only the last message should have cache_control.
        for (i, msg) in msgs.iter().enumerate() {
            let has_cache = msg["content"]
                .as_array()
                .and_then(|parts| parts.last())
                .and_then(|p| p.get("cache_control"))
                .is_some();
            if i == msgs.len() - 1 {
                assert!(has_cache, "last message should have cache_control");
            } else {
                assert!(!has_cache, "non-last message should not have cache_control");
            }
        }
    }

    #[test]
    fn cache_control_wraps_raw_string_content_part() {
        // Regression: a raw string as the last content part used to panic
        // because serde_json cannot index into Value::String with a string key.
        let mut msg = serde_json::json!({
            "role": "system",
            "content": ["valid text", "raw string"]
        });
        add_cache_control_to_message_content(&mut msg);

        let parts = msg["content"].as_array().unwrap();
        let last = parts.last().unwrap();
        assert_eq!(last["type"], "text");
        assert_eq!(last["text"], "raw string");
        assert_eq!(last["cache_control"]["type"], "ephemeral");
    }

    #[test]
    fn cache_control_skips_non_object_non_string_content_part() {
        // Numbers and booleans are not cacheable content; the function must
        // not panic and must simply skip the marker.
        let mut msg = serde_json::json!({
            "role": "system",
            "content": [{"type": "text", "text": "ok"}, 42]
        });
        add_cache_control_to_message_content(&mut msg);

        let parts = msg["content"].as_array().unwrap();
        let last = parts.last().unwrap();
        assert!(last.is_number(), "number part should remain untouched");
        assert!(last.get("cache_control").is_none());
    }

    // ─── SSE decode-loop integrity ───
    //
    // These tests drive the real byte-stream decoding loop over a local TCP
    // socket. The decoder must assemble events per the SSE wire format (no
    // silent frame drops) and fail loud on any corrupted or truncated stream,
    // because a dropped arguments delta can still concatenate into valid JSON
    // — a truncated shell command that would then execute.

    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    /// Serve `segments` once over a local TCP socket (one write + small pause
    /// per segment, so chunk boundaries land where the test puts them) and
    /// collect provider events up to and including the first terminal event.
    async fn terminal_events_from_sse_segments(
        segments: Vec<Vec<u8>>,
        include_usage: bool,
        profile: Box<dyn ProviderProfile>,
    ) -> Vec<GenerateEvent> {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let total: usize = segments.iter().map(Vec::len).sum();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = vec![0; 8192];
            let _ = socket.read(&mut request).await.unwrap();
            let header = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {total}\r\nConnection: close\r\n\r\n",
            );
            socket.write_all(header.as_bytes()).await.unwrap();
            for segment in segments {
                socket.write_all(&segment).await.unwrap();
                socket.flush().await.unwrap();
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        });

        let provider =
            OpenAICompatProvider::new(&format!("http://{address}/v1"), "test", profile, false);
        let config = GenerateConfig {
            include_usage,
            ..Default::default()
        };
        let mut stream = provider.generate(&[], &[], &config).await.unwrap();
        let events = tokio::time::timeout(Duration::from_secs(5), async move {
            let mut events = Vec::new();
            while let Some(event) = stream.next().await {
                let terminal = matches!(
                    event,
                    GenerateEvent::MessageEnd | GenerateEvent::Error(_) | GenerateEvent::Cancelled
                );
                events.push(event);
                if terminal {
                    break;
                }
            }
            events
        })
        .await
        .expect("SSE stream did not reach a terminal event in time");
        server.await.unwrap();
        events
    }

    async fn terminal_events_from_sse(
        body: &str,
        include_usage: bool,
        profile: Box<dyn ProviderProfile>,
    ) -> Vec<GenerateEvent> {
        terminal_events_from_sse_segments(vec![body.as_bytes().to_vec()], include_usage, profile)
            .await
    }

    fn text_chunk(content: &str, finish: Option<&str>) -> String {
        serde_json::json!({
            "choices": [{
                "index": 0,
                "delta": {"content": content},
                "finish_reason": finish,
            }]
        })
        .to_string()
    }

    fn arguments_chunk(arguments: &str) -> String {
        serde_json::json!({
            "choices": [{
                "index": 0,
                "delta": {"tool_calls": [{
                    "index": 0,
                    "function": {"arguments": arguments},
                }]},
                "finish_reason": null,
            }]
        })
        .to_string()
    }

    fn collected_arguments(events: &[GenerateEvent]) -> String {
        events
            .iter()
            .filter_map(|event| match event {
                GenerateEvent::ToolCallDelta {
                    arguments_delta, ..
                } => Some(arguments_delta.as_str()),
                _ => None,
            })
            .collect()
    }

    // EOF before [DONE] and before any finish_reason is a truncated stream;
    // it must surface an Error after the partial delta, never a silent
    // MessageEnd.
    #[tokio::test]
    async fn truncated_stream_without_done_or_finish_reports_error() {
        let body = format!("data: {}\n\n", text_chunk("partial", None));
        let events =
            terminal_events_from_sse(&body, false, Box::new(profile::GenericProfile)).await;

        assert!(
            matches!(&events[0], GenerateEvent::TextDelta(text) if text == "partial"),
            "partial delta must be preserved: {events:?}"
        );
        assert!(
            matches!(events.last(), Some(GenerateEvent::Error(_))),
            "EOF before [DONE]/finish_reason must end in Error, got: {events:?}"
        );
    }

    // Non-empty malformed SSE data must not be silently skipped.
    #[tokio::test]
    async fn malformed_sse_data_reports_error() {
        let events = terminal_events_from_sse(
            "data: {not json}\n\n",
            false,
            Box::new(profile::GenericProfile),
        )
        .await;

        assert!(
            matches!(events.first(), Some(GenerateEvent::Error(_))),
            "malformed SSE data must surface an Error, got: {events:?}"
        );
    }

    // A malformed chunk mid-stream must not be masked by a later [DONE]
    // marker turning the turn into a silent false success.
    #[tokio::test]
    async fn malformed_sse_data_before_done_reports_error() {
        let events = terminal_events_from_sse(
            "data: {not json}\n\ndata: [DONE]\n\n",
            false,
            Box::new(profile::GenericProfile),
        )
        .await;

        assert!(
            matches!(events.first(), Some(GenerateEvent::Error(_))),
            "malformed SSE data must surface an Error even before [DONE], got: {events:?}"
        );
    }

    // The space after `data:` is optional on the wire. A mid-stream frame
    // without it must be decoded, not dropped — dropping it reassembles the
    // remaining deltas into a truncated but valid-looking tool call.
    #[tokio::test]
    async fn data_line_without_space_is_not_dropped() {
        let head = arguments_chunk("{\"command\": \"head");
        let mid = arguments_chunk(" | mid");
        let tail = arguments_chunk(" | tail\"}");
        let finish = serde_json::json!({
            "choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls"}]
        })
        .to_string();
        let body = format!(
            "data: {head}\n\ndata:{mid}\n\ndata: {tail}\n\ndata: {finish}\n\ndata: [DONE]\n\n"
        );
        let events =
            terminal_events_from_sse(&body, false, Box::new(profile::GenericProfile)).await;

        assert_eq!(
            collected_arguments(&events),
            "{\"command\": \"head | mid | tail\"}",
            "no-space data frame must not be dropped: {events:?}"
        );
        assert!(
            matches!(events.last(), Some(GenerateEvent::MessageEnd)),
            "stream must still complete normally: {events:?}"
        );
    }

    // Extra whitespace after the optional space belongs to the payload; JSON
    // parsing tolerates it.
    #[tokio::test]
    async fn data_line_with_two_spaces_parses_payload() {
        let body = format!(
            "data:  {}\n\ndata: [DONE]\n\n",
            text_chunk("hello", Some("stop"))
        );
        let events =
            terminal_events_from_sse(&body, false, Box::new(profile::GenericProfile)).await;

        assert!(
            matches!(&events[0], GenerateEvent::TextDelta(text) if text == "hello"),
            "payload after double space must parse: {events:?}"
        );
    }

    // One event spread over several `data` lines joins with newlines per the
    // SSE format before parsing. Split between JSON tokens so the seam
    // newline is legal whitespace.
    #[tokio::test]
    async fn multi_data_lines_join_before_parse() {
        let chunk = text_chunk("joined", Some("stop"));
        let seam = chunk
            .find(",\"finish_reason\"")
            .expect("chunk contains a token boundary");
        let (first, second) = chunk.split_at(seam);
        let body = format!("data: {first}\ndata: {second}\n\ndata: [DONE]\n\n");
        let events =
            terminal_events_from_sse(&body, false, Box::new(profile::GenericProfile)).await;

        assert!(
            matches!(&events[0], GenerateEvent::TextDelta(text) if text == "joined"),
            "multi-line data must join and parse: {events:?}"
        );
        assert!(
            matches!(events.last(), Some(GenerateEvent::MessageEnd)),
            "stream must complete: {events:?}"
        );
    }

    // CRLF line endings, comment lines, and non-data fields are all part of
    // the wire format and must not disturb data assembly.
    #[tokio::test]
    async fn crlf_comments_and_field_lines_are_tolerated() {
        let body = format!(
            ": keep-alive\r\nevent: message\r\nid: 42\r\nretry: 100\r\ndata: {}\r\n\r\ndata: [DONE]\r\n\r\n",
            text_chunk("crlf", Some("stop"))
        );
        let events =
            terminal_events_from_sse(&body, false, Box::new(profile::GenericProfile)).await;

        assert!(
            matches!(&events[0], GenerateEvent::TextDelta(text) if text == "crlf"),
            "CRLF-framed data must parse: {events:?}"
        );
        assert!(
            matches!(events.last(), Some(GenerateEvent::MessageEnd)),
            "stream must complete: {events:?}"
        );
    }

    // A multi-byte character split across network chunks must reassemble
    // instead of decaying into replacement characters that corrupt the
    // payload.
    #[tokio::test]
    async fn multibyte_character_split_across_chunks_reassembles() {
        let chunk_json = format!(
            "data: {}\n\ndata: [DONE]\n\n",
            text_chunk("键值", Some("stop"))
        );
        let bytes = chunk_json.into_bytes();
        // Split inside the first multi-byte character of the payload.
        let split_at = bytes
            .iter()
            .position(|&b| b >= 0x80)
            .expect("payload contains a multi-byte character")
            + 1;
        let (head, tail) = bytes.split_at(split_at);
        let events = terminal_events_from_sse_segments(
            vec![head.to_vec(), tail.to_vec()],
            false,
            Box::new(profile::GenericProfile),
        )
        .await;

        assert!(
            matches!(&events[0], GenerateEvent::TextDelta(text) if text == "键值"),
            "split multi-byte character must reassemble: {events:?}"
        );
    }

    // A line that cannot be decoded as UTF-8 is corrupted transport data and
    // must fail loud rather than degrade into replacement characters.
    #[tokio::test]
    async fn invalid_utf8_line_reports_error() {
        let mut body = b"data: {\"choices\":[{\"delta\":{\"content\":\"".to_vec();
        body.extend_from_slice(&[0xFF, 0xFE]);
        body.extend_from_slice(b"\"},\"finish_reason\":null}]}\n\n");
        let events =
            terminal_events_from_sse_segments(vec![body], false, Box::new(profile::GenericProfile))
                .await;

        assert!(
            matches!(events.first(), Some(GenerateEvent::Error(_))),
            "invalid UTF-8 must surface an Error, got: {events:?}"
        );
    }

    // With deferred MessageEnd (usage requested), an EOF after finish_reason
    // but before usage/[DONE] still completes: the content is whole and a
    // missing usage payload is not worth failing the turn.
    #[tokio::test]
    async fn eof_after_finish_reason_without_done_is_message_end() {
        let body = format!("data: {}\n\n", text_chunk("done", Some("stop")));
        let events =
            terminal_events_from_sse(&body, true, Box::new(profile::DashScopeProfile)).await;

        assert!(
            matches!(&events[0], GenerateEvent::TextDelta(text) if text == "done"),
            "content must be delivered: {events:?}"
        );
        assert!(
            matches!(events.last(), Some(GenerateEvent::MessageEnd)),
            "EOF after finish_reason must complete, not error: {events:?}"
        );
    }

    // A stream that ends without the final blank-line separator must still
    // flush the buffered event.
    #[tokio::test]
    async fn final_event_without_trailing_separator_is_flushed() {
        let body = format!("data: {}", text_chunk("tail", Some("stop")));
        let events =
            terminal_events_from_sse(&body, false, Box::new(profile::GenericProfile)).await;

        assert!(
            matches!(&events[0], GenerateEvent::TextDelta(text) if text == "tail"),
            "unterminated final event must be flushed: {events:?}"
        );
        assert!(
            matches!(events.last(), Some(GenerateEvent::MessageEnd)),
            "finish_reason=stop makes this a complete stream: {events:?}"
        );
    }

    // An event that keeps accumulating data lines while withholding the
    // blank-line separator must fail loud instead of growing memory without
    // bound.
    #[test]
    fn oversized_event_reports_error_instead_of_accumulating() {
        let mut buffer = SseEventBuffer::default();
        let line = format!("data: {}", "x".repeat(1024 * 1024));
        let mut overflowed = None;
        for _ in 0..(MAX_SSE_EVENT_BYTES / (1024 * 1024) + 1) {
            match buffer.push_line(&line) {
                Ok(_) => {}
                Err(message) => {
                    overflowed = Some(message);
                    break;
                }
            }
        }
        let message = overflowed.expect("accumulation past the bound must error");
        assert!(
            message.contains("maximum size"),
            "error must name the bound: {message}"
        );
        // The buffer resets after overflow so the stream state cannot keep
        // the oversized payload alive.
        assert!(buffer.take_data().is_none());
    }

    // The SSE grammar permits exactly one leading BOM; it must not hide the
    // first line's field name and silently drop the first event.
    #[tokio::test]
    async fn leading_bom_does_not_drop_the_first_event() {
        let body = format!(
            "\u{feff}data: {}\n\ndata: [DONE]\n\n",
            text_chunk("first", Some("stop"))
        );
        let events =
            terminal_events_from_sse(&body, false, Box::new(profile::GenericProfile)).await;

        assert!(
            matches!(&events[0], GenerateEvent::TextDelta(text) if text == "first"),
            "the first event after a BOM must be decoded: {events:?}"
        );
        assert!(
            matches!(events.last(), Some(GenerateEvent::MessageEnd)),
            "stream must complete: {events:?}"
        );
    }

    // A BOM must be removed exactly once: a second U+FEFF is payload.
    #[test]
    fn only_the_first_bom_is_stripped() {
        let mut buffer = SseEventBuffer::default();
        assert_eq!(buffer.push_line("\u{feff}data: a"), Ok(None));
        // Not at stream start: the BOM makes this an unknown field line.
        assert_eq!(buffer.push_line("\u{feff}data: b"), Ok(None));
        assert_eq!(buffer.push_line(""), Ok(Some("a".to_string())));
    }

    // Payload-free data lines must consume the event budget too; counting
    // only payload bytes would let an endpoint retain unbounded line
    // structures while the counter stays at zero.
    #[test]
    fn empty_data_lines_cannot_bypass_the_event_bound() {
        let mut buffer = SseEventBuffer::default();
        let within_bound = MAX_SSE_EVENT_BYTES / SSE_LINE_OVERHEAD_BYTES + 1;
        let mut overflowed = None;
        for _ in 0..(within_bound + 1) {
            if let Err(message) = buffer.push_line("data:") {
                overflowed = Some(message);
                break;
            }
        }
        let message = overflowed.expect("empty-line flood must hit the bound");
        assert!(
            message.contains("maximum size"),
            "error must name the bound: {message}"
        );
        assert!(buffer.take_data().is_none());
    }

    // A bare CR terminates its line immediately; waiting for the next byte
    // would stall a stream that flushes a CR-framed event and then idles.
    #[test]
    fn chunk_final_bare_cr_yields_the_line_without_more_bytes() {
        let mut buffer = SseByteBuffer::default();
        buffer.extend(b"data: x\r\r");
        assert_eq!(buffer.take_line().as_deref(), Some(&b"data: x"[..]));
        // The second CR is the blank-line event boundary, available now.
        assert_eq!(buffer.take_line().as_deref(), Some(&b""[..]));
        assert!(buffer.take_line().is_none());
        assert!(buffer.is_empty());
    }

    // The LF half of a chunk-split CRLF must be swallowed, not turned into
    // an extra empty line (an event boundary).
    #[test]
    fn chunk_split_crlf_does_not_fabricate_an_event_boundary() {
        let mut buffer = SseByteBuffer::default();
        buffer.extend(b"data: a\r");
        assert_eq!(buffer.take_line().as_deref(), Some(&b"data: a"[..]));
        buffer.extend(b"\ndata: b\n");
        assert_eq!(buffer.take_line().as_deref(), Some(&b"data: b"[..]));
        assert!(buffer.take_line().is_none());
        // A bare CR followed by a normal byte keeps that byte.
        buffer.extend(b"c\r");
        assert_eq!(buffer.take_line().as_deref(), Some(&b"c"[..]));
        buffer.extend(b"d\n");
        assert_eq!(buffer.take_line().as_deref(), Some(&b"d"[..]));
    }

    // Streaming liveness: a CR-framed event flushed on an idle connection
    // must dispatch before any further bytes (such as [DONE]) arrive.
    #[tokio::test]
    async fn cr_framed_event_dispatches_before_the_stream_idles() {
        let first = format!("data: {}\r\r", text_chunk("live", None));
        let second = "data: [DONE]\r\r".to_string();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let total = first.len() + second.len();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = vec![0; 8192];
            let _ = socket.read(&mut request).await.unwrap();
            let header = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {total}\r\nConnection: close\r\n\r\n",
            );
            socket.write_all(header.as_bytes()).await.unwrap();
            socket.write_all(first.as_bytes()).await.unwrap();
            socket.flush().await.unwrap();
            // Idle: the event above must dispatch during this window.
            tokio::time::sleep(Duration::from_millis(1000)).await;
            socket.write_all(second.as_bytes()).await.unwrap();
        });

        let provider = OpenAICompatProvider::new(
            &format!("http://{address}/v1"),
            "test",
            Box::new(profile::GenericProfile),
            false,
        );
        let config = GenerateConfig::default();
        let mut stream = provider.generate(&[], &[], &config).await.unwrap();
        let first_event = tokio::time::timeout(Duration::from_millis(700), stream.next())
            .await
            .expect("CR-framed event must dispatch while the stream idles")
            .expect("stream must yield an event");
        assert!(
            matches!(&first_event, GenerateEvent::TextDelta(text) if text == "live"),
            "expected the idle-window delta: {first_event:?}"
        );
        // Drain the rest so the server task completes cleanly.
        let _ = tokio::time::timeout(Duration::from_secs(5), async move {
            while let Some(event) = stream.next().await {
                if matches!(
                    event,
                    GenerateEvent::MessageEnd | GenerateEvent::Error(_) | GenerateEvent::Cancelled
                ) {
                    break;
                }
            }
        })
        .await;
        server.await.unwrap();
    }

    // WHATWG line endings include a bare CR; a valid CR-only stream must
    // decode instead of buffering to EOF and failing as one malformed line.
    #[tokio::test]
    async fn bare_cr_line_endings_are_decoded() {
        let body = format!(
            "data: {}\r\rdata: [DONE]\r\r",
            text_chunk("cr-only", Some("stop"))
        );
        let events =
            terminal_events_from_sse(&body, false, Box::new(profile::GenericProfile)).await;

        assert!(
            matches!(&events[0], GenerateEvent::TextDelta(text) if text == "cr-only"),
            "bare-CR framed data must parse: {events:?}"
        );
        assert!(
            matches!(events.last(), Some(GenerateEvent::MessageEnd)),
            "stream must complete: {events:?}"
        );
    }

    // A CRLF split across network chunks is one line ending, not a line plus
    // an empty line (which would be an event boundary).
    #[tokio::test]
    async fn crlf_split_across_chunks_is_one_line_ending() {
        let body = format!(
            "data: {}\r\n\r\ndata: [DONE]\r\n\r\n",
            text_chunk("split-crlf", Some("stop"))
        );
        let bytes = body.into_bytes();
        // Split right after the first CR so its LF arrives in the next chunk.
        let split_at = bytes.iter().position(|&b| b == b'\r').unwrap() + 1;
        let (head, tail) = bytes.split_at(split_at);
        let events = terminal_events_from_sse_segments(
            vec![head.to_vec(), tail.to_vec()],
            false,
            Box::new(profile::GenericProfile),
        )
        .await;

        assert!(
            matches!(&events[0], GenerateEvent::TextDelta(text) if text == "split-crlf"),
            "a chunk-split CRLF must stay one line ending: {events:?}"
        );
    }

    // Once a terminal event went out, the stream is sealed: a [DONE] behind
    // a finish_reason-driven MessageEnd must not yield a second terminal.
    #[tokio::test]
    async fn finish_reason_then_done_yields_a_single_message_end() {
        let body = format!(
            "data: {}\n\ndata: [DONE]\n\n",
            text_chunk("once", Some("stop"))
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let response_body = body.clone();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = vec![0; 8192];
            let _ = socket.read(&mut request).await.unwrap();
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                response_body.len(),
                response_body
            );
            socket.write_all(response.as_bytes()).await.unwrap();
        });

        let provider = OpenAICompatProvider::new(
            &format!("http://{address}/v1"),
            "test",
            Box::new(profile::GenericProfile),
            false,
        );
        let config = GenerateConfig::default();
        let mut stream = provider.generate(&[], &[], &config).await.unwrap();
        // Consume the stream to exhaustion (past the first terminal event).
        let events = tokio::time::timeout(Duration::from_secs(5), async move {
            let mut events = Vec::new();
            while let Some(event) = stream.next().await {
                events.push(event);
            }
            events
        })
        .await
        .expect("stream must end after the terminal event");
        server.await.unwrap();

        let terminals = events
            .iter()
            .filter(|event| matches!(event, GenerateEvent::MessageEnd))
            .count();
        assert_eq!(terminals, 1, "exactly one MessageEnd expected: {events:?}");
    }

    // Non-UTF-8 error bodies must be decoded using the charset declared in the
    // Content-Type header. Without reqwest's charset feature, the body is read
    // as UTF-8 and corrupts Chinese, Japanese, or legacy Windows-1252 text.
    #[tokio::test]
    async fn error_response_with_non_utf8_charset_is_decoded() {
        // "中文错误" encoded in GBK.
        let body: Vec<u8> = vec![0xD6, 0xD0, 0xCE, 0xC4, 0xB4, 0xED, 0xCE, 0xF3];
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = vec![0; 8192];
            let _ = socket.read(&mut request).await.unwrap();
            let header = format!(
                "HTTP/1.1 400 Bad Request\r\nContent-Type: text/plain; charset=gbk\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            socket.write_all(header.as_bytes()).await.unwrap();
            socket.write_all(&body).await.unwrap();
        });

        let provider =
            OpenAICompatProvider::new_generic(&format!("http://{address}/v1"), "sk-test");
        let config = GenerateConfig::default();
        let err = match provider.generate(&[], &[], &config).await {
            Ok(_) => panic!("expected generate() to fail on non-success response"),
            Err(e) => e,
        };
        server.await.unwrap();

        assert!(
            err.contains("API error 400 Bad Request: 中文错误"),
            "GBK error body must be decoded using the declared charset: {err}"
        );
    }
}
