//! Gateway control-plane errors, categorized for the daemon error codes.

use asc_credential_store::CredentialStoreError;
use asc_gateway_plan::PlanError;

/// Category of a gateway control-plane failure, mapped onto daemon error codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GatewayErrorKind {
    /// The request named an invalid value.
    InvalidArgument,
    /// The named credential or Scope is not configured.
    NotFound,
    /// The request conflicts with the configured state.
    Conflict,
    /// The caller may not use the named Scope.
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
    process_exited: bool,
}

impl GatewayError {
    pub(crate) fn new(kind: GatewayErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            process_exited: false,
        }
    }

    // Unavailable host evidence is not proof of exit; only process owners set this marker.
    pub(crate) fn process_exited() -> Self {
        Self {
            kind: GatewayErrorKind::Unavailable,
            message: "process exited".into(),
            process_exited: true,
        }
    }

    pub(crate) fn is_process_exited(&self) -> bool {
        self.process_exited
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
            process_exited: self.process_exited,
        }
    }
}

impl From<CredentialStoreError> for GatewayError {
    fn from(error: CredentialStoreError) -> Self {
        let kind = match &error {
            CredentialStoreError::Invalid { .. } => GatewayErrorKind::InvalidArgument,
            CredentialStoreError::UnknownCredential(_) => GatewayErrorKind::NotFound,
            CredentialStoreError::DuplicateValue(_)
            | CredentialStoreError::Conflict(_)
            | CredentialStoreError::Retired(_) => GatewayErrorKind::Conflict,
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
        Self::new(GatewayErrorKind::InvalidArgument, error.to_string())
    }
}

impl From<asc_pap::PapError> for GatewayError {
    fn from(error: asc_pap::PapError) -> Self {
        let kind = match error {
            asc_pap::PapError::NotFound => GatewayErrorKind::NotFound,
            asc_pap::PapError::Conflict => GatewayErrorKind::Conflict,
            _ => GatewayErrorKind::Unavailable,
        };
        Self::new(kind, "canonical PAP repository query failed")
    }
}
