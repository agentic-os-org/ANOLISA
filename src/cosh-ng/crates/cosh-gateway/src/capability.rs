//! Fail-closed capability policy and single-use permit coordination.

mod approval;
mod broker;
mod execution;
mod memory;
mod provider;

pub use approval::{
    BrokeredApprovalBinding, DurableApprovalCoordinator, DurableApprovalError,
    DurableApprovalOutcome, DurableApprovalResolution, DurableProviderApprovalResolution,
};
pub use broker::{
    AuthoritativeRequestBinding, BrokerError, CapabilityBroker, ParentBinding, PermitClaim,
    PermitExpectation, PermitStore, PermitStoreError, PolicyDecision, PolicyError, PolicyPort,
    RequestContext,
};
pub use execution::{
    BoundExecutionOperation, ExecutionCommandBuildStage, ExecutionTarget, ExecutionTargetOutcome,
    GovernedExecutionCoordinator, GovernedExecutionError, GovernedExecutionResult,
    SecurityAuditError, SecurityAuditGate,
};
pub use memory::MemoryPermitStore;
pub use provider::{SealedCapabilityProviderRegistry, SealedProviderAdmissionError};
