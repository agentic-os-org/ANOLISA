use super::event::{ParsedSseEvent, SSEEvent, SSEEvents};
use crate::probes::sslsniff::SslEvent;
use std::rc::Rc;

/// SSE Parser - parses SSE stream data into events (legacy version)
pub struct SSEParser;

impl SSEParser {
    /// Parse SSE stream buffer and extract complete events
    /// Returns SSEEvents container with parsed events and unconsumed data
    pub fn parse_stream(buffer: &str) -> SSEEvents {
        let mut result = SSEEvents::new();
        let mut current_event = SSEEvent::new("");
        let mut data_lines: Vec<String> = Vec::new();
        // Only a complete stream prefix has this guarantee. Include the
        // ignored mark in the original buffer's consumed-byte coordinates.
        let mut consumed_len = usize::from(buffer.starts_with('\u{feff}')) * 3;

        // Split inclusive so CRLF lines are measured by their real byte
        // length: `str::lines()` strips the `\r`, and adding only 1 for it
        // undercounts every CRLF line, which pushed the remaining-slice start
        // into the middle of a multi-byte character (panicking) and dropped
        // the wrong number of unconsumed bytes. A trailing line without a
        // newline is never consumed, so it stays in `remaining` intact.
        for raw in buffer[consumed_len..].split_inclusive('\n') {
            let Some(raw_line) = raw.strip_suffix('\n') else {
                break;
            };
            let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
            consumed_len += raw.len();

            if line.is_empty() {
                // Empty line terminates the event
                if current_event.id.is_some()
                    || current_event.event.is_some()
                    || current_event.retry.is_some()
                    || !data_lines.is_empty()
                {
                    current_event.data = data_lines.join("\n");
                    result.events.push(current_event);
                    current_event = SSEEvent::new("");
                    data_lines.clear();
                }
            } else if line.starts_with(':') {
                // Comment line - ignore per spec
                continue;
            } else if let Some((field, value)) = line.split_once(':') {
                // Field with optional value (strip leading space if present)
                let value = value.strip_prefix(' ').unwrap_or(value);
                match field {
                    "id" => current_event.id = Some(value.to_string()),
                    "event" => current_event.event = Some(value.to_string()),
                    "data" => data_lines.push(value.to_string()),
                    "retry" => current_event.retry = value.parse().ok(),
                    _ => {} // Unknown field, ignore per spec
                }
            } else {
                // Field without colon, entire line is field name with empty value
                match line {
                    "id" => current_event.id = Some(String::new()),
                    "event" => current_event.event = Some(String::new()),
                    "data" => data_lines.push(String::new()),
                    "retry" => current_event.retry = Some(0),
                    _ => {} // Unknown field, ignore per spec
                }
            }
        }

        // `consumed_len` only advances past complete (newline-terminated)
        // lines, so the split is always on a char boundary and any trailing
        // partial line is returned verbatim.
        result.remaining = buffer[consumed_len..].to_string();
        result.consumed_bytes = consumed_len;

        result
    }
}

/// SseParser - new version with zero-copy ParsedSseEvent
#[derive(Debug, Default)]
pub struct SseParser;

impl SseParser {
    /// Create a new SseParser
    pub fn new() -> Self {
        Self
    }

    /// Parse SslEvent and extract SSE events
    /// Returns Vec of ParsedSseEvent
    ///
    /// Only newline-terminated lines are parsed. A trailing line without a
    /// newline is a torn line from an SSL_read split and produces no event
    /// (the legacy parser returns the same tail in `remaining`); bytes after
    /// the last complete line are not returned, so a caller that needs them
    /// has to retain the raw buffer. An event whose fields are complete but
    /// lacks the terminating blank line is still emitted, matching the
    /// legacy parser's end-of-buffer handling.
    ///
    /// Note: For multi-line data fields, data is concatenated with '\n' separators.
    /// The data_offset points to the first data line, data_len covers all data content
    /// including internal newlines.
    pub fn parse(&self, event: Rc<SslEvent>) -> Vec<ParsedSseEvent> {
        self.parse_from_offset(event, 0)
    }

    /// Ignore one UTF-8 BOM at a known stream start; arbitrary reads use `parse`.
    pub(crate) fn parse_at_stream_start(&self, event: Rc<SslEvent>) -> Vec<ParsedSseEvent> {
        let offset =
            usize::from(event.buf[..event.buf_size() as usize].starts_with(b"\xef\xbb\xbf")) * 3;
        self.parse_from_offset(event, offset)
    }

    fn parse_from_offset(&self, event: Rc<SslEvent>, offset: usize) -> Vec<ParsedSseEvent> {
        let buf_len = event.buf_size() as usize;
        let buf = &event.buf[offset..buf_len];

        let mut events = Vec::new();
        let mut current_id: Option<String> = None;
        let mut current_event: Option<String> = None;
        let mut current_retry: Option<u64> = None;
        let mut data_parts: Vec<&[u8]> = Vec::new();
        let mut data_start: Option<usize> = None;

        let mut byte_offset = offset;
        // Set when the buffer's last segment has no terminating newline: the
        // event is still in flight and must not be flushed at end-of-buffer.
        let mut trailing_line_torn = false;

        // Iterate the ORIGINAL bytes, not a lossy UTF-8 conversion: the
        // zero-copy offsets below index into event.buf, so they must stay
        // in raw-buffer coordinates. from_utf8_lossy expands every invalid
        // byte (e.g. a multi-byte character split across TLS record
        // boundaries) into a 3-byte U+FFFD, which would shift every offset
        // after it and make data() slice the wrong bytes.
        let lines_iter = buf.split_inclusive(|b| *b == b'\n');

        for line_with_end in lines_iter {
            let line_with_end_len = line_with_end.len();
            let line_start = byte_offset;

            // A final segment without '\n' is not a complete line: it is the
            // torn prefix of a line split across SSL_read buffers (possibly
            // partial JSON carrying a delta or usage). Emitting it would
            // dispatch a bogus event whose remainder cannot be recognized in
            // the next buffer; the legacy parser returns such a tail in
            // `remaining` instead.
            let Some(line_with_cr) = line_with_end.strip_suffix(b"\n") else {
                trailing_line_torn = true;
                break;
            };

            // Strip a trailing \r for parsing, but keep the original length
            // for the raw-buffer offsets computed from line_start.
            let mut end = line_with_cr.len();
            while end > 0 && line_with_cr[end - 1] == b'\r' {
                end -= 1;
            }
            let line_bytes = &line_with_cr[..end];
            let line = String::from_utf8_lossy(line_bytes);

            if line.is_empty() {
                // Empty line terminates the event
                if current_id.is_some()
                    || current_event.is_some()
                    || current_retry.is_some()
                    || !data_parts.is_empty()
                {
                    // For zero-copy design, we only record the first data line
                    // Multi-line data concatenation is not supported in zero-copy mode
                    // Users needing full multi-line data should use the legacy SSEParser
                    let (data_offset, data_len) = if !data_parts.is_empty() {
                        // Only use first data line for zero-copy access;
                        // its raw byte length is exact because trailing \r
                        // was already trimmed above.
                        (data_start.unwrap_or(0), data_parts[0].len())
                    } else {
                        (0, 0)
                    };

                    events.push(ParsedSseEvent::new(
                        current_id.clone(),
                        current_event.clone(),
                        current_retry,
                        data_offset,
                        data_len,
                        Rc::clone(&event),
                    ));

                    // Reset for next event
                    current_id = None;
                    current_event = None;
                    current_retry = None;
                    data_parts.clear();
                    data_start = None;
                }
            } else if line.starts_with(':') {
                // Comment line - ignore per spec
            } else if let Some((field, value)) = line.split_once(':') {
                // Field with optional value (strip leading space if present per SSE spec)
                // SSE spec: "If line starts with a U+003A COLON character (':'), ignore the line."
                // "If the line contains a U+003A COLON character (':'), collect the characters
                // on the line before the first U+003A COLON character (':'), and let field be that string.
                // Collect the characters on the line after the first U+003A COLON character (':'),
                // and let value be that string. If value starts with a U+0020 SPACE character,
                // remove it from value."
                let has_space_after_colon = value.starts_with(' ');
                let value_stripped = value.strip_prefix(' ').unwrap_or(value);

                // Calculate value_start in the original buffer
                // line_start: start of line in buffer
                // field.len() + 1: skip field and colon
                // +1 if there was a space after colon
                // A field only matches when it is pure ASCII, so the lossy
                // prefix has the same length as the raw prefix.
                let value_start =
                    line_start + field.len() + 1 + if has_space_after_colon { 1 } else { 0 };

                match field {
                    "id" => current_id = Some(value_stripped.to_string()),
                    "event" => current_event = Some(value_stripped.to_string()),
                    "data" => {
                        if data_start.is_none() {
                            data_start = Some(value_start);
                        }
                        // Keep the RAW value bytes: the recorded length must
                        // count original buffer bytes, not the lossy
                        // expansion of any invalid UTF-8 inside the value.
                        data_parts.push(&line_bytes[value_start - line_start..]);
                    }
                    "retry" => current_retry = value_stripped.parse().ok(),
                    _ => {} // Unknown field, ignore per spec
                }
            } else {
                // Field without colon, entire line is field name with empty value
                match line.as_ref() {
                    "id" => current_id = Some(String::new()),
                    "event" => current_event = Some(String::new()),
                    "data" => {
                        if data_start.is_none() {
                            data_start = Some(line_start + 5); // "data" + 0 chars
                        }
                        data_parts.push(&[]);
                    }
                    "retry" => current_retry = Some(0),
                    _ => {} // Unknown field, ignore per spec
                }
            }

            byte_offset += line_with_end_len;
        }

        // Handle event at end without double newline. Suppressed when the
        // buffer ended on a torn line: the event is incomplete, so flushing
        // it here would publish a partial event and drop its remainder.
        if !trailing_line_torn
            && (current_id.is_some()
                || current_event.is_some()
                || current_retry.is_some()
                || !data_parts.is_empty())
        {
            let (data_offset, data_len) = if !data_parts.is_empty() {
                // Only use first data line for zero-copy access
                let first_line_len = data_parts[0].len();
                (data_start.unwrap_or(0), first_line_len)
            } else {
                (0, 0)
            };

            events.push(ParsedSseEvent::new(
                current_id,
                current_event,
                current_retry,
                data_offset,
                data_len,
                Rc::clone(&event),
            ));
        }

        events
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_event(data: Vec<u8>) -> Rc<SslEvent> {
        let len = data.len();

        Rc::new(SslEvent {
            source: 1,
            timestamp_ns: 0,
            delta_ns: 0,
            pid: 1234,
            tid: 1234,
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
    fn test_known_stream_start_bom_preserves_raw_offsets_and_payload() {
        let parser = SseParser::new();
        let marked = create_test_event("\u{feff}data: 中\u{feff}文\n\n".as_bytes().to_vec());
        assert!(parser.parse(marked.clone()).is_empty());
        let events = parser.parse_at_stream_start(marked.clone());
        assert_eq!(events[0].data(), "中\u{feff}文".as_bytes());
        assert_eq!(events[0].data_offset(), 9);
        assert!(std::ptr::eq(events[0].source_event(), marked.as_ref()));
        let double = create_test_event("\u{feff}\u{feff}data: ignored\n\n".as_bytes().to_vec());
        assert!(parser.parse_at_stream_start(double).is_empty());
        let late = create_test_event(
            "data: first\n\n\u{feff}data: ignored\n\n"
                .as_bytes()
                .to_vec(),
        );
        assert_eq!(parser.parse_at_stream_start(late).len(), 1);
    }

    #[test]
    fn test_parse_simple_sse_event() {
        let parser = SseParser::new();
        let data = b"data: hello world\n\n".to_vec();
        let event = create_test_event(data);

        let events = parser.parse(event);
        assert_eq!(events.len(), 1);

        let evt = &events[0];
        assert_eq!(evt.id, None);
        assert_eq!(evt.event, None);
        assert_eq!(evt.data(), b"hello world");
        assert!(!evt.is_done());
    }

    #[test]
    fn test_parse_sse_with_id_and_event() {
        let parser = SseParser::new();
        let data = b"id: 123\nevent: message\ndata: hello\n\n".to_vec();
        let event = create_test_event(data);

        let events = parser.parse(event);
        assert_eq!(events.len(), 1);

        let evt = &events[0];
        assert_eq!(evt.id, Some("123".to_string()));
        assert_eq!(evt.event, Some("message".to_string()));
        assert_eq!(evt.data(), b"hello");
    }

    #[test]
    fn test_parse_multiple_events() {
        let parser = SseParser::new();
        let data = b"data: first\n\ndata: second\n\n".to_vec();
        let event = create_test_event(data);

        let events = parser.parse(event);
        assert_eq!(events.len(), 2);

        assert_eq!(events[0].data(), b"first");
        assert_eq!(events[1].data(), b"second");
    }

    #[test]
    fn test_parse_offsets_survive_invalid_utf8_bytes() {
        // A TLS record boundary can split a multi-byte character, so a
        // captured buffer may contain invalid UTF-8. Offsets must stay in
        // ORIGINAL buffer coordinates: with the lossy whole-buffer string,
        // each invalid byte expanded into a 3-byte U+FFFD and every later
        // event's data() returned the wrong bytes.
        let parser = SseParser::new();
        let data = b"data: \xff\n\ndata: hi\n\n".to_vec();
        let event = create_test_event(data);

        let events = parser.parse(event);
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].data(), b"\xff");
        assert_eq!(events[1].data(), b"hi");
    }

    #[test]
    fn test_parse_offsets_survive_invalid_utf8_inside_data() {
        // Invalid bytes inside the first data line must not inflate the
        // recorded data_len either.
        let parser = SseParser::new();
        let data = b"event: usage\ndata: {\"a\":1}\xff\ndata: second\n\n".to_vec();
        let event = create_test_event(data);

        let events = parser.parse(event);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event.as_deref(), Some("usage"));
        assert_eq!(events[0].data(), b"{\"a\":1}\xff");
    }

    #[test]
    fn test_parse_done_marker() {
        let parser = SseParser::new();
        let data = b"event: done\ndata: [DONE]\n\n".to_vec();
        let event = create_test_event(data);

        let events = parser.parse(event);
        assert_eq!(events.len(), 1);

        let evt = &events[0];
        assert!(evt.is_done());
    }

    #[test]
    fn test_parse_multiline_data() {
        let parser = SseParser::new();
        let data = b"data: line1\ndata: line2\n\n".to_vec();
        let event = create_test_event(data);

        let events = parser.parse(event);
        assert_eq!(events.len(), 1);

        let evt = &events[0];
        // Multi-line data: offset points to first line "line1"
        // data_len is the length of first line only (5)
        // Full multi-line concatenation is not implemented in zero-copy mode
        assert_eq!(evt.data(), b"line1");
        assert_eq!(evt.data_len(), 5);
    }

    #[test]
    fn test_parse_comment_ignored() {
        let parser = SseParser::new();
        let data = b": this is a comment\ndata: hello\n\n".to_vec();
        let event = create_test_event(data);

        let events = parser.parse(event);
        assert_eq!(events.len(), 1);

        let evt = &events[0];
        assert_eq!(evt.data(), b"hello");
    }

    #[test]
    fn test_parse_retry_field() {
        let parser = SseParser::new();
        let data = b"retry: 5000\ndata: reconnect\n\n".to_vec();
        let event = create_test_event(data);

        let events = parser.parse(event);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].retry, Some(5000));
    }

    #[test]
    fn test_parse_empty_data() {
        let parser = SseParser::new();
        let data = b"data:\n\n".to_vec();
        let event = create_test_event(data);

        let events = parser.parse(event);
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn test_parse_end_marker() {
        let parser = SseParser::new();
        let data = b"data: [END]\n\n".to_vec();
        let event = create_test_event(data);

        let events = parser.parse(event);
        assert_eq!(events.len(), 1);
        assert!(events[0].is_done());
    }

    #[test]
    fn test_parse_json_body() {
        let parser = SseParser::new();
        let data = b"data: {\"key\":\"value\"}\n\n".to_vec();
        let event = create_test_event(data);

        let events = parser.parse(event);
        assert_eq!(events.len(), 1);
        let json = events[0].json_body().unwrap();
        assert_eq!(json["key"], "value");
    }

    #[test]
    fn test_parse_no_terminator() {
        let parser = SseParser::new();
        // Event without double newline at end
        let data = b"data: incomplete\n".to_vec();
        let event = create_test_event(data);

        let events = parser.parse(event);
        // Should still emit the event at end
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data(), b"incomplete");
    }

    #[test]
    fn test_parse_torn_trailing_line_no_event() {
        // One SSL_read can split the stream mid-line. A final segment without
        // a terminating newline is a torn prefix of a line (e.g. partial
        // JSON), not a complete event: dispatching it loses the remainder,
        // which arrives in the next buffer as an unrecognizable fragment.
        // The legacy SSEParser returns such a tail in `remaining` instead.
        let parser = SseParser::new();
        let data = b"data: {\"a\":1".to_vec();
        let event = create_test_event(data);

        let events = parser.parse(event);
        assert!(
            events.is_empty(),
            "torn trailing line must not be emitted as an event, got {} event(s)",
            events.len()
        );
    }

    #[test]
    fn test_parse_complete_event_then_torn_tail() {
        // Only the blank-line-terminated event is complete; the torn tail
        // must not become a second partial event.
        let parser = SseParser::new();
        let data = b"data: complete\n\ndata: {\"a\":1".to_vec();
        let event = create_test_event(data);

        let events = parser.parse(event);
        assert_eq!(events.len(), 1, "only the complete event may be emitted");
        assert_eq!(events[0].data(), b"complete");
    }

    #[test]
    fn test_parse_synthetic_done_marker() {
        let event = create_test_event(b"dummy".to_vec());
        let done = ParsedSseEvent::new_done_marker(event);
        assert!(done.is_done());
        assert_eq!(done.data_len(), 0);
    }

    #[test]
    fn test_body_str() {
        let parser = SseParser::new();
        let data = b"data: text content\n\n".to_vec();
        let event = create_test_event(data);
        let events = parser.parse(event);
        assert_eq!(events[0].body_str(), "text content");
    }

    // Legacy SSEParser tests
    #[test]
    fn test_legacy_initial_bom_only_once_with_raw_offsets() {
        let buffer = "\u{feff}data: 中\n\ndata: 尾";
        let result = SSEParser::parse_stream(buffer);
        assert_eq!(result.events.len(), 1);
        assert_eq!(result.events[0].data, "中");
        assert_eq!(result.consumed_bytes, "\u{feff}data: 中\n\n".len());
        assert_eq!(result.remaining, "data: 尾");
        assert!(
            SSEParser::parse_stream("\u{feff}\u{feff}data: ignored\n\n")
                .events
                .is_empty()
        );
        let result = SSEParser::parse_stream(
            "data: first\n\n\u{feff}data: ignored\n\ndata: \u{feff}payload\n\n",
        );
        assert_eq!(result.events.len(), 2);
        assert_eq!(result.events[1].data, "\u{feff}payload");
    }

    #[test]
    fn test_legacy_parse_stream_single_event() {
        let result = SSEParser::parse_stream("data: hello\n\n");
        assert_eq!(result.len(), 1);
        assert_eq!(result.events[0].data, "hello");
        assert!(result.remaining.is_empty());
    }

    #[test]
    fn test_legacy_parse_stream_multiple_events() {
        let result = SSEParser::parse_stream("data: first\n\ndata: second\n\n");
        assert_eq!(result.len(), 2);
        assert_eq!(result.events[0].data, "first");
        assert_eq!(result.events[1].data, "second");
    }

    #[test]
    fn test_legacy_parse_stream_with_id_event_retry() {
        let result =
            SSEParser::parse_stream("id: 42\nevent: update\nretry: 1000\ndata: payload\n\n");
        assert_eq!(result.len(), 1);
        let evt = &result.events[0];
        assert_eq!(evt.id, Some("42".to_string()));
        assert_eq!(evt.event, Some("update".to_string()));
        assert_eq!(evt.retry, Some(1000));
        assert_eq!(evt.data, "payload");
    }

    #[test]
    fn test_legacy_parse_stream_multiline_data() {
        let result = SSEParser::parse_stream("data: line1\ndata: line2\ndata: line3\n\n");
        assert_eq!(result.len(), 1);
        assert_eq!(result.events[0].data, "line1\nline2\nline3");
    }

    #[test]
    fn test_legacy_parse_stream_comment_ignored() {
        let result = SSEParser::parse_stream(": comment\ndata: value\n\n");
        assert_eq!(result.len(), 1);
        assert_eq!(result.events[0].data, "value");
    }

    #[test]
    fn test_legacy_parse_stream_incomplete() {
        let result = SSEParser::parse_stream("data: partial");
        // No double newline means no complete event
        assert_eq!(result.len(), 0);
    }

    #[test]
    fn test_legacy_parse_stream_crlf_multibyte_tail() {
        // CRLF lines must be counted by their full byte length. The old
        // `line.len() + 1` accounting dropped the `\r`, so the remaining
        // slice started one byte early per CRLF line and landed inside the
        // final multi-byte character ("中" at bytes 19..22 -> byte 21).
        let buffer = "data:a\r\ndata:中中中";
        let result = SSEParser::parse_stream(buffer);
        assert_eq!(result.len(), 0);
        assert_eq!(result.remaining, "data:中中中");
        assert_eq!(result.consumed_bytes, "data:a\r\n".len());
    }

    #[test]
    fn test_legacy_parse_stream_crlf_consumed_bytes() {
        let buffer = "data: one\r\n\r\ndata: partial";
        let result = SSEParser::parse_stream(buffer);
        assert_eq!(result.len(), 1);
        assert_eq!(result.events[0].data, "one");
        assert_eq!(result.remaining, "data: partial");
        assert_eq!(result.consumed_bytes, "data: one\r\n\r\n".len());
    }

    #[test]
    fn test_legacy_parse_stream_trailing_partial_line_returned() {
        // LF accounting is unchanged: complete lines are consumed, the
        // unterminated tail is handed back byte-for-byte.
        let result = SSEParser::parse_stream("data: a\ndata: b");
        assert_eq!(result.len(), 0);
        assert_eq!(result.remaining, "data: b");
        assert_eq!(result.consumed_bytes, "data: a\n".len());
    }

    #[test]
    fn test_legacy_sse_event_is_keepalive() {
        let evt = SSEEvent::new("");
        assert!(evt.is_keepalive());
        let evt = SSEEvent::new("data");
        assert!(!evt.is_keepalive());
    }

    #[test]
    fn test_legacy_sse_event_to_sse_string() {
        let mut evt = SSEEvent::new("hello world");
        evt.id = Some("1".to_string());
        evt.event = Some("message".to_string());
        let s = evt.to_sse_string();
        assert!(s.contains("id:1\n"));
        assert!(s.contains("event:message\n"));
        assert!(s.contains("data:hello world\n"));
        assert!(s.ends_with("\n"));
    }

    #[test]
    fn test_sse_events_container() {
        let mut container = SSEEvents::new();
        assert!(container.is_empty());
        assert_eq!(container.len(), 0);
        container.events.push(SSEEvent::new("test"));
        assert!(!container.is_empty());
        assert_eq!(container.len(), 1);
        let taken = container.take_events();
        assert_eq!(taken.len(), 1);
        assert!(container.is_empty());
    }
}
