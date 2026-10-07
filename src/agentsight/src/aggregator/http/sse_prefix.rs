//! One-time decoding of a marked stream prefix retained across TLS reads.
use crate::parser::sse::{ParsedSseEvent, SseParser};
use crate::probes::sslsniff::SslEvent;
use std::rc::Rc;

#[derive(Debug, Default)]
pub(super) struct SseReadState {
    pub last_source_ptr: Option<usize>,
    pub last_chunk_start: usize,
    pub bom_checked: bool,
}

/// Recover only the first event, without treating later reads as stream starts.
/// Returns true when a prefix-only RawData read completes the stream.
pub(super) fn repair_prefix(
    prefix: &[u8],
    events: &mut Vec<ParsedSseEvent>,
    source: &SslEvent,
    state: &mut SseReadState,
) -> bool {
    if state.bom_checked || prefix.is_empty() {
        return false;
    }
    const BOM: &[u8] = b"\xef\xbb\xbf";
    if !prefix.starts_with(&BOM[..prefix.len().min(BOM.len())]) {
        state.bom_checked = true;
        return false;
    }
    if prefix.len() < BOM.len() {
        return false;
    }

    // Restrict recovery to the first block. An unknown/comment-only block
    // must not give a later identical payload the first event's identity.
    let mut line_start = BOM.len();
    let mut pos = line_start;
    let mut block_end = None;
    while pos < prefix.len() {
        if matches!(prefix[pos], b'\r' | b'\n') {
            let end =
                pos + 1 + usize::from(prefix[pos] == b'\r' && prefix.get(pos + 1) == Some(&b'\n'));
            if pos == line_start {
                block_end = Some(end);
                break;
            }
            line_start = end;
            pos = end;
        } else {
            pos += 1;
        }
    }
    let bytes = prefix[..block_end.unwrap_or(prefix.len())].to_vec();
    let synthetic = SslEvent {
        source: source.source,
        timestamp_ns: source.timestamp_ns,
        delta_ns: source.delta_ns,
        pid: source.pid,
        tid: source.tid,
        uid: source.uid,
        len: bytes.len() as u32,
        rw: source.rw,
        comm: source.comm.clone(),
        buf: bytes,
        is_handshake: source.is_handshake,
        ssl_ptr: source.ssl_ptr,
    };
    let candidate = SseParser::new()
        .parse_at_stream_start(Rc::new(synthetic))
        .into_iter()
        .next();
    let Some(candidate) = candidate else {
        state.bom_checked = block_end.is_some() || prefix.len() >= 1 << 20;
        return false;
    };
    if candidate.data_len() == 0 && !candidate.is_done() && block_end.is_none() {
        state.bom_checked = prefix.len() >= 1 << 20;
        return false;
    }
    state.bom_checked = true;
    let done = candidate.is_done();
    let source_ptr = source as *const _ as usize;
    if let Some(first) = events.first_mut() {
        let original = first.source_event();
        let original_ptr = original as *const _ as usize;
        let start = if original_ptr == source_ptr {
            Some(state.last_chunk_start)
        } else if original.buf.starts_with(BOM) && prefix.starts_with(&original.buf) {
            Some(0)
        } else {
            None
        };
        if start.is_some_and(|start| start + first.data_offset() == candidate.data_offset())
            && first.data() == candidate.data()
        {
            // Keep the original Rc: pointer dedup must not retain a freed
            // allocation that could be reused by the next SSL read.
            first.id = candidate.id;
            first.event = candidate.event;
            first.retry = candidate.retry;
            return done;
        }
    }
    events.insert(0, candidate);
    done
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(bytes: &[u8]) -> Rc<SslEvent> {
        Rc::new(SslEvent {
            source: 0,
            timestamp_ns: 1,
            delta_ns: 0,
            pid: 1,
            tid: 1,
            uid: 0,
            len: bytes.len() as u32,
            rw: 0,
            comm: "fixture".into(),
            buf: bytes.to_vec(),
            is_handshake: false,
            ssl_ptr: 1,
        })
    }

    #[test]
    fn named_prefix_repairs_metadata_without_replacing_source() {
        let read = source(b"\xef\xbb\xbfevent: update\ndata: payload\n\n");
        let mut events = SseParser::new().parse(read.clone());
        assert!(events[0].event.is_none());
        let original = events[0].source_event() as *const _;
        assert!(!repair_prefix(
            &read.buf,
            &mut events,
            &read,
            &mut SseReadState::default()
        ));
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event.as_deref(), Some("update"));
        assert_eq!(events[0].source_event() as *const _, original);
    }

    #[test]
    fn first_block_identity_and_late_marks_are_preserved() {
        let read = source(b"\xef\xbb\xbfdata: same\n\ndata: same\n\n");
        let mut events = SseParser::new().parse(read.clone());
        assert_eq!(events.len(), 1);
        let mut state = SseReadState::default();
        repair_prefix(&read.buf, &mut events, &read, &mut state);
        assert_eq!(events.len(), 2);
        let late = source(b"\xef\xbb\xbfdata: ignored\n\n");
        assert!(!repair_prefix(&late.buf, &mut events, &late, &mut state));
        assert_eq!(events.len(), 2);
        let comment = source(b"\xef\xbb\xbf: first block\n\ndata: later\n\n");
        let mut events = SseParser::new().parse(comment.clone());
        repair_prefix(
            &comment.buf,
            &mut events,
            &comment,
            &mut SseReadState::default(),
        );
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].data(), b"later");
    }
}
