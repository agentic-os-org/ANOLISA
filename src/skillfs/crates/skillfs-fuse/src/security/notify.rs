//! N2 + N3: Notify Change Client and Protocol Event Log.
//!
//! Sends `skill_ledger.skillfs_notify_change` notifications to an external
//! daemon over a Unix domain socket. The notification tells the daemon that
//! a skill's source workspace may have changed; the daemon owns scan,
//! reconcile, and activation refresh.
//!
//! N3 optionally injects a [`super::protocol_events::ProtocolEventWriter`]
//! into the controller. When present, each debounced notification also
//! writes an append-only JSONL protocol event before the socket send, so
//! the local log is written even when the daemon is unreachable.
//!
//! Failure semantics: notify failure and protocol event write failure are
//! both diagnostic only. Neither changes the in-memory
//! [`super::ActiveSkillResolver`] mapping. The existing trusted view stays
//! in place until the daemon writes a new `activation.json` / xattr.
//!
//! Wire format follows §4 of `SKILL_LEDGER_SKILLFS_INTEGRATION_zh.md`
//! (SkillFS Notify v2 contract).

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Read as IoRead, Write as IoWrite};
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde::Serialize;
use tokio::runtime::{Builder, Handle};
use tokio::sync::{Notify as TokioNotify, mpsc};
use tracing::{debug, info, warn};

use super::activation_reload::ActivationReloadController;
use super::activation_watcher::WatcherRegistrar;
#[cfg(test)]
use super::auth::authenticate_server;
use super::auth::{
    FrameSender, NOTIFY_CLIENT_DOMAIN, NOTIFY_SERVER_DOMAIN, SharedSecret, authenticate_client,
};
use super::lifecycle::is_reserved_lifecycle_name;
use super::path::is_skill_meta_path;
use super::protocol_events::{NoopProtocolEventWriter, ProtocolEvent, ProtocolEventWriter};
use super::refresh::MutationKind;

/// Daemon method used for SkillFS change notifications.
pub const NOTIFY_METHOD: &str = "skill_ledger.skillfs_notify_change";
/// Notify request and response schema version accepted by SkillFS.
pub const NOTIFY_SCHEMA_VERSION: u64 = 2;
pub const DEFAULT_NOTIFY_TIMEOUT_MS: u64 = 5000;
pub const DEFAULT_NOTIFY_DEBOUNCE_MS: u64 = 300;
/// Maximum number of relative paths per notification. Exceeding this sends
/// `paths: []` to signal "whole skill may have changed".
pub const MAX_NOTIFY_PATHS: usize = 64;

/// Maximum response body size (bytes) accepted from the daemon.
/// Responses exceeding this limit are rejected as `InvalidResponse`
/// to prevent unbounded memory allocation on a malicious/buggy peer.
const MAX_RESPONSE_BYTES: u64 = 64 * 1024;

/// Delay before the first retry of a transiently failed reconcile.
pub const RECONCILE_RETRY_BASE_MS: u64 = 250;
/// Ceiling on the reconcile retry interval. Transient retries continue at
/// this interval for as long as the daemon stays unreachable, so a mount
/// that starts before the daemon still converges without operator action.
pub const RECONCILE_RETRY_MAX_MS: u64 = 30_000;
/// Attempt limit for [`NotifyRetryClass::Ambiguous`] failures, where the
/// wire cannot distinguish a daemon restart from a refused authentication
/// proof. Eight attempts span roughly 32 seconds of backoff, covering slower
/// container recovery while ensuring that a permanently wrong key still has
/// a fixed, endpoint-wide cost instead of causing a retry storm.
pub const RECONCILE_AMBIGUOUS_RETRY_LIMIT: u32 = 8;
/// Jitter amplitude as a percentage of the un-jittered interval. Spreads
/// retries from many concurrently converging skills across the window
/// instead of aligning them on one instant.
const RECONCILE_RETRY_JITTER_PCT: u64 = 25;
/// Exponent cap. `RECONCILE_RETRY_BASE_MS << 8` already exceeds
/// `RECONCILE_RETRY_MAX_MS`; the cap only keeps the shift well-defined.
const RECONCILE_RETRY_MAX_SHIFT: u32 = 16;

static REQUEST_COUNTER: AtomicU64 = AtomicU64::new(1);

fn next_request_id() -> String {
    let seq = REQUEST_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("skillfs-{seq}")
}

// ---------------------------------------------------------------------------
// NotifyEventKind
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NotifyEventKind {
    Mkdir,
    Create,
    Write,
    Rename,
    Unlink,
    Rmdir,
    Truncate,
    Reconcile,
    Unknown,
}

impl NotifyEventKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Mkdir => "mkdir",
            Self::Create => "create",
            Self::Write => "write",
            Self::Rename => "rename",
            Self::Unlink => "unlink",
            Self::Rmdir => "rmdir",
            Self::Truncate => "truncate",
            Self::Reconcile => "reconcile",
            Self::Unknown => "unknown",
        }
    }

    pub fn from_mutation_kind(kind: MutationKind) -> Self {
        match kind {
            MutationKind::Mkdir => Self::Mkdir,
            MutationKind::Create => Self::Create,
            MutationKind::Write => Self::Write,
            MutationKind::Rename => Self::Rename,
            MutationKind::Unlink => Self::Unlink,
            MutationKind::Rmdir => Self::Rmdir,
            MutationKind::SetattrTruncate => Self::Truncate,
        }
    }
}

// ---------------------------------------------------------------------------
// Wire format
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct NotifyChangeEvent {
    pub id: String,
    pub method: &'static str,
    pub params: NotifyParams,
    pub trace_context: serde_json::Value,
    pub timeout_ms: u64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
/// Versioned notify parameters sent inside the daemon request envelope.
pub struct NotifyParams {
    /// Protocol version; only v2 is supported.
    pub schema_version: u64,
    /// User-visible absolute Skill directory addressed by the ledger.
    pub canonical_skill_dir: String,
    /// Complete Skill id relative to the canonical root.
    pub skill_id: String,
    /// Last mutation kind observed in the debounce window.
    pub event_kind: String,
    /// Sorted changed paths relative to the canonical Skill directory.
    pub paths: Vec<String>,
}

impl NotifyChangeEvent {
    pub fn new(
        canonical_skill_dir: impl Into<String>,
        skill_id: impl Into<String>,
        event_kind: NotifyEventKind,
        paths: Vec<String>,
        timeout_ms: u64,
    ) -> Self {
        Self {
            id: next_request_id(),
            method: NOTIFY_METHOD,
            params: NotifyParams {
                schema_version: NOTIFY_SCHEMA_VERSION,
                canonical_skill_dir: canonical_skill_dir.into(),
                skill_id: skill_id.into(),
                event_kind: event_kind.as_str().to_string(),
                paths,
            },
            trace_context: serde_json::json!({}),
            timeout_ms,
        }
    }
}

// ---------------------------------------------------------------------------
// Error
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub enum NotifyError {
    Connect(std::io::Error),
    Write(std::io::Error),
    Read(std::io::Error),
    Timeout,
    InvalidResponse {
        body: String,
    },
    Rejected {
        body: String,
    },
    /// The endpoint is not usable *yet*: the parent directory or the socket
    /// file does not exist, or the socket is being recreated. This is the
    /// SkillFS-started-before-the-daemon case and is retryable.
    EndpointUnavailable(String),
    /// The endpoint exists but its ownership, mode, or file type is unsafe.
    /// A misconfiguration or an active spoofing attempt; retrying cannot
    /// turn an untrusted endpoint into a trusted one.
    EndpointUntrusted(String),
    /// Authentication or framing transport failure: handshake I/O error or
    /// handshake timeout. Retryable.
    AuthTransport(String),
    /// The handshake ended without a verdict — the peer closed the
    /// connection or sent an unparseable frame while SkillFS was waiting
    /// for `auth.ok`.
    ///
    /// This is deliberately its own variant because the wire cannot
    /// distinguish the two causes: the daemon closes the connection *both*
    /// when it is restarting mid-handshake *and* when it has refused the
    /// client proof. See [`NotifyRetryClass::Ambiguous`].
    AuthInconclusive(String),
    /// Authentication was answered and refused: HMAC proof mismatch,
    /// wrong key material, or an oversized frame. Retrying with the same
    /// key would only produce the same refusal.
    AuthRejected(String),
}

impl std::fmt::Display for NotifyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Connect(e) => write!(f, "notify: connect failed: {e}"),
            Self::Write(e) => write!(f, "notify: write failed: {e}"),
            Self::Read(e) => write!(f, "notify: read failed: {e}"),
            Self::Timeout => write!(f, "notify: timeout"),
            Self::InvalidResponse { body } => {
                write!(f, "notify: invalid response: {body}")
            }
            Self::Rejected { body } => {
                write!(f, "notify: rejected: {body}")
            }
            Self::EndpointUnavailable(message) => {
                write!(f, "notify: endpoint unavailable: {message}")
            }
            Self::EndpointUntrusted(message) => {
                write!(f, "notify: endpoint is not trusted: {message}")
            }
            Self::AuthTransport(message) => {
                write!(f, "notify: authentication transport failed: {message}")
            }
            Self::AuthInconclusive(message) => {
                write!(f, "notify: authentication inconclusive: {message}")
            }
            Self::AuthRejected(message) => {
                write!(f, "notify: authentication rejected: {message}")
            }
        }
    }
}

impl std::error::Error for NotifyError {}

/// Whether a failed notify delivery is worth attempting again.
///
/// Only reconcile deliveries act on this: ordinary FUSE mutations keep
/// their best-effort, single-attempt semantics (see
/// [`NotifyController::observe`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotifyRetryClass {
    /// The peer or the endpoint may become healthy on its own — retry with
    /// backoff until it does.
    Transient,
    /// The failure cannot be told apart from a permanent one on the wire.
    /// Retry through the controller-wide endpoint gate, but allow only
    /// [`RECONCILE_AMBIGUOUS_RETRY_LIMIT`] probes across all pending Skills.
    ///
    /// The motivating case is a wrong notify authentication key: the daemon
    /// refuses the client proof by closing the connection, which is
    /// byte-for-byte what a daemon restarting mid-handshake looks like.
    /// Treating it as `Transient` would retry a wrong key forever; treating
    /// it as `Permanent` would silently drop a reconcile lost to a genuine
    /// restart. A bounded retry converges in the restart case and turns the
    /// wrong-key case into a handful of attempts instead of a storm.
    Ambiguous,
    /// The failure is a configuration, trust, or protocol mismatch — an
    /// identical retry produces an identical failure.
    Permanent,
}

impl NotifyError {
    /// Classify this failure for the reconcile retry loop.
    pub fn retry_class(&self) -> NotifyRetryClass {
        match self {
            Self::Connect(e) | Self::Write(e) | Self::Read(e) => io_retry_class(e.kind()),
            Self::Timeout | Self::EndpointUnavailable(_) | Self::AuthTransport(_) => {
                NotifyRetryClass::Transient
            }
            Self::AuthInconclusive(_) => NotifyRetryClass::Ambiguous,
            Self::InvalidResponse { .. }
            | Self::Rejected { .. }
            | Self::EndpointUntrusted(_)
            | Self::AuthRejected(_) => NotifyRetryClass::Permanent,
        }
    }

    /// Whether this failure is retried at all — either indefinitely
    /// ([`NotifyRetryClass::Transient`]) or through the controller-wide,
    /// bounded endpoint gate ([`NotifyRetryClass::Ambiguous`]).
    pub fn is_retryable(&self) -> bool {
        self.retry_class() != NotifyRetryClass::Permanent
    }

    /// Whether this failure is retried without an attempt limit.
    pub fn is_transient(&self) -> bool {
        self.retry_class() == NotifyRetryClass::Transient
    }

    /// Whether the failure proves the business request was never sent.
    ///
    /// Authentication and response errors are deliberately excluded: the
    /// public error variant does not always identify which protocol phase
    /// failed, so the activation watcher remains the conservative fallback.
    fn business_request_not_sent(&self) -> bool {
        matches!(
            self,
            Self::Connect(_) | Self::EndpointUnavailable(_) | Self::EndpointUntrusted(_)
        )
    }
}

/// Classify a raw I/O failure kind.
///
/// The transient set is exactly the socket lifecycle: the daemon has not
/// bound the socket yet (`NotFound`, `ConnectionRefused`), the daemon
/// restarted mid-exchange (`ConnectionReset`, `ConnectionAborted`,
/// `BrokenPipe`, `NotConnected`, `UnexpectedEof`), or the transfer hit a
/// timeout (`TimedOut`, `WouldBlock`, `Interrupted`).
///
/// Everything else — including `PermissionDenied` on the socket and the
/// `Other` kind used to wrap serialization failures — is permanent. An
/// unrecognized kind is treated as permanent so an unknown failure mode
/// cannot turn into an unbounded retry loop.
fn io_retry_class(kind: std::io::ErrorKind) -> NotifyRetryClass {
    use std::io::ErrorKind as Kind;
    match kind {
        Kind::NotFound
        | Kind::ConnectionRefused
        | Kind::ConnectionReset
        | Kind::ConnectionAborted
        | Kind::NotConnected
        | Kind::BrokenPipe
        | Kind::UnexpectedEof
        | Kind::AddrNotAvailable
        | Kind::TimedOut
        | Kind::WouldBlock
        | Kind::Interrupted => NotifyRetryClass::Transient,
        _ => NotifyRetryClass::Permanent,
    }
}

/// Map an authentication/framing failure onto a typed [`NotifyError`].
///
/// The mapping is by `AuthError` variant, not by rendered message, so the
/// retry loop never has to pattern-match on strings.
fn auth_error_to_notify(error: super::auth::AuthError) -> NotifyError {
    use super::auth::AuthError as Auth;
    let message = error.to_string();
    match error {
        // Transport-level: the daemon may simply be restarting.
        Auth::Io(io) => match io_retry_class(io.kind()) {
            NotifyRetryClass::Permanent => NotifyError::AuthRejected(message),
            _ => NotifyError::AuthTransport(message),
        },
        Auth::Timeout => NotifyError::AuthTransport(message),
        // `InvalidFrame` is what the client observes when the peer closes
        // the connection before answering — `read_frame` maps `Ok(0)` to it.
        // The daemon does exactly that both when it is restarting and when
        // it has rejected our client proof, so a wrong key surfaces here
        // rather than as `VerificationFailed`. Bounded retry is the only
        // safe reading of an ambiguous signal.
        Auth::InvalidFrame => NotifyError::AuthInconclusive(message),
        // Entropy is an environment condition, not a trust decision; the
        // capped retry interval bounds the cost of retrying.
        Auth::Entropy(_) => NotifyError::AuthTransport(message),
        // Proof mismatch, unusable key material, or a protocol violation.
        Auth::VerificationFailed
        | Auth::FrameTooLarge(_)
        | Auth::RelativePath
        | Auth::Open(_)
        | Auth::NotRegular
        | Auth::InsecurePermissions
        | Auth::WrongOwner
        | Auth::InvalidLength => NotifyError::AuthRejected(message),
    }
}

// ---------------------------------------------------------------------------
// Client trait + implementations
// ---------------------------------------------------------------------------

pub trait NotifyClient: Send + Sync {
    fn send(&self, event: &NotifyChangeEvent) -> Result<(), NotifyError>;
}

/// Production client that sends one NDJSON request frame per Unix socket
/// connection. Each call opens a new connection (matching the
/// single-connection-per-request protocol).
pub struct UnixSocketNotifyClient {
    socket_path: PathBuf,
    timeout: Duration,
    auth_secret: Option<SharedSecret>,
}

impl UnixSocketNotifyClient {
    pub fn new(socket_path: impl Into<PathBuf>, timeout: Duration) -> Self {
        Self {
            socket_path: socket_path.into(),
            timeout,
            auth_secret: None,
        }
    }

    /// Creates a notify client that mutually authenticates before sending
    /// the unchanged notify v2 business frame.
    pub fn new_authenticated(
        socket_path: impl Into<PathBuf>,
        timeout: Duration,
        key_file: &Path,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        Ok(Self {
            socket_path: socket_path.into(),
            timeout,
            auth_secret: Some(SharedSecret::load(key_file)?),
        })
    }
}

impl NotifyClient for UnixSocketNotifyClient {
    fn send(&self, event: &NotifyChangeEvent) -> Result<(), NotifyError> {
        let expected_peer = if self.auth_secret.is_some() {
            Some(validate_authenticated_notify_endpoint(&self.socket_path)?)
        } else {
            None
        };
        let mut stream = UnixStream::connect(&self.socket_path).map_err(NotifyError::Connect)?;
        if let Some(expected_uid) = expected_peer {
            let peer = super::control_socket::get_peer_credentials(&stream).map_err(|error| {
                endpoint_untrusted_error(format!("cannot verify daemon peer: {error}"))
            })?;
            if peer.uid != expected_uid {
                return Err(endpoint_untrusted_error(
                    "daemon kernel UID differs from socket owner",
                ));
            }
        }
        stream
            .set_write_timeout(Some(self.timeout))
            .map_err(NotifyError::Write)?;
        stream
            .set_read_timeout(Some(self.timeout))
            .map_err(NotifyError::Read)?;

        let authenticated_session = if let Some(secret) = &self.auth_secret {
            Some(
                authenticate_client(
                    &mut stream,
                    secret,
                    NOTIFY_CLIENT_DOMAIN,
                    NOTIFY_SERVER_DOMAIN,
                )
                .map_err(auth_error_to_notify)?,
            )
        } else {
            None
        };

        let request = serde_json::to_vec(event)
            .map_err(|e| NotifyError::Write(std::io::Error::new(std::io::ErrorKind::Other, e)))?;
        if let Some(session) = &authenticated_session {
            session
                .write_frame(&mut stream, FrameSender::Client, &request)
                .map_err(auth_error_to_notify)?;
        } else {
            let mut writer = std::io::BufWriter::new(&stream);
            writer.write_all(&request).map_err(NotifyError::Write)?;
            writer.write_all(b"\n").map_err(NotifyError::Write)?;
            writer.flush().map_err(NotifyError::Write)?;
        }

        let line = if let Some(session) = &authenticated_session {
            let response = session
                .read_frame(
                    &mut stream,
                    FrameSender::Server,
                    MAX_RESPONSE_BYTES as usize,
                )
                .map_err(auth_error_to_notify)?;
            String::from_utf8(response).map_err(|error| NotifyError::InvalidResponse {
                body: format!("response is not UTF-8: {error}"),
            })?
        } else {
            let reader = BufReader::new(&stream);
            let mut limited = reader.take(MAX_RESPONSE_BYTES + 1);
            let mut line = String::new();
            match limited.read_line(&mut line) {
                Ok(0) => {
                    return Err(NotifyError::Read(std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        "notify acknowledgement ended before any response",
                    )));
                }
                Ok(n) if n as u64 > MAX_RESPONSE_BYTES => {
                    return Err(NotifyError::InvalidResponse {
                        body: format!("response exceeds {MAX_RESPONSE_BYTES} byte limit"),
                    });
                }
                Ok(_) if !line.ends_with('\n') => {
                    return Err(NotifyError::Read(std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        "notify acknowledgement ended before newline",
                    )));
                }
                Ok(_) => {}
                Err(e)
                    if e.kind() == std::io::ErrorKind::TimedOut
                        || e.kind() == std::io::ErrorKind::WouldBlock =>
                {
                    return Err(NotifyError::Timeout);
                }
                Err(e) => return Err(NotifyError::Read(e)),
            }
            line
        };

        validate_response(&line)
    }
}

fn validate_authenticated_notify_endpoint(socket_path: &Path) -> Result<u32, NotifyError> {
    let effective_uid = unsafe { libc::geteuid() };
    let raw = socket_path
        .to_str()
        .ok_or_else(|| endpoint_untrusted_error("socket path must be UTF-8"))?;
    if !socket_path.is_absolute()
        || raw.contains('\0')
        || raw.split('/').any(|p| p == "." || p == "..")
        || raw.contains("//")
    {
        return Err(endpoint_untrusted_error(
            "socket path must be absolute and normalized",
        ));
    }
    let parent = socket_path
        .parent()
        .ok_or_else(|| endpoint_untrusted_error("socket path has no parent directory"))?;
    // Validate ancestors as well as the endpoint. Root-owned sticky /tmp is allowed;
    // HMAC and the connected kernel peer remain mandatory after this path inspection.
    for ancestor in parent.ancestors() {
        let metadata = std::fs::symlink_metadata(ancestor).map_err(|error| {
            endpoint_stat_error(
                format!("cannot inspect parent '{}': {error}", ancestor.display()),
                &error,
            )
        })?;
        if !metadata.is_dir()
            || ![0, effective_uid].contains(&metadata.uid())
            || (metadata.mode() & 0o022 != 0
                && !(metadata.uid() == 0 && metadata.mode() & 0o1000 != 0))
        {
            return Err(endpoint_untrusted_error(
                "parent has unsafe ownership, type or group or other permissions",
            ));
        }
    }
    let parent_metadata = std::fs::symlink_metadata(parent)
        .map_err(|error| endpoint_stat_error(format!("cannot inspect parent: {error}"), &error))?;
    let socket_metadata = std::fs::symlink_metadata(socket_path)
        .map_err(|error| endpoint_stat_error(format!("cannot inspect socket: {error}"), &error))?;
    if !socket_metadata.file_type().is_socket() {
        return Err(endpoint_untrusted_error("endpoint is not a Unix socket"));
    }
    let private = parent_metadata.uid() == effective_uid
        && parent_metadata.mode() & 0o077 == 0
        && socket_metadata.uid() == effective_uid
        && socket_metadata.mode() & 0o077 == 0;
    let public = parent_metadata.uid() == 0
        && parent_metadata.mode() & 0o777 == 0o755
        && socket_metadata.uid() == 0
        && socket_metadata.mode() & 0o777 == 0o666;
    if !private && !public {
        return Err(endpoint_untrusted_error(
            "invalid endpoint owner or group or other permissions; expected private socket or root-owned 0755/0666 daemon endpoint",
        ));
    }
    Ok(socket_metadata.uid())
}

/// A `stat` failure on the parent directory or the socket itself.
///
/// `NotFound` means the daemon has not created the endpoint yet — that is
/// a startup ordering condition, not a trust violation, so it must stay
/// retryable. Any other `stat` failure (notably `PermissionDenied`) means
/// SkillFS cannot establish that the endpoint is safe and is permanent.
fn endpoint_stat_error(message: impl Into<String>, error: &std::io::Error) -> NotifyError {
    if error.kind() == std::io::ErrorKind::NotFound {
        NotifyError::EndpointUnavailable(message.into())
    } else {
        endpoint_untrusted_error(message)
    }
}

fn endpoint_untrusted_error(message: impl Into<String>) -> NotifyError {
    NotifyError::EndpointUntrusted(format!(
        "notify socket endpoint is not trusted: {}",
        message.into()
    ))
}

fn validate_response(body: &str) -> Result<(), NotifyError> {
    let parsed: serde_json::Value =
        serde_json::from_str(body.trim()).map_err(|_| NotifyError::InvalidResponse {
            body: body.trim().to_string(),
        })?;

    let ok = parsed.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
    if !ok {
        return Err(NotifyError::Rejected {
            body: body.trim().to_string(),
        });
    }

    let data = parsed
        .get("data")
        .and_then(|value| value.as_object())
        .ok_or_else(|| NotifyError::InvalidResponse {
            body: body.trim().to_string(),
        })?;
    let schema_version = data.get("schemaVersion").and_then(|value| value.as_u64());
    if schema_version != Some(NOTIFY_SCHEMA_VERSION) {
        return Err(NotifyError::InvalidResponse {
            body: body.trim().to_string(),
        });
    }

    let accepted = data
        .get("accepted")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if !accepted {
        return Err(NotifyError::Rejected {
            body: body.trim().to_string(),
        });
    }

    Ok(())
}

/// No-op client for tests and for when notify is disabled.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopNotifyClient;

impl NotifyClient for NoopNotifyClient {
    fn send(&self, _event: &NotifyChangeEvent) -> Result<(), NotifyError> {
        Ok(())
    }
}

/// In-memory client that records events for tests.
#[derive(Debug, Default)]
pub struct InMemoryNotifyClient {
    events: Mutex<Vec<CapturedNotify>>,
}

#[derive(Debug, Clone)]
/// Notify v2 values recorded by [`InMemoryNotifyClient`].
pub struct CapturedNotify {
    /// Captured protocol schema version.
    pub schema_version: u64,
    /// Captured complete Skill id.
    pub skill_id: String,
    /// Captured mutation kind.
    pub event_kind: String,
    /// Captured relative paths.
    pub paths: Vec<String>,
    /// Captured user-visible canonical Skill directory.
    pub canonical_skill_dir: String,
}

impl InMemoryNotifyClient {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn events(&self) -> Vec<CapturedNotify> {
        self.events.lock().clone()
    }

    pub fn len(&self) -> usize {
        self.events.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.events.lock().is_empty()
    }
}

impl NotifyClient for InMemoryNotifyClient {
    fn send(&self, event: &NotifyChangeEvent) -> Result<(), NotifyError> {
        self.events.lock().push(CapturedNotify {
            schema_version: event.params.schema_version,
            skill_id: event.params.skill_id.clone(),
            event_kind: event.params.event_kind.clone(),
            paths: event.params.paths.clone(),
            canonical_skill_dir: event.params.canonical_skill_dir.clone(),
        });
        Ok(())
    }
}

/// Client that sleeps for a configured duration before returning.
/// Used to verify that FUSE callbacks are not blocked by slow notify.
pub struct SlowNotifyClient {
    delay: Duration,
}

impl SlowNotifyClient {
    pub fn new(delay: Duration) -> Self {
        Self { delay }
    }
}

impl NotifyClient for SlowNotifyClient {
    fn send(&self, _event: &NotifyChangeEvent) -> Result<(), NotifyError> {
        std::thread::sleep(self.delay);
        Ok(())
    }
}

/// Client that always fails, for testing failure resilience.
#[derive(Debug, Default)]
pub struct FailingNotifyClient;

impl NotifyClient for FailingNotifyClient {
    fn send(&self, _event: &NotifyChangeEvent) -> Result<(), NotifyError> {
        Err(NotifyError::Connect(std::io::Error::new(
            std::io::ErrorKind::ConnectionRefused,
            "test: daemon unavailable",
        )))
    }
}

/// Failure mode injected by [`ScriptedNotifyClient`]. One representative
/// error per retry class so tests assert on classification, not wording.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScriptedNotifyFailure {
    /// `Connect(ConnectionRefused)` — daemon has not bound the socket.
    /// Transient.
    ConnectionRefused,
    /// `EndpointUnavailable` — socket file does not exist yet. Transient.
    SocketMissing,
    /// `EndpointUntrusted` — socket mode or owner is unsafe. Permanent.
    UntrustedEndpoint,
    /// `AuthInconclusive` — the peer closed the connection while SkillFS
    /// waited for `auth.ok`, which is what both a daemon restart and a
    /// refused client proof look like. Ambiguous (bounded retry).
    HandshakeEof,
    /// `AuthRejected` — HMAC proof mismatch reported explicitly. Permanent.
    AuthRejected,
    /// `Rejected` — daemon answered but refused the request. Permanent.
    DaemonRejected,
}

impl ScriptedNotifyFailure {
    fn to_error(self) -> NotifyError {
        match self {
            Self::ConnectionRefused => NotifyError::Connect(std::io::Error::new(
                std::io::ErrorKind::ConnectionRefused,
                "test: daemon not listening",
            )),
            Self::SocketMissing => {
                NotifyError::EndpointUnavailable("test: socket not created yet".to_string())
            }
            Self::UntrustedEndpoint => NotifyError::EndpointUntrusted(
                "test: socket must not grant group or other permissions".to_string(),
            ),
            Self::HandshakeEof => {
                // Exactly what auth_error_to_notify produces for the EOF
                // that read_frame reports as AuthError::InvalidFrame.
                auth_error_to_notify(super::auth::AuthError::InvalidFrame)
            }
            Self::AuthRejected => NotifyError::AuthRejected(
                "test: authentication proof verification failed".to_string(),
            ),
            Self::DaemonRejected => NotifyError::Rejected {
                body: r#"{"ok":false,"error":{"code":"unknown_skill"}}"#.to_string(),
            },
        }
    }
}

/// Test client that fails its first `failures` attempts with a chosen error
/// and acknowledges every attempt after that. Records attempt count and
/// accepted events so tests can assert both retry cadence and convergence.
#[derive(Debug)]
pub struct ScriptedNotifyClient {
    failure: ScriptedNotifyFailure,
    failures: u32,
    attempts: AtomicU64,
    events: Mutex<Vec<CapturedNotify>>,
}

impl ScriptedNotifyClient {
    /// Fail the first `failures` attempts, then succeed.
    pub fn new(failure: ScriptedNotifyFailure, failures: u32) -> Self {
        Self {
            failure,
            failures,
            attempts: AtomicU64::new(0),
            events: Mutex::new(Vec::new()),
        }
    }

    /// Never succeed.
    pub fn always_failing(failure: ScriptedNotifyFailure) -> Self {
        Self::new(failure, u32::MAX)
    }

    /// Total `send` calls received, including failed ones.
    pub fn attempts(&self) -> u64 {
        self.attempts.load(Ordering::Relaxed)
    }

    /// Events from the attempts that succeeded.
    pub fn events(&self) -> Vec<CapturedNotify> {
        self.events.lock().clone()
    }
}

impl NotifyClient for ScriptedNotifyClient {
    fn send(&self, event: &NotifyChangeEvent) -> Result<(), NotifyError> {
        let attempt = self.attempts.fetch_add(1, Ordering::Relaxed);
        if attempt < u64::from(self.failures) {
            return Err(self.failure.to_error());
        }
        self.events.lock().push(CapturedNotify {
            schema_version: event.params.schema_version,
            skill_id: event.params.skill_id.clone(),
            event_kind: event.params.event_kind.clone(),
            paths: event.params.paths.clone(),
            canonical_skill_dir: event.params.canonical_skill_dir.clone(),
        });
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Reconcile retry scheduling
// ---------------------------------------------------------------------------

/// Exponential backoff with jitter for reconcile retries.
///
/// `attempts` is the number of failed deliveries so far, so the first
/// retry passes `1`. The un-jittered interval is
/// `RECONCILE_RETRY_BASE_MS * 2^(attempts - 1)` capped at
/// `RECONCILE_RETRY_MAX_MS`; `jitter_permille` in `[0, 1000)` then spreads
/// the result uniformly over `±RECONCILE_RETRY_JITTER_PCT` of that
/// interval. The jitter source is a parameter so the schedule is
/// deterministically testable.
///
/// The final clamp keeps the result inside `[1, RECONCILE_RETRY_MAX_MS]`,
/// so once the interval saturates the cap the jitter becomes one-sided
/// (downward only). That still decorrelates many skills retrying against
/// one daemon, which is the point of the jitter.
fn reconcile_retry_delay(attempts: u32, jitter_permille: u64) -> Duration {
    let shift = attempts.saturating_sub(1).min(RECONCILE_RETRY_MAX_SHIFT);
    let base = RECONCILE_RETRY_BASE_MS
        .saturating_mul(1_u64 << shift)
        .min(RECONCILE_RETRY_MAX_MS);
    let span = base * RECONCILE_RETRY_JITTER_PCT / 100;
    // Map [0, 1000) onto [-span, +span) without signed arithmetic.
    let offset = span * 2 * jitter_permille.min(999) / 1000;
    let jittered = base + offset - span;
    Duration::from_millis(jittered.clamp(1, RECONCILE_RETRY_MAX_MS))
}

/// Uniform jitter draw in `[0, 1000)`.
///
/// An entropy failure degrades to the midpoint (no jitter) rather than
/// failing the retry — losing jitter is strictly better than losing
/// convergence.
fn random_permille() -> u64 {
    let mut bytes = [0_u8; 2];
    if getrandom::fill(&mut bytes).is_err() {
        return 500;
    }
    u64::from(u16::from_le_bytes(bytes)) % 1000
}

/// Cumulative notify delivery counters plus the current pending gauge.
///
/// `attempted`, `succeeded`, and `failed` are monotonic counters over the
/// controller's lifetime; `pending` is a gauge sampled at snapshot time.
///
/// A skill whose reconcile fails transiently increments `failed` **and**
/// remains counted in `pending`, because it is still queued for another
/// attempt. So `attempted != succeeded + failed` only while an attempt is
/// in flight, and `pending` is not derivable from the counters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct NotifyMetricsSnapshot {
    /// Delivery attempts made, counting every retry separately.
    pub attempted: u64,
    /// Attempts the daemon acknowledged.
    pub succeeded: u64,
    /// Attempts that failed, counting every retry separately.
    pub failed: u64,
    /// Skills currently *queued* for a delivery attempt.
    ///
    /// A skill whose attempt is in flight right now is not counted: the
    /// worker drains an entry out of the queue before dispatching it and
    /// only puts it back if the attempt failed retryably. So this reads 0
    /// during the send window of the last unconverged skill. Alert on it
    /// being non-zero over a window, not on a single sample.
    pub pending: u64,
}

/// What the worker should do with an entry after one delivery attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SendOutcome {
    /// The daemon acknowledged; the entry is done.
    Delivered,
    /// Transient failure; a reconcile entry should be requeued with
    /// backoff, without an attempt limit. Ordinary mutations are still
    /// dropped.
    Retry,
    /// Ambiguous failure; a reconcile entry should be requeued only while
    /// the shared endpoint budget is under
    /// [`RECONCILE_AMBIGUOUS_RETRY_LIMIT`].
    RetryBounded,
    /// Permanent failure; requeueing cannot help.
    Failed,
}

// ---------------------------------------------------------------------------
// NotifyController (debounce + dispatch)
// ---------------------------------------------------------------------------

pub struct NotifyController {
    inner: Arc<NotifyInner>,
}

struct NotifyInner {
    client: Arc<dyn NotifyClient>,
    protocol_event_writer: Arc<dyn ProtocolEventWriter>,
    reload_controller: Option<Arc<ActivationReloadController>>,
    /// A5: watcher registrar for auto-tracking skills observed through
    /// notify. Set post-construction via `set_watcher_registrar`.
    watcher_registrar: Mutex<Option<Arc<dyn WatcherRegistrar>>>,
    canonical_root: PathBuf,
    protocol_event_root: PathBuf,
    debounce: Duration,
    timeout_ms: u64,
    /// Keyed by canonical Skill identity (`skill_id`), so an entry that is
    /// requeued after a failed reconcile deduplicates against any
    /// mutation queued in the meantime.
    pending: Mutex<HashMap<String, NotifyPendingState>>,
    /// Authentication ambiguity is an endpoint property: every Skill uses
    /// the same socket and key. This gate ensures only one Skill probes the
    /// endpoint per backoff round instead of multiplying retries by the
    /// number of pending Skills.
    endpoint_retry: Mutex<EndpointRetryState>,
    /// Set before the shutdown command is sent. Blocks requeues so a
    /// permanently unreachable daemon cannot keep the worker alive.
    shutdown: AtomicBool,
    attempted: AtomicU64,
    succeeded: AtomicU64,
    failed: AtomicU64,
    notify: TokioNotify,
    sender: mpsc::UnboundedSender<NotifyCommand>,
}

#[derive(Debug, Clone)]
struct NotifyPendingState {
    skill_id: String,
    event_kind: NotifyEventKind,
    paths: HashSet<String>,
    fire_at: Instant,
    /// Failed delivery attempts already made for this entry. Only
    /// reconcile entries are requeued, so this stays `0` for ordinary
    /// FUSE mutations.
    attempts: u32,
    /// Endpoint retry cycle that owns this entry. An older drained batch
    /// must not resume after a later explicit enqueue reopens an exhausted
    /// authentication gate.
    reconcile_generation: u64,
}

#[derive(Debug, Default)]
struct EndpointRetryState {
    ambiguous_failures: u32,
    retry_at: Option<Instant>,
    exhausted: bool,
    generation: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EndpointGateAction {
    Attempt,
    DeferUntil(Instant),
    Abandon,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AmbiguousFailureAction {
    RetryAt {
        retry_at: Instant,
        ambiguous_failures: u32,
    },
    Exhausted {
        ambiguous_failures: u32,
    },
    Stale,
}

#[derive(Debug)]
enum NotifyCommand {
    Wakeup,
    Shutdown,
}

impl NotifyController {
    pub fn new(
        client: Arc<dyn NotifyClient>,
        canonical_root: impl Into<PathBuf>,
        debounce: Duration,
        timeout_ms: u64,
    ) -> Arc<Self> {
        let canonical_root = canonical_root.into();
        Self::new_with_protocol_writer(
            client,
            canonical_root.clone(),
            canonical_root,
            debounce,
            timeout_ms,
            Arc::new(NoopProtocolEventWriter),
        )
    }

    pub fn new_with_protocol_writer(
        client: Arc<dyn NotifyClient>,
        canonical_root: impl Into<PathBuf>,
        protocol_event_root: impl Into<PathBuf>,
        debounce: Duration,
        timeout_ms: u64,
        protocol_event_writer: Arc<dyn ProtocolEventWriter>,
    ) -> Arc<Self> {
        Self::new_full(
            client,
            canonical_root,
            protocol_event_root,
            debounce,
            timeout_ms,
            protocol_event_writer,
            None,
        )
    }

    pub fn new_with_reload(
        client: Arc<dyn NotifyClient>,
        canonical_root: impl Into<PathBuf>,
        protocol_event_root: impl Into<PathBuf>,
        debounce: Duration,
        timeout_ms: u64,
        protocol_event_writer: Arc<dyn ProtocolEventWriter>,
        reload_controller: Arc<ActivationReloadController>,
    ) -> Arc<Self> {
        Self::new_full(
            client,
            canonical_root,
            protocol_event_root,
            debounce,
            timeout_ms,
            protocol_event_writer,
            Some(reload_controller),
        )
    }

    /// Build a controller with **no background worker**, for unit tests that
    /// need to own dispatch timing.
    ///
    /// The `flush_*_for_testing` helpers drain the same `pending` map the
    /// worker does, so with a live worker present both race for entries —
    /// and `enqueue_startup_reconcile` makes that race easy to lose,
    /// because it queues at `fire_at = now` and wakes the worker
    /// immediately regardless of the debounce. A test that asserts on
    /// `pending_len`, attempt counts, or backoff state must therefore be
    /// the only thing draining the map.
    ///
    /// Tests covering the production dispatch path should use a normal
    /// constructor instead and assert only on eventual convergence.
    #[cfg(test)]
    fn new_for_testing_without_worker(
        client: Arc<dyn NotifyClient>,
        canonical_root: impl Into<PathBuf>,
        protocol_event_root: impl Into<PathBuf>,
        debounce: Duration,
        timeout_ms: u64,
        protocol_event_writer: Arc<dyn ProtocolEventWriter>,
        reload_controller: Option<Arc<ActivationReloadController>>,
    ) -> Arc<Self> {
        Self::build(
            client,
            canonical_root,
            protocol_event_root,
            debounce,
            timeout_ms,
            protocol_event_writer,
            reload_controller,
            false,
        )
    }

    fn new_full(
        client: Arc<dyn NotifyClient>,
        canonical_root: impl Into<PathBuf>,
        protocol_event_root: impl Into<PathBuf>,
        debounce: Duration,
        timeout_ms: u64,
        protocol_event_writer: Arc<dyn ProtocolEventWriter>,
        reload_controller: Option<Arc<ActivationReloadController>>,
    ) -> Arc<Self> {
        Self::build(
            client,
            canonical_root,
            protocol_event_root,
            debounce,
            timeout_ms,
            protocol_event_writer,
            reload_controller,
            true,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn build(
        client: Arc<dyn NotifyClient>,
        canonical_root: impl Into<PathBuf>,
        protocol_event_root: impl Into<PathBuf>,
        debounce: Duration,
        timeout_ms: u64,
        protocol_event_writer: Arc<dyn ProtocolEventWriter>,
        reload_controller: Option<Arc<ActivationReloadController>>,
        spawn_worker: bool,
    ) -> Arc<Self> {
        let (tx, rx) = mpsc::unbounded_channel();
        let inner = Arc::new(NotifyInner {
            client,
            protocol_event_writer,
            reload_controller,
            watcher_registrar: Mutex::new(None),
            canonical_root: canonical_root.into(),
            protocol_event_root: protocol_event_root.into(),
            debounce,
            timeout_ms,
            pending: Mutex::new(HashMap::new()),
            endpoint_retry: Mutex::new(EndpointRetryState::default()),
            shutdown: AtomicBool::new(false),
            attempted: AtomicU64::new(0),
            succeeded: AtomicU64::new(0),
            failed: AtomicU64::new(0),
            notify: TokioNotify::new(),
            sender: tx,
        });
        if !spawn_worker {
            return Arc::new(Self { inner });
        }
        let worker_inner = inner.clone();
        match Handle::try_current() {
            Ok(handle) => {
                handle.spawn(async move { notify_worker_loop(worker_inner, rx).await });
            }
            Err(_) => {
                let spawn_result = std::thread::Builder::new()
                    .name("skillfs-notify".to_string())
                    .spawn(move || {
                        let rt = match Builder::new_current_thread()
                            .enable_time()
                            .enable_io()
                            .build()
                        {
                            Ok(rt) => rt,
                            Err(e) => {
                                warn!(
                                    error = %e,
                                    "failed to build Tokio runtime for notify worker; \
                                     notifications will be dropped"
                                );
                                return;
                            }
                        };
                        rt.block_on(notify_worker_loop(worker_inner, rx));
                    });
                if spawn_result.is_err() {
                    warn!(
                        "failed to spawn skillfs-notify worker thread; \
                         notifications will be dropped"
                    );
                }
            }
        }
        Arc::new(Self { inner })
    }

    /// Record a FUSE mutation observation. Returns `true` when accepted,
    /// `false` when filtered (skill-discover, `.skill-meta/**`, lifecycle).
    ///
    /// Mutation delivery stays best-effort and single-attempt: unlike
    /// reconcile, a failed mutation notify is not requeued. The daemon
    /// re-derives the same state from the next reconcile or the next
    /// mutation in the same skill.
    pub fn observe(
        &self,
        skill_id: &str,
        relative_path: Option<&Path>,
        kind: MutationKind,
    ) -> bool {
        if !is_notify_eligible(skill_id) {
            return false;
        }
        if let Some(rel) = relative_path {
            if is_skill_meta_path(rel) {
                return false;
            }
        }

        let event_kind = NotifyEventKind::from_mutation_kind(kind);
        let now = Instant::now();
        let fire_at = now + self.inner.debounce;
        let reconcile_generation = self.inner.endpoint_retry.lock().generation;
        {
            let mut pending = self.inner.pending.lock();
            let entry = pending
                .entry(skill_id.to_string())
                .or_insert_with(|| NotifyPendingState {
                    skill_id: skill_id.to_string(),
                    event_kind,
                    paths: HashSet::new(),
                    fire_at,
                    attempts: 0,
                    reconcile_generation,
                });
            // A queued reconcile is a full rescan with empty `paths`, so it
            // already subsumes this single-path mutation. Leave the entry
            // completely alone: overwriting `event_kind` would downgrade the
            // rescan into a partial notify and discard its retry state, and
            // touching `fire_at` would either defeat the reconcile backoff
            // or delay the reconcile. Nothing is lost — whenever the
            // reconcile does fire, it covers this mutation too.
            if entry.event_kind != NotifyEventKind::Reconcile {
                // Trailing-edge debounce: every mutation pushes the deadline
                // out, so a continuous write burst dispatches once after it
                // goes quiet rather than mid-write.
                entry.fire_at = fire_at;
                entry.event_kind = event_kind;
                if let Some(rel) = relative_path {
                    let path_str = rel.to_string_lossy().to_string();
                    if !path_str.is_empty() {
                        entry.paths.insert(path_str);
                    }
                }
            }
        }
        let _ = self.inner.sender.send(NotifyCommand::Wakeup);
        self.inner.notify.notify_one();
        true
    }

    /// Convenience wrapper matching `RefreshController::observe_mutation`.
    pub fn observe_mutation(
        &self,
        skill_id: &str,
        relative_path: Option<&Path>,
        kind: MutationKind,
    ) -> bool {
        self.observe(skill_id, relative_path, kind)
    }

    /// Drain and send all pending notifications synchronously. Test helper.
    ///
    /// Mirrors the worker: a transiently failed reconcile is requeued with
    /// backoff, so the returned count is the number of *attempts* made,
    /// not the number of skills that converged. Use
    /// [`Self::flush_until_delivered_for_testing`] to drive retries to
    /// completion.
    pub fn flush_for_testing(&self) -> usize {
        let attempted_before = self.inner.attempted.load(Ordering::Relaxed);
        let drained = self
            .inner
            .drain_due(Instant::now() + self.inner.debounce * 2);
        for state in drained {
            self.inner.dispatch_one(state);
        }
        self.inner
            .attempted
            .load(Ordering::Relaxed)
            .saturating_sub(attempted_before) as usize
    }

    /// Repeatedly drain and send every pending entry, ignoring backoff
    /// deadlines, until nothing is pending or `max_rounds` is exhausted.
    /// Test helper for retry convergence; returns the total number of
    /// delivery attempts made.
    pub fn flush_until_delivered_for_testing(&self, max_rounds: usize) -> usize {
        let attempted_before = self.inner.attempted.load(Ordering::Relaxed);
        for _ in 0..max_rounds {
            // A far-future deadline sweeps up entries still inside their
            // backoff window so tests do not have to sleep.
            let drained = self
                .inner
                .drain_due(Instant::now() + Duration::from_secs(86_400));
            if drained.is_empty() {
                break;
            }
            for state in drained {
                self.inner.dispatch_one(state);
            }
        }
        self.inner
            .attempted
            .load(Ordering::Relaxed)
            .saturating_sub(attempted_before) as usize
    }

    /// Number of skills currently queued and awaiting a successful
    /// delivery.
    pub fn pending_len(&self) -> usize {
        self.inner.pending.lock().len()
    }

    /// Snapshot the delivery counters and the pending gauge. See
    /// [`NotifyMetricsSnapshot`] for how the four values relate.
    pub fn metrics(&self) -> NotifyMetricsSnapshot {
        NotifyMetricsSnapshot {
            attempted: self.inner.attempted.load(Ordering::Relaxed),
            succeeded: self.inner.succeeded.load(Ordering::Relaxed),
            failed: self.inner.failed.load(Ordering::Relaxed),
            pending: self.inner.pending.lock().len() as u64,
        }
    }

    /// Emit startup reconcile events for all given skills. Bypasses
    /// debounce and sends immediately. Each eligible skill gets one
    /// `eventKind="reconcile"` protocol event and one notify. Filtered
    /// skills (skill-discover, lifecycle roots) are skipped.
    ///
    /// **Blocking and single-attempt**: each `client.send()` may wait up
    /// to `timeout_ms` per skill, and a failed send is only logged. The
    /// returned count is the number of skills *attempted*, not the number
    /// the daemon acknowledged.
    ///
    /// Production mount startup must use
    /// [`Self::enqueue_startup_reconcile`] instead: it neither blocks the
    /// caller nor drops a delivery that failed because the daemon socket
    /// was not ready yet.
    pub fn emit_startup_reconcile(&self, skill_ids: &[String]) -> usize {
        let mut count = 0;
        for skill_id in skill_ids {
            if !is_notify_eligible(skill_id) {
                continue;
            }
            let canonical_skill_dir = self.inner.canonical_root.join(skill_id);
            let canonical_skill_dir = canonical_skill_dir.to_string_lossy().to_string();
            let protocol_skill_dir = self.inner.protocol_event_root.join(skill_id);
            let protocol_skill_dir = protocol_skill_dir.to_string_lossy().to_string();

            let protocol_event = ProtocolEvent::new(
                &protocol_skill_dir,
                skill_id.as_str(),
                "reconcile",
                Vec::new(),
            );
            self.inner.protocol_event_writer.emit(&protocol_event);

            let event = NotifyChangeEvent::new(
                &canonical_skill_dir,
                skill_id.as_str(),
                NotifyEventKind::Reconcile,
                Vec::new(),
                self.inner.timeout_ms,
            );

            self.inner.attempted.fetch_add(1, Ordering::Relaxed);
            if let Err(e) = self.inner.client.send(&event) {
                self.inner.failed.fetch_add(1, Ordering::Relaxed);
                warn!(
                    skill = %skill_id,
                    error = %e,
                    retry_class = ?e.retry_class(),
                    "reconcile: failed to send reconcile notification"
                );
            } else {
                self.inner.succeeded.fetch_add(1, Ordering::Relaxed);
                debug!(
                    skill = %skill_id,
                    "reconcile: startup reconcile notification sent"
                );
            }

            count += 1;
        }
        info!(count, "reconcile: startup reconcile complete");
        count
    }

    /// Queue a startup reconcile for every eligible skill and let the
    /// background worker own delivery.
    ///
    /// Each eligible skill is written into the shared pending map as an
    /// immediately due `eventKind="reconcile"` entry with empty `paths`,
    /// deduplicated by canonical Skill identity. The worker then keeps
    /// retrying entries with exponential backoff and jitter for as long as
    /// delivery fails transiently — daemon socket not created yet,
    /// connection refused, socket recreated mid-handshake — so a mount
    /// that starts before the daemon still converges without an external
    /// trigger. An inconclusive authentication handshake is retried through
    /// a controller-wide endpoint gate: one Skill probes per backoff round,
    /// and the current reconcile set is abandoned after the bounded budget
    /// is exhausted. Permanent failures (untrusted endpoint, explicit
    /// authentication refusal, daemon rejection, malformed response) are
    /// logged at `error` and dropped because an identical retry can only
    /// fail identically.
    ///
    /// This replaces the previous detached one-shot thread. Enqueueing
    /// touches no socket and spawns no thread, so it neither blocks mount
    /// startup nor holds an `Arc<NotifyController>` that would stop `Drop`
    /// from triggering shutdown.
    ///
    /// Returns the number of newly inserted pending entries after identity
    /// deduplication. Existing entries may still be upgraded to reconcile,
    /// so the return value is not the total queue depth or a delivery count.
    /// Read [`Self::metrics`] for the current depth and delivery outcomes.
    pub fn enqueue_startup_reconcile(&self, skill_ids: &[String]) -> usize {
        let now = Instant::now();
        let has_eligible = skill_ids
            .iter()
            .any(|skill_id| is_notify_eligible(skill_id));
        let reconcile_generation = has_eligible
            .then(|| self.inner.start_new_reconcile_cycle_if_exhausted())
            .unwrap_or_default();

        let mut newly_queued = 0;
        let mut accepted = 0;
        for skill_id in skill_ids {
            if !is_notify_eligible(skill_id) {
                continue;
            }
            accepted += 1;
            if self.inner.merge_pending(NotifyPendingState {
                skill_id: skill_id.clone(),
                event_kind: NotifyEventKind::Reconcile,
                paths: HashSet::new(),
                fire_at: now,
                attempts: 0,
                reconcile_generation,
            }) {
                newly_queued += 1;
            }
        }
        if accepted > 0 {
            let _ = self.inner.sender.send(NotifyCommand::Wakeup);
            self.inner.notify.notify_one();
        }
        info!(
            accepted,
            newly_queued, "reconcile: startup reconcile queued for retrying delivery"
        );
        newly_queued
    }

    /// A5: inject an activation watcher registrar so that skills observed
    /// through notify are automatically tracked for late-activation
    /// convergence. Called post-construction because the watcher is
    /// built after the notify controller.
    pub fn set_watcher_registrar(&self, registrar: Arc<dyn WatcherRegistrar>) {
        *self.inner.watcher_registrar.lock() = Some(registrar);
    }

    /// Enqueue a notification for immediate dispatch by the background
    /// worker. Bypasses the debounce window (fire_at = now) but does NOT
    /// block the calling thread on socket send or activation reload poll.
    /// The worker picks it up on its next iteration.
    pub fn enqueue_immediate(&self, skill_id: &str, kind: MutationKind, paths: Vec<String>) {
        let event_kind = NotifyEventKind::from_mutation_kind(kind);
        let reconcile_generation = self.inner.endpoint_retry.lock().generation;
        self.inner.merge_pending(NotifyPendingState {
            skill_id: skill_id.to_string(),
            event_kind,
            paths: paths.into_iter().collect(),
            fire_at: Instant::now(),
            attempts: 0,
            reconcile_generation,
        });
        let _ = self.inner.sender.send(NotifyCommand::Wakeup);
        self.inner.notify.notify_one();
    }

    pub fn shutdown(&self) {
        // Set the flag before signalling so an in-flight attempt that
        // fails during shutdown cannot requeue itself.
        self.inner.shutdown.store(true, Ordering::Release);
        let _ = self.inner.sender.send(NotifyCommand::Shutdown);
        self.inner.notify.notify_waiters();
    }

    pub fn debounce(&self) -> Duration {
        self.inner.debounce
    }
}

impl Drop for NotifyController {
    fn drop(&mut self) {
        self.shutdown();
    }
}

impl NotifyInner {
    /// A later explicit startup reconcile is a new operator-visible cycle.
    /// Re-open only an exhausted gate; repeated enqueues during an active
    /// retry cycle must not replenish the wrong-key budget.
    fn start_new_reconcile_cycle_if_exhausted(&self) -> u64 {
        let mut endpoint_retry = self.endpoint_retry.lock();
        if endpoint_retry.exhausted {
            endpoint_retry.ambiguous_failures = 0;
            endpoint_retry.retry_at = None;
            endpoint_retry.exhausted = false;
            endpoint_retry.generation = endpoint_retry.generation.saturating_add(1);
            info!("reconcile: reopening exhausted authentication retry gate for a new cycle");
        }
        endpoint_retry.generation
    }

    fn drain_due(&self, deadline: Instant) -> Vec<NotifyPendingState> {
        let mut due = Vec::new();
        let mut guard = self.pending.lock();
        let keys: Vec<String> = guard.keys().cloned().collect();
        for key in keys {
            if let Some(state) = guard.get(&key) {
                if state.fire_at <= deadline {
                    if let Some(removed) = guard.remove(&key) {
                        due.push(removed);
                    }
                }
            }
        }
        due
    }

    fn next_fire_at(&self) -> Option<Instant> {
        self.pending.lock().values().map(|s| s.fire_at).min()
    }

    /// Merge an incoming entry into the pending map, deduplicated by
    /// canonical Skill identity.
    ///
    /// Reconcile dominates: if either side is a reconcile the merged entry
    /// stays a reconcile with empty `paths`, because a reconcile is a full
    /// rescan of the current on-disk state and so already covers any path
    /// list. That is what makes requeueing a failed reconcile safe when a
    /// mutation landed on the same skill while the reconcile was in
    /// flight — the requeue merges instead of overwriting, and neither
    /// request is lost.
    ///
    /// `fire_at` takes the incoming entry's deadline, matching the
    /// overwrite semantics this had before it became a merge. For a
    /// reconcile requeue that is the backoff deadline, so the backoff is
    /// honoured rather than being short-circuited by whatever mutation
    /// happened to be queued. Delaying that mutation costs nothing: the
    /// backoff only exists because the daemon is unreachable, so an earlier
    /// attempt would fail anyway. `attempts` takes the larger so backoff
    /// keeps growing across a merge instead of restarting at zero.
    ///
    /// Returns `true` only when the Skill identity was newly inserted.
    fn merge_pending(&self, incoming: NotifyPendingState) -> bool {
        let mut pending = self.pending.lock();
        match pending.get_mut(&incoming.skill_id) {
            Some(existing) => {
                if incoming.reconcile_generation < existing.reconcile_generation {
                    return false;
                }
                if incoming.reconcile_generation > existing.reconcile_generation {
                    *existing = incoming;
                    return false;
                }
                existing.fire_at = incoming.fire_at;
                existing.attempts = existing.attempts.max(incoming.attempts);
                if existing.event_kind == NotifyEventKind::Reconcile
                    || incoming.event_kind == NotifyEventKind::Reconcile
                {
                    existing.event_kind = NotifyEventKind::Reconcile;
                    existing.paths.clear();
                } else {
                    existing.event_kind = incoming.event_kind;
                    existing.paths.extend(incoming.paths);
                }
                false
            }
            None => {
                pending.insert(incoming.skill_id.clone(), incoming);
                true
            }
        }
    }

    /// Decide whether this scheduled reconcile may probe the endpoint.
    ///
    /// Comparing scheduled deadlines rather than wall-clock time keeps the
    /// test drain helpers deterministic while preserving production timing:
    /// an entry from the previous batch has an older deadline and therefore
    /// cannot bypass a gate advanced by the first ambiguous failure.
    fn endpoint_gate_action(&self, generation: u64, scheduled_at: Instant) -> EndpointGateAction {
        let endpoint_retry = self.endpoint_retry.lock();
        if generation != endpoint_retry.generation || endpoint_retry.exhausted {
            return EndpointGateAction::Abandon;
        }
        match endpoint_retry.retry_at {
            Some(retry_at) if scheduled_at < retry_at => EndpointGateAction::DeferUntil(retry_at),
            _ => EndpointGateAction::Attempt,
        }
    }

    /// Record one endpoint-wide ambiguous authentication result.
    fn record_ambiguous_failure(&self, generation: u64) -> AmbiguousFailureAction {
        let mut endpoint_retry = self.endpoint_retry.lock();
        if generation != endpoint_retry.generation || endpoint_retry.exhausted {
            return AmbiguousFailureAction::Stale;
        }
        endpoint_retry.ambiguous_failures = endpoint_retry.ambiguous_failures.saturating_add(1);
        let ambiguous_failures = endpoint_retry.ambiguous_failures;
        if ambiguous_failures >= RECONCILE_AMBIGUOUS_RETRY_LIMIT {
            endpoint_retry.retry_at = None;
            endpoint_retry.exhausted = true;
            drop(endpoint_retry);

            // Entries already drained by the worker observe `exhausted` in
            // `endpoint_gate_action`; entries not yet drained are removed
            // here. Together those paths stop the whole endpoint without
            // performing one extra handshake per Skill.
            self.abandon_reconcile_generation(generation);
            AmbiguousFailureAction::Exhausted { ambiguous_failures }
        } else {
            let delay = reconcile_retry_delay(ambiguous_failures, random_permille());
            let retry_at = Instant::now() + delay;
            endpoint_retry.retry_at = Some(retry_at);
            AmbiguousFailureAction::RetryAt {
                retry_at,
                ambiguous_failures,
            }
        }
    }

    /// Remove only reconciles owned by the exhausted retry cycle.
    ///
    /// The endpoint lock is intentionally released before taking `pending`.
    /// A concurrent explicit enqueue may reopen the gate in that interval,
    /// so deleting every reconcile would erase the new generation too.
    fn abandon_reconcile_generation(&self, generation: u64) {
        self.pending.lock().retain(|_, state| {
            state.event_kind != NotifyEventKind::Reconcile
                || state.reconcile_generation != generation
        });
    }

    /// Any daemon acknowledgement proves that the shared endpoint and key
    /// are healthy, so the next pending Skill need not wait behind the gate.
    fn reset_endpoint_retry_after_ack(&self) {
        let mut endpoint_retry = self.endpoint_retry.lock();
        if endpoint_retry.ambiguous_failures != 0
            || endpoint_retry.retry_at.is_some()
            || endpoint_retry.exhausted
        {
            if endpoint_retry.exhausted {
                // The exhausted cycle already abandoned its drained batch.
                // A later mutation ACK may prove the endpoint recovered,
                // but must not resurrect those obsolete entries.
                endpoint_retry.generation = endpoint_retry.generation.saturating_add(1);
            }
            endpoint_retry.ambiguous_failures = 0;
            endpoint_retry.retry_at = None;
            endpoint_retry.exhausted = false;
            debug!("reconcile: daemon acknowledgement cleared authentication retry gate");
        }
    }

    /// Put a reconcile back without recording another send attempt.
    fn defer_reconcile_until(&self, mut state: NotifyPendingState, retry_at: Instant) {
        if self.shutdown.load(Ordering::Acquire) {
            return;
        }
        state.fire_at = retry_at;
        self.merge_pending(state);
        let _ = self.sender.send(NotifyCommand::Wakeup);
        self.notify.notify_one();
    }

    /// Requeue a reconcile whose delivery failed transiently.
    ///
    /// `attempts` is the running failure count including the attempt that
    /// just failed. No-op once shutdown has been signalled, so an
    /// unreachable daemon cannot keep the worker — and with it the
    /// controller's private runtime thread — alive indefinitely.
    fn requeue_reconcile(&self, skill_id: &str, attempts: u32, reconcile_generation: u64) {
        if self.shutdown.load(Ordering::Acquire) {
            debug!(
                skill = %skill_id,
                "reconcile: shutdown in progress; not requeueing"
            );
            return;
        }
        if self.endpoint_retry.lock().generation != reconcile_generation {
            debug!(
                skill = %skill_id,
                reconcile_generation,
                "reconcile: not requeueing an obsolete retry cycle"
            );
            return;
        }
        let delay = reconcile_retry_delay(attempts, random_permille());
        self.merge_pending(NotifyPendingState {
            skill_id: skill_id.to_string(),
            event_kind: NotifyEventKind::Reconcile,
            paths: HashSet::new(),
            fire_at: Instant::now() + delay,
            attempts,
            reconcile_generation,
        });
        warn!(
            skill = %skill_id,
            attempts,
            retry_in_ms = delay.as_millis() as u64,
            "reconcile: delivery failed transiently; skill stays degraded \
             until the daemon acknowledges a reconcile"
        );
        let _ = self.sender.send(NotifyCommand::Wakeup);
        self.notify.notify_one();
    }

    /// Requeue the single endpoint probe at the shared authentication
    /// deadline. Other Skills are deferred to the same deadline without a
    /// send attempt when the worker reaches them.
    fn requeue_ambiguous_probe(
        &self,
        skill_id: &str,
        attempts: u32,
        retry_at: Instant,
        ambiguous_failures: u32,
        reconcile_generation: u64,
    ) {
        if self.shutdown.load(Ordering::Acquire) {
            return;
        }
        self.merge_pending(NotifyPendingState {
            skill_id: skill_id.to_string(),
            event_kind: NotifyEventKind::Reconcile,
            paths: HashSet::new(),
            fire_at: retry_at,
            attempts,
            reconcile_generation,
        });
        warn!(
            skill = %skill_id,
            ambiguous_failures,
            limit = RECONCILE_AMBIGUOUS_RETRY_LIMIT,
            retry_in_ms = retry_at.saturating_duration_since(Instant::now()).as_millis() as u64,
            "reconcile: authentication outcome inconclusive; endpoint probe queued"
        );
        let _ = self.sender.send(NotifyCommand::Wakeup);
        self.notify.notify_one();
    }

    /// Deliver one drained entry and apply the retry policy to the result.
    ///
    /// Retry is deliberately scoped to reconcile. Ordinary FUSE mutations
    /// keep their best-effort, single-attempt semantics: the daemon
    /// re-derives the same state from the next reconcile or the next
    /// mutation, and requeueing them would change the latency and
    /// ordering guarantees of the hot write path.
    fn dispatch_one(&self, state: NotifyPendingState) {
        let skill_id = state.skill_id.clone();
        let is_reconcile = state.event_kind == NotifyEventKind::Reconcile;
        let attempts = state.attempts;
        let reconcile_generation = state.reconcile_generation;
        if !is_reconcile {
            // Mutations are best-effort and single-attempt regardless of
            // outcome.
            if self.send_one(state) == SendOutcome::Delivered {
                self.reset_endpoint_retry_after_ack();
            }
            return;
        }

        match self.endpoint_gate_action(reconcile_generation, state.fire_at) {
            EndpointGateAction::Attempt => {}
            EndpointGateAction::DeferUntil(retry_at) => {
                self.defer_reconcile_until(state, retry_at);
                return;
            }
            EndpointGateAction::Abandon => {
                debug!(
                    skill = %skill_id,
                    "reconcile: authentication retry gate exhausted; abandoning queued skill"
                );
                return;
            }
        }

        let attempted = attempts + 1;
        match self.send_one(state) {
            SendOutcome::Delivered => self.reset_endpoint_retry_after_ack(),
            SendOutcome::Retry => {
                self.requeue_reconcile(&skill_id, attempted, reconcile_generation)
            }
            SendOutcome::RetryBounded => {
                match self.record_ambiguous_failure(reconcile_generation) {
                    AmbiguousFailureAction::RetryAt {
                        retry_at,
                        ambiguous_failures,
                    } => self.requeue_ambiguous_probe(
                        &skill_id,
                        attempted,
                        retry_at,
                        ambiguous_failures,
                        reconcile_generation,
                    ),
                    AmbiguousFailureAction::Exhausted { ambiguous_failures } => {
                        tracing::error!(
                            skill = %skill_id,
                            attempts = attempted,
                            ambiguous_failures,
                            limit = RECONCILE_AMBIGUOUS_RETRY_LIMIT,
                            "reconcile: endpoint authentication retry budget exhausted; \
                             check the notify authentication key — pending startup \
                            reconciles will not converge"
                        );
                    }
                    AmbiguousFailureAction::Stale => {
                        debug!(
                            skill = %skill_id,
                            reconcile_generation,
                            "reconcile: ignoring ambiguous result from an obsolete retry cycle"
                        );
                    }
                }
            }
            SendOutcome::Failed => {
                // Deliberately not requeued: an identical retry would
                // reproduce the same refusal, so this needs an operator.
                tracing::error!(
                    skill = %skill_id,
                    attempts = attempted,
                    "reconcile: delivery failed permanently; skill will not \
                     converge until the notify endpoint or daemon is repaired"
                );
            }
        }
    }

    fn send_one(&self, state: NotifyPendingState) -> SendOutcome {
        let canonical_skill_dir = self.canonical_root.join(&state.skill_id);
        let canonical_skill_dir = canonical_skill_dir.to_string_lossy().to_string();
        let protocol_skill_dir = self.protocol_event_root.join(&state.skill_id);
        let protocol_skill_dir = protocol_skill_dir.to_string_lossy().to_string();

        // A3: snapshot activation freshness BEFORE sending the notify so
        // the poll baseline predates the daemon's activation write.
        // Covers both activation.json mtime and skill dir ctime (xattr).
        let pre_notify_freshness = self
            .reload_controller
            .as_ref()
            .map(|r| r.snapshot_freshness(&state.skill_id));

        let paths: Vec<String> = if state.paths.len() > MAX_NOTIFY_PATHS {
            Vec::new()
        } else {
            let mut sorted: Vec<String> = state.paths.into_iter().collect();
            sorted.sort();
            sorted
        };

        // Write protocol event log regardless of notify outcome.
        let protocol_event = ProtocolEvent::new(
            &protocol_skill_dir,
            &state.skill_id,
            state.event_kind.as_str(),
            paths.clone(),
        );
        self.protocol_event_writer.emit(&protocol_event);

        let event = NotifyChangeEvent::new(
            canonical_skill_dir.clone(),
            state.skill_id.clone(),
            state.event_kind,
            paths,
            self.timeout_ms,
        );

        self.attempted.fetch_add(1, Ordering::Relaxed);
        let (outcome, business_request_not_sent) = match self.client.send(&event) {
            Err(e) => {
                self.failed.fetch_add(1, Ordering::Relaxed);
                let business_request_not_sent = e.business_request_not_sent();
                let retry_class = e.retry_class();
                warn!(
                    skill = %state.skill_id,
                    error = %e,
                    ?retry_class,
                    "notify: failed to send change notification; \
                     current activation mapping unchanged"
                );
                // A5: daemon unreachable — register for watcher convergence
                // so a later daemon repair can still be observed.
                self.register_with_watcher(&state.skill_id);
                let outcome = match retry_class {
                    NotifyRetryClass::Transient => SendOutcome::Retry,
                    NotifyRetryClass::Ambiguous => SendOutcome::RetryBounded,
                    NotifyRetryClass::Permanent => SendOutcome::Failed,
                };
                (outcome, business_request_not_sent)
            }
            Ok(()) => {
                self.succeeded.fetch_add(1, Ordering::Relaxed);
                debug!(
                    skill = %state.skill_id,
                    event_kind = state.event_kind.as_str(),
                    "notify: change notification accepted"
                );
                (SendOutcome::Delivered, false)
            }
        };

        // A retryable reconcile failure, or a permanent failure that proves
        // the business request was never sent, cannot produce an activation
        // write. Skipping the poll keeps the serial worker from paying one
        // full reload timeout per Skill. Post-delivery failures still poll,
        // and watcher registration above covers conservative late repair.
        let skip_reload_poll = state.event_kind == NotifyEventKind::Reconcile
            && (matches!(outcome, SendOutcome::Retry | SendOutcome::RetryBounded)
                || business_request_not_sent);

        // A3: poll-after-notify activation reload.
        if let Some(reload) = self
            .reload_controller
            .as_ref()
            .filter(|_| !skip_reload_poll)
        {
            let baseline = pre_notify_freshness
                .expect("reload_controller presence implies freshness was captured");
            debug!(
                skill = %state.skill_id,
                "notify: starting activation reload poll"
            );
            let reload_outcome = reload.poll_reload_skill(&state.skill_id, baseline);
            debug!(
                skill = %state.skill_id,
                outcome = ?reload_outcome,
                "notify: activation reload poll completed"
            );

            // A5: on poll timeout, register the skill with the watcher
            // so late activation writes are still caught by the
            // background convergence loop.
            if matches!(
                reload_outcome,
                super::activation_reload::ReloadOutcome::Timeout
            ) {
                self.register_with_watcher(&state.skill_id);
            }

            // A4: emit reload outcome as a protocol event.
            let reload_event = ProtocolEvent::with_reload_outcome(
                &protocol_skill_dir,
                &state.skill_id,
                reload_outcome.as_protocol_label(),
            );
            self.protocol_event_writer.emit(&reload_event);
        }

        outcome
    }

    /// A5: register a skill with the activation watcher (if set).
    fn register_with_watcher(&self, skill_name: &str) {
        if let Some(ref registrar) = *self.watcher_registrar.lock() {
            registrar.register(skill_name);
            debug!(
                skill = %skill_name,
                "notify: registered skill with activation watcher for convergence"
            );
        }
    }
}

fn is_notify_eligible(skill: &str) -> bool {
    if skill.is_empty() {
        return false;
    }
    if skill == "skill-discover" {
        return false;
    }
    if is_reserved_lifecycle_name(skill) {
        return false;
    }
    true
}

async fn notify_worker_loop(
    inner: Arc<NotifyInner>,
    mut rx: mpsc::UnboundedReceiver<NotifyCommand>,
) {
    debug!("notify worker starting");
    loop {
        let sleep_for = match inner.next_fire_at() {
            Some(t) => t.saturating_duration_since(Instant::now()),
            None => Duration::from_secs(60),
        };
        tokio::select! {
            cmd = rx.recv() => {
                match cmd {
                    Some(NotifyCommand::Wakeup) => {}
                    Some(NotifyCommand::Shutdown) | None => {
                        debug!("notify worker shutting down");
                        return;
                    }
                }
            }
            _ = tokio::time::sleep(sleep_for) => {}
        }

        // Re-check after the select: a Shutdown may have been queued behind
        // a Wakeup, and a requeue after this point would never be drained.
        if inner.shutdown.load(Ordering::Acquire) {
            debug!("notify worker shutting down");
            return;
        }

        let due = inner.drain_due(Instant::now());
        if due.is_empty() {
            continue;
        }
        for state in due {
            // Each dispatch can block for a full socket timeout plus an
            // activation reload poll, so a shutdown arriving mid-batch must
            // abandon the rest of the batch instead of paying that cost per
            // remaining skill.
            if inner.shutdown.load(Ordering::Acquire) {
                debug!("notify worker shutting down; abandoning the rest of the batch");
                return;
            }
            let inner_clone = inner.clone();
            let skill_id = state.skill_id.clone();
            let is_reconcile = state.event_kind == NotifyEventKind::Reconcile;
            let attempts = state.attempts;
            let reconcile_generation = state.reconcile_generation;
            let join = tokio::task::spawn_blocking(move || inner_clone.dispatch_one(state)).await;
            if let Err(e) = join {
                warn!(error = %e, "notify: blocking task join failed");
                // The attempt never reported an outcome, so treat a
                // reconcile as transiently failed rather than losing it.
                if is_reconcile {
                    inner.requeue_reconcile(&skill_id, attempts + 1, reconcile_generation);
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notify_change_event_json_shape() {
        let event = NotifyChangeEvent {
            id: "skillfs-42".to_string(),
            method: NOTIFY_METHOD,
            params: NotifyParams {
                schema_version: NOTIFY_SCHEMA_VERSION,
                canonical_skill_dir: "/srv/skills/category/tianqi-weather".to_string(),
                skill_id: "category/tianqi-weather".to_string(),
                event_kind: "write".to_string(),
                paths: vec!["SKILL.md".to_string()],
            },
            trace_context: serde_json::json!({}),
            timeout_ms: 5000,
        };

        let json = serde_json::to_value(&event).unwrap();
        let envelope = json.as_object().unwrap();
        assert_eq!(envelope.len(), 5);
        assert_eq!(json["id"], "skillfs-42");
        assert_eq!(json["method"], "skill_ledger.skillfs_notify_change");
        let params = json["params"].as_object().unwrap();
        assert_eq!(params.len(), 5);
        assert_eq!(json["params"]["schemaVersion"], 2);
        assert_eq!(
            json["params"]["canonicalSkillDir"],
            "/srv/skills/category/tianqi-weather"
        );
        assert_eq!(json["params"]["skillId"], "category/tianqi-weather");
        assert_eq!(json["params"]["eventKind"], "write");
        assert_eq!(json["params"]["paths"], serde_json::json!(["SKILL.md"]));
        for forbidden in [
            "skillDir",
            "skillName",
            "mountId",
            "generation",
            "resolverSocket",
            "sourceId",
        ] {
            assert!(
                !params.contains_key(forbidden),
                "unexpected field: {forbidden}"
            );
        }
        assert_eq!(json["trace_context"], serde_json::json!({}));
        assert_eq!(json["timeout_ms"], 5000);
    }

    #[test]
    fn notify_event_kind_from_mutation_kind_mapping() {
        assert_eq!(
            NotifyEventKind::from_mutation_kind(MutationKind::Mkdir),
            NotifyEventKind::Mkdir
        );
        assert_eq!(
            NotifyEventKind::from_mutation_kind(MutationKind::Create),
            NotifyEventKind::Create
        );
        assert_eq!(
            NotifyEventKind::from_mutation_kind(MutationKind::Write),
            NotifyEventKind::Write
        );
        assert_eq!(
            NotifyEventKind::from_mutation_kind(MutationKind::Rename),
            NotifyEventKind::Rename
        );
        assert_eq!(
            NotifyEventKind::from_mutation_kind(MutationKind::Unlink),
            NotifyEventKind::Unlink
        );
        assert_eq!(
            NotifyEventKind::from_mutation_kind(MutationKind::Rmdir),
            NotifyEventKind::Rmdir
        );
        assert_eq!(
            NotifyEventKind::from_mutation_kind(MutationKind::SetattrTruncate),
            NotifyEventKind::Truncate
        );
    }

    #[test]
    fn notify_event_kind_labels() {
        assert_eq!(NotifyEventKind::Mkdir.as_str(), "mkdir");
        assert_eq!(NotifyEventKind::Create.as_str(), "create");
        assert_eq!(NotifyEventKind::Write.as_str(), "write");
        assert_eq!(NotifyEventKind::Rename.as_str(), "rename");
        assert_eq!(NotifyEventKind::Unlink.as_str(), "unlink");
        assert_eq!(NotifyEventKind::Rmdir.as_str(), "rmdir");
        assert_eq!(NotifyEventKind::Truncate.as_str(), "truncate");
        assert_eq!(NotifyEventKind::Unknown.as_str(), "unknown");
    }

    #[test]
    fn noop_client_succeeds() {
        let client = NoopNotifyClient;
        let event = NotifyChangeEvent::new(
            "/srv/skills/alpha",
            "alpha",
            NotifyEventKind::Write,
            vec!["SKILL.md".to_string()],
            5000,
        );
        assert!(client.send(&event).is_ok());
    }

    #[test]
    fn in_memory_client_records() {
        let client = InMemoryNotifyClient::new();
        assert!(client.is_empty());
        let event = NotifyChangeEvent::new(
            "/srv/skills/alpha",
            "alpha",
            NotifyEventKind::Write,
            vec!["SKILL.md".to_string()],
            5000,
        );
        client.send(&event).unwrap();
        assert_eq!(client.len(), 1);
        let events = client.events();
        assert_eq!(events[0].schema_version, 2);
        assert_eq!(events[0].skill_id, "alpha");
        assert_eq!(events[0].event_kind, "write");
        assert_eq!(events[0].paths, vec!["SKILL.md"]);
    }

    #[test]
    fn validate_response_accepts_ok_accepted() {
        let body = r#"{"ok":true,"data":{"schemaVersion":2,"accepted":true}}"#;
        assert!(validate_response(body).is_ok());
    }

    #[test]
    fn validate_response_rejects_v1_schema() {
        let body = r#"{"ok":true,"data":{"schemaVersion":1,"accepted":true}}"#;
        assert!(matches!(
            validate_response(body),
            Err(NotifyError::InvalidResponse { .. })
        ));
    }

    #[test]
    fn validate_response_rejects_missing_schema() {
        let body = r#"{"ok":true,"data":{"accepted":true}}"#;
        assert!(matches!(
            validate_response(body),
            Err(NotifyError::InvalidResponse { .. })
        ));
    }

    #[test]
    fn validate_response_rejects_ok_false() {
        let body = r#"{"ok":false,"error":{"code":"not_found"}}"#;
        assert!(matches!(
            validate_response(body),
            Err(NotifyError::Rejected { .. })
        ));
    }

    #[test]
    fn validate_response_rejects_accepted_false() {
        let body = r#"{"ok":true,"data":{"schemaVersion":2,"accepted":false}}"#;
        assert!(matches!(
            validate_response(body),
            Err(NotifyError::Rejected { .. })
        ));
    }

    #[test]
    fn validate_response_rejects_invalid_json() {
        let body = "not json at all";
        assert!(matches!(
            validate_response(body),
            Err(NotifyError::InvalidResponse { .. })
        ));
    }

    #[test]
    fn validate_response_rejects_missing_data() {
        let body = r#"{"ok":true}"#;
        assert!(matches!(
            validate_response(body),
            Err(NotifyError::InvalidResponse { .. })
        ));
    }

    #[test]
    fn controller_filters_skill_discover() {
        let client = Arc::new(InMemoryNotifyClient::new());
        let ctrl = NotifyController::new(
            client.clone(),
            "/srv/skills",
            Duration::from_millis(50),
            5000,
        );
        let accepted = ctrl.observe(
            "skill-discover",
            Some(Path::new("SKILL.md")),
            MutationKind::Write,
        );
        assert!(!accepted);
        ctrl.shutdown();
    }

    #[test]
    fn controller_filters_skill_meta_paths() {
        let client = Arc::new(InMemoryNotifyClient::new());
        let ctrl = NotifyController::new(
            client.clone(),
            "/srv/skills",
            Duration::from_millis(50),
            5000,
        );
        let accepted = ctrl.observe(
            "alpha",
            Some(Path::new(".skill-meta/manifest.json")),
            MutationKind::Write,
        );
        assert!(!accepted);
        ctrl.shutdown();
    }

    #[test]
    fn controller_filters_lifecycle_reserved() {
        let client = Arc::new(InMemoryNotifyClient::new());
        let ctrl = NotifyController::new(
            client.clone(),
            "/srv/skills",
            Duration::from_millis(50),
            5000,
        );
        for name in &[".staging", ".certified", ".quarantine", ".archive"] {
            let accepted = ctrl.observe(name, None, MutationKind::Mkdir);
            assert!(!accepted, "{name} must be filtered");
        }
        ctrl.shutdown();
    }

    #[test]
    fn controller_debounce_collapses() {
        let client = Arc::new(InMemoryNotifyClient::new());
        let ctrl = NotifyController::new(
            client.clone(),
            "/srv/skills",
            Duration::from_millis(50),
            5000,
        );
        for _ in 0..5 {
            ctrl.observe("alpha", Some(Path::new("SKILL.md")), MutationKind::Write);
        }
        let processed = ctrl.flush_for_testing();
        assert_eq!(processed, 1, "five observations must collapse to one");
        let events = client.events();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].skill_id, "alpha");
        ctrl.shutdown();
    }

    #[test]
    fn controller_collects_and_deduplicates_paths() {
        let client = Arc::new(InMemoryNotifyClient::new());
        let ctrl = NotifyController::new(
            client.clone(),
            "/srv/skills",
            Duration::from_millis(50),
            5000,
        );
        ctrl.observe("alpha", Some(Path::new("SKILL.md")), MutationKind::Write);
        ctrl.observe(
            "alpha",
            Some(Path::new("scripts/run.sh")),
            MutationKind::Create,
        );
        ctrl.observe("alpha", Some(Path::new("SKILL.md")), MutationKind::Write);
        let processed = ctrl.flush_for_testing();
        assert_eq!(processed, 1);
        let events = client.events();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].paths, vec!["SKILL.md", "scripts/run.sh"]);
        ctrl.shutdown();
    }

    #[test]
    fn controller_caps_paths_at_limit() {
        let client = Arc::new(InMemoryNotifyClient::new());
        let ctrl = NotifyController::new(
            client.clone(),
            "/srv/skills",
            Duration::from_millis(50),
            5000,
        );
        for i in 0..MAX_NOTIFY_PATHS + 10 {
            ctrl.observe(
                "alpha",
                Some(Path::new(&format!("file_{i}.txt"))),
                MutationKind::Write,
            );
        }
        ctrl.flush_for_testing();
        let events = client.events();
        assert_eq!(events.len(), 1);
        assert!(
            events[0].paths.is_empty(),
            "exceeding MAX_NOTIFY_PATHS must send empty paths"
        );
        ctrl.shutdown();
    }

    #[test]
    fn controller_notify_failure_is_silent() {
        let client = Arc::new(FailingNotifyClient);
        let ctrl = NotifyController::new(client, "/srv/skills", Duration::from_millis(50), 5000);
        ctrl.observe("alpha", Some(Path::new("SKILL.md")), MutationKind::Write);
        // Must not panic or propagate error
        let processed = ctrl.flush_for_testing();
        assert_eq!(processed, 1);
        ctrl.shutdown();
    }

    #[test]
    fn controller_canonical_root_appears_in_flat_skill_dir() {
        let client = Arc::new(InMemoryNotifyClient::new());
        let ctrl = NotifyController::new(
            client.clone(),
            "/home/user/skills",
            Duration::from_millis(50),
            5000,
        );
        ctrl.observe("weather", Some(Path::new("SKILL.md")), MutationKind::Write);
        ctrl.flush_for_testing();
        let events = client.events();
        assert_eq!(events[0].schema_version, 2);
        assert_eq!(events[0].skill_id, "weather");
        assert_eq!(events[0].canonical_skill_dir, "/home/user/skills/weather");
        ctrl.shutdown();
    }

    #[test]
    fn controller_preserves_hermes_full_skill_id() {
        let client = Arc::new(InMemoryNotifyClient::new());
        let ctrl = NotifyController::new(
            client.clone(),
            "/home/user/skills",
            Duration::from_millis(50),
            5000,
        );
        ctrl.observe(
            "category/weather",
            Some(Path::new("scripts/run.sh")),
            MutationKind::Write,
        );
        ctrl.flush_for_testing();

        let events = client.events();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].skill_id, "category/weather");
        assert_eq!(
            events[0].canonical_skill_dir,
            "/home/user/skills/category/weather"
        );
        assert_eq!(events[0].paths, vec!["scripts/run.sh"]);
        ctrl.shutdown();
    }

    #[test]
    fn eligible_rejects_empty_name() {
        assert!(!is_notify_eligible(""));
    }

    #[test]
    fn eligible_accepts_normal_skill() {
        assert!(is_notify_eligible("alpha"));
        assert!(is_notify_eligible("my-weather-skill"));
    }

    #[test]
    fn request_ids_are_sequential() {
        let id1 = next_request_id();
        let id2 = next_request_id();
        assert!(id1.starts_with("skillfs-"));
        assert!(id2.starts_with("skillfs-"));
        assert_ne!(id1, id2);
    }

    #[test]
    fn no_ambient_runtime_controller_tears_down_on_drop() {
        let start = std::time::Instant::now();
        for _ in 0..8 {
            let client = Arc::new(InMemoryNotifyClient::new());
            let ctrl = NotifyController::new(
                client.clone(),
                "/srv/skills",
                Duration::from_millis(20),
                5000,
            );
            ctrl.observe("alpha", Some(Path::new("SKILL.md")), MutationKind::Write);
            assert_eq!(ctrl.flush_for_testing(), 1);
            let events = client.events();
            assert_eq!(events.len(), 1);
            assert_eq!(events[0].skill_id, "alpha");
            // Drop the Arc — Drop sends Shutdown, the worker returns,
            // the private runtime thread exits cleanly.
            drop(ctrl);
        }
        let elapsed = start.elapsed();
        assert!(
            elapsed < Duration::from_secs(10),
            "8 controller create/drop cycles took {elapsed:?}; \
             a leaked worker thread would have blocked here"
        );
    }

    // -------------------------------------------------------------------
    // N3 Protocol Event integration tests
    // -------------------------------------------------------------------

    use super::super::protocol_events::InMemoryProtocolEventWriter;

    #[test]
    fn protocol_event_written_on_flush() {
        let client = Arc::new(InMemoryNotifyClient::new());
        let writer = Arc::new(InMemoryProtocolEventWriter::new());
        let ctrl = NotifyController::new_with_protocol_writer(
            client.clone(),
            "/srv/skills",
            "/srv/skills",
            Duration::from_millis(50),
            5000,
            writer.clone(),
        );
        ctrl.observe("alpha", Some(Path::new("SKILL.md")), MutationKind::Write);
        ctrl.flush_for_testing();
        assert_eq!(writer.len(), 1);
        let events = writer.events();
        assert_eq!(events[0].schema_version, 1);
        assert_eq!(events[0].skill_name, "alpha");
        assert_eq!(events[0].event_kind, "write");
        assert_eq!(events[0].paths, vec!["SKILL.md"]);
        assert_eq!(events[0].skill_dir, "/srv/skills/alpha");
        assert!(events[0].time.ends_with('Z'), "time must be RFC3339 UTC");
        ctrl.shutdown();
    }

    #[test]
    fn protocol_event_written_even_when_notify_fails() {
        let client = Arc::new(FailingNotifyClient);
        let writer = Arc::new(InMemoryProtocolEventWriter::new());
        let ctrl = NotifyController::new_with_protocol_writer(
            client,
            "/srv/skills",
            "/srv/skills",
            Duration::from_millis(50),
            5000,
            writer.clone(),
        );
        ctrl.observe("alpha", Some(Path::new("SKILL.md")), MutationKind::Write);
        ctrl.flush_for_testing();
        assert_eq!(
            writer.len(),
            1,
            "protocol event must be written even when notify client fails"
        );
        ctrl.shutdown();
    }

    #[test]
    fn protocol_event_debounce_collapses() {
        let client = Arc::new(InMemoryNotifyClient::new());
        let writer = Arc::new(InMemoryProtocolEventWriter::new());
        let ctrl = NotifyController::new_with_protocol_writer(
            client,
            "/srv/skills",
            "/srv/skills",
            Duration::from_millis(50),
            5000,
            writer.clone(),
        );
        for _ in 0..5 {
            ctrl.observe("alpha", Some(Path::new("SKILL.md")), MutationKind::Write);
        }
        ctrl.flush_for_testing();
        assert_eq!(
            writer.len(),
            1,
            "five observations must collapse to one protocol event"
        );
        ctrl.shutdown();
    }

    #[test]
    fn protocol_event_collects_and_deduplicates_paths() {
        let client = Arc::new(InMemoryNotifyClient::new());
        let writer = Arc::new(InMemoryProtocolEventWriter::new());
        let ctrl = NotifyController::new_with_protocol_writer(
            client,
            "/srv/skills",
            "/srv/skills",
            Duration::from_millis(50),
            5000,
            writer.clone(),
        );
        ctrl.observe("alpha", Some(Path::new("SKILL.md")), MutationKind::Write);
        ctrl.observe(
            "alpha",
            Some(Path::new("scripts/run.sh")),
            MutationKind::Create,
        );
        ctrl.observe("alpha", Some(Path::new("SKILL.md")), MutationKind::Write);
        ctrl.flush_for_testing();
        assert_eq!(writer.len(), 1);
        let events = writer.events();
        let mut paths = events[0].paths.clone();
        paths.sort();
        assert_eq!(paths, vec!["SKILL.md", "scripts/run.sh"]);
        ctrl.shutdown();
    }

    #[test]
    fn protocol_event_caps_paths_at_limit() {
        let client = Arc::new(InMemoryNotifyClient::new());
        let writer = Arc::new(InMemoryProtocolEventWriter::new());
        let ctrl = NotifyController::new_with_protocol_writer(
            client,
            "/srv/skills",
            "/srv/skills",
            Duration::from_millis(50),
            5000,
            writer.clone(),
        );
        for i in 0..MAX_NOTIFY_PATHS + 10 {
            ctrl.observe(
                "alpha",
                Some(Path::new(&format!("file_{i}.txt"))),
                MutationKind::Write,
            );
        }
        ctrl.flush_for_testing();
        assert_eq!(writer.len(), 1);
        assert!(
            writer.events()[0].paths.is_empty(),
            "exceeding MAX_NOTIFY_PATHS must send empty paths in protocol event"
        );
        ctrl.shutdown();
    }

    #[test]
    fn protocol_event_empty_paths_for_mkdir() {
        let client = Arc::new(InMemoryNotifyClient::new());
        let writer = Arc::new(InMemoryProtocolEventWriter::new());
        let ctrl = NotifyController::new_with_protocol_writer(
            client,
            "/srv/skills",
            "/srv/skills",
            Duration::from_millis(50),
            5000,
            writer.clone(),
        );
        ctrl.observe("alpha", None, MutationKind::Mkdir);
        ctrl.flush_for_testing();
        assert_eq!(writer.len(), 1);
        assert!(writer.events()[0].paths.is_empty());
        assert_eq!(writer.events()[0].event_kind, "mkdir");
        ctrl.shutdown();
    }

    #[test]
    fn protocol_event_not_written_for_skill_meta() {
        let client = Arc::new(InMemoryNotifyClient::new());
        let writer = Arc::new(InMemoryProtocolEventWriter::new());
        let ctrl = NotifyController::new_with_protocol_writer(
            client,
            "/srv/skills",
            "/srv/skills",
            Duration::from_millis(50),
            5000,
            writer.clone(),
        );
        let accepted = ctrl.observe(
            "alpha",
            Some(Path::new(".skill-meta/manifest.json")),
            MutationKind::Write,
        );
        assert!(!accepted);
        ctrl.flush_for_testing();
        assert!(
            writer.is_empty(),
            ".skill-meta/** must not produce protocol events"
        );
        ctrl.shutdown();
    }

    #[test]
    fn protocol_event_not_written_for_skill_discover() {
        let client = Arc::new(InMemoryNotifyClient::new());
        let writer = Arc::new(InMemoryProtocolEventWriter::new());
        let ctrl = NotifyController::new_with_protocol_writer(
            client,
            "/srv/skills",
            "/srv/skills",
            Duration::from_millis(50),
            5000,
            writer.clone(),
        );
        let accepted = ctrl.observe(
            "skill-discover",
            Some(Path::new("SKILL.md")),
            MutationKind::Write,
        );
        assert!(!accepted);
        ctrl.flush_for_testing();
        assert!(
            writer.is_empty(),
            "skill-discover must not produce protocol events"
        );
        ctrl.shutdown();
    }

    #[test]
    fn protocol_event_not_written_for_lifecycle_reserved() {
        let client = Arc::new(InMemoryNotifyClient::new());
        let writer = Arc::new(InMemoryProtocolEventWriter::new());
        let ctrl = NotifyController::new_with_protocol_writer(
            client,
            "/srv/skills",
            "/srv/skills",
            Duration::from_millis(50),
            5000,
            writer.clone(),
        );
        for name in &[".staging", ".certified", ".quarantine", ".archive"] {
            let accepted = ctrl.observe(name, None, MutationKind::Mkdir);
            assert!(!accepted, "{name} must be filtered");
        }
        ctrl.flush_for_testing();
        assert!(
            writer.is_empty(),
            "lifecycle reserved roots must not produce protocol events"
        );
        ctrl.shutdown();
    }

    #[test]
    fn protocol_event_all_mutation_kinds() {
        let client = Arc::new(InMemoryNotifyClient::new());
        let writer = Arc::new(InMemoryProtocolEventWriter::new());
        let ctrl = NotifyController::new_with_protocol_writer(
            client,
            "/srv/skills",
            "/srv/skills",
            Duration::from_millis(50),
            5000,
            writer.clone(),
        );
        let kinds = [
            (MutationKind::Write, "write"),
            (MutationKind::Create, "create"),
            (MutationKind::Rename, "rename"),
            (MutationKind::Unlink, "unlink"),
            (MutationKind::Rmdir, "rmdir"),
            (MutationKind::SetattrTruncate, "truncate"),
        ];
        for (i, (kind, _label)) in kinds.iter().enumerate() {
            let skill = format!("skill-{i}");
            ctrl.observe(&skill, Some(Path::new("file.txt")), *kind);
        }
        ctrl.flush_for_testing();
        assert_eq!(writer.len(), kinds.len());
        let events = writer.events();
        // HashMap drain order is not deterministic, so check by skill name.
        let mut by_skill: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();
        for e in &events {
            by_skill.insert(e.skill_name.clone(), e.event_kind.clone());
        }
        for (i, (_, label)) in kinds.iter().enumerate() {
            let skill = format!("skill-{i}");
            assert_eq!(
                by_skill.get(&skill).map(|s| s.as_str()),
                Some(*label),
                "{skill} event_kind"
            );
        }
        ctrl.shutdown();
    }

    #[test]
    fn protocol_event_jsonl_file_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("protocol-events.jsonl");
        let jsonl_writer = Arc::new(
            super::super::protocol_events::JsonlProtocolEventWriter::new(&path, 0)
                .expect("open writer"),
        );
        let client = Arc::new(InMemoryNotifyClient::new());
        let ctrl = NotifyController::new_with_protocol_writer(
            client,
            "/srv/skills",
            "/srv/skills",
            Duration::from_millis(50),
            5000,
            jsonl_writer.clone(),
        );
        ctrl.observe("alpha", Some(Path::new("SKILL.md")), MutationKind::Write);
        ctrl.observe("beta", Some(Path::new("lib.rs")), MutationKind::Create);
        ctrl.flush_for_testing();
        // Give the writer thread time to flush; use generous margin for
        // contended CI environments.
        std::thread::sleep(Duration::from_millis(500));
        ctrl.shutdown();

        let body = std::fs::read_to_string(&path).expect("read protocol events file");
        let lines: Vec<&str> = body.lines().collect();
        assert_eq!(lines.len(), 2, "expected 2 JSONL lines, got {body:?}");
        for line in &lines {
            let parsed: serde_json::Value = serde_json::from_str(line).expect("valid JSON");
            assert_eq!(parsed["schemaVersion"], 1);
            assert!(parsed["time"].as_str().unwrap().ends_with('Z'));
            assert!(parsed.get("skillDir").is_some());
            assert!(parsed.get("skillName").is_some());
            assert!(parsed.get("eventKind").is_some());
            assert!(parsed.get("paths").is_some());
        }
        // HashMap drain order is non-deterministic; check by skill name.
        let mut skill_names: Vec<String> = lines
            .iter()
            .map(|l| {
                let v: serde_json::Value = serde_json::from_str(l).unwrap();
                v["skillName"].as_str().unwrap().to_string()
            })
            .collect();
        skill_names.sort();
        assert_eq!(skill_names, vec!["alpha", "beta"]);
    }

    #[test]
    fn noop_protocol_writer_does_not_block_controller() {
        let client = Arc::new(InMemoryNotifyClient::new());
        let ctrl = NotifyController::new(
            client.clone(),
            "/srv/skills",
            Duration::from_millis(50),
            5000,
        );
        ctrl.observe("alpha", Some(Path::new("SKILL.md")), MutationKind::Write);
        ctrl.flush_for_testing();
        // Verify notify still works with default noop writer.
        let events = client.events();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].skill_id, "alpha");
        ctrl.shutdown();
    }

    // -------------------------------------------------------------------
    // A4 Reconcile tests
    // -------------------------------------------------------------------

    #[test]
    fn reconcile_event_kind_label() {
        assert_eq!(NotifyEventKind::Reconcile.as_str(), "reconcile");
    }

    #[test]
    fn emit_startup_reconcile_sends_notify_and_protocol_event() {
        let client = Arc::new(InMemoryNotifyClient::new());
        let writer = Arc::new(InMemoryProtocolEventWriter::new());
        let ctrl = NotifyController::new_with_protocol_writer(
            client.clone(),
            "/srv/skills",
            "/srv/skills",
            Duration::from_millis(50),
            5000,
            writer.clone(),
        );

        let names = vec!["alpha".to_string(), "beta".to_string()];
        let count = ctrl.emit_startup_reconcile(&names);

        assert_eq!(count, 2);

        // Verify notify events.
        let notify_events = client.events();
        assert_eq!(notify_events.len(), 2);
        let mut notify_names: Vec<String> =
            notify_events.iter().map(|e| e.skill_id.clone()).collect();
        notify_names.sort();
        assert_eq!(notify_names, vec!["alpha", "beta"]);
        for e in &notify_events {
            assert_eq!(e.schema_version, 2);
            assert_eq!(e.canonical_skill_dir, format!("/srv/skills/{}", e.skill_id));
            assert_eq!(e.event_kind, "reconcile");
            assert!(e.paths.is_empty());
        }

        // Verify protocol events.
        let proto_events = writer.events();
        assert_eq!(proto_events.len(), 2);
        let mut proto_names: Vec<String> =
            proto_events.iter().map(|e| e.skill_name.clone()).collect();
        proto_names.sort();
        assert_eq!(proto_names, vec!["alpha", "beta"]);
        for e in &proto_events {
            assert_eq!(e.event_kind, "reconcile");
            assert!(e.paths.is_empty());
            assert!(e.reload_outcome.is_none());
        }

        ctrl.shutdown();
    }

    #[test]
    fn emit_startup_reconcile_filters_skill_discover() {
        let client = Arc::new(InMemoryNotifyClient::new());
        let writer = Arc::new(InMemoryProtocolEventWriter::new());
        let ctrl = NotifyController::new_with_protocol_writer(
            client.clone(),
            "/srv/skills",
            "/srv/skills",
            Duration::from_millis(50),
            5000,
            writer.clone(),
        );

        let names = vec![
            "alpha".to_string(),
            "skill-discover".to_string(),
            "beta".to_string(),
        ];
        let count = ctrl.emit_startup_reconcile(&names);

        assert_eq!(count, 2, "skill-discover must be filtered");
        let notify_events = client.events();
        assert_eq!(notify_events.len(), 2);
        assert!(
            notify_events.iter().all(|e| e.skill_id != "skill-discover"),
            "skill-discover must not appear in notify events"
        );

        ctrl.shutdown();
    }

    #[test]
    fn emit_startup_reconcile_filters_lifecycle_roots() {
        let client = Arc::new(InMemoryNotifyClient::new());
        let writer = Arc::new(InMemoryProtocolEventWriter::new());
        let ctrl = NotifyController::new_with_protocol_writer(
            client.clone(),
            "/srv/skills",
            "/srv/skills",
            Duration::from_millis(50),
            5000,
            writer.clone(),
        );

        let names = vec![
            "alpha".to_string(),
            ".staging".to_string(),
            ".certified".to_string(),
            ".quarantine".to_string(),
            ".archive".to_string(),
        ];
        let count = ctrl.emit_startup_reconcile(&names);

        assert_eq!(count, 1, "lifecycle roots must be filtered");
        let notify_events = client.events();
        assert_eq!(notify_events.len(), 1);
        assert_eq!(notify_events[0].skill_id, "alpha");

        ctrl.shutdown();
    }

    #[test]
    fn emit_startup_reconcile_notify_failure_is_silent() {
        let client = Arc::new(FailingNotifyClient);
        let writer = Arc::new(InMemoryProtocolEventWriter::new());
        let ctrl = NotifyController::new_with_protocol_writer(
            client,
            "/srv/skills",
            "/srv/skills",
            Duration::from_millis(50),
            5000,
            writer.clone(),
        );

        let names = vec!["alpha".to_string()];
        let count = ctrl.emit_startup_reconcile(&names);

        assert_eq!(count, 1);
        // Protocol event must still be written even when notify fails.
        assert_eq!(writer.len(), 1);
        assert_eq!(writer.events()[0].event_kind, "reconcile");

        ctrl.shutdown();
    }

    #[test]
    fn emit_startup_reconcile_empty_list() {
        let client = Arc::new(InMemoryNotifyClient::new());
        let ctrl = NotifyController::new(
            client.clone(),
            "/srv/skills",
            Duration::from_millis(50),
            5000,
        );

        let count = ctrl.emit_startup_reconcile(&[]);
        assert_eq!(count, 0);
        assert!(client.is_empty());

        ctrl.shutdown();
    }

    #[test]
    fn emit_startup_reconcile_skill_dir_path() {
        let client = Arc::new(InMemoryNotifyClient::new());
        let writer = Arc::new(InMemoryProtocolEventWriter::new());
        let ctrl = NotifyController::new_with_protocol_writer(
            client.clone(),
            "/home/user/skills",
            "/home/user/skills",
            Duration::from_millis(50),
            5000,
            writer.clone(),
        );

        ctrl.emit_startup_reconcile(&["weather".to_string()]);

        let notify_events = client.events();
        assert_eq!(
            notify_events[0].canonical_skill_dir,
            "/home/user/skills/weather"
        );

        let proto_events = writer.events();
        assert_eq!(proto_events[0].skill_dir, "/home/user/skills/weather");

        ctrl.shutdown();
    }

    // -------------------------------------------------------------------
    // Reconcile retry: error classification
    // -------------------------------------------------------------------

    #[test]
    fn io_kinds_that_mean_the_socket_is_not_ready_are_transient() {
        use std::io::ErrorKind as Kind;
        for kind in [
            Kind::NotFound,
            Kind::ConnectionRefused,
            Kind::ConnectionReset,
            Kind::ConnectionAborted,
            Kind::NotConnected,
            Kind::BrokenPipe,
            Kind::UnexpectedEof,
            Kind::AddrNotAvailable,
            Kind::TimedOut,
            Kind::WouldBlock,
            Kind::Interrupted,
        ] {
            assert_eq!(
                io_retry_class(kind),
                NotifyRetryClass::Transient,
                "{kind:?} must be retryable"
            );
        }
    }

    #[test]
    fn io_kinds_that_mean_misconfiguration_are_permanent() {
        use std::io::ErrorKind as Kind;
        for kind in [
            Kind::PermissionDenied,
            Kind::InvalidData,
            Kind::InvalidInput,
            Kind::Other,
        ] {
            assert_eq!(
                io_retry_class(kind),
                NotifyRetryClass::Permanent,
                "{kind:?} must not be retried"
            );
        }
    }

    #[test]
    fn notify_error_retry_classification_table() {
        let transient: Vec<NotifyError> = vec![
            NotifyError::Timeout,
            NotifyError::EndpointUnavailable("socket missing".to_string()),
            NotifyError::AuthTransport("handshake io".to_string()),
            NotifyError::Connect(std::io::Error::from(std::io::ErrorKind::NotFound)),
            NotifyError::Connect(std::io::Error::from(std::io::ErrorKind::ConnectionRefused)),
            NotifyError::Write(std::io::Error::from(std::io::ErrorKind::BrokenPipe)),
            NotifyError::Read(std::io::Error::from(std::io::ErrorKind::ConnectionReset)),
        ];
        for error in &transient {
            assert!(error.is_transient(), "must be transient: {error}");
        }

        let permanent: Vec<NotifyError> = vec![
            NotifyError::EndpointUntrusted("bad mode".to_string()),
            NotifyError::AuthRejected("proof mismatch".to_string()),
            NotifyError::InvalidResponse {
                body: "not json".to_string(),
            },
            NotifyError::Rejected {
                body: r#"{"ok":false}"#.to_string(),
            },
            NotifyError::Connect(std::io::Error::from(std::io::ErrorKind::PermissionDenied)),
            NotifyError::Write(std::io::Error::other("serialize failed")),
        ];
        for error in &permanent {
            assert!(!error.is_transient(), "must be permanent: {error}");
        }
    }

    #[test]
    fn auth_errors_map_by_variant_not_by_message() {
        use super::super::auth::AuthError;

        // Handshake transport problems: the daemon may be restarting, and
        // the failure is distinguishable from a trust decision.
        for error in [
            AuthError::Timeout,
            AuthError::Io(std::io::Error::from(std::io::ErrorKind::ConnectionReset)),
        ] {
            let mapped = auth_error_to_notify(error);
            assert!(
                matches!(mapped, NotifyError::AuthTransport(_)),
                "expected AuthTransport, got {mapped:?}"
            );
            assert!(mapped.is_transient());
        }

        // Trust decisions and protocol violations: retrying is pointless.
        for error in [
            AuthError::VerificationFailed,
            AuthError::FrameTooLarge(64),
            AuthError::InsecurePermissions,
            AuthError::WrongOwner,
            AuthError::InvalidLength,
            AuthError::NotRegular,
            AuthError::RelativePath,
            AuthError::Io(std::io::Error::from(std::io::ErrorKind::PermissionDenied)),
        ] {
            let mapped = auth_error_to_notify(error);
            assert!(
                matches!(mapped, NotifyError::AuthRejected(_)),
                "expected AuthRejected, got {mapped:?}"
            );
            assert!(!mapped.is_transient());
        }
    }

    #[test]
    fn missing_socket_is_unavailable_but_bad_mode_is_untrusted() {
        use std::os::unix::fs::PermissionsExt;
        use std::os::unix::net::UnixListener;

        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let sock_path = dir.path().join("absent.sock");

        // The daemon has not created the socket yet.
        let result = validate_authenticated_notify_endpoint(&sock_path);
        assert!(
            matches!(result, Err(NotifyError::EndpointUnavailable(_))),
            "absent socket must be retryable, got {result:?}"
        );
        assert!(result.unwrap_err().is_transient());

        // A socket that exists but is world-writable is a trust failure.
        let listener = UnixListener::bind(&sock_path).unwrap();
        std::fs::set_permissions(&sock_path, std::fs::Permissions::from_mode(0o666)).unwrap();
        let result = validate_authenticated_notify_endpoint(&sock_path);
        assert!(
            matches!(result, Err(NotifyError::EndpointUntrusted(_))),
            "permissive socket must be permanent, got {result:?}"
        );
        assert!(!result.unwrap_err().is_transient());
        drop(listener);
    }

    #[test]
    fn missing_parent_directory_is_unavailable() {
        let dir = tempfile::tempdir().unwrap();
        let sock_path = dir.path().join("not-created-yet").join("notify.sock");
        let result = validate_authenticated_notify_endpoint(&sock_path);
        assert!(
            matches!(result, Err(NotifyError::EndpointUnavailable(_))),
            "absent parent directory must be retryable, got {result:?}"
        );
    }

    // -------------------------------------------------------------------
    // Reconcile retry: backoff schedule
    // -------------------------------------------------------------------

    #[test]
    fn retry_delay_grows_exponentially_from_the_base() {
        // jitter_permille = 500 is the midpoint, i.e. no offset.
        assert_eq!(
            reconcile_retry_delay(1, 500),
            Duration::from_millis(RECONCILE_RETRY_BASE_MS)
        );
        assert_eq!(
            reconcile_retry_delay(2, 500),
            Duration::from_millis(RECONCILE_RETRY_BASE_MS * 2)
        );
        assert_eq!(
            reconcile_retry_delay(3, 500),
            Duration::from_millis(RECONCILE_RETRY_BASE_MS * 4)
        );
    }

    #[test]
    fn ambiguous_retry_budget_spans_about_thirty_seconds() {
        let total = (1..RECONCILE_AMBIGUOUS_RETRY_LIMIT)
            .map(|attempts| reconcile_retry_delay(attempts, 500))
            .sum::<Duration>();

        assert_eq!(total, Duration::from_millis(31_750));
    }

    #[test]
    fn retry_delay_is_capped_at_the_maximum_interval() {
        for attempts in [8_u32, 16, 64, 1_000, u32::MAX] {
            let delay = reconcile_retry_delay(attempts, 999);
            assert!(
                delay <= Duration::from_millis(RECONCILE_RETRY_MAX_MS),
                "attempt {attempts} produced {delay:?}, above the cap"
            );
        }
        // Once capped the interval stops growing, so retries continue
        // forever at a bounded rate rather than backing off to never.
        assert_eq!(
            reconcile_retry_delay(1_000, 500),
            Duration::from_millis(RECONCILE_RETRY_MAX_MS)
        );
    }

    #[test]
    fn retry_delay_jitter_stays_within_the_configured_band() {
        // Attempts 1..=7 stay strictly below RECONCILE_RETRY_MAX_MS, so
        // jitter spreads in both directions there.
        for attempts in 1..=7_u32 {
            let center = reconcile_retry_delay(attempts, 500).as_millis() as u64;
            assert!(
                center < RECONCILE_RETRY_MAX_MS,
                "attempt {attempts} is expected to be below the cap"
            );
            let span = center * RECONCILE_RETRY_JITTER_PCT / 100;
            let low = reconcile_retry_delay(attempts, 0).as_millis() as u64;
            let high = reconcile_retry_delay(attempts, 999).as_millis() as u64;
            assert!(low < center, "attempt {attempts}: no spread downward");
            assert!(high > center, "attempt {attempts}: no spread upward");
            assert!(
                low >= center - span && high <= center + span,
                "attempt {attempts}: [{low}, {high}] escapes ±{span} around {center}"
            );
            assert!(low >= 1, "delay must never be zero");
        }
    }

    #[test]
    fn retry_delay_jitter_is_one_sided_at_the_cap() {
        // At the cap the final clamp removes the upward half of the band.
        // Spread survives downward, which is all jitter needs to decorrelate
        // many skills retrying against one daemon.
        let center = reconcile_retry_delay(32, 500).as_millis() as u64;
        assert_eq!(center, RECONCILE_RETRY_MAX_MS);
        let low = reconcile_retry_delay(32, 0).as_millis() as u64;
        let high = reconcile_retry_delay(32, 999).as_millis() as u64;
        let span = RECONCILE_RETRY_MAX_MS * RECONCILE_RETRY_JITTER_PCT / 100;
        assert_eq!(low, RECONCILE_RETRY_MAX_MS - span);
        assert_eq!(high, RECONCILE_RETRY_MAX_MS, "never exceeds the cap");
    }

    #[test]
    fn retry_delay_never_zero_even_with_extreme_jitter() {
        for jitter in [0_u64, 1, 499, 500, 501, 999, 5_000] {
            assert!(reconcile_retry_delay(1, jitter) >= Duration::from_millis(1));
        }
    }

    #[test]
    fn random_permille_stays_in_range() {
        for _ in 0..64 {
            assert!(random_permille() < 1000);
        }
    }

    // -------------------------------------------------------------------
    // Reconcile retry: convergence
    // -------------------------------------------------------------------

    /// Worker-free controller: these tests assert on `pending_len`, attempt
    /// counts, and backoff state, so they must be the only thing draining
    /// the pending map. A live worker would race every one of them.
    fn retry_controller(client: Arc<dyn NotifyClient>) -> Arc<NotifyController> {
        NotifyController::new_for_testing_without_worker(
            client,
            "/srv/skills",
            "/srv/skills",
            Duration::from_millis(50),
            5000,
            Arc::new(NoopProtocolEventWriter),
            None,
        )
    }

    struct SequencedNotifyClient {
        failures: Mutex<std::collections::VecDeque<ScriptedNotifyFailure>>,
        attempts: AtomicU64,
        events: Mutex<Vec<CapturedNotify>>,
    }

    impl SequencedNotifyClient {
        fn new(failures: impl IntoIterator<Item = ScriptedNotifyFailure>) -> Self {
            Self {
                failures: Mutex::new(failures.into_iter().collect()),
                attempts: AtomicU64::new(0),
                events: Mutex::new(Vec::new()),
            }
        }

        fn attempts(&self) -> u64 {
            self.attempts.load(Ordering::Relaxed)
        }
    }

    impl NotifyClient for SequencedNotifyClient {
        fn send(&self, event: &NotifyChangeEvent) -> Result<(), NotifyError> {
            self.attempts.fetch_add(1, Ordering::Relaxed);
            if let Some(failure) = self.failures.lock().pop_front() {
                return Err(failure.to_error());
            }
            self.events.lock().push(CapturedNotify {
                schema_version: event.params.schema_version,
                skill_id: event.params.skill_id.clone(),
                event_kind: event.params.event_kind.clone(),
                paths: event.params.paths.clone(),
                canonical_skill_dir: event.params.canonical_skill_dir.clone(),
            });
            Ok(())
        }
    }

    #[test]
    fn reconcile_retries_until_the_daemon_acks() {
        let client = Arc::new(ScriptedNotifyClient::new(
            ScriptedNotifyFailure::ConnectionRefused,
            2,
        ));
        let ctrl = retry_controller(client.clone());

        assert_eq!(ctrl.enqueue_startup_reconcile(&["alpha".to_string()]), 1);
        assert_eq!(ctrl.pending_len(), 1, "reconcile must be queued, not sent");

        let attempts = ctrl.flush_until_delivered_for_testing(8);

        assert_eq!(attempts, 3, "two failures then one success");
        assert_eq!(client.attempts(), 3);
        assert_eq!(
            ctrl.pending_len(),
            0,
            "pending must drain once the daemon ACKs"
        );

        let delivered = client.events();
        assert_eq!(delivered.len(), 1);
        assert_eq!(delivered[0].skill_id, "alpha");
        assert_eq!(delivered[0].event_kind, "reconcile");
        assert!(
            delivered[0].paths.is_empty(),
            "reconcile is a full rescan and carries no paths"
        );

        let metrics = ctrl.metrics();
        assert_eq!(metrics.attempted, 3);
        assert_eq!(metrics.succeeded, 1);
        assert_eq!(metrics.failed, 2);
        assert_eq!(metrics.pending, 0);
        ctrl.shutdown();
    }

    #[test]
    fn reconcile_retries_when_the_socket_appears_only_later() {
        let client = Arc::new(ScriptedNotifyClient::new(
            ScriptedNotifyFailure::SocketMissing,
            3,
        ));
        let ctrl = retry_controller(client.clone());

        ctrl.enqueue_startup_reconcile(&["alpha".to_string()]);
        let attempts = ctrl.flush_until_delivered_for_testing(8);

        assert_eq!(attempts, 4);
        assert_eq!(ctrl.pending_len(), 0);
        assert_eq!(client.events().len(), 1);
        ctrl.shutdown();
    }

    #[test]
    fn transient_failure_keeps_the_skill_pending_while_counting_a_failure() {
        let client = Arc::new(ScriptedNotifyClient::always_failing(
            ScriptedNotifyFailure::ConnectionRefused,
        ));
        let ctrl = retry_controller(client.clone());

        ctrl.enqueue_startup_reconcile(&["alpha".to_string()]);
        ctrl.flush_for_testing();

        // A transiently failed skill contributes to `failed` *and* is still
        // counted in `pending` — the two are independent by design.
        let metrics = ctrl.metrics();
        assert_eq!(metrics.attempted, 1);
        assert_eq!(metrics.succeeded, 0);
        assert_eq!(metrics.failed, 1);
        assert_eq!(metrics.pending, 1);
        ctrl.shutdown();
    }

    #[test]
    fn multiple_skills_converge_independently() {
        // beta needs three attempts; alpha and gamma succeed at once. The
        // shared client counter makes the total attempt count the sum.
        let client = Arc::new(ScriptedNotifyClient::new(
            ScriptedNotifyFailure::ConnectionRefused,
            2,
        ));
        let ctrl = retry_controller(client.clone());

        let names = vec!["alpha".to_string(), "beta".to_string(), "gamma".to_string()];
        assert_eq!(ctrl.enqueue_startup_reconcile(&names), 3);
        assert_eq!(ctrl.pending_len(), 3);

        ctrl.flush_until_delivered_for_testing(16);

        assert_eq!(ctrl.pending_len(), 0, "every skill must converge");
        let mut delivered: Vec<String> = client
            .events()
            .into_iter()
            .map(|event| event.skill_id)
            .collect();
        delivered.sort();
        assert_eq!(delivered, vec!["alpha", "beta", "gamma"]);
        assert_eq!(ctrl.metrics().succeeded, 3);
        ctrl.shutdown();
    }

    #[test]
    fn repeated_reconcile_enqueue_dedups_by_skill_identity() {
        let client = Arc::new(ScriptedNotifyClient::new(
            ScriptedNotifyFailure::ConnectionRefused,
            0,
        ));
        let ctrl = retry_controller(client.clone());

        assert_eq!(
            ctrl.enqueue_startup_reconcile(&["category/alpha".to_string()]),
            1
        );
        for _ in 0..4 {
            assert_eq!(
                ctrl.enqueue_startup_reconcile(&["category/alpha".to_string()]),
                0,
                "an existing identity is merged rather than newly queued"
            );
        }
        assert_eq!(
            ctrl.pending_len(),
            1,
            "five enqueues of the same identity must collapse to one entry"
        );

        let attempts = ctrl.flush_until_delivered_for_testing(4);
        assert_eq!(attempts, 1, "dedup must yield a single delivery");
        assert_eq!(client.events()[0].skill_id, "category/alpha");
        ctrl.shutdown();
    }

    #[test]
    fn untrusted_endpoint_is_attempted_exactly_once() {
        let client = Arc::new(ScriptedNotifyClient::always_failing(
            ScriptedNotifyFailure::UntrustedEndpoint,
        ));
        let ctrl = retry_controller(client.clone());

        ctrl.enqueue_startup_reconcile(&["alpha".to_string()]);
        let attempts = ctrl.flush_until_delivered_for_testing(16);

        assert_eq!(attempts, 1, "a trust failure must not be retried");
        assert_eq!(client.attempts(), 1);
        assert_eq!(
            ctrl.pending_len(),
            0,
            "a permanently failed reconcile must not linger in pending"
        );
        let metrics = ctrl.metrics();
        assert_eq!(metrics.attempted, 1);
        assert_eq!(metrics.failed, 1);
        assert_eq!(metrics.succeeded, 0);
        ctrl.shutdown();
    }

    #[test]
    fn pre_delivery_reconcile_failure_skips_reload_poll() {
        use super::super::activation_reload::ActivationReloadController;

        let dir = tempfile::tempdir().unwrap();
        let resolver = Arc::new(super::super::active::ActiveSkillResolver::new(dir.path()));
        let reload = Arc::new(ActivationReloadController::new(
            dir.path(),
            resolver,
            Duration::from_millis(5),
            Duration::from_millis(20),
        ));
        let writer = Arc::new(InMemoryProtocolEventWriter::new());
        let client = Arc::new(ScriptedNotifyClient::always_failing(
            ScriptedNotifyFailure::UntrustedEndpoint,
        ));
        let ctrl = NotifyController::new_for_testing_without_worker(
            client,
            dir.path(),
            dir.path(),
            Duration::from_millis(5),
            20,
            writer.clone(),
            Some(reload),
        );

        ctrl.enqueue_startup_reconcile(&["alpha".to_string()]);
        ctrl.flush_until_delivered_for_testing(1);

        let events = writer.events();
        assert_eq!(events.len(), 1, "only the reconcile event is emitted");
        assert_eq!(events[0].event_kind, "reconcile");
        assert!(
            events.iter().all(|event| event.event_kind != "reload"),
            "a request rejected before delivery cannot trigger activation"
        );
        ctrl.shutdown();
    }

    #[test]
    fn post_delivery_reconcile_failure_keeps_reload_poll() {
        use super::super::activation_reload::ActivationReloadController;

        let dir = tempfile::tempdir().unwrap();
        let resolver = Arc::new(super::super::active::ActiveSkillResolver::new(dir.path()));
        let reload = Arc::new(ActivationReloadController::new(
            dir.path(),
            resolver,
            Duration::from_millis(5),
            Duration::from_millis(20),
        ));
        let writer = Arc::new(InMemoryProtocolEventWriter::new());
        let client = Arc::new(ScriptedNotifyClient::always_failing(
            ScriptedNotifyFailure::DaemonRejected,
        ));
        let ctrl = NotifyController::new_for_testing_without_worker(
            client,
            dir.path(),
            dir.path(),
            Duration::from_millis(5),
            20,
            writer.clone(),
            Some(reload),
        );

        ctrl.enqueue_startup_reconcile(&["alpha".to_string()]);
        ctrl.flush_until_delivered_for_testing(1);

        assert!(
            writer
                .events()
                .iter()
                .any(|event| event.event_kind == "reload"),
            "a daemon response proves the request arrived, so reload still polls"
        );
        ctrl.shutdown();
    }

    #[test]
    fn wrong_hmac_key_does_not_produce_a_retry_storm() {
        let client = Arc::new(ScriptedNotifyClient::always_failing(
            ScriptedNotifyFailure::AuthRejected,
        ));
        let ctrl = retry_controller(client.clone());

        ctrl.enqueue_startup_reconcile(&["alpha".to_string(), "beta".to_string()]);
        ctrl.flush_until_delivered_for_testing(32);

        assert_eq!(
            client.attempts(),
            2,
            "one attempt per skill, no retries on a rejected proof"
        );
        assert_eq!(ctrl.pending_len(), 0);
        ctrl.shutdown();
    }

    #[test]
    fn daemon_rejection_is_not_retried() {
        let client = Arc::new(ScriptedNotifyClient::always_failing(
            ScriptedNotifyFailure::DaemonRejected,
        ));
        let ctrl = retry_controller(client.clone());

        ctrl.enqueue_startup_reconcile(&["alpha".to_string()]);
        ctrl.flush_until_delivered_for_testing(16);

        assert_eq!(client.attempts(), 1);
        assert_eq!(ctrl.pending_len(), 0);
        ctrl.shutdown();
    }

    #[test]
    fn shutdown_stops_further_reconcile_attempts() {
        let client = Arc::new(ScriptedNotifyClient::always_failing(
            ScriptedNotifyFailure::ConnectionRefused,
        ));
        let ctrl = retry_controller(client.clone());

        ctrl.enqueue_startup_reconcile(&["alpha".to_string()]);
        ctrl.flush_for_testing();
        assert_eq!(client.attempts(), 1);
        assert_eq!(ctrl.pending_len(), 1, "requeued for another try");

        ctrl.shutdown();

        // After shutdown a failed delivery must not be requeued, so the
        // next drain empties the map instead of refilling it.
        let attempts = ctrl.flush_until_delivered_for_testing(16);
        assert_eq!(attempts, 1, "exactly the already-queued entry, then stop");
        assert_eq!(client.attempts(), 2);
        assert_eq!(
            ctrl.pending_len(),
            0,
            "shutdown must forbid requeueing failed requests"
        );
    }

    #[test]
    fn a_worker_that_never_reaches_the_daemon_still_lets_the_controller_drop() {
        // The retry loop must not hold an Arc<NotifyController>, otherwise
        // Drop could never fire and the private runtime thread would leak.
        let start = std::time::Instant::now();
        for _ in 0..4 {
            let client = Arc::new(ScriptedNotifyClient::always_failing(
                ScriptedNotifyFailure::ConnectionRefused,
            ));
            let ctrl = NotifyController::new(client, "/srv/skills", Duration::from_millis(5), 5000);
            ctrl.enqueue_startup_reconcile(&["alpha".to_string()]);
            std::thread::sleep(Duration::from_millis(30));
            drop(ctrl);
        }
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "a leaked retry loop would have blocked teardown"
        );
    }

    // -------------------------------------------------------------------
    // Reconcile retry: dedup against a concurrent mutation
    // -------------------------------------------------------------------

    #[test]
    fn requeued_reconcile_does_not_overwrite_a_concurrent_mutation() {
        let client = Arc::new(ScriptedNotifyClient::always_failing(
            ScriptedNotifyFailure::ConnectionRefused,
        ));
        let ctrl = retry_controller(client.clone());

        // Reconcile is drained and fails...
        ctrl.enqueue_startup_reconcile(&["alpha".to_string()]);
        // ...and a mutation for the same skill lands before the requeue.
        ctrl.observe("alpha", Some(Path::new("SKILL.md")), MutationKind::Write);
        ctrl.flush_for_testing();

        // Neither request may be lost: the merged entry stays a reconcile,
        // which is a full rescan and therefore already covers SKILL.md.
        assert_eq!(ctrl.pending_len(), 1, "merged into one entry, not dropped");
        let pending = ctrl.inner.pending.lock();
        let entry = pending.get("alpha").expect("alpha still pending");
        assert_eq!(entry.event_kind, NotifyEventKind::Reconcile);
        assert!(
            entry.paths.is_empty(),
            "a reconcile carries no paths; it rescans the whole skill"
        );
        assert_eq!(entry.attempts, 1, "retry state must survive the merge");
        drop(pending);
        ctrl.shutdown();
    }

    #[test]
    fn a_mutation_must_not_downgrade_a_queued_reconcile() {
        let client = Arc::new(ScriptedNotifyClient::new(
            ScriptedNotifyFailure::ConnectionRefused,
            0,
        ));
        let ctrl = retry_controller(client.clone());

        ctrl.enqueue_startup_reconcile(&["alpha".to_string()]);
        ctrl.observe("alpha", Some(Path::new("SKILL.md")), MutationKind::Write);
        ctrl.observe(
            "alpha",
            Some(Path::new("scripts/run.sh")),
            MutationKind::Create,
        );

        assert_eq!(ctrl.pending_len(), 1);
        assert_eq!(ctrl.flush_until_delivered_for_testing(4), 1);

        let delivered = client.events();
        assert_eq!(delivered.len(), 1);
        assert_eq!(
            delivered[0].event_kind, "reconcile",
            "the queued reconcile must win over later mutation kinds"
        );
        assert!(delivered[0].paths.is_empty());
        ctrl.shutdown();
    }

    #[test]
    fn a_mutation_does_not_disturb_a_backed_off_reconcile_deadline() {
        let client = Arc::new(ScriptedNotifyClient::always_failing(
            ScriptedNotifyFailure::ConnectionRefused,
        ));
        let ctrl = retry_controller(client.clone());

        ctrl.enqueue_startup_reconcile(&["alpha".to_string()]);
        ctrl.flush_for_testing();
        let backed_off = ctrl.inner.pending.lock().get("alpha").unwrap().fire_at;

        ctrl.observe("alpha", Some(Path::new("SKILL.md")), MutationKind::Write);
        let after_mutation = ctrl.inner.pending.lock().get("alpha").unwrap().fire_at;

        // Pulling the deadline forward would defeat the backoff, and pushing
        // it out would delay the reconcile. The backoff exists because the
        // daemon is unreachable, so an earlier attempt would fail anyway.
        assert_eq!(
            after_mutation, backed_off,
            "a mutation must leave a backed-off reconcile deadline alone"
        );
        ctrl.shutdown();
    }

    #[test]
    fn mutation_debounce_stays_trailing_edge() {
        let client = Arc::new(ScriptedNotifyClient::new(
            ScriptedNotifyFailure::ConnectionRefused,
            0,
        ));
        let debounce = Duration::from_millis(200);
        let ctrl = NotifyController::new_for_testing_without_worker(
            client.clone(),
            "/srv/skills",
            "/srv/skills",
            debounce,
            5000,
            Arc::new(NoopProtocolEventWriter),
            None,
        );

        ctrl.observe("alpha", Some(Path::new("a.txt")), MutationKind::Write);
        let first = ctrl.inner.pending.lock().get("alpha").unwrap().fire_at;

        // A second mutation must push the deadline out, so a continuous
        // write burst dispatches once after it goes quiet rather than
        // firing on a deadline anchored at the first write.
        std::thread::sleep(Duration::from_millis(20));
        ctrl.observe("alpha", Some(Path::new("b.txt")), MutationKind::Write);
        let second = ctrl.inner.pending.lock().get("alpha").unwrap().fire_at;

        assert!(
            second > first,
            "each mutation must reset the debounce deadline (trailing edge)"
        );
        ctrl.shutdown();
    }

    // -------------------------------------------------------------------
    // Reconcile retry: ambiguous authentication outcome
    // -------------------------------------------------------------------

    #[test]
    fn handshake_eof_is_ambiguous_not_transient() {
        use super::super::auth::AuthError;

        // The daemon closes the connection both when it is restarting and
        // when it has refused our client proof; `read_frame` maps that EOF
        // to `InvalidFrame`. It must be bounded-retry, not infinite-retry.
        let mapped = auth_error_to_notify(AuthError::InvalidFrame);
        assert!(
            matches!(mapped, NotifyError::AuthInconclusive(_)),
            "expected AuthInconclusive, got {mapped:?}"
        );
        assert_eq!(mapped.retry_class(), NotifyRetryClass::Ambiguous);
        assert!(
            mapped.is_retryable(),
            "a daemon restart must still converge"
        );
        assert!(
            !mapped.is_transient(),
            "a wrong key must not retry without a limit"
        );
    }

    #[test]
    fn an_inconclusive_handshake_retries_but_gives_up_at_the_limit() {
        let client = Arc::new(ScriptedNotifyClient::always_failing(
            ScriptedNotifyFailure::HandshakeEof,
        ));
        let ctrl = retry_controller(client.clone());

        ctrl.enqueue_startup_reconcile(&["alpha".to_string()]);
        let attempts = ctrl.flush_until_delivered_for_testing(64);

        assert_eq!(
            attempts, RECONCILE_AMBIGUOUS_RETRY_LIMIT as usize,
            "an ambiguous failure must retry, but only up to the limit"
        );
        assert_eq!(
            client.attempts(),
            u64::from(RECONCILE_AMBIGUOUS_RETRY_LIMIT)
        );
        assert_eq!(
            ctrl.pending_len(),
            0,
            "the entry must be dropped once the limit is reached"
        );
        ctrl.shutdown();
    }

    #[test]
    fn a_wrong_key_that_recovers_before_the_limit_still_converges() {
        // A daemon restarting mid-handshake looks identical to a wrong key,
        // so the bounded retry must be enough to ride the restart out.
        let client = Arc::new(ScriptedNotifyClient::new(
            ScriptedNotifyFailure::HandshakeEof,
            RECONCILE_AMBIGUOUS_RETRY_LIMIT - 1,
        ));
        let ctrl = retry_controller(client.clone());

        ctrl.enqueue_startup_reconcile(&["alpha".to_string()]);
        ctrl.flush_until_delivered_for_testing(64);

        assert_eq!(ctrl.pending_len(), 0);
        assert_eq!(
            ctrl.metrics().succeeded,
            1,
            "must converge before the limit"
        );
        assert_eq!(client.events().len(), 1);
        ctrl.shutdown();
    }

    #[test]
    fn transient_failures_do_not_consume_the_ambiguous_budget() {
        let failures = std::iter::repeat_n(ScriptedNotifyFailure::ConnectionRefused, 6).chain(
            std::iter::repeat_n(
                ScriptedNotifyFailure::HandshakeEof,
                RECONCILE_AMBIGUOUS_RETRY_LIMIT as usize - 1,
            ),
        );
        let client = Arc::new(SequencedNotifyClient::new(failures));
        let ctrl = retry_controller(client.clone());

        ctrl.enqueue_startup_reconcile(&["alpha".to_string()]);
        ctrl.flush_until_delivered_for_testing(64);

        assert_eq!(
            client.attempts(),
            6 + u64::from(RECONCILE_AMBIGUOUS_RETRY_LIMIT),
            "transient failures, the bounded ambiguous budget, then ACK"
        );
        assert_eq!(ctrl.metrics().succeeded, 1);
        assert_eq!(ctrl.pending_len(), 0);
        ctrl.shutdown();
    }

    #[test]
    fn an_inconclusive_handshake_uses_one_endpoint_wide_budget() {
        let client = Arc::new(ScriptedNotifyClient::always_failing(
            ScriptedNotifyFailure::HandshakeEof,
        ));
        let ctrl = retry_controller(client.clone());

        ctrl.enqueue_startup_reconcile(&[
            "alpha".to_string(),
            "beta".to_string(),
            "gamma".to_string(),
        ]);
        ctrl.flush_until_delivered_for_testing(256);

        assert_eq!(
            client.attempts(),
            u64::from(RECONCILE_AMBIGUOUS_RETRY_LIMIT),
            "the shared endpoint budget must not scale with Skill count"
        );
        assert_eq!(ctrl.pending_len(), 0);
        ctrl.shutdown();
    }

    #[test]
    fn endpoint_ack_releases_every_deferred_skill() {
        let client = Arc::new(ScriptedNotifyClient::new(
            ScriptedNotifyFailure::HandshakeEof,
            RECONCILE_AMBIGUOUS_RETRY_LIMIT - 1,
        ));
        let ctrl = retry_controller(client.clone());

        ctrl.enqueue_startup_reconcile(&["alpha".to_string(), "beta".to_string()]);
        ctrl.flush_until_delivered_for_testing(64);

        assert_eq!(client.events().len(), 2);
        assert_eq!(ctrl.metrics().succeeded, 2);
        assert_eq!(ctrl.pending_len(), 0);
        assert_eq!(
            ctrl.inner.endpoint_retry.lock().ambiguous_failures,
            0,
            "an ACK proves the shared endpoint is healthy"
        );
        ctrl.shutdown();
    }

    #[test]
    fn a_new_reconcile_cycle_can_probe_after_exhaustion() {
        let client = Arc::new(ScriptedNotifyClient::new(
            ScriptedNotifyFailure::HandshakeEof,
            RECONCILE_AMBIGUOUS_RETRY_LIMIT,
        ));
        let ctrl = retry_controller(client.clone());

        ctrl.enqueue_startup_reconcile(&["alpha".to_string(), "beta".to_string()]);
        ctrl.flush_until_delivered_for_testing(64);
        assert_eq!(ctrl.pending_len(), 0);
        assert!(ctrl.inner.endpoint_retry.lock().exhausted);

        assert_eq!(ctrl.enqueue_startup_reconcile(&["alpha".to_string()]), 1);
        ctrl.flush_until_delivered_for_testing(4);

        assert_eq!(ctrl.metrics().succeeded, 1);
        assert!(!ctrl.inner.endpoint_retry.lock().exhausted);
        ctrl.shutdown();
    }

    #[test]
    fn reopening_the_gate_cannot_resume_an_abandoned_drained_batch() {
        let client = Arc::new(ScriptedNotifyClient::new(
            ScriptedNotifyFailure::HandshakeEof,
            RECONCILE_AMBIGUOUS_RETRY_LIMIT,
        ));
        let ctrl = retry_controller(client.clone());

        ctrl.enqueue_startup_reconcile(&["alpha".to_string(), "beta".to_string()]);
        let mut initial_batch = ctrl
            .inner
            .drain_due(Instant::now() + Duration::from_secs(1));
        let old_beta = initial_batch
            .iter()
            .position(|state| state.skill_id == "beta")
            .map(|index| initial_batch.swap_remove(index))
            .expect("beta was drained into the old batch");
        let alpha = initial_batch.pop().expect("alpha was drained");

        // Hold beta outside `pending` while alpha consumes the shared
        // endpoint budget, matching a worker batch drained before the gate
        // was exhausted.
        ctrl.inner.dispatch_one(alpha);
        for _ in 1..RECONCILE_AMBIGUOUS_RETRY_LIMIT {
            let probe = ctrl
                .inner
                .drain_due(Instant::now() + Duration::from_secs(86_400))
                .pop()
                .expect("the next endpoint probe was requeued");
            ctrl.inner.dispatch_one(probe);
        }
        assert_eq!(
            client.attempts(),
            u64::from(RECONCILE_AMBIGUOUS_RETRY_LIMIT)
        );

        // A new cycle reopens the gate, but beta still belongs to the old
        // generation and must not become an extra authentication attempt.
        ctrl.enqueue_startup_reconcile(&["gamma".to_string()]);
        ctrl.inner.dispatch_one(old_beta);
        assert_eq!(
            client.attempts(),
            u64::from(RECONCILE_AMBIGUOUS_RETRY_LIMIT)
        );

        ctrl.flush_until_delivered_for_testing(4);
        assert_eq!(client.events().len(), 1);
        assert_eq!(client.events()[0].skill_id, "gamma");
        ctrl.shutdown();
    }

    #[test]
    fn exhausting_an_old_cycle_does_not_delete_a_new_cycles_pending_work() {
        let ctrl = retry_controller(Arc::new(InMemoryNotifyClient::new()));
        ctrl.inner.merge_pending(NotifyPendingState {
            skill_id: "old".to_string(),
            event_kind: NotifyEventKind::Reconcile,
            paths: HashSet::new(),
            fire_at: Instant::now(),
            attempts: 4,
            reconcile_generation: 0,
        });
        ctrl.inner.merge_pending(NotifyPendingState {
            skill_id: "new".to_string(),
            event_kind: NotifyEventKind::Reconcile,
            paths: HashSet::new(),
            fire_at: Instant::now(),
            attempts: 0,
            reconcile_generation: 1,
        });

        // Models a new enqueue landing after the old generation marks the
        // gate exhausted but before its pending-map cleanup acquires the
        // lock. Cleanup must be scoped to the exhausted generation.
        ctrl.inner.abandon_reconcile_generation(0);

        let pending = ctrl.inner.pending.lock();
        assert!(!pending.contains_key("old"));
        assert_eq!(pending.get("new").unwrap().reconcile_generation, 1);
        drop(pending);
        ctrl.shutdown();
    }

    #[test]
    fn mutation_failures_are_still_best_effort_and_never_requeued() {
        let client = Arc::new(ScriptedNotifyClient::always_failing(
            ScriptedNotifyFailure::ConnectionRefused,
        ));
        let ctrl = retry_controller(client.clone());

        ctrl.observe("alpha", Some(Path::new("SKILL.md")), MutationKind::Write);
        assert_eq!(ctrl.flush_for_testing(), 1);

        assert_eq!(client.attempts(), 1);
        assert_eq!(
            ctrl.pending_len(),
            0,
            "retry is scoped to reconcile; mutations keep best-effort semantics"
        );
        ctrl.shutdown();
    }

    #[test]
    fn enqueue_startup_reconcile_filters_ineligible_skills() {
        let client = Arc::new(ScriptedNotifyClient::new(
            ScriptedNotifyFailure::ConnectionRefused,
            0,
        ));
        let ctrl = retry_controller(client.clone());

        let names = vec![
            "alpha".to_string(),
            "skill-discover".to_string(),
            ".staging".to_string(),
            ".certified".to_string(),
            ".quarantine".to_string(),
            ".archive".to_string(),
            String::new(),
        ];
        assert_eq!(ctrl.enqueue_startup_reconcile(&names), 1);
        assert_eq!(ctrl.pending_len(), 1);

        ctrl.flush_until_delivered_for_testing(4);
        let delivered = client.events();
        assert_eq!(delivered.len(), 1);
        assert_eq!(delivered[0].skill_id, "alpha");
        ctrl.shutdown();
    }

    #[test]
    fn enqueue_startup_reconcile_empty_list_queues_nothing() {
        let client = Arc::new(InMemoryNotifyClient::new());
        let ctrl = retry_controller(client.clone());
        assert_eq!(ctrl.enqueue_startup_reconcile(&[]), 0);
        assert_eq!(ctrl.pending_len(), 0);
        assert_eq!(ctrl.metrics(), NotifyMetricsSnapshot::default());
        ctrl.shutdown();
    }

    #[test]
    fn queued_reconcile_writes_a_protocol_event_per_attempt() {
        let client = Arc::new(ScriptedNotifyClient::new(
            ScriptedNotifyFailure::ConnectionRefused,
            2,
        ));
        let writer = Arc::new(InMemoryProtocolEventWriter::new());
        let ctrl = NotifyController::new_with_protocol_writer(
            client,
            "/srv/skills",
            "/srv/skills",
            Duration::from_secs(3600),
            5000,
            writer.clone(),
        );

        ctrl.enqueue_startup_reconcile(&["alpha".to_string()]);
        ctrl.flush_until_delivered_for_testing(8);

        // The protocol event log is append-only and records every attempt,
        // so an operator can see the two failures before the ACK.
        assert_eq!(writer.len(), 3);
        for event in writer.events() {
            assert_eq!(event.event_kind, "reconcile");
            assert_eq!(event.skill_name, "alpha");
            assert!(event.paths.is_empty());
            assert_eq!(event.skill_dir, "/srv/skills/alpha");
        }
        ctrl.shutdown();
    }

    // -------------------------------------------------------------------
    // A4 Reload outcome protocol event tests
    // -------------------------------------------------------------------

    // -------------------------------------------------------------------
    // Response size limit tests
    // -------------------------------------------------------------------

    #[test]
    fn unix_socket_client_accepts_normal_response() {
        use std::os::unix::net::UnixListener;

        let dir = tempfile::tempdir().unwrap();
        let sock_path = dir.path().join("test.sock");
        let listener = UnixListener::bind(&sock_path).unwrap();

        let client = UnixSocketNotifyClient::new(&sock_path, Duration::from_secs(5));
        let event = NotifyChangeEvent::new(
            "/srv/skills/alpha",
            "alpha",
            NotifyEventKind::Write,
            vec!["SKILL.md".to_string()],
            5000,
        );

        let handle = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(&stream);
            let mut _req = String::new();
            reader.read_line(&mut _req).unwrap();
            use std::io::Write;
            let mut writer = std::io::BufWriter::new(&stream);
            writer
                .write_all(br#"{"ok":true,"data":{"schemaVersion":2,"accepted":true}}"#)
                .unwrap();
            writer.write_all(b"\n").unwrap();
            writer.flush().unwrap();
        });

        let result = client.send(&event);
        handle.join().unwrap();
        assert!(
            result.is_ok(),
            "normal response must be accepted: {result:?}"
        );
    }

    #[test]
    fn unauthenticated_notify_eof_before_ack_is_transient() {
        use std::os::unix::net::UnixListener;

        let dir = tempfile::tempdir().unwrap();
        let sock_path = dir.path().join("test.sock");
        let listener = UnixListener::bind(&sock_path).unwrap();
        let client = UnixSocketNotifyClient::new(&sock_path, Duration::from_secs(5));
        let event = NotifyChangeEvent::new(
            "/srv/skills/alpha",
            "alpha",
            NotifyEventKind::Reconcile,
            vec![],
            5000,
        );

        let handle = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut request = String::new();
            BufReader::new(&stream).read_line(&mut request).unwrap();
        });

        let result = client.send(&event);
        handle.join().unwrap();
        assert!(
            matches!(
                result,
                Err(NotifyError::Read(ref error))
                    if error.kind() == std::io::ErrorKind::UnexpectedEof
            ),
            "EOF before an acknowledgement must be retryable: {result:?}"
        );
    }

    #[test]
    fn unauthenticated_notify_truncated_ack_is_transient() {
        use std::os::unix::net::UnixListener;

        let dir = tempfile::tempdir().unwrap();
        let sock_path = dir.path().join("test.sock");
        let listener = UnixListener::bind(&sock_path).unwrap();
        let client = UnixSocketNotifyClient::new(&sock_path, Duration::from_secs(5));
        let event = NotifyChangeEvent::new(
            "/srv/skills/alpha",
            "alpha",
            NotifyEventKind::Reconcile,
            vec![],
            5000,
        );

        let handle = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut request = String::new();
            BufReader::new(&stream).read_line(&mut request).unwrap();
            let mut writer = std::io::BufWriter::new(&stream);
            writer.write_all(br#"{"ok":true"#).unwrap();
            writer.flush().unwrap();
        });

        let result = client.send(&event);
        handle.join().unwrap();
        assert!(
            matches!(
                result,
                Err(NotifyError::Read(ref error))
                    if error.kind() == std::io::ErrorKind::UnexpectedEof
            ),
            "a truncated acknowledgement must be retryable: {result:?}"
        );
    }

    #[test]
    fn unauthenticated_notify_complete_malformed_ack_is_permanent() {
        use std::os::unix::net::UnixListener;

        let dir = tempfile::tempdir().unwrap();
        let sock_path = dir.path().join("test.sock");
        let listener = UnixListener::bind(&sock_path).unwrap();
        let client = UnixSocketNotifyClient::new(&sock_path, Duration::from_secs(5));
        let event = NotifyChangeEvent::new(
            "/srv/skills/alpha",
            "alpha",
            NotifyEventKind::Reconcile,
            vec![],
            5000,
        );

        let handle = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut request = String::new();
            BufReader::new(&stream).read_line(&mut request).unwrap();
            let mut writer = std::io::BufWriter::new(&stream);
            writer.write_all(b"not-json\n").unwrap();
            writer.flush().unwrap();
        });

        let result = client.send(&event);
        handle.join().unwrap();
        assert!(
            matches!(result, Err(NotifyError::InvalidResponse { .. })),
            "a complete malformed acknowledgement remains permanent: {result:?}"
        );
    }

    #[test]
    fn authenticated_unix_socket_client_handshakes_before_notify() {
        use std::os::unix::fs::PermissionsExt;
        use std::os::unix::net::UnixListener;

        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let sock_path = dir.path().join("test.sock");
        let key_path = dir.path().join("key");
        std::fs::write(&key_path, [5_u8; 32]).unwrap();
        std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let listener = UnixListener::bind(&sock_path).unwrap();
        std::fs::set_permissions(&sock_path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let client = UnixSocketNotifyClient::new_authenticated(
            &sock_path,
            Duration::from_secs(5),
            &key_path,
        )
        .unwrap();
        let server_secret = SharedSecret::load(&key_path).unwrap();
        let event = NotifyChangeEvent::new(
            "/srv/skills/alpha",
            "alpha",
            NotifyEventKind::Write,
            vec![],
            5000,
        );

        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let session = authenticate_server(
                &mut stream,
                &server_secret,
                NOTIFY_CLIENT_DOMAIN,
                NOTIFY_SERVER_DOMAIN,
            )
            .unwrap();
            let request = session
                .read_frame(
                    &mut stream,
                    FrameSender::Client,
                    MAX_RESPONSE_BYTES as usize,
                )
                .unwrap();
            let request = String::from_utf8(request).unwrap();
            assert!(request.contains(NOTIFY_METHOD));
            session
                .write_frame(
                    &mut stream,
                    FrameSender::Server,
                    br#"{"ok":true,"data":{"schemaVersion":2,"accepted":true}}"#,
                )
                .unwrap();
        });

        let result = client.send(&event);
        handle.join().unwrap();
        assert!(result.is_ok(), "authenticated notify failed: {result:?}");
    }

    #[test]
    fn authenticated_notify_rejects_permissive_socket() {
        use std::os::unix::fs::PermissionsExt;
        use std::os::unix::net::UnixListener;

        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let sock_path = dir.path().join("test.sock");
        let key_path = dir.path().join("key");
        std::fs::write(&key_path, [5_u8; 32]).unwrap();
        std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let _listener = UnixListener::bind(&sock_path).unwrap();
        std::fs::set_permissions(&sock_path, std::fs::Permissions::from_mode(0o660)).unwrap();
        let client = UnixSocketNotifyClient::new_authenticated(
            &sock_path,
            Duration::from_secs(5),
            &key_path,
        )
        .unwrap();
        let event = NotifyChangeEvent::new(
            "/srv/skills/alpha",
            "alpha",
            NotifyEventKind::Write,
            vec![],
            5000,
        );

        let result = client.send(&event);
        assert!(matches!(
            result,
            Err(NotifyError::EndpointUntrusted(message))
                if message.contains("group or other permissions")
        ));
    }

    #[test]
    fn authenticated_notify_rejects_permissive_parent() {
        use std::os::unix::fs::PermissionsExt;
        use std::os::unix::net::UnixListener;

        let dir = tempfile::tempdir().unwrap();
        let sock_path = dir.path().join("test.sock");
        let key_path = dir.path().join("key");
        std::fs::write(&key_path, [5_u8; 32]).unwrap();
        std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let _listener = UnixListener::bind(&sock_path).unwrap();
        std::fs::set_permissions(&sock_path, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o750)).unwrap();
        let client = UnixSocketNotifyClient::new_authenticated(
            &sock_path,
            Duration::from_secs(5),
            &key_path,
        )
        .unwrap();
        let event = NotifyChangeEvent::new(
            "/srv/skills/alpha",
            "alpha",
            NotifyEventKind::Write,
            vec![],
            5000,
        );

        let result = client.send(&event);
        assert!(matches!(
            result,
            Err(NotifyError::EndpointUntrusted(message))
                if message.contains("group or other permissions")
        ));
    }

    #[test]
    fn authenticated_notify_accepts_restrictive_parent() {
        use std::os::unix::fs::PermissionsExt;
        use std::os::unix::net::UnixListener;

        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let sock_path = dir.path().join("test.sock");
        let _listener = UnixListener::bind(&sock_path).unwrap();
        std::fs::set_permissions(&sock_path, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o300)).unwrap();

        let result = validate_authenticated_notify_endpoint(&sock_path);

        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(
            result.is_ok(),
            "owner-only parent permissions must be accepted: {result:?}"
        );
    }

    #[test]
    fn authenticated_notify_rejects_non_socket_endpoint() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let sock_path = dir.path().join("not-a-socket");
        let key_path = dir.path().join("key");
        std::fs::write(&sock_path, b"not a socket").unwrap();
        std::fs::set_permissions(&sock_path, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::fs::write(&key_path, [5_u8; 32]).unwrap();
        std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let client = UnixSocketNotifyClient::new_authenticated(
            &sock_path,
            Duration::from_secs(5),
            &key_path,
        )
        .unwrap();
        let event = NotifyChangeEvent::new(
            "/srv/skills/alpha",
            "alpha",
            NotifyEventKind::Write,
            vec![],
            5000,
        );

        let result = client.send(&event);
        assert!(matches!(
            result,
            Err(NotifyError::EndpointUntrusted(message)) if message.contains("not a Unix socket")
        ));
    }

    #[test]
    fn unix_socket_client_rejects_oversized_response() {
        use std::os::unix::net::UnixListener;

        let dir = tempfile::tempdir().unwrap();
        let sock_path = dir.path().join("test.sock");
        let listener = UnixListener::bind(&sock_path).unwrap();

        let client = UnixSocketNotifyClient::new(&sock_path, Duration::from_secs(5));
        let event = NotifyChangeEvent::new(
            "/srv/skills/alpha",
            "alpha",
            NotifyEventKind::Write,
            vec![],
            5000,
        );

        let handle = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(&stream);
            let mut _req = String::new();
            reader.read_line(&mut _req).unwrap();
            use std::io::Write;
            let mut writer = std::io::BufWriter::new(&stream);
            // Write >64KB without a newline — should be rejected.
            let payload = vec![b'A'; (MAX_RESPONSE_BYTES as usize) + 100];
            writer.write_all(&payload).unwrap();
            writer.write_all(b"\n").unwrap();
            writer.flush().unwrap();
        });

        let result = client.send(&event);
        handle.join().unwrap();
        assert!(
            matches!(result, Err(NotifyError::InvalidResponse { .. })),
            "oversized response must be rejected: {result:?}"
        );
    }

    #[test]
    fn reload_outcome_emitted_as_protocol_event_after_flush() {
        use super::super::activation_reload::ActivationReloadController;

        let dir = tempfile::tempdir().unwrap();
        let skill_dir = dir.path().join("alpha");
        let meta = skill_dir.join(".skill-meta");
        std::fs::create_dir_all(&meta).unwrap();
        // No activation.json → reload will timeout since nothing is fresh.
        // But reload_skill_once will fail-safe hidden.
        // Actually, for send_one path we need the poll. Let's create a valid
        // activation that is already fresh to get an outcome quickly.
        let snap = skill_dir.join(".skill-meta/versions/v000001.snapshot");
        std::fs::create_dir_all(&snap).unwrap();
        std::fs::write(
            meta.join("activation.json"),
            r#"{"schemaVersion": 1, "target": ".skill-meta/versions/v000001.snapshot"}"#,
        )
        .unwrap();

        let resolver = Arc::new(super::super::active::ActiveSkillResolver::new(dir.path()));
        let reload_ctrl = Arc::new(ActivationReloadController::new(
            dir.path(),
            resolver.clone(),
            Duration::from_millis(30),
            Duration::from_millis(500),
        ));

        let client = Arc::new(InMemoryNotifyClient::new());
        let writer = Arc::new(InMemoryProtocolEventWriter::new());
        let ctrl = NotifyController::new_with_reload(
            client.clone(),
            dir.path().to_path_buf(),
            dir.path().to_path_buf(),
            Duration::from_millis(50),
            5000,
            writer.clone(),
            reload_ctrl,
        );

        ctrl.observe("alpha", Some(Path::new("SKILL.md")), MutationKind::Write);

        // Wait briefly to allow mtime to advance, then re-write activation.
        std::thread::sleep(Duration::from_millis(15));
        std::fs::write(
            meta.join("activation.json"),
            r#"{"schemaVersion": 1, "target": ".skill-meta/versions/v000001.snapshot"}"#,
        )
        .unwrap();

        ctrl.flush_for_testing();

        // Should have at least 2 protocol events: the mutation event and
        // the reload outcome event.
        let events = writer.events();
        assert!(
            events.len() >= 2,
            "expected at least 2 protocol events (mutation + reload), got {}",
            events.len()
        );

        let reload_events: Vec<_> = events.iter().filter(|e| e.event_kind == "reload").collect();
        assert!(
            !reload_events.is_empty(),
            "expected at least one reload protocol event"
        );
        let reload_event = &reload_events[0];
        assert!(
            reload_event.reload_outcome.is_some(),
            "reload event must have reload_outcome"
        );
        let outcome = reload_event.reload_outcome.as_ref().unwrap();
        assert!(
            outcome == "activation_updated"
                || outcome == "activation_unchanged"
                || outcome == "activation_timeout",
            "unexpected reload outcome: {outcome}"
        );

        ctrl.shutdown();
    }
}

#[cfg(test)]
mod public_endpoint_tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::os::unix::net::UnixListener;

    #[test]
    fn public_endpoint_requires_root_owner_and_no_writable_or_symlink_parent() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        let socket = directory.path().join("daemon.sock");
        let _listener = UnixListener::bind(&socket).unwrap();
        std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o666)).unwrap();
        let result = validate_authenticated_notify_endpoint(&socket);
        if unsafe { libc::geteuid() } == 0 {
            assert_eq!(result.unwrap(), 0);
        } else {
            assert!(matches!(result, Err(NotifyError::EndpointUntrusted(_))));
        }
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o777)).unwrap();
        assert!(validate_authenticated_notify_endpoint(&socket).is_err());
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        let alias = tempfile::tempdir().unwrap();
        symlink(directory.path(), alias.path().join("linked-parent")).unwrap();
        assert!(
            validate_authenticated_notify_endpoint(&alias.path().join("linked-parent/daemon.sock"))
                .is_err()
        );
    }
}
