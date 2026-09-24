#![forbid(unsafe_code)]
//! Pinned, serial capability execution using the versioned AW contract bundle.
//!
//! Core invokes a trusted Host and journals facts. It never starts an Agent,
//! installs OS protection, dispatches tools or manufactures adoption evidence.

pub mod journal;
pub mod ports;

mod execute;
mod prepare;

pub use prepare::{PrepareRequest, PreparedPlan, StepInput};

use aw_contracts::{orchestration::InvocationEvidence, Registry};
use ports::{HostError, JournalError, ProviderResult};
use serde_json::Value;

/// Runtime failures leave an existing journal claim intact for reconciliation.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Schema or cross-record validation rejected a supplied fact.
    #[error(transparent)]
    Contract(#[from] aw_contracts::Error),
    /// No provider was registered for a selected route.
    #[error("selected provider is unavailable")]
    ProviderUnavailable,
    /// An admitted provider descriptor changed before dispatch.
    #[error("provider descriptor changed after plan preparation")]
    ProviderChanged,
    /// Input preparation did not match the complete pinned plan.
    #[error("invalid preparation: {0}")]
    Preparation(&'static str),
    /// Host transport failed without trustworthy result evidence.
    #[error(transparent)]
    Host(#[from] HostError),
    /// A journal claim or append failed; execution must not proceed.
    #[error(transparent)]
    Journal(#[from] JournalError),
    /// A receipt's timestamps contradict the local call interval.
    #[error("receipt is outside the observed host call interval")]
    HostTime,
}

/// One correlated call retained in memory, with potentially sensitive input/output.
#[derive(Debug)]
pub struct CallRecord {
    invocation: Value,
    result: ProviderResult,
}

impl CallRecord {
    /// The immutable invocation actually sent to the Host.
    pub fn invocation(&self) -> &Value {
        &self.invocation
    }

    /// Host-owned receipt and candidate/inspection, not adoption.
    pub fn result(&self) -> &ProviderResult {
        &self.result
    }

    /// Borrows the existing contract validator's evidence view.
    pub fn evidence(&self) -> InvocationEvidence<'_> {
        InvocationEvidence {
            invocation: &self.invocation,
            receipt: &self.result.receipt,
            output: self.result.output.as_ref(),
        }
    }
}

/// Validated terminal execution, returned only after the journal acknowledges it.
///
/// `proceed` admits the next native stage, not actual dispatch or adoption. The
/// native owner must still perform fresh intent/OS checks or observe delivery.
pub struct Execution {
    plan: Value,
    boundary: Value,
    execution: Value,
    calls: Vec<CallRecord>,
    journal_ack: Value,
}

impl Execution {
    /// The exact policy-resolved plan pinned before the first provider call.
    pub fn plan(&self) -> &Value {
        &self.plan
    }

    /// Boundary powers supplied by the trusted native adapter.
    pub fn boundary(&self) -> &Value {
        &self.boundary
    }

    /// The complete `plan-execution/v1` record, including skipped steps.
    pub fn record(&self) -> &Value {
        &self.execution
    }

    /// Actual calls only; gaps and skipped steps do not fabricate receipts.
    pub fn calls(&self) -> &[CallRecord] {
        &self.calls
    }

    /// Acknowledgement for storing this terminal execution record.
    pub fn journal_ack(&self) -> &Value {
        &self.journal_ack
    }
}

/// Validated, serial capability execution over trusted runtime ports.
pub struct Core {
    registry: Registry,
}

impl Core {
    /// Builds the offline schema registry once for subsequent executions.
    ///
    /// # Errors
    /// Returns an error if the embedded contract bundle cannot be compiled.
    pub fn new() -> Result<Self, Error> {
        Ok(Self {
            registry: Registry::new()?,
        })
    }
}
