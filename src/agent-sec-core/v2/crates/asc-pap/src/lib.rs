//! Transport-independent Policy Administration Point use cases.
//!
//! PAP owns current-record Policy, Scope, and Binding CRUD with monotonic revisions.
//! Policy authoring is lowered synchronously through [`PolicyCompiler`].
//! A Binding revision is a complete immutable snapshot, while only the current
//! revision and its lifecycle status are retained by the Repository.
//! Target-specific translation, Adapter dispatch, and retries are intentionally
//! outside this crate.
//!
//! Binding writes commit current intent before notifying [`BindingReconcileEnqueuer`].
//! The daemon wires this port to the Policy Runtime's queue and workers; a
//! successful CRUD response acknowledges intent, not completed target deployment.
//! Confirmed scheduling rejections atomically fail pending intent with a reason.
//! Compensation scans repair missed notifications still eligible in Binding state.
//! Durable intent and revision/status fencing across restart remain acceptance
//! gates for the persistent Repository work package; this crate owns no worker
//! or durable outbox.

#![forbid(unsafe_code)]

mod compiler;
mod error;
mod model;
mod repository;
mod service;

pub use compiler::PolicyCompiler;
pub use error::{EnqueueError, PapError};
pub use model::{Page, PolicyRevisionState, ScopeRevisionState};
pub use repository::PapRepository;
pub use service::PapService;

/// Post-commit Binding wake-up; notifications contain no command or spec.
pub trait BindingReconcileEnqueuer: Send + Sync {
    /// # Errors
    /// Rejects Binding mutations when the background service cannot accept work.
    /// Policy/Scope CRUD does not depend on this port. Individual attempt errors
    /// and temporary scan failures do not close Binding admission.
    fn check_ready(&self) -> Result<(), PapError>;
    /// Returns a typed scheduling rejection after intent was committed.
    /// Existing IDs merge successfully even at capacity.
    /// # Errors
    /// Returns Full or Stopped when this notification cannot be accepted.
    fn enqueue(&self, id: &asc_foundation_types::ResourceId) -> Result<(), EnqueueError>;
}
