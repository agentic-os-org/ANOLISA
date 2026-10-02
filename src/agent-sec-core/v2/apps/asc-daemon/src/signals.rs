use std::io;

use asc_daemon_service::ShutdownToken;
use tokio::signal::unix::{Signal, SignalKind, signal};

/// Installed process signal streams for cooperative foreground shutdown.
pub struct ProcessSignals {
    terminate: Signal,
    interrupt: Signal,
    hangup: Signal,
}

impl ProcessSignals {
    /// Installs SIGTERM, SIGINT, and SIGHUP streams before the socket is bound.
    ///
    /// # Errors
    /// Returns an installation error before any service resource is acquired.
    pub fn install() -> Result<Self, SignalError> {
        Ok(Self {
            terminate: signal(SignalKind::terminate()).map_err(SignalError::Install)?,
            interrupt: signal(SignalKind::interrupt()).map_err(SignalError::Install)?,
            hangup: signal(SignalKind::hangup()).map_err(SignalError::Install)?,
        })
    }

    /// Waits for SIGTERM or SIGINT and requests the shared service shutdown.
    ///
    /// SIGHUP is deliberately consumed without changing configuration or process
    /// state, preserving the current no-reload lifecycle behavior.
    pub async fn request_shutdown(mut self, shutdown: ShutdownToken) {
        loop {
            tokio::select! {
                biased;
                _ = self.terminate.recv() => {
                    shutdown.request();
                    return;
                }
                _ = self.interrupt.recv() => {
                    shutdown.request();
                    return;
                }
                _ = self.hangup.recv() => {}
            }
        }
    }
}

/// Failure to install a required Unix signal stream.
#[derive(Debug, thiserror::Error)]
pub enum SignalError {
    /// Tokio could not register a process signal listener.
    #[error("daemon signal handler installation failed")]
    Install(#[source] io::Error),
}
