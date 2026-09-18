//! One bounded daemon call per UDS connection, with no retries or local execution.

use std::io::{self, Read as _, Write as _};
use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::{Duration, Instant};

use asc_daemon_protocol::{DaemonRequest, DaemonResponse};
use socket2::{Domain, SockAddr, Socket, Type};

/// LF-inclusive wire limit, matching the current daemon bootstrap defaults.
pub const MAX_FRAME_BYTES: usize = 4 * 1024 * 1024;

/// Sends one request and preserves the complete protocol response.
///
/// The deadline covers connect, write, and read together. A complete LF frame
/// returns immediately; EOF also terminates a nonempty frame. This function
/// never retries, including after failures whose execution outcome is unknown.
/// The caller's thread blocks until completion; no async runtime is needed.
/// Encoding precedes the I/O deadline, and decoding follows receipt of the frame.
///
/// # Errors
/// Returns local encoding/limit failures, connection failures, a call deadline,
/// I/O failures, or malformed/oversized responses. Daemon errors remain responses.
pub fn call(
    socket: &Path,
    request: &DaemonRequest,
    timeout: Duration,
) -> Result<DaemonResponse, ClientError> {
    if timeout.is_zero() || Instant::now().checked_add(timeout).is_none() {
        return Err(ClientError::InvalidTimeout);
    }
    let mut payload = serde_json::to_vec(request).map_err(ClientError::Encode)?;
    payload.push(b'\n');
    if payload.len() > MAX_FRAME_BYTES {
        return Err(ClientError::RequestTooLarge);
    }

    let deadline = Instant::now()
        .checked_add(timeout)
        .ok_or(ClientError::InvalidTimeout)?;
    let address = SockAddr::unix(socket).map_err(ClientError::Connect)?;
    let socket = Socket::new(Domain::UNIX, Type::STREAM, None).map_err(ClientError::Connect)?;
    // std's UnixStream has no bounded connect; socket2 uses nonblocking connect
    // and an OS readiness wait without a runtime or helper thread.
    socket
        .connect_timeout(&address, remaining(deadline, false)?)
        .map_err(|error| transport_error(error, false))?;
    // Linux can return EAGAIN for a full UDS listen queue without starting a
    // connection. Writable readiness alone does not prove that it connected.
    socket.peer_addr().map_err(ClientError::Connect)?;
    let mut stream = UnixStream::from(OwnedFd::from(socket));
    let mut write_started = false;
    let mut pending = payload.as_slice();
    while !pending.is_empty() {
        stream
            .set_write_timeout(Some(remaining(deadline, write_started)?))
            .map_err(|error| transport_error(error, write_started))?;
        // Even a failed write may have delivered enough bytes for execution.
        write_started = true;
        match stream.write(pending) {
            Ok(0) => return Err(ClientError::Io(io::ErrorKind::WriteZero.into())),
            Ok(count) => pending = &pending[count..],
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(transport_error(error, true)),
        }
    }
    read_response(&mut stream, deadline)
}

fn remaining(deadline: Instant, request_may_have_executed: bool) -> Result<Duration, ClientError> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|duration| !duration.is_zero())
        .ok_or(ClientError::Timeout {
            request_may_have_executed,
        })
}

fn transport_error(error: io::Error, request_may_have_executed: bool) -> ClientError {
    // Blocking socket timeouts are reported as WouldBlock on Linux and may be
    // TimedOut on other platforms. Neither permits replaying a sent request.
    if matches!(
        error.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
    ) {
        ClientError::Timeout {
            request_may_have_executed,
        }
    } else if request_may_have_executed {
        ClientError::Io(error)
    } else {
        ClientError::Connect(error)
    }
}

fn read_response(
    stream: &mut UnixStream,
    deadline: Instant,
) -> Result<DaemonResponse, ClientError> {
    let mut frame = Vec::new();
    let mut chunk = [0_u8; 8192];
    // A fixed SO_RCVTIMEO restarts for every read, letting a partial response
    // consume the original timeout again. Reapplying it per read is not portable:
    // macOS rejects that setsockopt after a peer closes an EOF-terminated frame.
    // Nonblocking reads plus a short bounded wait enforce one deadline on every
    // platform without touching socket options after the response begins.
    stream.set_nonblocking(true).map_err(ClientError::Io)?;
    loop {
        let count = match stream.read(&mut chunk) {
            Ok(count) => count,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                let wait = remaining(deadline, true)?.min(Duration::from_millis(1));
                std::thread::sleep(wait);
                continue;
            }
            Err(error) => return Err(transport_error(error, true)),
        };
        if count == 0 {
            break;
        }
        let newline = chunk[..count].iter().position(|byte| *byte == b'\n');
        let end = newline.map_or(count, |index| index + 1);
        if frame.len() + end > MAX_FRAME_BYTES {
            return Err(ClientError::ResponseTooLarge);
        }
        frame.extend_from_slice(&chunk[..end]);
        if newline.is_some() {
            break;
        }
        // The socket timeout bounds one read; this bounds their sum so a slow
        // trickle of chunks cannot outlast the shared deadline.
        remaining(deadline, true)?;
    }
    if frame.is_empty() {
        return Err(ClientError::EmptyResponse);
    }
    serde_json::from_slice(&frame).map_err(ClientError::Decode)
}

/// Client failures are separate from structured daemon method errors.
#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    /// Invalid caller configuration; no request was sent.
    #[error("timeout must be positive and representable")]
    InvalidTimeout,
    /// Local request encoding failed before connecting.
    #[error("request encoding failed: {0}")]
    Encode(serde_json::Error),
    /// Local wire limit exceeded before connecting.
    #[error("request exceeds the 4194304-byte frame limit; not sent")]
    RequestTooLarge,
    /// No connection was established.
    #[error("daemon connection unavailable; request not sent: {0}")]
    Connect(io::Error),
    /// A single call deadline elapsed, without replaying the request.
    #[error(
        "daemon call timed out (request may have executed: {request_may_have_executed}); not retried"
    )]
    Timeout {
        /// True once any request write has been attempted.
        request_may_have_executed: bool,
    },
    /// Write/read failures do not prove the operation failed to execute.
    #[error("daemon I/O failed; request may have executed; not retried: {0}")]
    Io(io::Error),
    /// No valid response means the execution outcome is unknown.
    #[error("daemon returned an empty response; request may have executed; not retried")]
    EmptyResponse,
    /// No valid response means the execution outcome is unknown.
    #[error(
        "daemon response exceeds the 4194304-byte frame limit; request may have executed; not retried"
    )]
    ResponseTooLarge,
    /// No valid response means the execution outcome is unknown.
    #[error("invalid daemon response; request may have executed; not retried: {0}")]
    Decode(serde_json::Error),
}
