//! Transport-independent Policy Administration Point use cases.
//!
//! PAP owns revisioned Policy records, immutable Scope assignments and system Bindings.
//! Authored templates are validated before storage and compiled by target Adapters.
//! A Binding revision is a complete immutable snapshot, while only the current
//! revision and its lifecycle status are retained by the Repository.
//! Target-specific translation, Adapter dispatch, and retries are intentionally
//! outside this crate.
//!
//! Binding writes commit current intent before notifying [`BindingReconcileEnqueuer`].
//! The daemon wires this port to the Policy Runtime's queue and workers; a
//! successful admission acknowledges intent, not completed target deployment.
//! Confirmed scheduling rejections atomically fail pending intent with a reason.
//! Compensation scans repair missed notifications still eligible in Binding state.
//! Durable intent and revision/status fencing across restart remain acceptance
//! gates for the persistent Repository work package; this crate owns no worker
//! or durable outbox.

#![forbid(unsafe_code)]

mod error;
mod model;
mod repository;
mod service;

pub use error::{EnqueueError, PapError};
pub use model::{Page, PolicyRevisionState, ScopeDiscoverySeed};
pub use repository::PapRepository;
pub use service::PapService;

/// Post-commit Binding wake-up; notifications contain no command or spec.
pub trait BindingReconcileEnqueuer: Send + Sync {
    /// # Errors
    /// Rejects new Scope assignments when the background service cannot accept work.
    /// Queries, Policy writes and Scope deletion remain available. Individual
    /// attempt errors and temporary scan failures do not close admission.
    fn check_ready(&self) -> Result<(), PapError>;
    /// Returns a typed scheduling rejection after intent was committed.
    /// Existing IDs merge successfully even at capacity.
    /// # Errors
    /// Returns Full or Stopped when this notification cannot be accepted.
    fn enqueue(&self, id: &asc_foundation_types::ResourceId) -> Result<(), EnqueueError>;
}

/// Lifecycle of Scope-owned instance discovery.
pub trait ScopeDiscovery: Send + Sync {
    /// Starts one worker from the stored assignment. On error no worker may remain.
    /// # Errors
    /// Rejects unavailable discovery or exhausted job capacity.
    fn start(&self, seed: &ScopeDiscoverySeed) -> Result<(), PapError>;
    /// Cancel and join only this Scope's worker and close its observation stream.
    /// # Errors
    /// Reports failure to stop the worker.
    fn stop(&self, id: &asc_foundation_types::ResourceId) -> Result<(), PapError>;
}

/// Trusted discovery input; repository transactions own admission and deduplication.
pub trait ScopeBindingSink: Send + Sync {
    /// Reconciles the complete known instance set, preserving unreadable instances.
    /// # Errors
    /// Returns storage or scheduling failures; the next scan may retry admission.
    fn sync_instances(
        &self,
        id: &asc_foundation_types::ResourceId,
        instances: &[asc_policy_types::process_discovery::ProcessIdentity],
    ) -> Result<(), PapError>;
}
