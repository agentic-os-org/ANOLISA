use crate::chrome_trace::{ChromeTraceEvent, ns_to_us};
use crate::probes::sslsniff::SslEvent;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::rc::Rc;

/// SSE Event - Standard Server-Sent Events message (legacy version with String data)
/// Follows the W3C EventSource specification: https://html.spec.whatwg.org/multipage/server-sent-events.html
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SSEEvent {
    /// Event ID (optional, used for reconnection)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Event type/name (default is "message")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event: Option<String>,
    /// Event data (can be multi-line, joined with \n)
    pub data: String,
    /// Retry hint in milliseconds (optional)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry: Option<u64>,
}

/// Parsed SSE Event - zero-copy version with SslEvent reference
/// Follows the W3C EventSource specification
#[derive(Clone)]
pub struct ParsedSseEvent {
    /// Event ID (optional, used for reconnection)
    pub id: Option<String>,
    /// Event type/name (default is "message")
    pub event: Option<String>,
    /// Retry hint in milliseconds (optional)
    pub retry: Option<u64>,
    /// Data offset in source_event.buf
    data_offset: usize,
    /// Data length
    data_len: usize,
    /// Original SslEvent (Rc to avoid cloning)
    source_event: Rc<SslEvent>,
    /// Whether this is a synthetic "done" marker (e.g., from chunked end "0\r\n\r\n")
    is_synthetic_done: bool,
}

impl ParsedSseEvent {
    /// Create a new ParsedSseEvent
    pub fn new(
        id: Option<String>,
        event: Option<String>,
        retry: Option<u64>,
        data_offset: usize,
        data_len: usize,
        source_event: Rc<SslEvent>,
    ) -> Self {
        Self {
            id,
            event,
            retry,
            data_offset,
            data_len,
            source_event,
            is_synthetic_done: false,
        }
    }

    /// Create a synthetic [DONE] marker event
    /// Used when HTTP chunked transfer encoding end marker "0\r\n\r\n" is detected
    pub fn new_done_marker(source_event: Rc<SslEvent>) -> Self {
        Self {
            id: None,
            event: None,
            retry: None,
            data_offset: 0,
            data_len: 0,
            source_event,
            is_synthetic_done: true,
        }
    }

    /// Parse data as JSON value
    pub fn json_body(&self) -> Option<serde_json::Value> {
        serde_json::from_slice::<serde_json::Value>(self.data()).ok()
    }

    /// Get data (zero-copy)
    pub fn data(&self) -> &[u8] {
        let buf_len = self.source_event.buf.len();
        let start = self.data_offset.min(buf_len);
        let end = (self.data_offset + self.data_len).min(buf_len);
        &self.source_event.buf[start..end]
    }

    pub fn body_str(&self) -> &str {
        std::str::from_utf8(self.data()).unwrap_or("")
    }

    /// Check if this is a completion marker
    ///
    /// Recognizes:
    /// - OpenAI style: data is `[DONE]` or `[END]`
    /// - Anthropic style: event field is `message_stop`, or data is `{"type":"message_stop"}`
    /// - OpenAI Responses API: data contains parseable JSON with
    ///   `"type":"response.completed"` (or `.failed`/`.incomplete`).
    ///   The `event:` field alone does NOT terminate — the data payload of
    ///   `response.completed` routinely exceeds 16 KB and spans multiple TLS
    ///   records, so the stream must stay open until the full JSON is available
    ///   or the HTTP chunked terminator (`0\r\n\r\n`) fires a synthetic done.
    /// - DashScope/Bailian native protocol: a terminal `finish_reason` inside
    ///   the `output` **object**. It never sends `data: [DONE]`.
    pub fn is_done(&self) -> bool {
        if self.is_synthetic_done {
            return true;
        }
        // Anthropic SSE: event field is "message_stop"
        if self.event.as_deref() == Some("message_stop") {
            return true;
        }
        // OpenAI Responses API: do NOT terminate based on event field alone.
        // The `response.completed` data field routinely exceeds 16KB (echoes
        // instructions + tools + output), so the first TLS chunk only has
        // the event header + truncated data. The stream must stay open to
        // receive remaining chunks; it will terminate via the HTTP chunked
        // end marker `0\r\n\r\n` (synthetic done) or when the data-field
        // JSON is fully parseable (check below).
        let data = self.data();
        let text = String::from_utf8_lossy(data);
        let trimmed = text.trim();
        // OpenAI style
        if trimmed == "[DONE]" || trimmed == "[END]" {
            return true;
        }
        // Anthropic style: data contains {"type":"message_stop"}
        // Responses API style: data contains {"type":"response.completed",...}
        if trimmed.starts_with('{') {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(trimmed) {
                let t = v.get("type").and_then(|t| t.as_str());
                if t == Some("message_stop")
                    || t == Some("response.completed")
                    || t == Some("response.failed")
                    || t == Some("response.incomplete")
                {
                    return true;
                }
                if Self::is_dashscope_native_done(&v) {
                    return true;
                }
            }
        }
        false
    }

    /// DashScope/Bailian native protocol terminator detection.
    ///
    /// The end of stream is signalled by a terminal `finish_reason`, either at
    /// `output.finish_reason` (`result_format: text`) or
    /// `output.choices[].finish_reason` (`result_format: message`). In-progress
    /// chunks carry the *literal string* `"null"` there, so only the known
    /// terminal values below are accepted — an unrecognised value falls back to
    /// the HTTP chunked terminator rather than truncating the stream early.
    ///
    /// `output` must be an object: the OpenAI Responses API also uses the key,
    /// but as an array of output items.
    fn is_dashscope_native_done(v: &serde_json::Value) -> bool {
        fn is_terminal(node: &serde_json::Value) -> bool {
            matches!(
                node.get("finish_reason").and_then(|r| r.as_str()),
                Some("stop" | "length" | "tool_calls" | "content_filter")
            )
        }

        let Some(output) = v.get("output").filter(|o| o.is_object()) else {
            return false;
        };
        if is_terminal(output) {
            return true;
        }
        output
            .get("choices")
            .and_then(|c| c.as_array())
            .is_some_and(|choices| choices.iter().any(is_terminal))
    }

    /// Get data length
    pub fn data_len(&self) -> usize {
        self.data_len
    }

    /// Get reference to source SslEvent
    pub fn source_event(&self) -> &SslEvent {
        &self.source_event
    }
}

impl fmt::Debug for ParsedSseEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut debug = f.debug_struct("ParsedSseEvent");

        if let Some(ref id) = self.id {
            debug.field("id", id);
        }
        if let Some(ref event) = self.event {
            debug.field("event", event);
        }
        if let Some(retry) = self.retry {
            debug.field("retry", &retry);
        }

        // Check if this is a done marker
        if self.is_done() {
            debug.field("done", &true);
        }

        // Format data with smart detection
        let data = self.data();
        if !data.is_empty() {
            debug.field("data", &format_sse_data(data));
        }

        // Add metadata
        debug
            .field("data_len", &self.data_len)
            .field("pid", &self.source_event.pid)
            .field("timestamp_ns", &self.source_event.timestamp_ns);

        debug.finish()
    }
}

/// Format SSE data for debug output
fn format_sse_data(data: &[u8]) -> String {
    // Try JSON first
    if let Ok(json) = serde_json::from_slice::<serde_json::Value>(data) {
        let formatted = serde_json::to_string_pretty(&json).unwrap_or_default();
        format!("(json, {} bytes)\n{}", data.len(), formatted)
    } else if let Ok(text) = std::str::from_utf8(data) {
        // Text content
        let text = text.trim();
        format!("(text, {} bytes)\n{}", data.len(), text)
    } else {
        // Binary data - show as base64
        format!(
            "(binary, {} bytes)\n{}",
            data.len(),
            base64::Engine::encode(&base64::engine::general_purpose::STANDARD, data)
        )
    }
}

/// SSE Events container - holds one or more SSE events from a parse operation
#[derive(Debug, Clone)]
pub struct SSEEvents {
    /// Parsed SSE events
    pub events: Vec<SSEEvent>,
    /// Unconsumed buffer data (incomplete event at end)
    pub remaining: String,
    /// Total number of bytes consumed from input
    pub consumed_bytes: usize,
}

impl Default for SSEEvents {
    fn default() -> Self {
        Self::new()
    }
}

impl SSEEvents {
    /// Create a new empty SSEEvents container
    pub fn new() -> Self {
        SSEEvents {
            events: Vec::new(),
            remaining: String::new(),
            consumed_bytes: 0,
        }
    }

    /// Check if any events were parsed
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    /// Get the number of parsed events
    pub fn len(&self) -> usize {
        self.events.len()
    }

    /// Convert all SSE events to Chrome Trace Events
    pub fn to_chrome_trace_events(
        &self,
        pid: u32,
        tid: u64,
        base_timestamp_ns: u64,
    ) -> Vec<ChromeTraceEvent> {
        self.events
            .iter()
            .map(|event| event.to_chrome_trace_event(pid, tid, base_timestamp_ns))
            .collect()
    }

    /// Take the events, leaving the container empty
    pub fn take_events(&mut self) -> Vec<SSEEvent> {
        std::mem::take(&mut self.events)
    }

    /// Convert SSEEvents to a single Chrome Trace Event
    ///
    /// This aggregates all SSE events into one trace event with:
    /// - Event count in name
    /// - Combined data from all events
    /// - Total data size
    pub fn to_chrome_trace_event(
        &self,
        pid: u32,
        tid: u64,
        timestamp_ns: u64,
    ) -> Option<ChromeTraceEvent> {
        if self.events.is_empty() {
            return None;
        }

        // Build event name with count
        let name = format!("SSE Stream ({} events)", self.events.len());

        // Build args with aggregated information
        let mut args = serde_json::Map::new();
        args.insert(
            "event_count".to_string(),
            serde_json::json!(self.events.len()),
        );
        args.insert(
            "consumed_bytes".to_string(),
            serde_json::json!(self.consumed_bytes),
        );
        args.insert(
            "remaining_bytes".to_string(),
            serde_json::json!(self.remaining.len()),
        );

        // Aggregate data from all events
        let total_data_size: usize = self.events.iter().map(|e| e.data.len()).sum();
        args.insert(
            "total_data_size".to_string(),
            serde_json::json!(total_data_size),
        );

        // Combine all events' data (no truncation, no limit)
        let all_data: Vec<String> = self
            .events
            .iter()
            .map(|e| format!("[{}] {}", e.event.as_deref().unwrap_or("message"), e.data))
            .collect();

        if !all_data.is_empty() {
            args.insert("data".to_string(), serde_json::json!(all_data));
        }

        // Collect all event types
        let event_types: Vec<&str> = self
            .events
            .iter()
            .filter_map(|e| e.event.as_deref())
            .collect();
        if !event_types.is_empty() {
            args.insert("event_types".to_string(), serde_json::json!(event_types));
        }

        // Convert timestamp from nanoseconds to microseconds
        let ts = ns_to_us(timestamp_ns);

        Some(ChromeTraceEvent {
            name,
            cat: "sse_stream".to_string(),
            ph: "X".to_string(), // Complete event (represents a stream)
            ts,
            dur: Some(0), // Duration would need end timestamp
            pid,
            tid,
            args: Some(serde_json::Value::Object(args)),
            id: None,
            bp: None,
        })
    }
}

impl SSEEvent {
    /// Create a new SSEEvent with just data
    pub fn new(data: impl Into<String>) -> Self {
        SSEEvent {
            id: None,
            event: None,
            data: data.into(),
            retry: None,
        }
    }

    /// Check if this is a "ping" or keepalive event (data is empty and no other fields)
    pub fn is_keepalive(&self) -> bool {
        self.data.is_empty() && self.id.is_none() && self.event.is_none() && self.retry.is_none()
    }

    /// Format as SSE protocol string
    pub fn to_sse_string(&self) -> String {
        let mut result = String::new();

        if let Some(id) = &self.id {
            result.push_str(&format!("id:{id}\n"));
        }
        if let Some(event) = &self.event {
            result.push_str(&format!("event:{event}\n"));
        }
        if let Some(retry) = self.retry {
            result.push_str(&format!("retry:{retry}\n"));
        }

        // Data can be multi-line
        for line in self.data.lines() {
            result.push_str(&format!("data:{line}\n"));
        }

        result.push('\n'); // Empty line to terminate event
        result
    }

    /// Convert SSE Event to Chrome Trace Event format for Perfetto visualization
    ///
    /// # Arguments
    /// * `pid` - Process ID
    /// * `tid` - Thread ID
    /// * `timestamp_ns` - Event timestamp in nanoseconds
    ///
    /// # Returns
    /// A ChromeTraceEvent suitable for visualization in Perfetto
    pub fn to_chrome_trace_event(&self, pid: u32, tid: u64, timestamp_ns: u64) -> ChromeTraceEvent {
        // Build event name based on event type or data preview
        let name = match &self.event {
            Some(event_type) => format!("SSE {event_type}"),
            None => "SSE Message".to_string(),
        };

        // Build args with SSE information
        let mut args = serde_json::Map::new();

        // Add data (truncated for display if very long)
        let data_preview = if self.data.len() > 500 {
            format!("{}... ({} bytes total)", &self.data[..500], self.data.len())
        } else {
            self.data.clone()
        };
        args.insert("data".to_string(), serde_json::json!(data_preview));
        args.insert(
            "data_length".to_string(),
            serde_json::json!(self.data.len()),
        );

        if let Some(id) = &self.id {
            args.insert("id".to_string(), serde_json::json!(id));
        }
        if let Some(event) = &self.event {
            args.insert("event_type".to_string(), serde_json::json!(event));
        }
        if let Some(retry) = self.retry {
            args.insert("retry".to_string(), serde_json::json!(retry));
        }

        // Convert timestamp from nanoseconds to microseconds
        let ts = ns_to_us(timestamp_ns);

        ChromeTraceEvent {
            name,
            cat: "sse".to_string(),
            ph: "i".to_string(), // Instant event (SSE events are point-in-time)
            ts,
            dur: None,
            pid,
            tid,
            args: Some(serde_json::Value::Object(args)),
            id: None,
            bp: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_event(data: &[u8]) -> Rc<SslEvent> {
        Rc::new(SslEvent {
            source: 0,
            timestamp_ns: 5000,
            delta_ns: 0,
            pid: 1,
            tid: 1,
            uid: 0,
            len: data.len() as u32,
            rw: 1,
            comm: "test".to_string(),
            buf: data.to_vec(),
            is_handshake: false,
            ssl_ptr: 0x1,
        })
    }

    #[test]
    fn test_parsed_sse_event_new() {
        let data = b"data: hello\n\n";
        let ev = make_event(data);
        let parsed = ParsedSseEvent::new(None, None, None, 6, 5, ev);
        assert_eq!(parsed.body_str(), "hello");
        assert_eq!(parsed.data_len(), 5);
        assert!(!parsed.is_done());
    }

    #[test]
    fn test_parsed_sse_event_done_marker() {
        let ev = make_event(b"");
        let parsed = ParsedSseEvent::new_done_marker(ev);
        assert!(parsed.is_done());
        assert_eq!(parsed.data_len(), 0);
    }

    #[test]
    fn test_parsed_sse_event_is_done_text() {
        let data = b"[DONE]";
        let ev = make_event(data);
        let parsed = ParsedSseEvent::new(None, None, None, 0, 6, ev);
        assert!(parsed.is_done());
    }

    #[test]
    fn test_parsed_sse_event_is_done_end() {
        let data = b"[END]";
        let ev = make_event(data);
        let parsed = ParsedSseEvent::new(None, None, None, 0, 5, ev);
        assert!(parsed.is_done());
    }

    /// DashScope native protocol never sends `data: [DONE]`. Its terminator is
    /// a real `finish_reason` inside the `output` object; without recognising
    /// it the stream only ends via the chunked terminator, which the
    /// interruption detector may read as `sse_truncated`.
    #[test]
    fn test_parsed_sse_event_is_done_dashscope_native_text_mode() {
        let data = br#"{"output":{"finish_reason":"stop","text":"hi"},"usage":{"input_tokens":1,"output_tokens":1,"total_tokens":2},"request_id":"r1"}"#;
        let ev = make_event(data);
        let parsed = ParsedSseEvent::new(None, None, None, 0, data.len(), ev);
        assert!(parsed.is_done());
    }

    #[test]
    fn test_parsed_sse_event_is_done_dashscope_native_message_mode() {
        let data = br#"{"output":{"choices":[{"finish_reason":"stop","message":{"role":"assistant","content":"hi"}}]},"usage":{"input_tokens":1,"output_tokens":1,"total_tokens":2},"request_id":"r1"}"#;
        let ev = make_event(data);
        let parsed = ParsedSseEvent::new(None, None, None, 0, data.len(), ev);
        assert!(parsed.is_done());
    }

    #[test]
    fn test_parsed_sse_event_is_done_dashscope_native_tool_calls() {
        let data = br#"{"output":{"choices":[{"finish_reason":"tool_calls","message":{"role":"assistant","tool_calls":[]}}]},"request_id":"r1"}"#;
        let ev = make_event(data);
        let parsed = ParsedSseEvent::new(None, None, None, 0, data.len(), ev);
        assert!(parsed.is_done());
    }

    /// In-progress native chunks carry the *literal string* `"null"` as
    /// finish_reason. Treating those as terminal would truncate the stream.
    #[test]
    fn test_parsed_sse_event_not_done_dashscope_native_in_progress() {
        for data in [
            br#"{"output":{"finish_reason":"null","text":"hi"},"request_id":"r1"}"#.as_slice(),
            br#"{"output":{"choices":[{"finish_reason":"null","message":{"content":"h"}}]}}"#
                .as_slice(),
            br#"{"output":{"choices":[{"finish_reason":"","message":{"content":"h"}}]}}"#
                .as_slice(),
            br#"{"output":{"choices":[{"message":{"content":"h"}}]}}"#.as_slice(),
        ] {
            let ev = make_event(data);
            let parsed = ParsedSseEvent::new(None, None, None, 0, data.len(), ev);
            assert!(
                !parsed.is_done(),
                "in-progress native chunk must keep the stream open: {}",
                String::from_utf8_lossy(data)
            );
        }
    }

    /// Regression guard: the Responses API nests `output` as an *array* under
    /// `response`; incremental events must not be mistaken for a native
    /// terminator.
    #[test]
    fn test_parsed_sse_event_not_done_responses_api_output_array() {
        let data = br#"{"type":"response.output_item.done","output_index":0,"output":[{"finish_reason":"stop"}]}"#;
        let ev = make_event(data);
        let parsed = ParsedSseEvent::new(None, None, None, 0, data.len(), ev);
        assert!(!parsed.is_done());
    }

    #[test]
    fn test_is_done_anthropic_message_stop_data() {
        // Anthropic sends data: {"type":"message_stop"}
        let data = b"{\"type\":\"message_stop\"}";
        let ev = make_event(data);
        let parsed = ParsedSseEvent::new(None, None, None, 0, data.len(), ev);
        assert!(parsed.is_done());
    }

    #[test]
    fn test_is_done_anthropic_message_stop_event_field() {
        // Anthropic SSE has event: message_stop field
        let data = b"{\"type\":\"message_stop\"}";
        let ev = make_event(data);
        let parsed = ParsedSseEvent::new(
            None,
            Some("message_stop".to_string()), // event field
            None,
            0,
            data.len(),
            ev,
        );
        assert!(parsed.is_done());
    }

    #[test]
    fn test_is_done_anthropic_event_field_only() {
        // Even with empty data, event=message_stop should trigger done
        let ev = make_event(b"");
        let parsed = ParsedSseEvent::new(None, Some("message_stop".to_string()), None, 0, 0, ev);
        assert!(parsed.is_done());
    }

    #[test]
    fn test_is_done_anthropic_other_event_not_done() {
        // Other Anthropic events (e.g. content_block_delta) should NOT be done
        let data = b"{\"type\":\"content_block_delta\"}";
        let ev = make_event(data);
        let parsed = ParsedSseEvent::new(
            None,
            Some("content_block_delta".to_string()),
            None,
            0,
            data.len(),
            ev,
        );
        assert!(!parsed.is_done());
    }

    #[test]
    fn test_is_done_responses_api_completed() {
        let data = b"{\"type\":\"response.completed\",\"response\":{\"id\":\"resp_x\"}}";
        let ev = make_event(data);
        let parsed = ParsedSseEvent::new(None, None, None, 0, data.len(), ev);
        assert!(parsed.is_done());
    }

    #[test]
    fn test_is_done_responses_api_failed() {
        let data = b"{\"type\":\"response.failed\"}";
        let ev = make_event(data);
        let parsed = ParsedSseEvent::new(None, None, None, 0, data.len(), ev);
        assert!(parsed.is_done());
    }

    #[test]
    fn test_is_done_responses_api_incomplete() {
        let data = b"{\"type\":\"response.incomplete\"}";
        let ev = make_event(data);
        let parsed = ParsedSseEvent::new(None, None, None, 0, data.len(), ev);
        assert!(parsed.is_done());
    }

    #[test]
    fn test_is_done_responses_api_delta_not_done() {
        let data = b"{\"type\":\"response.output_text.delta\",\"delta\":\"hi\"}";
        let ev = make_event(data);
        let parsed = ParsedSseEvent::new(None, None, None, 0, data.len(), ev);
        assert!(!parsed.is_done());
    }

    #[test]
    fn test_is_done_responses_api_event_field_alone_not_done() {
        // The `event: response.completed` field alone must NOT terminate the
        // stream — the data field may be truncated across TLS records and the
        // full JSON (containing usage) has not arrived yet. The stream
        // terminates via the HTTP chunked terminator `0\r\n\r\n`.
        let data = b"{\"sequence_number\":10,\"type\":\"response.completed\",\"response\":{\"instructions\":\"trunc";
        let ev = make_event(data);
        let parsed = ParsedSseEvent::new(
            None,
            Some("response.completed".to_string()),
            None,
            0,
            data.len(),
            ev,
        );
        assert!(!parsed.is_done());
    }

    #[test]
    fn test_is_done_responses_api_complete_json_is_done() {
        // When the full JSON IS available in data, is_done should fire.
        let data = b"{\"type\":\"response.completed\",\"response\":{\"status\":\"completed\"}}";
        let ev = make_event(data);
        let parsed = ParsedSseEvent::new(
            None,
            Some("response.completed".to_string()),
            None,
            0,
            data.len(),
            ev,
        );
        assert!(parsed.is_done());
    }

    #[test]
    fn test_parsed_sse_event_json_body() {
        let data = b"{\"key\":\"value\"}";
        let ev = make_event(data);
        let parsed = ParsedSseEvent::new(None, None, None, 0, data.len(), ev);
        let json = parsed.json_body().unwrap();
        assert_eq!(json["key"], "value");
    }

    #[test]
    fn test_parsed_sse_event_source_event() {
        let ev = make_event(b"test");
        let parsed = ParsedSseEvent::new(None, None, None, 0, 4, ev);
        assert_eq!(parsed.source_event().pid, 1);
    }

    #[test]
    fn test_sse_event_new() {
        let e = SSEEvent::new("hello world");
        assert_eq!(e.data, "hello world");
        assert!(e.id.is_none());
        assert!(e.event.is_none());
    }

    #[test]
    fn test_sse_event_is_keepalive() {
        let e = SSEEvent {
            id: None,
            event: None,
            data: String::new(),
            retry: None,
        };
        assert!(e.is_keepalive());

        let e2 = SSEEvent::new("data");
        assert!(!e2.is_keepalive());
    }

    #[test]
    fn test_sse_event_to_sse_string() {
        let e = SSEEvent {
            id: Some("123".to_string()),
            event: Some("update".to_string()),
            data: "line1\nline2".to_string(),
            retry: Some(5000),
        };
        let s = e.to_sse_string();
        assert!(s.contains("id:123"));
        assert!(s.contains("event:update"));
        assert!(s.contains("retry:5000"));
        assert!(s.contains("data:line1"));
        assert!(s.contains("data:line2"));
    }

    #[test]
    fn test_sse_event_to_chrome_trace_event() {
        let e = SSEEvent {
            id: Some("1".to_string()),
            event: Some("delta".to_string()),
            data: "content".to_string(),
            retry: None,
        };
        let trace = e.to_chrome_trace_event(10, 20, 1000);
        assert_eq!(trace.name, "SSE delta");
        assert_eq!(trace.cat, "sse");
        assert_eq!(trace.ph, "i");
        assert_eq!(trace.ts, 1); // 1000ns = 1us
    }

    #[test]
    fn test_sse_events_container() {
        let mut container = SSEEvents::new();
        assert!(container.is_empty());
        assert_eq!(container.len(), 0);

        container.events.push(SSEEvent::new("event1"));
        container.events.push(SSEEvent::new("event2"));
        assert!(!container.is_empty());
        assert_eq!(container.len(), 2);
    }

    #[test]
    fn test_sse_events_take_events() {
        let mut container = SSEEvents::new();
        container.events.push(SSEEvent::new("data"));
        let events = container.take_events();
        assert_eq!(events.len(), 1);
        assert!(container.is_empty());
    }

    #[test]
    fn test_sse_events_to_chrome_trace_event() {
        let mut container = SSEEvents::new();
        assert!(container.to_chrome_trace_event(1, 1, 1000).is_none());

        container.events.push(SSEEvent::new("data1"));
        container.events.push(SSEEvent {
            id: None,
            event: Some("delta".to_string()),
            data: "data2".to_string(),
            retry: None,
        });
        container.consumed_bytes = 100;

        let trace = container.to_chrome_trace_event(1, 2, 2000).unwrap();
        assert!(trace.name.contains("2 events"));
        assert_eq!(trace.cat, "sse_stream");
    }

    #[test]
    fn test_sse_events_to_chrome_trace_events() {
        let mut container = SSEEvents::new();
        container.events.push(SSEEvent::new("e1"));
        container.events.push(SSEEvent::new("e2"));
        let traces = container.to_chrome_trace_events(1, 1, 0);
        assert_eq!(traces.len(), 2);
    }

    #[test]
    fn test_format_sse_data_json() {
        let data = b"{\"model\":\"gpt-4\"}";
        let result = format_sse_data(data);
        assert!(result.contains("json"));
    }

    #[test]
    fn test_format_sse_data_text() {
        let result = format_sse_data(b"plain text");
        assert!(result.contains("text"));
    }

    #[test]
    fn test_parsed_sse_event_debug() {
        let data = b"{\"k\":\"v\"}";
        let ev = make_event(data);
        let parsed = ParsedSseEvent::new(
            Some("id1".to_string()),
            Some("message".to_string()),
            Some(3000),
            0,
            data.len(),
            ev,
        );
        let debug = format!("{parsed:?}");
        assert!(debug.contains("id1"));
        assert!(debug.contains("message"));
    }
}
