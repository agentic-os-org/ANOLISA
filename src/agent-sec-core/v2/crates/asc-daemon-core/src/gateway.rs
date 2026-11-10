//! Credential pair administration and shared public CA queries.

use crate::PeerCredentials;

/// Category projected onto daemon error codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GatewayFailureKind {
    /// Invalid request input.
    InvalidArgument,
    /// Unknown credential pair or Scope.
    NotFound,
    /// Conflicting execution state.
    Conflict,
    /// Caller lacks authority over the operation.
    PermissionDenied,
    /// A bounded resource is exhausted.
    ResourceExhausted,
    /// Gateway or host prerequisite is unavailable.
    Unavailable,
    /// Unexpected local failure.
    Internal,
}

/// Value-free gateway failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct GatewayFailure {
    /// Failure category.
    pub kind: GatewayFailureKind,
    /// Redacted diagnostic.
    pub message: String,
}

/// Root pair operations and authenticated public gateway queries.
pub trait GatewayAdministration: Send + Sync {
    /// Enables the gateway.
    /// # Errors
    /// Returns prerequisite or publication failures.
    fn enable(&self) -> Result<serde_json::Value, GatewayFailure>;
    /// Withdraws routing before stopping the proxy.
    /// # Errors
    /// Returns host or runtime cleanup failures.
    fn disable(&self) -> Result<serde_json::Value, GatewayFailure>;
    /// Reports execution state filtered by the authenticated caller.
    /// # Errors
    /// Returns execution-state projection failures.
    fn status(&self, peer: PeerCredentials) -> Result<serde_json::Value, GatewayFailure>;
    /// Globally retires a credential pair and all its grants.
    /// # Errors
    /// Returns unknown-pair, storage or publication failures.
    fn remove_key(&self, pairid: &str) -> Result<serde_json::Value, GatewayFailure>;
    /// Lists currently imported pair identifiers, without values.
    /// # Errors
    /// Returns credential storage or projection failures.
    fn list_keys(&self) -> Result<serde_json::Value, GatewayFailure>;
    /// Returns the shared public CA bundle path, without Scope or credential values.
    /// # Errors
    /// Returns gateway-readiness or trust-bundle failures.
    fn ca(&self, peer: PeerCredentials) -> Result<serde_json::Value, GatewayFailure>;
}
