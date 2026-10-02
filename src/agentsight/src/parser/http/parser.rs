//! HTTP Parser - 无状态 HTTP 解析器

use super::request::ParsedRequest;
use super::response::ParsedResponse;
use crate::probes::sslsniff::SslEvent;
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::rc::Rc;

/// 最大 headers 数量
const MAX_HEADERS: usize = 64;

/// 解析后的 HTTP 消息
#[derive(Debug, Clone)]
pub enum ParsedHttpMessage {
    Request(ParsedRequest),
    Response(ParsedResponse),
}

/// HTTP 解析器（无状态）
#[derive(Debug, Default)]
pub struct HttpParser;

impl HttpParser {
    /// 创建新的解析器实例
    pub fn new() -> Self {
        Self
    }

    /// 解析 SslEvent，返回 Request 或 Response
    pub fn parse(&self, event: Rc<SslEvent>) -> Result<ParsedHttpMessage> {
        // 只使用实际数据长度，而非整个 buf 数组
        let data_len = event.buf_size() as usize;
        let data = &event.buf[..data_len];

        // 尝试解析为 Request
        match Self::parse_request(data, &event) {
            Ok(req) => return Ok(ParsedHttpMessage::Request(req)),
            Err(e) => log::trace!("Failed to parse as HTTP request: {e}"),
        }

        // 尝试解析为 Response
        match Self::parse_response(data, &event) {
            Ok(resp) => return Ok(ParsedHttpMessage::Response(resp)),
            Err(e) => log::trace!("Failed to parse as HTTP response: {e}"),
        }

        Err(anyhow::anyhow!(
            "Failed to parse HTTP message (len={data_len}), not a valid HTTP request or response, raw data: {event:?}"
        ))
    }

    /// 解析 HTTP Request
    fn parse_request(data: &[u8], event: &Rc<SslEvent>) -> Result<ParsedRequest> {
        let mut headers = [httparse::EMPTY_HEADER; MAX_HEADERS];
        let mut req = httparse::Request::new(&mut headers);

        let header_end = match req.parse(data)? {
            httparse::Status::Complete(n) => n,
            httparse::Status::Partial => {
                return Err(anyhow::anyhow!(
                    "HTTP request parsing incomplete (partial data)"
                ));
            }
        };

        let method = req.method.context("Missing HTTP method")?.to_string();
        let path = req.path.context("Missing HTTP path")?.to_string();
        let version = req.version.context("Missing HTTP version")?;

        let parsed_headers: HashMap<String, String> = req
            .headers
            .iter()
            .map(|h| {
                let key = h.name.to_lowercase();
                let value = String::from_utf8_lossy(h.value).to_string();
                (key, value)
            })
            .collect();

        let body_len = data.len().saturating_sub(header_end);

        Ok(ParsedRequest {
            method,
            path,
            version,
            headers: parsed_headers,
            body_offset: header_end,
            body_len,
            source_event: Rc::clone(event),
            reassembled_body: None,
        })
    }

    /// 解析 HTTP Response
    fn parse_response(data: &[u8], event: &Rc<SslEvent>) -> Result<ParsedResponse> {
        let mut headers = [httparse::EMPTY_HEADER; MAX_HEADERS];
        let mut resp = httparse::Response::new(&mut headers);

        let header_end = match resp.parse(data)? {
            httparse::Status::Complete(n) => n,
            httparse::Status::Partial => {
                return Err(anyhow::anyhow!(
                    "HTTP response parsing incomplete (partial data)"
                ));
            }
        };

        let version = resp.version.context("Missing HTTP version")?;
        let status_code = resp.code.context("Missing HTTP status code")?;
        let reason = resp.reason.context("Missing HTTP reason")?.to_string();

        let parsed_headers: HashMap<String, String> = resp
            .headers
            .iter()
            .map(|h| {
                let key = h.name.to_lowercase();
                let value = String::from_utf8_lossy(h.value).to_string();
                (key, value)
            })
            .collect();

        let body_len = data.len().saturating_sub(header_end);

        Ok(ParsedResponse {
            version,
            status_code,
            reason,
            headers: parsed_headers,
            body_offset: header_end,
            body_len,
            source_event: Rc::clone(event),
        })
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
    fn test_parse_http_request() {
        let data = b"POST /v1/chat/completions HTTP/1.1\r\nHost: api.openai.com\r\nContent-Type: application/json\r\n\r\n{\"model\":\"gpt-4\"}";
        let event = make_ssl_event(data);
        let parser = HttpParser::new();
        let result = parser.parse(event);
        assert!(result.is_ok());
        match result.unwrap() {
            ParsedHttpMessage::Request(req) => {
                assert_eq!(req.method, "POST");
                assert_eq!(req.path, "/v1/chat/completions");
                assert_eq!(req.headers.get("host").unwrap(), "api.openai.com");
                assert_eq!(req.headers.get("content-type").unwrap(), "application/json");
                assert!(req.body_len > 0);
            }
            _ => panic!("Expected Request"),
        }
    }

    #[test]
    fn test_parse_http_response() {
        let data =
            b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n{\"id\":\"chatcmpl-123\"}";
        let event = make_ssl_event(data);
        let parser = HttpParser::new();
        let result = parser.parse(event);
        assert!(result.is_ok());
        match result.unwrap() {
            ParsedHttpMessage::Response(resp) => {
                assert_eq!(resp.status_code, 200);
                assert_eq!(resp.reason, "OK");
                assert_eq!(
                    resp.headers.get("content-type").unwrap(),
                    "application/json"
                );
                assert!(resp.body_len > 0);
            }
            _ => panic!("Expected Response"),
        }
    }

    #[test]
    fn test_parse_get_request() {
        let data = b"GET /health HTTP/1.1\r\nHost: localhost\r\n\r\n";
        let event = make_ssl_event(data);
        let parser = HttpParser::new();
        let result = parser.parse(event);
        assert!(result.is_ok());
        match result.unwrap() {
            ParsedHttpMessage::Request(req) => {
                assert_eq!(req.method, "GET");
                assert_eq!(req.path, "/health");
            }
            _ => panic!("Expected Request"),
        }
    }

    #[test]
    fn test_parse_404_response() {
        let data = b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n";
        let event = make_ssl_event(data);
        let parser = HttpParser::new();
        let result = parser.parse(event);
        assert!(result.is_ok());
        match result.unwrap() {
            ParsedHttpMessage::Response(resp) => {
                assert_eq!(resp.status_code, 404);
                assert_eq!(resp.reason, "Not Found");
            }
            _ => panic!("Expected Response"),
        }
    }

    #[test]
    fn test_parse_invalid_data() {
        let data = b"not http at all";
        let event = make_ssl_event(data);
        let parser = HttpParser::new();
        let result = parser.parse(event);
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_multiple_headers() {
        let data = b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: keep-alive\r\n\r\n";
        let event = make_ssl_event(data);
        let parser = HttpParser::new();
        let result = parser.parse(event);
        assert!(result.is_ok());
        match result.unwrap() {
            ParsedHttpMessage::Response(resp) => {
                assert_eq!(
                    resp.headers.get("content-type").unwrap(),
                    "text/event-stream"
                );
                assert_eq!(resp.headers.get("transfer-encoding").unwrap(), "chunked");
                assert_eq!(resp.headers.get("connection").unwrap(), "keep-alive");
            }
            _ => panic!("Expected Response"),
        }
    }

    #[test]
    fn test_ssl_event_is_http_request() {
        let event = SslEvent {
            source: 0,
            timestamp_ns: 0,
            delta_ns: 0,
            pid: 1,
            tid: 1,
            uid: 0,
            len: 10,
            rw: 1,
            comm: String::new(),
            buf: b"POST /api HTTP/1.1\r\n".to_vec(),
            is_handshake: false,
            ssl_ptr: 0,
        };
        assert!(event.is_http_request());
        assert!(event.is_http());
        assert!(!event.is_http_response());
    }

    #[test]
    fn test_ssl_event_is_http_response() {
        let event = SslEvent {
            source: 0,
            timestamp_ns: 0,
            delta_ns: 0,
            pid: 1,
            tid: 1,
            uid: 0,
            len: 15,
            rw: 0,
            comm: String::new(),
            buf: b"HTTP/1.1 200 OK\r\n".to_vec(),
            is_handshake: false,
            ssl_ptr: 0,
        };
        assert!(event.is_http_response());
        assert!(event.is_http());
        assert!(!event.is_http_request());
    }

    #[test]
    fn test_ssl_event_is_http2_preface() {
        let event = SslEvent {
            source: 0,
            timestamp_ns: 0,
            delta_ns: 0,
            pid: 1,
            tid: 1,
            uid: 0,
            len: 24,
            rw: 1,
            comm: String::new(),
            buf: b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n".to_vec(),
            is_handshake: false,
            ssl_ptr: 0,
        };
        assert!(event.is_http2_preface());
        assert!(event.is_http2());
    }

    #[test]
    fn test_ssl_event_is_http2_frame() {
        // Valid HTTP/2 frame: length=0, type=4(SETTINGS), flags=0, stream_id=0
        let event = SslEvent {
            source: 0,
            timestamp_ns: 0,
            delta_ns: 0,
            pid: 1,
            tid: 1,
            uid: 0,
            len: 9,
            rw: 0,
            comm: String::new(),
            buf: vec![0, 0, 0, 4, 0, 0, 0, 0, 0],
            is_handshake: false,
            ssl_ptr: 0,
        };
        assert!(event.is_http2_frame());
        assert!(event.is_http2());
    }

    #[test]
    fn test_ssl_event_not_http2_frame_bad_type() {
        // Frame type > 9 is invalid
        let event = SslEvent {
            source: 0,
            timestamp_ns: 0,
            delta_ns: 0,
            pid: 1,
            tid: 1,
            uid: 0,
            len: 9,
            rw: 0,
            comm: String::new(),
            buf: vec![0, 0, 0, 10, 0, 0, 0, 0, 0],
            is_handshake: false,
            ssl_ptr: 0,
        };
        assert!(!event.is_http2_frame());
    }

    #[test]
    fn test_ssl_event_payload_and_helpers() {
        let event = SslEvent {
            source: 0,
            timestamp_ns: 100,
            delta_ns: 0,
            pid: 42,
            tid: 42,
            uid: 0,
            len: 5,
            rw: 0,
            comm: "curl".to_string(),
            buf: b"hello".to_vec(),
            is_handshake: false,
            ssl_ptr: 0x2000,
        };
        assert_eq!(event.payload(), Some("hello"));
        assert_eq!(event.comm_str(), "curl");
        assert_eq!(event.ssl_ptr(), 0x2000);
        assert_eq!(event.connection_id(), (42, 0x2000));
        assert_eq!(event.buf_size(), 5);
    }

    #[test]
    fn test_chunked_gzip_response_body_decompresses() {
        use std::io::Write;
        let json_body =
            b"{\"id\":\"chatcmpl-123\",\"choices\":[{\"message\":{\"content\":\"hello\"}}]}";
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(json_body).unwrap();
        let gzipped = encoder.finish().unwrap();

        let mut body = Vec::new();
        body.extend_from_slice(format!("{:x}\r\n", gzipped.len()).as_bytes());
        body.extend_from_slice(&gzipped);
        body.extend_from_slice(b"\r\n0\r\n\r\n");

        let mut data = Vec::new();
        data.extend_from_slice(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Encoding: gzip\r\nTransfer-Encoding: chunked\r\n\r\n");
        data.extend_from_slice(&body);

        let event = make_ssl_event(&data);
        let parser = HttpParser::new();
        let result = parser.parse(event).unwrap();
        match result {
            ParsedHttpMessage::Response(resp) => {
                assert_eq!(resp.status_code, 200);
                assert!(resp.body_len > 0);
                let parsed = resp.json_body();
                assert!(
                    parsed.is_some(),
                    "chunked+gzip body should decompress to valid JSON"
                );
                let val = parsed.unwrap();
                assert_eq!(val["id"], "chatcmpl-123");
            }
            _ => panic!("Expected Response"),
        }
    }
}
