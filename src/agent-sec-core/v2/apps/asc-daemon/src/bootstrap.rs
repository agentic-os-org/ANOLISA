use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use asc_daemon_service::{
    BindError, BoundUnixSocket, ConfigError, RejectionEncoder, RequestDispatcher, ServeError,
    ServeReport, ServiceConfig, ShutdownToken, UnixService,
};

const DEFAULT_MAX_FRAME_BYTES: usize = 4 * 1024 * 1024;
const DEFAULT_MAX_CONNECTIONS: usize = 64;
const DEFAULT_MAX_REJECTION_CONNECTIONS: usize = 8;
const DEFAULT_REJECTION_ENCODE_TIMEOUT: Duration = Duration::from_millis(250);
const DEFAULT_REQUEST_READ_TIMEOUT: Duration = Duration::from_secs(5);
const DEFAULT_DISPATCH_TIMEOUT: Duration = Duration::from_secs(5);
const DEFAULT_RESPONSE_WRITE_TIMEOUT: Duration = Duration::from_secs(5);
const DEFAULT_DRAIN_TIMEOUT: Duration = Duration::from_secs(2);
const DEFAULT_ACCEPT_ERROR_BACKOFF: Duration = Duration::from_millis(50);
const DEFAULT_SOCKET_MODE: u32 = 0o600;

/// Process-owned inputs needed to bind and run the transport service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootstrapConfig {
    /// Absolute path selected by deployment or an explicit development invocation.
    pub socket_path: PathBuf,
    /// Filesystem mode applied after binding the socket.
    pub socket_mode: u32,
    /// Bounded transport limits owned by the composition root.
    pub service: ServiceConfig,
}

impl BootstrapConfig {
    /// Creates a bootstrap configuration using the current transport bring-up limits.
    pub fn new(socket_path: impl Into<PathBuf>) -> Self {
        Self {
            socket_path: socket_path.into(),
            socket_mode: DEFAULT_SOCKET_MODE,
            service: default_service_config(),
        }
    }
}

/// Returns the explicit transport limits used by the runnable daemon bootstrap.
pub const fn default_service_config() -> ServiceConfig {
    ServiceConfig {
        max_request_frame_bytes: asc_daemon_protocol::MAX_REQUEST_FRAME_BYTES,
        max_response_frame_bytes: DEFAULT_MAX_FRAME_BYTES,
        max_connections: DEFAULT_MAX_CONNECTIONS,
        max_rejection_connections: DEFAULT_MAX_REJECTION_CONNECTIONS,
        rejection_encode_timeout: DEFAULT_REJECTION_ENCODE_TIMEOUT,
        request_read_timeout: DEFAULT_REQUEST_READ_TIMEOUT,
        dispatch_timeout: DEFAULT_DISPATCH_TIMEOUT,
        response_write_timeout: DEFAULT_RESPONSE_WRITE_TIMEOUT,
        drain_timeout: DEFAULT_DRAIN_TIMEOUT,
        accept_error_backoff: DEFAULT_ACCEPT_ERROR_BACKOFF,
    }
}

/// Binds the configured UDS endpoint with application dispatch and rejection encoding.
///
/// The caller owns process signals, runtime-directory validation, singleton and
/// stale-socket policy. Those policies require their own deployment acceptance
/// before a packaging default socket path can be selected.
///
/// # Errors
/// Returns a stable bootstrap error when binding, configuration validation,
/// serving, or owned-socket cleanup fails.
pub async fn serve(
    config: BootstrapConfig,
    dispatcher: Arc<dyn RequestDispatcher>,
    rejection_encoder: Arc<dyn RejectionEncoder>,
    shutdown: ShutdownToken,
) -> Result<ServeReport, BootstrapError> {
    let socket = BoundUnixSocket::bind(&config.socket_path, config.socket_mode)?;
    let service = UnixService::new(socket, config.service, dispatcher, rejection_encoder)?;
    Ok(service.serve(shutdown).await?)
}

/// Failure to start or run the daemon transport.
#[derive(Debug, thiserror::Error)]
pub enum BootstrapError {
    /// The configured Unix socket could not be bound safely.
    #[error("daemon socket bind failed")]
    Bind(#[from] BindError),
    /// One or more service resource bounds were invalid.
    #[error("daemon service configuration is invalid")]
    Config(#[from] ConfigError),
    /// The listener or owned socket cleanup failed.
    #[error("daemon service failed")]
    Serve(#[from] ServeError),
}
