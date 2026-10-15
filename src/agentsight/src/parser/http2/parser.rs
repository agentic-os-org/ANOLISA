#![allow(clippy::vec_init_then_push)]
//! HTTP/2 Frame Parser - binary frame parser with bounded split-frame reassembly
//!
//! Parses HTTP/2 binary frames from raw SSL event data, handles the connection
//! preface, and extracts individual frames. A frame split across two SSL reads
//! is completed from a bounded per-connection prefix: a TLS record caps at
//! 16 KiB while a maximum-size DATA frame is 16384+9 bytes, and the
//! continuation of a split frame starts inside a payload, so it can never be
//! recognized as a frame on its own.

use super::frame::{Http2FrameType, ParsedHttp2Frame};
use crate::config::DEFAULT_CONNECTION_CAPACITY;
use crate::probes::sslsniff::SslEvent;
use lru::LruCache;
use std::cell::RefCell;
use std::num::NonZeroUsize;
use std::rc::Rc;

/// HTTP/2 connection preface (RFC 7540 Section 3.5)
const HTTP2_PREFACE: &[u8] = b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n";

/// HTTP/2 frame header size
const FRAME_HEADER_SIZE: usize = 9;

/// Largest incomplete frame retained per connection and direction.
///
/// A frame may declare up to 16 MiB-1 (RFC 7540 §4.2), but each captured read
/// is at most 4 MiB and the HTTP/1 aggregator's per-connection body cap is
/// 8 MiB. Matching that cap keeps reassembly within the same worst-case
/// retention (capacity x cap) the rest of the aggregator is bounded by.
const MAX_PENDING_FRAME_BYTES: usize = 8 * 1024 * 1024;

/// Connection and direction identity for split-frame reassembly.
#[derive(Debug, Clone, Copy, Hash, Eq, PartialEq)]
struct ReassemblyKey {
    pid: u32,
    ssl_ptr: u64,
    rw: i32,
}

impl ReassemblyKey {
    fn from_ssl_event(event: &SslEvent) -> Self {
        Self {
            pid: event.pid,
            ssl_ptr: event.ssl_ptr,
            rw: event.rw,
        }
    }
}

/// HTTP/2 frame parser with bounded per-connection split-frame reassembly.
#[derive(Debug)]
pub struct Http2Parser {
    /// Incomplete frame tail per connection+direction. An entry with an empty
    /// value marks a connection already identified as HTTP/2 and sitting on a
    /// frame boundary; the entry itself routes the next read here even when it
    /// starts inside a frame payload.
    pending: RefCell<LruCache<ReassemblyKey, Vec<u8>>>,
}

impl Default for Http2Parser {
    fn default() -> Self {
        Self::new()
    }
}

impl Http2Parser {
    pub fn new() -> Self {
        // Clamp like the aggregator's caches: a zero capacity would otherwise
        // panic instead of degrading to maximum eviction.
        let capacity = NonZeroUsize::new(DEFAULT_CONNECTION_CAPACITY).unwrap_or(NonZeroUsize::MIN);
        Self {
            pending: RefCell::new(LruCache::new(capacity)),
        }
    }

    /// Whether `event` belongs to a connection+direction that is mid-frame or
    /// already identified as HTTP/2, so it must be routed here by state before
    /// any stateless heuristic.
    pub fn is_tracking(&self, event: &SslEvent) -> bool {
        self.pending
            .borrow()
            .contains(&ReassemblyKey::from_ssl_event(event))
    }

    /// Parse all HTTP/2 frames from an SslEvent buffer, completing a frame
    /// split across reads from the retained prefix.
    pub fn parse(&self, event: Rc<SslEvent>) -> Vec<ParsedHttp2Frame> {
        let key = ReassemblyKey::from_ssl_event(&event);
        let pending = self.pending.borrow_mut().pop(&key);

        // Merge the retained prefix with this read. The continuation alone
        // starts inside a payload and could never be recognized as a frame; a
        // merge past the cap drops the prefix (and the tracking entry) so a
        // broken stream cannot pin memory.
        let source = match pending {
            Some(mut buffered) if !buffered.is_empty() => {
                if buffered.len() + event.buf_size() as usize > MAX_PENDING_FRAME_BYTES {
                    log::warn!(
                        "HTTP/2 split frame exceeds {MAX_PENDING_FRAME_BYTES} bytes; dropping reassembly | pid={} ssl_ptr={:#x} rw={}",
                        key.pid,
                        key.ssl_ptr,
                        key.rw,
                    );
                    return Vec::new();
                }
                buffered.extend_from_slice(&event.buf[..event.buf_size() as usize]);
                let mut merged = (*event).clone();
                merged.buf = buffered;
                merged.len = merged.buf.len() as u32;
                Rc::new(merged)
            }
            _ => Rc::clone(&event),
        };

        let data = &source.buf[..source.buf_size() as usize];

        let mut pos = 0;
        let mut frames = Vec::new();
        let mut saw_frame = false;

        // Skip HTTP/2 connection preface if present
        if data.starts_with(HTTP2_PREFACE) {
            pos = HTTP2_PREFACE.len();
        }

        while pos + FRAME_HEADER_SIZE <= data.len() {
            // 3-byte big-endian length
            let length = ((data[pos] as usize) << 16)
                | ((data[pos + 1] as usize) << 8)
                | (data[pos + 2] as usize);

            let frame_type_byte = data[pos + 3];
            let flags = data[pos + 4];

            // 4-byte big-endian stream ID, mask reserved bit
            let stream_id = ((data[pos + 5] as u32 & 0x7F) << 24)
                | ((data[pos + 6] as u32) << 16)
                | ((data[pos + 7] as u32) << 8)
                | (data[pos + 8] as u32);

            let payload_offset = pos + FRAME_HEADER_SIZE;

            if payload_offset + length > data.len() {
                log::debug!(
                    "HTTP/2 frame @ {}: incomplete payload (need {} bytes, have {}); retaining prefix for the next read",
                    pos,
                    length,
                    data.len() - payload_offset
                );
                break;
            }

            saw_frame = true;
            let frame = ParsedHttp2Frame {
                frame_type: Http2FrameType::from_u8(frame_type_byte),
                flags,
                stream_id,
                payload_offset,
                payload_len: length,
                source_event: Rc::clone(&source),
            };

            if frame.is_data()
                || frame.is_headers()
                || frame.is_continuation()
                || frame.is_settings()
            {
                if frame.is_data() {
                    log::debug!(
                        "HTTP/2 DATA frame: stream={} flags={} len={}",
                        stream_id,
                        frame.flags_description(),
                        length,
                    );
                } else {
                    log::trace!(
                        "HTTP/2 {:?} frame: stream={} flags={} len={}",
                        frame.frame_type,
                        stream_id,
                        frame.flags_description(),
                        length,
                    );
                }
                frames.push(frame);
            }

            pos = payload_offset + length;
        }

        // Retain the tail for the next read. A tail that does not start with a
        // full frame header is only ever seen on a connection already being
        // reassembled, which is also the only case where those bytes can
        // belong to a frame whose prefix was captured earlier.
        let tail = &data[pos..];
        if saw_frame || data.starts_with(HTTP2_PREFACE) || !tail.is_empty() {
            if tail.len() <= MAX_PENDING_FRAME_BYTES {
                self.pending.borrow_mut().put(key, tail.to_vec());
            } else {
                log::warn!(
                    "HTTP/2 frame prefix exceeds {MAX_PENDING_FRAME_BYTES} bytes; dropping connection state | pid={} ssl_ptr={:#x} rw={}",
                    key.pid,
                    key.ssl_ptr,
                    key.rw,
                );
            }
        }

        frames
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_event(data: Vec<u8>) -> Rc<SslEvent> {
        Rc::new(SslEvent {
            source: 0,
            timestamp_ns: 1000,
            delta_ns: 0,
            pid: 1234,
            tid: 1,
            uid: 0,
            len: data.len() as u32,
            rw: 0,
            comm: "test".to_string(),
            buf: data,
            is_handshake: false,
            ssl_ptr: 0x1000,
        })
    }

    /// Build a raw HTTP/2 frame from parts
    fn build_frame(frame_type: u8, flags: u8, stream_id: u32, payload: &[u8]) -> Vec<u8> {
        let len = payload.len();
        let mut buf = Vec::with_capacity(9 + len);
        // 3-byte length
        buf.push(((len >> 16) & 0xFF) as u8);
        buf.push(((len >> 8) & 0xFF) as u8);
        buf.push((len & 0xFF) as u8);
        // type, flags
        buf.push(frame_type);
        buf.push(flags);
        // 4-byte stream ID
        buf.push(((stream_id >> 24) & 0x7F) as u8);
        buf.push(((stream_id >> 16) & 0xFF) as u8);
        buf.push(((stream_id >> 8) & 0xFF) as u8);
        buf.push((stream_id & 0xFF) as u8);
        // payload
        buf.extend_from_slice(payload);
        buf
    }

    #[test]
    fn test_parse_single_data_frame() {
        let payload = b"hello world";
        let raw = build_frame(0, 0x01, 1, payload); // DATA, END_STREAM, stream 1
        let event = create_test_event(raw);
        let parser = Http2Parser::new();
        let frames = parser.parse(event);

        assert_eq!(frames.len(), 1);
        let f = &frames[0];
        assert!(f.is_data());
        assert_eq!(f.stream_id, 1);
        assert!(f.has_end_stream());
        assert_eq!(f.payload(), b"hello world");
        assert_eq!(f.body_str(), "hello world");
    }

    #[test]
    fn test_parse_headers_and_settings_kept() {
        // SETTINGS frame is now kept (needed for HPACK table size updates)
        let raw = build_frame(4, 0x01, 0, &[]);
        let event = create_test_event(raw);
        let parser = Http2Parser::new();
        let frames = parser.parse(event);
        assert_eq!(frames.len(), 1);
        assert!(frames[0].is_settings());

        // HEADERS frame is now kept (needed for HPACK decode)
        let hpack_data = vec![0x82, 0x86, 0x84];
        let raw = build_frame(1, 0x05, 3, &hpack_data);
        let event = create_test_event(raw);
        let parser = Http2Parser::new();
        let frames = parser.parse(event);
        assert_eq!(frames.len(), 1);
        assert!(frames[0].is_headers());
    }

    #[test]
    fn test_parse_irrelevant_frames_filtered() {
        // WINDOW_UPDATE (type=8) should still be filtered out
        let raw = build_frame(8, 0x00, 1, &[0x00, 0x00, 0x00, 0x01]);
        let event = create_test_event(raw);
        let parser = Http2Parser::new();
        let frames = parser.parse(event);
        assert_eq!(frames.len(), 0);

        // PING (type=6) should still be filtered out
        let raw = build_frame(6, 0x00, 0, &[0; 8]);
        let event = create_test_event(raw);
        let parser = Http2Parser::new();
        let frames = parser.parse(event);
        assert_eq!(frames.len(), 0);
    }

    #[test]
    fn test_parse_multiple_frames_settings_and_data_kept() {
        let mut raw = build_frame(4, 0x00, 0, &[0x00, 0x03, 0x00, 0x00, 0x00, 0x64]); // SETTINGS
        raw.extend(build_frame(0, 0x01, 1, b"{\"ok\":true}")); // DATA END_STREAM
        let event = create_test_event(raw);
        let parser = Http2Parser::new();
        let frames = parser.parse(event);

        // Both SETTINGS and DATA frames are kept
        assert_eq!(frames.len(), 2);
        assert!(frames[0].is_settings());
        assert!(frames[1].is_data());
        assert_eq!(frames[1].body_str(), "{\"ok\":true}");
    }

    #[test]
    fn test_parse_with_connection_preface() {
        let mut raw = b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n".to_vec();
        raw.extend(build_frame(0, 0x01, 1, b"data")); // DATA
        let event = create_test_event(raw);
        let parser = Http2Parser::new();
        let frames = parser.parse(event);

        assert_eq!(frames.len(), 1);
        assert!(frames[0].is_data());
    }

    #[test]
    fn test_parse_incomplete_frame() {
        // Valid header but truncated payload
        let mut raw = Vec::new();
        raw.push(0x00);
        raw.push(0x00);
        raw.push(0x20); // length = 32
        raw.push(0x00); // DATA
        raw.push(0x00); // no flags
        raw.extend(&[0x00, 0x00, 0x00, 0x01]); // stream 1
        raw.extend(b"short"); // only 5 bytes, not 32
        let event = create_test_event(raw);
        let parser = Http2Parser::new();
        let frames = parser.parse(event);

        assert_eq!(frames.len(), 0);
    }

    #[test]
    fn test_parse_empty_buffer() {
        let event = create_test_event(vec![0x01, 0x02, 0x03]); // less than 9 bytes
        let parser = Http2Parser::new();
        let frames = parser.parse(event);
        assert!(frames.is_empty());
    }

    #[test]
    fn test_data_frame_json_body() {
        let json_payload =
            br#"{"model":"qwen3.5-plus","messages":[{"role":"user","content":"hello"}]}"#;
        let raw = build_frame(0, 0x01, 5, json_payload);
        let event = create_test_event(raw);
        let parser = Http2Parser::new();
        let frames = parser.parse(event);

        assert_eq!(frames.len(), 1);
        let body = frames[0].json_body().unwrap();
        assert_eq!(body["model"], "qwen3.5-plus");
        assert_eq!(body["messages"][0]["role"], "user");
    }

    #[test]
    fn test_frame_flags() {
        // DATA without END_STREAM
        let raw = build_frame(0, 0x00, 1, b"data");
        let event = create_test_event(raw);
        let parser = Http2Parser::new();
        let frames = parser.parse(event);
        assert_eq!(frames.len(), 1);
        assert!(!frames[0].has_end_stream());
        assert!(!frames[0].has_end_headers());

        // DATA with END_STREAM
        let raw = build_frame(0, 0x01, 1, b"data");
        let event = create_test_event(raw);
        let frames = parser.parse(event);
        assert_eq!(frames.len(), 1);
        assert!(frames[0].has_end_stream());
    }

    #[test]
    fn test_sample_data_from_python_script() {
        // Test vector from scripts/http2_parser.py
        // Contains a HEADERS frame (len=83, stream=171) and an incomplete DATA frame (len=16384, truncated)
        let sample_data: Vec<u8> = vec![
            0, 0, 83, 1, 4, 0, 0, 0, 171, 203, 131, 4, 153, 96, 135, 166, 177, 164, 209, 208, 85,
            169, 60, 133, 99, 184, 88, 36, 227, 75, 4, 61, 53, 208, 84, 152, 245, 35, 135, 202,
            201, 200, 199, 198, 197, 196, 195, 194, 31, 8, 158, 186, 81, 216, 91, 20, 71, 85, 156,
            11, 196, 1, 28, 117, 240, 180, 86, 138, 208, 227, 145, 151, 218, 142, 87, 136, 65, 133,
            185, 25, 143, 193, 192, 191, 190, 15, 13, 132, 117, 166, 94, 111, 0, 64, 0, 0, 0, 0, 0,
            0, 171, 123, 34, 109, 111, 100, 101, 108, 34, 58, 34, 113, 119, 101, 110, 51, 46, 53,
            45, 112, 108, 117, 115, 34, 44, 34, 109, 101, 115, 115, 97, 103, 101, 115, 34, 58, 91,
            123, 34, 114, 111, 108, 101, 34, 58, 34, 115, 121, 115, 116, 101, 109, 34, 44, 34, 99,
            111, 110, 116, 101, 110, 116, 34, 58, 34, 89, 111, 117,
        ];
        let event = create_test_event(sample_data);
        let parser = Http2Parser::new();
        let frames = parser.parse(event);

        // HEADERS frame is now kept, DATA frame is truncated -> 1 HEADERS frame
        assert_eq!(frames.len(), 1);
        assert!(frames[0].is_headers());
        assert_eq!(frames[0].stream_id, 171);
    }

    #[test]
    fn test_split_frame_reassembled_across_reads() {
        // A TLS record caps at 16 KiB while a maximum-size DATA frame is
        // 16384+9 bytes, so a frame split across two SSL reads is ordinary.
        // The continuation starts inside the payload, so it must be joined
        // with the prefix retained from the first read.
        let payload = br#"{"model":"gpt-4","messages":[{"role":"user"}]}"#;
        let raw = build_frame(0, 0x01, 7, payload);
        let cut = 9 + payload.len() / 2;
        let parser = Http2Parser::new();

        let mut frames = parser.parse(create_test_event(raw[..cut].to_vec()));
        assert!(
            frames.is_empty(),
            "a truncated frame must not be emitted before its continuation"
        );
        frames.extend(parser.parse(create_test_event(raw[cut..].to_vec())));

        assert_eq!(
            frames.len(),
            1,
            "the split frame must complete exactly once"
        );
        assert!(frames[0].is_data());
        assert_eq!(frames[0].stream_id, 7);
        assert_eq!(frames[0].payload(), payload);
    }
}
