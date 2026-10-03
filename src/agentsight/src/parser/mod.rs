//! Parser module for protocol parsing
//!
//! This module provides parsers for various protocols including HTTP and SSE.
//!
//! # Quick Start
//! ```rust,ignore
//! use agentsight::parser::{HttpParser, SseParser, ParsedHttpMessage};
//! use agentsight::aggregator::HttpConnectionAggregator;
//!
//! let http_parser = HttpParser::new();
//! let sse_parser = SseParser::new();
//! let mut aggregator = HttpConnectionAggregator::new();
//!
//! // Parse and aggregate
//! for ssl_event in ssl_events {
//!     if let Some(msg) = http_parser.parse(rc_event.clone()) {
//!         // Handle HTTP message
//!     } else {
//!         let sse_events = sse_parser.parse(rc_event);
//!         // Handle SSE events
//!     }
//! }
//! ```
//!
//! For a unified interface, use `Parser`:
//! ```rust,ignore
//! use agentsight::parser::Parser;
//!
//! let mut parser = Parser::new();
//! let result = parser.parse_ssl_event(&ssl_event);
//! ```

pub mod http;
pub mod http2;
pub mod proctrace;
mod result;
pub mod sse;
mod unified;

// Re-export result types
pub use result::{ParseResult, ParsedMessage};

// Re-export unified parser
pub use unified::Parser;

// Re-export HTTP types
pub use http::{HttpParser, ParsedHttpMessage, ParsedRequest, ParsedResponse};

// Re-export SSE types
pub use sse::{ParsedSseEvent, SseParser};

// Re-export proctrace types
pub use proctrace::{ParsedProcEvent, ProcEventType, ProcTraceParser};

// Re-export HTTP/2 types
pub use http2::{Http2FrameType, Http2Parser, ParsedHttp2Frame};

/// Longest prefix of `s` that fits in `max_bytes` and ends on a char boundary.
///
/// Trace previews and debug output shorten captured payloads by a byte budget,
/// but payloads carry model output, which is routinely non-ASCII. Slicing a
/// `&str` at an arbitrary byte offset panics (`byte index N is not a char
/// boundary`) and takes the whole capture path down with it, so every shortening
/// site goes through here instead. A multi-byte character that would straddle the
/// budget is dropped rather than truncated, so the result is at most `max_bytes`
/// long.
pub fn truncate_for_preview(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

#[cfg(test)]
mod tests {
    use super::truncate_for_preview;

    #[test]
    fn truncate_for_preview_keeps_short_input_intact() {
        assert_eq!(truncate_for_preview("abc", 200), "abc");
        assert_eq!(truncate_for_preview("", 0), "");
    }

    #[test]
    fn truncate_for_preview_does_not_split_multibyte_characters() {
        // 67 three-byte characters: byte 200 falls inside the 67th.
        let s = "中".repeat(67);
        assert_eq!(s.len(), 201);
        let cut = truncate_for_preview(&s, 200);
        assert_eq!(cut.len(), 198, "the straddling character is dropped whole");
        assert!(cut.chars().all(|c| c == '中'));

        // Every budget in a multi-byte string must be a valid boundary.
        let mixed = "aé中🙂b";
        for max in 0..=mixed.len() {
            let cut = truncate_for_preview(mixed, max);
            assert!(cut.len() <= max);
            assert!(
                mixed.starts_with(cut),
                "truncation must not alter the prefix"
            );
        }
    }
}
