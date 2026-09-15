use std::io::Write;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

/// Kernel-reported identity of the process connected to the Unix socket.
///
/// This is transport evidence, not an authorization role. A later daemon
/// adapter binds it to server-owned authentication and authorization policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PeerCredentials {
    uid: u32,
    gid: u32,
    pid: u32,
}

impl PeerCredentials {
    pub(crate) const fn new(uid: u32, gid: u32, pid: u32) -> Self {
        Self { uid, gid, pid }
    }

    /// Returns the kernel-reported effective user ID.
    pub const fn uid(self) -> u32 {
        self.uid
    }

    /// Returns the kernel-reported effective group ID.
    pub const fn gid(self) -> u32 {
        self.gid
    }

    /// Returns the kernel-reported process ID.
    pub const fn pid(self) -> u32 {
        self.pid
    }
}

/// Cooperative lifetime control for one application dispatch.
///
/// The service requests cancellation when the dispatch deadline expires, the
/// connection task is aborted during shutdown, or the request otherwise leaves
/// the service-owned dispatch lifetime. Blocking application code must check
/// this signal around interruptible work; Rust cannot forcibly stop a running
/// blocking thread safely.
#[derive(Debug, Clone)]
pub struct DispatchControl {
    cancelled: Arc<AtomicBool>,
    deadline: Instant,
}

impl DispatchControl {
    pub fn new(deadline: Instant) -> Self {
        Self {
            cancelled: Arc::new(AtomicBool::new(false)),
            deadline,
        }
    }

    pub(crate) fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    /// Returns whether the service has ended this request's dispatch lifetime.
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    /// Returns the transport-owned hard dispatch deadline.
    pub const fn deadline(&self) -> Instant {
        self.deadline
    }
}

/// One bounded request frame and its trusted transport peer.
#[derive(Debug, Clone)]
pub struct DispatchRequest {
    /// Kernel-reported peer credentials captured before request decoding.
    pub peer: PeerCredentials,
    /// Request payload without the optional LF wire delimiter.
    pub payload: Vec<u8>,
    /// Deadline and cooperative cancellation signal for application dispatch.
    pub control: DispatchControl,
}

/// A connection rejected before normal protocol dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RejectedRequest {
    /// Kernel-reported peer credentials when credential extraction succeeded.
    pub peer: PeerCredentials,
    /// Transport-owned reason that the request could not be dispatched.
    pub reason: RejectionReason,
}

/// Protocol-independent reason for rejecting a connection or response.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RejectionReason {
    /// The bounded normal-connection admission limit was reached.
    Busy,
    /// Shutdown began before the accepted connection could be admitted.
    ShuttingDown,
    /// The first request frame did not complete within the configured deadline.
    RequestReadTimeout,
    /// The first request frame exceeded its configured wire limit.
    RequestFrameTooLarge,
    /// EOF arrived without any request bytes.
    EmptyRequest,
    /// Dispatch failed without a safe protocol response.
    DispatchFailed,
    /// Application dispatch exceeded its service-owned hard deadline.
    DispatchTimedOut,
    /// The encoded response exceeded its configured wire limit.
    ResponseFrameTooLarge,
    /// The dispatcher attempted to emit an invalid framed response.
    InvalidResponseFrame,
}

/// Whether a dispatcher-produced frame should be sent or the connection closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResponseDisposition {
    /// Send the encoded response followed by one LF delimiter.
    Send,
    /// Close the connection without sending a response.
    Close,
}

/// Safe, non-diagnostic failure at the dispatcher boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("request dispatcher failed")]
pub struct DispatchError;

/// Protocol adapter injected into the UDS service framework.
///
/// Implementations decode request bytes, generate protocol request identities,
/// bind the peer to a trusted principal, invoke one application use case, and
/// encode its response. Implementations must not retain `response`; the service
/// owns and bounds that writer. Transport rejection encoding is a separate
/// [`RejectionEncoder`] dependency.
pub trait RequestDispatcher: Send + Sync + 'static {
    /// Selects a method-specific execution budget after a complete bounded frame arrives.
    /// The transport caps overrides at 120 seconds. Default adapters keep configured limits.
    /// Implementations must perform bounded parsing only, never I/O or capability work.
    fn dispatch_timeout(&self, _payload: &[u8]) -> Option<std::time::Duration> {
        None
    }

    /// Dispatches one complete bounded request frame.
    ///
    /// # Errors
    /// Returns `DispatchError` when no safe response was encoded.
    fn dispatch(
        &self,
        request: DispatchRequest,
        response: &mut dyn Write,
    ) -> Result<ResponseDisposition, DispatchError>;
}

/// Protocol-only encoder for failures detected before or around application dispatch.
///
/// This is deliberately separate from [`RequestDispatcher`], so overload,
/// timeout, and shutdown responses do not require an application/PAP dependency.
/// Implementations should contain only request-ID generation and wire error
/// projection; the service also bounds their execution time.
pub trait RejectionEncoder: Send + Sync + 'static {
    /// Encodes a safe response for one transport-owned rejection.
    ///
    /// # Errors
    /// Returns `DispatchError` when the connection must be closed silently.
    fn encode_rejection(
        &self,
        request: RejectedRequest,
        response: &mut dyn Write,
    ) -> Result<ResponseDisposition, DispatchError>;
}
