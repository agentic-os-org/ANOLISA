//! SSE framing retains an unfinished event and reduced output instead of TLS history.

use super::chunk_stream::ChunkStream;
use super::sse_reduce::SseReducer;
use crate::parser::http::ParsedResponse;
use crate::parser::sse::ParsedSseEvent;
use crate::parser::sse::parser::SseFrame;
use crate::probes::sslsniff::SslEvent;
use crate::utils::stream_decode::StreamDecoder;
use std::io;

#[derive(Debug)]
pub(crate) struct SseStream {
    chunks: Option<ChunkStream>,
    decoder: StreamDecoder,
    lines: Lines,
    remaining: Option<usize>,
    source: SslEvent,
    pub(super) last_timestamp_ns: u64,
}

#[derive(Debug)]
struct Lines {
    pending: Vec<u8>,
    scan: usize,
    line_start: usize,
    reducer: SseReducer,
    limit: usize,
    at_start: bool,
    done: bool,
}

impl Lines {
    fn feed(&mut self, bytes: &[u8], source: &SslEvent) -> io::Result<()> {
        self.pending.extend_from_slice(bytes);
        let mut consumed = 0;
        let result = (|| {
            while self.scan < self.pending.len() {
                let Some(offset) = memchr::memchr2(b'\r', b'\n', &self.pending[self.scan..]) else {
                    self.scan = self.pending.len();
                    break;
                };
                let pos = self.scan + offset;
                if self.pending[pos] == b'\r' && pos + 1 == self.pending.len() {
                    self.scan = pos;
                    break;
                }
                let end = pos
                    + 1
                    + usize::from(
                        self.pending[pos] == b'\r' && self.pending.get(pos + 1) == Some(&b'\n'),
                    );
                self.scan = end;
                let blank = pos == self.line_start;
                self.line_start = end;
                if blank {
                    if end - consumed > self.limit {
                        return Err(io::Error::new(
                            io::ErrorKind::OutOfMemory,
                            "SSE event exceeds byte limit",
                        ));
                    }
                    let frame = SseFrame::parse(&self.pending[consumed..end], self.at_start);
                    self.at_start = false;
                    consumed = end;
                    if let Some(frame) = frame {
                        self.done |= self.reducer.push(frame, source);
                        if self.reducer.retained_bytes() > self.limit {
                            return Err(io::Error::new(
                                io::ErrorKind::OutOfMemory,
                                "reduced SSE output exceeds byte limit",
                            ));
                        }
                    }
                }
            }
            Ok(())
        })();
        if consumed != 0 {
            self.pending.drain(..consumed);
            self.scan -= consumed;
            self.line_start -= consumed;
            if self.pending.is_empty() && self.pending.capacity() > 64 * 1024 {
                self.pending = Vec::new();
            }
        }
        result?;
        if self.pending.len() > self.limit {
            return Err(io::Error::new(
                io::ErrorKind::OutOfMemory,
                "unfinished SSE event exceeds byte limit",
            ));
        }
        Ok(())
    }
}

impl SseStream {
    pub(crate) fn new(response: &ParsedResponse, limit: usize) -> io::Result<Self> {
        let body = response.body();
        let encoding = response.content_encoding().or_else(|| {
            if body.starts_with(&[0x1f, 0x8b]) {
                Some("gzip")
            } else if body.starts_with(&[0x28, 0xb5, 0x2f, 0xfd]) {
                Some("zstd")
            } else {
                None
            }
        });
        let chunks = response
            .headers
            .get("transfer-encoding")
            .is_some_and(|value| {
                value
                    .split(',')
                    .any(|value| value.trim().eq_ignore_ascii_case("chunked"))
            });
        Ok(Self {
            source: response.source_event.metadata_only(),
            chunks: chunks.then(ChunkStream::default),
            decoder: StreamDecoder::new(encoding, limit)?,
            lines: Lines {
                pending: Vec::new(),
                scan: 0,
                line_start: 0,
                reducer: SseReducer::default(),
                limit,
                at_start: true,
                done: false,
            },
            remaining: (!chunks)
                .then(|| {
                    response
                        .headers
                        .get("content-length")
                        .and_then(|value| value.parse().ok())
                })
                .flatten(),
            last_timestamp_ns: response.source_event.timestamp_ns,
        })
    }

    pub(crate) fn feed(&mut self, bytes: &[u8], source: &SslEvent) -> io::Result<()> {
        self.last_timestamp_ns = source.timestamp_ns;
        if let Some(remaining) = self.remaining.as_mut() {
            *remaining = remaining.saturating_sub(bytes.len());
        }
        let Self {
            chunks,
            decoder,
            lines,
            ..
        } = self;
        let mut consume = |data: &[u8]| {
            decoder.feed(data, |decoded| {
                for fragment in decoded.chunks(16 * 1024) {
                    lines.feed(fragment, source)?;
                }
                Ok(())
            })
        };
        if let Some(chunks) = chunks {
            chunks.feed(bytes, &mut consume)?;
        } else {
            consume(bytes)?;
        }
        if self.chunks.as_ref().is_some_and(ChunkStream::done) || self.remaining == Some(0) {
            self.finish(source)?;
        }
        Ok(())
    }

    pub(crate) fn finish(&mut self, source: &SslEvent) -> io::Result<()> {
        if !self.lines.pending.is_empty() {
            self.lines.feed(b"\n\n", source)?;
        }
        Ok(())
    }

    /// Respect an explicit DashScope mode; otherwise retain compatible prefix detection.
    pub(crate) fn set_native_incremental(&mut self, enabled: Option<bool>) {
        self.lines.reducer.native_incremental = enabled;
    }

    pub(crate) fn done(&self) -> bool {
        self.lines.done
            || self.chunks.as_ref().is_some_and(ChunkStream::done)
            || self.remaining == Some(0)
    }

    pub(crate) fn payload_retained_bytes(&self) -> usize {
        self.lines.pending.capacity()
            + self.lines.reducer.retained_bytes()
            + self.chunks.as_ref().map_or(0, ChunkStream::retained_bytes)
    }

    pub(crate) fn retained_bytes(&self) -> usize {
        self.lines.pending.capacity()
            + self.lines.reducer.retained_bytes()
            + self.chunks.as_ref().map_or(0, ChunkStream::retained_bytes)
            + self.decoder.retained_bytes()
            + std::mem::size_of::<SslEvent>()
            + self.source.comm.capacity()
    }

    pub(crate) fn into_events(mut self) -> (Vec<ParsedSseEvent>, usize, u64) {
        let mut source = self.source.metadata_only();
        source.timestamp_ns = self.last_timestamp_ns;
        if let Err(error) = self.finish(&source) {
            log::warn!("Cannot fully finalize buffered SSE evidence: {error}");
        }
        let Self {
            decoder,
            chunks,
            lines,
            last_timestamp_ns,
            ..
        } = self;
        drop(decoder);
        drop(chunks);
        let Lines {
            reducer, pending, ..
        } = lines;
        drop(pending);
        let count = reducer.count;
        (reducer.into_events(), count, last_timestamp_ns)
    }
}
