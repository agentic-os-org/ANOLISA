//! Bounded assembly of request headers before a typed request exists.

use super::pending_response::MAX_RESPONSE_HEADERS as MAX_REQUEST_HEADERS;
use super::{ConnectionId, ConnectionState, HttpConnectionAggregator};
use crate::aggregator::RemoveReason;
use crate::parser::http::{HttpParser, ParsedHttpMessage, ParsedRequest};
use crate::probes::sslsniff::SslEvent;
use std::rc::Rc;

/// Recognize a supported request method, including a partial method token.
pub(super) fn could_start_request(bytes: &[u8]) -> bool {
    // Match the methods recognized by SslEvent, including a split method token.
    const METHODS: &[&[u8]] = &[
        b"GET ",
        b"POST ",
        b"PUT ",
        b"DELETE ",
        b"HEAD ",
        b"OPTIONS ",
        b"PATCH ",
    ];
    !bytes.is_empty()
        && METHODS
            .iter()
            .any(|method| method.starts_with(bytes) || bytes.starts_with(method))
}

/// Retain a prefix or delegate a complete header block to the HTTP parser.
pub(super) fn parse_headers(
    event: &Rc<SslEvent>,
    max_headers: usize,
) -> Result<Option<ParsedRequest>, RemoveReason> {
    if !could_start_request(&event.buf) {
        return Err(RemoveReason::ProtocolError);
    }
    let mut headers = vec![httparse::EMPTY_HEADER; max_headers.max(1)];
    let mut request = httparse::Request::new(&mut headers);
    match request.parse(&event.buf) {
        Ok(httparse::Status::Partial) if event.buf.len() <= MAX_REQUEST_HEADERS => Ok(None),
        Ok(httparse::Status::Complete(end)) if end <= MAX_REQUEST_HEADERS => {
            match HttpParser::with_max_headers(max_headers).parse(Rc::clone(event)) {
                Ok(ParsedHttpMessage::Request(request)) => Ok(Some(request)),
                _ => Err(RemoveReason::ProtocolError),
            }
        }
        Ok(_) => Err(RemoveReason::OverBudget),
        Err(_) => Err(RemoveReason::ProtocolError),
    }
}

/// Append one capture inside the shared header and connection body bounds.
pub(super) fn append_headers(
    event: &mut Rc<SslEvent>,
    incoming: &SslEvent,
    body_limit: usize,
    max_headers: usize,
) -> Result<Option<ParsedRequest>, RemoveReason> {
    if incoming.buf.len()
        > body_limit
            .saturating_add(MAX_REQUEST_HEADERS)
            .saturating_sub(event.buf.len())
    {
        return Err(RemoveReason::OverBudget);
    }
    let source = Rc::make_mut(event);
    source.buf.reserve_exact(incoming.buf.len());
    source.buf.extend_from_slice(&incoming.buf);
    source.len = u32::try_from(source.buf.len()).unwrap_or(u32::MAX);
    parse_headers(event, max_headers)
}

impl HttpConnectionAggregator {
    /// Assemble captured request header bytes before a typed request exists.
    pub(super) fn process_request_header_bytes(&mut self, incoming: &SslEvent) -> bool {
        let id = ConnectionId::from_ssl_event(incoming);
        let progress = match self.connections.get_mut(&id) {
            Some(ConnectionState::RequestHeadersPending { event }) if event.rw == incoming.rw => {
                append_headers(event, incoming, self.max_body_bytes, self.max_headers)
            }
            None | Some(ConnectionState::Idle) if could_start_request(&incoming.buf) => {
                let event = Rc::new(incoming.clone());
                match parse_headers(&event, self.max_headers) {
                    Ok(None) => {
                        self.connections.pop(&id);
                        self.insert(id, ConnectionState::RequestHeadersPending { event });
                        return true;
                    }
                    Ok(Some(request)) => {
                        self.process_request(request);
                        return true;
                    }
                    Err(_) => return true,
                }
            }
            _ => return false,
        };
        match progress {
            Ok(None) => {
                if let Some(state) = self.connections.pop(&id) {
                    self.insert(id, state);
                }
            }
            Ok(Some(request)) => {
                self.connections.pop(&id);
                self.process_request(request);
            }
            Err(reason) => self.abandon(&id, reason),
        }
        true
    }

    /// Original capture bytes owned by an incomplete request header block.
    pub(crate) fn accepts_request_header_bytes(&self, event: &SslEvent) -> bool {
        self.connections
            .peek(&ConnectionId::from_ssl_event(event))
            .and_then(ConnectionState::pending_header_source)
            .is_some_and(|source| source.rw == event.rw)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aggregator::{Aggregator, HttpConnectionAggregator};
    use crate::config::RuntimeLimits;
    use crate::event::Event;
    use crate::parser::Parser;
    use std::time::Duration;

    fn event(id: u64, bytes: &[u8]) -> SslEvent {
        SslEvent {
            source: 0,
            timestamp_ns: 1_000_000_000,
            delta_ns: 0,
            pid: 123,
            tid: 123,
            uid: 0,
            len: bytes.len() as u32,
            rw: 1,
            comm: "fixture".into(),
            buf: bytes.to_vec(),
            is_handshake: false,
            ssl_ptr: id,
        }
    }

    #[test]
    fn completing_request_headers_is_a_transition_not_a_replacement() {
        for rw in [0, 1] {
            for coalesced_body in [false, true] {
                let recorder =
                    metrics_exporter_prometheus::PrometheusBuilder::new().build_recorder();
                let handle = recorder.handle();
                metrics::with_local_recorder(&recorder, || {
                    let mut http = HttpConnectionAggregator::with_capacity(1);
                    let mut prefix = event(1, b"PO");
                    prefix.rw = rw;
                    assert!(http.process_raw_body_data(&prefix).is_none());

                    let mut bytes = b"ST / HTTP/1.1\r\nContent-Length: 2\r\n\r\n".to_vec();
                    if coalesced_body {
                        bytes.extend_from_slice(b"{}");
                    }
                    let mut tail = event(1, &bytes);
                    tail.rw = rw;
                    assert!(http.process_raw_body_data(&tail).is_none());
                    let id = ConnectionId::from_ssl_event(&prefix);
                    match http.connections.peek(&id) {
                        Some(ConnectionState::RequestPending { request }) if coalesced_body => {
                            assert_eq!(request.body(), b"{}");
                        }
                        Some(ConnectionState::RequestBodyPending { body_buffer, .. })
                            if !coalesced_body =>
                        {
                            assert!(body_buffer.is_empty());
                        }
                        state => panic!("expected completed request headers, got {state:?}"),
                    }
                    assert_eq!(http.metrics().pending_connection_count, 1);
                    assert!(http.metrics().connection_cache_bytes > 0);
                    assert!(http.take_ready().is_empty());
                    let snapshot = handle.render();
                    assert!(
                        !snapshot.lines().any(|line| {
                            line.starts_with("agentsight_http_conn_removed_total{")
                                && line.contains("reason=\"replaced\"")
                        }),
                        "successful header assembly must not report replacement: {snapshot}"
                    );

                    let mut new_request =
                        event(1, b"POST / HTTP/1.1\r\nContent-Length: 2\r\n\r\n{}");
                    new_request.rw = rw;
                    let ParsedHttpMessage::Request(request) =
                        HttpParser::new().parse(Rc::new(new_request)).unwrap()
                    else {
                        panic!("expected a request");
                    };
                    http.process_request(request);
                    let snapshot = handle.render();
                    assert!(snapshot.lines().any(|line| {
                        line.starts_with("agentsight_http_conn_removed_total{")
                            && line.contains("reason=\"replaced\"")
                            && line.ends_with(" 1")
                    }));
                    assert_eq!(http.metrics().pending_connection_count, 1);
                    http.abandon(&id, RemoveReason::PidExited);
                    assert_eq!(http.metrics().pending_connection_count, 0);
                    assert_eq!(http.metrics().connection_cache_bytes, 0);
                });
            }
        }
    }

    #[test]
    fn method_prefixes_are_bounded_by_supported_methods() {
        for bytes in [b"P".as_slice(), b"POST /", b"G", b"GET /", b"PATCH /"] {
            assert!(could_start_request(bytes));
        }
        for bytes in [b"".as_slice(), b"junk", b"PRI * HTTP/2.0", b"POSTX"] {
            assert!(!could_start_request(bytes));
        }
        assert!(matches!(
            parse_headers(&Rc::new(event(1, b"PURGE / HTTP/1.1\r\n\r\n")), 64),
            Err(RemoveReason::ProtocolError)
        ));
    }

    #[test]
    fn header_limit_does_not_count_a_coalesced_body_as_header_bytes() {
        let mut bytes = b"POST / HTTP/1.1\r\nContent-Length: 100000\r\n\r\n".to_vec();
        bytes.resize(bytes.len() + 100_000, b'x');
        assert!(matches!(
            parse_headers(&Rc::new(event(1, &bytes)), 64),
            Ok(Some(_))
        ));
        let mut oversized = b"POST / HTTP/1.1\r\nX-Fixture: ".to_vec();
        oversized.resize(MAX_REQUEST_HEADERS + 1, b'x');
        assert!(matches!(
            parse_headers(&Rc::new(event(1, &oversized)), 64),
            Err(RemoveReason::OverBudget)
        ));
    }

    #[test]
    fn incomplete_headers_share_count_and_retirement_lifecycle() {
        let mut http = HttpConnectionAggregator::with_capacity(1);
        http.process_raw_body_data(&event(1, b"PO"));
        assert_eq!(http.metrics().pending_connection_count, 1);
        assert!(http.metrics().connection_cache_bytes > 0);
        http.process_raw_body_data(&event(2, b"PO"));
        assert_eq!(http.metrics().pending_connection_count, 1);
        assert!(http.take_ready().is_empty());
        let ids: Vec<_> = http.connections.iter().map(|(id, _)| *id).collect();
        for id in ids {
            http.abandon(&id, RemoveReason::PidExited);
        }
        assert_eq!(http.metrics().connection_cache_bytes, 0);
        assert_eq!(http.metrics().pending_connection_count, 0);
        assert!(http.take_ready().is_empty());
    }

    #[test]
    fn incomplete_headers_are_released_on_hard_timeout() {
        let mut http = HttpConnectionAggregator::new();
        http.hard_timeout = Duration::from_secs(1);
        http.process_raw_body_data(&event(1, b"PO"));
        let id = super::super::ConnectionId::from_ssl_event(&event(1, b""));
        http.last_activity
            .put(id, std::time::Instant::now() - Duration::from_secs(2));
        http.evict_idle_and_oversized();
        assert_eq!(http.metrics().connection_cache_bytes, 0);
        assert_eq!(http.metrics().pending_connection_count, 0);
    }

    #[test]
    fn header_growth_is_charged_to_the_shared_connection_byte_budget() {
        let limits = RuntimeLimits {
            connection_max_bytes: 2048,
            ..Default::default()
        };
        let mut aggregator = Aggregator::with_limits(8, &limits);
        aggregator.process_result(Parser::new().parse_event(Event::Ssl(event(1, b"PO"))));
        assert_eq!(aggregator.connection_metrics().pending_connection_count, 1);
        let mut tail = b"ST / HTTP/1.1\r\nX-Fixture: ".to_vec();
        tail.resize(4096, b'x');
        let output =
            aggregator.process_result(Parser::new().parse_event(Event::Ssl(event(1, &tail))));
        assert!(output.is_empty());
        assert!(
            aggregator.connection_metrics().connection_cache_bytes <= limits.connection_max_bytes
        );
    }
}
