//! Port of the secret-gateway control plane.
//!
//! The handler projects requests onto this port; the composition root binds
//! the concrete gateway runtime. Results are value-free JSON documents except
//! the credentials query, which carries only authorized domain fakes.

use std::path::PathBuf;

use crate::PeerCredentials;

/// Category of a gateway failure, mapped onto daemon error codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GatewayFailureKind {
    /// The request named an invalid value.
    InvalidArgument,
    /// The named Agent domain or preset is not configured.
    NotFound,
    /// The request conflicts with the configured state.
    Conflict,
    /// The caller may not use the named Agent domain.
    PermissionDenied,
    /// A fixed capacity is used up.
    ResourceExhausted,
    /// The gateway or a host prerequisite is not available.
    Unavailable,
    /// An unexpected local failure.
    Internal,
}

/// Gateway failure with a value-free message.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct GatewayFailure {
    /// Category.
    pub kind: GatewayFailureKind,
    /// Human-readable reason.
    pub message: String,
}

/// Request to protect a preset credential for an Agent domain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayKeyRequest {
    /// Agent domain.
    pub agent: String,
    /// Built-in preset name.
    pub preset: String,
    /// File holding the real value; it must belong to the calling peer.
    pub secret_file: PathBuf,
}

/// Secret-gateway use cases; implementations serialize their own operations.
pub trait GatewayAdministration: Send + Sync {
    /// Starts serving the configured Agent domains.
    ///
    /// # Errors
    /// Returns the reason the gateway cannot start.
    fn enable(&self) -> Result<serde_json::Value, GatewayFailure>;

    /// Stops serving and removes runtime files.
    ///
    /// # Errors
    /// Returns the first cleanup failure.
    fn disable(&self) -> Result<serde_json::Value, GatewayFailure>;

    /// Reports the gateway without values; root sees all domains, other peers only their own.
    ///
    /// # Errors
    /// Returns an unexpected local failure.
    fn status(&self, peer: PeerCredentials) -> Result<serde_json::Value, GatewayFailure>;

    /// Protects a preset credential for a registered domain; `peer` must own the secret file.
    ///
    /// # Errors
    /// Returns invalid input, conflicts or apply failures.
    fn add_key(
        &self,
        peer: PeerCredentials,
        request: GatewayKeyRequest,
    ) -> Result<serde_json::Value, GatewayFailure>;

    /// Withdraws a preset credential from an Agent domain.
    ///
    /// # Errors
    /// Returns unknown names or apply failures.
    fn remove_key(&self, agent: &str, preset: &str) -> Result<serde_json::Value, GatewayFailure>;

    /// Lists credentials without values.
    ///
    /// # Errors
    /// Returns store failures.
    fn list_keys(&self) -> Result<serde_json::Value, GatewayFailure>;

    /// Registers an already-running process independently of gateway startup.
    ///
    /// The handler requires root; `uid` and `pid` identify the target, not the caller.
    ///
    /// # Errors
    /// Returns invalid targets, domain conflicts, or host failures.
    fn register(
        &self,
        uid: u32,
        pid: u32,
        agent: &str,
    ) -> Result<serde_json::Value, GatewayFailure>;

    /// Removes a registered Agent domain and its credential grants.
    ///
    /// # Errors
    /// Returns unknown domains or cleanup failures.
    fn unregister(&self, agent: &str) -> Result<serde_json::Value, GatewayFailure>;

    /// Returns domain fakes only; non-root peers may query only their own domain.
    ///
    /// # Errors
    /// Returns unknown domains, denied ownership, or unavailable credentials.
    fn credentials(
        &self,
        peer: PeerCredentials,
        agent: &str,
    ) -> Result<serde_json::Value, GatewayFailure>;
}
