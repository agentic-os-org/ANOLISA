//! Structured provider failures keep dispatch evidence separate from error kind.

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderFailure {
    pub code: String,
    pub kind: ProviderFailureKind,
    pub dispatch: DispatchState,
    pub request_id: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProviderFailureKind {
    ActivationRequired,
    CreditsPreparing,
    QuotaExhausted,
    CredentialsExpired,
    PermissionDenied,
    Identity,
    Transport,
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DispatchState {
    NotDispatched,
    Accepted,
    Unknown,
}
