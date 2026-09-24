//! Transport-independent daemon identity, authorization, and PAP orchestration.

#![forbid(unsafe_code)]

mod observability;
pub use observability::{ObservabilityService, ObservabilitySink, ObservabilityWriteError};
mod action;
mod identity;
pub use action::ActionService;
mod pap;

pub use identity::{
    PeerCredentials, Principal, PrincipalPolicy, PrincipalPolicyError, PrincipalRole,
    RootManagedPrincipalPolicy,
};
pub use pap::{
    EnqueueError, NotFoundResource, PolicyAdministration, PolicyAdministrationError,
    PolicyInputError, ResourcePage,
};
