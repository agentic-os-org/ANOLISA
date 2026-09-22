//! Unified Parser - high-level entry point for protocol parsing
//
//! This module provides a unified interface for parsing SSL events and process events.
//! It combines HTTP Parser, SSE Parser, and ProcTrace Parser, but does NOT include aggregation logic.
//
//! For aggregation, use:
//! - `HttpConnectionAggregator` for HTTP/SSE events
//! - `ProcessEventAggregator` for process events

use super::{ParseResult, ParsedMessage};
use crate::event::Event;
use crate::parser::http::{HttpParser, ParsedHttpMessage};
use crate::parser::http2::Http2Parser;
use crate::parser::proctrace::ProcTraceParser;
use crate::parser::sse::{ParsedSseEvent, SseParser};
use crate::probes::proctrace::VariableEvent;
use crate::probes::sslsniff::SslEvent;
use crate::runtime_metrics::StageTimer;
use std::rc::Rc;

/// Unified parser for SSL and process events
///
/// This parser provides a unified entry point for parsing but does NOT
/// aggregate or correlate messages. Use aggregators for that.
pub struct Parser {
    http_parser: HttpParser,
    http2_parser: Http2Parser,
    sse_parser: SseParser,
}

impl Default for Parser {
    fn default() -> Self {
        Self::new()
    }
}

impl Parser {
    /// Create new parser
    pub fn new() -> Self {
        Parser {
            http_parser: HttpParser::new(),
            http2_parser: Http2Parser::new(),
            sse_parser: SseParser::new(),
        }
    }

    /// Parse SSL event into messages
    //
    /// Returns parsed HTTP Request/Response or SSE Events.
    /// Does NOT aggregate or correlate - use `HttpConnectionAggregator` for that.
    pub fn parse_ssl_event(&self, ssl_event: Rc<SslEvent>) -> ParseResult {
        log::debug!("parse_ssl_event: length={}", ssl_event.buf_size());

        let _comm = ssl_event.comm.trim_end_matches('\0');

        // 1. HTTP/1.x detection (text-based protocols)
        if ssl_event.is_http() {
            match self.http_parser.parse(ssl_event.clone()) {
                Ok(msg) => {
                    let message = match msg {
                        ParsedHttpMessage::Request(req) => ParsedMessage::Request(req),
                        ParsedHttpMessage::Response(resp) => ParsedMessage::Response(resp),
                    };
                    return ParseResult {
                        messages: vec![message],
                    };
                }
                Err(e) => {
                    log::debug!("Failed to parse HTTP/1.x event: {e}");
                }
            }
        }

        // 2. HTTP/2 detection (binary frame protocol)
        if ssl_event.is_http2() {
            let frames = self.http2_parser.parse(ssl_event.clone());
            if !frames.is_empty() {
                return ParseResult {
                    messages: vec![ParsedMessage::Http2Frames(frames)],
                };
            }
        }

        // 2.5. Detect HTTP chunked transfer encoding end marker "0\r\n\r\n".
        // This signals end of a chunked SSE stream (e.g., OpenAI Responses API).
        // The terminator is often appended to the end of the last SSE data
        // chunk rather than arriving as a standalone 5-byte read, so we check
        // whether the buffer ends with (or equals) `0\r\n\r\n`.
        //
        // When found, strip the terminator bytes, parse any remaining SSE data
        // from the prefix, and synthesize a [DONE] event so the aggregator can
        // complete the stream.
        {
            let buf_size = ssl_event.buf_size() as usize;
            let buf = &ssl_event.buf[..buf_size];
            const TERMINATOR: &[u8] = b"0\r\n\r\n";

            let terminator_pos = if buf.len() >= TERMINATOR.len() && buf.ends_with(TERMINATOR) {
                Some(buf.len() - TERMINATOR.len())
            } else {
                None
            };

            if let Some(prefix_len) = terminator_pos {
                let prefix = &buf[..prefix_len];
                let mut messages = Vec::new();

                // Parse SSE events from the data before the terminator
                if !prefix.is_empty() {
                    let trimmed = SslEvent {
                        source: ssl_event.source,
                        timestamp_ns: ssl_event.timestamp_ns,
                        delta_ns: ssl_event.delta_ns,
                        pid: ssl_event.pid,
                        tid: ssl_event.tid,
                        uid: ssl_event.uid,
                        len: prefix_len as u32,
                        rw: ssl_event.rw,
                        comm: ssl_event.comm.clone(),
                        buf: ssl_event.buf[..prefix_len].to_vec(),
                        is_handshake: ssl_event.is_handshake,
                        ssl_ptr: ssl_event.ssl_ptr,
                    };
                    let prefix_events = self.sse_parser.parse(Rc::new(trimmed));
                    for ev in prefix_events {
                        messages.push(ParsedMessage::SseEvent(ev));
                    }
                }

                // Always append a synthetic done marker
                messages.push(ParsedMessage::SseEvent(ParsedSseEvent::new_done_marker(
                    Rc::clone(&ssl_event),
                )));

                return ParseResult { messages };
            }
        }

        // 3. Write-direction data that failed all protocol detection → RawData
        // This is likely a body continuation for an in-progress HTTP/1.1 request
        if ssl_event.rw == 1 {
            log::debug!(
                "parse_ssl_event: unrecognized write-direction data (len={}), emitting RawData",
                ssl_event.buf_size()
            );
            return ParseResult {
                messages: vec![ParsedMessage::RawData(ssl_event)],
            };
        }

        // 4. Fallback: SSE data (read-direction only)
        let sse_events = self.sse_parser.parse(ssl_event.clone());
        if sse_events.is_empty() {
            // No SSE events could be parsed from this read-direction chunk.
            // This happens when the SSE stream is compressed (gzip/zstd/br):
            // the bytes are not text and yield no `data:`/`event:` lines.
            // Forward the raw event so the aggregator — which knows the
            // connection's Content-Encoding — can buffer and later decompress
            // it. For non-SSE connections the aggregator simply ignores it.
            return ParseResult {
                messages: vec![ParsedMessage::RawData(ssl_event)],
            };
        }
        let messages = sse_events
            .into_iter()
            .map(ParsedMessage::SseEvent)
            .collect();
        ParseResult { messages }
    }

    /// Parse process event into messages
    ///
    /// Returns parsed process event (Exec/Stdout/Exit).
    /// Does NOT aggregate - use `ProcessEventAggregator` for that.
    pub fn parse_proc_event(&self, event: &VariableEvent) -> ParseResult {
        match ProcTraceParser::parse_variable(event) {
            Some(parsed) => ParseResult {
                messages: vec![ParsedMessage::ProcEvent(parsed)],
            },
            None => ParseResult {
                messages: Vec::new(),
            },
        }
    }

    /// Parse unified Event
    pub fn parse_event(&self, event: Event) -> ParseResult {
        let timer = StageTimer::start("parser");
        log::trace!("Parsing event({:?})", event.event_type());
        let result = match event {
            Event::Ssl(ssl_event) => self.parse_ssl_event(Rc::new(ssl_event)),
            Event::Proc(proc_event) => self.parse_proc_event(&proc_event),
            Event::ProcMon(_) => ParseResult {
                messages: Vec::new(),
            },
            Event::FileWatch(_) => ParseResult {
                messages: Vec::new(),
            },
            Event::FileWrite(_) => ParseResult {
                messages: Vec::new(),
            },
            Event::UdpDns(_) => ParseResult {
                messages: Vec::new(),
            },
        };
        timer.record_outputs(result.messages.len());
        result
    }

    /// Get reference to HTTP parser
    pub fn http_parser(&self) -> &HttpParser {
        &self.http_parser
    }

    /// Get reference to SSE parser
    pub fn sse_parser(&self) -> &SseParser {
        &self.sse_parser
    }

    /// Get reference to HTTP/2 parser
    pub fn http2_parser(&self) -> &Http2Parser {
        &self.http2_parser
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_ssl_event(data: Vec<u8>) -> Rc<SslEvent> {
        let len = data.len();
        Rc::new(SslEvent {
            source: 0,
            timestamp_ns: 0,
            delta_ns: 0,
            pid: 1,
            tid: 1,
            uid: 0,
            len: len as u32,
            rw: 0,
            comm: String::new(),
            buf: data,
            is_handshake: false,
            ssl_ptr: 0x1000,
        })
    }

    #[test]
    fn test_chunked_terminator_standalone() {
        let parser = Parser::new();
        let event = make_ssl_event(b"0\r\n\r\n".to_vec());
        let result = parser.parse_ssl_event(event);
        assert_eq!(result.messages.len(), 1);
        assert!(matches!(
            result.messages[0],
            ParsedMessage::SseEvent(ref e) if e.is_done()
        ));
    }

    #[test]
    fn test_chunked_terminator_appended_to_sse_data() {
        let parser = Parser::new();
        let data = b"data: {\"type\":\"response.output_text.delta\"}\n\n0\r\n\r\n".to_vec();
        let event = make_ssl_event(data);
        let result = parser.parse_ssl_event(event);
        // Should produce: 1 SSE event from the data + 1 done marker
        assert_eq!(result.messages.len(), 2);
        assert!(matches!(
            result.messages[0],
            ParsedMessage::SseEvent(ref e) if !e.is_done()
        ));
        assert!(matches!(
            result.messages[1],
            ParsedMessage::SseEvent(ref e) if e.is_done()
        ));
    }

    #[test]
    fn test_no_chunked_terminator_normal_sse() {
        let parser = Parser::new();
        let data = b"data: hello\n\n".to_vec();
        let event = make_ssl_event(data);
        let result = parser.parse_ssl_event(event);
        assert_eq!(result.messages.len(), 1);
        assert!(matches!(
            result.messages[0],
            ParsedMessage::SseEvent(ref e) if !e.is_done()
        ));
    }
}
