//! HTTP/2 Frame types
//!
//! Defines `Http2FrameType` and `ParsedHttp2Frame` for zero-copy
//! HTTP/2 binary frame representation.

use crate::chrome_trace::{ChromeTraceEvent, ToChromeTraceEvent, TraceArgs, ns_to_us};
use crate::probes::sslsniff::SslEvent;
use hpack::Decoder;
use serde_json::json;
use std::fmt;
use std::rc::Rc;

/// HTTP/2 frame type (RFC 7540 Section 6)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Http2FrameType {
    Data,
    Headers,
    Priority,
    RstStream,
    Settings,
    PushPromise,
    Ping,
    Goaway,
    WindowUpdate,
    Continuation,
    Unknown(u8),
}

impl Http2FrameType {
    /// Parse from raw frame type byte
    pub fn from_u8(value: u8) -> Self {
        match value {
            0 => Http2FrameType::Data,
            1 => Http2FrameType::Headers,
            2 => Http2FrameType::Priority,
            3 => Http2FrameType::RstStream,
            4 => Http2FrameType::Settings,
            5 => Http2FrameType::PushPromise,
            6 => Http2FrameType::Ping,
            7 => Http2FrameType::Goaway,
            8 => Http2FrameType::WindowUpdate,
            9 => Http2FrameType::Continuation,
            v => Http2FrameType::Unknown(v),
        }
    }

    /// Get human-readable name
    pub fn name(&self) -> &'static str {
        match self {
            Http2FrameType::Data => "DATA",
            Http2FrameType::Headers => "HEADERS",
            Http2FrameType::Priority => "PRIORITY",
            Http2FrameType::RstStream => "RST_STREAM",
            Http2FrameType::Settings => "SETTINGS",
            Http2FrameType::PushPromise => "PUSH_PROMISE",
            Http2FrameType::Ping => "PING",
            Http2FrameType::Goaway => "GOAWAY",
            Http2FrameType::WindowUpdate => "WINDOW_UPDATE",
            Http2FrameType::Continuation => "CONTINUATION",
            Http2FrameType::Unknown(_) => "UNKNOWN",
        }
    }
}

/// Parsed HTTP/2 frame (zero-copy)
#[derive(Clone)]
pub struct ParsedHttp2Frame {
    pub frame_type: Http2FrameType,
    pub flags: u8,
    pub stream_id: u32,
    pub payload_offset: usize,
    pub payload_len: usize,
    pub source_event: Rc<SslEvent>,
}

impl ParsedHttp2Frame {
    /// Zero-copy access to frame payload
    pub fn payload(&self) -> &[u8] {
        &self.source_event.buf[self.payload_offset..self.payload_offset + self.payload_len]
    }

    /// Payload as UTF-8 string
    pub fn body_str(&self) -> &str {
        std::str::from_utf8(self.payload()).unwrap_or("")
    }

    /// Try to parse payload as JSON (useful for DATA frames)
    pub fn json_body(&self) -> Option<serde_json::Value> {
        if self.payload_len == 0 {
            return None;
        }
        let body_str = String::from_utf8_lossy(self.payload());
        serde_json::from_str(&body_str).ok()
    }

    pub fn is_data(&self) -> bool {
        self.frame_type == Http2FrameType::Data
    }

    pub fn is_headers(&self) -> bool {
        self.frame_type == Http2FrameType::Headers
    }

    pub fn is_settings(&self) -> bool {
        self.frame_type == Http2FrameType::Settings
    }

    pub fn is_continuation(&self) -> bool {
        self.frame_type == Http2FrameType::Continuation
    }

    /// Check END_STREAM flag (0x01) on DATA/HEADERS frames
    pub fn has_end_stream(&self) -> bool {
        matches!(
            self.frame_type,
            Http2FrameType::Data | Http2FrameType::Headers
        ) && (self.flags & 0x01) != 0
    }

    /// Check END_HEADERS flag (0x04) on HEADERS/CONTINUATION frames
    pub fn has_end_headers(&self) -> bool {
        matches!(
            self.frame_type,
            Http2FrameType::Headers | Http2FrameType::Continuation
        ) && (self.flags & 0x04) != 0
    }

    /// Human-readable frame type name
    pub fn type_name(&self) -> &'static str {
        self.frame_type.name()
    }

    /// Human-readable flags description
    pub fn flags_description(&self) -> String {
        let mut flags = Vec::new();
        match self.frame_type {
            Http2FrameType::Data => {
                if self.flags & 0x01 != 0 {
                    flags.push("END_STREAM");
                }
                if self.flags & 0x08 != 0 {
                    flags.push("PADDED");
                }
            }
            Http2FrameType::Headers => {
                if self.flags & 0x01 != 0 {
                    flags.push("END_STREAM");
                }
                if self.flags & 0x04 != 0 {
                    flags.push("END_HEADERS");
                }
                if self.flags & 0x08 != 0 {
                    flags.push("PADDED");
                }
                if self.flags & 0x20 != 0 {
                    flags.push("PRIORITY");
                }
            }
            Http2FrameType::Settings | Http2FrameType::Ping => {
                if self.flags & 0x01 != 0 {
                    flags.push("ACK");
                }
            }
            Http2FrameType::Continuation => {
                if self.flags & 0x04 != 0 {
                    flags.push("END_HEADERS");
                }
            }
            Http2FrameType::PushPromise => {
                if self.flags & 0x04 != 0 {
                    flags.push("END_HEADERS");
                }
                if self.flags & 0x08 != 0 {
                    flags.push("PADDED");
                }
            }
            _ => {}
        }
        if flags.is_empty() {
            format!("0x{:02x}", self.flags)
        } else {
            flags.join(" | ")
        }
    }

    /// Decode HPACK-encoded headers using a provided decoder
    ///
    /// This method requires a stateful HPACK decoder because HTTP/2 header
    /// compression uses a dynamic table that persists across frames.
    ///
    /// # Arguments
    /// * `decoder` - A mutable reference to an HPACK decoder (maintains dynamic table state)
    ///
    /// # Returns
    /// * `Some(Vec<(String, String)>)` - Decoded header name-value pairs on success
    /// * `None` - If this is not a HEADERS frame or decoding failed
    pub fn decode_headers(&self, decoder: &mut Decoder) -> Option<Vec<(String, String)>> {
        if !self.is_headers() && self.frame_type != Http2FrameType::Continuation {
            return None;
        }

        let payload = self.payload();
        if payload.is_empty() {
            return Some(Vec::new());
        }

        decoder.decode(payload).ok().map(|headers| {
            headers
                .into_iter()
                .map(|(name, value)| {
                    let name_str = String::from_utf8_lossy(&name).to_string();
                    let value_str = String::from_utf8_lossy(&value).to_string();
                    (name_str, value_str)
                })
                .collect()
        })
    }

    /// Decode headers using only the static HPACK table (stateless)
    ///
    /// This method does NOT maintain dynamic table state, so it can only
    /// decode headers that use static table indices. Useful for quick inspection
    /// without tracking connection state.
    ///
    /// # Returns
    /// Decoded header name-value pairs (only static table entries will be resolved)
    pub fn decode_headers_stateless(&self) -> Vec<(String, Option<String>)> {
        if !self.is_headers() && self.frame_type != Http2FrameType::Continuation {
            return Vec::new();
        }

        let payload = self.payload();
        if payload.is_empty() {
            return Vec::new();
        }

        let mut result = Vec::new();
        let mut pos = 0;

        while pos < payload.len() {
            let first_byte = payload[pos];

            // Indexed Header Field (1xxxxxxx) - fully indexed in static or dynamic table
            if first_byte & 0x80 != 0 {
                let index = (first_byte & 0x7F) as usize;
                if let Some((name, value)) = Self::get_static_table_entry(index) {
                    result.push((name.to_string(), Some(value.to_string())));
                } else if index == 0 {
                    break; // Invalid
                } else {
                    // Dynamic table index - cannot decode without state
                    result.push((format!("<dynamic:{index}>"), None));
                }
                pos += 1;
            }
            // Literal Header Field with Incremental Indexing (01xxxxxx)
            else if first_byte & 0xC0 == 0x40 {
                let (name, value, consumed) = self.decode_literal_header(payload, pos, true);
                result.push((name, Some(value)));
                pos += consumed;
            }
            // Dynamic Table Size Update (001xxxxx)
            else if first_byte & 0xE0 == 0x20 {
                // Skip this for stateless decoding
                pos += 1;
            }
            // Literal Header Field without Indexing (0000xxxx) or Never Indexed (0001xxxx)
            else {
                let (name, value, consumed) = self.decode_literal_header(payload, pos, false);
                result.push((name, Some(value)));
                pos += consumed;
            }
        }

        result
    }

    /// Decode a literal header field
    fn decode_literal_header(
        &self,
        payload: &[u8],
        start: usize,
        _indexed: bool,
    ) -> (String, String, usize) {
        let mut pos = start;
        let first_byte = payload[pos];
        pos += 1;

        // Extract name (either from static table or literal)
        let name: String;
        let name_index = (first_byte & 0x3F) as usize;

        if name_index > 0 {
            // Name is in static table
            if let Some((n, _)) = Self::get_static_table_entry(name_index) {
                name = n.to_string();
            } else {
                name = format!("<unknown:{name_index}>");
            }
        } else {
            // Name is literal string
            let (lit_name, consumed) = Self::decode_literal_string(payload, pos);
            name = lit_name;
            pos += consumed;
        }

        // Decode value string
        let (value, consumed) = Self::decode_literal_string(payload, pos);
        pos += consumed;

        (name, value, pos - start)
    }

    /// Decode a literal string (length-prefixed, possibly Huffman encoded)
    fn decode_literal_string(payload: &[u8], start: usize) -> (String, usize) {
        if start >= payload.len() {
            return (String::new(), 0);
        }

        let mut pos = start;
        let first_byte = payload[pos];
        let is_huffman = (first_byte & 0x80) != 0;

        // Decode length (variable length integer, 7 bits in first byte)
        let mut length = (first_byte & 0x7F) as usize;
        pos += 1;

        // RFC 7541 §5.1: a full 7-bit prefix (0x7F) means the value continues
        // in the following bytes, seven more bits per byte. The base computed
        // above is part of the value, so the groups are added, not substituted.
        // A truncated block — and a hostile chain of continuation bytes — is
        // handled by stopping at the payload end and by dropping a shift that
        // has already passed the width of usize, after which the value can only
        // exceed any payload anyway.
        if length == 0x7F && pos < payload.len() {
            let mut shift = 0u32;
            while let Some(&b) = payload.get(pos) {
                pos += 1;
                if shift < usize::BITS {
                    length = length.saturating_add(((b & 0x7F) as usize) << shift);
                }
                shift += 7;
                if b & 0x80 == 0 {
                    break;
                }
            }
        }

        if pos + length > payload.len() {
            return (String::new(), pos - start);
        }

        let string_bytes = &payload[pos..pos + length];

        let result = if is_huffman {
            // Huffman decode
            Self::huffman_decode(string_bytes)
        } else {
            String::from_utf8_lossy(string_bytes).to_string()
        };

        (result, pos + length - start)
    }

    /// Decode an HPACK Huffman-encoded string (RFC 7541 Appendix B) using the
    /// hpack crate's canonical decoder. On decode error (corrupt/truncated data)
    /// we fall back to a lossy view of the raw bytes rather than a placeholder,
    /// so callers always get the most readable result available.
    fn huffman_decode(data: &[u8]) -> String {
        let mut decoder = hpack::huffman::HuffmanDecoder::new();
        match decoder.decode(data) {
            Ok(bytes) => String::from_utf8_lossy(&bytes).to_string(),
            Err(_) => String::from_utf8_lossy(data).to_string(),
        }
    }

    /// Get an entry from the HPACK static table (RFC 7541 Appendix A)
    fn get_static_table_entry(index: usize) -> Option<(&'static str, &'static str)> {
        match index {
            1 => Some((":authority", "")),
            2 => Some((":method", "GET")),
            3 => Some((":method", "POST")),
            4 => Some((":path", "/")),
            5 => Some((":path", "/index.html")),
            6 => Some((":scheme", "http")),
            7 => Some((":scheme", "https")),
            8 => Some((":status", "200")),
            9 => Some((":status", "204")),
            10 => Some((":status", "206")),
            11 => Some((":status", "304")),
            12 => Some((":status", "400")),
            13 => Some((":status", "404")),
            14 => Some((":status", "500")),
            15 => Some(("accept-charset", "")),
            16 => Some(("accept-encoding", "gzip, deflate")),
            17 => Some(("accept-language", "")),
            18 => Some(("accept-ranges", "")),
            19 => Some(("accept", "")),
            20 => Some(("access-control-allow-origin", "")),
            21 => Some(("age", "")),
            22 => Some(("allow", "")),
            23 => Some(("authorization", "")),
            24 => Some(("cache-control", "")),
            25 => Some(("content-disposition", "")),
            26 => Some(("content-encoding", "")),
            27 => Some(("content-language", "")),
            28 => Some(("content-length", "")),
            29 => Some(("content-location", "")),
            30 => Some(("content-range", "")),
            31 => Some(("content-type", "")),
            32 => Some(("date", "")),
            33 => Some(("etag", "")),
            34 => Some(("expect", "")),
            35 => Some(("expires", "")),
            36 => Some(("from", "")),
            37 => Some(("host", "")),
            38 => Some(("if-match", "")),
            39 => Some(("if-modified-since", "")),
            40 => Some(("if-none-match", "")),
            41 => Some(("if-range", "")),
            42 => Some(("if-unmodified-since", "")),
            43 => Some(("last-modified", "")),
            44 => Some(("link", "")),
            45 => Some(("location", "")),
            46 => Some(("max-forwards", "")),
            47 => Some(("proxy-authenticate", "")),
            48 => Some(("proxy-authorization", "")),
            49 => Some(("range", "")),
            50 => Some(("referer", "")),
            51 => Some(("refresh", "")),
            52 => Some(("retry-after", "")),
            53 => Some(("server", "")),
            54 => Some(("set-cookie", "")),
            55 => Some(("strict-transport-security", "")),
            56 => Some(("transfer-encoding", "")),
            57 => Some(("user-agent", "")),
            58 => Some(("vary", "")),
            59 => Some(("via", "")),
            60 => Some(("www-authenticate", "")),
            61 => Some(("x-forwarded-for", "")),
            _ => None,
        }
    }
}

impl TraceArgs for ParsedHttp2Frame {
    fn to_trace_args(&self) -> serde_json::Value {
        let mut args = serde_json::Map::new();
        args.insert("frame_type".to_string(), json!(self.type_name()));
        args.insert("flags".to_string(), json!(self.flags_description()));
        args.insert("stream_id".to_string(), json!(self.stream_id));
        args.insert("payload_len".to_string(), json!(self.payload_len));
        args.insert("pid".to_string(), json!(self.source_event.pid));
        args.insert("tid".to_string(), json!(self.source_event.tid));
        args.insert("comm".to_string(), json!(self.source_event.comm_str()));

        if self.is_data() && self.payload_len > 0 {
            if let Some(json_body) = self.json_body() {
                args.insert("body".to_string(), json_body);
            } else {
                let preview = self.body_str();
                if !preview.is_empty() {
                    let truncated = truncate_on_char_boundary(preview, 200);
                    args.insert("body_preview".to_string(), json!(truncated));
                }
            }
        }

        // Decode HPACK headers for HEADERS frames
        if self.is_headers() && self.payload_len > 0 {
            let headers = self.decode_headers_stateless();
            if !headers.is_empty() {
                let headers_json: serde_json::Map<String, serde_json::Value> = headers
                    .into_iter()
                    .map(|(name, value)| {
                        let v = value.unwrap_or_else(|| "<undecoded>".to_string());
                        (name, json!(v))
                    })
                    .collect();
                args.insert(
                    "headers".to_string(),
                    serde_json::Value::Object(headers_json),
                );
            }
        }

        serde_json::Value::Object(args)
    }
}

impl ToChromeTraceEvent for ParsedHttp2Frame {
    fn to_chrome_trace_events(&self) -> Vec<ChromeTraceEvent> {
        let ts_us = ns_to_us(self.source_event.timestamp_ns);
        const MIN_DUR_US: u64 = 1_000;

        let event = ChromeTraceEvent::complete(
            format!("H2 {} stream={}", self.type_name(), self.stream_id),
            "http2.frame",
            self.source_event.pid,
            self.source_event.tid as u64,
            ts_us,
            MIN_DUR_US,
        )
        .with_trace_args(self);

        vec![event]
    }
}

/// Shorten a body preview to at most `max_bytes`, cutting on a character
/// boundary so multi-byte text is never split mid-character.
fn truncate_on_char_boundary(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = max_bytes;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

impl fmt::Debug for ParsedHttp2Frame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut debug = f.debug_struct("ParsedHttp2Frame");
        debug
            .field("type", &self.type_name())
            .field("flags", &self.flags_description())
            .field("stream_id", &self.stream_id)
            .field("payload_len", &self.payload_len);

        if self.is_data() && self.payload_len > 0 {
            let body = self.payload();
            if let Ok(json) = serde_json::from_slice::<serde_json::Value>(body) {
                let formatted = serde_json::to_string_pretty(&json).unwrap_or_default();
                debug.field(
                    "body",
                    &format!("(json, {} bytes)\n{}", body.len(), formatted),
                );
            } else if let Ok(text) = std::str::from_utf8(body) {
                let text = text.trim();
                if text.len() > 200 {
                    let truncated = truncate_on_char_boundary(text, 200);
                    debug.field(
                        "body",
                        &format!("(text, {} bytes)\n{}...", body.len(), truncated),
                    );
                } else {
                    debug.field("body", &format!("(text, {} bytes)\n{}", body.len(), text));
                }
            } else {
                debug.field("body", &format!("(binary, {} bytes)", body.len()));
            }
        }

        // Decode HPACK headers for HEADERS frames
        if self.is_headers() && self.payload_len > 0 {
            let headers = self.decode_headers_stateless();
            if !headers.is_empty() {
                let header_strs: Vec<String> = headers
                    .into_iter()
                    .map(|(name, value)| match value {
                        Some(v) => format!("  {name}: {v}"),
                        None => format!("  {name}: <undecoded>"),
                    })
                    .collect();
                debug.field("headers", &format!("\n{}", header_strs.join("\n")));
            }
        }

        debug
            .field("pid", &self.source_event.pid)
            .field("tid", &self.source_event.tid)
            .field("timestamp_ns", &self.source_event.timestamp_ns);

        debug.finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_huffman_decode_rfc7541_vectors() {
        // RFC 7541 C.4.1: "www.example.com"
        let www = [
            0xf1, 0xe3, 0xc2, 0xe5, 0xf2, 0x3a, 0x6b, 0xa0, 0xab, 0x90, 0xf4, 0xff,
        ];
        assert_eq!(ParsedHttp2Frame::huffman_decode(&www), "www.example.com");

        // RFC 7541 C.4.2: "no-cache"
        let no_cache = [0xa8, 0xeb, 0x10, 0x64, 0x9c, 0xbf];
        assert_eq!(ParsedHttp2Frame::huffman_decode(&no_cache), "no-cache");

        // RFC 7541 C.6.1: ":status" value "302" -> Huffman 0x6402
        let s302 = [0x64, 0x02];
        assert_eq!(ParsedHttp2Frame::huffman_decode(&s302), "302");
    }

    #[test]
    fn test_huffman_decode_invalid_falls_back_to_lossy() {
        // Not a valid complete Huffman sequence: must not panic and must not
        // return the old "<huffman:N bytes>" placeholder.
        let out = ParsedHttp2Frame::huffman_decode(&[0x00]);
        assert!(!out.starts_with("<huffman:"));
    }

    fn data_frame(payload: Vec<u8>) -> ParsedHttp2Frame {
        let len = payload.len();
        ParsedHttp2Frame {
            frame_type: Http2FrameType::Data,
            flags: 0,
            stream_id: 1,
            payload_offset: 0,
            payload_len: len,
            source_event: Rc::new(SslEvent {
                source: 0,
                timestamp_ns: 0,
                delta_ns: 0,
                pid: 1,
                tid: 1,
                uid: 0,
                len: len as u32,
                rw: 0,
                comm: "curl".to_string(),
                buf: payload,
                is_handshake: false,
                ssl_ptr: 0,
            }),
        }
    }

    #[test]
    fn trace_args_cuts_multibyte_body_preview_on_char_boundary() {
        // A non-JSON body of dense CJK text over 200 bytes: byte 200 falls
        // inside a multi-byte character, so a raw &preview[..200] panicked
        // and took the eBPF event-handling thread down with it.
        let payload = "你".repeat(70); // 210 bytes of valid UTF-8, not JSON
        let frame = data_frame(payload.into_bytes());

        let args = frame.to_trace_args();
        let preview = args["body_preview"].as_str().unwrap();
        assert_eq!(preview, "你".repeat(66)); // 198 bytes = nearest boundary ≤ 200
    }

    #[test]
    fn debug_fmt_survives_multibyte_body() {
        let payload = "你".repeat(70);
        let frame = data_frame(payload.into_bytes());
        let rendered = format!("{frame:?}");
        assert!(rendered.contains("你"));
    }

    /// The extended length is a little-endian base-128 integer, so each
    /// continuation byte is shifted by seven more bits. Summing the groups
    /// decoded a 1337-byte header value as 94 bytes and left the rest of the
    /// block to be parsed from the middle of the value.
    #[test]
    fn test_decode_literal_string_extended_length_shifts_each_group() {
        // 1337 with a 7-bit prefix is 0x7F, 0xBA, 0x09 (RFC 7541 §5.1 example
        // shape): 127 + (58 << 0) + (9 << 7).
        let mut payload = vec![0x7f, 0xba, 0x09];
        payload.extend(std::iter::repeat_n(b'a', 1337));

        let (value, consumed) = ParsedHttp2Frame::decode_literal_string(&payload, 0);

        assert_eq!(value.len(), 1337, "the whole value must be decoded");
        assert_eq!(consumed, 3 + 1337);
    }

    /// A literal string whose extended length never terminates must not read
    /// past the payload. 0xFF = Huffman flag plus the 0x7F extended-length
    /// prefix; 0x80 is a continuation byte, so the loop keeps looking for the
    /// next byte after the payload ends. The helper's contract for
    /// corrupt/truncated input is a lossy result, not a panic — this input is
    /// reachable from captured bytes (HPACK header blocks are decoded from
    /// observed traffic).
    #[test]
    fn test_decode_literal_string_truncated_extended_length_does_not_panic() {
        let (value, consumed) = ParsedHttp2Frame::decode_literal_string(&[0xff, 0x80], 0);
        assert_eq!(value, "");
        assert_eq!(consumed, 2, "the whole payload is consumed");
    }
}
