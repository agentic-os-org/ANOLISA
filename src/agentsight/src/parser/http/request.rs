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

    /// `Content-Encoding` of the request, the same view `ParsedResponse` uses.
    pub fn content_encoding(&self) -> Option<&str> {
        self.headers.get("content-encoding").map(|e| e.as_str())
    }

    fn is_chunked(&self) -> bool {
        self.headers
            .get("transfer-encoding")
            .map(|v| v.to_lowercase().contains("chunked"))
            .unwrap_or(false)
    }

    /// Body with the chunked transfer encoding removed, or `None` when the
    /// request is not chunked.
    fn dechunked_body(&self) -> Option<Vec<u8>> {
        if !self.is_chunked() {
            return None;
        }
        let dechunked = crate::utils::decompress::dechunk_body(self.body());
        if dechunked.is_empty() && self.body_len > 0 {
            None
        } else {
            Some(dechunked)
        }
    }

    /// Body after chunked transfer encoding and `Content-Encoding` are removed.
    ///
    /// The same chain `ParsedResponse::decompressed_body` runs, so a request
    /// and the response it is paired with are decoded identically.
    pub fn decompressed_body(&self) -> Vec<u8> {
        if let Some(dechunked) = self.dechunked_body() {
            return crate::utils::decompress::decompress_body(&dechunked, self.content_encoding());
        }
        crate::utils::decompress::decompress_body(self.body(), self.content_encoding())
    }

    /// 尝试将 body 解析为 JSON
    ///
    /// 先按原样解析（未编码的 body 直接成功）；失败则走仓库统一的解码链：
    /// 剥掉 chunked 传输编码、再按 `Content-Encoding` 解压，与响应侧
    /// （`ParsedResponse::json_body`）和 HTTP/2 请求侧一致。以前这里用的是
    /// 本文件私有的字符串解码器：它既不解压，也不认 chunk extension
    /// （`1a;ext=1`），而完整性判定用的 `chunked_stream_complete` 是认的，
    /// 于是这类 body 被管线放行、又被唯一的解码器拒收，请求侧整条丢失。
    pub fn json_body(&self) -> Option<serde_json::Value> {
        let body = self.body();
        if body.is_empty() {
            return None;
        }
        if let Ok(v) = serde_json::from_slice(body) {
            return Some(v);
        }
        serde_json::from_slice(&self.decompressed_body()).ok()
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
    fn test_json_body_decodes_chunked_bodies() {
        // Standard chunked encoding: "e\r\n{"key":"val"}\r\n0\r\n\r\n"
        let chunked = "e\r\n{\"key\":\"val\"}\r\n0\r\n\r\n";
        let req = make_request(chunked.as_bytes(), &[("transfer-encoding", "chunked")]);
        let val = req.json_body().expect("chunked body parses");
        assert_eq!(val["key"], "val");
    }

    #[test]
    fn test_json_body_rejects_non_json_bodies() {
        let req = make_request(b"not chunked", &[]);
        assert!(req.json_body().is_none());
    }

    /// Regression: a binary body (e.g. OTLP/Protobuf) must not panic the
    /// decoder. The chunk framing is walked as bytes, so arbitrary
    /// non-UTF-8 content is simply "not JSON" rather than a slice that can
    /// land inside a `U+FFFD` replacement char.
    #[test]
    fn test_json_body_binary_body_does_not_panic() {
        // A hex digit + \r\n + arbitrary invalid-UTF8 bytes that would place a
        // character-based chunk boundary past a multi-byte char.
        let mut raw: Vec<u8> = b"c27\r\n".to_vec();
        for _ in 0..4096 {
            raw.push(0xC2); // invalid stray UTF-8 lead byte
        }
        let req = make_request(&raw, &[("transfer-encoding", "chunked")]);
        // Must not panic; the body is not JSON.
        assert!(req.json_body().is_none());
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

    /// Build a request whose body is exactly `body`.
    fn make_request(body: &[u8], headers: &[(&str, &str)]) -> ParsedRequest {
        let mut full = b"POST /api HTTP/1.1\r\n".to_vec();
        full.extend_from_slice(body);
        let body_offset = full.len() - body.len();
        let event = make_ssl_event(&full);
        ParsedRequest {
            method: "POST".to_string(),
            path: "/api".to_string(),
            version: 1,
            headers: headers
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            body_offset,
            body_len: body.len(),
            source_event: event,
            reassembled_body: None,
        }
    }

    /// A chunk may carry an extension (`1a;ext=1`), which RFC 7230 §4.1 allows
    /// and which the completeness check already tolerates: `HttpAggregator`
    /// decides a chunked request body is complete by walking its framing with
    /// `chunked_stream_complete`, and that walker stops the size line at `;`.
    /// The private decoder then rejected the same body, so `json_body` returned
    /// `None` and the entire request side (messages, tools, model, input
    /// tokens) was lost for a body the pipeline had already admitted.
    #[test]
    fn test_json_body_accepts_chunk_extensions() {
        let json = r#"{"key":"value"}"#;
        let chunked = format!("{:x};ext=1\r\n{json}\r\n0\r\n\r\n", json.len());
        let req = make_request(chunked.as_bytes(), &[("transfer-encoding", "chunked")]);
        let val = req
            .json_body()
            .expect("a chunk extension must not lose the body");
        assert_eq!(val["key"], "value");
    }

    /// A compressed request body arrived at `json_body` verbatim, while the
    /// response side (`ParsedResponse::json_body`) and the HTTP/2 request side
    /// both run the body through `utils::decompress` first. The whole request
    /// side was lost for any client that compresses its prompts.
    #[test]
    fn test_json_body_decompresses_gzip_requests() {
        let json = br#"{"key":"value"}"#;
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        std::io::Write::write_all(&mut encoder, json).expect("gzip fixture");
        let gz = encoder.finish().expect("gzip fixture");
        let req = make_request(&gz, &[("content-encoding", "gzip")]);
        let val = req
            .json_body()
            .expect("a gzip request body must be decompressed");
        assert_eq!(val["key"], "value");
    }
}
