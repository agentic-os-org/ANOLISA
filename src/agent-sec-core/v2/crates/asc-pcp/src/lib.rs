//! Synchronous, process-local Binding reconciliation core.
//!
//! The caller owns notification delivery and retry timers. A call owns its
//! Binding execution slot through all target I/O and bookkeeping. No detached
//! tasks are spawned; a blocking caller must join the call before shutdown.
//! Notification coalescing, capacity, retry timers and compensation scanning
//! live in asc-policy-runtime.
//! Target identity, payload preparation and replacement semantics belong to
//! Clients. This crate has no PAP service, concrete PEP, transport or SQL dependency.

#![forbid(unsafe_code)]

mod model;
mod ports;
mod reconciler;
mod retry;
mod state;

#[cfg(test)]
mod acceptance_tests;
#[cfg(test)]
mod panic_recovery_tests;
#[cfg(test)]
#[path = "../tests/support/store.rs"]
mod test_store;

use model::{AttemptOutcome, ExecutionSlot, PreparedAttempt};
pub use model::{AttemptSchedule, Disposition, ExpectedBinding};
pub use ports::*;
pub use reconciler::BindingReconciler;

// Re-export the shared contract for existing core consumers; definitions live
// below both the runtime and concrete Clients, never in this implementation.
pub use asc_policy_target_contracts::{
    TargetBindingAdapter, TargetDeploymentClient, TargetDeploymentClientFactory,
};
pub use asc_policy_types::target::{
    DeploymentReport, Failure, FailureKind, Observation, PreparedApply, Presence, TargetRef,
};

pub use asc_policy_repository::{
    BindingStateRepository, BindingStateSnapshot, BindingStateWrite, Deployment, RetryPolicy,
    StoreError, WriteResult,
};
/// Complete serialized state retained for existing fixture consumers.
pub type ReconcileRecord = BindingStateSnapshot;
