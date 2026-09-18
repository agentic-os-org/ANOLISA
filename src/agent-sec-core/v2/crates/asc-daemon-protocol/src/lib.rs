//! First-version wire contracts for the local daemon.
//!
//! This crate owns only untrusted serialized values across two method
//! families: PAP administration and Action capabilities. It reuses stable
//! Policy, Scope, Binding, identifier, and revision types rather than defining
//! daemon DTO copies. Execution, authorization, persistence, and transport live
//! in higher layers.

#![forbid(unsafe_code)]

mod action;
mod common;
mod envelope;
pub mod method;
mod pap;
mod response;

pub use action::CodeScanParams;
pub use common::{ListParams, ListResult, ResourceParams, RevisionParams};
pub use envelope::DaemonRequest;
pub use pap::{
    CreateBindingParams, CreatePolicyParams, CreateScopeParams, UpdateBindingParams,
    UpdatePolicyParams, UpdateScopeParams,
};
pub use response::{
    DaemonError, DaemonResponse, ErrorCode, ErrorResponse, MAX_DAEMON_ERROR_MESSAGE_BYTES,
    RequestId, SuccessResponse, error_code,
};
