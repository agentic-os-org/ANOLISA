//! Credential administration and Scope-owned gateway environment queries.

use std::path::PathBuf;

use crate::PeerCredentials;

/// Category projected onto daemon error codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GatewayFailureKind {
    /// Invalid request input.
    InvalidArgument,
    /// Unknown credential or Scope.
    NotFound,
    /// Conflicting execution state.
    Conflict,
    /// Caller lacks authority over the Scope.
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

/// Imports a real credential without replacing an active value or granting Scope access.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayKeyRequest {
    /// Credential identifier.
    pub credential: String,
    /// Caller-owned file containing the real value.
    pub secret_file: PathBuf,
}

/// Root credential operations and authenticated Scope environment queries.
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
    /// Imports a credential without replacing an active value or authorizing a Scope.
    /// # Errors
    /// Returns authorization, unsafe-file, storage or publication failures.
    fn add_key(
        &self,
        peer: PeerCredentials,
        request: GatewayKeyRequest,
    ) -> Result<serde_json::Value, GatewayFailure>;
    /// Globally retires a credential and all its grants.
    /// # Errors
    /// Returns unknown-credential, storage or publication failures.
    fn remove_key(&self, credential: &str) -> Result<serde_json::Value, GatewayFailure>;
    /// Lists currently imported credential identifiers.
    /// # Errors
    /// Returns credential storage or projection failures.
    fn list_keys(&self) -> Result<serde_json::Value, GatewayFailure>;
    /// Returns only the caller's Scope fakes and ready CA environment.
    /// # Errors
    /// Returns authorization, stale-Scope or gateway-readiness failures.
    fn credentials(
        &self,
        peer: PeerCredentials,
        scope_id: &str,
    ) -> Result<serde_json::Value, GatewayFailure>;
}
