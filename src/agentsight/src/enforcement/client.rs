//! Bounded synchronous client for the local enforcer socket.

use std::io::{self, BufReader, Read, Write};
use std::mem::{offset_of, size_of_val};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use agentsight_enforcement_protocol::{
    ApplyCredentialPolicy, ApplyPolicy, Binding, Command, FrameReader, HealthStatus, ProtocolError,
    ReplaceOutcome, ReplacePolicy, Request, Response, ResponseBody, SecurityEvent, ViolationEvent,
    read_frame, write_frame,
};
use thiserror::Error;
use uuid::Uuid;

/// Client-side failures for the enforcement trust boundary.
#[derive(Debug, Error)]
pub enum EnforcementError {
    /// Connecting to or configuring the UDS failed.
    #[error("enforcer I/O failed: {0}")]
    Io(#[from] std::io::Error),
    /// A bounded protocol frame was invalid.
    #[error("enforcer protocol failed: {0}")]
    Protocol(#[from] ProtocolError),
    /// The enforcer rejected a valid request.
    #[error("enforcer rejected request ({code}): {message}")]
    Remote {
        /// Stable machine-readable code.
        code: String,
        /// Sanitized remote detail.
        message: String,
    },
    /// A response did not correlate to its request.
    #[error("enforcer response id {actual} does not match request {expected}")]
    ResponseMismatch {
        /// Original request identifier.
        expected: Uuid,
        /// Response identifier supplied by the peer.
        actual: Uuid,
    },
    /// The response payload was valid but wrong for the operation.
    #[error("unexpected enforcer response: {0}")]
    UnexpectedResponse(String),
    /// A subscription connection closed between events.
    #[error("enforcer subscription disconnected")]
    Disconnected,
}

/// Local synchronous enforcement client.
#[derive(Clone)]
pub struct EnforcementClient {
    socket_path: PathBuf,
    timeout: Duration,
}

impl EnforcementClient {
    /// Creates a client with a five-second total deadline for each ordinary request.
    pub fn new(socket_path: impl AsRef<Path>) -> Self {
        Self {
            socket_path: socket_path.as_ref().to_path_buf(),
            timeout: Duration::from_secs(5),
        }
    }

    /// Overrides the total deadline for each ordinary request.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Returns the configured socket path.
    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    /// Returns the configured total bound for one ordinary request.
    pub(crate) fn request_timeout(&self) -> Duration {
        self.timeout
    }

    /// Queries backend readiness.
    ///
    /// # Errors
    ///
    /// Returns a transport, protocol, remote, or response-shape error.
    pub fn health(&self) -> Result<HealthStatus, EnforcementError> {
        match self.call(Command::Health)? {
            ResponseBody::Health(status) => Ok(status),
            body => Err(unexpected("health", &body)),
        }
    }

    /// Applies one desired binding.
    ///
    /// # Errors
    ///
    /// Returns a transport, protocol, remote, or response-shape error.
    pub fn apply(
        &self,
        request: ApplyPolicy,
        required_subscription_id: Uuid,
    ) -> Result<Binding, EnforcementError> {
        match self.call(Command::ApplyPolicyLeased {
            request,
            required_subscription_id,
        })? {
            ResponseBody::Applied(binding) => Ok(binding),
            body => Err(unexpected("apply", &body)),
        }
    }

    /// Applies a product-level credential policy compiled by the enforcer adapter.
    ///
    /// # Errors
    ///
    /// Returns a transport, validation, compilation, or response-shape error.
    pub fn apply_credential_policy(
        &self,
        request: ApplyCredentialPolicy,
        required_subscription_id: Uuid,
    ) -> Result<Binding, EnforcementError> {
        match self.call(Command::ApplyCredentialPolicyLeased {
            request,
            required_subscription_id,
        })? {
            ResponseBody::Applied(binding) => Ok(binding),
            body => Err(unexpected("apply credential policy", &body)),
        }
    }

    /// Replaces one exact active binding under a required evidence-stream lease.
    ///
    /// # Errors
    ///
    /// Returns a transport, validation, backend, or response-shape error.
    pub fn replace(
        &self,
        request: ReplacePolicy,
        required_subscription_id: Uuid,
    ) -> Result<ReplaceOutcome, EnforcementError> {
        match self.call(Command::ReplacePolicyLeased {
            request,
            required_subscription_id,
        })? {
            ResponseBody::Replaced(outcome) => Ok(outcome),
            body => Err(unexpected("replace", &body)),
        }
    }

    /// Detaches one binding after enforcer acknowledgement.
    ///
    /// # Errors
    ///
    /// Returns a transport, protocol, remote, or response-shape error.
    pub fn detach(&self, binding_id: Uuid) -> Result<(), EnforcementError> {
        match self.call(Command::DetachAgent { binding_id })? {
            ResponseBody::Detached => Ok(()),
            body => Err(unexpected("detach", &body)),
        }
    }

    /// Lists backend bindings.
    ///
    /// # Errors
    ///
    /// Returns a transport, protocol, remote, or response-shape error.
    pub fn bindings(&self) -> Result<Vec<Binding>, EnforcementError> {
        match self.call(Command::ListBindings)? {
            ResponseBody::Bindings(bindings) => Ok(bindings),
            body => Err(unexpected("list bindings", &body)),
        }
    }

    /// Opens an independent best-effort violation observer.
    ///
    /// # Errors
    ///
    /// Returns a transport, protocol, remote, or response-shape error.
    pub fn subscribe(&self) -> Result<ViolationSubscription, EnforcementError> {
        let subscription_id = Uuid::new_v4();
        self.open_subscription(
            Command::SubscribeViolations { subscription_id },
            subscription_id,
        )
    }

    /// Opens the required evidence stream used to authorize policy applies.
    ///
    /// # Errors
    ///
    /// Returns a transport, protocol, remote, or response-shape error.
    pub fn subscribe_required(&self) -> Result<ViolationSubscription, EnforcementError> {
        let subscription_id = Uuid::new_v4();
        self.open_subscription(
            Command::SubscribeRequiredViolations { subscription_id },
            subscription_id,
        )
    }

    fn open_subscription(
        &self,
        command: Command,
        subscription_id: Uuid,
    ) -> Result<ViolationSubscription, EnforcementError> {
        let request = Request::new(command);
        let mut stream = self.connect()?;
        stream.set_read_timeout(Some(Duration::from_millis(250)))?;
        write_frame(&mut stream, &request)?;
        let mut reader = BufReader::new(stream);
        let response = read_required_response(&mut reader, request.request_id)?;
        match response {
            ResponseBody::Subscribed => Ok(ViolationSubscription {
                reader: FrameReader::new(reader),
                request_id: request.request_id,
                subscription_id,
            }),
            body => Err(unexpected("subscribe", &body)),
        }
    }

    /// Opens an independent normalized security-event stream.
    ///
    /// # Errors
    ///
    /// Returns a transport, protocol, remote, or response-shape error.
    pub fn subscribe_security_events(&self) -> Result<SecurityEventSubscription, EnforcementError> {
        let request = Request::new(Command::SubscribeSecurityEvents);
        let mut stream = self.connect()?;
        stream.set_read_timeout(Some(Duration::from_millis(250)))?;
        write_frame(&mut stream, &request)?;
        let mut reader = BufReader::new(stream);
        let response = read_required_response(&mut reader, request.request_id)?;
        match response {
            ResponseBody::Subscribed => Ok(SecurityEventSubscription {
                reader: FrameReader::new(reader),
                request_id: request.request_id,
            }),
            body => Err(unexpected("subscribe security events", &body)),
        }
    }

    fn call(&self, command: Command) -> Result<ResponseBody, EnforcementError> {
        let request = Request::new(command);
        let deadline = RequestDeadline::new(self.timeout);
        let stream = connect_with_deadline(&self.socket_path, &deadline)?;
        let mut stream = DeadlineStream { stream, deadline };
        write_frame(&mut stream, &request)?;
        read_required_response(&mut BufReader::new(stream), request.request_id)
    }

    fn connect(&self) -> Result<UnixStream, EnforcementError> {
        let stream = UnixStream::connect(&self.socket_path)?;
        stream.set_read_timeout(Some(self.timeout))?;
        stream.set_write_timeout(Some(self.timeout))?;
        Ok(stream)
    }
}

#[derive(Clone, Copy)]
struct RequestDeadline {
    started_at: Instant,
    timeout: Duration,
}

impl RequestDeadline {
    fn new(timeout: Duration) -> Self {
        Self {
            started_at: Instant::now(),
            timeout,
        }
    }

    fn remaining(self) -> io::Result<Duration> {
        let remaining = self.timeout.saturating_sub(self.started_at.elapsed());
        if remaining.is_zero() {
            return Err(request_timeout_error());
        }
        Ok(remaining)
    }

    fn poll_timeout_ms(self) -> io::Result<libc::c_int> {
        let remaining = self.remaining()?;
        let millis = remaining.as_millis().max(1).min(libc::c_int::MAX as u128);
        Ok(millis as libc::c_int)
    }
}

struct DeadlineStream {
    stream: UnixStream,
    deadline: RequestDeadline,
}

impl Read for DeadlineStream {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.stream
            .set_read_timeout(Some(self.deadline.remaining()?))?;
        self.stream.read(buffer)
    }
}

impl Write for DeadlineStream {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.stream
            .set_write_timeout(Some(self.deadline.remaining()?))?;
        self.stream.write(buffer)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.stream
            .set_write_timeout(Some(self.deadline.remaining()?))?;
        self.stream.flush()
    }
}

fn connect_with_deadline(path: &Path, deadline: &RequestDeadline) -> io::Result<UnixStream> {
    // SAFETY: The returned descriptor is immediately owned by `OwnedFd` and uses valid constants.
    let raw_fd = unsafe {
        libc::socket(
            libc::AF_UNIX,
            libc::SOCK_STREAM | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK,
            0,
        )
    };
    if raw_fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `raw_fd` is a new, valid descriptor whose ownership is transferred exactly once.
    let fd = unsafe { OwnedFd::from_raw_fd(raw_fd) };
    let (address, address_len) = unix_socket_address(path)?;
    // SAFETY: `address` is initialized for AF_UNIX and `address_len` covers its populated bytes.
    let connected = unsafe {
        libc::connect(
            fd.as_raw_fd(),
            std::ptr::from_ref(&address).cast::<libc::sockaddr>(),
            address_len,
        )
    };
    if connected != 0 {
        let error = io::Error::last_os_error();
        if !matches!(
            error.raw_os_error(),
            Some(libc::EINPROGRESS) | Some(libc::EAGAIN)
        ) {
            return Err(error);
        }
        wait_for_connect(&fd, deadline)?;
    }
    let stream = UnixStream::from(fd);
    stream.set_nonblocking(false)?;
    Ok(stream)
}

fn unix_socket_address(path: &Path) -> io::Result<(libc::sockaddr_un, libc::socklen_t)> {
    let path = path.as_os_str().as_bytes();
    // SAFETY: A zeroed sockaddr_un is valid once its family and pathname are populated below.
    let mut address = unsafe { std::mem::zeroed::<libc::sockaddr_un>() };
    if path.is_empty() || path.contains(&0) || path.len() >= address.sun_path.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "enforcer socket path is empty, contains NUL, or is too long",
        ));
    }
    address.sun_family = libc::AF_UNIX as libc::sa_family_t;
    for (target, source) in address.sun_path.iter_mut().zip(path.iter().copied()) {
        *target = source as libc::c_char;
    }
    let length = offset_of!(libc::sockaddr_un, sun_path)
        .saturating_add(path.len())
        .saturating_add(1);
    let length = libc::socklen_t::try_from(length).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "enforcer socket address length is unsupported",
        )
    })?;
    Ok((address, length))
}

fn wait_for_connect(fd: &OwnedFd, deadline: &RequestDeadline) -> io::Result<()> {
    loop {
        let mut poll_fd = libc::pollfd {
            fd: fd.as_raw_fd(),
            events: libc::POLLOUT,
            revents: 0,
        };
        // SAFETY: `poll_fd` is a valid single-element poll array for the owned descriptor.
        let ready = unsafe { libc::poll(&mut poll_fd, 1, deadline.poll_timeout_ms()?) };
        if ready == 0 {
            return Err(request_timeout_error());
        }
        if ready < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        let mut socket_error = 0;
        let mut socket_error_len = libc::socklen_t::try_from(size_of_val(&socket_error))
            .map_err(|_| io::Error::other("socket error length is unsupported"))?;
        // SAFETY: The output pointer and length describe a writable `c_int` for SO_ERROR.
        let status = unsafe {
            libc::getsockopt(
                fd.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_ERROR,
                std::ptr::from_mut(&mut socket_error).cast(),
                &mut socket_error_len,
            )
        };
        if status != 0 {
            return Err(io::Error::last_os_error());
        }
        return if socket_error == 0 {
            Ok(())
        } else {
            Err(io::Error::from_raw_os_error(socket_error))
        };
    }
}

fn request_timeout_error() -> io::Error {
    io::Error::new(
        io::ErrorKind::TimedOut,
        "enforcer request exceeded its total timeout",
    )
}

/// One long-lived violation subscription.
pub struct ViolationSubscription {
    reader: FrameReader<BufReader<UnixStream>>,
    request_id: Uuid,
    subscription_id: Uuid,
}

impl ViolationSubscription {
    /// Returns this stream's generation identity.
    pub fn subscription_id(&self) -> Uuid {
        self.subscription_id
    }

    /// Waits briefly for the next violation.
    ///
    /// `Ok(None)` means the bounded read interval elapsed, allowing callers to
    /// observe shutdown state. EOF is returned as a disconnect error.
    ///
    /// # Errors
    ///
    /// Returns a protocol, remote, disconnect, or response-shape error.
    pub fn next_event(&mut self) -> Result<Option<ViolationEvent>, EnforcementError> {
        let response: Response = match self.reader.read_frame() {
            Ok(Some(response)) => response,
            Ok(None) => return Err(EnforcementError::Disconnected),
            Err(ProtocolError::Io(error))
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                return Ok(None);
            }
            Err(error) => return Err(error.into()),
        };
        let body = response_body(response, self.request_id)?;
        match body {
            ResponseBody::Violation(event) => Ok(Some(event)),
            body => Err(unexpected("violation subscription", &body)),
        }
    }
}

/// One long-lived normalized security-event subscription.
pub struct SecurityEventSubscription {
    reader: FrameReader<BufReader<UnixStream>>,
    request_id: Uuid,
}

impl SecurityEventSubscription {
    /// Waits briefly for the next normalized security event.
    ///
    /// `Ok(None)` means the bounded read interval elapsed. EOF is returned as a
    /// disconnect error so callers can reconnect without losing desired state.
    ///
    /// # Errors
    ///
    /// Returns a protocol, remote, disconnect, or response-shape error.
    pub fn next_event(&mut self) -> Result<Option<SecurityEvent>, EnforcementError> {
        let response: Response = match self.reader.read_frame() {
            Ok(Some(response)) => response,
            Ok(None) => return Err(EnforcementError::Disconnected),
            Err(ProtocolError::Io(error))
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                return Ok(None);
            }
            Err(error) => return Err(error.into()),
        };
        let body = response_body(response, self.request_id)?;
        match body {
            ResponseBody::SecurityEvent(event) => Ok(Some(event)),
            body => Err(unexpected("security event subscription", &body)),
        }
    }
}

fn read_required_response<R: Read>(
    reader: &mut BufReader<R>,
    request_id: Uuid,
) -> Result<ResponseBody, EnforcementError> {
    let response: Response = read_frame(reader)?.ok_or(EnforcementError::Disconnected)?;
    response_body(response, request_id)
}

fn response_body(response: Response, request_id: Uuid) -> Result<ResponseBody, EnforcementError> {
    if response.request_id != request_id {
        return Err(EnforcementError::ResponseMismatch {
            expected: request_id,
            actual: response.request_id,
        });
    }
    response.result.map_err(|error| EnforcementError::Remote {
        code: error.code,
        message: error.message,
    })
}

fn unexpected(operation: &str, body: &ResponseBody) -> EnforcementError {
    EnforcementError::UnexpectedResponse(format!("{operation} returned {body:?}"))
}

#[cfg(test)]
mod tests {
    use std::io::{BufReader, Write};
    use std::os::unix::net::UnixListener;
    use std::thread;

    use agentsight_enforcement_protocol::{PROTOCOL_VERSION, Request, Response};

    use super::*;

    #[test]
    fn ordinary_request_timeout_is_total_across_partial_reads() {
        let socket_path =
            std::env::temp_dir().join(format!("enforcement-timeout-{}.sock", Uuid::new_v4()));
        let listener = UnixListener::bind(&socket_path).expect("fixture listener should bind");
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("fixture client should connect");
            let request: Request = read_frame(&mut BufReader::new(
                stream
                    .try_clone()
                    .expect("fixture stream should clone for reading"),
            ))
            .expect("fixture request should decode")
            .expect("fixture request should exist");
            let response = Response {
                protocol_version: PROTOCOL_VERSION,
                request_id: request.request_id,
                result: Ok(ResponseBody::Health(HealthStatus {
                    ready: true,
                    backend: "fixture".into(),
                    capabilities:
                        agentsight_enforcement_protocol::EnforcementCapabilities::mock_development(),
                    message: None,
                })),
            };
            let mut bytes = Vec::new();
            write_frame(&mut bytes, &response).expect("fixture response should encode");
            let split = bytes.len() / 2;
            thread::sleep(Duration::from_millis(200));
            stream
                .write_all(&bytes[..split])
                .expect("first response segment should write");
            stream.flush().expect("first response segment should flush");
            thread::sleep(Duration::from_millis(200));
            let _ = stream.write_all(&bytes[split..]);
        });

        let result = EnforcementClient::new(&socket_path)
            .with_timeout(Duration::from_millis(300))
            .health();
        server.join().expect("fixture server should join");
        std::fs::remove_file(&socket_path).expect("fixture socket should be removed");

        let error = result.expect_err("split response must not reset the total request deadline");
        assert!(matches!(
            error,
            EnforcementError::Protocol(ProtocolError::Io(error))
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                )
        ));
    }
}
