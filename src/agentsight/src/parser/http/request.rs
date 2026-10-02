#![allow(clippy::same_item_push)]
//! HTTP Request types

use crate::chrome_trace::{ChromeTraceEvent, ToChromeTraceEvent, TraceArgs, ns_to_us};
use crate::probes::sslsniff::SslEvent;
use serde_json::json;
use std::collections::HashMap;
use std::fmt;
use std::rc::Rc;

/// 解析后的 HTTP Request
#[derive(Clone)]
pub struct ParsedRequest {
    pub method: String, // GET, POST, etc.
    pub path: String,   // /api/chat
    pub version: u8,    // 11 for HTTP/1.1
    pub headers: HashMap<String, String>,
    pub body_offset: usize,         // body 在 source_event.buf 中的起始位置
    pub body_len: usize,            // body 长度
    pub source_event: Rc<SslEvent>, // 原始 SslEvent (Rc 避免拷贝)
    /// 重组后的完整 body（跨多事件聚合时使用）
    pub reassembled_body: Option<Vec<u8>>,
}

impl ParsedRequest {
    /// 获取 body 数据（零拷贝，或返回重组后的 body）
    pub fn body(&self) -> &[u8] {
        if let Some(ref buf) = self.reassembled_body {
            buf
        } else {
            &self.source_event.buf[self.body_offset..self.body_offset + self.body_len]
        }
    }

    pub fn body_str(&self) -> &str {
        std::str::from_utf8(self.body()).unwrap_or("")
    }

    /// 尝试将 body 解析为 JSON
    ///
    /// 如果 body 是有效的 UTF-8 且是有效的 JSON，返回解析后的 Value。
    /// 如果直接解析失败，会尝试剥离 HTTP chunked transfer encoding 后再解析。
    pub fn json_body(&self) -> Option<serde_json::Value> {
        let body = self.body();
        if body.is_empty() {
            return None;
        }
        let body_str = String::from_utf8_lossy(body);

        // Try direct JSON parse first
        if let Ok(v) = serde_json::from_str(&body_str) {
            return Some(v);
        }

        // Fallback: try stripping HTTP chunked transfer encoding
        // Format: {hex_size}\r\n{data}\r\n...0\r\n\r\n
        Self::decode_chunked_json(&body_str)
    }

    /// Decode HTTP chunked transfer encoding and parse as JSON
    ///
    /// All slicing uses `str::get(..)` so that arbitrary binary bodies (e.g.
    /// OpenTelemetry Protobuf streams that we converted via
    /// `from_utf8_lossy`) can't panic with "byte index N is not a char
    /// boundary" when the parsed chunk size happens to point into the middle
    /// of a multi-byte `U+FFFD` replacement char. In those cases we simply
    /// abandon the chunked-decode attempt and return `None`, which the caller
    /// treats as "not JSON".
    fn decode_chunked_json(body: &str) -> Option<serde_json::Value> {
        let mut decoded = String::new();
        let mut remaining = body;

        loop {
            // Find the chunk size line
            let newline_pos = remaining.find("\r\n")?;
            let size_str = remaining.get(..newline_pos)?;
            let chunk_size = usize::from_str_radix(size_str.trim(), 16).ok()?;

            if chunk_size == 0 {
                break; // End of chunks
            }

            let data_start = newline_pos + 2;
            let data_end = data_start.checked_add(chunk_size)?;
            if data_end > remaining.len() {
                // Partial chunk — decode what we have (still guarded against
                // landing inside a multi-byte char from from_utf8_lossy).
                decoded.push_str(remaining.get(data_start..)?);
                break;
            }
            decoded.push_str(remaining.get(data_start..data_end)?);

            // Skip past chunk data and trailing \r\n
            remaining = remaining.get(data_end..)?;
            if remaining.starts_with("\r\n") {
                remaining = remaining.get(2..)?;
            }
        }

        if decoded.is_empty() {
            return None;
        }

        serde_json::from_str(&decoded).ok()
    }
}

impl TraceArgs for ParsedRequest {
    fn to_trace_args(&self) -> serde_json::Value {
        let mut args = serde_json::Map::new();

        // Basic request info
        args.insert("method".to_string(), json!(&self.method));
        args.insert("path".to_string(), json!(&self.path));
        args.insert(
            "version".to_string(),
            json!(format!("HTTP/1.{}", self.version)),
        );

        // Process info
        args.insert("pid".to_string(), json!(self.source_event.pid));
        args.insert("tid".to_string(), json!(self.source_event.tid));
        args.insert("comm".to_string(), json!(self.source_event.comm_str()));

        // Add headers if present
        if !self.headers.is_empty() {
            args.insert("headers".to_string(), json!(&self.headers));
        }

        // Add body info if present
        if self.body_len > 0 {
            args.insert("body_length".to_string(), json!(self.body_len));

            // Try to parse as JSON first, fallback to full string
            if let Some(json_body) = self.json_body() {
                args.insert("body".to_string(), json_body);
            } else {
                let body_str = String::from_utf8_lossy(self.body()).to_string();
                if !body_str.is_empty() {
                    args.insert("body".to_string(), json!(body_str));
                }
            }
        }

        serde_json::Value::Object(args)
    }
}

impl ToChromeTraceEvent for ParsedRequest {
    fn to_chrome_trace_events(&self) -> Vec<ChromeTraceEvent> {
        let ts_us = ns_to_us(self.source_event.timestamp_ns);

        // Minimum duration: 10ms = 10,000 microseconds
        const MIN_DUR_US: u64 = 10_000;

        let event = ChromeTraceEvent::complete(
            format!("{} {}", self.method, self.path),
            "http.request",
            self.source_event.pid,
            self.source_event.tid as u64,
            ts_us,
            MIN_DUR_US,
        )
        .with_trace_args(self);

        vec![event]
    }
}

impl fmt::Debug for ParsedRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut debug = f.debug_struct("ParsedRequest");
        debug
            .field("method", &self.method)
            .field("path", &self.path)
            .field("version", &format!("HTTP/1.{}", self.version));

        // Format headers
        debug.field("headers", &self.headers);

        // Format body with smart detection
        let body = self.body();
        if !body.is_empty() {
            debug.field("body", &format_body(body));
        }

        // Add metadata from source_event
        debug
            .field("pid", &self.source_event.pid)
            .field("tid", &self.source_event.tid)
            .field("timestamp_ns", &self.source_event.timestamp_ns);

        debug.finish()
    }
}

/// Format body data for debug output
fn format_body(data: &[u8]) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn make_ssl_event(data: &[u8]) -> Rc<SslEvent> {
        Rc::new(SslEvent {
            source: 0,
            timestamp_ns: 1000,
            delta_ns: 0,
            pid: 100,
            tid: 100,
            uid: 1000,
            len: data.len() as u32,
            rw: 0,
            comm: "test".to_string(),
            buf: data.to_vec(),
            is_handshake: false,
            ssl_ptr: 0x1000,
        })
    }

    #[test]
    fn test_parsed_request_body_str() {
        let body = b"POST /api HTTP/1.1\r\nContent-Length: 5\r\n\r\nhello";
        let event = make_ssl_event(body);
        let req = ParsedRequest {
            method: "POST".to_string(),
            path: "/api".to_string(),
            version: 1,
            headers: HashMap::new(),
            body_offset: body.len() - 5,
            body_len: 5,
            source_event: event,
            reassembled_body: None,
        };
        assert_eq!(req.body_str(), "hello");
        assert_eq!(req.body(), b"hello");
    }

    #[test]
    fn test_parsed_request_json_body() {
        let json_str = r#"{"key":"value"}"#;
        let full = format!("POST / HTTP/1.1\r\n\r\n{json_str}");
        let bytes = full.as_bytes();
        let event = make_ssl_event(bytes);
        let body_offset = bytes.len() - json_str.len();
        let req = ParsedRequest {
            method: "POST".to_string(),
            path: "/".to_string(),
            version: 1,
            headers: HashMap::new(),
            body_offset,
            body_len: json_str.len(),
            source_event: event,
            reassembled_body: None,
        };
        let val = req.json_body().unwrap();
        assert_eq!(val["key"], "value");
    }

    #[test]
    fn test_parsed_request_json_body_empty() {
        let event = make_ssl_event(b"GET / HTTP/1.1\r\n\r\n");
        let req = ParsedRequest {
            method: "GET".to_string(),
            path: "/".to_string(),
            version: 1,
            headers: HashMap::new(),
            body_offset: 0,
            body_len: 0,
            source_event: event,
            reassembled_body: None,
        };
        assert!(req.json_body().is_none());
    }

    #[test]
    fn test_decode_chunked_json() {
        // Standard chunked encoding: "e\r\n{"key":"val"}\r\n0\r\n\r\n"
        let chunked = "e\r\n{\"key\":\"val\"}\r\n0\r\n\r\n";
        let val = ParsedRequest::decode_chunked_json(chunked).unwrap();
        assert_eq!(val["key"], "val");
    }

    #[test]
    fn test_decode_chunked_json_invalid() {
        assert!(ParsedRequest::decode_chunked_json("not chunked").is_none());
    }

    /// Regression: a binary body (e.g. OTLP/Protobuf) passed through
    /// `String::from_utf8_lossy` contains `U+FFFD` replacement chars that are
    /// 3 bytes wide. If the chunk-size parser succeeds by accident and the
    /// computed slice boundary lands inside one of those chars, the old
    /// implementation panicked with "byte index N is not a char boundary".
    /// The current implementation must return `None` instead.
    #[test]
    fn test_decode_chunked_json_binary_body_does_not_panic() {
        // A hex digit + \r\n + arbitrary invalid-UTF8 bytes (rendered as
        // replacement chars by from_utf8_lossy) that intentionally place
        // chunk_size past a multi-byte boundary.
        let mut raw: Vec<u8> = b"c27\r\n".to_vec();
        for _ in 0..4096 {
            raw.push(0xC2); // invalid stray UTF-8 lead byte
        }
        let lossy = String::from_utf8_lossy(&raw);
        // Must not panic; chunked decode should give up and return None.
        assert!(ParsedRequest::decode_chunked_json(&lossy).is_none());
    }

    #[test]
    fn test_trace_args() {
        let body = b"POST /v1/chat/completions HTTP/1.1\r\nHost: api.openai.com\r\n\r\n{\"m\":1}";
        let event = make_ssl_event(body);
        let mut headers = HashMap::new();
        headers.insert("Host".to_string(), "api.openai.com".to_string());
        let req = ParsedRequest {
            method: "POST".to_string(),
            path: "/v1/chat/completions".to_string(),
            version: 1,
            headers,
            body_offset: body.len() - 7,
            body_len: 7,
            source_event: event,
            reassembled_body: None,
        };
        let args = req.to_trace_args();
        assert_eq!(args["method"], "POST");
        assert_eq!(args["path"], "/v1/chat/completions");
    }

    #[test]
    fn test_to_chrome_trace_events() {
        let event = make_ssl_event(b"GET / HTTP/1.1\r\n\r\n");
        let req = ParsedRequest {
            method: "GET".to_string(),
            path: "/health".to_string(),
            version: 1,
            headers: HashMap::new(),
            body_offset: 0,
            body_len: 0,
            source_event: event,
            reassembled_body: None,
        };
        let events = req.to_chrome_trace_events();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].name, "GET /health");
        assert_eq!(events[0].ph, "X");
    }

    #[test]
    fn test_format_body_json() {
        let data = b"{\"hello\":\"world\"}";
        let result = format_body(data);
        assert!(result.contains("json"));
        assert!(result.contains("hello"));
    }

    #[test]
    fn test_format_body_text() {
        let data = b"plain text content";
        let result = format_body(data);
        assert!(result.contains("text"));
    }

    #[test]
    fn test_format_body_binary() {
        let data: &[u8] = &[0xFF, 0xFE, 0xFD, 0x00, 0x01];
        let result = format_body(data);
        assert!(result.contains("binary"));
    }

    #[test]
    fn test_debug_format() {
        let event = make_ssl_event(b"GET / HTTP/1.1\r\n\r\n");
        let req = ParsedRequest {
            method: "GET".to_string(),
            path: "/".to_string(),
            version: 1,
            headers: HashMap::new(),
            body_offset: 0,
            body_len: 0,
            source_event: event,
            reassembled_body: None,
        };
        let debug_str = format!("{req:?}");
        assert!(debug_str.contains("GET"));
    }
}
