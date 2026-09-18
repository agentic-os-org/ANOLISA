//! Shared endpoint validation for the daemon client and service.
//!
//! V2 deployment owns the daemon endpoint. Callers can pass `--socket` or use
//! `AGENT_SEC_DAEMON_SOCKET`; neither binary derives a per-user XDG path.

use std::ffi::OsStr;
use std::path::PathBuf;

/// Environment variable carrying the deployed daemon's absolute UDS endpoint.
pub const DAEMON_SOCKET_ENV: &str = "AGENT_SEC_DAEMON_SOCKET";

/// Validates a socket path read from [`DAEMON_SOCKET_ENV`].
///
/// # Errors
///
/// Returns an error when the variable is absent, empty, or not absolute.
pub fn daemon_socket_path_from_env(
    socket: Option<&OsStr>,
) -> Result<PathBuf, DaemonSocketPathError> {
    let socket = socket
        .filter(|path| !path.is_empty())
        .ok_or(DaemonSocketPathError::MissingEnvironmentSocket)?;
    let socket = PathBuf::from(socket);
    if !socket.is_absolute() {
        return Err(DaemonSocketPathError::RelativeSocket);
    }
    Ok(socket)
}

/// Errors resolving the deployed daemon socket.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum DaemonSocketPathError {
    /// No endpoint was supplied through the environment.
    #[error("{DAEMON_SOCKET_ENV} is required when --socket is omitted")]
    MissingEnvironmentSocket,
    /// UDS endpoints must be absolute paths.
    #[error("{DAEMON_SOCKET_ENV} must be an absolute path")]
    RelativeSocket,
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;

    use super::*;

    #[test]
    fn accepts_an_absolute_deployment_socket() {
        assert_eq!(
            daemon_socket_path_from_env(Some(OsStr::new("/run/agent-sec-core/daemon.sock"))),
            Ok(PathBuf::from("/run/agent-sec-core/daemon.sock"))
        );
    }

    #[test]
    fn rejects_missing_and_relative_environment_sockets() {
        assert_eq!(
            daemon_socket_path_from_env(None),
            Err(DaemonSocketPathError::MissingEnvironmentSocket)
        );
        assert_eq!(
            daemon_socket_path_from_env(Some(OsStr::new("relative"))),
            Err(DaemonSocketPathError::RelativeSocket)
        );
    }
}
