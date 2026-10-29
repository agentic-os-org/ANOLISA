//! Gateway control-plane errors, categorized for the daemon error codes.

use asc_credential_store::CredentialStoreError;
use asc_gateway_plan::PlanError;

/// Category of a gateway control-plane failure, mapped onto daemon error codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GatewayErrorKind {
    /// The request named an invalid value.
    InvalidArgument,
    /// The named Agent domain or preset is not configured.
    NotFound,
    /// The request conflicts with the configured state.
    Conflict,
    /// The caller may not use the named Agent domain.
    PermissionDenied,
    /// A fixed capacity, such as the listener ports, is used up.
    ResourceExhausted,
    /// The gateway or a host prerequisite is not available.
    Unavailable,
    /// An unexpected local failure.
    Internal,
}

/// Gateway control-plane failure; messages never contain real or fake values.
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct GatewayError {
    kind: GatewayErrorKind,
    message: String,
}

impl GatewayError {
    pub(crate) fn new(kind: GatewayErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    pub(crate) fn invalid(message: impl Into<String>) -> Self {
        Self::new(GatewayErrorKind::InvalidArgument, message)
    }

    pub(crate) fn unavailable(message: impl Into<String>) -> Self {
        Self::new(GatewayErrorKind::Unavailable, message)
    }

    pub(crate) fn internal(message: impl Into<String>) -> Self {
        Self::new(GatewayErrorKind::Internal, message)
    }

    pub(crate) fn io(context: &str, path: &std::path::Path, error: &std::io::Error) -> Self {
        Self::internal(format!("{context} {}: {error}", path.display()))
    }

    /// Returns the failure category.
    pub fn kind(&self) -> GatewayErrorKind {
        self.kind
    }

    /// Prefixes the message with what had already been done when the failure happened.
    #[must_use]
    pub(crate) fn context(self, prefix: &str) -> Self {
        Self {
            kind: self.kind,
            message: format!("{prefix}: {}", self.message),
        }
    }
}

impl From<CredentialStoreError> for GatewayError {
    fn from(error: CredentialStoreError) -> Self {
        let kind = match &error {
            CredentialStoreError::Invalid { .. } => GatewayErrorKind::InvalidArgument,
            CredentialStoreError::UnknownCredential(_) => GatewayErrorKind::NotFound,
            CredentialStoreError::DuplicateValue(_) | CredentialStoreError::Retired(_) => {
                GatewayErrorKind::Conflict
            }
            CredentialStoreError::Capacity(_) => GatewayErrorKind::ResourceExhausted,
            CredentialStoreError::Locked { .. } | CredentialStoreError::NotEnabled { .. } => {
                GatewayErrorKind::Unavailable
            }
            _ => GatewayErrorKind::Internal,
        };
        Self::new(kind, error.to_string())
    }
}

impl From<PlanError> for GatewayError {
    fn from(error: PlanError) -> Self {
        // Plans come from fixed presets, so only a preset name can be the caller's fault.
        let kind = match &error {
            PlanError::Invalid {
                field: "preset", ..
            } => GatewayErrorKind::InvalidArgument,
            _ => GatewayErrorKind::Internal,
        };
        Self::new(kind, error.to_string())
    }
}
