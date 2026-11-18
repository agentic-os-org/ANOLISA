//! Stdio transport with inbound frame bounding.
//!
//! The official `Stdio` reads whole lines before yielding them, so a peer that
//! never sends a newline can make the reader allocate without bound. This keeps
//! the official connection, codec and outgoing framing, and replaces only the
//! incoming line stream with one that enforces a byte cap before allocation.
//!
//! The outgoing sink also carries the post-EOF close budget: the official
//! `connect_to` drain waits for the sink without a deadline, so a peer that stops
//! reading would keep the process resident forever.
//!
//! The same sink confirms the connection-level in-flight admission gauge, which
//! bounds the notifications the official unbounded outgoing queue may hold while
//! the connection is still alive and no EOF has armed the close budget.

use std::io;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use agent_client_protocol::Lines;
use futures::Stream;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
use tokio::sync::watch;

/// One-shot close budget shared by the incoming EOF signal and the outgoing sink.
///
/// Armed once at clean incoming EOF and never restarted by write progress.
/// Before EOF no deadline applies, so a slow but healthy peer is not cut off.
#[derive(Clone)]
pub(crate) struct CloseWatch {
    grace: Duration,
    deadline: Arc<watch::Sender<Option<Instant>>>,
    observer: watch::Receiver<Option<Instant>>,
}

impl CloseWatch {
    pub(crate) fn new(grace: Duration) -> Self {
        let (deadline, observer) = watch::channel(None);
        Self {
            grace,
            deadline: Arc::new(deadline),
            observer,
        }
    }

    /// Arms the budget. Called from the SDK's public `on_close` hook.
    pub(crate) fn arm(&self) {
        let _ = self.deadline.send(Some(Instant::now() + self.grace));
    }

    /// Resolves once the armed budget is exhausted; pending forever before EOF.
    async fn expired(&self) {
        let mut observer = self.observer.clone();
        loop {
            let deadline = *observer.borrow_and_update();
            if let Some(deadline) = deadline {
                tokio::time::sleep_until(deadline.into()).await;
                return;
            }
            if observer.changed().await.is_err() {
                // Dropped without arming: no budget applies to this connection.
                std::future::pending::<()>().await;
            }
        }
    }
}

/// What this transport confirmed on the wire, so an exhausted budget can name
/// the unconfirmed remainder instead of claiming a clean close.
#[derive(Default)]
struct WireTally {
    frames: usize,
    bytes: usize,
}

/// Notifications accepted by the official outgoing queue but not yet confirmed
/// on the wire, shared by the accept site and the sink of one connection.
///
/// Only `session/update` notifications are reserved and confirmed. Responses and
/// permission or host-execute requests are control frames: they are never gated,
/// so a terminal state still reaches the peer after the data bound is reached.
#[derive(Clone)]
pub(crate) struct InFlight {
    pending: Arc<AtomicUsize>,
    cap: usize,
}

impl InFlight {
    pub(crate) fn new(cap: usize) -> Self {
        Self {
            pending: Arc::new(AtomicUsize::new(0)),
            cap,
        }
    }

    /// Reserves one notification slot, refusing once `cap` slots are held.
    pub(crate) fn admit(&self) -> bool {
        if self.pending.fetch_add(1, Ordering::SeqCst) < self.cap {
            true
        } else {
            self.pending.fetch_sub(1, Ordering::SeqCst);
            false
        }
    }

    /// Drops a reservation whose frame the official queue never accepted.
    pub(crate) fn release(&self) {
        self.pending.fetch_sub(1, Ordering::SeqCst);
    }

    /// Confirms one notification written and flushed on the wire.
    fn confirm(&self) {
        self.pending.fetch_sub(1, Ordering::SeqCst);
    }

    pub(crate) fn depth(&self) -> usize {
        self.pending.load(Ordering::SeqCst)
    }
}

/// The method of an outgoing frame, borrowed so confirming a frame does not
/// allocate a copy of its payload.
#[derive(serde::Deserialize)]
struct WireMethod<'a> {
    #[serde(borrow)]
    method: Option<&'a str>,
}

fn is_notification(line: &str) -> bool {
    serde_json::from_str::<WireMethod>(line)
        .ok()
        .and_then(|frame| frame.method)
        == Some("session/update")
}

/// The outgoing half of the bounded stdio transport, over any async writer.
///
/// Separated from `bounded_stdio` so the close budget and the in-flight
/// confirmation can be exercised over a duplex instead of only over the real
/// process stdout.
pub(crate) fn outgoing_sink<W>(
    writer: W,
    close: CloseWatch,
    inflight: InFlight,
) -> impl futures::Sink<String, Error = io::Error> + Send + 'static
where
    W: tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    futures::sink::unfold(
        (writer, close, WireTally::default(), inflight),
        |(mut writer, close, mut tally, inflight), line: String| async move {
            let frame_bytes = line.len() + 1;
            let notification = is_notification(&line);
            let write = async {
                writer.write_all(line.as_bytes()).await?;
                writer.write_all(b"\n").await?;
                writer.flush().await
            };
            tokio::select! {
                result = write => result?,
                () = close.expired() => return Err(incomplete_tail(frame_bytes, &tally)),
            }
            tally.frames += 1;
            tally.bytes += frame_bytes;
            if notification {
                inflight.confirm();
            }
            Ok::<_, io::Error>((writer, close, tally, inflight))
        },
    )
}

/// Builds the bounded stdio transport for the given frame cap and close budget.
pub(crate) fn bounded_stdio(
    max_line_bytes: usize,
    close: CloseWatch,
    inflight: InFlight,
) -> Lines<
    impl futures::Sink<String, Error = io::Error> + Send + 'static,
    impl Stream<Item = io::Result<String>> + Send + 'static,
> {
    let outgoing = outgoing_sink(tokio::io::stdout(), close, inflight);
    let incoming = tokio::io::BufReader::new(tokio::io::stdin());
    Lines::new(
        Box::pin(outgoing),
        Box::pin(bounded_lines(incoming, max_line_bytes)),
    )
}

/// Yields whole lines, failing instead of growing past the cap.
///
/// A cap violation ends the stream: the official incoming actor treats a stream
/// error as a transport failure, so an oversized frame closes the connection
/// rather than being silently truncated into a valid-looking prefix.
pub(super) fn bounded_lines<R>(
    reader: R,
    max_line_bytes: usize,
) -> impl Stream<Item = io::Result<String>> + Send + 'static
where
    R: tokio::io::AsyncBufRead + Unpin + Send + 'static,
{
    futures::stream::unfold((reader, false), move |(mut reader, done)| async move {
        if done {
            return None;
        }
        let mut frame: Vec<u8> = Vec::new();
        loop {
            let available = match reader.fill_buf().await {
                Ok(bytes) => bytes,
                Err(error) => return Some((Err(error), (reader, true))),
            };
            if available.is_empty() {
                if frame.is_empty() {
                    return None;
                }
                return Some((decode(frame), (reader, true)));
            }
            if let Some(end) = available.iter().position(|byte| *byte == b'\n') {
                if frame.len().saturating_add(end) > max_line_bytes {
                    return Some((Err(over_frame(max_line_bytes)), (reader, true)));
                }
                frame.extend_from_slice(&available[..end]);
                reader.consume(end + 1);
                return Some((decode(frame), (reader, false)));
            }
            if frame.len().saturating_add(available.len()) > max_line_bytes {
                return Some((Err(over_frame(max_line_bytes)), (reader, true)));
            }
            frame.extend_from_slice(available);
            let consumed = available.len();
            reader.consume(consumed);
        }
    })
}

fn incomplete_tail(pending_bytes: usize, tally: &WireTally) -> io::Error {
    io::Error::new(
        io::ErrorKind::TimedOut,
        format!(
            "ACP close budget exhausted after stdin EOF; the outgoing tail is incomplete: \
             current frame ({pending_bytes} bytes) unconfirmed, {} frames / {} bytes \
             confirmed written",
            tally.frames, tally.bytes
        ),
    )
}

fn decode(frame: Vec<u8>) -> io::Result<String> {
    String::from_utf8(frame)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "ACP frame is not valid UTF-8"))
}

fn over_frame(max_line_bytes: usize) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("ACP frame exceeds the {max_line_bytes}-byte inbound limit"),
    )
}
