//! Bounded HTTP/1 response assembly. Only message framing can establish completion.

use super::{
    AggregatedResponse, AggregatedResult, ConnectionId, ConnectionState, HttpConnectionAggregator,
    HttpPair,
};
use crate::parser::http::{HttpParser, ParsedHttpMessage, ParsedRequest, ParsedResponse};
use crate::probes::sslsniff::SslEvent;
use anyhow::{Result, bail};
use std::rc::Rc;

const MAX_RESPONSE_HEADERS: usize = 64 * 1024;

/// Interim 1xx statuses that do not finalize the request they answer.
///
/// 101 is excluded: it switches protocols and the response state machine
/// treats it as a final response.
pub(super) fn is_informational(status: u16) -> bool {
    (100..200).contains(&status) && status != 101
}

impl ConnectionState {
    /// Request evidence shared by idle, process-exit and dead-PID persistence.
    pub(crate) fn pending_request(&self) -> Option<&ParsedRequest> {
        match self {
            Self::RequestPending { request } => Some(request),
            Self::ResponsePending { request, .. } | Self::SseActive { request, .. } => {
                request.as_ref()
            }
            Self::Idle | Self::RequestBodyPending { .. } => None,
        }
    }
}

/// In-flight response bytes, retained only within the connection limits.
#[derive(Debug, Clone)]
pub(crate) enum PendingResponse {
    /// Header prefix awaiting the terminating blank line.
    Headers(Rc<SslEvent>),
    /// Parsed headers with undecoded body bytes and a framing cursor.
    Body {
        response: ParsedResponse,
        framing: Framing,
        end_timestamp_ns: u64,
    },
}

/// HTTP message-length rules, applied before content decoding.
#[derive(Debug, Clone)]
pub(crate) enum Framing {
    /// Content-Length or a response that cannot carry a body.
    Length(usize),
    /// Cursor at the next chunk-size or trailer line.
    Chunked { offset: usize, trailers: bool },
    /// No close notification is available; idle timeout cannot imply completion.
    CloseDelimited,
}

impl Framing {
    fn new(response: &ParsedResponse, method: Option<&str>) -> Result<Self> {
        if method == Some("HEAD")
            || (100..200).contains(&response.status_code)
            || matches!(response.status_code, 204 | 304)
            || (method == Some("CONNECT") && (200..300).contains(&response.status_code))
        {
            return Ok(Self::Length(0));
        }
        if let Some(encoding) = response.headers.get("transfer-encoding") {
            let chunked = encoding
                .rsplit(',')
                .next()
                .is_some_and(|v| v.trim().eq_ignore_ascii_case("chunked"));
            return Ok(if chunked {
                Self::Chunked {
                    offset: 0,
                    trailers: false,
                }
            } else {
                Self::CloseDelimited
            });
        }
        match response.headers.get("content-length") {
            Some(length) => Ok(Self::Length(length.trim().parse()?)),
            None => Ok(Self::CloseDelimited),
        }
    }

    fn complete_len(&mut self, body: &[u8]) -> Result<Option<usize>> {
        match self {
            Self::Length(len) => Ok((body.len() >= *len).then_some(*len)),
            Self::CloseDelimited => Ok(None),
            Self::Chunked { offset, trailers } => loop {
                let Some(line_len) = body[*offset..].windows(2).position(|w| w == b"\r\n") else {
                    return Ok(None);
                };
                let line_end = *offset + line_len;
                if *trailers {
                    *offset = line_end + 2;
                    if line_len == 0 {
                        return Ok(Some(*offset));
                    }
                    continue;
                }
                let line = std::str::from_utf8(&body[*offset..line_end])?;
                let size = usize::from_str_radix(line.split(';').next().unwrap_or("").trim(), 16)?;
                if size == 0 {
                    *offset = line_end + 2;
                    *trailers = true;
                    continue;
                }
                let Some(end) = (line_end + 2)
                    .checked_add(size)
                    .and_then(|n| n.checked_add(2))
                else {
                    bail!("chunk size overflows response length");
                };
                if end > body.len() {
                    return Ok(None);
                }
                if &body[end - 2..end] != b"\r\n" {
                    bail!("missing CRLF after response chunk");
                }
                *offset = end;
            },
        }
    }
}

impl PendingResponse {
    /// Bytes retained while response framing is incomplete.
    pub(super) fn buffered_bytes(&self) -> usize {
        match self {
            Self::Headers(event) => event.buf.len(),
            Self::Body { response, .. } => response.source_event.buf.len(),
        }
    }

    /// Select message framing without decoding the response body.
    pub(super) fn body(response: ParsedResponse, method: Option<&str>) -> Result<Self> {
        let framing = Framing::new(&response, method)?;
        let end_timestamp_ns = response.source_event.timestamp_ns;
        Ok(Self::Body {
            response,
            framing,
            end_timestamp_ns,
        })
    }

    /// SSL direction that owns the response.
    pub(super) fn direction(&self) -> i32 {
        match self {
            Self::Headers(event) => event.rw,
            Self::Body { response, .. } => response.source_event.rw,
        }
    }

    /// Append captured bytes after enforcing the connection budget.
    pub(super) fn append(&mut self, event: &SslEvent, limit: usize) -> Result<()> {
        let data = &event.buf[..event.buf_size() as usize];
        let (source, used, cap) = match self {
            Self::Headers(source) => {
                let len = source.buf.len();
                (source, len, limit.saturating_add(MAX_RESPONSE_HEADERS))
            }
            Self::Body {
                response,
                end_timestamp_ns,
                ..
            } => {
                *end_timestamp_ns = event.timestamp_ns;
                let used = response.body_len;
                response.body_len = used.saturating_add(data.len());
                (&mut response.source_event, used, limit)
            }
        };
        if data.len() > cap.saturating_sub(used) {
            bail!("response buffer exceeds {cap} bytes");
        }
        let source = Rc::make_mut(source);
        source.buf.extend_from_slice(data);
        source.len = source.buf.len() as u32;
        Ok(())
    }

    /// Parse a bounded complete header block, preserving its first timestamp.
    pub(super) fn parsed_headers(&self) -> Result<Option<ParsedResponse>> {
        let Self::Headers(event) = self else {
            return Ok(None);
        };
        let Some(end) = event.buf.windows(4).position(|w| w == b"\r\n\r\n") else {
            if event.buf.len() > MAX_RESPONSE_HEADERS {
                bail!("response headers exceed {MAX_RESPONSE_HEADERS} bytes");
            }
            return Ok(None);
        };
        if end + 4 > MAX_RESPONSE_HEADERS {
            bail!("response headers exceed {MAX_RESPONSE_HEADERS} bytes");
        }
        match HttpParser::new().parse(Rc::clone(event))? {
            ParsedHttpMessage::Response(response) => Ok(Some(response)),
            _ => bail!("invalid response headers"),
        }
    }

    /// Trim to the framed message length once all bytes are present.
    pub(super) fn complete(&mut self, limit: usize) -> Result<bool> {
        let Self::Body {
            response, framing, ..
        } = self
        else {
            return Ok(false);
        };
        if response.body_len > limit || matches!(framing, Framing::Length(n) if *n > limit) {
            bail!("response body exceeds {limit} bytes");
        }
        let Some(len) = framing.complete_len(response.body())? else {
            return Ok(false);
        };
        response.body_len = len;
        Ok(true)
    }
}

impl HttpConnectionAggregator {
    /// Whether connection state takes precedence over stateless protocol detection.
    pub(crate) fn accepts_response_bytes(&self, event: &SslEvent) -> bool {
        match self.connections.peek(&ConnectionId::from_ssl_event(event)) {
            Some(ConnectionState::ResponsePending { assembly, .. }) => {
                assembly.direction() == event.rw
            }
            _ => false,
        }
    }

    /// Recognize a response prefix on a pending request connection.
    pub(super) fn starts_response(&self, event: &SslEvent) -> bool {
        let data = &event.buf[..event.buf_size() as usize];
        match self.connections.peek(&ConnectionId::from_ssl_event(event)) {
            Some(
                ConnectionState::RequestPending { request }
                | ConnectionState::RequestBodyPending { request, .. },
            ) => {
                request.source_event.rw != event.rw
                    && !data.is_empty()
                    && (data.starts_with(b"HTTP/1.") || b"HTTP/1.".starts_with(data))
            }
            _ => false,
        }
    }

    /// Begin bounded assembly or emit an already complete response.
    pub(super) fn start_response(
        &mut self,
        id: ConnectionId,
        request: Option<ParsedRequest>,
        response: ParsedResponse,
    ) -> Option<AggregatedResult> {
        match PendingResponse::body(response, request.as_ref().map(|r| r.method.as_str())) {
            Ok(assembly) => self.advance_response(id, request, assembly),
            Err(error) => {
                log::warn!("[HttpAggregator] discarded invalid response | conn={id:?}: {error}");
                None
            }
        }
    }

    fn advance_response(
        &mut self,
        id: ConnectionId,
        request: Option<ParsedRequest>,
        mut assembly: PendingResponse,
    ) -> Option<AggregatedResult> {
        match assembly.complete(self.max_body_bytes) {
            Ok(true) => {
                if let PendingResponse::Body {
                    response,
                    end_timestamp_ns,
                    ..
                } = assembly
                {
                    let mut response = AggregatedResponse::from_parsed(response);
                    response.completion_timestamp_ns = Some(end_timestamp_ns);
                    return Some(match request {
                        Some(request) => AggregatedResult::HttpComplete(HttpPair {
                            connection_id: id,
                            request,
                            response,
                        }),
                        None => AggregatedResult::ResponseOnly {
                            connection_id: id,
                            response,
                        },
                    });
                }
            }
            Ok(false) => self.insert(id, ConnectionState::ResponsePending { request, assembly }),
            Err(error) => {
                log::warn!("[HttpAggregator] discarded incomplete response | conn={id:?}: {error}")
            }
        }
        None
    }

    /// Continue response assembly using the original SSL bytes.
    pub(super) fn process_response_bytes(&mut self, event: &SslEvent) -> Option<AggregatedResult> {
        let id = ConnectionId::from_ssl_event(event);
        let state = self.connections.pop(&id)?;
        let (request, assembly) = match state {
            ConnectionState::ResponsePending {
                request,
                mut assembly,
            } => {
                if let Err(error) = assembly.append(event, self.max_body_bytes) {
                    log::warn!(
                        "[HttpAggregator] discarded incomplete response | conn={id:?}: {error}"
                    );
                    return None;
                }
                (request, assembly)
            }
            ConnectionState::RequestPending { request } => (
                Some(request),
                PendingResponse::Headers(Rc::new(event.clone())),
            ),
            ConnectionState::RequestBodyPending {
                mut request,
                expected_body_len,
                body_buffer,
            } => {
                let assembly = PendingResponse::Headers(Rc::new(event.clone()));
                // An interim 1xx response (`100 Continue`) does not answer the
                // request: a client that sent `Expect: 100-continue` writes the
                // body only after receiving it. Collapsing to RequestPending
                // here would expose `reassembled_body` as the empty partial
                // body and every body continuation afterwards would hit the
                // no-op fallback of `process_raw_body_data`, silently dropping
                // the prompt. Keep the pending body state instead.
                if matches!(
                    assembly.parsed_headers(),
                    Ok(Some(ref response)) if is_informational(response.status_code)
                ) {
                    self.insert(
                        id,
                        ConnectionState::RequestBodyPending {
                            request,
                            expected_body_len,
                            body_buffer,
                        },
                    );
                    return None;
                }
                request.reassembled_body = Some(body_buffer);
                (Some(request), assembly)
            }
            other => {
                self.insert(id, other);
                return None;
            }
        };
        match assembly.parsed_headers() {
            Ok(Some(response)) => {
                drop(assembly);
                if let Some(request) = request {
                    self.insert(id, ConnectionState::RequestPending { request });
                } else {
                    self.insert(id, ConnectionState::Idle);
                }
                let mut result = self.process_response(response);
                if let Some(AggregatedResult::HttpComplete(pair)) = &mut result {
                    pair.response.completion_timestamp_ns = Some(event.timestamp_ns);
                }
                if let Some(ConnectionState::ResponsePending {
                    assembly:
                        PendingResponse::Body {
                            end_timestamp_ns, ..
                        },
                    ..
                }) = self.connections.peek_mut(&id)
                {
                    *end_timestamp_ns = event.timestamp_ns;
                }
                return result;
            }
            Err(error) => {
                log::warn!(
                    "[HttpAggregator] discarded invalid response headers | conn={id:?}: {error}"
                );
                return None;
            }
            Ok(None) => {}
        }
        self.advance_response(id, request, assembly)
    }
}
