//! Local security evidence, risk-case models, and SQLite persistence.

use std::path::Path;

#[cfg(target_os = "linux")]
mod containment;
#[cfg(target_os = "linux")]
mod coordinator;
#[cfg(any(target_os = "linux", test))]
mod delivery;
mod query;
mod store;

#[cfg(target_os = "linux")]
pub use containment::enforcer::{
    ContainmentEnforcer, ContainmentEnforcerError, ContainmentReadinessLease,
    ContainmentReadinessStamp, StampedBinding, StampedBindings, stable_readiness_lease,
};
#[cfg(target_os = "linux")]
pub use containment::{
    ContainmentCandidate, ContainmentCoordinator, ContainmentError, ContainmentPlan,
    ContainmentRequest,
};
#[cfg(target_os = "linux")]
pub use coordinator::{SecurityCoordinator, SecurityCoordinatorError};
pub use query::{
    ContainmentAction, ContainmentFailureStage, ContainmentLifecycle, RiskCase, RiskCaseDetail,
    RiskCaseStatus, RiskSeverity, SecurityCountBy, SecurityEventFilter, SecurityEventPage,
    SecuritySession, SecuritySessionPage, SecuritySummary,
};
pub use store::{
    ContainmentActivationResult, ContainmentClaimResult, SecurityEventStore, SecurityStore,
    SecurityStoreError,
};

pub(crate) fn open_private_store(
    state_dir: impl AsRef<Path>,
) -> Result<SecurityStore, SecurityStoreError> {
    let connection =
        crate::private_sqlite::open_private_connection(state_dir.as_ref(), "security.db")
            .map_err(|error| SecurityStoreError::Open(error.to_string()))?;
    SecurityStore::from_connection(connection)
}
