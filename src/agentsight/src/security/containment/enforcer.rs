//! Generation-stamped enforcement boundary used by containment activation.

use std::time::Duration;

use agentsight_enforcement_protocol::{Binding, CredentialPolicySnapshot, ReplacePolicy};
use thiserror::Error;
use uuid::Uuid;

use crate::enforcement::{
    EnforcementCoordinator, EnforcementCoordinatorError, EnforcementError, EnforcementStoreError,
    IngestionGenerationLease, IngestionLease, PolicyTransition, TransitionKey, TransitionPhase,
};

/// Typed failures returned by the containment enforcement boundary.
#[derive(Debug, Error)]
pub enum ContainmentEnforcerError {
    /// No durable transition exists for this containment action and direction.
    #[error("policy transition for action {0} does not exist")]
    MissingTransition(Uuid),
    /// Readiness, transport, or local enforcement state is temporarily unavailable.
    #[error("{0}")]
    Unavailable(String),
    /// The enforcement boundary rejected or contradicted the durable request.
    #[error("{0}")]
    Rejected(String),
}

/// Applied binding paired with the ingestion generation that acknowledged it.
pub struct StampedBinding {
    binding: Binding,
    readiness_stamp: ContainmentReadinessStamp,
}

impl StampedBinding {
    /// Wraps a binding for an enforcer whose readiness does not change.
    pub fn stable(binding: Binding) -> Self {
        Self::new(binding, ContainmentReadinessStamp::stable())
    }

    pub(crate) fn new(binding: Binding, readiness_stamp: ContainmentReadinessStamp) -> Self {
        Self {
            binding,
            readiness_stamp,
        }
    }

    pub(crate) fn into_parts(self) -> (Binding, ContainmentReadinessStamp) {
        (self.binding, self.readiness_stamp)
    }
}

/// Persisted binding snapshot paired with the ingestion generation that observed it.
pub struct StampedBindings {
    bindings: Vec<Binding>,
    readiness_stamp: ContainmentReadinessStamp,
}

impl StampedBindings {
    /// Wraps a snapshot for an enforcer whose readiness does not change.
    pub fn stable(bindings: Vec<Binding>) -> Self {
        Self::new(bindings, ContainmentReadinessStamp::stable())
    }

    pub(crate) fn new(bindings: Vec<Binding>, readiness_stamp: ContainmentReadinessStamp) -> Self {
        Self {
            bindings,
            readiness_stamp,
        }
    }

    pub(crate) fn into_parts(self) -> (Vec<Binding>, ContainmentReadinessStamp) {
        (self.bindings, self.readiness_stamp)
    }
}

/// Short-lived proof that one stamped ingestion generation remains ready.
pub trait ContainmentReadinessLease {}

/// Opaque identity of the ingestion generation that observed an acknowledgement.
pub struct ContainmentReadinessStamp(ReadinessStampKind);

enum ReadinessStampKind {
    Stable,
    Enforcement(IngestionLease),
}

impl ContainmentReadinessStamp {
    const fn stable() -> Self {
        Self(ReadinessStampKind::Stable)
    }
}

struct StableReadinessLease;

impl ContainmentReadinessLease for StableReadinessLease {}

/// Creates a lease for test or in-process enforcers with immutable readiness.
pub fn stable_readiness_lease() -> Box<dyn ContainmentReadinessLease> {
    Box::new(StableReadinessLease)
}

/// Enforcement operations required by containment orchestration.
pub trait ContainmentEnforcer: Send + Sync {
    /// Bounds foreground apply ownership before restart reconciliation may take over.
    fn foreground_claim_lease(&self) -> Duration;
    /// Persists and executes one atomic policy ownership transition.
    fn begin_transition(
        &self,
        key: TransitionKey,
        request: ReplacePolicy,
    ) -> Result<StampedBinding, ContainmentEnforcerError>;
    /// Resumes one exact durable transition without caller-supplied policy state.
    fn resume_transition(
        &self,
        key: &TransitionKey,
    ) -> Result<StampedBinding, ContainmentEnforcerError>;
    /// Builds and executes the reverse transition from durable forward state.
    fn begin_reverse_transition(
        &self,
        action_id: Uuid,
    ) -> Result<StampedBinding, ContainmentEnforcerError>;
    /// Detaches a previously applied binding.
    fn detach(&self, binding_id: Uuid) -> Result<(), String>;
    /// Lists persisted enforcement bindings.
    fn bindings(&self) -> Result<StampedBindings, ContainmentEnforcerError>;
    /// Reads immutable structured provenance for one credential binding.
    fn credential_policy_snapshot(
        &self,
        binding_id: Uuid,
    ) -> Result<Option<CredentialPolicySnapshot>, ContainmentEnforcerError>;
    /// Leases the stamped ready generation for a short activation transaction.
    fn lease_ready(
        &self,
        stamp: ContainmentReadinessStamp,
    ) -> Result<Box<dyn ContainmentReadinessLease + '_>, ContainmentEnforcerError>;
}

struct EnforcementReadinessLease<'a> {
    _lease: IngestionGenerationLease<'a>,
}

impl ContainmentReadinessLease for EnforcementReadinessLease<'_> {}

impl ContainmentEnforcer for EnforcementCoordinator {
    fn foreground_claim_lease(&self) -> Duration {
        self.credential_apply_claim_lease()
    }

    fn begin_transition(
        &self,
        key: TransitionKey,
        request: ReplacePolicy,
    ) -> Result<StampedBinding, ContainmentEnforcerError> {
        let stamp = current_stamp(self)?;
        let transition = EnforcementCoordinator::begin_transition(self, key, request)
            .map_err(containment_enforcer_error)?;
        ensure_current_stamp(self, &stamp)?;
        stamped_completed_transition(transition, stamp)
    }

    fn resume_transition(
        &self,
        key: &TransitionKey,
    ) -> Result<StampedBinding, ContainmentEnforcerError> {
        let stamp = current_stamp(self)?;
        let transition = EnforcementCoordinator::resume_transition(self, key)
            .map_err(containment_enforcer_error)?;
        ensure_current_stamp(self, &stamp)?;
        stamped_completed_transition(transition, stamp)
    }

    fn begin_reverse_transition(
        &self,
        action_id: Uuid,
    ) -> Result<StampedBinding, ContainmentEnforcerError> {
        let stamp = current_stamp(self)?;
        let transition = EnforcementCoordinator::begin_reverse_transition(self, action_id)
            .map_err(containment_enforcer_error)?;
        ensure_current_stamp(self, &stamp)?;
        stamped_completed_transition(transition, stamp)
    }

    fn detach(&self, binding_id: Uuid) -> Result<(), String> {
        EnforcementCoordinator::detach(self, binding_id).map_err(|error| {
            log::error!("containment detach failed: {error}");
            "enforcement detach failed".into()
        })
    }

    fn bindings(&self) -> Result<StampedBindings, ContainmentEnforcerError> {
        let stamp = current_stamp(self)?;
        let bindings =
            EnforcementCoordinator::bindings(self).map_err(containment_enforcer_error)?;
        ensure_current_stamp(self, &stamp)?;
        Ok(StampedBindings::new(bindings, stamp))
    }

    fn credential_policy_snapshot(
        &self,
        binding_id: Uuid,
    ) -> Result<Option<CredentialPolicySnapshot>, ContainmentEnforcerError> {
        EnforcementCoordinator::credential_policy_snapshot(self, binding_id)
            .map_err(containment_enforcer_error)
    }

    fn lease_ready(
        &self,
        stamp: ContainmentReadinessStamp,
    ) -> Result<Box<dyn ContainmentReadinessLease + '_>, ContainmentEnforcerError> {
        let ReadinessStampKind::Enforcement(stamp) = stamp.0 else {
            return Err(ingestion_changed());
        };
        self.lease_ingestion_generation(&stamp)
            .map(|lease| {
                Box::new(EnforcementReadinessLease { _lease: lease })
                    as Box<dyn ContainmentReadinessLease>
            })
            .ok_or_else(ingestion_changed)
    }
}

fn current_stamp(
    coordinator: &EnforcementCoordinator,
) -> Result<ContainmentReadinessStamp, ContainmentEnforcerError> {
    coordinator
        .ingestion_generation()
        .map(|lease| ContainmentReadinessStamp(ReadinessStampKind::Enforcement(lease)))
        .map_err(containment_enforcer_error)
}

fn ensure_current_stamp(
    coordinator: &EnforcementCoordinator,
    expected: &ContainmentReadinessStamp,
) -> Result<(), ContainmentEnforcerError> {
    let ReadinessStampKind::Enforcement(expected) = &expected.0 else {
        return Err(ingestion_changed());
    };
    if coordinator.ingestion_generation_is_current(expected) {
        return Ok(());
    }
    Err(ingestion_changed())
}

fn ingestion_changed() -> ContainmentEnforcerError {
    ContainmentEnforcerError::Unavailable("enforcement ingestion generation changed".into())
}

fn containment_enforcer_error(error: EnforcementCoordinatorError) -> ContainmentEnforcerError {
    if let EnforcementCoordinatorError::Store(EnforcementStoreError::MissingTransition(action_id)) =
        &error
    {
        return ContainmentEnforcerError::MissingTransition(*action_id);
    }
    let unavailable = matches!(
        &error,
        EnforcementCoordinatorError::IngestionUnavailable
            | EnforcementCoordinatorError::EnforcementUnavailable(_)
            | EnforcementCoordinatorError::TransitionUnavailable
            | EnforcementCoordinatorError::Store(_)
            | EnforcementCoordinatorError::Thread(_)
            | EnforcementCoordinatorError::Client(
                EnforcementError::Io(_) | EnforcementError::Disconnected
            )
    );
    log::error!("containment enforcement operation failed: {error}");
    if unavailable {
        ContainmentEnforcerError::Unavailable("enforcement service is unavailable".into())
    } else {
        ContainmentEnforcerError::Rejected("enforcement operation was rejected".into())
    }
}

fn stamped_completed_transition(
    transition: PolicyTransition,
    stamp: ContainmentReadinessStamp,
) -> Result<StampedBinding, ContainmentEnforcerError> {
    if transition.phase == TransitionPhase::Completed
        && let Some(binding) = transition.acknowledgement
    {
        return Ok(StampedBinding::new(binding, stamp));
    }
    Err(ContainmentEnforcerError::Rejected(
        "policy replacement retained the source audit binding".into(),
    ))
}
