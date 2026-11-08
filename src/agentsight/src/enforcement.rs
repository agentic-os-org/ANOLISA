//! AgentSight-side coordination for the privileged enforcement service.

mod client;
mod coordinator;
mod store;
mod target;
mod transition;

pub use agentsight_enforcement_protocol::{
    ApplyPolicy, Binding, BindingState, Effect, HealthStatus, ViolationEvent,
};
pub use client::{
    EnforcementClient, EnforcementError, SecurityEventSubscription, ViolationSubscription,
};
pub use coordinator::{EnforcementCoordinator, EnforcementCoordinatorError};
pub(crate) use coordinator::{IngestionGenerationLease, IngestionLease};
pub use store::{EnforcementStore, EnforcementStoreError};
pub(crate) use target::{canonical_policy_file, read_process_start_time};
pub use transition::{PolicyTransition, TransitionDirection, TransitionKey, TransitionPhase};
